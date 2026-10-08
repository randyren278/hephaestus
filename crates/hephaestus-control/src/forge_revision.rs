//! Strict hosted prompt revisions alongside the unchanged catalog contract.

use super::verification::{
    ForgeChildBinding, decode_forge_proposal, forge_proposal_record, validate_hypothesis,
    verified_forge_source_in, verified_prompt_bytes, verify_forge_child_source,
};
use super::{
    ArtifactBackend, ArtifactId, ControlError, ControlPlane, EventIndex, EventInput, EventLedger,
    ExecuteError, ForgeProposalEventRecord, ForgeProposalPayload, ForgeRevisionPayload,
    ForgeRevisionRecord, GenomeRecord, MAX_SOURCE_FILE_BYTES, MutationTarget, OPERATOR_ACTOR,
    RegisteredObjects, ResponseData, RunSpec, StoredEvent, compile_forge_child, forge_aggregate_id,
    forge_event_id, hex_encode, read_source_text, timestamp_millis, validate_job_id,
    verified_forge_source,
};

// Admission must never accept a prompt that the runtime/replay contract rejects.
const _: () = assert!(MAX_SOURCE_FILE_BYTES <= hephaestus_runtime::MAX_TASK_INPUT_BYTES as u64);

pub(super) enum ForgeProposalKind {
    Catalog(ForgeProposalPayload),
    Revision(ForgeRevisionPayload),
}

impl ForgeProposalKind {
    pub(super) fn proposal_id(&self) -> &str {
        match self {
            Self::Catalog(p) => &p.proposal_id,
            Self::Revision(p) => &p.proposal_id,
        }
    }
    pub(super) fn world_id(&self) -> &str {
        match self {
            Self::Catalog(p) => &p.world_id,
            Self::Revision(p) => &p.world_id,
        }
    }
    pub(super) fn parent_genome_id(&self) -> &str {
        match self {
            Self::Catalog(p) => &p.parent_genome_id,
            Self::Revision(p) => &p.parent_genome_id,
        }
    }
    pub(super) fn child(&self) -> &GenomeRecord {
        match self {
            Self::Catalog(p) => &p.child,
            Self::Revision(p) => &p.child,
        }
    }
    pub(super) fn into_catalog(self, reason: &str) -> Result<ForgeProposalPayload, ExecuteError> {
        match self {
            Self::Catalog(payload) => Ok(payload),
            Self::Revision(_) => Err(ExecuteError::Rejected(reason.to_owned())),
        }
    }
    pub(super) fn into_response(self, event: &StoredEvent) -> ResponseData {
        match self {
            Self::Catalog(payload) => ResponseData::ForgeProposal {
                proposal: Box::new(forge_proposal_record(payload, event)),
            },
            Self::Revision(payload) => ResponseData::ForgeRevision {
                revision: Box::new(revision_record(payload, event)),
            },
        }
    }
}

pub(super) fn decode_forge_proposal_kind(
    event: &StoredEvent,
) -> Result<ForgeProposalKind, ControlError> {
    let value: serde_json::Value = serde_json::from_slice(&event.payload)?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(1) => decode_forge_proposal(event).map(ForgeProposalKind::Catalog),
        Some(2) => {
            let payload: ForgeRevisionPayload = serde_json::from_value(value).map_err(|_| {
                ControlError::Projection("Forge revision payload is invalid".to_owned())
            })?;
            if serde_json::to_vec(&serde_json::to_value(&payload)?)? != event.payload {
                return Err(ControlError::Projection(
                    "Forge revision payload is not canonical".to_owned(),
                ));
            }
            Ok(ForgeProposalKind::Revision(payload))
        }
        _ => Err(ControlError::Projection(
            "Forge proposal schema is invalid".to_owned(),
        )),
    }
}

fn revision_record(payload: ForgeRevisionPayload, event: &StoredEvent) -> ForgeRevisionRecord {
    ForgeRevisionRecord {
        payload,
        event: ForgeProposalEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_hash: hex_encode(&event.hash),
        },
        promotion_eligible: false,
    }
}

