//! Durable, replay-verified autonomous evolution runs.
//!
//! An evolution run drives the existing Arena, Forge, and Champion primitives
//! one generation at a time from the daemon's own reconciliation loop
//! (`ControlPlane::advance_evolution`, called every `serve` tick), never from
//! a client connection. Each generation evaluates the current Champion as a
//! selected candidate, proposes one Forge mutation of it, evaluates the child
//! against the Champion, assesses that evidence, and promotes the child only
//! when the deterministic policy admits it. One `evolution.started` event
//! bounds a run's configuration, one `evolution.generation` event per
//! completed generation records its outcome, and one `evolution.finished`
//! event records why the run stopped. `evolution.cancel_requested` is the
//! only externally triggered event; every other event is written solely by
//! the reconciliation loop. Replay recomputes and cross-references every
//! event's shape from the history that preceded it, exactly like Champion
//! transitions.

use hephaestus_genome::RegisteredObjects;
use hephaestus_ledger::StoredEvent;

use super::champion::decode_champion_transition;
use super::{
    ControlError, OPERATOR_ACTOR, decode_forge_assessment, decode_forge_proposal,
    forge_assessment_event_id, forge_event_id, hex_encode, validate_job_id,
};
use crate::protocol::{
    ChampionTransitionKind, EvolutionCancelPayload, EvolutionEventRecord, EvolutionFinishReason,
    EvolutionFinishedPayload, EvolutionGenerationPayload, EvolutionGenerationRecord,
    EvolutionRunRecord, EvolutionRunState, EvolutionStartedPayload, ForgeAssessmentOutcome,
};

pub(super) const EVOLUTION_STARTED_TYPE: &str = "evolution.started";
pub(super) const EVOLUTION_GENERATION_TYPE: &str = "evolution.generation";
pub(super) const EVOLUTION_FINISHED_TYPE: &str = "evolution.finished";
pub(super) const EVOLUTION_CANCEL_TYPE: &str = "evolution.cancel_requested";
const EVOLUTION_PREFIX: &str = "evolution:";

/// Paired Arena evaluations (trials) exactly one generation consumes: a
/// diagnostic selection establishing the Champion as the mutated candidate,
/// plus a child-versus-Champion assessment evaluation.
pub(super) const TRIALS_PER_GENERATION: u64 = 2;

pub(super) fn evolution_started_event_id(run_id: &str) -> String {
    format!("{EVOLUTION_PREFIX}{run_id}:started")
}

pub(super) fn evolution_generation_event_id(run_id: &str, generation_index: u32) -> String {
    format!("{EVOLUTION_PREFIX}{run_id}:generation:{generation_index}")
}

pub(super) fn evolution_finished_event_id(run_id: &str) -> String {
    format!("{EVOLUTION_PREFIX}{run_id}:finished")
}

pub(super) fn evolution_cancel_event_id(run_id: &str) -> String {
    format!("{EVOLUTION_PREFIX}{run_id}:cancel_requested")
}

pub(super) fn evolution_aggregate_id(run_id: &str) -> String {
    format!("{EVOLUTION_PREFIX}{run_id}")
}

fn is_evolution_event(event: &StoredEvent) -> bool {
    event.event_type == EVOLUTION_STARTED_TYPE
        || event.event_type == EVOLUTION_GENERATION_TYPE
        || event.event_type == EVOLUTION_FINISHED_TYPE
        || event.event_type == EVOLUTION_CANCEL_TYPE
        || event.event_id.starts_with(EVOLUTION_PREFIX)
        || event.aggregate_id.starts_with(EVOLUTION_PREFIX)
}

