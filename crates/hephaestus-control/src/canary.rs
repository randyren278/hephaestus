//! Staged canary rollout control: 5/25/50/100% advancement gated by
//! deterministic health evidence, automatic abort on staged regression, and
//! automatic Champion rollback on a live post-completion regression.
//!
//! A canary never promotes on its own authority: completion (reaching 100%)
//! calls the existing `champion::promote_payload` policy with the same
//! Forge assessment bound at `Started`, and a live regression calls the
//! existing `champion::rollback_payload` policy. Every transition is one
//! `canary.transitioned` event; replay recomputes each payload from the
//! history that preceded it, exactly like Champion transitions.

use std::path::Path;

use hephaestus_arena::{EvaluationStores, SelectionReceipt, verify_selection_event};
use hephaestus_genome::RegisteredObjects;
use hephaestus_ledger::{EventInput, StoredEvent};

use super::champion::{
    self, ChampionRequest, champion_event_id, champion_projection, champion_transition_payload,
};
use super::{
    ControlError, ExecuteError, OPERATOR_ACTOR, decode_forge_assessment, forge_assessment_event_id,
    hex_encode, validate_job_id,
};
use crate::protocol::{
    CanaryEventRecord, CanaryEvidence, CanaryRecord, CanaryStage, CanaryTransitionKind,
    CanaryTransitionPayload, CanaryTransitionRecord, ChampionTransitionKind,
};

pub(super) const CANARY_EVENT_TYPE: &str = "canary.transitioned";
const CANARY_PREFIX: &str = "canary:";

/// A shift at or beyond this many basis points in latency is a regression.
pub(super) const LATENCY_REGRESSION_BPS: i64 = 2_000;
/// A shift at or beyond this many basis points in cost is a regression.
pub(super) const COST_REGRESSION_BPS: i64 = 2_000;
/// A drop at or beyond this many basis points in correctness is a regression.
pub(super) const CORRECTNESS_REGRESSION_BPS: i64 = 500;
/// A drop at or beyond this many basis points in reliability is a regression.
pub(super) const RELIABILITY_REGRESSION_BPS: i64 = 500;

/// Signed candidate-vs-parent deltas in basis points recomputed from a
/// verified `SelectionReceipt`. Positive latency/cost deltas and negative
/// correctness/reliability deltas are regressive.
#[derive(Clone, Copy, Debug)]
pub(super) struct RegressionDeltas {
    pub(super) latency_bps: i64,
    pub(super) cost_bps: i64,
    pub(super) correctness_bps: i64,
    pub(super) reliability_bps: i64,
}

fn bps_delta(before: u64, after: u64) -> i64 {
    if before == 0 {
        return if after == 0 { 0 } else { i64::MAX };
    }
    let before = i128::from(before);
    let after = i128::from(after);
    let delta = ((after - before) * 10_000) / before;
    i64::try_from(delta.clamp(i128::from(i64::MIN), i128::from(i64::MAX))).unwrap_or(i64::MAX)
}

pub(super) fn regression_deltas(receipt: &SelectionReceipt) -> RegressionDeltas {
    RegressionDeltas {
        latency_bps: bps_delta(receipt.parent_latency_millis(), receipt.candidate_latency_millis()),
        cost_bps: bps_delta(receipt.parent_cost_microusd(), receipt.candidate_cost_microusd()),
        correctness_bps: i64::from(receipt.candidate_correctness_bps())
            - i64::from(receipt.parent_correctness_bps()),
        reliability_bps: i64::from(receipt.candidate_reliability_bps())
            - i64::from(receipt.parent_reliability_bps()),
    }
}

pub(super) fn is_regression(deltas: &RegressionDeltas) -> bool {
    deltas.latency_bps >= LATENCY_REGRESSION_BPS
        || deltas.cost_bps >= COST_REGRESSION_BPS
        || deltas.correctness_bps <= -CORRECTNESS_REGRESSION_BPS
        || deltas.reliability_bps <= -RELIABILITY_REGRESSION_BPS
}

