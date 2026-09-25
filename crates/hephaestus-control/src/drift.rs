//! Drift records: durable, replay-verified evidence that a paired evaluation
//! of the current Champion (as the evidence's parent, its baseline) against
//! a candidate Genome shifted beyond a fixed, documented threshold. A drift
//! record never replaces a Champion; it only cites verified evidence for an
//! operator (or a later automated adaptation branch) to act on.

use std::path::Path;

use hephaestus_genome::RegisteredObjects;
use hephaestus_ledger::{EventInput, StoredEvent};

use super::canary::{
    CORRECTNESS_REGRESSION_BPS, COST_REGRESSION_BPS, LATENCY_REGRESSION_BPS,
    RELIABILITY_REGRESSION_BPS, regression_deltas,
};
use super::champion::champion_projection;
use super::{ControlError, ExecuteError, OPERATOR_ACTOR, hex_encode, validate_job_id};
use crate::protocol::{DriftEventRecord, DriftKind, DriftRecord, DriftRecordPayload};

pub(super) const DRIFT_EVENT_TYPE: &str = "drift.recorded";
const DRIFT_PREFIX: &str = "drift:";

pub(super) fn drift_event_id(drift_id: &str) -> String {
    format!("{DRIFT_PREFIX}{drift_id}:recorded")
}

pub(super) fn drift_aggregate_id(world_id: &str) -> String {
    format!("{DRIFT_PREFIX}{world_id}")
}

fn is_drift_event(event: &StoredEvent) -> bool {
    event.event_type == DRIFT_EVENT_TYPE
        || event.event_id.starts_with(DRIFT_PREFIX)
        || event.aggregate_id.starts_with(DRIFT_PREFIX)
}

fn threshold_bps(kind: DriftKind) -> u32 {
    match kind {
        DriftKind::Latency => u32::try_from(LATENCY_REGRESSION_BPS).unwrap_or(u32::MAX),
        DriftKind::Cost => u32::try_from(COST_REGRESSION_BPS).unwrap_or(u32::MAX),
        DriftKind::Correctness => u32::try_from(CORRECTNESS_REGRESSION_BPS).unwrap_or(u32::MAX),
        DriftKind::Workload => u32::try_from(RELIABILITY_REGRESSION_BPS).unwrap_or(u32::MAX),
    }
}

pub(super) fn decode_drift_record(event: &StoredEvent) -> Result<DriftRecordPayload, ControlError> {
    let payload = serde_json::from_slice::<DriftRecordPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("drift payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "drift payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

pub(super) fn drift_record(payload: DriftRecordPayload, event: &StoredEvent) -> DriftRecord {
    DriftRecord {
        payload,
        event: DriftEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_hash: hex_encode(&event.hash),
        },
    }
}

pub(super) fn drift_projection(
    history: &[StoredEvent],
    drift_id: &str,
) -> Result<Option<DriftRecord>, ControlError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == DRIFT_EVENT_TYPE)
    {
        let payload = decode_drift_record(event)?;
        if payload.drift_id == drift_id {
            return Ok(Some(drift_record(payload, event)));
        }
    }
    Ok(None)
}

/// Recent drift records, newest first, bounded by `limit`.
pub(super) fn drift_list(
    history: &[StoredEvent],
    limit: u32,
) -> Result<Vec<DriftRecord>, ControlError> {
    let mut drifts = Vec::new();
    for event in history
        .iter()
        .rev()
        .filter(|event| event.event_type == DRIFT_EVENT_TYPE)
    {
        if drifts.len() >= limit as usize {
            break;
        }
        let payload = decode_drift_record(event)?;
        drifts.push(drift_record(payload, event));
    }
    Ok(drifts)
}

pub(super) fn existing_drift_record(
    history: &[StoredEvent],
    drift_id: &str,
    world_id: &str,
    kind: DriftKind,
    evidence_evaluation_id: &str,
) -> Result<Option<DriftRecord>, ExecuteError> {
    let event_id = drift_event_id(drift_id);
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return Ok(None);
    };
    let payload = decode_drift_record(event).map_err(|_| ExecuteError::Internal)?;
    if payload.world_id != world_id
        || payload.kind != kind
        || payload.evidence_evaluation_id != evidence_evaluation_id
    {
        return Err(ExecuteError::Rejected(
            "drift_id is already bound to different drift content".to_owned(),
        ));
    }
    Ok(Some(drift_record(payload, event)))
}

