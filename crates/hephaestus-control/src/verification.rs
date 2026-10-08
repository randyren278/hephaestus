//! History verifiers and their supporting types: audited-command records,
//! error mapping, forge/selection/invariant/cluster verification, forge
//! proposal/assessment payload construction, evolution id builders, and
//! command-field validation, split out of server.rs.

use super::forge_revision::{
    ForgeProposalKind, decode_forge_proposal_kind, validate_revision_event, verify_revision,
};

use super::{
    ArenaError, ArtifactBackend, ArtifactId, BTreeMap, BTreeSet, CHAMPION_EVENT_TYPE,
    CLUSTER_EVENT_PREFIX, ChampionTransitionPayload, ClusterAnalysis, ClusterEvent, Command,
    CompiledWorld, ControlError, ControlState, Deserialize, EvaluationEventRecord,
    EvaluationForgeSummary, EvaluationRecord, EventIndex, ForgeAnalysisBinding,
    ForgeAnalysisRecord, ForgeAssessmentEventRecord, ForgeAssessmentOutcome,
    ForgeAssessmentPayload, ForgeAssessmentRecord, ForgeProposalEventRecord, ForgeProposalPayload,
    ForgeProposalRecord, GENE_EVENT_TYPE, GeneTransferOutcome, GenomeRecord, HashSet,
    InvariantEvent, InvariantReceipt, InvariantRecord, JobState, JobTerminal,
    MAX_EVOLUTION_RUN_ID_BYTES, MAX_LIST_LIMIT, MAX_META_RUN_ID_BYTES, MAX_WORKER_TTL_SECONDS,
    MUTATION_CATALOG_VERSION, McpDecision, MutationSlot, MutationTarget, OPERATOR_ACTOR,
    ReferenceInstruction, RegisteredObjects, ResponseData, SelectionEvent, SelectionEventRecord,
    SelectionReceipt, SelectionRecord, Serialize, SourceFormat, StoredEvent, SuggestedMutation,
    TRANSFER_RECORDED_EVENT_TYPE, TRIALS_PER_GENERATION, cluster_event_references, compile_genome,
    decode_gene_extracted, decode_transfer_recorded, hex_encode, invariant_event_references,
    is_catalog_edge, load_recorded_evaluation_in, mutation_edge_kind, selection_event_references,
    validate_job_id, validate_reason, verify_cluster_event_in,
    verify_reference_output_invariant_event_in, verify_selection_event_in,
};

#[derive(Serialize)]
pub(super) struct AuditedCommand<'a> {
    pub(super) request_id: &'a str,
    pub(super) command: &'a Command,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordedCommand {
    pub(super) request_id: String,
    pub(super) command: Command,
}

#[derive(Debug)]
pub(super) enum ExecuteError {
    Invalid(&'static str),
    Rejected(String),
    NotFound,
    Internal,
    Busy,
}

pub(super) fn map_evaluator_open_error(error: &ArenaError) -> ExecuteError {
    let message = match error {
        ArenaError::WorldArtifactMismatch("arena.evaluator") => {
            "configured evaluator does not match the World's pinned evaluator; stop this daemon and restart it with the matching installation used to prepare this World, then retry"
        }
        ArenaError::EvaluatorExecution(message) if message == "evaluator executable is unsafe" => {
            "configured evaluator executable is unsafe; it requires a regular executable file with one hard link and no symlink; restore it from the matching installation, then retry"
        }
        ArenaError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            "configured evaluator executable is missing; restore it at the configured path from the matching installation, then retry"
        }
        ArenaError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            "configured evaluator executable cannot be read; check its access permissions or restore it from the matching installation, then retry"
        }
        _ => return ExecuteError::Internal,
    };
    ExecuteError::Rejected(message.to_owned())
}

pub(super) fn map_selection_error(error: &ArenaError) -> ExecuteError {
    match error {
        ArenaError::UnknownEvaluation(_) => ExecuteError::NotFound,
        ArenaError::UnsupportedSelectionConfidence(confidence) => ExecuteError::Rejected(format!(
            "registered World confidence {confidence} bps is unsupported for Arena selection"
        )),
        ArenaError::BootstrapWorkExceeded => ExecuteError::Rejected(
            "Arena selection exceeds its deterministic bootstrap work limit".to_owned(),
        ),
        _ => ExecuteError::Internal,
    }
}

pub(super) fn map_invariant_error(error: ArenaError) -> ExecuteError {
    match error {
        ArenaError::UnknownEvaluation(_) | ArenaError::UnknownInvariantCheck(_) => {
            ExecuteError::NotFound
        }
        ArenaError::MissingWorldArtifact("arena.invariant_manifest") => ExecuteError::Rejected(
            "registered World has no reference-output invariant profile".to_owned(),
        ),
        ArenaError::InvariantConflict(message) => ExecuteError::Rejected(message),
        _ => ExecuteError::Internal,
    }
}

pub(super) fn map_cluster_error(error: ArenaError) -> ExecuteError {
    match error {
        ArenaError::UnknownEvaluation(_) | ArenaError::UnknownClusterAnalysis(_) => {
            ExecuteError::NotFound
        }
        ArenaError::ClusterConflict(message) => ExecuteError::Rejected(message),
        ArenaError::InvalidId { .. } => ExecuteError::Invalid("analysis_id is invalid"),
        _ => ExecuteError::Internal,
    }
}

pub(super) fn selection_record(
    world_id: &str,
    receipt: &SelectionReceipt,
    event: &SelectionEvent,
) -> SelectionRecord {
    SelectionRecord {
        evaluation_id: receipt.evaluation_id().to_owned(),
        world_id: world_id.to_owned(),
        receipt: receipt.clone(),
        event: SelectionEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_type: event.event_type.clone(),
            actor: event.actor.clone(),
            event_hash: event.event_hash.clone(),
            receipt_artifact_id: event.receipt_artifact_id.clone(),
        },
    }
}

pub(super) fn invariant_record(
    receipt: &InvariantReceipt,
    event: &InvariantEvent,
) -> InvariantRecord {
    InvariantRecord {
        receipt: receipt.clone(),
        event: event.clone(),
    }
}

pub(super) fn forge_analysis_record(
    analysis: &ClusterAnalysis,
    event: &ClusterEvent,
) -> ForgeAnalysisRecord {
    ForgeAnalysisRecord {
        analysis: analysis.clone(),
        event: event.clone(),
    }
}

pub(super) fn forge_assessment_summary(
    history: &[StoredEvent],
    evaluation_id: &str,
) -> Option<EvaluationForgeSummary> {
    history
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
        .filter_map(|event| decode_forge_assessment(event).ok())
        .find(|payload| payload.evaluation_id == evaluation_id)
        .map(|payload| EvaluationForgeSummary {
            assessment_id: payload.assessment_id,
            outcome: payload.outcome,
        })
}

pub(super) fn champion_transition_ids_for(
    history: &[StoredEvent],
    evaluation_id: &str,
) -> Vec<String> {
    history
        .iter()
        .filter(|event| event.event_type == CHAMPION_EVENT_TYPE)
        .filter_map(|event| {
            serde_json::from_slice::<ChampionTransitionPayload>(&event.payload).ok()
        })
        .filter(|payload| {
            payload
                .promotion
                .as_ref()
                .is_some_and(|promotion| promotion.evaluation_id == evaluation_id)
        })
        .map(|payload| payload.transition_id)
        .collect()
}

pub(super) fn evaluation_record_from_operator(
    operator: &hephaestus_arena::OperatorEvaluation,
) -> EvaluationRecord {
    let recorded = operator.candidate_result();
    EvaluationRecord {
        evaluation_id: recorded.summary.evaluation_id.clone(),
        world_id: recorded.summary.world_id.clone(),
        parent_genome_id: recorded.summary.parent_genome_id.clone(),
        candidate_genome_id: recorded.summary.candidate_genome_id.clone(),
        parent_visible_correct: recorded.summary.parent_visible_correct,
        candidate_visible_correct: recorded.summary.candidate_visible_correct,
        visible_total: recorded.summary.visible_total,
        event: EvaluationEventRecord {
            sequence: recorded.event.sequence,
            event_id: recorded.event.event_id.clone(),
            aggregate_id: recorded.event.aggregate_id.clone(),
            event_type: recorded.event.event_type.clone(),
            actor: recorded.event.actor.clone(),
            timestamp_millis: recorded.event.timestamp_millis,
        },
    }
}

pub(super) fn evaluation_record_from_recorded(
    recorded: &hephaestus_arena::RecordedEvaluation,
) -> EvaluationRecord {
    EvaluationRecord {
        evaluation_id: recorded.summary.evaluation_id.clone(),
        world_id: recorded.summary.world_id.clone(),
        parent_genome_id: recorded.summary.parent_genome_id.clone(),
        candidate_genome_id: recorded.summary.candidate_genome_id.clone(),
        parent_visible_correct: recorded.summary.parent_visible_correct,
        candidate_visible_correct: recorded.summary.candidate_visible_correct,
        visible_total: recorded.summary.visible_total,
        event: EvaluationEventRecord {
            sequence: recorded.event.sequence,
            event_id: recorded.event.event_id.clone(),
            aggregate_id: recorded.event.aggregate_id.clone(),
            event_type: recorded.event.event_type.clone(),
            actor: recorded.event.actor.clone(),
            timestamp_millis: recorded.event.timestamp_millis,
        },
    }
}

/// Remembers evidence a live daemon has already verified during projection
/// refresh, so each refresh re-verifies only events appended or changed
/// since the last one instead of the whole history every time.
///
/// A key embeds the event's chain hash (or, for an Arena job terminal, a
/// digest of the recorded summary), which commits to the event and its
/// entire ledger prefix; every refresh still re-verifies the hash chain
/// first via `EventLedger::replay_verified()`, so a rewritten ledger prefix
/// changes every later event's hash and misses the cache. Startup, `replay`,
/// and every direct verifier call use a fresh, empty cache and verify
/// everything. See `docs/dev/TECH_DEBT.md` TD-16: unlike before this cache existed,
/// a cache hit no longer implies reopening stores or replaying the ledger —
/// [`load_operator_evaluation_in`] and its siblings verify a cache *miss*
/// against the already-replayed `history` in one pass, so a cold cache (a
/// fresh daemon, or many events appended between refreshes) is itself linear
/// in history size rather than quadratic. The cache remains because a
/// warm-cache refresh is still cheaper than any full linear pass: it makes
/// the total cost of many refreshes over a growing history linear in the
/// number of *new* events, not the square of history length. A CAS blob
/// tampered with after its event was verified is not re-detected until the
/// cache is empty again (the next daemon start or `replay`).
#[derive(Default)]
pub(super) struct EvidenceCache {
    verified: HashSet<String>,
}

