//! Deterministic selection analysis and durable, evaluation-bound receipts.

use hephaestus_genome::CompiledWorld;
use hephaestus_ledger::{ArtifactId, EventInput, StoredEvent};
use serde::{Deserialize, Serialize};

use crate::{ArenaError, EvaluationStores, SelectionEvidence, load_operator_evaluation};

const EVENT_TYPE: &str = "selection.recorded";
const EVENT_ACTOR: &str = "arena-plane";
const RECEIPT_SCHEMA_VERSION: u16 = 1;
/// Original dominance rule: candidate latency must be no worse than the
/// parent's, at all, on raw measured wall-clock milliseconds. Retained only
/// so ledgers written before the tolerant rule below still recompute
/// byte-identical receipts; new selections never use it.
const ALGORITHM_V1: &str = "histogram-bootstrap-v1";
/// Current dominance rule: candidate latency only counts as a regression
/// once it exceeds the parent's by more than a fixed noise floor
/// (`max(10% of parent latency, 50ms * paired task count)`), because raw
/// wall-clock milliseconds on small reference tasks are dominated by
/// scheduling jitter rather than a real performance difference. See
/// `latency_tolerance_millis` and `pareto_dominates_v2`.
const ALGORITHM_V2: &str = "histogram-bootstrap-pareto-tolerant-v2";
/// Algorithm identity used for every newly recorded selection.
const CURRENT_ALGORITHM: &str = ALGORITHM_V2;
const RESAMPLES: usize = 10_000;
const MAX_BOOTSTRAP_DRAWS: usize = 20_000_000;

/// Canonical deterministic selection receipt schema.
// These four booleans are separate canonical audit facts for distinct gates.
// Keeping them visible prevents measured eligibility from implying promotion.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionReceipt {
    schema_version: u16,
    algorithm: String,
    resamples: u32,
    seed: u64,
    evaluation_id: String,
    evaluation_event_id: String,
    evaluation_event_hash: String,
    world_id: String,
    parent_genome_id: String,
    candidate_genome_id: String,
    maximum_cost_microusd: u64,
    minimum_delta_bps: i64,
    maximum_regressions: u32,
    confidence_bps: u16,
    correctness_regressions: u32,
    correctness_unchanged: u32,
    correctness_improvements: u32,
    estimate_bps: i64,
    lower_bps: i64,
    upper_bps: i64,
    parent_correctness_bps: u32,
    candidate_correctness_bps: u32,
    parent_reliability_bps: u32,
    candidate_reliability_bps: u32,
    parent_cost_microusd: u64,
    candidate_cost_microusd: u64,
    parent_latency_millis: u64,
    candidate_latency_millis: u64,
    candidate_pareto_dominates: bool,
    metrics_eligible: bool,
    invariant_gate_verified: bool,
    promotion_eligible: bool,
}

