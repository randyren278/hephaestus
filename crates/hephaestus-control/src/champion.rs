//! Deterministic Champion transitions: operator seed, evidence-joined
//! promotion, and rollback to the previous Champion.
//!
//! Every transition is one `champion.transitioned` event. Replay recomputes each
//! payload from the history that preceded it, so a transition can only exist if
//! the policy admitted it at that point.

use hephaestus_arena::verify_reference_output_invariant_event_in;
use hephaestus_genome::RegisteredObjects;
use hephaestus_ledger::{ArtifactBackend, StoredEvent};

use super::{
    ControlError, ExecuteError, OPERATOR_ACTOR, decode_forge_assessment, forge_assessment_event_id,
    hex_encode, validate_job_id,
};
use crate::protocol::{
    ChampionEventRecord, ChampionPromotionEvidence, ChampionRecord, ChampionTransitionKind,
    ChampionTransitionPayload, ChampionTransitionRecord, ForgeAssessmentOutcome,
};

pub(super) const CHAMPION_EVENT_TYPE: &str = "champion.transitioned";
const CHAMPION_PREFIX: &str = "champion:";

/// Operator request that a transition payload is derived from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ChampionRequest {
    Seed {
        world_id: String,
        genome_id: String,
        reason: String,
    },
    Promote {
        assessment_id: String,
    },
    Rollback {
        world_id: String,
        reason: String,
    },
}

impl ChampionRequest {
    fn from_payload(payload: &ChampionTransitionPayload) -> Result<Self, ControlError> {
        let invalid =
            || ControlError::Projection("Champion transition shape is invalid".to_owned());
        match payload.kind {
            ChampionTransitionKind::Seeded => Ok(Self::Seed {
                world_id: payload.world_id.clone(),
                genome_id: payload.champion_genome_id.clone(),
                reason: payload.reason.clone().ok_or_else(invalid)?,
            }),
            ChampionTransitionKind::Promoted => Ok(Self::Promote {
                assessment_id: payload
                    .promotion
                    .as_ref()
                    .ok_or_else(invalid)?
                    .assessment_id
                    .clone(),
            }),
            ChampionTransitionKind::RolledBack => Ok(Self::Rollback {
                world_id: payload.world_id.clone(),
                reason: payload.reason.clone().ok_or_else(invalid)?,
            }),
        }
    }
}

pub(super) fn champion_event_id(transition_id: &str) -> String {
    format!("{CHAMPION_PREFIX}{transition_id}:recorded")
}

pub(super) fn champion_aggregate_id(world_id: &str) -> String {
    format!("{CHAMPION_PREFIX}{world_id}")
}

pub(super) fn validate_reason(reason: &str) -> Result<(), ExecuteError> {
    if reason.trim().is_empty() || reason.len() > 512 || reason.chars().any(char::is_control) {
        return Err(ExecuteError::Invalid(
            "reason must be 1 to 512 printable UTF-8 bytes",
        ));
    }
    Ok(())
}

fn is_champion_event(event: &StoredEvent) -> bool {
    event.event_type == CHAMPION_EVENT_TYPE
        || event.event_id.starts_with(CHAMPION_PREFIX)
        || event.aggregate_id.starts_with(CHAMPION_PREFIX)
}

pub(super) fn decode_champion_transition(
    event: &StoredEvent,
) -> Result<ChampionTransitionPayload, ControlError> {
    let payload =
        serde_json::from_slice::<ChampionTransitionPayload>(&event.payload).map_err(|_| {
            ControlError::Projection("Champion transition payload is invalid".to_owned())
        })?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "Champion transition payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

pub(super) fn champion_transition_record(
    payload: ChampionTransitionPayload,
    event: &StoredEvent,
) -> ChampionTransitionRecord {
    ChampionTransitionRecord {
        payload,
        event: ChampionEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_hash: hex_encode(&event.hash),
        },
    }
}

/// Champion projection of one World from already-verified history.
pub(super) fn champion_projection(
    history: &[StoredEvent],
    world_id: &str,
) -> Result<ChampionRecord, ControlError> {
    let mut lineage: Vec<String> = Vec::new();
    let mut quarantined = Vec::new();
    let mut transitions = Vec::new();
    for event in history
        .iter()
        .filter(|event| event.event_type == CHAMPION_EVENT_TYPE)
    {
        let payload = decode_champion_transition(event)?;
        if payload.world_id != world_id {
            continue;
        }
        match payload.kind {
            ChampionTransitionKind::Seeded | ChampionTransitionKind::Promoted => {
                lineage.push(payload.champion_genome_id.clone());
            }
            ChampionTransitionKind::RolledBack => {
                if let Some(removed) = lineage.pop() {
                    quarantined.push(removed);
                }
            }
        }
        transitions.push(champion_transition_record(payload, event));
    }
    let champion_genome_id = lineage.pop();
    Ok(ChampionRecord {
        world_id: world_id.to_owned(),
        champion_genome_id,
        standby_genome_ids: lineage,
        quarantined_genome_ids: quarantined,
        transitions,
    })
}

