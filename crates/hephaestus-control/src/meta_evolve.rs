//! Recursive evolution of the Evolver (roadmap item 13).
//!
//! An Evolver strategy is a versioned, content-addressed bundle of the
//! knobs that steer the evolve engine's own admission policy: how many
//! generations it may run and how many paired trials it may spend, plus
//! forward-looking prioritization fields Forge does not yet act on. A
//! strategy cannot alter meta-evaluators, Laws, receipts, or budgets by
//! construction: [`EvolverStrategyConfig`] has no field that names or
//! touches any of those objects, and registering a strategy only ever
//! appends one content-addressed `meta_strategy.registered` event.
//!
//! A meta-evaluation (`meta_evolution.evaluated`) drives the existing,
//! unmodified evolve engine (`EvolveStart`, drained through the ordinary
//! reconciliation step) once per strategy over each of a caller-supplied set
//! of held-out base lineages, restoring each lineage's World Champion to its
//! starting Genome between the two strategy runs (and once more afterward)
//! by cooperative rollback, so the comparison is always paired on identical
//! starting conditions and the meta-evaluation leaves no lasting Champion
//! side effect. The resulting receipt records, per lineage, the Champion
//! quality reached (a coarse promotions-count proxy; see module docs on
//! `EvolveCommand` for why only the reference-operation-flip mutation is
//! distinguishable today) and the paired-trial cost spent, plus a bootstrap
//! confidence interval over both deltas computed in the same
//! histogram-resampling style as the Arena selection receipt
//! (`hephaestus_arena::selection`), independently implemented here because
//! that module's bootstrap is private to its own error type.

use hephaestus_ledger::StoredEvent;

use super::{ControlError, hex_encode};
use crate::protocol::{
    EvolutionEventRecord, EvolverStrategyConfig, MetaBootstrapInterval, MetaEvaluationPayload,
    MetaReceiptRecord, MetaStrategyRecord, MetaStrategyRegisteredPayload,
};

pub(super) const META_STRATEGY_EVENT_TYPE: &str = "meta_strategy.registered";
pub(super) const META_EVALUATION_EVENT_TYPE: &str = "meta_evolution.evaluated";
const META_STRATEGY_PREFIX: &str = "meta-strategy:";
const META_EVALUATION_PREFIX: &str = "meta-evolution:";

/// Deterministic bootstrap resamples, matching the Arena selection receipt's
/// magnitude.
pub(super) const RESAMPLES: usize = 10_000;

/// Versioned deterministic bootstrap algorithm identity for meta-evaluation
/// receipts.
pub(super) const BOOTSTRAP_ALGORITHM: &str = "lineage-paired-histogram-bootstrap-v1";

pub(super) fn meta_strategy_id(config: &EvolverStrategyConfig) -> Result<String, ControlError> {
    let canonical = canonical_bytes(config)?;
    Ok(format!(
        "hephaestus:meta-strategy:{}",
        blake3::hash(&canonical).to_hex()
    ))
}

pub(super) fn meta_strategy_event_id(strategy_id: &str) -> String {
    format!("{META_STRATEGY_PREFIX}{strategy_id}:registered")
}

pub(super) fn meta_strategy_aggregate_id(strategy_id: &str) -> String {
    format!("{META_STRATEGY_PREFIX}{strategy_id}")
}

pub(super) fn meta_evaluation_event_id(meta_run_id: &str) -> String {
    format!("{META_EVALUATION_PREFIX}{meta_run_id}:evaluated")
}

pub(super) fn meta_evaluation_aggregate_id(meta_run_id: &str) -> String {
    format!("{META_EVALUATION_PREFIX}{meta_run_id}")
}

fn canonical_bytes<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, ControlError> {
    let value = serde_json::to_value(value)?;
    Ok(serde_json::to_vec(&value)?)
}

fn is_meta_strategy_event(event: &StoredEvent) -> bool {
    event.event_type == META_STRATEGY_EVENT_TYPE
        || event.aggregate_id.starts_with(META_STRATEGY_PREFIX)
}

fn is_meta_evaluation_event(event: &StoredEvent) -> bool {
    event.event_type == META_EVALUATION_EVENT_TYPE
        || event.aggregate_id.starts_with(META_EVALUATION_PREFIX)
}