/// Operator request that a canary transition payload is derived from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CanaryRequest {
    Start {
        world_id: String,
        candidate_genome_id: String,
        assessment_id: String,
    },
    Advance {
        evidence_evaluation_id: String,
    },
    LiveCheck {
        evidence_evaluation_id: String,
    },
}

impl CanaryRequest {
    fn from_payload(payload: &CanaryTransitionPayload) -> Result<Self, ControlError> {
        let invalid = || ControlError::Projection("canary transition shape is invalid".to_owned());
        match payload.kind {
            CanaryTransitionKind::Started => Ok(Self::Start {
                world_id: payload.world_id.clone(),
                candidate_genome_id: payload.candidate_genome_id.clone(),
                assessment_id: payload.assessment_id.clone(),
            }),
            CanaryTransitionKind::Advanced | CanaryTransitionKind::Aborted => {
                Ok(Self::Advance {
                    evidence_evaluation_id: payload
                        .evidence
                        .as_ref()
                        .ok_or_else(invalid)?
                        .evidence_evaluation_id
                        .clone(),
                })
            }
            CanaryTransitionKind::LiveRegressionDetected => Ok(Self::LiveCheck {
                evidence_evaluation_id: payload
                    .evidence
                    .as_ref()
                    .ok_or_else(invalid)?
                    .evidence_evaluation_id
                    .clone(),
            }),
        }
    }
}

pub(super) fn canary_started_event_id(canary_id: &str) -> String {
    format!("{CANARY_PREFIX}{canary_id}:started")
}

pub(super) fn canary_advance_event_id(canary_id: &str, evidence_evaluation_id: &str) -> String {
    format!("{CANARY_PREFIX}{canary_id}:advance:{evidence_evaluation_id}")
}

pub(super) fn canary_livecheck_event_id(canary_id: &str, evidence_evaluation_id: &str) -> String {
    format!("{CANARY_PREFIX}{canary_id}:livecheck:{evidence_evaluation_id}")
}

pub(super) fn canary_aggregate_id(canary_id: &str) -> String {
    format!("{CANARY_PREFIX}{canary_id}")
}

fn is_canary_event(event: &StoredEvent) -> bool {
    event.event_type == CANARY_EVENT_TYPE
        || event.event_id.starts_with(CANARY_PREFIX)
        || event.aggregate_id.starts_with(CANARY_PREFIX)
}