impl EvidenceCache {
    fn event_key(kind: &str, event: &StoredEvent) -> String {
        format!(
            "{kind}:{}:{}",
            event.event_id,
            blake3::Hash::from(event.hash).to_hex()
        )
    }

    pub(super) fn contains(&self, kind: &str, event: &StoredEvent) -> bool {
        self.verified.contains(&Self::event_key(kind, event))
    }

    pub(super) fn insert(&mut self, kind: &str, event: &StoredEvent) {
        self.verified.insert(Self::event_key(kind, event));
    }

    fn contains_key(&self, key: &str) -> bool {
        self.verified.contains(key)
    }

    fn insert_key(&mut self, key: String) {
        self.verified.insert(key);
    }
}

/// Verifies every succeeded Arena job's terminal summary against the trusted
/// evaluation evidence in `history`, using the daemon's already-open
/// `artifacts` store and the already-replayed `history` instead of reopening
/// stores per job (see `docs/dev/TECH_DEBT.md` TD-16).
pub(super) fn verify_arena_evaluation_records(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    state: &ControlState,
) -> Result<(), ControlError> {
    verify_arena_evaluation_records_with(artifacts, history, state, &mut EvidenceCache::default())
}

/// Cache-aware counterpart of [`verify_arena_evaluation_records`] used by a
/// live daemon's projection refresh: `cache` remembers, by evaluation id and
/// a digest of the recorded summary, which terminals this process has
/// already verified this run, so a repeated refresh skips re-verifying a job
/// whose terminal has not changed since. Every event is still fully
/// re-verified against `history`'s freshly re-verified hash chain the first
/// time (or after its recorded summary changes), so this is an optimization,
/// not a relaxation: `verify_arena_evaluation_records`, startup, and
/// `replay` always use a fresh cache and verify everything.
pub(super) fn verify_arena_evaluation_records_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    state: &ControlState,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for job in state.arena_jobs.values().filter(|job| {
        job.state == JobState::Succeeded && job.terminal == Some(JobTerminal::Succeeded)
    }) {
        let key = arena_record_cache_key(&job.evaluation_id, job.evaluation.as_ref())?;
        if cache.contains_key(&key) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let recorded =
            load_recorded_evaluation_in(index, artifacts, &job.evaluation_id).map_err(|_| {
                ControlError::Projection("Arena terminal lacks trusted evaluation evidence".into())
            })?;
        if job.evaluation.as_ref() != Some(&evaluation_record_from_recorded(&recorded)) {
            return Err(ControlError::Projection(
                "Arena terminal differs from trusted evaluation evidence".to_owned(),
            ));
        }
        cache.insert_key(key);
    }
    Ok(())
}

fn arena_record_cache_key(
    evaluation_id: &str,
    evaluation: Option<&EvaluationRecord>,
) -> Result<String, ControlError> {
    let digest = blake3::hash(&serde_json::to_vec(&evaluation)?);
    Ok(format!("arena_record:{evaluation_id}:{}", digest.to_hex()))
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `docs/dev/TECH_DEBT.md` TD-16).
pub(super) fn verify_forge_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_forge_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_forge_history`]; see [`EvidenceCache`].
pub(super) fn verify_forge_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    let mut proposal_ids = BTreeSet::new();
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.proposed")
    {
        let proposal = decode_forge_proposal_kind(event)?;
        match &proposal {
            ForgeProposalKind::Catalog(payload) => validate_forge_event(event, payload)?,
            ForgeProposalKind::Revision(payload) => validate_revision_event(event, payload)?,
        }
        if !proposal_ids.insert(proposal.proposal_id().to_owned()) {
            return Err(ControlError::Projection(
                "Forge proposal id was recorded more than once".to_owned(),
            ));
        }
        if cache.contains("forge", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let payload = match proposal {
            ForgeProposalKind::Catalog(payload) => payload,
            ForgeProposalKind::Revision(payload) => {
                verify_revision(artifacts, index, registered, event, &payload)?;
                cache.insert("forge", event);
                continue;
            }
        };
        let selection_event = index
            .get(&payload.selection_event_id)
            .filter(|selection| selection.sequence < event.sequence)
            .ok_or_else(|| {
                ControlError::Projection(
                    "Forge proposal source selection is missing or out of order".to_owned(),
                )
            })?;
        let (_evaluation_id, selection_world_id) = selection_event_references(selection_event)
            .map_err(|_| {
                ControlError::Projection("Forge source selection is invalid".to_owned())
            })?;
        let world = registered.world(&selection_world_id).ok_or_else(|| {
            ControlError::Projection("Forge source World is not registered".to_owned())
        })?;
        let selected =
            verify_selection_event_in(index, artifacts, selection_event, world.compiled())
                .map_err(|_| {
                    ControlError::Projection("Forge source selection is unverified".to_owned())
                })?;
        let receipt = selected.receipt().clone();
        let expected_hash = selected.event().event_hash.clone();
        if payload.selection_event_hash != expected_hash
            || payload.evaluation_id != receipt.evaluation_id()
            || payload.world_id != receipt.world_id()
            || payload.parent_genome_id != receipt.candidate_genome_id()
        {
            return Err(ControlError::Projection(
                "Forge proposal is not bound to its selected candidate".to_owned(),
            ));
        }
        verify_forge_child(artifacts, registered, event, &payload, world.compiled())?;
        validate_hypothesis(&payload.hypothesis)
            .map_err(|_| ControlError::Projection("Forge hypothesis is invalid".to_owned()))?;
        cache.insert("forge", event);
    }
    Ok(())
}

pub(super) fn verify_forge_child(
    artifacts: &dyn ArtifactBackend,
    registered: &RegisteredObjects,
    event: &StoredEvent,
    payload: &ForgeProposalPayload,
    world: &CompiledWorld,
) -> Result<(), ControlError> {
    let parent = registered
        .genome(&payload.parent_genome_id)
        .ok_or_else(|| {
            ControlError::Projection("Forge proposal parent is not registered".to_owned())
        })?;
    let child = registered.genome(&payload.child.genome_id).ok_or_else(|| {
        ControlError::Projection("Forge proposal child is not registered".to_owned())
    })?;
    if parent.registration_sequence() >= event.sequence
        || child.registration_sequence() != event.sequence
        || child.record() != &payload.child
        || child.record().world_id != payload.world_id
        || child.compiled().parents() != [payload.parent_genome_id.clone()]
        || parent.record().world_id != payload.world_id
        || parent.compiled().artifact_id("agent.prompt")
            != Some(payload.prompt_artifact_before.as_str())
        || child.compiled().artifact_id("agent.prompt")
            != Some(payload.prompt_artifact_after.as_str())
        || payload.artifact_name != "agent.prompt"
    {
        return Err(ControlError::Projection(
            "Forge proposal child differs from its registered lineage".to_owned(),
        ));
    }
    verify_forge_prompt(artifacts, payload)?;
    verify_forge_child_compiles(artifacts, registered, payload, world)
}

pub(super) fn verify_forge_prompt(
    artifacts: &dyn ArtifactBackend,
    payload: &ForgeProposalPayload,
) -> Result<(), ControlError> {
    let before_bytes = verified_prompt_bytes(artifacts, &payload.prompt_artifact_before)?;
    let after_bytes = verified_prompt_bytes(artifacts, &payload.prompt_artifact_after)?;
    let before_text = std::str::from_utf8(&before_bytes)
        .map_err(|_| ControlError::Protocol("Forge prompt is not UTF-8"))?;
    let after_text = std::str::from_utf8(&after_bytes)
        .map_err(|_| ControlError::Protocol("Forge prompt is not UTF-8"))?;
    let before = ReferenceInstruction::parse(before_text)
        .map_err(|_| ControlError::Protocol("Forge parent prompt is unsupported"))?;
    let after = ReferenceInstruction::parse(after_text)
        .map_err(|_| ControlError::Protocol("Forge child prompt is unsupported"))?;
    // Replay never re-derives an "expected" target the way a new proposal
    // does: it only checks that the recorded `before -> after` edge is a
    // representable catalog edge (roadmap items 8, 10, 13), so every
    // previously recordable receipt (the identity/uppercase flip) still
    // verifies, and a cluster- or Gene-derived proposal targeting any of the
    // 16 reference operations verifies the same way. World mutation-scope
    // authorization is a propose-time-only check (see `forge_prompt_mutation`).
    if mutation_edge_kind(before.operation_name(), after.operation_name()).is_none() {
        return Err(ControlError::Protocol(
            "Forge prompt mutation is not a representable catalog edge",
        ));
    }
    let edge_kind =
        mutation_edge_kind(before.operation_name(), after.operation_name()).expect("checked above");
    let expected_text = mutate_reference_instruction_document(before_text, before, after)
        .map_err(|()| ControlError::Protocol("Forge prompt is outside mutation scope"))?;
    if after_text != expected_text
        || payload.operation_before != reference_instruction_operation(before)
        || payload.operation_after != reference_instruction_operation(after)
        || payload
            .catalog_version
            .is_some_and(|version| version != MUTATION_CATALOG_VERSION)
        || payload
            .mutation_kind
            .as_deref()
            .is_some_and(|kind| kind != edge_kind.as_str())
    {
        return Err(ControlError::Projection(
            "Forge prompt mutation is not a representable one-step catalog edge".to_owned(),
        ));
    }
    Ok(())
}

pub(super) struct ForgeChildBinding<'a> {
    pub(super) proposal_id: &'a str,
    pub(super) parent_genome_id: &'a str,
    pub(super) prompt_artifact_after: &'a str,
    pub(super) world_id: &'a str,
    pub(super) child: &'a GenomeRecord,
}

pub(super) fn verify_forge_child_compiles(
    artifacts: &dyn ArtifactBackend,
    registered: &RegisteredObjects,
    payload: &ForgeProposalPayload,
    world: &CompiledWorld,
) -> Result<(), ControlError> {
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
        world,
    )
}

pub(super) fn verify_forge_child_source(
    artifacts: &dyn ArtifactBackend,
    registered: &RegisteredObjects,
    payload: &ForgeChildBinding<'_>,
    world: &CompiledWorld,
) -> Result<(), ControlError> {
    let parent = registered.genome(payload.parent_genome_id).ok_or_else(|| {
        ControlError::Projection("Forge proposal parent is not registered".to_owned())
    })?;
    let child = registered.genome(&payload.child.genome_id).ok_or_else(|| {
        ControlError::Projection("Forge proposal child is not registered".to_owned())
    })?;
    let mut expected_source: serde_json::Value =
        serde_json::from_slice(parent.compiled().canonical_json())?;
    let object = expected_source
        .as_object_mut()
        .ok_or(ControlError::Protocol("Forge parent Genome is invalid"))?;
    object.insert(
        "name".to_owned(),
        serde_json::Value::String(format!(
            "{}-forge-{}",
            parent.record().name,
            payload.proposal_id
        )),
    );
    object.insert(
        "parents".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(
            payload.parent_genome_id.to_owned(),
        )]),
    );
    object
        .get_mut("artifacts")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(ControlError::Protocol("Forge parent artifacts are invalid"))?
        .insert(
            "agent.prompt".to_owned(),
            serde_json::Value::String(payload.prompt_artifact_after.to_owned()),
        );
    let source = serde_json::to_string(&expected_source)?;
    let parents = registered
        .genomes()
        .filter(|genome| genome.record().world_id == payload.world_id)
        .map(|genome| (genome.record().genome_id.clone(), genome.compiled().clone()))
        .collect::<BTreeMap<_, _>>();
    let expected = compile_genome(&source, SourceFormat::Json, world, &parents, artifacts)
        .map_err(|_| {
            ControlError::Projection("Forge child source no longer compiles".to_owned())
        })?;
    if expected.id() != child.record().genome_id
        || expected.canonical_json() != child.compiled().canonical_json()
    {
        return Err(ControlError::Projection(
            "Forge child changes more than its single proposed prompt mutation".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn existing_forge_response(
    history: &[StoredEvent],
    payload: &ForgeProposalPayload,
) -> Result<Option<ResponseData>, ExecuteError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.proposed")
    {
        let existing = decode_forge_proposal_kind(event).map_err(|_| ExecuteError::Internal)?;
        if existing.proposal_id() == payload.proposal_id {
            let ForgeProposalKind::Catalog(existing) = existing else {
                return Err(ExecuteError::Rejected(
                    "proposal_id is already bound to a revision proposal".to_owned(),
                ));
            };
            if existing != *payload {
                return Err(ExecuteError::Rejected(
                    "proposal_id is already bound to different proposal content".to_owned(),
                ));
            }
            return Ok(Some(ResponseData::ForgeProposal {
                proposal: Box::new(forge_proposal_record(existing, event)),
            }));
        }
    }
    Ok(None)
}

pub(super) fn validate_forge_event(
    event: &StoredEvent,
    payload: &ForgeProposalPayload,
) -> Result<(), ControlError> {
    if payload.schema_version != 1
        || event.actor != OPERATOR_ACTOR
        || event.event_type != "forge.proposed"
        || event.event_id != forge_event_id(&payload.proposal_id)
        || event.aggregate_id != forge_aggregate_id(&payload.proposal_id)
    {
        return Err(ControlError::Projection(
            "Forge proposal event identity is invalid".to_owned(),
        ));
    }
    validate_job_id(&payload.proposal_id)
        .map_err(|_| ControlError::Projection("Forge proposal id is invalid".to_owned()))?;
    validate_hypothesis(&payload.hypothesis)
        .map_err(|_| ControlError::Projection("Forge hypothesis is invalid".to_owned()))?;
    Ok(())
}

pub(super) fn decode_forge_proposal(
    event: &StoredEvent,
) -> Result<ForgeProposalPayload, ControlError> {
    let payload = serde_json::from_slice::<ForgeProposalPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("Forge proposal payload is invalid".to_owned()))?;
    if payload.schema_version != 1 {
        return Err(ControlError::Projection(
            "Forge catalog proposal schema is invalid".to_owned(),
        ));
    }
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "Forge proposal payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `docs/dev/TECH_DEBT.md` TD-16).
pub(super) fn verify_forge_assessment_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_forge_assessment_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_forge_assessment_history`]; see [`EvidenceCache`].
pub(super) fn verify_forge_assessment_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
    {
        if cache.contains("forge_assessment", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let payload = decode_forge_assessment(event)?;
        if payload.schema_version != 1
            || event.actor != OPERATOR_ACTOR
            || event.event_id != forge_assessment_event_id(&payload.assessment_id)
            || event.aggregate_id != forge_aggregate_id(&payload.proposal_id)
        {
            return Err(ControlError::Projection(
                "Forge assessment event identity is invalid".to_owned(),
            ));
        }
        validate_job_id(&payload.assessment_id)
            .map_err(|_| ControlError::Projection("Forge assessment id is invalid".to_owned()))?;
        validate_job_id(&payload.proposal_id).map_err(|_| {
            ControlError::Projection("Forge assessment proposal id is invalid".to_owned())
        })?;
        let expected = forge_assessment_payload(
            artifacts,
            index,
            registered,
            &payload.assessment_id,
            &payload.proposal_id,
            &payload.selection_event_id,
        )
        .map_err(|_| ControlError::Projection("Forge assessment evidence is invalid".to_owned()))?;
        let selection_event = index.get(&payload.selection_event_id).ok_or_else(|| {
            ControlError::Projection("Forge assessment selection is missing".to_owned())
        })?;
        if selection_event.sequence >= event.sequence || payload != expected {
            return Err(ControlError::Projection(
                "Forge assessment differs from verified evidence".to_owned(),
            ));
        }
        cache.insert("forge_assessment", event);
    }
    Ok(())
}