/// Freeze state immediately after `history`; the control plane starts frozen.
fn frozen_after(history: &[StoredEvent]) -> bool {
    let mut frozen = true;
    for event in history {
        match event.event_type.as_str() {
            "control.freeze" => frozen = true,
            "control.unfreeze" => frozen = false,
            _ => {}
        }
    }
    frozen
}

/// Returns the recorded transition for `transition_id` if the request matches it.
pub(super) fn existing_champion_transition(
    history: &[StoredEvent],
    transition_id: &str,
    request: &ChampionRequest,
) -> Result<Option<ChampionTransitionRecord>, ExecuteError> {
    let event_id = champion_event_id(transition_id);
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return Ok(None);
    };
    let payload = decode_champion_transition(event).map_err(|_| ExecuteError::Internal)?;
    let recorded = ChampionRequest::from_payload(&payload).map_err(|_| ExecuteError::Internal)?;
    if &recorded != request {
        return Err(ExecuteError::Rejected(
            "transition_id is already bound to different Champion content".to_owned(),
        ));
    }
    Ok(Some(champion_transition_record(payload, event)))
}

/// Derives the only payload the policy admits for `request` after `history`.
pub(super) fn champion_transition_payload(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    transition_id: &str,
    request: &ChampionRequest,
) -> Result<ChampionTransitionPayload, ExecuteError> {
    match request {
        ChampionRequest::Seed {
            world_id,
            genome_id,
            reason,
        } => seed_payload(
            history,
            registered,
            transition_id,
            world_id,
            genome_id,
            reason,
        ),
        ChampionRequest::Promote { assessment_id } => {
            promote_payload(artifacts, history, registered, transition_id, assessment_id)
        }
        ChampionRequest::Rollback { world_id, reason } => {
            rollback_payload(history, registered, transition_id, world_id, reason)
        }
    }
}

fn previous_transition(
    projection: &ChampionRecord,
) -> (Option<String>, Option<String>, Option<String>) {
    projection
        .transitions
        .last()
        .map_or((None, None, None), |last| {
            (
                projection.champion_genome_id.clone(),
                Some(last.event.event_id.clone()),
                Some(last.event.event_hash.clone()),
            )
        })
}

fn seed_payload(
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    transition_id: &str,
    world_id: &str,
    genome_id: &str,
    reason: &str,
) -> Result<ChampionTransitionPayload, ExecuteError> {
    validate_reason(reason)?;
    if frozen_after(history) {
        return Err(ExecuteError::Invalid("evolution is frozen"));
    }
    registered.world(world_id).ok_or(ExecuteError::NotFound)?;
    let genome = registered.genome(genome_id).ok_or(ExecuteError::NotFound)?;
    if genome.record().world_id != world_id {
        return Err(ExecuteError::Rejected(
            "Genome is not compiled under the requested World".to_owned(),
        ));
    }
    let projection = champion_projection(history, world_id).map_err(|_| ExecuteError::Internal)?;
    if !projection.transitions.is_empty() {
        return Err(ExecuteError::Rejected(
            "World already has Champion history; only promotion or rollback may change it"
                .to_owned(),
        ));
    }
    Ok(ChampionTransitionPayload {
        schema_version: 1,
        transition_id: transition_id.to_owned(),
        world_id: world_id.to_owned(),
        kind: ChampionTransitionKind::Seeded,
        champion_genome_id: genome_id.to_owned(),
        previous_champion_genome_id: None,
        previous_transition_event_id: None,
        previous_transition_event_hash: None,
        promotion: None,
        reason: Some(reason.to_owned()),
    })
}