pub(super) fn decode_canary_transition(
    event: &StoredEvent,
) -> Result<CanaryTransitionPayload, ControlError> {
    let payload = serde_json::from_slice::<CanaryTransitionPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("canary transition payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "canary transition payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

pub(super) fn canary_transition_record(
    payload: CanaryTransitionPayload,
    event: &StoredEvent,
) -> CanaryTransitionRecord {
    CanaryTransitionRecord {
        payload,
        event: CanaryEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_hash: hex_encode(&event.hash),
        },
    }
}

/// Canary projection of one canary from already-verified history.
pub(super) fn canary_projection(
    history: &[StoredEvent],
    canary_id: &str,
) -> Result<Option<CanaryRecord>, ControlError> {
    let mut record: Option<CanaryRecord> = None;
    for event in history
        .iter()
        .filter(|event| event.event_type == CANARY_EVENT_TYPE)
    {
        let payload = decode_canary_transition(event)?;
        if payload.canary_id != canary_id {
            continue;
        }
        match payload.kind {
            CanaryTransitionKind::Started => {
                record = Some(CanaryRecord {
                    canary_id: payload.canary_id.clone(),
                    world_id: payload.world_id.clone(),
                    candidate_genome_id: payload.candidate_genome_id.clone(),
                    previous_champion_genome_id: payload.previous_champion_genome_id.clone(),
                    stage: payload.stage,
                    transitions: Vec::new(),
                });
            }
            CanaryTransitionKind::Advanced | CanaryTransitionKind::Aborted => {
                let run = record.as_mut().ok_or_else(|| {
                    ControlError::Projection("canary advance precedes its start".to_owned())
                })?;
                run.stage = payload.stage;
            }
            CanaryTransitionKind::LiveRegressionDetected => {}
        }
        let run = record.as_mut().ok_or_else(|| {
            ControlError::Projection("canary transition precedes its start".to_owned())
        })?;
        run.transitions.push(canary_transition_record(payload, event));
    }
    Ok(record)
}

/// Returns the recorded transition for the request's deterministic event ID
/// if the request matches it.
pub(super) fn existing_canary_transition(
    history: &[StoredEvent],
    canary_id: &str,
    request: &CanaryRequest,
) -> Result<Option<CanaryTransitionRecord>, ExecuteError> {
    let event_id = match request {
        CanaryRequest::Start { .. } => canary_started_event_id(canary_id),
        CanaryRequest::Advance {
            evidence_evaluation_id,
        } => canary_advance_event_id(canary_id, evidence_evaluation_id),
        CanaryRequest::LiveCheck {
            evidence_evaluation_id,
        } => canary_livecheck_event_id(canary_id, evidence_evaluation_id),
    };
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return Ok(None);
    };
    let payload = decode_canary_transition(event).map_err(|_| ExecuteError::Internal)?;
    let recorded = CanaryRequest::from_payload(&payload).map_err(|_| ExecuteError::Internal)?;
    if &recorded != request {
        return Err(ExecuteError::Rejected(
            "canary_id / evidence is already bound to different canary content".to_owned(),
        ));
    }
    Ok(Some(canary_transition_record(payload, event)))
}

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

fn next_stage(stage: CanaryStage) -> Option<CanaryStage> {
    match stage {
        CanaryStage::Pending => Some(CanaryStage::Stage5),
        CanaryStage::Stage5 => Some(CanaryStage::Stage25),
        CanaryStage::Stage25 => Some(CanaryStage::Stage50),
        CanaryStage::Stage50 => Some(CanaryStage::Completed),
        CanaryStage::Completed | CanaryStage::Aborted => None,
    }
}

fn load_selection_receipt(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    world_id: &str,
    evidence_evaluation_id: &str,
) -> Result<(SelectionReceipt, StoredEvent), ExecuteError> {
    validate_job_id(evidence_evaluation_id)
        .map_err(|_| ExecuteError::Invalid("evidence_evaluation_id is invalid"))?;
    let selection_event_id = format!("arena:selection:{evidence_evaluation_id}:selected");
    let selection_event = history
        .iter()
        .find(|event| event.event_id == selection_event_id)
        .cloned()
        .ok_or(ExecuteError::NotFound)?;
    let world = registered.world(world_id).ok_or(ExecuteError::Internal)?;
    let stores = EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
        .map_err(|_| ExecuteError::Internal)?;
    let verified = verify_selection_event(stores, &selection_event, world.compiled())
        .map_err(|_| ExecuteError::Internal)?;
    let receipt = verified.receipt().clone();
    drop(verified.into_stores());
    Ok((receipt, selection_event))
}

/// Derives the only payload the policy admits for `request` after `history`.
///
/// `canary_id` and `data_dir` drive lookups; the deterministic ledger
/// events actually appended by a request live in `server.rs`, since a
/// completing or live-regressing canary also appends a separate, existing
/// `champion.transitioned` event through `champion::champion_transition_payload`.
#[allow(clippy::too_many_lines)]
pub(super) fn canary_transition_payload(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    canary_id: &str,
    request: &CanaryRequest,
) -> Result<CanaryTransitionPayload, ExecuteError> {
    match request {
        CanaryRequest::Start {
            world_id,
            candidate_genome_id,
            assessment_id,
        } => start_payload(history, registered, canary_id, world_id, candidate_genome_id, assessment_id),
        CanaryRequest::Advance {
            evidence_evaluation_id,
        } => advance_payload(data_dir, history, registered, canary_id, evidence_evaluation_id),
        CanaryRequest::LiveCheck {
            evidence_evaluation_id,
        } => live_check_payload(data_dir, history, registered, canary_id, evidence_evaluation_id),
    }
}

