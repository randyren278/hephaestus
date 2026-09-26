//! Automatic drift-to-canary adaptation (roadmap item 12).
//!
//! For a World whose Law `auto_canary_on_drift` opted in, the daemon's own
//! reconciliation loop (`ControlPlane::advance_drift_adaptations`, called
//! every `serve` tick alongside `advance_evolution`, never from a client
//! connection) drives an unadapted `drift.recorded` event through the
//! ordinary Forge, Arena, and canary primitives: propose a mutation of the
//! current Champion, run a shadow evaluation of it, and roll it out through
//! the unchanged staged canary. One `drift.adaptation_started` event records
//! the chosen branch; one `drift.adaptation_finished` event records why the
//! pipeline stopped, cross-referencing the arena/selection/forge/canary
//! events it already produced rather than repeating their content. Every
//! step is idempotent and driven solely from durable history, so a daemon
//! restart mid-pipeline resumes exactly where it left off, exactly like
//! `evolve`.
//!
//! This module only derives, verifies, and projects the two event payloads;
//! the stateful orchestration (submitting Arena work, proposing, assessing,
//! and driving the canary) lives on `ControlPlane` in `server.rs`, exactly
//! like `drift.rs` and `canary.rs` split from the same file.

use hephaestus_ledger::{EventInput, StoredEvent};

use super::{
    ControlError, ExecuteError, OPERATOR_ACTOR, decode_forge_proposal, forge_assessment_event_id,
    forge_event_id, hex_encode, validate_job_id,
};
use crate::protocol::{
    CanaryStage, DriftAdaptationEventRecord, DriftAdaptationFinishReason,
    DriftAdaptationFinishedPayload, DriftAdaptationStartedPayload, DriftAdaptationSummary,
};

pub(super) const DRIFT_ADAPTATION_STARTED_TYPE: &str = "drift.adaptation_started";
pub(super) const DRIFT_ADAPTATION_FINISHED_TYPE: &str = "drift.adaptation_finished";
// Deliberately distinct from `drift.rs`'s `"drift:"` and `canary.rs`'s
// `"canary:"` aggregate prefixes: both those modules' own `is_*_event`
// history scans fuzzy-match on aggregate/event ID prefix (not just event
// type) as an extra fail-closed net, so an adaptation event whose IDs
// happened to start with either prefix would wrongly be swept into their
// scans and rejected as an undecodable drift/canary payload.
const ADAPTATION_PREFIX: &str = "adaptation:";

pub(super) fn adaptation_aggregate_id(drift_id: &str) -> String {
    format!("{ADAPTATION_PREFIX}{drift_id}")
}

pub(super) fn adaptation_started_event_id(drift_id: &str) -> String {
    format!("{ADAPTATION_PREFIX}{drift_id}:started")
}

pub(super) fn adaptation_finished_event_id(drift_id: &str) -> String {
    format!("{ADAPTATION_PREFIX}{drift_id}:finished")
}

// The job-id-style identities below (proposal, evaluation, assessment, and
// canary IDs) are, unlike the ledger event/aggregate IDs above, passed
// straight into `validate_job_id` by `submit_arena_job`,
// `propose_genome_from_source`, `assess_genome`, and `transition_canary`,
// which allows only ASCII alphanumerics, `-`, `_`, and `.` — no colons.

/// Deterministic Forge proposal identity for one drift's adaptation.
pub(super) fn adaptation_proposal_id(drift_id: &str) -> String {
    format!("adapt-{drift_id}-proposal")
}

/// Deterministic diagnostic evaluation identity: establishes the Champion as
/// the verified selected candidate `propose_genome_from_source` requires,
/// exactly the role `evolve`'s own diagnostic evaluation plays. Paired
/// against the drift's own `shifted_genome_id` so no additional Genome needs
/// registering.
pub(super) fn adaptation_diagnostic_evaluation_id(drift_id: &str) -> String {
    format!("adapt-{drift_id}-diagnostic")
}

/// Deterministic shadow evaluation identity: Champion versus the proposed child.
pub(super) fn adaptation_shadow_evaluation_id(drift_id: &str) -> String {
    format!("adapt-{drift_id}-shadow")
}