impl SelectionReceipt {
    /// Receipt schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
    /// Versioned deterministic bootstrap algorithm.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }
    /// Number of bootstrap resamples.
    #[must_use]
    pub const fn resamples(&self) -> u32 {
        self.resamples
    }
    /// Seed bound into the source evaluation.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }
    /// Stable input evaluation identity.
    #[must_use]
    pub fn evaluation_id(&self) -> &str {
        &self.evaluation_id
    }
    /// Source evaluation event identity.
    #[must_use]
    pub fn evaluation_event_id(&self) -> &str {
        &self.evaluation_event_id
    }
    /// Hash of the exact verified source event.
    #[must_use]
    pub fn evaluation_event_hash(&self) -> &str {
        &self.evaluation_event_hash
    }
    /// Exact compiled World identity.
    #[must_use]
    pub fn world_id(&self) -> &str {
        &self.world_id
    }
    /// Parent Genome identity.
    #[must_use]
    pub fn parent_genome_id(&self) -> &str {
        &self.parent_genome_id
    }
    /// Candidate Genome identity.
    #[must_use]
    pub fn candidate_genome_id(&self) -> &str {
        &self.candidate_genome_id
    }
    /// World maximum-delta policy in basis points.
    #[must_use]
    pub const fn minimum_delta_bps(&self) -> i64 {
        self.minimum_delta_bps
    }
    /// World maximum invariant regression policy (not checked by this receipt).
    #[must_use]
    pub const fn maximum_regressions(&self) -> u32 {
        self.maximum_regressions
    }
    /// World confidence policy in basis points.
    #[must_use]
    pub const fn confidence_bps(&self) -> u16 {
        self.confidence_bps
    }
    /// World aggregate candidate cost ceiling in micro-US dollars.
    #[must_use]
    pub const fn maximum_cost_microusd(&self) -> u64 {
        self.maximum_cost_microusd
    }
    /// Whether measured confidence and Pareto dimensions pass their gates.
    #[must_use]
    pub const fn metrics_eligible(&self) -> bool {
        self.metrics_eligible
    }
    /// Whether an independent trusted invariant evaluator supplied invariant evidence.
    #[must_use]
    pub const fn invariant_gate_verified(&self) -> bool {
        self.invariant_gate_verified
    }
    /// Promotion is always false until the invariant gate is implemented.
    #[must_use]
    pub const fn promotion_eligible(&self) -> bool {
        self.promotion_eligible
    }
    /// Correctness losses in the paired task histogram; this is not an invariant count.
    #[must_use]
    pub const fn correctness_regressions(&self) -> u32 {
        self.correctness_regressions
    }
    /// Number of unchanged paired correctness outcomes.
    #[must_use]
    pub const fn correctness_unchanged(&self) -> u32 {
        self.correctness_unchanged
    }
    /// Number of improved paired correctness outcomes.
    #[must_use]
    pub const fn correctness_improvements(&self) -> u32 {
        self.correctness_improvements
    }
    /// Paired correctness mean estimate in basis points.
    #[must_use]
    pub const fn estimate_bps(&self) -> i64 {
        self.estimate_bps
    }
    /// Lower percentile bound of the paired correctness delta.
    #[must_use]
    pub const fn lower_bps(&self) -> i64 {
        self.lower_bps
    }
    /// Upper percentile bound of the paired correctness delta.
    #[must_use]
    pub const fn upper_bps(&self) -> i64 {
        self.upper_bps
    }
    /// Parent correctness fitness in basis points.
    #[must_use]
    pub const fn parent_correctness_bps(&self) -> u32 {
        self.parent_correctness_bps
    }
    /// Candidate correctness fitness in basis points.
    #[must_use]
    pub const fn candidate_correctness_bps(&self) -> u32 {
        self.candidate_correctness_bps
    }
    /// Parent reliable-trial proportion in basis points.
    #[must_use]
    pub const fn parent_reliability_bps(&self) -> u32 {
        self.parent_reliability_bps
    }
    /// Candidate reliable-trial proportion in basis points.
    #[must_use]
    pub const fn candidate_reliability_bps(&self) -> u32 {
        self.candidate_reliability_bps
    }
    /// Parent aggregate cost in micro-US dollars.
    #[must_use]
    pub const fn parent_cost_microusd(&self) -> u64 {
        self.parent_cost_microusd
    }
    /// Candidate aggregate cost in micro-US dollars.
    #[must_use]
    pub const fn candidate_cost_microusd(&self) -> u64 {
        self.candidate_cost_microusd
    }
    /// Parent aggregate terminal latency in milliseconds.
    #[must_use]
    pub const fn parent_latency_millis(&self) -> u64 {
        self.parent_latency_millis
    }
    /// Candidate aggregate terminal latency in milliseconds.
    #[must_use]
    pub const fn candidate_latency_millis(&self) -> u64 {
        self.candidate_latency_millis
    }
    /// Whether the candidate is no worse in every measured Pareto dimension and strictly better in one.
    #[must_use]
    pub const fn candidate_pareto_dominates(&self) -> bool {
        self.candidate_pareto_dominates
    }
}

/// Payload-free event metadata for a durable selection receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionEvent {
    /// Canonical global sequence.
    pub sequence: u64,
    /// Deterministic selection event identity.
    pub event_id: String,
    /// Stable evaluation aggregate identity.
    pub aggregate_id: String,
    /// Event type.
    pub event_type: String,
    /// Trusted actor.
    pub actor: String,
    /// Event-chain hash in canonical lowercase hex.
    pub event_hash: String,
    /// BLAKE3 address of canonical selection receipt bytes.
    pub receipt_artifact_id: String,
}

/// Trusted result of selecting one persisted evaluation.
pub struct OperatorSelection {
    receipt: SelectionReceipt,
    event: SelectionEvent,
    stores: EvaluationStores,
}