fn decode_started(event: &StoredEvent) -> Result<EvolutionStartedPayload, ControlError> {
    let payload = serde_json::from_slice::<EvolutionStartedPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("evolution start payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "evolution start payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

fn decode_generation(event: &StoredEvent) -> Result<EvolutionGenerationPayload, ControlError> {
    let payload =
        serde_json::from_slice::<EvolutionGenerationPayload>(&event.payload).map_err(|_| {
            ControlError::Projection("evolution generation payload is invalid".to_owned())
        })?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "evolution generation payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

fn decode_finished(event: &StoredEvent) -> Result<EvolutionFinishedPayload, ControlError> {
    let payload = serde_json::from_slice::<EvolutionFinishedPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("evolution finish payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "evolution finish payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

fn decode_cancel(event: &StoredEvent) -> Result<EvolutionCancelPayload, ControlError> {
    let payload = serde_json::from_slice::<EvolutionCancelPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("evolution cancel payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "evolution cancel payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

fn event_record(event: &StoredEvent) -> EvolutionEventRecord {
    EvolutionEventRecord {
        sequence: event.sequence,
        event_id: event.event_id.clone(),
        aggregate_id: event.aggregate_id.clone(),
        event_type: event.event_type.clone(),
        actor: event.actor.clone(),
        event_hash: hex_encode(&event.hash),
    }
}

/// The `run_id` of the one evolution run, if any, that has started but not
/// yet finished. Admission guarantees at most one such run exists at a time.
pub(super) fn active_evolution_run_id(
    history: &[StoredEvent],
) -> Result<Option<String>, ControlError> {
    let mut started: Vec<String> = Vec::new();
    let mut finished: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for event in history {
        match event.event_type.as_str() {
            EVOLUTION_STARTED_TYPE => {
                let payload = decode_started(event)?;
                if !started.contains(&payload.run_id) {
                    started.push(payload.run_id);
                }
            }
            EVOLUTION_FINISHED_TYPE => {
                finished.insert(decode_finished(event)?.run_id);
            }
            _ => {}
        }
    }
    Ok(started
        .into_iter()
        .find(|run_id| !finished.contains(run_id)))
}

/// Reconstructs one evolution run's durable projection from verified history.
pub(super) fn evolution_projection(
    history: &[StoredEvent],
    run_id: &str,
) -> Result<Option<EvolutionRunRecord>, ControlError> {
    let mut record: Option<EvolutionRunRecord> = None;
    for event in history {
        match event.event_type.as_str() {
            EVOLUTION_STARTED_TYPE => {
                let payload = decode_started(event)?;
                if payload.run_id != run_id {
                    continue;
                }
                record = Some(EvolutionRunRecord {
                    run_id: payload.run_id,
                    world_id: payload.world_id,
                    from_genome_id: payload.from_genome_id,
                    baseline_genome_id: payload.baseline_genome_id,
                    max_generations: payload.max_generations,
                    max_paired_trials: payload.max_paired_trials,
                    trials_consumed: 0,
                    state: EvolutionRunState::Running,
                    cancel_requested: false,
                    finish_reason: None,
                    generations: Vec::new(),
                    started_event: event_record(event),
                    finished_event: None,
                });
            }
            EVOLUTION_GENERATION_TYPE => {
                let payload = decode_generation(event)?;
                if payload.run_id != run_id {
                    continue;
                }
                let run = record.as_mut().ok_or_else(|| {
                    ControlError::Projection("evolution generation precedes its run".to_owned())
                })?;
                run.trials_consumed = run.trials_consumed.saturating_add(TRIALS_PER_GENERATION);
                run.generations.push(EvolutionGenerationRecord {
                    payload,
                    event: event_record(event),
                });
            }
            EVOLUTION_FINISHED_TYPE => {
                let payload = decode_finished(event)?;
                if payload.run_id != run_id {
                    continue;
                }
                let run = record.as_mut().ok_or_else(|| {
                    ControlError::Projection("evolution finish precedes its run".to_owned())
                })?;
                run.state = EvolutionRunState::Finished;
                run.finish_reason = Some(payload.reason);
                run.finished_event = Some(event_record(event));
            }
            EVOLUTION_CANCEL_TYPE => {
                let payload = decode_cancel(event)?;
                if payload.run_id != run_id {
                    continue;
                }
                let run = record.as_mut().ok_or_else(|| {
                    ControlError::Projection("evolution cancel precedes its run".to_owned())
                })?;
                run.cancel_requested = true;
            }
            _ => {}
        }
    }
    Ok(record)
}

/// Recomputes and cross-references every evolution event from the history
/// that preceded it. Never submits Arena work or performs any side effect.
#[allow(clippy::too_many_lines)]
pub(super) fn verify_evolution_history(
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    let bad = || ControlError::Projection("evolution history is invalid".to_owned());
    let mut seen_started: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut seen_finished: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (index, event) in history.iter().enumerate() {
        if !is_evolution_event(event) {
            continue;
        }
        let prefix_ok = event.event_id.starts_with(EVOLUTION_PREFIX)
            && event.aggregate_id.starts_with(EVOLUTION_PREFIX)
            && event.actor == OPERATOR_ACTOR;
        match event.event_type.as_str() {
            EVOLUTION_STARTED_TYPE => {
                let payload = decode_started(event)?;
                if !prefix_ok
                    || event.event_id != evolution_started_event_id(&payload.run_id)
                    || event.aggregate_id != evolution_aggregate_id(&payload.run_id)
                    || validate_job_id(&payload.run_id).is_err()
                    || payload.schema_version != 1
                    || !seen_started.insert(payload.run_id.clone())
                {
                    return Err(bad());
                }
                registered.world(&payload.world_id).ok_or_else(bad)?;
                let from_genome = registered.genome(&payload.from_genome_id).ok_or_else(bad)?;
                let baseline_genome = registered
                    .genome(&payload.baseline_genome_id)
                    .ok_or_else(bad)?;
                if from_genome.record().world_id != payload.world_id
                    || baseline_genome.record().world_id != payload.world_id
                    || payload.baseline_genome_id == payload.from_genome_id
                    || payload.max_generations == 0
                    || payload.max_paired_trials < TRIALS_PER_GENERATION
                {
                    return Err(bad());
                }
            }
            EVOLUTION_GENERATION_TYPE => {
                let payload = decode_generation(event)?;
                if !prefix_ok
                    || event.event_id
                        != evolution_generation_event_id(&payload.run_id, payload.generation_index)
                    || event.aggregate_id != evolution_aggregate_id(&payload.run_id)
                    || payload.schema_version != 1
                    || !seen_started.contains(&payload.run_id)
                    || seen_finished.contains(&payload.run_id)
                {
                    return Err(bad());
                }
                let prior =
                    evolution_projection(&history[..index], &payload.run_id)?.ok_or_else(bad)?;
                let expected_index = u32::try_from(prior.generations.len()).map_err(|_| bad())?;
                if payload.generation_index != expected_index {
                    return Err(bad());
                }
                let expected_champion_before = prior.generations.last().map_or_else(
                    || prior.from_genome_id.clone(),
                    |generation| generation.payload.champion_after.clone(),
                );
                if payload.champion_before != expected_champion_before {
                    return Err(bad());
                }
                let proposal_event = history[..index]
                    .iter()
                    .find(|candidate| candidate.event_id == forge_event_id(&payload.proposal_id))
                    .ok_or_else(bad)?;
                let proposal = decode_forge_proposal(proposal_event)?;
                if proposal.parent_genome_id != payload.champion_before
                    || proposal.child.genome_id != payload.child_genome_id
                    || proposal.world_id != prior.world_id
                {
                    return Err(bad());
                }
                let assessment_event = history[..index]
                    .iter()
                    .find(|candidate| {
                        candidate.event_id == forge_assessment_event_id(&payload.assessment_id)
                    })
                    .ok_or_else(bad)?;
                let assessment = decode_forge_assessment(assessment_event)?;
                if assessment.proposal_id != payload.proposal_id
                    || assessment.evaluation_id != payload.child_evaluation_id
                    || assessment.parent_genome_id != payload.champion_before
                    || assessment.child_genome_id != payload.child_genome_id
                {
                    return Err(bad());
                }
                if payload.promoted && assessment.outcome != ForgeAssessmentOutcome::MetricsPassed {
                    return Err(bad());
                }
                if payload.promoted {
                    if payload.champion_after != payload.child_genome_id {
                        return Err(bad());
                    }
                    let promoted = history[..index].iter().any(|candidate| {
                        decode_champion_transition(candidate).is_ok_and(|transition| {
                            transition.kind == ChampionTransitionKind::Promoted
                                && transition.champion_genome_id == payload.child_genome_id
                                && transition.promotion.as_ref().is_some_and(|evidence| {
                                    evidence.assessment_id == payload.assessment_id
                                })
                        })
                    });
                    if !promoted {
                        return Err(bad());
                    }
                } else if payload.champion_after != payload.champion_before {
                    return Err(bad());
                }
            }
            EVOLUTION_FINISHED_TYPE => {
                let payload = decode_finished(event)?;
                if !prefix_ok
                    || event.event_id != evolution_finished_event_id(&payload.run_id)
                    || event.aggregate_id != evolution_aggregate_id(&payload.run_id)
                    || payload.schema_version != 1
                    || !seen_started.contains(&payload.run_id)
                    || !seen_finished.insert(payload.run_id.clone())
                {
                    return Err(bad());
                }
                let prior =
                    evolution_projection(&history[..index], &payload.run_id)?.ok_or_else(bad)?;
                let completed = u32::try_from(prior.generations.len()).map_err(|_| bad())?;
                if payload.generations_completed != completed
                    || payload.trials_consumed != prior.trials_consumed
                    || (payload.reason == EvolutionFinishReason::Cancelled
                        && !prior.cancel_requested)
                {
                    return Err(bad());
                }
            }
            EVOLUTION_CANCEL_TYPE => {
                let payload = decode_cancel(event)?;
                if !prefix_ok
                    || event.event_id != evolution_cancel_event_id(&payload.run_id)
                    || event.aggregate_id != evolution_aggregate_id(&payload.run_id)
                    || payload.schema_version != 1
                    || !seen_started.contains(&payload.run_id)
                    || seen_finished.contains(&payload.run_id)
                {
                    return Err(bad());
                }
            }
            _ => return Err(bad()),
        }
    }
    Ok(())
}