fn start_payload(
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    canary_id: &str,
    world_id: &str,
    candidate_genome_id: &str,
    assessment_id: &str,
) -> Result<CanaryTransitionPayload, ExecuteError> {
    if frozen_after(history) {
        return Err(ExecuteError::Invalid("evolution is frozen"));
    }
    registered.world(world_id).ok_or(ExecuteError::NotFound)?;
    let candidate = registered
        .genome(candidate_genome_id)
        .ok_or(ExecuteError::NotFound)?;
    if candidate.record().world_id != world_id {
        return Err(ExecuteError::Rejected(
            "candidate Genome is not compiled under the requested World".to_owned(),
        ));
    }
    let champion = champion_projection(history, world_id).map_err(|_| ExecuteError::Internal)?;
    let Some(current_champion) = champion.champion_genome_id else {
        return Err(ExecuteError::Rejected(
            "World has no Champion; seed one before starting a canary".to_owned(),
        ));
    };
    if candidate_genome_id == current_champion {
        return Err(ExecuteError::Rejected(
            "candidate must differ from the current Champion".to_owned(),
        ));
    }
    validate_job_id(assessment_id).map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))?;
    let assessment_event_id = forge_assessment_event_id(assessment_id);
    let assessment_event = history
        .iter()
        .find(|event| event.event_id == assessment_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let assessment = decode_forge_assessment(assessment_event).map_err(|_| ExecuteError::Internal)?;
    // The bound assessment is this canary's shadow evaluation: it must exist
    // and exactly evidence the current Champion against the candidate, but
    // its outcome is not itself a gate. Completion still requires a
    // `MetricsPassed` assessment through the unchanged Champion promotion
    // policy, and every stage in between is independently health-gated.
    if assessment.world_id != world_id
        || assessment.parent_genome_id != current_champion
        || assessment.child_genome_id != candidate_genome_id
    {
        return Err(ExecuteError::Rejected(
            "assessment does not evidence the current Champion against this candidate".to_owned(),
        ));
    }
    Ok(CanaryTransitionPayload {
        schema_version: 1,
        canary_id: canary_id.to_owned(),
        world_id: world_id.to_owned(),
        kind: CanaryTransitionKind::Started,
        stage: CanaryStage::Pending,
        candidate_genome_id: candidate_genome_id.to_owned(),
        previous_champion_genome_id: current_champion,
        assessment_id: assessment_id.to_owned(),
        evidence: None,
        champion_promotion: None,
        champion_rollback_event_id: None,
        champion_rollback_event_hash: None,
        reason: None,
    })
}