impl OperatorSelection {
    /// Canonical deterministic measurements and policy-bound decision fields.
    #[must_use]
    pub const fn receipt(&self) -> &SelectionReceipt {
        &self.receipt
    }
    /// Canonical event metadata binding the receipt artifact into history.
    #[must_use]
    pub const fn event(&self) -> &SelectionEvent {
        &self.event
    }
    /// Returns the stores for the next durable Arena operation.
    #[must_use]
    pub fn into_stores(self) -> EvaluationStores {
        self.stores
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct SelectionEventPayload {
    schema_version: u16,
    evaluation_id: String,
    world_id: String,
    receipt_artifact_id: String,
}

/// Resolve identity hints from a strict canonical selection envelope.
/// They are untrusted until [`verify_selection_event`] succeeds.
///
/// # Errors
///
/// Returns an error if the event is malformed or its envelope is non-canonical.
pub fn selection_event_references(event: &StoredEvent) -> Result<(String, String), ArenaError> {
    let payload: SelectionEventPayload = serde_json::from_slice(&event.payload)?;
    if serde_json::to_vec(&payload)? != event.payload
        || payload.schema_version != RECEIPT_SCHEMA_VERSION
        || event.event_type != EVENT_TYPE
        || event.actor != EVENT_ACTOR
        || event.event_id != format!("arena:selection:{}:selected", payload.evaluation_id)
        || event.aggregate_id != format!("arena:selection:{}", payload.evaluation_id)
    {
        return Err(ArenaError::InvalidSelectionEvent);
    }
    Ok((payload.evaluation_id, payload.world_id))
}

/// Load trusted evaluation evidence, compute the deterministic selection receipt,
/// and persist one event. An exact retry recomputes and returns the existing receipt.
///
/// # Errors
///
/// Rejects an unverified evaluation, mismatched World, unsupported 10000-bps
/// confidence, excessive bootstrap work, malformed history, or conflicting retry.
pub fn select_and_record(
    stores: EvaluationStores,
    evaluation_id: &str,
    world: &CompiledWorld,
    timestamp_millis: i64,
) -> Result<OperatorSelection, ArenaError> {
    select(stores, evaluation_id, world, timestamp_millis, true)
}

/// Recomputes and validates one already-recorded selection without appending.
///
/// # Errors
///
/// Fails when the source evaluation, World policy, selection event, or CAS
/// artifact is missing, malformed, or inconsistent with recomputed evidence.
pub fn load_selection(
    stores: EvaluationStores,
    evaluation_id: &str,
    world: &CompiledWorld,
) -> Result<OperatorSelection, ArenaError> {
    select(stores, evaluation_id, world, 0, false)
}

/// Validate one selection during daemon startup or explicit history replay.
///
/// # Errors
///
/// Fails unless the supplied event exactly matches canonical history and the
/// receipt recomputes from the verified source evaluation.
pub fn verify_selection_event(
    stores: EvaluationStores,
    event: &StoredEvent,
    world: &CompiledWorld,
) -> Result<OperatorSelection, ArenaError> {
    let (evaluation_id, world_id) = selection_event_references(event)?;
    if world_id != world.id() {
        return Err(ArenaError::SelectionWorldMismatch);
    }
    let selection = load_selection(stores, &evaluation_id, world)?;
    let canonical = selection
        .stores
        .events
        .replay_verified()?
        .into_iter()
        .find(|stored| stored.event_id == event.event_id)
        .ok_or(ArenaError::InvalidSelectionEvent)?;
    validate_event_snapshot(event, &canonical)?;
    Ok(selection)
}

fn validate_event_snapshot(
    supplied: &StoredEvent,
    canonical: &StoredEvent,
) -> Result<(), ArenaError> {
    if supplied == canonical {
        Ok(())
    } else {
        Err(ArenaError::InvalidSelectionEvent)
    }
}

fn select(
    stores: EvaluationStores,
    evaluation_id: &str,
    world: &CompiledWorld,
    timestamp_millis: i64,
    append_missing: bool,
) -> Result<OperatorSelection, ArenaError> {
    let operator = load_operator_evaluation(stores, evaluation_id)?;
    let evidence = operator.selection_evidence();
    if evidence.world_id() != world.id() {
        return Err(ArenaError::SelectionWorldMismatch);
    }
    let policy = world.evaluation_policy();
    ensure_supported_confidence(policy.confidence_bps())?;
    let mut stores = operator.into_stores();
    let history = stores.events.replay_verified()?;
    let selection_event_id = format!("arena:selection:{evaluation_id}:selected");
    let source_event = history
        .iter()
        .find(|event| event.event_id == evidence.evaluation_event_id())
        .ok_or_else(|| ArenaError::UnknownEvaluation(evaluation_id.to_owned()))?;
    validate_selection_chronology(
        source_event.sequence,
        history
            .iter()
            .find(|event| event.event_id == selection_event_id)
            .map(|event| event.sequence),
    )?;
    // Recompute with whichever algorithm the existing receipt was produced
    // under, so a stored v1 receipt still verifies byte-for-byte; a brand
    // new selection always uses `CURRENT_ALGORITHM`.
    let algorithm = match history
        .iter()
        .find(|event| event.event_id == selection_event_id)
    {
        Some(event) => existing_receipt_algorithm(&stores, event)?,
        None => CURRENT_ALGORITHM.to_owned(),
    };
    let receipt = analyze(&evidence, world, &algorithm)?;
    if let Some(event) = history
        .iter()
        .find(|event| event.event_id == selection_event_id)
    {
        return rehydrate_selection(stores, event, &receipt);
    }
    if !append_missing {
        return Err(ArenaError::UnknownSelection(evaluation_id.to_owned()));
    }
    let receipt_bytes = serde_json::to_vec(&receipt)?;
    let receipt_id = stores.artifacts.put(&receipt_bytes)?;
    let payload = SelectionEventPayload {
        schema_version: RECEIPT_SCHEMA_VERSION,
        evaluation_id: evaluation_id.to_owned(),
        world_id: evidence.world_id().to_owned(),
        receipt_artifact_id: receipt_id.as_str().to_owned(),
    };
    let payload_bytes = serde_json::to_vec(&payload)?;
    let event = stores.events.append(EventInput::new(
        selection_event_id,
        format!("arena:selection:{evaluation_id}"),
        EVENT_TYPE,
        EVENT_ACTOR,
        timestamp_millis,
        payload_bytes,
    ))?;
    let metadata = event_metadata(&event, receipt_id.as_str());
    Ok(OperatorSelection {
        receipt,
        event: metadata,
        stores,
    })
}

fn rehydrate_selection(
    stores: EvaluationStores,
    event: &StoredEvent,
    expected: &SelectionReceipt,
) -> Result<OperatorSelection, ArenaError> {
    let payload: SelectionEventPayload = serde_json::from_slice(&event.payload)?;
    let canonical_payload = serde_json::to_vec(&payload)?;
    if payload.schema_version != RECEIPT_SCHEMA_VERSION
        || canonical_payload != event.payload
        || payload.evaluation_id != expected.evaluation_id
        || payload.world_id != expected.world_id
        || event.event_id != format!("arena:selection:{}:selected", expected.evaluation_id)
        || event.aggregate_id != format!("arena:selection:{}", expected.evaluation_id)
        || event.event_type != EVENT_TYPE
        || event.actor != EVENT_ACTOR
    {
        return Err(ArenaError::InvalidSelectionEvent);
    }
    let receipt_id = ArtifactId::parse(payload.receipt_artifact_id.clone())?;
    let bytes = stores.artifacts.get(&receipt_id)?;
    let receipt: SelectionReceipt = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&receipt)? != bytes || &receipt != expected {
        return Err(ArenaError::SelectionConflict(
            expected.evaluation_id.clone(),
        ));
    }
    Ok(OperatorSelection {
        receipt,
        event: event_metadata(event, receipt_id.as_str()),
        stores,
    })
}

/// Peeks the algorithm identity of an already-recorded selection receipt so
/// recompute can dispatch to the matching dominance rule. This is an
/// untrusted read: `rehydrate_selection` still recomputes and compares the
/// full canonical receipt bytes before trusting anything read here.
fn existing_receipt_algorithm(
    stores: &EvaluationStores,
    event: &StoredEvent,
) -> Result<String, ArenaError> {
    let payload: SelectionEventPayload = serde_json::from_slice(&event.payload)?;
    let receipt_id = ArtifactId::parse(payload.receipt_artifact_id)?;
    let bytes = stores.artifacts.get(&receipt_id)?;
    let receipt: SelectionReceipt = serde_json::from_slice(&bytes)?;
    Ok(receipt.algorithm)
}

fn event_metadata(event: &StoredEvent, receipt_artifact_id: &str) -> SelectionEvent {
    SelectionEvent {
        sequence: event.sequence,
        event_id: event.event_id.clone(),
        aggregate_id: event.aggregate_id.clone(),
        event_type: event.event_type.clone(),
        actor: event.actor.clone(),
        event_hash: super::encode_hash(event.hash),
        receipt_artifact_id: receipt_artifact_id.to_owned(),
    }
}

fn analyze(
    evidence: &SelectionEvidence,
    world: &CompiledWorld,
    algorithm: &str,
) -> Result<SelectionReceipt, ArenaError> {
    let policy = world.evaluation_policy();
    let histogram = evidence.correctness_outcomes();
    let total =
        usize::try_from(histogram.regressions() + histogram.unchanged() + histogram.improvements())
            .map_err(|_| ArenaError::MetricOverflow("selection task count"))?;
    ensure_minimum_task_count(total)?;
    validate_bootstrap_work(total)?;
    // BTreeMap task ordering is intentionally erased to the canonical histogram order [-1, 0, 1].
    let mut deltas = Vec::with_capacity(total);
    deltas.extend(std::iter::repeat_n(
        -1_i64,
        histogram.regressions() as usize,
    ));
    deltas.extend(std::iter::repeat_n(0_i64, histogram.unchanged() as usize));
    deltas.extend(std::iter::repeat_n(
        1_i64,
        histogram.improvements() as usize,
    ));
    let interval = bootstrap(
        &deltas,
        evidence.seed(),
        usize::from(policy.confidence_bps()),
    )?;
    let parent = evidence.parent_fitness();
    let candidate = evidence.candidate_fitness();
    let parent_correctness_bps = ratio_bps(parent.correct_trials(), parent.total_trials())?;
    let candidate_correctness_bps =
        ratio_bps(candidate.correct_trials(), candidate.total_trials())?;
    let parent_reliability_bps = ratio_bps(parent.reliable_trials(), parent.total_trials())?;
    let candidate_reliability_bps =
        ratio_bps(candidate.reliable_trials(), candidate.total_trials())?;
    let parent_metrics = [
        u64::from(parent_correctness_bps),
        u64::from(parent_reliability_bps),
        parent.total_cost_microusd(),
        parent.total_latency_millis(),
    ];
    let candidate_metrics = [
        u64::from(candidate_correctness_bps),
        u64::from(candidate_reliability_bps),
        candidate.total_cost_microusd(),
        candidate.total_latency_millis(),
    ];
    let dominates = match algorithm {
        ALGORITHM_V1 => pareto_dominates_v1(parent_metrics, candidate_metrics),
        ALGORITHM_V2 => pareto_dominates_v2(
            parent_metrics,
            candidate_metrics,
            latency_tolerance_millis(
                parent.total_latency_millis(),
                u64::from(histogram.regressions())
                    + u64::from(histogram.unchanged())
                    + u64::from(histogram.improvements()),
            ),
        ),
        _ => return Err(ArenaError::InvalidStoredReceipt("selection algorithm")),
    };
    let metrics_eligible = metrics_eligible(
        interval.lower,
        policy.minimum_delta_bps(),
        dominates,
        candidate.total_cost_microusd(),
        policy.maximum_cost_microusd(),
    );
    Ok(SelectionReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        algorithm: algorithm.to_owned(),
        resamples: u32::try_from(RESAMPLES)
            .map_err(|_| ArenaError::MetricOverflow("bootstrap resamples"))?,
        seed: evidence.seed(),
        evaluation_id: evidence.evaluation_id().to_owned(),
        evaluation_event_id: evidence.evaluation_event_id().to_owned(),
        evaluation_event_hash: evidence.evaluation_event_hash().to_owned(),
        world_id: evidence.world_id().to_owned(),
        parent_genome_id: evidence.parent_genome_id().to_owned(),
        candidate_genome_id: evidence.candidate_genome_id().to_owned(),
        maximum_cost_microusd: policy.maximum_cost_microusd(),
        minimum_delta_bps: policy.minimum_delta_bps(),
        maximum_regressions: policy.maximum_regressions(),
        confidence_bps: policy.confidence_bps(),
        correctness_regressions: histogram.regressions(),
        correctness_unchanged: histogram.unchanged(),
        correctness_improvements: histogram.improvements(),
        estimate_bps: interval.estimate,
        lower_bps: interval.lower,
        upper_bps: interval.upper,
        parent_correctness_bps,
        candidate_correctness_bps,
        parent_reliability_bps,
        candidate_reliability_bps,
        parent_cost_microusd: parent.total_cost_microusd(),
        candidate_cost_microusd: candidate.total_cost_microusd(),
        parent_latency_millis: parent.total_latency_millis(),
        candidate_latency_millis: candidate.total_latency_millis(),
        candidate_pareto_dominates: dominates,
        metrics_eligible,
        invariant_gate_verified: false,
        promotion_eligible: false,
    })
}