pub(super) fn decode_forge_assessment(
    event: &StoredEvent,
) -> Result<ForgeAssessmentPayload, ControlError> {
    let payload = serde_json::from_slice::<ForgeAssessmentPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("Forge assessment payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "Forge assessment payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

pub(super) fn forge_proposal_record(
    payload: ForgeProposalPayload,
    event: &StoredEvent,
) -> ForgeProposalRecord {
    ForgeProposalRecord {
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

pub(super) fn forge_assessment_event_id(assessment_id: &str) -> String {
    format!("forge-assessment:{assessment_id}:recorded")
}

pub(super) fn forge_assessment_record(
    payload: ForgeAssessmentPayload,
    event: &StoredEvent,
) -> ForgeAssessmentRecord {
    ForgeAssessmentRecord {
        payload,
        event: ForgeAssessmentEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_hash: hex_encode(&event.hash),
        },
    }
}

pub(super) fn existing_forge_assessment_response(
    history: &[StoredEvent],
    assessment_id: &str,
    proposal_id: &str,
    selection_event_id: &str,
) -> Result<Option<ResponseData>, ExecuteError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
    {
        let existing = decode_forge_assessment(event).map_err(|_| ExecuteError::Internal)?;
        if existing.assessment_id == assessment_id {
            if existing.proposal_id != proposal_id
                || existing.selection_event_id != selection_event_id
            {
                return Err(ExecuteError::Rejected(
                    "assessment_id is already bound to different assessment content".to_owned(),
                ));
            }
            return Ok(Some(ResponseData::ForgeAssessment {
                assessment: Box::new(forge_assessment_record(existing, event)),
            }));
        }
    }
    Ok(None)
}

pub(super) fn forge_assessment_payload(
    artifacts: &dyn ArtifactBackend,
    index: &EventIndex<'_>,
    registered: &RegisteredObjects,
    assessment_id: &str,
    proposal_id: &str,
    selection_event_id: &str,
) -> Result<ForgeAssessmentPayload, ExecuteError> {
    let proposal_event_id = forge_event_id(proposal_id);
    let proposal_event = index
        .get(&proposal_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let proposal =
        decode_forge_proposal_kind(proposal_event).map_err(|_| ExecuteError::Internal)?;
    match &proposal {
        ForgeProposalKind::Catalog(payload) => validate_forge_event(proposal_event, payload),
        ForgeProposalKind::Revision(payload) => validate_revision_event(proposal_event, payload),
    }
    .map_err(|_| ExecuteError::Internal)?;
    if proposal.proposal_id() != proposal_id {
        return Err(ExecuteError::Internal);
    }

    let selection_event = index
        .get(selection_event_id)
        .ok_or(ExecuteError::NotFound)?;
    if selection_event.event_type != "selection.recorded" {
        return Err(ExecuteError::Rejected(
            "selection_event_id does not identify a selection".to_owned(),
        ));
    }
    let (routed_evaluation_id, routed_world_id) =
        selection_event_references(selection_event).map_err(|_| ExecuteError::Internal)?;
    let world = registered
        .world(&routed_world_id)
        .ok_or(ExecuteError::Internal)?;
    let selected = verify_selection_event_in(index, artifacts, selection_event, world.compiled())
        .map_err(|_| ExecuteError::Internal)?;
    let receipt = selected.receipt().clone();
    let verified_selection_event_id = selected.event().event_id.clone();
    let selection_event_hash = selected.event().event_hash.clone();
    let selection_receipt_artifact_id = selected.event().receipt_artifact_id.clone();
    let selection_sequence = selected.event().sequence;

    let evaluation_event = index
        .get(receipt.evaluation_event_id())
        .ok_or(ExecuteError::Internal)?;
    if evaluation_event.event_type != "evaluation.recorded"
        || hex_encode(&evaluation_event.hash) != receipt.evaluation_event_hash()
    {
        return Err(ExecuteError::Internal);
    }
    if proposal_event.sequence >= evaluation_event.sequence
        || evaluation_event.sequence >= selection_sequence
    {
        return Err(ExecuteError::Rejected(
            "Forge assessment evidence is out of order".to_owned(),
        ));
    }
    if routed_evaluation_id != receipt.evaluation_id()
        || routed_world_id != receipt.world_id()
        || receipt.world_id() != proposal.world_id()
        || receipt.parent_genome_id() != proposal.parent_genome_id()
        || receipt.candidate_genome_id() != proposal.child().genome_id
    {
        return Err(ExecuteError::Rejected(
            "selection evidence does not match the proposed child".to_owned(),
        ));
    }

    Ok(ForgeAssessmentPayload {
        schema_version: 1,
        assessment_id: assessment_id.to_owned(),
        proposal_id: proposal_id.to_owned(),
        proposal_event_id: proposal_event.event_id.clone(),
        proposal_event_hash: hex_encode(&proposal_event.hash),
        selection_event_id: verified_selection_event_id,
        selection_event_hash,
        selection_receipt_artifact_id,
        evaluation_id: receipt.evaluation_id().to_owned(),
        evaluation_event_id: receipt.evaluation_event_id().to_owned(),
        evaluation_event_hash: receipt.evaluation_event_hash().to_owned(),
        world_id: receipt.world_id().to_owned(),
        parent_genome_id: receipt.parent_genome_id().to_owned(),
        child_genome_id: receipt.candidate_genome_id().to_owned(),
        outcome: if receipt.metrics_eligible() {
            ForgeAssessmentOutcome::MetricsPassed
        } else {
            ForgeAssessmentOutcome::MetricsRejected
        },
        invariant_gate_verified: false,
        promotion_eligible: false,
    })
}

pub(super) fn forge_event_id(proposal_id: &str) -> String {
    format!("forge:{proposal_id}:proposed")
}

pub(super) fn forge_aggregate_id(proposal_id: &str) -> String {
    format!("forge:{proposal_id}")
}

// Every identifier one evolution generation derives is `evolve-{run_id}-g{n}-<tag>`,
// which satisfies `validate_job_id` (alphanumeric, `-`, `_`, `.` only) exactly
// like any other caller-selected idempotency key, so each of these can be
// submitted through the ordinary Arena, Forge, and Champion command paths.
pub(super) fn evolution_diagnostic_evaluation_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-d")
}

fn evolution_child_evaluation_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-c")
}

fn evolution_proposal_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-p")
}