/// Derives the only payload the policy admits for this drift request.
pub(super) fn drift_record_payload(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    drift_id: &str,
    world_id: &str,
    kind: DriftKind,
    evidence_evaluation_id: &str,
) -> Result<DriftRecordPayload, ExecuteError> {
    registered.world(world_id).ok_or(ExecuteError::NotFound)?;
    let champion = champion_projection(history, world_id).map_err(|_| ExecuteError::Internal)?;
    let Some(baseline_genome_id) = champion.champion_genome_id else {
        return Err(ExecuteError::Rejected(
            "World has no Champion; seed one before recording drift".to_owned(),
        ));
    };

    validate_job_id(evidence_evaluation_id)
        .map_err(|_| ExecuteError::Invalid("evidence_evaluation_id is invalid"))?;
    let selection_event_id = format!("arena:selection:{evidence_evaluation_id}:selected");
    let selection_event = history
        .iter()
        .find(|event| event.event_id == selection_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let world = registered.world(world_id).ok_or(ExecuteError::Internal)?;
    let stores = hephaestus_arena::EvaluationStores::open(
        data_dir.join("events.sqlite3"),
        data_dir.join("blobs"),
    )
    .map_err(|_| ExecuteError::Internal)?;
    let verified =
        hephaestus_arena::verify_selection_event(stores, selection_event, world.compiled())
            .map_err(|_| ExecuteError::Internal)?;
    let receipt = verified.receipt().clone();
    drop(verified.into_stores());

    if receipt.world_id() != world_id || receipt.parent_genome_id() != baseline_genome_id {
        return Err(ExecuteError::Rejected(
            "evidence does not pair the current Champion as its baseline".to_owned(),
        ));
    }
    let shifted_genome_id = receipt.candidate_genome_id().to_owned();
    let deltas = regression_deltas(&receipt);
    let threshold = threshold_bps(kind);
    let (observed_delta_bps, crossed) = match kind {
        DriftKind::Latency => (
            deltas.latency_bps,
            deltas.latency_bps >= i64::from(threshold),
        ),
        DriftKind::Cost => (deltas.cost_bps, deltas.cost_bps >= i64::from(threshold)),
        DriftKind::Correctness => (
            deltas.correctness_bps,
            deltas.correctness_bps <= -i64::from(threshold),
        ),
        DriftKind::Workload => (
            deltas.reliability_bps,
            deltas.reliability_bps <= -i64::from(threshold),
        ),
    };
    if !crossed {
        return Err(ExecuteError::Rejected(
            "evidence does not show a shift beyond the documented threshold for this kind"
                .to_owned(),
        ));
    }

    Ok(DriftRecordPayload {
        schema_version: 1,
        drift_id: drift_id.to_owned(),
        world_id: world_id.to_owned(),
        kind,
        evidence_evaluation_id: evidence_evaluation_id.to_owned(),
        selection_event_id: selection_event.event_id.clone(),
        selection_event_hash: hex_encode(&selection_event.hash),
        baseline_genome_id,
        shifted_genome_id,
        threshold_bps: threshold,
        observed_delta_bps,
    })
}

/// Recomputes every drift record from the history that preceded it.
pub(super) fn verify_drift_history(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    for (index, event) in history.iter().enumerate() {
        if !is_drift_event(event) {
            continue;
        }
        let payload = decode_drift_record(event)?;
        if payload.schema_version != 1
            || event.event_type != DRIFT_EVENT_TYPE
            || event.actor != OPERATOR_ACTOR
            || event.event_id != drift_event_id(&payload.drift_id)
            || event.aggregate_id != drift_aggregate_id(&payload.world_id)
            || validate_job_id(&payload.drift_id).is_err()
        {
            return Err(ControlError::Projection(
                "drift event identity is invalid".to_owned(),
            ));
        }
        let expected = drift_record_payload(
            data_dir,
            &history[..index],
            registered,
            &payload.drift_id,
            &payload.world_id,
            payload.kind,
            &payload.evidence_evaluation_id,
        )
        .map_err(|_| ControlError::Projection("drift record was not admissible".to_owned()))?;
        if payload != expected {
            return Err(ControlError::Projection(
                "drift record differs from verified evidence".to_owned(),
            ));
        }
    }
    Ok(())
}

pub(super) fn drift_event_input(
    payload: &DriftRecordPayload,
    timestamp: i64,
) -> Result<EventInput, ExecuteError> {
    let payload_value = serde_json::to_value(payload).map_err(|_| ExecuteError::Internal)?;
    let payload_bytes = serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
    Ok(EventInput::new(
        drift_event_id(&payload.drift_id),
        drift_aggregate_id(&payload.world_id),
        DRIFT_EVENT_TYPE,
        OPERATOR_ACTOR,
        timestamp,
        payload_bytes,
    ))
}