#[derive(Clone, Copy)]
struct Interval {
    estimate: i64,
    lower: i64,
    upper: i64,
}

fn bootstrap(deltas: &[i64], seed: u64, confidence_bps: usize) -> Result<Interval, ArenaError> {
    let count = i64::try_from(deltas.len()).map_err(|_| ArenaError::BootstrapWorkExceeded)?;
    let estimate_sum = deltas
        .iter()
        .try_fold(0_i64, |sum, value| sum.checked_add(*value))
        .ok_or(ArenaError::MetricOverflow("bootstrap estimate"))?;
    let estimate_bps = estimate_sum
        .checked_mul(10_000)
        .ok_or(ArenaError::MetricOverflow("bootstrap estimate"))?
        .div_euclid(count);
    let mut random = SplitMix64(seed);
    let mut distribution = Vec::with_capacity(RESAMPLES);
    for _ in 0..RESAMPLES {
        let mut sum = 0_i64;
        for _ in 0..deltas.len() {
            let index =
                usize::try_from(random.next() % deltas.len() as u64).expect("bounded index");
            sum = sum
                .checked_add(deltas[index])
                .ok_or(ArenaError::MetricOverflow("bootstrap sample"))?;
        }
        let value = sum
            .checked_mul(10_000)
            .ok_or(ArenaError::MetricOverflow("bootstrap sample"))?
            .div_euclid(count);
        distribution.push(value);
    }
    distribution.sort_unstable();
    let tail = (10_000 - confidence_bps) / 2;
    let lower_index = tail * (RESAMPLES - 1) / 10_000;
    let upper_index = (10_000 - tail) * (RESAMPLES - 1) / 10_000;
    Ok(Interval {
        estimate: estimate_bps,
        lower: distribution[lower_index],
        upper: distribution[upper_index],
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

fn ratio_bps(numerator: u32, denominator: u32) -> Result<u32, ArenaError> {
    if denominator == 0 {
        return Err(ArenaError::InvalidStoredReceipt("selection trials"));
    }
    let scaled = u64::from(numerator) * 10_000;
    u32::try_from(scaled / u64::from(denominator))
        .map_err(|_| ArenaError::MetricOverflow("fitness basis points"))
}

fn ensure_minimum_task_count(total: usize) -> Result<(), ArenaError> {
    if total < 2 {
        Err(ArenaError::BootstrapWorkExceeded)
    } else {
        Ok(())
    }
}

fn validate_selection_chronology(
    source_sequence: u64,
    selection_sequence: Option<u64>,
) -> Result<(), ArenaError> {
    if source_sequence >= selection_sequence.unwrap_or(u64::MAX) {
        Err(ArenaError::InvalidStoredReceipt("selection chronology"))
    } else {
        Ok(())
    }
}

fn metrics_eligible(
    lower_bps: i64,
    minimum_delta_bps: i64,
    candidate_pareto_dominates: bool,
    candidate_cost_microusd: u64,
    world_cost_ceiling_microusd: u64,
) -> bool {
    lower_bps > minimum_delta_bps
        && candidate_pareto_dominates
        && candidate_cost_microusd <= world_cost_ceiling_microusd
}

/// `ALGORITHM_V1` dominance rule: the candidate must be no worse than the
/// parent on every raw measured dimension, including wall-clock latency, and
/// strictly better on at least one. Preserved only for recomputing receipts
/// recorded before `ALGORITHM_V2`.
fn pareto_dominates_v1(parent: [u64; 4], candidate: [u64; 4]) -> bool {
    let no_worse = candidate[0] >= parent[0]
        && candidate[1] >= parent[1]
        && candidate[2] <= parent[2]
        && candidate[3] <= parent[3];
    let better = candidate[0] > parent[0]
        || candidate[1] > parent[1]
        || candidate[2] < parent[2]
        || candidate[3] < parent[3];
    no_worse && better
}

/// The fixed noise floor a candidate's total latency may exceed the
/// parent's by before it counts as a real regression: whichever is larger of
/// 10% of the parent's total latency, or 50ms per paired task. Small
/// reference-task latencies are a few milliseconds and dominated by
/// scheduling jitter, so a fixed absolute floor (`50ms * tasks`) matters as
/// much as the proportional one for cheap, fast Worlds.
fn latency_tolerance_millis(parent_latency_millis: u64, task_count: u64) -> u64 {
    (parent_latency_millis / 10).max(task_count.saturating_mul(50))
}

/// `ALGORITHM_V2` dominance rule: identical to `ALGORITHM_V1` on
/// correctness, reliability, and cost, but latency only counts as a
/// regression once it exceeds the parent's by more than
/// `latency_tolerance_millis`. This stops a strictly-better candidate from
/// being randomly rejected because it happened to run a millisecond or two
/// slower on a trivial task.
fn pareto_dominates_v2(
    parent: [u64; 4],
    candidate: [u64; 4],
    latency_tolerance_millis: u64,
) -> bool {
    let latency_no_worse = candidate[3] <= parent[3].saturating_add(latency_tolerance_millis);
    let no_worse = candidate[0] >= parent[0]
        && candidate[1] >= parent[1]
        && candidate[2] <= parent[2]
        && latency_no_worse;
    let better = candidate[0] > parent[0]
        || candidate[1] > parent[1]
        || candidate[2] < parent[2]
        || candidate[3] < parent[3];
    no_worse && better
}

fn ensure_supported_confidence(confidence_bps: u16) -> Result<(), ArenaError> {
    if confidence_bps == 10_000 {
        Err(ArenaError::UnsupportedSelectionConfidence(confidence_bps))
    } else {
        Ok(())
    }
}

fn validate_bootstrap_work(task_count: usize) -> Result<(), ArenaError> {
    let draws = task_count
        .checked_mul(RESAMPLES)
        .ok_or(ArenaError::MetricOverflow("bootstrap work"))?;
    if draws > MAX_BOOTSTRAP_DRAWS {
        Err(ArenaError::BootstrapWorkExceeded)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SplitMix64, bootstrap, ensure_minimum_task_count, ensure_supported_confidence,
        latency_tolerance_millis, metrics_eligible, pareto_dominates_v1, pareto_dominates_v2,
        ratio_bps, validate_bootstrap_work, validate_event_snapshot, validate_selection_chronology,
    };
    use crate::ArenaError;
    use hephaestus_ledger::StoredEvent;

    #[test]
    fn histogram_bootstrap_matches_python_floor_rounding_for_negative_deltas() {
        let interval = bootstrap(&[-1, -1, 0], 42, 9_500).unwrap();
        assert_eq!(
            (interval.estimate, interval.lower, interval.upper),
            (-6_667, -10_000, 0)
        );
    }

    #[test]
    fn negative_resampled_mean_uses_floor_division_in_the_confidence_tail() {
        // The deterministic 99.5% lower tail lands on a resample with four
        // negative deltas: -40_000 / 6 must floor to -6_667, not truncate to
        // -6_666 as Rust's signed `/` would.
        let interval = bootstrap(&[-1, 0, 0, 0, 0, 0], 42, 9_950).unwrap();
        assert_eq!(
            (interval.estimate, interval.lower, interval.upper),
            (-1_667, -6_667, 0)
        );
    }

    #[test]
    fn bootstrap_uses_canonical_histogram_order_for_fixed_seed() {
        let interval = bootstrap(&[-1, 0, 1], 0, 9_500).unwrap();
        assert_eq!(
            (interval.estimate, interval.lower, interval.upper),
            (0, -10_000, 10_000)
        );
    }

    #[test]
    fn bootstrap_matches_seed_sensitive_histogram_reference() {
        let deltas = [-1, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        for (seed, upper) in [(0, 8_333), (7, 8_888), (42, 8_888), (123_456_789, 8_333)] {
            let interval = bootstrap(&deltas, seed, 9_500).unwrap();
            assert_eq!(
                (interval.estimate, interval.lower, interval.upper),
                (6_111, 3_333, upper)
            );
        }
    }

    #[test]
    fn splitmix64_matches_python_reference() {
        let mut random = SplitMix64(7);
        assert_eq!(random.next(), 7_191_089_600_892_374_487);
    }

    #[test]
    fn metrics_gate_requires_strict_effect_and_enforces_world_cost_ceiling() {
        assert!(!metrics_eligible(10, 10, true, 100, 100));
        assert!(metrics_eligible(11, 10, true, 100, 100));
        assert!(!metrics_eligible(11, 10, true, 101, 100));
        assert!(!metrics_eligible(11, 10, false, 100, 100));
    }

    #[test]
    fn selection_validation_edges_fail_closed() {
        assert!(ensure_minimum_task_count(2).is_ok());
        assert!(matches!(
            ensure_minimum_task_count(1),
            Err(ArenaError::BootstrapWorkExceeded)
        ));
        assert!(validate_selection_chronology(3, Some(4)).is_ok());
        assert!(validate_selection_chronology(3, None).is_ok());
        assert!(matches!(
            validate_selection_chronology(4, Some(3)),
            Err(ArenaError::InvalidStoredReceipt("selection chronology"))
        ));
        assert_eq!(ratio_bps(1, 2).unwrap(), 5_000);
        assert!(matches!(
            ratio_bps(0, 0),
            Err(ArenaError::InvalidStoredReceipt("selection trials"))
        ));
    }

    #[test]
    fn selection_event_references_reject_noncanonical_envelopes() {
        let payload = br#"{"schema_version":1,"evaluation_id":"eval-1","world_id":"world-1","receipt_artifact_id":"not-parsed-here"}"#;
        let canonical = StoredEvent {
            sequence: 1,
            event_id: "arena:selection:eval-1:selected".to_owned(),
            aggregate_id: "arena:selection:eval-1".to_owned(),
            event_type: "selection.recorded".to_owned(),
            actor: "arena-plane".to_owned(),
            timestamp_millis: 1,
            payload: payload.to_vec(),
            previous_hash: [0; 32],
            hash: [0; 32],
        };
        assert_eq!(
            super::selection_event_references(&canonical).unwrap(),
            ("eval-1".to_owned(), "world-1".to_owned())
        );
        let mut cases = Vec::new();
        let mut event = canonical.clone();
        event.payload.push(b' ');
        cases.push(event);
        let mut event = canonical.clone();
        event.actor = "untrusted".to_owned();
        cases.push(event);
        let mut event = canonical.clone();
        event.event_type = "other".to_owned();
        cases.push(event);
        let mut event = canonical.clone();
        event.event_id = "other".to_owned();
        cases.push(event);
        let mut event = canonical.clone();
        event.aggregate_id = "other".to_owned();
        cases.push(event);
        for event in cases {
            assert!(matches!(
                super::selection_event_references(&event),
                Err(ArenaError::InvalidSelectionEvent)
            ));
        }
    }

    #[test]
    fn pareto_v1_gate_rejects_regression_in_each_independent_metric() {
        let parent = [100, 100, 100, 100];
        assert!(pareto_dominates_v1(parent, [101, 100, 100, 100]));
        assert!(!pareto_dominates_v1(parent, [99, 101, 99, 99]));
        assert!(!pareto_dominates_v1(parent, [101, 99, 99, 99]));
        assert!(!pareto_dominates_v1(parent, [101, 101, 101, 99]));
        assert!(!pareto_dominates_v1(parent, [101, 101, 99, 101]));
        assert!(pareto_dominates_v1(parent, [101, 101, 99, 99]));
        assert!(!pareto_dominates_v1(parent, parent));
    }

    #[test]
    fn latency_tolerance_uses_the_larger_of_the_proportional_or_fixed_floor() {
        // 10% of 1000ms (100ms) beats the fixed 50ms * 2 tasks (100ms) — tie goes either way, both 100.
        assert_eq!(latency_tolerance_millis(1_000, 2), 100);
        // Cheap, fast World: 10% of 10ms (1ms) loses to the fixed floor, 50ms * 4 tasks = 200ms.
        assert_eq!(latency_tolerance_millis(10, 4), 200);
        // Large World: 10% of 100_000ms (10_000ms) dominates the fixed floor.
        assert_eq!(latency_tolerance_millis(100_000, 3), 10_000);
    }

    #[test]
    fn pareto_v2_gate_tolerates_latency_noise_but_still_rejects_a_real_regression() {
        let parent = [100, 100, 100, 100];
        // Tolerance is max(10, 50*1) = 50. A 1ms slower candidate that is
        // strictly better on correctness is still eligible: this is the bug fix.
        assert!(pareto_dominates_v2(parent, [101, 100, 100, 101], 50));
        // Exactly at the tolerance boundary still passes.
        assert!(pareto_dominates_v2(parent, [101, 100, 100, 150], 50));
        // One millisecond past the tolerance is a real regression and fails closed.
        assert!(!pareto_dominates_v2(parent, [101, 100, 100, 151], 50));
        // Correctness, reliability, and cost regressions are untouched by the tolerance.
        assert!(!pareto_dominates_v2(parent, [99, 101, 99, 99], 50));
        assert!(!pareto_dominates_v2(parent, [101, 99, 99, 99], 50));
        assert!(!pareto_dominates_v2(parent, [101, 101, 101, 99], 50));
        assert!(!pareto_dominates_v2(parent, parent, 50));
    }

    #[test]
    fn event_verification_compares_the_complete_canonical_snapshot() {
        let canonical = StoredEvent {
            sequence: 4,
            event_id: "arena:selection:e1:selected".to_owned(),
            aggregate_id: "arena:selection:e1".to_owned(),
            event_type: "selection.recorded".to_owned(),
            actor: "arena-plane".to_owned(),
            timestamp_millis: 99,
            payload: b"canonical".to_vec(),
            previous_hash: [1; 32],
            hash: [2; 32],
        };
        assert!(validate_event_snapshot(&canonical, &canonical).is_ok());
        let mutations: [fn(&mut StoredEvent); 5] = [
            |event| event.sequence += 1,
            |event| event.timestamp_millis += 1,
            |event| event.payload.push(b'!'),
            |event| event.previous_hash[0] ^= 1,
            |event| event.hash[0] ^= 1,
        ];
        for mutate in mutations {
            let mut forged = canonical.clone();
            mutate(&mut forged);
            assert!(matches!(
                validate_event_snapshot(&forged, &canonical),
                Err(ArenaError::InvalidSelectionEvent)
            ));
        }
    }

    #[test]
    fn confidence_endpoint_and_bootstrap_work_fail_closed() {
        assert!(ensure_supported_confidence(9_999).is_ok());
        assert!(matches!(
            ensure_supported_confidence(10_000),
            Err(ArenaError::UnsupportedSelectionConfidence(10_000))
        ));
        assert!(validate_bootstrap_work(2_000).is_ok());
        assert!(matches!(
            validate_bootstrap_work(2_001),
            Err(ArenaError::BootstrapWorkExceeded)
        ));
    }
}