fn advance_payload(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    canary_id: &str,
    evidence_evaluation_id: &str,
) -> Result<CanaryTransitionPayload, ExecuteError> {
    let canary = canary_projection(history, canary_id)
        .map_err(|_| ExecuteError::Internal)?
        .ok_or(ExecuteError::NotFound)?;
    if matches!(canary.stage, CanaryStage::Completed | CanaryStage::Aborted) {
        return Err(ExecuteError::Rejected(
            "canary has already reached a terminal stage".to_owned(),
        ));
    }
    let Some(target_stage) = next_stage(canary.stage) else {
        return Err(ExecuteError::Internal);
    };

    let (receipt, selection_event) = load_selection_receipt(
        data_dir,
        history,
        registered,
        &canary.world_id,
        evidence_evaluation_id,
    )?;
    if receipt.world_id() != canary.world_id
        || receipt.parent_genome_id() != canary.previous_champion_genome_id
        || receipt.candidate_genome_id() != canary.candidate_genome_id
    {
        return Err(ExecuteError::Rejected(
            "evidence does not pair the current Champion against this canary's candidate".to_owned(),
        ));
    }
    let deltas = regression_deltas(&receipt);
    let regressed = is_regression(&deltas);
    let evidence = CanaryEvidence {
        evidence_evaluation_id: evidence_evaluation_id.to_owned(),
        selection_event_id: selection_event.event_id.clone(),
        selection_event_hash: hex_encode(&selection_event.hash),
        latency_delta_bps: deltas.latency_bps,
        cost_delta_bps: deltas.cost_bps,
        correctness_delta_bps: deltas.correctness_bps,
        reliability_delta_bps: deltas.reliability_bps,
        regressed,
    };

    if regressed {
        return Ok(CanaryTransitionPayload {
            schema_version: 1,
            canary_id: canary_id.to_owned(),
            world_id: canary.world_id.clone(),
            kind: CanaryTransitionKind::Aborted,
            stage: CanaryStage::Aborted,
            candidate_genome_id: canary.candidate_genome_id.clone(),
            previous_champion_genome_id: canary.previous_champion_genome_id.clone(),
            assessment_id: canary.assessment_id().to_owned(),
            evidence: Some(evidence),
            champion_promotion: None,
            champion_rollback_event_id: None,
            champion_rollback_event_hash: None,
            reason: Some(format!(
                "staged health evidence from evaluation {evidence_evaluation_id} regressed beyond the documented threshold"
            )),
        });
    }

    if frozen_after(history) {
        return Err(ExecuteError::Invalid("evolution is frozen"));
    }

    let champion_promotion = if target_stage == CanaryStage::Completed {
        let promotion_payload = champion_transition_payload(
            data_dir,
            history,
            registered,
            &canary_id_promotion_transition_id(canary_id),
            &ChampionRequest::Promote {
                assessment_id: canary.assessment_id().to_owned(),
            },
        )
        .map_err(|_| {
            ExecuteError::Rejected(
                "completion evidence does not satisfy the Champion promotion policy".to_owned(),
            )
        })?;
        promotion_payload.promotion
    } else {
        None
    };

    Ok(CanaryTransitionPayload {
        schema_version: 1,
        canary_id: canary_id.to_owned(),
        world_id: canary.world_id.clone(),
        kind: CanaryTransitionKind::Advanced,
        stage: target_stage,
        candidate_genome_id: canary.candidate_genome_id.clone(),
        previous_champion_genome_id: canary.previous_champion_genome_id.clone(),
        assessment_id: canary.assessment_id().to_owned(),
        evidence: Some(evidence),
        champion_promotion,
        champion_rollback_event_id: None,
        champion_rollback_event_hash: None,
        reason: None,
    })
}