fn evolution_assessment_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-a")
}

pub(super) fn evolution_promotion_transition_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-x")
}

pub(super) fn evolution_analysis_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-analysis")
}

// TD-17 (roadmap items 10, 13): a generation's rank-0 candidate keeps
// exactly today's unsuffixed id (so a single-candidate generation --
// candidate_count == 1, or no bound strategy -- is byte-for-byte identical
// to a generation recorded before multi-candidate proposals existed); every
// other ranked candidate gets a distinct, still `validate_job_id`-legal
// suffix.
pub(super) fn evolution_candidate_proposal_id(
    run_id: &str,
    generation_index: u32,
    rank: u32,
) -> String {
    let base = evolution_proposal_id(run_id, generation_index);
    if rank == 0 {
        base
    } else {
        format!("{base}-c{rank}")
    }
}

pub(super) fn evolution_candidate_child_evaluation_id(
    run_id: &str,
    generation_index: u32,
    rank: u32,
) -> String {
    let base = evolution_child_evaluation_id(run_id, generation_index);
    if rank == 0 {
        base
    } else {
        format!("{base}-c{rank}")
    }
}

pub(super) fn evolution_candidate_assessment_id(
    run_id: &str,
    generation_index: u32,
    rank: u32,
) -> String {
    let base = evolution_assessment_id(run_id, generation_index);
    if rank == 0 {
        base
    } else {
        format!("{base}-c{rank}")
    }
}

pub(super) fn validate_hypothesis(hypothesis: &str) -> Result<(), ExecuteError> {
    if hypothesis.trim().is_empty()
        || hypothesis.len() > 512
        || hypothesis.chars().any(char::is_control)
    {
        return Err(ExecuteError::Invalid(
            "hypothesis must be 1 to 512 printable UTF-8 bytes",
        ));
    }
    Ok(())
}

pub(super) fn verified_prompt_bytes(
    artifacts: &dyn ArtifactBackend,
    artifact_id: &str,
) -> Result<Vec<u8>, ControlError> {
    let id = ArtifactId::parse(artifact_id.to_owned())?;
    Ok(artifacts.get(&id)?)
}

pub(super) fn reference_instruction_operation(instruction: ReferenceInstruction) -> &'static str {
    instruction.operation_name()
}

pub(super) fn reference_instruction_document(instruction: ReferenceInstruction) -> String {
    format!(
        "```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{}\"}}\n```",
        reference_instruction_operation(instruction)
    )
}

pub(super) fn compile_forge_child(
    registered: &RegisteredObjects,
    world: &CompiledWorld,
    artifact_store: &dyn ArtifactBackend,
    parent_genome_id: &str,
    world_id: &str,
    proposal_id: &str,
    prompt_artifact: &str,
) -> Result<GenomeRecord, ExecuteError> {
    let parent = registered
        .genome(parent_genome_id)
        .ok_or(ExecuteError::NotFound)?;
    let mut source: serde_json::Value = serde_json::from_slice(parent.compiled().canonical_json())
        .map_err(|_| ExecuteError::Internal)?;
    let object = source.as_object_mut().ok_or(ExecuteError::Internal)?;
    object.insert(
        "name".to_owned(),
        serde_json::Value::String(format!("{}-forge-{proposal_id}", parent.record().name)),
    );
    object.insert(
        "parents".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(parent_genome_id.to_owned())]),
    );
    object
        .get_mut("artifacts")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(ExecuteError::Internal)?
        .insert(
            "agent.prompt".to_owned(),
            serde_json::Value::String(prompt_artifact.to_owned()),
        );
    let source = serde_json::to_string(&source).map_err(|_| ExecuteError::Internal)?;
    let parents = registered
        .genomes()
        .filter(|genome| genome.record().world_id == world_id)
        .map(|genome| (genome.record().genome_id.clone(), genome.compiled().clone()))
        .collect::<BTreeMap<_, _>>();
    let child = compile_genome(&source, SourceFormat::Json, world, &parents, artifact_store)
        .map_err(|error| ExecuteError::Rejected(format!("Forge child rejected: {error}")))?;
    let artifact = artifact_store
        .put(child.canonical_json())
        .map_err(|_| ExecuteError::Internal)?;
    Ok(GenomeRecord {
        genome_id: child.id().to_owned(),
        name: child.name().to_owned(),
        world_id: world_id.to_owned(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: child.parents().to_vec(),
    })
}

pub(super) fn verified_forge_source(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    selection_event_id: &str,
    parent_genome_id: &str,
) -> Result<(String, String, String), ExecuteError> {
    verified_forge_source_in(
        artifacts,
        &EventIndex::build(history),
        registered,
        selection_event_id,
        parent_genome_id,
    )
}

pub(super) fn verified_forge_source_in(
    artifacts: &dyn ArtifactBackend,
    index: &EventIndex<'_>,
    registered: &RegisteredObjects,
    selection_event_id: &str,
    parent_genome_id: &str,
) -> Result<(String, String, String), ExecuteError> {
    let event = index
        .get(selection_event_id)
        .ok_or(ExecuteError::NotFound)?;
    if event.event_type != "selection.recorded" {
        return Err(ExecuteError::Rejected(
            "selection_event_id does not identify a selection".to_owned(),
        ));
    }
    let (evaluation_id, world_id) =
        selection_event_references(event).map_err(|_| ExecuteError::Internal)?;
    let world = registered.world(&world_id).ok_or(ExecuteError::Internal)?;
    let selection = verify_selection_event_in(index, artifacts, event, world.compiled())
        .map_err(|_| ExecuteError::Internal)?;
    let receipt = selection.receipt().clone();
    let selection_hash = selection.event().event_hash.clone();
    if receipt.evaluation_id() != evaluation_id
        || receipt.world_id() != world_id
        || receipt.candidate_genome_id() != parent_genome_id
    {
        return Err(ExecuteError::Rejected(
            "the parent must be the selected candidate under the same World".to_owned(),
        ));
    }
    Ok((selection_hash, evaluation_id, world_id))
}

/// The two mutually exclusive ways a Forge proposal supplies its hypothesis.
pub(super) enum ForgeHypothesisSource {
    /// The unchanged operator-authored path.
    Operator(String),
    /// A verified `forge.clustered` analysis and cluster index within it.
    Analysis {
        analysis_id: String,
        cluster_index: u32,
        /// Use the bound cluster's `secondary_suggested_mutation` instead of
        /// its primary `suggested_mutation` (TD-17, roadmap items 10, 13).
        /// Always `false` outside a `candidate_count > 1` generation's
        /// non-highest-ranked candidates.
        use_secondary: bool,
    },
}

/// Parses a bare reference-operation name (as recorded in a `ForgeProposalPayload`,
/// `SuggestedMutation::ReferenceOperation`, or a Gene's `operation_after`)
/// into a [`ReferenceInstruction`] by round-tripping it through the same
/// strict document parser every prompt artifact uses.
fn parse_reference_operation_name(operation: &str) -> Result<ReferenceInstruction, ExecuteError> {
    let document = format!(
        "```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{operation}\"}}\n```"
    );
    ReferenceInstruction::parse(&document).map_err(|_| ExecuteError::Internal)
}

/// Best-effort lookup of a registered Genome's `agent.prompt` reference
/// operation. Returns `None` for any Genome without a supported prompt
/// (unknown Genome, no `agent.prompt` artifact, unreadable or unparsable
/// content) rather than failing: callers use this only as a deterministic
/// hint for choosing a mutation target, and every choice it feeds into is
/// independently re-verified against catalog edges and canonical bytes.
pub(super) fn genome_reference_instruction(
    registered: &RegisteredObjects,
    artifacts: &dyn ArtifactBackend,
    genome_id: &str,
) -> Option<ReferenceInstruction> {
    let genome = registered.genome(genome_id)?;
    let artifact = genome.compiled().artifact_id("agent.prompt")?;
    let id = ArtifactId::parse(artifact.to_owned()).ok()?;
    let bytes = artifacts.get(&id).ok()?;
    let body = std::str::from_utf8(&bytes).ok()?;
    ReferenceInstruction::parse(body).ok()
}

/// Deterministic Gene Bank lookup for `EvolverStrategyConfig::gene_selection
/// == HighestTransferEffect` (roadmap items 8, 10, 13): among every
/// extracted Gene whose `operation_before` equals `current_operation`,
/// returns the `operation_after` of the one with the highest mean
/// `estimate_bps` across its `Positive`-outcome transfer trials, requiring
/// at least one such trial. Ties break on the lexicographically smallest
/// `gene_id` for determinism. Returns `None` when no such Gene exists.
pub(super) fn best_gene_target_operation(
    history: &[StoredEvent],
    current_operation: &str,
) -> Option<String> {
    let mut best: Option<(i64, String, String)> = None; // (mean_bps, gene_id, operation_after)
    for gene_event in history
        .iter()
        .filter(|event| event.event_type == GENE_EVENT_TYPE)
    {
        let Ok(gene) = decode_gene_extracted(gene_event) else {
            continue;
        };
        if gene.operation_before != current_operation {
            continue;
        }
        let mut total: i64 = 0;
        let mut count: i64 = 0;
        for transfer_event in history
            .iter()
            .filter(|event| event.event_type == TRANSFER_RECORDED_EVENT_TYPE)
        {
            let Ok(recorded) = decode_transfer_recorded(transfer_event) else {
                continue;
            };
            if recorded.gene_id != gene.gene_id || recorded.outcome != GeneTransferOutcome::Positive
            {
                continue;
            }
            total += recorded.estimate_bps;
            count += 1;
        }
        if count == 0 {
            continue;
        }
        let mean = total / count;
        let better = best.as_ref().is_none_or(|(best_mean, best_gene_id, _)| {
            mean > *best_mean || (mean == *best_mean && gene.gene_id < *best_gene_id)
        });
        if better {
            best = Some((mean, gene.gene_id, gene.operation_after));
        }
    }
    best.map(|(_, _, operation_after)| operation_after)
}

/// Resolves the hypothesis text and, for an analysis-derived proposal, the
/// binding recorded in the proposal payload plus an explicit mutation target
/// when the bound cluster names one. This never proposes, mutates, or
/// promotes anything; it only reads and verifies already-recorded evidence.
pub(super) fn resolve_forge_hypothesis(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    world: &CompiledWorld,
    evaluation_id: &str,
    parent_genome_id: &str,
    source: ForgeHypothesisSource,
) -> Result<
    (
        String,
        Option<ForgeAnalysisBinding>,
        Option<ReferenceInstruction>,
    ),
    ExecuteError,