fn promote_payload(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    transition_id: &str,
    assessment_id: &str,
) -> Result<ChampionTransitionPayload, ExecuteError> {
    validate_job_id(assessment_id)
        .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))?;
    if frozen_after(history) {
        return Err(ExecuteError::Invalid("evolution is frozen"));
    }
    let assessment_event_id = forge_assessment_event_id(assessment_id);
    let assessment_event = history
        .iter()
        .find(|event| event.event_id == assessment_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let assessment =
        decode_forge_assessment(assessment_event).map_err(|_| ExecuteError::Internal)?;
    if assessment.outcome != ForgeAssessmentOutcome::MetricsPassed {
        return Err(ExecuteError::Rejected(
            "Forge assessment did not pass the World metrics policy".to_owned(),
        ));
    }

    let projection =
        champion_projection(history, &assessment.world_id).map_err(|_| ExecuteError::Internal)?;
    let Some(current) = projection.champion_genome_id.clone() else {
        return Err(ExecuteError::Rejected(
            "World has no Champion; seed one before promotion".to_owned(),
        ));
    };
    if assessment.parent_genome_id != current {
        return Err(ExecuteError::Rejected(
            "assessment parent is not the current Champion".to_owned(),
        ));
    }
    if assessment.child_genome_id == current
        || projection
            .standby_genome_ids
            .contains(&assessment.child_genome_id)
        || projection
            .quarantined_genome_ids
            .contains(&assessment.child_genome_id)
    {
        return Err(ExecuteError::Rejected(
            "assessment child already held or lost the Champion role".to_owned(),
        ));
    }

    let invariant_event_id = format!("arena:invariants:{}:checked", assessment.evaluation_id);
    let invariant_event = history
        .iter()
        .find(|event| event.event_id == invariant_event_id)
        .ok_or_else(|| {
            ExecuteError::Rejected(
                "invariant evidence for the assessed evaluation is required".to_owned(),
            )
        })?;
    let world = registered
        .world(&assessment.world_id)
        .ok_or(ExecuteError::Internal)?;
    let verified = verify_reference_output_invariant_event_in(
        &hephaestus_ledger::EventIndex::build(history),
        artifacts,
        invariant_event,
        world.compiled(),
    )
    .map_err(|_| ExecuteError::Internal)?;
    let receipt = verified.receipt().clone();
    let invariant_receipt_artifact_id = verified.event().receipt_artifact_id.clone();
    if receipt.evaluation_id != assessment.evaluation_id
        || receipt.world_id != assessment.world_id
        || receipt.parent_genome_id != assessment.parent_genome_id
        || receipt.candidate_genome_id != assessment.child_genome_id
    {
        return Err(ExecuteError::Internal);
    }
    if !(receipt.regressions_within_budget && receipt.candidate_contract_satisfied) {
        return Err(ExecuteError::Rejected(
            "invariant evidence does not satisfy the World contract".to_owned(),
        ));
    }

    let (previous_champion, previous_event_id, previous_event_hash) =
        previous_transition(&projection);
    Ok(ChampionTransitionPayload {
        schema_version: 1,
        transition_id: transition_id.to_owned(),
        world_id: assessment.world_id.clone(),
        kind: ChampionTransitionKind::Promoted,
        champion_genome_id: assessment.child_genome_id.clone(),
        previous_champion_genome_id: previous_champion,
        previous_transition_event_id: previous_event_id,
        previous_transition_event_hash: previous_event_hash,
        promotion: Some(ChampionPromotionEvidence {
            assessment_id: assessment.assessment_id.clone(),
            assessment_event_id: assessment_event.event_id.clone(),
            assessment_event_hash: hex_encode(&assessment_event.hash),
            evaluation_id: assessment.evaluation_id.clone(),
            selection_receipt_artifact_id: assessment.selection_receipt_artifact_id.clone(),
            invariant_event_id: invariant_event.event_id.clone(),
            invariant_event_hash: hex_encode(&invariant_event.hash),
            invariant_receipt_artifact_id,
        }),
        reason: None,
    })
}

fn rollback_payload(
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    transition_id: &str,
    world_id: &str,
    reason: &str,
) -> Result<ChampionTransitionPayload, ExecuteError> {
    validate_reason(reason)?;
    registered.world(world_id).ok_or(ExecuteError::NotFound)?;
    let projection = champion_projection(history, world_id).map_err(|_| ExecuteError::Internal)?;
    let Some(restored) = projection.standby_genome_ids.last().cloned() else {
        return Err(ExecuteError::Rejected(
            "World has no previous Champion to restore".to_owned(),
        ));
    };
    let (previous_champion, previous_event_id, previous_event_hash) =
        previous_transition(&projection);
    Ok(ChampionTransitionPayload {
        schema_version: 1,
        transition_id: transition_id.to_owned(),
        world_id: world_id.to_owned(),
        kind: ChampionTransitionKind::RolledBack,
        champion_genome_id: restored,
        previous_champion_genome_id: previous_champion,
        previous_transition_event_id: previous_event_id,
        previous_transition_event_hash: previous_event_hash,
        promotion: None,
        reason: Some(reason.to_owned()),
    })
}

/// Recomputes every Champion transition from the history that preceded it.
/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `TECH_DEBT.md` TD-16).
pub(super) fn verify_champion_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_champion_history_with(
        artifacts,
        history,
        registered,
        &mut super::EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_champion_history`]; see `EvidenceCache`.
pub(super) fn verify_champion_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut super::EvidenceCache,
) -> Result<(), ControlError> {
    for (index, event) in history.iter().enumerate() {
        if !is_champion_event(event) || cache.contains("champion", event) {
            continue;
        }
        let payload = decode_champion_transition(event)?;
        if payload.schema_version != 1
            || event.event_type != CHAMPION_EVENT_TYPE
            || event.actor != OPERATOR_ACTOR
            || event.event_id != champion_event_id(&payload.transition_id)
            || event.aggregate_id != champion_aggregate_id(&payload.world_id)
            || validate_job_id(&payload.transition_id).is_err()
        {
            return Err(ControlError::Projection(
                "Champion transition event identity is invalid".to_owned(),
            ));
        }
        let request = ChampionRequest::from_payload(&payload)?;
        let expected = champion_transition_payload(
            artifacts,
            &history[..index],
            registered,
            &payload.transition_id,
            &request,
        )
        .map_err(|_| {
            ControlError::Projection("Champion transition was not admissible".to_owned())
        })?;
        if payload != expected {
            return Err(ControlError::Projection(
                "Champion transition differs from verified evidence".to_owned(),
            ));
        }
        cache.insert("champion", event);
    }
    Ok(())
}