fn live_check_payload(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    canary_id: &str,
    evidence_evaluation_id: &str,
) -> Result<CanaryTransitionPayload, ExecuteError> {
    let canary = canary_projection(history, canary_id)
        .map_err(|_| ExecuteError::Internal)?
        .ok_or(ExecuteError::NotFound)?;
    if canary.stage != CanaryStage::Completed {
        return Err(ExecuteError::Rejected(
            "canary has not completed; there is no live Champion to check".to_owned(),
        ));
    }
    let world_champion =
        champion_projection(history, &canary.world_id).map_err(|_| ExecuteError::Internal)?;
    if world_champion.champion_genome_id.as_deref() != Some(canary.candidate_genome_id.as_str()) {
        return Err(ExecuteError::Rejected(
            "World Champion no longer matches this canary's promoted candidate".to_owned(),
        ));
    }

    let (receipt, selection_event) = load_selection_receipt(
        data_dir,
        history,
        registered,
        &canary.world_id,
        evidence_evaluation_id,
    )?;
    // Same evidence orientation as staged advancement: parent is the
    // previous Champion (the baseline), candidate is this canary's rollout
    // candidate, which is now the live Champion. A regression here means
    // the live Champion (candidate) measured worse than the previous
    // Champion (parent) it replaced.
    if receipt.world_id() != canary.world_id
        || receipt.parent_genome_id() != canary.previous_champion_genome_id
        || receipt.candidate_genome_id() != canary.candidate_genome_id
    {
        return Err(ExecuteError::Rejected(
            "evidence does not pair the previous Champion against the live Champion".to_owned(),
        ));
    }
    let deltas = regression_deltas(&receipt);
    if !is_regression(&deltas) {
        return Err(ExecuteError::Rejected(
            "evidence does not show a live regression".to_owned(),
        ));
    }
    let evidence = CanaryEvidence {
        evidence_evaluation_id: evidence_evaluation_id.to_owned(),
        selection_event_id: selection_event.event_id.clone(),
        selection_event_hash: hex_encode(&selection_event.hash),
        latency_delta_bps: deltas.latency_bps,
        cost_delta_bps: deltas.cost_bps,
        correctness_delta_bps: deltas.correctness_bps,
        reliability_delta_bps: deltas.reliability_bps,
        regressed: true,
    };

    let rollback_transition_id = canary_id_rollback_transition_id(canary_id);
    let _rollback_payload = champion_transition_payload(
        data_dir,
        history,
        registered,
        &rollback_transition_id,
        &ChampionRequest::Rollback {
            world_id: canary.world_id.clone(),
            reason: format!(
                "canary {canary_id} automatic rollback: live evaluation {evidence_evaluation_id} regressed beyond the documented threshold"
            ),
        },
    )
    .map_err(|_| ExecuteError::Internal)?;
    let rollback_event_id = champion_event_id(&rollback_transition_id);

    Ok(CanaryTransitionPayload {
        schema_version: 1,
        canary_id: canary_id.to_owned(),
        world_id: canary.world_id.clone(),
        kind: CanaryTransitionKind::LiveRegressionDetected,
        stage: CanaryStage::Completed,
        candidate_genome_id: canary.candidate_genome_id.clone(),
        previous_champion_genome_id: canary.previous_champion_genome_id.clone(),
        assessment_id: canary.assessment_id().to_owned(),
        evidence: Some(evidence),
        champion_promotion: None,
        champion_rollback_event_id: Some(rollback_event_id),
        champion_rollback_event_hash: Some(rollback_payload_hash_placeholder()),
        reason: Some(format!(
            "canary {canary_id} live Champion regressed against its previous Champion"
        )),
    })
}

/// Deterministic idempotency key for the Champion promotion this canary's
/// completion joins. One canary can complete at most once, so binding the
/// transition ID to the canary ID is safe and keeps promotion reachable only
/// through this one path.
pub(super) fn canary_id_promotion_transition_id(canary_id: &str) -> String {
    format!("canary-promote-{canary_id}")
}

/// Deterministic idempotency key for the Champion rollback this canary's
/// live regression check joins.
pub(super) fn canary_id_rollback_transition_id(canary_id: &str) -> String {
    format!("canary-rollback-{canary_id}")
}

/// The rollback event hash is only known once `server.rs` has appended the
/// event; `server.rs` overwrites this placeholder with the real hash before
/// persisting the canary event. Kept as a named helper so both sites agree
/// on the sentinel value.
pub(super) fn rollback_payload_hash_placeholder() -> String {
    String::new()
}