> {
    match source {
        ForgeHypothesisSource::Operator(hypothesis) => {
            validate_hypothesis(&hypothesis)?;
            Ok((hypothesis, None, None))
        }
        ForgeHypothesisSource::Analysis {
            analysis_id,
            cluster_index,
            use_secondary,
        } => {
            let event_id = format!("{CLUSTER_EVENT_PREFIX}{analysis_id}:clustered");
            let event = history
                .iter()
                .find(|event| event.event_id == event_id)
                .ok_or(ExecuteError::NotFound)?;
            let current_operation =
                genome_reference_instruction(registered, artifacts, parent_genome_id)
                    .map(ReferenceInstruction::operation_name);
            let verified = verify_cluster_event_in(
                &EventIndex::build(history),
                artifacts,
                event,
                world,
                current_operation,
            )
            .map_err(|_| ExecuteError::Internal)?;
            let analysis = verified.analysis().clone();
            let analysis_event_hash = verified.event().event_hash.clone();
            if analysis.evaluation_id != evaluation_id
                || analysis.candidate_genome_id != parent_genome_id
            {
                // Forge's "parent" for this proposal is the winning candidate
                // from the analyzed evaluation: the same role `verified_forge_source`
                // requires the selection's `candidate_genome_id` to match.
                return Err(ExecuteError::Rejected(
                    "the bound analysis must have clustered the exact same selected candidate"
                        .to_owned(),
                ));
            }
            let cluster = analysis
                .clusters
                .get(usize::try_from(cluster_index).map_err(|_| ExecuteError::NotFound)?)
                .ok_or(ExecuteError::NotFound)?;
            let chosen_mutation = if use_secondary {
                cluster.secondary_suggested_mutation.as_ref()
            } else {
                cluster.suggested_mutation.as_ref()
            };
            let target = match chosen_mutation {
                Some(SuggestedMutation::ReferenceOperationFlip) => None,
                Some(SuggestedMutation::ReferenceOperation { operation_after }) => {
                    Some(parse_reference_operation_name(operation_after)?)
                }
                None => {
                    return Err(ExecuteError::Rejected(
                        "the selected cluster has no supported mutation".to_owned(),
                    ));
                }
            };
            let mutation_slot = use_secondary.then_some(MutationSlot::Secondary);
            Ok((
                cluster.hypothesis.clone(),
                Some(ForgeAnalysisBinding {
                    analysis_id,
                    analysis_event_id: event_id,
                    analysis_event_hash,
                    cluster_index,
                    cluster_signature: cluster.signature.clone(),
                    mutation_slot,
                }),
                target,
            ))
        }
    }
}

/// Proposes (or validates an explicit) one-step mutation of `parent_genome_id`'s
/// `agent.prompt` artifact.
///
/// Authorization is enforced here, on the propose path only: a proposal for
/// `agent.prompt` requires `MutationTarget::Harness` in the World's
/// `mutation_scope` (roadmap items 8, 10, 13). Replay of an already-recorded
/// `forge.proposed` event does not re-check World scope (see
/// `verify_forge_prompt`), only that the recorded edge is a representable
/// catalog edge, so proposals recorded before this field existed still
/// verify.
///
/// `target`, when supplied, must be a representable [`mutation_catalog`]
/// edge from the parent's current operation (any of the 16 reference
/// operations); this is how a cluster- or Gene-derived hypothesis reaches a
/// Gauntlet fix, not only the casing flip. Without `target`, the default
/// one-step mutation is the historical `identity`/`ascii_uppercase` flip; any
/// other current operation is rejected as outside Forge's default mutation
/// (an explicit `target` is required to mutate a Gauntlet operation).
pub(super) fn forge_prompt_mutation(
    artifacts: &dyn ArtifactBackend,
    registered: &RegisteredObjects,
    parent_genome_id: &str,
    world: &CompiledWorld,
    target: Option<ReferenceInstruction>,
) -> Result<(String, ReferenceInstruction, ReferenceInstruction, String), ExecuteError> {
    if !world.mutation_scope().contains(&MutationTarget::Harness) {
        return Err(ExecuteError::Rejected(
            "World mutation scope does not authorize harness mutations".to_owned(),
        ));
    }
    let parent = registered
        .genome(parent_genome_id)
        .filter(|genome| genome.record().world_id == world.id())
        .ok_or(ExecuteError::NotFound)?;
    let prompt_before = parent
        .compiled()
        .artifact_id("agent.prompt")
        .ok_or_else(|| {
            ExecuteError::Rejected(
                "the selected candidate has no supported prompt to mutate".to_owned(),
            )
        })?
        .to_owned();
    let prompt_id = ArtifactId::parse(prompt_before.clone()).map_err(|_| ExecuteError::Internal)?;
    let prompt_bytes = artifacts
        .get(&prompt_id)
        .map_err(|_| ExecuteError::Internal)?;
    let prompt_text = std::str::from_utf8(&prompt_bytes).map_err(|_| ExecuteError::Internal)?;
    let before = ReferenceInstruction::parse(prompt_text).map_err(|_| {
        ExecuteError::Rejected(
            "the selected candidate prompt is outside the supported mutation language".to_owned(),
        )
    })?;
    let after = match target {
        Some(explicit) => {
            if !is_catalog_edge(before.operation_name(), explicit.operation_name()) {
                return Err(ExecuteError::Rejected(
                    "the requested mutation target is not a representable catalog edge".to_owned(),
                ));
            }
            explicit
        }
        None => match before {
            ReferenceInstruction::Identity => ReferenceInstruction::AsciiUppercase,
            ReferenceInstruction::AsciiUppercase => ReferenceInstruction::Identity,
            // Every Gauntlet-mode operation (roadmap item 10) needs an
            // explicit catalog-derived target; there is no default flip for
            // it.
            _ => {
                return Err(ExecuteError::Rejected(
                    "the selected candidate prompt is outside the Forge mutation scope".to_owned(),
                ));
            }
        },
    };
    let after_text =
        mutate_reference_instruction_document(prompt_text, before, after).map_err(|()| {
            ExecuteError::Rejected(
                "the selected candidate prompt is outside the Forge mutation scope".to_owned(),
            )
        })?;
    Ok((prompt_before, before, after, after_text))
}

pub(super) fn mutate_reference_instruction_document(
    prompt_text: &str,
    before: ReferenceInstruction,
    after: ReferenceInstruction,
) -> Result<String, ()> {
    let normalized = prompt_text.replace("\r\n", "\n");
    let canonical = reference_instruction_document(before);
    if normalized != canonical && normalized != format!("{canonical}\n") {
        return Err(());
    }
    let old = format!(
        "\"operation\":\"{}\"",
        reference_instruction_operation(before)
    );
    let new = format!(
        "\"operation\":\"{}\"",
        reference_instruction_operation(after)
    );
    if prompt_text.matches(&old).count() != 1 {
        return Err(());
    }
    Ok(prompt_text.replacen(&old, &new, 1))
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `docs/dev/TECH_DEBT.md` TD-16).
pub(super) fn verify_selection_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_selection_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_selection_history`]; see [`EvidenceCache`].
pub(super) fn verify_selection_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for event in history
        .iter()
        .filter(|event| event.event_type == "selection.recorded")
    {
        if cache.contains("selection", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        // The World identity in the event envelope is only a routing hint. Arena
        // independently recomputes the evaluation receipt and compares the full
        // canonical selection event against this registered World's policy.
        let (_evaluation_id, world_id) = selection_event_references(event).map_err(|_| {
            ControlError::Projection("canonical selection event is invalid".to_owned())
        })?;
        let world = registered.world(&world_id).ok_or_else(|| {
            ControlError::Projection("selection World is not registered".to_owned())
        })?;
        verify_selection_event_in(index, artifacts, event, world.compiled()).map_err(|_| {
            ControlError::Projection("canonical selection receipt is invalid".to_owned())
        })?;
        cache.insert("selection", event);
    }
    Ok(())
}

/// `invariant_event_references` already canonically validates the full
/// envelope (schema version, identity fields, artifact-id shape); this only
/// pulls `receipt_artifact_id` back out for the reference-level cross-check
/// in `verify_invariant_history_with` (see `docs/dev/TECH_DEBT.md` TD-4).
#[derive(Deserialize)]
pub(super) struct InvariantEventEnvelope {
    receipt_artifact_id: String,
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `docs/dev/TECH_DEBT.md` TD-16).
pub(super) fn verify_invariant_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_invariant_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_invariant_history`]; see [`EvidenceCache`].
pub(super) fn verify_invariant_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for event in history.iter().filter(|event| {
        event.event_type == "invariants.recorded"
            || event.event_id.starts_with("arena:invariants:")
            || event.aggregate_id.starts_with("arena:invariants:")
    }) {
        if cache.contains("invariant", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let (evaluation_id, world_id) = invariant_event_references(event).map_err(|_| {
            ControlError::Projection("canonical invariant event envelope is invalid".to_owned())
        })?;
        let envelope: InvariantEventEnvelope =
            serde_json::from_slice(&event.payload).map_err(|_| {
                ControlError::Projection("canonical invariant event envelope is invalid".to_owned())
            })?;
        let world = registered.world(&world_id).ok_or_else(|| {
            ControlError::Projection("invariant World is not registered".to_owned())
        })?;
        let verified =
            verify_reference_output_invariant_event_in(index, artifacts, event, world.compiled())
                .map_err(|_| {
                ControlError::Projection("canonical invariant receipt is invalid".to_owned())
            })?;
        if verified.receipt().evaluation_id != evaluation_id
            || verified.receipt().world_id != world_id
            || verified.event().receipt_artifact_id != envelope.receipt_artifact_id
        {
            return Err(ControlError::Projection(
                "canonical invariant event differs from verified receipt".to_owned(),
            ));
        }
        cache.insert("invariant", event);
    }
    Ok(())
}