pub(super) fn reject_cross_kind_retry(
    history: &[StoredEvent],
    proposal_id: &str,
    revision: bool,
) -> Result<(), ExecuteError> {
    if let Some(event) = history
        .iter()
        .find(|event| event.event_id == forge_event_id(proposal_id))
    {
        let kind = decode_forge_proposal_kind(event).map_err(|_| ExecuteError::Internal)?;
        match (revision, kind) {
            (true, ForgeProposalKind::Catalog(_)) => {
                return Err(ExecuteError::Rejected(
                    "proposal_id is already bound to a catalog proposal".to_owned(),
                ));
            }
            (false, ForgeProposalKind::Revision(_)) => {
                return Err(ExecuteError::Rejected(
                    "proposal_id is already bound to a revision proposal".to_owned(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn validate_revision_event(
    event: &StoredEvent,
    payload: &ForgeRevisionPayload,
) -> Result<(), ControlError> {
    if payload.schema_version != 2
        || event.actor != OPERATOR_ACTOR
        || event.event_type != "forge.proposed"
        || event.event_id != forge_event_id(&payload.proposal_id)
        || event.aggregate_id != forge_aggregate_id(&payload.proposal_id)
    {
        return Err(ControlError::Projection(
            "Forge revision event identity is invalid".to_owned(),
        ));
    }
    validate_job_id(&payload.proposal_id)
        .map_err(|_| ControlError::Projection("Forge revision id is invalid".to_owned()))?;
    validate_hypothesis(&payload.hypothesis)
        .map_err(|_| ControlError::Projection("Forge revision hypothesis is invalid".to_owned()))?;
    Ok(())
}

pub(super) fn verify_revision(
    artifacts: &dyn ArtifactBackend,
    index: &EventIndex<'_>,
    registered: &RegisteredObjects,
    event: &StoredEvent,
    payload: &ForgeRevisionPayload,
) -> Result<(), ControlError> {
    validate_revision_event(event, payload)?;
    let source = index
        .get(&payload.selection_event_id)
        .filter(|source| source.sequence < event.sequence)
        .ok_or_else(|| {
            ControlError::Projection("Forge revision source is missing or out of order".to_owned())
        })?;
    let (selection_hash, evaluation_id, world_id) = verified_forge_source_in(
        artifacts,
        index,
        registered,
        &source.event_id,
        &payload.parent_genome_id,
    )
    .map_err(|_| ControlError::Projection("Forge revision selection is unverified".to_owned()))?;
    if payload.selection_event_hash != selection_hash
        || payload.evaluation_id != evaluation_id
        || payload.world_id != world_id
    {
        return Err(ControlError::Projection(
            "Forge revision source binding differs".to_owned(),
        ));
    }
    let world = registered
        .world(&world_id)
        .ok_or_else(|| ControlError::Projection("Forge revision World is missing".to_owned()))?;
    if !world
        .compiled()
        .mutation_scope()
        .contains(&MutationTarget::Harness)
    {
        return Err(ControlError::Projection(
            "Forge revision World forbids harness mutations".to_owned(),
        ));
    }
    let parent = registered
        .genome(&payload.parent_genome_id)
        .ok_or_else(|| ControlError::Projection("Forge revision parent is missing".to_owned()))?;
    let child = registered
        .genome(&payload.child.genome_id)
        .ok_or_else(|| ControlError::Projection("Forge revision child is missing".to_owned()))?;
    if !matches!(parent.compiled().model_provider(), "codex" | "claude")
        || parent.registration_sequence() >= event.sequence
        || child.registration_sequence() != event.sequence
        || child.record() != &payload.child
        || child.record().world_id != payload.world_id
        || parent.record().world_id != payload.world_id
        || child.compiled().parents() != [payload.parent_genome_id.clone()]
        || parent.compiled().artifact_id("agent.prompt")
            != Some(payload.prompt_artifact_before.as_str())
        || child.compiled().artifact_id("agent.prompt")
            != Some(payload.prompt_artifact_after.as_str())
        || payload.artifact_name != "agent.prompt"
    {
        return Err(ControlError::Projection(
            "Forge revision differs from its registered lineage".to_owned(),
        ));
    }
    let before = verified_prompt_bytes(artifacts, &payload.prompt_artifact_before)?;
    let after = verified_prompt_bytes(artifacts, &payload.prompt_artifact_after)?;
    let text = std::str::from_utf8(&after)
        .map_err(|_| ControlError::Protocol("Forge revision prompt is not UTF-8"))?;
    if after.len() > hephaestus_runtime::MAX_TASK_INPUT_BYTES
        || text.trim().is_empty()
        || before == after
    {
        return Err(ControlError::Projection(
            "Forge revision prompt is empty, oversized or unchanged".to_owned(),
        ));
    }
    verify_forge_child_source(
        artifacts,
        registered,
        &ForgeChildBinding {
            proposal_id: &payload.proposal_id,
            parent_genome_id: &payload.parent_genome_id,
            prompt_artifact_after: &payload.prompt_artifact_after,
            world_id: &payload.world_id,
            child: &payload.child,
        },
        world.compiled(),
    )
}

impl ControlPlane {
    #[allow(clippy::too_many_lines)]
    pub(super) fn revise_genome(
        &mut self,
        proposal_id: &str,
        selection_event_id: &str,
        parent_genome_id: &str,
        prompt_path: &str,
        hypothesis: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if self.state.freeze.is_frozen() {
            return Err(ExecuteError::Invalid("evolution is frozen"));
        }
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        validate_hypothesis(hypothesis)?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        reject_cross_kind_retry(&history, proposal_id, true)?;
        let after_text = read_source_text(prompt_path, MAX_SOURCE_FILE_BYTES)?;
        let after_id = ArtifactId::for_bytes(after_text.as_bytes());
        if let Some(event) = history
            .iter()
            .find(|event| event.event_id == forge_event_id(proposal_id))
        {
            let ForgeProposalKind::Revision(existing) =
                decode_forge_proposal_kind(event).map_err(|_| ExecuteError::Internal)?
            else {
                return Err(ExecuteError::Internal);
            };
            if existing.selection_event_id != selection_event_id
                || existing.parent_genome_id != parent_genome_id
                || existing.hypothesis != hypothesis
                || existing.prompt_artifact_after != after_id.as_str()
            {
                return Err(ExecuteError::Rejected(
                    "proposal_id is already bound to different proposal content".to_owned(),
                ));
            }
            return Ok(ForgeProposalKind::Revision(existing).into_response(event));
        }
        if proposal_id.starts_with("evolve-") || proposal_id.starts_with("adapt-") {
            return Err(ExecuteError::Rejected(
                "proposal_id uses a reserved automatic-proposal prefix".to_owned(),
            ));
        }
        let (selection_hash, evaluation_id, world_id) = verified_forge_source(
            &storage.artifacts,
            &history,
            &self.state.registered,
            selection_event_id,
            parent_genome_id,
        )?;
        let world = self
            .state
            .registered
            .world(&world_id)
            .ok_or(ExecuteError::Internal)?;
        if !world
            .compiled()
            .mutation_scope()
            .contains(&MutationTarget::Harness)
        {
            return Err(ExecuteError::Rejected(
                "World mutation scope does not authorize harness mutations".to_owned(),
            ));
        }
        let parent = self
            .state
            .registered
            .genome(parent_genome_id)
            .ok_or(ExecuteError::NotFound)?;
        if !matches!(parent.compiled().model_provider(), "codex" | "claude") {
            return Err(ExecuteError::Rejected(
                "prompt revisions require a hosted-provider parent".to_owned(),
            ));
        }
        RunSpec::validate_provider_model(parent.compiled().model_family()).map_err(|_| {
            ExecuteError::Rejected("registered provider model is invalid".to_owned())
        })?;
        let before = parent
            .compiled()
            .artifact_id("agent.prompt")
            .ok_or_else(|| {
                ExecuteError::Rejected("selected candidate has no prompt to revise".to_owned())
            })?
            .to_owned();
        let before_bytes = verified_prompt_bytes(&storage.artifacts, &before)
            .map_err(|_| ExecuteError::Internal)?;
        if after_text.trim().is_empty() {
            return Err(ExecuteError::Rejected(
                "revised prompt must not be blank".to_owned(),
            ));
        }
        if before_bytes == after_text.as_bytes() {
            return Err(ExecuteError::Rejected(
                "revised prompt must change its exact bytes".to_owned(),
            ));
        }
        let after = storage
            .artifacts
            .put(after_text.as_bytes())
            .map_err(|_| ExecuteError::Internal)?;
        let child = compile_forge_child(
            &self.state.registered,
            world.compiled(),
            &storage.artifacts,
            parent_genome_id,
            &world_id,
            proposal_id,
            after.as_str(),
        )?;
        if self.state.registered.genome(&child.genome_id).is_some() {
            return Err(ExecuteError::Rejected(
                "derived child identity is already registered".to_owned(),
            ));
        }
        let payload = ForgeRevisionPayload {
            schema_version: 2,
            proposal_id: proposal_id.to_owned(),
            selection_event_id: selection_event_id.to_owned(),
            selection_event_hash: selection_hash,
            evaluation_id,
            world_id,
            parent_genome_id: parent_genome_id.to_owned(),
            child,
            hypothesis: hypothesis.to_owned(),
            artifact_name: "agent.prompt".to_owned(),
            prompt_artifact_before: before,
            prompt_artifact_after: after.as_str().to_owned(),
        };
        let bytes = serde_json::to_vec(
            &serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let event = storage
            .ledger
            .append(EventInput::new(
                forge_event_id(proposal_id),
                forge_aggregate_id(proposal_id),
                "forge.proposed",
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ForgeProposalKind::Revision(payload).into_response(&event))
    }
}