/// Recomputes every canary transition from the history that preceded it,
/// including cross-referencing the separate Champion transition event a
/// completion or live regression must have also appended.
#[allow(clippy::too_many_lines)]
pub(super) fn verify_canary_history(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    for (index, event) in history.iter().enumerate() {
        if !is_canary_event(event) {
            continue;
        }
        let payload = decode_canary_transition(event)?;
        let expected_event_id = match payload.kind {
            CanaryTransitionKind::Started => canary_started_event_id(&payload.canary_id),
            CanaryTransitionKind::Advanced | CanaryTransitionKind::Aborted => {
                let evidence = payload.evidence.as_ref().ok_or_else(|| {
                    ControlError::Projection("canary advance is missing evidence".to_owned())
                })?;
                canary_advance_event_id(&payload.canary_id, &evidence.evidence_evaluation_id)
            }
            CanaryTransitionKind::LiveRegressionDetected => {
                let evidence = payload.evidence.as_ref().ok_or_else(|| {
                    ControlError::Projection("canary live check is missing evidence".to_owned())
                })?;
                canary_livecheck_event_id(&payload.canary_id, &evidence.evidence_evaluation_id)
            }
        };
        if payload.schema_version != 1
            || event.event_type != CANARY_EVENT_TYPE
            || event.actor != OPERATOR_ACTOR
            || event.event_id != expected_event_id
            || event.aggregate_id != canary_aggregate_id(&payload.canary_id)
            || validate_job_id(&payload.canary_id).is_err()
        {
            return Err(ControlError::Projection(
                "canary transition event identity is invalid".to_owned(),
            ));
        }
        let request = CanaryRequest::from_payload(&payload)?;
        let mut expected = canary_transition_payload(
            data_dir,
            &history[..index],
            registered,
            &payload.canary_id,
            &request,
        )
        .map_err(|_| ControlError::Projection("canary transition was not admissible".to_owned()))?;
        // The live rollback event hash is only known once its Champion
        // transition is appended; recompute it from the actually-appended
        // Champion event before comparing.
        if payload.kind == CanaryTransitionKind::LiveRegressionDetected {
            let rollback_event_id = expected
                .champion_rollback_event_id
                .clone()
                .ok_or_else(|| ControlError::Projection("canary rollback is unbound".to_owned()))?;
            let rollback_event = history
                .iter()
                .find(|candidate| candidate.event_id == rollback_event_id)
                .ok_or_else(|| ControlError::Projection("canary rollback event is missing".to_owned()))?;
            let rollback_payload = champion::decode_champion_transition(rollback_event)?;
            if rollback_payload.kind != ChampionTransitionKind::RolledBack
                || rollback_payload.world_id != payload.world_id
            {
                return Err(ControlError::Projection(
                    "canary rollback does not match its Champion transition".to_owned(),
                ));
            }
            expected.champion_rollback_event_hash = Some(hex_encode(&rollback_event.hash));
        }
        if payload != expected {
            return Err(ControlError::Projection(
                "canary transition differs from verified evidence".to_owned(),
            ));
        }
    }
    Ok(())
}

impl CanaryRecord {
    fn assessment_id(&self) -> &str {
        self.transitions
            .first()
            .map_or(self.candidate_genome_id.as_str(), |first| {
                first.payload.assessment_id.as_str()
            })
    }
}

/// Builds the ledger event the daemon should append for one admitted canary
/// request. Returns the payload alongside the deterministic event ID.
pub(super) fn canary_event_input(
    payload: &CanaryTransitionPayload,
    timestamp: i64,
) -> Result<EventInput, ExecuteError> {
    let event_id = match payload.kind {
        CanaryTransitionKind::Started => canary_started_event_id(&payload.canary_id),
        CanaryTransitionKind::Advanced | CanaryTransitionKind::Aborted => {
            let evidence = payload
                .evidence
                .as_ref()
                .ok_or(ExecuteError::Internal)?;
            canary_advance_event_id(&payload.canary_id, &evidence.evidence_evaluation_id)
        }
        CanaryTransitionKind::LiveRegressionDetected => {
            let evidence = payload
                .evidence
                .as_ref()
                .ok_or(ExecuteError::Internal)?;
            canary_livecheck_event_id(&payload.canary_id, &evidence.evidence_evaluation_id)
        }
    };
    let payload_value = serde_json::to_value(payload).map_err(|_| ExecuteError::Internal)?;
    let payload_bytes = serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
    Ok(EventInput::new(
        event_id,
        canary_aggregate_id(&payload.canary_id),
        CANARY_EVENT_TYPE,
        OPERATOR_ACTOR,
        timestamp,
        payload_bytes,
    ))
}