/// Deterministic Forge assessment identity binding the shadow evaluation.
pub(super) fn adaptation_assessment_id(drift_id: &str) -> String {
    format!("adapt-{drift_id}-assessment")
}

/// Deterministic canary identity this adaptation drives.
pub(super) fn adaptation_canary_id(drift_id: &str) -> String {
    format!("adapt-{drift_id}-canary")
}

/// Deterministic evaluation identity for one staged canary advance, indexed
/// by how many advances this adaptation's canary has already recorded.
pub(super) fn adaptation_stage_evaluation_id(drift_id: &str, stage_index: u32) -> String {
    format!("adapt-{drift_id}-stage-{stage_index}")
}

fn is_adaptation_event(event: &StoredEvent) -> bool {
    event.event_type == DRIFT_ADAPTATION_STARTED_TYPE
        || event.event_type == DRIFT_ADAPTATION_FINISHED_TYPE
}

pub(super) fn decode_started(
    event: &StoredEvent,
) -> Result<DriftAdaptationStartedPayload, ControlError> {
    let payload =
        serde_json::from_slice::<DriftAdaptationStartedPayload>(&event.payload).map_err(|_| {
            ControlError::Projection("drift adaptation start payload is invalid".to_owned())
        })?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "drift adaptation start payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

pub(super) fn decode_finished(
    event: &StoredEvent,
) -> Result<DriftAdaptationFinishedPayload, ControlError> {
    let payload = serde_json::from_slice::<DriftAdaptationFinishedPayload>(&event.payload)
        .map_err(|_| {
            ControlError::Projection("drift adaptation finish payload is invalid".to_owned())
        })?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "drift adaptation finish payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

fn event_record(event: &StoredEvent) -> DriftAdaptationEventRecord {
    DriftAdaptationEventRecord {
        sequence: event.sequence,
        event_id: event.event_id.clone(),
        aggregate_id: event.aggregate_id.clone(),
        event_hash: hex_encode(&event.hash),
    }
}

/// Durable projection of one drift's adaptation, reconstructed from already
/// hash-chain-verified history. Distinct from the operator-visible
/// `DriftAdaptationSummary` only in also carrying the canary's live stage,
/// which the caller resolves separately (`canary_projection`) since this
/// module does not depend on `canary.rs`'s internals.
#[derive(Clone, Debug, Default)]
pub(super) struct AdaptationProjection {
    pub(super) started: Option<DriftAdaptationStartedPayload>,
    pub(super) started_event: Option<DriftAdaptationEventRecord>,
    pub(super) finished: Option<DriftAdaptationFinishedPayload>,
    pub(super) finished_event: Option<DriftAdaptationEventRecord>,
}

/// Reconstructs one drift's adaptation projection from verified history.
pub(super) fn adaptation_projection(
    history: &[StoredEvent],
    drift_id: &str,
) -> Result<AdaptationProjection, ControlError> {
    let mut projection = AdaptationProjection::default();
    for event in history {
        match event.event_type.as_str() {
            DRIFT_ADAPTATION_STARTED_TYPE => {
                let payload = decode_started(event)?;
                if payload.drift_id != drift_id {
                    continue;
                }
                projection.started = Some(payload);
                projection.started_event = Some(event_record(event));
            }
            DRIFT_ADAPTATION_FINISHED_TYPE => {
                let payload = decode_finished(event)?;
                if payload.drift_id != drift_id {
                    continue;
                }
                projection.finished = Some(payload);
                projection.finished_event = Some(event_record(event));
            }
            _ => {}
        }
    }
    Ok(projection)
}

/// Read-side summary combining the adaptation projection with the driven
/// canary's live stage (resolved by the caller, which alone knows how to
/// call into `canary.rs`).
pub(super) fn adaptation_summary(
    history: &[StoredEvent],
    drift_id: &str,
    canary_stage: Option<CanaryStage>,
) -> Result<DriftAdaptationSummary, ControlError> {
    let projection = adaptation_projection(history, drift_id)?;

    // Reads live, in-progress evidence directly from history by the same
    // deterministic IDs the pipeline itself uses, rather than only what a
    // `drift.adaptation_finished` event later cross-references: an operator
    // watching an adaptation mid-flight sees real progress, not just
    // "started".
    let child_genome_id = projection
        .started
        .as_ref()
        .and_then(|started| {
            history
                .iter()
                .find(|event| event.event_id == forge_event_id(&started.proposal_id))
        })
        .and_then(|event| decode_forge_proposal(event).ok())
        .map(|proposal| proposal.child.genome_id);
    let shadow_evaluation_id = projection.started.as_ref().and_then(|_| {
        let candidate_id = adaptation_shadow_evaluation_id(drift_id);
        let selection_event_id = format!("arena:selection:{candidate_id}:selected");
        history
            .iter()
            .any(|event| event.event_id == selection_event_id)
            .then_some(candidate_id)
    });
    let assessment_id = projection.started.as_ref().and_then(|_| {
        let candidate_id = adaptation_assessment_id(drift_id);
        history
            .iter()
            .any(|event| event.event_id == forge_assessment_event_id(&candidate_id))
            .then_some(candidate_id)
    });

    Ok(DriftAdaptationSummary {
        started: projection.started.is_some(),
        started_event: projection.started_event,
        champion_genome_id: projection
            .started
            .as_ref()
            .map(|started| started.champion_genome_id.clone()),
        proposal_id: projection
            .started
            .as_ref()
            .map(|started| started.proposal_id.clone()),
        child_genome_id,
        shadow_evaluation_id,
        assessment_id,
        canary_id: projection
            .started
            .as_ref()
            .map(|started| adaptation_canary_id(&started.drift_id)),
        canary_stage,
        finished: projection.finished.is_some(),
        finished_event: projection.finished_event,
        finish_reason: projection.finished.as_ref().map(|finished| finished.reason),
    })
}

pub(super) fn started_event_input(
    payload: &DriftAdaptationStartedPayload,
    timestamp: i64,
) -> Result<EventInput, ExecuteError> {
    let payload_value = serde_json::to_value(payload).map_err(|_| ExecuteError::Internal)?;
    let payload_bytes = serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
    Ok(EventInput::new(
        adaptation_started_event_id(&payload.drift_id),
        adaptation_aggregate_id(&payload.drift_id),
        DRIFT_ADAPTATION_STARTED_TYPE,
        OPERATOR_ACTOR,
        timestamp,
        payload_bytes,
    ))
}

pub(super) fn finished_event_input(
    payload: &DriftAdaptationFinishedPayload,
    timestamp: i64,
) -> Result<EventInput, ExecuteError> {
    let payload_value = serde_json::to_value(payload).map_err(|_| ExecuteError::Internal)?;
    let payload_bytes = serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
    Ok(EventInput::new(
        adaptation_finished_event_id(&payload.drift_id),
        adaptation_aggregate_id(&payload.drift_id),
        DRIFT_ADAPTATION_FINISHED_TYPE,
        OPERATOR_ACTOR,
        timestamp,
        payload_bytes,
    ))
}

/// Recomputes and cross-references every `drift.adaptation_*` event from the
/// history that preceded it, exactly like `verify_evolution_history`: it
/// checks identity, ordering, and that every cross-referenced event (the
/// triggering drift, the Forge proposal and assessment, the canary, and any
/// claimed Champion promotion) actually exists with matching content,
/// without repeating the Arena/Forge/canary derivation those modules already
/// verify. Fails closed on forged or out-of-order claims.
#[allow(clippy::too_many_lines)]
pub(super) fn verify_drift_adaptation_history(history: &[StoredEvent]) -> Result<(), ControlError> {
    let bad = || ControlError::Projection("drift adaptation history is invalid".to_owned());
    let mut seen_started: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut seen_finished: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (index, event) in history.iter().enumerate() {
        if !is_adaptation_event(event) {
            continue;
        }
        let prefix_ok =
            event.aggregate_id.starts_with(ADAPTATION_PREFIX) && event.actor == OPERATOR_ACTOR;
        match event.event_type.as_str() {
            DRIFT_ADAPTATION_STARTED_TYPE => {
                let payload = decode_started(event)?;
                if !prefix_ok
                    || event.event_id != adaptation_started_event_id(&payload.drift_id)
                    || event.aggregate_id != adaptation_aggregate_id(&payload.drift_id)
                    || payload.schema_version != 1
                    || validate_job_id(&payload.drift_id).is_err()
                    || !seen_started.insert(payload.drift_id.clone())
                {
                    return Err(bad());
                }
                if payload.proposal_id != adaptation_proposal_id(&payload.drift_id) {
                    return Err(bad());
                }
                // The triggering drift must already exist, in this same
                // World, at or before this event.
                let drift_event = history[..index]
                    .iter()
                    .find(|candidate| candidate.event_id == payload.drift_event_id)
                    .ok_or_else(bad)?;
                if drift_event.event_type != "drift.recorded"
                    || hex_encode(&drift_event.hash) != payload.drift_event_hash
                {
                    return Err(bad());
                }
                let drift_payload =
                    super::drift::decode_drift_record(drift_event).map_err(|_| bad())?;
                if drift_payload.drift_id != payload.drift_id
                    || drift_payload.world_id != payload.world_id
                {
                    return Err(bad());
                }
            }
            DRIFT_ADAPTATION_FINISHED_TYPE => {
                let payload = decode_finished(event)?;
                if !prefix_ok
                    || event.event_id != adaptation_finished_event_id(&payload.drift_id)
                    || event.aggregate_id != adaptation_aggregate_id(&payload.drift_id)
                    || payload.schema_version != 1
                    || !seen_started.contains(&payload.drift_id)
                    || !seen_finished.insert(payload.drift_id.clone())
                {
                    return Err(bad());
                }
                let prior = adaptation_projection(&history[..index], &payload.drift_id)?;
                let started = prior.started.ok_or_else(bad)?;
                if started.world_id != payload.world_id {
                    return Err(bad());
                }
                match payload.reason {
                    DriftAdaptationFinishReason::NoCandidateMutation => {
                        if payload.canary_id.is_some()
                            || payload.final_canary_stage.is_some()
                            || payload.promotion_transition_id.is_some()
                        {
                            return Err(bad());
                        }
                    }
                    DriftAdaptationFinishReason::CanaryAborted => {
                        let canary_id = payload.canary_id.as_ref().ok_or_else(bad)?;
                        if *canary_id != adaptation_canary_id(&payload.drift_id)
                            || payload.final_canary_stage != Some(CanaryStage::Aborted)
                            || payload.promotion_transition_id.is_some()
                        {
                            return Err(bad());
                        }
                        let canary = super::canary::canary_projection(&history[..index], canary_id)
                            .map_err(|_| bad())?
                            .ok_or_else(bad)?;
                        if canary.stage != CanaryStage::Aborted {
                            return Err(bad());
                        }
                    }
                    DriftAdaptationFinishReason::Promoted => {
                        let canary_id = payload.canary_id.as_ref().ok_or_else(bad)?;
                        let promotion_transition_id =
                            payload.promotion_transition_id.as_ref().ok_or_else(bad)?;
                        if *canary_id != adaptation_canary_id(&payload.drift_id)
                            || payload.final_canary_stage != Some(CanaryStage::Completed)
                            || *promotion_transition_id
                                != super::canary::canary_id_promotion_transition_id(canary_id)
                        {
                            return Err(bad());
                        }
                        let canary = super::canary::canary_projection(&history[..index], canary_id)
                            .map_err(|_| bad())?
                            .ok_or_else(bad)?;
                        if canary.stage != CanaryStage::Completed {
                            return Err(bad());
                        }
                        let promotion_event_id =
                            super::champion::champion_event_id(promotion_transition_id);
                        let promoted = history[..index].iter().any(|candidate| {
                            candidate.event_id == promotion_event_id
                                && super::champion::decode_champion_transition(candidate).is_ok_and(
                                    |transition| {
                                        transition.kind
                                            == crate::protocol::ChampionTransitionKind::Promoted
                                            && transition.champion_genome_id
                                                == canary.candidate_genome_id
                                    },
                                )
                        });
                        if !promoted {
                            return Err(bad());
                        }
                    }
                    DriftAdaptationFinishReason::Interrupted => {
                        // Terminal, exactly like `EvolutionFinishReason::Interrupted`:
                        // an unexpected failure (never a routine "an Arena
                        // job is still in flight") ends this adaptation
                        // rather than retrying it forever. No further
                        // cross-referenced evidence is required or expected.
                    }
                }
            }
            _ => return Err(bad()),
        }
    }
    Ok(())
}