fn decode_strategy(event: &StoredEvent) -> Result<MetaStrategyRegisteredPayload, ControlError> {
    let payload = serde_json::from_slice::<MetaStrategyRegisteredPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("meta strategy payload is invalid".to_owned()))?;
    if canonical_bytes(&payload)? != event.payload {
        return Err(ControlError::Projection(
            "meta strategy payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

fn decode_evaluation(event: &StoredEvent) -> Result<MetaEvaluationPayload, ControlError> {
    let payload = serde_json::from_slice::<MetaEvaluationPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("meta evaluation payload is invalid".to_owned()))?;
    if canonical_bytes(&payload)? != event.payload {
        return Err(ControlError::Projection(
            "meta evaluation payload is not canonical".to_owned(),
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

/// Reconstructs one registered strategy's durable projection from verified
/// history.
pub(super) fn meta_strategy_projection(
    history: &[StoredEvent],
    strategy_id: &str,
) -> Result<Option<MetaStrategyRecord>, ControlError> {
    for event in history {
        if event.event_type != META_STRATEGY_EVENT_TYPE {
            continue;
        }
        let payload = decode_strategy(event)?;
        if payload.strategy_id == strategy_id {
            return Ok(Some(MetaStrategyRecord {
                strategy_id: payload.strategy_id,
                config: payload.config,
                event: event_record(event),
            }));
        }
    }
    Ok(None)
}

/// Every registered strategy, oldest first.
pub(super) fn meta_strategy_list(
    history: &[StoredEvent],
) -> Result<Vec<MetaStrategyRecord>, ControlError> {
    let mut strategies = Vec::new();
    for event in history {
        if event.event_type != META_STRATEGY_EVENT_TYPE {
            continue;
        }
        let payload = decode_strategy(event)?;
        strategies.push(MetaStrategyRecord {
            strategy_id: payload.strategy_id,
            config: payload.config,
            event: event_record(event),
        });
    }
    Ok(strategies)
}

/// Reconstructs one meta-evaluation's durable receipt from verified history.
pub(super) fn meta_evaluation_projection(
    history: &[StoredEvent],
    meta_run_id: &str,
) -> Result<Option<MetaReceiptRecord>, ControlError> {
    for event in history {
        if event.event_type != META_EVALUATION_EVENT_TYPE {
            continue;
        }
        let payload = decode_evaluation(event)?;
        if payload.meta_run_id == meta_run_id {
            return Ok(Some(MetaReceiptRecord {
                payload,
                event: event_record(event),
            }));
        }
    }
    Ok(None)
}

/// Recent meta-evaluation receipts, newest first, bounded by `limit`.
pub(super) fn meta_evaluation_list(
    history: &[StoredEvent],
    limit: u32,
) -> Result<Vec<MetaReceiptRecord>, ControlError> {
    let mut receipts = Vec::new();
    for event in history.iter().rev() {
        if event.event_type != META_EVALUATION_EVENT_TYPE {
            continue;
        }
        if receipts.len() >= limit as usize {
            break;
        }
        let payload = decode_evaluation(event)?;
        receipts.push(MetaReceiptRecord {
            payload,
            event: event_record(event),
        });
    }
    Ok(receipts)
}

/// Recomputes and cross-references every meta-evolution event from the
/// history that preceded it. Never submits Arena work or performs any side
/// effect.
#[allow(clippy::too_many_lines)]
pub(super) fn verify_meta_evolution_history(history: &[StoredEvent]) -> Result<(), ControlError> {
    let bad = || ControlError::Projection("meta evolution history is invalid".to_owned());
    let mut seen_strategies: std::collections::BTreeMap<String, MetaStrategyRegisteredPayload> =
        std::collections::BTreeMap::new();
    let mut seen_evaluations: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for (index, event) in history.iter().enumerate() {
        if is_meta_strategy_event(event) {
            if event.event_type != META_STRATEGY_EVENT_TYPE {
                return Err(bad());
            }
            let payload = decode_strategy(event)?;
            let expected_id = meta_strategy_id(&payload.config).map_err(|_| bad())?;
            if payload.strategy_id != expected_id
                || event.event_id != meta_strategy_event_id(&payload.strategy_id)
                || event.aggregate_id != meta_strategy_aggregate_id(&payload.strategy_id)
                || payload.schema_version != 1
                || payload.config.schema_version != 1
                || payload.config.generation_count == 0
                || payload.config.candidate_count == 0
            {
                return Err(bad());
            }
            if let Some(parent_id) = payload.config.parent_strategy_id.as_deref()
                && (parent_id == payload.strategy_id || !seen_strategies.contains_key(parent_id))
            {
                return Err(bad());
            }
            if let Some(existing) = seen_strategies.get(&payload.strategy_id)
                && *existing != payload
            {
                return Err(bad());
            }
            seen_strategies.insert(payload.strategy_id.clone(), payload);
        } else if is_meta_evaluation_event(event) {
            if event.event_type != META_EVALUATION_EVENT_TYPE {
                return Err(bad());
            }
            let payload = decode_evaluation(event)?;
            if event.event_id != meta_evaluation_event_id(&payload.meta_run_id)
                || event.aggregate_id != meta_evaluation_aggregate_id(&payload.meta_run_id)
                || payload.schema_version != 1
                || payload.algorithm != BOOTSTRAP_ALGORITHM
                || payload.bootstrap_resamples != u32::try_from(RESAMPLES).map_err(|_| bad())?
                || !seen_evaluations.insert(payload.meta_run_id.clone())
                || payload.lineages.len() < 2
                || payload.strategy_a_id == payload.strategy_b_id
            {
                return Err(bad());
            }
            let strategy_a = seen_strategies
                .get(&payload.strategy_a_id)
                .ok_or_else(bad)?;
            let strategy_b = seen_strategies
                .get(&payload.strategy_b_id)
                .ok_or_else(bad)?;
            let mut quality_deltas = Vec::with_capacity(payload.lineages.len());
            let mut cost_deltas = Vec::with_capacity(payload.lineages.len());
            for lineage in &payload.lineages {
                let run_a =
                    super::evolution_projection(&history[..index], &lineage.strategy_a_run_id)
                        .map_err(|_| bad())?
                        .ok_or_else(bad)?;
                let run_b =
                    super::evolution_projection(&history[..index], &lineage.strategy_b_run_id)
                        .map_err(|_| bad())?
                        .ok_or_else(bad)?;
                if run_a.world_id != lineage.world_id
                    || run_b.world_id != lineage.world_id
                    || run_a.from_genome_id != lineage.from_genome_id
                    || run_b.from_genome_id != lineage.from_genome_id
                    || run_a.max_generations != strategy_a.config.generation_count
                    || run_a.max_paired_trials != strategy_a.config.experiment_allocation
                    || run_b.max_generations != strategy_b.config.generation_count
                    || run_b.max_paired_trials != strategy_b.config.experiment_allocation
                    || run_a.state != crate::protocol::EvolutionRunState::Finished
                    || run_b.state != crate::protocol::EvolutionRunState::Finished
                {
                    return Err(bad());
                }
                let promotions_a = promotions_of(&run_a);
                let promotions_b = promotions_of(&run_b);
                let champion_a = champion_after(&run_a);
                let champion_b = champion_after(&run_b);
                if lineage.strategy_a_promotions != promotions_a
                    || lineage.strategy_b_promotions != promotions_b
                    || lineage.strategy_a_trials_consumed != run_a.trials_consumed
                    || lineage.strategy_b_trials_consumed != run_b.trials_consumed
                    || lineage.strategy_a_champion_genome_id != champion_a
                    || lineage.strategy_b_champion_genome_id != champion_b
                {
                    return Err(bad());
                }
                quality_deltas.push(i64::from(promotions_b) - i64::from(promotions_a));
                cost_deltas.push(
                    i64::try_from(run_b.trials_consumed)
                        .ok()
                        .zip(i64::try_from(run_a.trials_consumed).ok())
                        .map(|(b, a)| b - a)
                        .ok_or_else(bad)?,
                );
            }
            let expected_quality = paired_bootstrap(
                &quality_deltas,
                payload.bootstrap_seed,
                payload.confidence_bps,
            )
            .map_err(|_| bad())?;
            let expected_cost =
                paired_bootstrap(&cost_deltas, payload.bootstrap_seed, payload.confidence_bps)
                    .map_err(|_| bad())?;
            if payload.quality_delta != expected_quality || payload.cost_delta != expected_cost {
                return Err(bad());
            }
            let expected_verdict = descendant_verdict(
                &payload.strategy_a_id,
                &strategy_a.config,
                &payload.strategy_b_id,
                &strategy_b.config,
                &expected_quality,
                &expected_cost,
            );
            if payload.descendant_cheaper_at_equal_quality != expected_verdict {
                return Err(bad());
            }
        }
    }
    Ok(())
}

/// Whether one of the two compared strategies is the other's declared
/// descendant and, if so, whether that descendant reached an equal-or-better
/// Champion at a statistically lower experiment cost.
///
/// `quality_delta`/`cost_delta` are always oriented strategy-B-minus-A; when
/// A is the declared descendant of B, this reorients them to
/// descendant-minus-ancestor before comparing (negating an interval swaps
/// which bound is the lower one). Returns `None` when neither strategy
/// declares the other as its parent.
pub(super) fn descendant_verdict(
    strategy_a_id: &str,
    strategy_a: &crate::protocol::EvolverStrategyConfig,
    strategy_b_id: &str,
    strategy_b: &crate::protocol::EvolverStrategyConfig,
    quality_delta: &MetaBootstrapInterval,
    cost_delta: &MetaBootstrapInterval,
) -> Option<bool> {
    let (quality_lower, cost_upper) = if strategy_b.parent_strategy_id.as_deref() == Some(strategy_a_id) {
        (quality_delta.lower_x10000, cost_delta.upper_x10000)
    } else if strategy_a.parent_strategy_id.as_deref() == Some(strategy_b_id) {
        (-quality_delta.upper_x10000, -cost_delta.lower_x10000)
    } else {
        return None;
    };
    Some(quality_lower >= 0 && cost_upper < 0)
}

pub(super) fn promotions_of(run: &crate::protocol::EvolutionRunRecord) -> u32 {
    u32::try_from(
        run.generations
            .iter()
            .filter(|generation| generation.payload.promoted)
            .count(),
    )
    .unwrap_or(u32::MAX)
}

pub(super) fn champion_after(run: &crate::protocol::EvolutionRunRecord) -> String {
    run.generations.last().map_or_else(
        || run.from_genome_id.clone(),
        |generation| generation.payload.champion_after.clone(),
    )
}

/// Errors a bootstrap computation can fail with; kept distinct from
/// `hephaestus_arena::ArenaError` because this module has no Arena
/// evaluation context to attach.
#[derive(Debug)]
pub(super) struct BootstrapError;

/// Deterministic paired bootstrap over lineage-level integer deltas, in the
/// same histogram-resampling style as the Arena selection receipt: a fixed
/// resample count driven by a seeded `SplitMix64` stream, with the estimate
/// and interval bounds scaled by `10_000`.
///
/// # Errors
///
/// Fails if `deltas` is empty or an intermediate sum overflows `i64`.
pub(super) fn paired_bootstrap(
    deltas: &[i64],
    seed: u64,
    confidence_bps: u16,
) -> Result<MetaBootstrapInterval, BootstrapError> {
    if deltas.is_empty() {
        return Err(BootstrapError);
    }
    let count = i64::try_from(deltas.len()).map_err(|_| BootstrapError)?;
    let estimate_sum = deltas
        .iter()
        .try_fold(0_i64, |sum, value| sum.checked_add(*value))
        .ok_or(BootstrapError)?;
    let estimate_x10000 = estimate_sum
        .checked_mul(10_000)
        .ok_or(BootstrapError)?
        .div_euclid(count);
    let mut random = SplitMix64(seed);
    let mut distribution = Vec::with_capacity(RESAMPLES);
    for _ in 0..RESAMPLES {
        let mut sum = 0_i64;
        for _ in 0..deltas.len() {
            let index =
                usize::try_from(random.next() % deltas.len() as u64).map_err(|_| BootstrapError)?;
            sum = sum.checked_add(deltas[index]).ok_or(BootstrapError)?;
        }
        let value = sum
            .checked_mul(10_000)
            .ok_or(BootstrapError)?
            .div_euclid(count);
        distribution.push(value);
    }
    distribution.sort_unstable();
    let confidence = u64::from(confidence_bps.min(9_999));
    let tail = (10_000 - confidence) / 2;
    let lower_index = usize::try_from(tail * (RESAMPLES as u64 - 1) / 10_000).unwrap_or(0);
    let upper_index =
        usize::try_from((10_000 - tail) * (RESAMPLES as u64 - 1) / 10_000).unwrap_or(RESAMPLES - 1);
    Ok(MetaBootstrapInterval {
        estimate_x10000,
        lower_x10000: distribution[lower_index],
        upper_x10000: distribution[upper_index],
    })
}

struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::{SplitMix64, paired_bootstrap};

    #[test]
    fn splitmix64_matches_reference_stream() {
        let mut random = SplitMix64(0);
        assert_eq!(random.next(), 16_294_208_416_658_607_535);
        assert_eq!(random.next(), 7_960_286_522_194_355_700);
    }

    #[test]
    fn bootstrap_of_identical_strategies_centers_on_zero() {
        let interval = paired_bootstrap(&[0, 0, 0, 0], 42, 9_500).expect("bootstrap succeeds");
        assert_eq!(interval.estimate_x10000, 0);
        assert_eq!(interval.lower_x10000, 0);
        assert_eq!(interval.upper_x10000, 0);
    }

    #[test]
    fn bootstrap_is_deterministic_for_a_fixed_seed() {
        let deltas = [1_i64, -1, 2, 0, 1];
        let first = paired_bootstrap(&deltas, 7, 9_500).expect("bootstrap succeeds");
        let second = paired_bootstrap(&deltas, 7, 9_500).expect("bootstrap succeeds");
        assert_eq!(first.estimate_x10000, second.estimate_x10000);
        assert_eq!(first.lower_x10000, second.lower_x10000);
        assert_eq!(first.upper_x10000, second.upper_x10000);
    }

    #[test]
    fn bootstrap_rejects_empty_deltas() {
        assert!(paired_bootstrap(&[], 0, 9_500).is_err());
    }
}