/// `cluster_event_references` already canonically validates the full
/// envelope (schema version, identity fields, artifact-id shape); this only
/// pulls `analysis_artifact_id` back out for the reference-level cross-check
/// in `verify_cluster_history` (see `docs/dev/TECH_DEBT.md` TD-4).
#[derive(Deserialize)]
pub(super) struct ClusterEventEnvelope {
    analysis_artifact_id: String,
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `docs/dev/TECH_DEBT.md` TD-16).
pub(super) fn verify_cluster_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_cluster_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_cluster_history`]; see [`EvidenceCache`].
pub(super) fn verify_cluster_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only when at least one cluster event exists, so the common case
    // (no cluster analyses recorded yet) never pays the O(history length)
    // cost of indexing it.
    let mut index = None;
    for event in history.iter().filter(|event| {
        event.event_type == "forge.clustered"
            || event.event_id.starts_with(CLUSTER_EVENT_PREFIX)
            || event.aggregate_id.starts_with(CLUSTER_EVENT_PREFIX)
    }) {
        if cache.contains("cluster", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let (_analysis_id, evaluation_id, world_id) =
            cluster_event_references(event).map_err(|_| {
                ControlError::Projection("canonical cluster event envelope is invalid".to_owned())
            })?;
        let envelope: ClusterEventEnvelope =
            serde_json::from_slice(&event.payload).map_err(|_| {
                ControlError::Projection("canonical cluster event envelope is invalid".to_owned())
            })?;
        let world = registered.world(&world_id).ok_or_else(|| {
            ControlError::Projection("cluster World is not registered".to_owned())
        })?;
        // Re-derive the candidate's current operation the same deterministic
        // way the control plane did when the analysis was first computed
        // (roadmap items 8, 10, 13), so a `failure-cluster-v2` analysis
        // recomputes byte-identically on replay.
        let current_operation = load_recorded_evaluation_in(index, artifacts, &evaluation_id)
            .ok()
            .and_then(|recorded| {
                genome_reference_instruction(
                    registered,
                    artifacts,
                    &recorded.summary.candidate_genome_id,
                )
            })
            .map(ReferenceInstruction::operation_name);
        let verified =
            verify_cluster_event_in(index, artifacts, event, world.compiled(), current_operation)
                .map_err(|_| {
                ControlError::Projection("canonical cluster analysis is invalid".to_owned())
            })?;
        if verified.analysis().evaluation_id != evaluation_id
            || verified.analysis().world_id != world_id
            || verified.event().analysis_artifact_id != envelope.analysis_artifact_id
        {
            return Err(ControlError::Projection(
                "canonical cluster event differs from verified analysis".to_owned(),
            ));
        }
        cache.insert("cluster", event);
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
pub(super) fn require_command_fields(command: &Command) -> Result<(), ExecuteError> {
    if let Command::GenomeProposalShow { proposal_id } = command {
        validate_job_id(proposal_id)?;
    }
    if let Command::GenomeShow { genome_id }
    | Command::GenomePrompt { genome_id }
    | Command::GenomeProfile { genome_id } = command
        && genome_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("genome_id is required"));
    }
    if let Command::WorldShow { world_id } | Command::GenomeRegister { world_id, .. } = command
        && world_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("world_id is required"));
    }
    if let Command::WorldRegister { path }
    | Command::GenomeRegister { path, .. }
    | Command::ArtifactPut { path } = command
        && path.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("path is required"));
    }
    if let Command::RunSubmit { job_id, genome_id } = command
        && (job_id.trim().is_empty() || genome_id.trim().is_empty())
    {
        return Err(ExecuteError::Invalid("job_id and genome_id are required"));
    }
    if let Command::GenomePropose {
        proposal_id,
        selection_event_id,
        parent_genome_id,
        hypothesis,
        analysis_id,
        cluster_index,
    } = command
    {
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        if selection_event_id.trim().is_empty() || parent_genome_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "selection_event_id and parent_genome_id are required",
            ));
        }
        match (hypothesis, analysis_id, cluster_index) {
            (Some(hypothesis), None, None) => validate_hypothesis(hypothesis)?,
            (None, Some(analysis_id), Some(_)) => {
                validate_job_id(analysis_id)
                    .map_err(|_| ExecuteError::Invalid("analysis_id is invalid"))?;
            }
            _ => {
                return Err(ExecuteError::Invalid(
                    "exactly one of hypothesis or (analysis_id and cluster_index) is required",
                ));
            }
        }
    }
    if let Command::GenomeRevise {
        proposal_id,
        selection_event_id,
        parent_genome_id,
        prompt_path,
        hypothesis,
    } = command
    {
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        if selection_event_id.trim().is_empty() || parent_genome_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "selection_event_id and parent_genome_id are required",
            ));
        }
        if prompt_path.trim().is_empty() {
            return Err(ExecuteError::Invalid("prompt_path is required"));
        }
        validate_hypothesis(hypothesis)?;
    }
    if let Command::GenomeAssess {
        assessment_id,
        proposal_id,
        selection_event_id,
    } = command
    {
        validate_job_id(assessment_id)
            .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))?;
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        if selection_event_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("selection_event_id is required"));
        }
    }
    if let Command::JobStatus { job_id } | Command::JobKill { job_id } = command
        && job_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("job_id is required"));
    }
    if let Command::RunReference { genome_id } | Command::RunEvaluation { genome_id, .. } = &command
        && genome_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("genome_id is required"));
    }
    if let Command::EvaluatePair {
        evaluation_id,
        parent_genome_id,
        candidate_genome_id,
        remote: _,
    }
    | Command::EvaluatePairConfirmed {
        evaluation_id,
        parent_genome_id,
        candidate_genome_id,
        ..
    } = command
        && (evaluation_id.trim().is_empty()
            || parent_genome_id.trim().is_empty()
            || candidate_genome_id.trim().is_empty())
    {
        return Err(ExecuteError::Invalid(
            "evaluation and Genome identifiers are required",
        ));
    }
    if let Command::ArenaSelect { evaluation_id } | Command::ArenaSelectionShow { evaluation_id } =
        command
        && evaluation_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("evaluation_id is required"));
    }
    if let Command::ArenaInvariants { evaluation_id } = command
        && evaluation_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("evaluation_id is required"));
    }
    if let Command::ForgeAnalyze {
        analysis_id,
        evaluation_id,
    } = command
    {
        validate_job_id(analysis_id)
            .map_err(|_| ExecuteError::Invalid("analysis_id is invalid"))?;
        if evaluation_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("evaluation_id is required"));
        }
    }
    if let Command::RunList { limit }
    | Command::EvaluationList { limit }
    | Command::DenialList { limit }
    | Command::MetaList { limit }
    | Command::DriftList { limit }
    | Command::CanaryList { limit } = command
        && (*limit == 0 || *limit > MAX_LIST_LIMIT)
    {
        return Err(ExecuteError::Invalid("limit must be between 1 and 200"));
    }
    if let Command::EvolveStart {
        run_id,
        world_id,
        from_genome_id,
        generations,
        budget,
        strategy_id: _,
    } = command
    {
        validate_job_id(run_id).map_err(|_| ExecuteError::Invalid("run_id is invalid"))?;
        if run_id.len() > MAX_EVOLUTION_RUN_ID_BYTES {
            return Err(ExecuteError::Invalid(
                "run_id must leave room for the run's derived identifiers",
            ));
        }
        if world_id.trim().is_empty() || from_genome_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "world_id and from_genome_id are required",
            ));
        }
        if *generations == 0 {
            return Err(ExecuteError::Invalid("generations must be positive"));
        }
        if *budget < TRIALS_PER_GENERATION {
            return Err(ExecuteError::Invalid(
                "budget must allow at least one generation",
            ));
        }
    }
    if let Command::EvolveStatus { run_id } | Command::EvolveCancel { run_id } = command
        && validate_job_id(run_id).is_err()
    {
        return Err(ExecuteError::Invalid("run_id is invalid"));
    }
    if let Command::McpCall {
        client_id,
        tool,
        decision,
        ..
    } = command
    {
        if client_id.trim().is_empty() || tool.trim().is_empty() {
            return Err(ExecuteError::Invalid("client_id and tool are required"));
        }
        if let McpDecision::Allowed { command: inner } = decision
            && matches!(**inner, Command::McpCall { .. })
        {
            return Err(ExecuteError::Invalid("mcp_call must not nest mcp_call"));
        }
    }
    if let Command::WorkerCredentialMint {
        worker_id,
        ttl_seconds,
    } = command
        && (worker_id.trim().is_empty()
            || *ttl_seconds == 0
            || *ttl_seconds > MAX_WORKER_TTL_SECONDS)
    {
        return Err(ExecuteError::Invalid(
            "worker_id is required and ttl_seconds must be between 1 and the maximum",
        ));
    }
    if let Command::WorkerCredentialRevoke { credential_id } = command
        && credential_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("credential_id is required"));
    }
    if let Command::RemoteRunSubmit { job_id, genome_id } = command {
        validate_job_id(job_id).map_err(|_| ExecuteError::Invalid("job_id is invalid"))?;
        if genome_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("genome_id is required"));
        }
    }
    if let Command::RemoteJobStatus { job_id } = command
        && job_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("job_id is required"));
    }
    if let Command::MetaStrategyRegister { path } = command
        && path.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("path is required"));
    }
    if let Command::MetaStrategyShow { strategy_id } = command
        && strategy_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("strategy_id is required"));
    }
    if let Command::MetaEvaluate {
        meta_run_id,
        strategy_a_id,
        strategy_b_id,
        lineages,
        confidence_bps,
        bootstrap_seed: _,
    } = command
    {
        validate_job_id(meta_run_id)
            .map_err(|_| ExecuteError::Invalid("meta_run_id is invalid"))?;
        if meta_run_id.len() > MAX_META_RUN_ID_BYTES {
            return Err(ExecuteError::Invalid(
                "meta_run_id must leave room for its derived run identifiers",
            ));
        }
        if strategy_a_id.trim().is_empty() || strategy_b_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "strategy_a_id and strategy_b_id are required",
            ));
        }
        if strategy_a_id == strategy_b_id {
            return Err(ExecuteError::Invalid(
                "strategy_a_id and strategy_b_id must differ",
            ));
        }
        if lineages.len() < 2 {
            return Err(ExecuteError::Invalid(
                "at least two held-out lineages are required for a bootstrap comparison",
            ));
        }
        let mut worlds = std::collections::BTreeSet::new();
        for lineage in lineages {
            if lineage.world_id.trim().is_empty() || lineage.from_genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "lineage world_id and from_genome_id are required",
                ));
            }
            if !worlds.insert(lineage.world_id.clone()) {
                return Err(ExecuteError::Invalid(
                    "held-out lineages must use distinct Worlds",
                ));
            }
        }
        if *confidence_bps == 0 || *confidence_bps >= 10_000 {
            return Err(ExecuteError::Invalid(
                "confidence_bps must be between 1 and 9999",
            ));
        }
    }
    if let Command::MetaShow { meta_run_id } | Command::MetaStatus { meta_run_id } = command
        && validate_job_id(meta_run_id).is_err()
    {
        return Err(ExecuteError::Invalid("meta_run_id is invalid"));
    }
    require_champion_fields(command)?;
    require_drift_and_canary_fields(command)?;
    require_gene_fields(command)
}

fn require_gene_fields(command: &Command) -> Result<(), ExecuteError> {
    match command {
        Command::GeneExtract {
            gene_id,
            promotion_transition_id,
        } => {
            validate_job_id(gene_id).map_err(|_| ExecuteError::Invalid("gene_id is invalid"))?;
            validate_job_id(promotion_transition_id)
                .map_err(|_| ExecuteError::Invalid("promotion_transition_id is invalid"))?;
        }
        Command::GeneTransfer {
            trial_id,
            gene_id,
            to_genome_id,
        } => {
            validate_job_id(trial_id).map_err(|_| ExecuteError::Invalid("trial_id is invalid"))?;
            if gene_id.trim().is_empty() || to_genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "gene_id and to_genome_id are required",
                ));
            }
        }
        Command::GeneRecord {
            trial_id,
            evaluation_id,
        } => {
            validate_job_id(trial_id).map_err(|_| ExecuteError::Invalid("trial_id is invalid"))?;
            if evaluation_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("evaluation_id is required"));
            }
        }
        Command::GeneShow { gene_id } if gene_id.trim().is_empty() => {
            return Err(ExecuteError::Invalid("gene_id is required"));
        }
        Command::GeneSpeciate {
            species_id,
            gene_id,
            domain_world_id,
        } => {
            validate_job_id(species_id)
                .map_err(|_| ExecuteError::Invalid("species_id is invalid"))?;
            if gene_id.trim().is_empty() || domain_world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "gene_id and domain_world_id are required",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn require_champion_fields(command: &Command) -> Result<(), ExecuteError> {
    let transition_id = match command {
        Command::ChampionSeed {
            transition_id,
            world_id,
            genome_id,
            reason,
        } => {
            if world_id.trim().is_empty() || genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id and genome_id are required"));
            }
            validate_reason(reason)?;
            transition_id
        }
        Command::ChampionPromote {
            transition_id,
            assessment_id,
        } => {
            validate_job_id(assessment_id)
                .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))?;
            transition_id
        }
        Command::ChampionRollback {
            transition_id,
            world_id,
            reason,
        } => {
            if world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id is required"));
            }
            validate_reason(reason)?;
            transition_id
        }
        Command::ChampionShow { world_id } => {
            if world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id is required"));
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    validate_job_id(transition_id).map_err(|_| ExecuteError::Invalid("transition_id is invalid"))
}

fn require_drift_and_canary_fields(command: &Command) -> Result<(), ExecuteError> {
    match command {
        Command::DriftRecord {
            drift_id,
            world_id,
            evidence_evaluation_id,
            ..
        } => {
            validate_job_id(drift_id).map_err(|_| ExecuteError::Invalid("drift_id is invalid"))?;
            if world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id is required"));
            }
            validate_job_id(evidence_evaluation_id)
                .map_err(|_| ExecuteError::Invalid("evidence_evaluation_id is invalid"))
        }
        Command::DriftShow { drift_id } => {
            validate_job_id(drift_id).map_err(|_| ExecuteError::Invalid("drift_id is invalid"))
        }
        Command::CanaryStart {
            canary_id,
            world_id,
            candidate_genome_id,
            assessment_id,
        } => {
            validate_job_id(canary_id)
                .map_err(|_| ExecuteError::Invalid("canary_id is invalid"))?;
            if world_id.trim().is_empty() || candidate_genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "world_id and candidate_genome_id are required",
                ));
            }
            validate_job_id(assessment_id)
                .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))
        }
        Command::CanaryAdvance {
            canary_id,
            evidence_evaluation_id,
        }
        | Command::CanaryLiveCheck {
            canary_id,
            evidence_evaluation_id,
        } => {
            validate_job_id(canary_id)
                .map_err(|_| ExecuteError::Invalid("canary_id is invalid"))?;
            validate_job_id(evidence_evaluation_id)
                .map_err(|_| ExecuteError::Invalid("evidence_evaluation_id is invalid"))
        }
        Command::CanaryShow { canary_id } => {
            validate_job_id(canary_id).map_err(|_| ExecuteError::Invalid("canary_id is invalid"))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod verification_unit_tests {
    use super::{
        ArenaError, Command, ExecuteError, GENE_EVENT_TYPE, GeneTransferOutcome,
        TRANSFER_RECORDED_EVENT_TYPE, best_gene_target_operation, map_cluster_error,
        map_evaluator_open_error, require_command_fields,
    };
    use crate::{DriftKind, GeneExtractedPayload, GeneTransferRecordedPayload, MetaLineageSpec};
    use hephaestus_ledger::StoredEvent;

    #[test]
    fn evaluator_open_errors_explain_recovery_and_keep_unknown_details_private() {
        let sensitive = "sealed-task-and-credential-sentinel";
        let cases = [
            ArenaError::WorldArtifactMismatch("arena.evaluator"),
            ArenaError::EvaluatorExecution("evaluator executable is unsafe".to_owned()),
            ArenaError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, sensitive)),
            ArenaError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                sensitive,
            )),
        ];
        for error in cases {
            let ExecuteError::Rejected(message) = map_evaluator_open_error(&error) else {
                panic!("known installation error must explain recovery");
            };
            assert!(message.contains("configured evaluator"));
            assert!(message.contains("matching installation") && message.contains("retry"));
            assert!(!message.contains(sensitive));
            assert!(message.len() <= 256);
        }
        for error in [
            ArenaError::WorldArtifactMismatch(sensitive),
            ArenaError::EvaluatorExecution(sensitive.to_owned()),
            ArenaError::Io(std::io::Error::other(sensitive)),
            ArenaError::EvaluatorProtocol(sensitive),
        ] {
            assert!(matches!(
                map_evaluator_open_error(&error),
                ExecuteError::Internal
            ));
        }
    }

    fn event(sequence: u64, event_type: &str, payload_bytes: Vec<u8>) -> StoredEvent {
        StoredEvent {
            sequence,
            event_id: format!("fixture-{sequence}"),
            aggregate_id: "fixture".to_owned(),
            event_type: event_type.to_owned(),
            actor: "test-fixture".to_owned(),
            timestamp_millis: 1,
            payload: payload_bytes,
            previous_hash: [0; 32],
            hash: [0; 32],
        }
    }

    fn gene_event(sequence: u64, gene_id: &str, before: &str, after: &str) -> StoredEvent {
        let payload = GeneExtractedPayload {
            schema_version: 1,
            gene_id: gene_id.to_owned(),
            promotion_transition_id: "promotion".to_owned(),
            promotion_event_id: "promotion-event".to_owned(),
            promotion_event_hash: "0".repeat(64),
            assessment_id: "assessment".to_owned(),
            assessment_event_id: "assessment-event".to_owned(),
            assessment_event_hash: "0".repeat(64),
            proposal_id: "proposal".to_owned(),
            proposal_event_id: "proposal-event".to_owned(),
            proposal_event_hash: "0".repeat(64),
            selection_event_id: "selection-event".to_owned(),
            selection_event_hash: "0".repeat(64),
            invariant_event_id: "invariant-event".to_owned(),
            invariant_event_hash: "0".repeat(64),
            world_id: "world".to_owned(),
            origin_parent_genome_id: "parent".to_owned(),
            origin_child_genome_id: "child".to_owned(),
            operation_before: before.to_owned(),
            operation_after: after.to_owned(),
            evidence_trials: 10,
            evidence_threshold: 5,
        };
        let canonical = serde_json::to_vec(&serde_json::to_value(&payload).unwrap()).unwrap();
        event(sequence, GENE_EVENT_TYPE, canonical)
    }

    fn transfer_event(
        sequence: u64,
        gene_id: &str,
        outcome: GeneTransferOutcome,
        estimate_bps: i64,
    ) -> StoredEvent {
        let payload = GeneTransferRecordedPayload {
            schema_version: 1,
            trial_id: format!("trial-{sequence}"),
            gene_id: gene_id.to_owned(),
            applied_event_id: "applied-event".to_owned(),
            applied_event_hash: "0".repeat(64),
            evaluation_id: "evaluation".to_owned(),
            selection_event_id: "selection-event".to_owned(),
            selection_event_hash: "0".repeat(64),
            selection_receipt_artifact_id: "sha256:receipt".to_owned(),
            outcome,
            estimate_bps,
            lower_bps: estimate_bps - 10,
            upper_bps: estimate_bps + 10,
        };
        let canonical = serde_json::to_vec(&serde_json::to_value(&payload).unwrap()).unwrap();
        event(sequence, TRANSFER_RECORDED_EVENT_TYPE, canonical)
    }

    #[test]
    fn map_cluster_error_reports_invalid_analysis_id() {
        assert!(matches!(
            map_cluster_error(ArenaError::InvalidId {
                field: "analysis_id",
                value: "bad id".to_owned(),
            }),
            ExecuteError::Invalid("analysis_id is invalid")
        ));
    }

    #[test]
    fn best_gene_target_operation_ignores_unrelated_genes_and_undecodable_events() {
        let history = [
            // Undecodable Gene payload: skipped rather than failing the scan.
            event(1, GENE_EVENT_TYPE, b"not json".to_vec()),
            // Gene for a different current operation is skipped entirely.
            gene_event(2, "gene-other-op", "ascii_uppercase", "context_loss_aware"),
            transfer_event(3, "gene-other-op", GeneTransferOutcome::Positive, 500),
        ];
        assert_eq!(best_gene_target_operation(&history, "identity"), None);
    }

    #[test]
    fn best_gene_target_operation_requires_at_least_one_positive_trial() {
        let history = [
            gene_event(1, "gene-no-trials", "identity", "ascii_uppercase"),
            gene_event(2, "gene-neutral-only", "identity", "context_loss_aware"),
            // Wrong gene_id and non-Positive outcomes are both skipped.
            transfer_event(3, "gene-does-not-exist", GeneTransferOutcome::Positive, 500),
            transfer_event(4, "gene-neutral-only", GeneTransferOutcome::Neutral, 500),
            transfer_event(5, "gene-neutral-only", GeneTransferOutcome::Negative, 500),
            // Undecodable transfer payload: skipped rather than failing the scan.
            event(6, TRANSFER_RECORDED_EVENT_TYPE, b"not json".to_vec()),
        ];
        assert_eq!(best_gene_target_operation(&history, "identity"), None);
    }

    #[test]
    fn best_gene_target_operation_picks_the_highest_mean_positive_effect() {
        let history = [
            gene_event(1, "gene-low", "identity", "ascii_uppercase"),
            gene_event(2, "gene-high", "identity", "context_loss_aware"),
            transfer_event(3, "gene-low", GeneTransferOutcome::Positive, 100),
            transfer_event(4, "gene-high", GeneTransferOutcome::Positive, 300),
            transfer_event(5, "gene-high", GeneTransferOutcome::Positive, 500),
        ];
        assert_eq!(
            best_gene_target_operation(&history, "identity"),
            Some("context_loss_aware".to_owned())
        );
    }

    #[test]
    fn best_gene_target_operation_breaks_ties_by_smallest_gene_id() {
        let history = [
            gene_event(1, "gene-zzz", "identity", "ascii_uppercase"),
            gene_event(2, "gene-aaa", "identity", "context_loss_aware"),
            transfer_event(3, "gene-zzz", GeneTransferOutcome::Positive, 200),
            transfer_event(4, "gene-aaa", GeneTransferOutcome::Positive, 200),
        ];
        assert_eq!(
            best_gene_target_operation(&history, "identity"),
            Some("context_loss_aware".to_owned())
        );
    }

    #[test]
    fn require_command_fields_rejects_out_of_range_list_limits() {
        assert!(matches!(
            require_command_fields(&Command::MetaList { limit: 0 }),
            Err(ExecuteError::Invalid("limit must be between 1 and 200"))
        ));
    }

    #[test]
    fn require_command_fields_rejects_incomplete_mcp_call() {
        assert!(matches!(
            require_command_fields(&Command::McpCall {
                client_id: String::new(),
                tool: "tool".to_owned(),
                tool_version: 1,
                decision: crate::McpDecision::Denied {
                    reason: "policy".to_owned(),
                },
            }),
            Err(ExecuteError::Invalid("client_id and tool are required"))
        ));
    }

    #[test]
    fn require_command_fields_rejects_invalid_worker_credential_mint() {
        assert!(matches!(
            require_command_fields(&Command::WorkerCredentialMint {
                worker_id: String::new(),
                ttl_seconds: 60,
            }),
            Err(ExecuteError::Invalid(
                "worker_id is required and ttl_seconds must be between 1 and the maximum"
            ))
        ));
    }

    #[test]
    fn require_command_fields_rejects_empty_worker_credential_revoke() {
        assert!(matches!(
            require_command_fields(&Command::WorkerCredentialRevoke {
                credential_id: String::new(),
            }),
            Err(ExecuteError::Invalid("credential_id is required"))
        ));
    }

    #[test]
    fn require_command_fields_rejects_incomplete_remote_run_submit_and_status() {
        assert!(matches!(
            require_command_fields(&Command::RemoteRunSubmit {
                job_id: "job".to_owned(),
                genome_id: String::new(),
            }),
            Err(ExecuteError::Invalid("genome_id is required"))
        ));
        assert!(matches!(
            require_command_fields(&Command::RemoteJobStatus {
                job_id: String::new(),
            }),
            Err(ExecuteError::Invalid("job_id is required"))
        ));
    }

    #[test]
    fn require_command_fields_rejects_empty_meta_strategy_paths_and_ids() {
        assert!(matches!(
            require_command_fields(&Command::MetaStrategyRegister {
                path: String::new(),
            }),
            Err(ExecuteError::Invalid("path is required"))
        ));
        assert!(matches!(
            require_command_fields(&Command::MetaStrategyShow {
                strategy_id: String::new(),
            }),
            Err(ExecuteError::Invalid("strategy_id is required"))
        ));
    }

    fn valid_meta_evaluate() -> Command {
        Command::MetaEvaluate {
            meta_run_id: "meta-run".to_owned(),
            strategy_a_id: "strategy-a".to_owned(),
            strategy_b_id: "strategy-b".to_owned(),
            lineages: vec![
                MetaLineageSpec {
                    world_id: "world-a".to_owned(),
                    from_genome_id: "genome-a".to_owned(),
                },
                MetaLineageSpec {
                    world_id: "world-b".to_owned(),
                    from_genome_id: "genome-b".to_owned(),
                },
            ],
            confidence_bps: 9_500,
            bootstrap_seed: 1,
        }
    }

    #[test]
    fn require_command_fields_rejects_an_oversized_meta_run_id() {
        let mut command = valid_meta_evaluate();
        if let Command::MetaEvaluate { meta_run_id, .. } = &mut command {
            *meta_run_id = "m".repeat(100);
        }
        assert!(matches!(
            require_command_fields(&command),
            Err(ExecuteError::Invalid(
                "meta_run_id must leave room for its derived run identifiers"
            ))
        ));
    }

    #[test]
    fn require_command_fields_rejects_incomplete_or_duplicate_meta_strategies() {
        let mut empty_a = valid_meta_evaluate();
        if let Command::MetaEvaluate { strategy_a_id, .. } = &mut empty_a {
            *strategy_a_id = String::new();
        }
        assert!(matches!(
            require_command_fields(&empty_a),
            Err(ExecuteError::Invalid(
                "strategy_a_id and strategy_b_id are required"
            ))
        ));

        let mut duplicate = valid_meta_evaluate();
        if let Command::MetaEvaluate {
            strategy_a_id,
            strategy_b_id,
            ..
        } = &mut duplicate
        {
            strategy_b_id.clone_from(strategy_a_id);
        }
        assert!(matches!(
            require_command_fields(&duplicate),
            Err(ExecuteError::Invalid(
                "strategy_a_id and strategy_b_id must differ"
            ))
        ));
    }

    #[test]
    fn require_command_fields_rejects_too_few_or_invalid_meta_lineages() {
        let mut single_lineage = valid_meta_evaluate();
        if let Command::MetaEvaluate { lineages, .. } = &mut single_lineage {
            lineages.truncate(1);
        }
        assert!(matches!(
            require_command_fields(&single_lineage),
            Err(ExecuteError::Invalid(
                "at least two held-out lineages are required for a bootstrap comparison"
            ))
        ));

        let mut empty_lineage_field = valid_meta_evaluate();
        if let Command::MetaEvaluate { lineages, .. } = &mut empty_lineage_field {
            lineages[0].world_id.clear();
        }
        assert!(matches!(
            require_command_fields(&empty_lineage_field),
            Err(ExecuteError::Invalid(
                "lineage world_id and from_genome_id are required"
            ))
        ));

        let mut duplicate_world = valid_meta_evaluate();
        if let Command::MetaEvaluate { lineages, .. } = &mut duplicate_world {
            let world_id = lineages[0].world_id.clone();
            lineages[1].world_id = world_id;
        }
        assert!(matches!(
            require_command_fields(&duplicate_world),
            Err(ExecuteError::Invalid(
                "held-out lineages must use distinct Worlds"
            ))
        ));
    }

    #[test]
    fn require_command_fields_rejects_out_of_range_meta_confidence() {
        let mut command = valid_meta_evaluate();
        if let Command::MetaEvaluate { confidence_bps, .. } = &mut command {
            *confidence_bps = 10_000;
        }
        assert!(matches!(
            require_command_fields(&command),
            Err(ExecuteError::Invalid(
                "confidence_bps must be between 1 and 9999"
            ))
        ));
    }

    #[test]
    fn require_command_fields_accepts_a_valid_meta_evaluate() {
        assert!(require_command_fields(&valid_meta_evaluate()).is_ok());
    }

    #[test]
    fn require_command_fields_rejects_incomplete_gene_commands() {
        assert!(matches!(
            require_command_fields(&Command::GeneTransfer {
                trial_id: "trial".to_owned(),
                gene_id: String::new(),
                to_genome_id: "genome".to_owned(),
            }),
            Err(ExecuteError::Invalid(
                "gene_id and to_genome_id are required"
            ))
        ));
        assert!(matches!(
            require_command_fields(&Command::GeneRecord {
                trial_id: "trial".to_owned(),
                evaluation_id: String::new(),
            }),
            Err(ExecuteError::Invalid("evaluation_id is required"))
        ));
        assert!(matches!(
            require_command_fields(&Command::GeneSpeciate {
                species_id: "species".to_owned(),
                gene_id: String::new(),
                domain_world_id: "world".to_owned(),
            }),
            Err(ExecuteError::Invalid(
                "gene_id and domain_world_id are required"
            ))
        ));
    }

    #[test]
    fn require_command_fields_rejects_incomplete_drift_record() {
        assert!(matches!(
            require_command_fields(&Command::DriftRecord {
                drift_id: "drift".to_owned(),
                world_id: String::new(),
                kind: DriftKind::Latency,
                evidence_evaluation_id: "evaluation".to_owned(),
            }),
            Err(ExecuteError::Invalid("world_id is required"))
        ));
    }

    fn unit_world() -> (
        tempfile::TempDir,
        hephaestus_ledger::ArtifactStore,
        super::CompiledWorld,
    ) {
        use hephaestus_genome::{SourceFormat, compile_world};
        use hephaestus_ledger::ArtifactStore;
        let directory = tempfile::tempdir().expect("artifact directory");
        let artifacts =
            ArtifactStore::open(directory.path().join("blobs")).expect("open artifact store");
        let source = r#"{"schema_version":1,"name":"unit-world","laws":{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0},"authority_ceiling":{"workspace_write":false,"network":false},"mutation_scope":[],"promotion":{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500},"objectives":["coverage"],"evaluator_artifacts":{}}"#;
        let world = compile_world(source, SourceFormat::Json, &artifacts).expect("compile World");
        (directory, artifacts, world)
    }

    fn empty_forge_proposal_payload() -> super::ForgeProposalPayload {
        super::ForgeProposalPayload {
            schema_version: 1,
            proposal_id: "proposal".to_owned(),
            selection_event_id: "selection-event".to_owned(),
            selection_event_hash: "0".repeat(64),
            evaluation_id: "evaluation".to_owned(),
            world_id: "world".to_owned(),
            parent_genome_id: "missing-parent".to_owned(),
            child: super::GenomeRecord {
                genome_id: "missing-child".to_owned(),
                name: "child".to_owned(),
                world_id: "world".to_owned(),
                artifact_id: "sha256:child".to_owned(),
                parent_ids: vec!["missing-parent".to_owned()],
            },
            hypothesis: "hypothesis".to_owned(),
            artifact_name: "agent.prompt".to_owned(),
            prompt_artifact_before: "sha256:before".to_owned(),
            prompt_artifact_after: "sha256:after".to_owned(),
            operation_before: "identity".to_owned(),
            operation_after: "ascii_uppercase".to_owned(),
            analysis_binding: None,
            catalog_version: None,
            mutation_kind: None,
        }
    }

    #[test]
    fn verify_forge_child_rejects_an_unregistered_parent() {
        let (_directory, artifacts, world) = unit_world();
        let registered = super::RegisteredObjects::default();
        let event = event(1, "forge.proposed", Vec::new());
        let payload = empty_forge_proposal_payload();
        assert!(matches!(
            super::verify_forge_child(&artifacts, &registered, &event, &payload, &world),
            Err(super::ControlError::Projection(message))
                if message == "Forge proposal parent is not registered"
        ));
    }

    #[test]
    fn verify_forge_child_compiles_rejects_an_unregistered_parent() {
        let (_directory, artifacts, world) = unit_world();
        let registered = super::RegisteredObjects::default();
        let payload = empty_forge_proposal_payload();
        assert!(matches!(
            super::verify_forge_child_compiles(&artifacts, &registered, &payload, &world),
            Err(super::ControlError::Projection(message))
                if message == "Forge proposal parent is not registered"
        ));
    }
}
