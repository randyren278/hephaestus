//! Deterministic failure-cluster analysis for one candidate's failed trials.
//!
//! Groups a candidate's failed trials from a verified, paired Arena evaluation
//! into stable clusters by observable failure signature: completion reason
//! (a non-`Success` terminal outcome that is not budget exhaustion), budget
//! exhaustion (wall-clock or output-byte), and, on VISIBLE tasks only, the
//! shape relation between the candidate's actual output and the task's
//! expected output. Sealed tasks contribute only aggregate counts by
//! completion reason; their inputs, expected outputs, and outputs are never
//! read for shape analysis and never leave this module.
//!
//! Each cluster carries an explicit, deterministic hypothesis string and, when
//! one exists, the single supported mutation that would address it. Today the
//! only supported mutation is the Forge reference-operation flip
//! (`identity` <-> `ascii_uppercase`); every other cluster reports
//! `suggested_mutation: None` rather than guessing.
//!
//! The orchestration half of this module mirrors `invariants.rs`: it
//! rehydrates authenticated evidence, recomputes a canonical receipt, and
//! records or verifies one idempotent `forge.clustered` ledger event.

use std::collections::BTreeMap;

use hephaestus_experience::RunCompletionReason;
use hephaestus_genome::CompiledWorld;
use hephaestus_ledger::{ArtifactId, EventIndex, EventInput, StoredEvent};
use hephaestus_runtime::{mutation_casing_flip, mutation_family_fix_for};
use serde::{Deserialize, Serialize};

use crate::invariants::verified_submission_outputs;
use crate::{
    ArenaError, ArtifactStore, EvaluationStores, OperatorReceipt, TrustedManifest, Visibility,
    load_operator_evaluation, load_operator_evaluation_in,
};

const RECEIPT_SCHEMA_VERSION: u16 = 1;
/// The historical clustering algorithm (roadmap items 8, 10, 13): every
/// cluster's suggestion depends only on its own shape signature, and the
/// only suggestion it can ever make is the casing flip on a
/// `shape_case_mismatch` cluster. A `ClusterAnalysis` recomputed with this
/// algorithm is byte-for-byte identical to what this module produced before
/// `failure-cluster-v2` existed, so every previously recorded receipt still
/// replays exactly.
const ALGORITHM_V1: &str = "failure-cluster-v1";
/// The current clustering algorithm: every new analysis is computed with
/// this algorithm. Its suggestions additionally depend on the analyzed
/// candidate's own current reference operation (`candidate_operation`): a
/// Gauntlet "bad" operation's failures all suggest that family's fix
/// regardless of shape signature; the casing pair behaves exactly like v1;
/// a Gauntlet "fix" operation's failures never suggest a regression back to
/// its "bad" pair. See [`describe_v2`].
const ALGORITHM_V2: &str = "failure-cluster-v2";
const EVENT_TYPE: &str = "forge.clustered";
const EVENT_ACTOR: &str = "arena-plane";
const EVENT_ID_PREFIX: &str = "forge:analysis:";

/// Which clustering algorithm produced (or should reproduce) one
/// [`ClusterAnalysis`]. Replay dispatches on the algorithm string already
/// recorded in history; a brand-new analysis (no matching event yet) is
/// always computed with [`Self::V2`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClusterAlgorithm {
    V1,
    V2,
}

impl ClusterAlgorithm {
    const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => ALGORITHM_V1,
            Self::V2 => ALGORITHM_V2,
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            ALGORITHM_V1 => Some(Self::V1),
            ALGORITHM_V2 => Some(Self::V2),
            _ => None,
        }
    }
}

/// One supported, minimal mutation a cluster may recommend.
///
/// This never executes anything; it only names the mutation an operator (or
/// an operator-directed `genome propose --analysis ... --cluster ...` call)
/// may apply through the ordinary Forge proposal path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedMutation {
    /// Flip the parent's single supported reference-prompt operation.
    /// `failure-cluster-v1` only, kept exactly as-is so an already-recorded
    /// v1 receipt still replays byte-for-byte; `failure-cluster-v2` always
    /// uses [`Self::ReferenceOperation`] instead, even for the casing pair.
    ReferenceOperationFlip,
    /// Propose the named reference operation as the parent's next mutation
    /// target. `failure-cluster-v2` only.
    ReferenceOperation {
        /// One of the 16 reference-runtime operation names
        /// (`crates/hephaestus-runtime/src/mutation_catalog.rs`).
        operation_after: String,
    },
}

/// One deterministic cluster of the candidate's failed trials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FailureCluster {
    /// Stable, sorted cluster signature key.
    pub signature: String,
    /// Failed visible trials contributing to this cluster.
    pub visible_count: u32,
    /// Failed sealed trials contributing to this cluster (aggregate only).
    pub sealed_count: u32,
    /// `visible_count + sealed_count`.
    pub total_count: u32,
    /// Explicit, deterministic hypothesis text for this cluster.
    pub hypothesis: String,
    /// The single minimal supported mutation, when one exists.
    pub suggested_mutation: Option<SuggestedMutation>,
}

/// Canonical aggregate receipt recomputed from authenticated candidate trial evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterAnalysis {
    /// Receipt schema version.
    pub schema_version: u16,
    /// Deterministic clustering algorithm.
    pub algorithm: String,
    /// Operator-chosen idempotency key for this analysis.
    pub analysis_id: String,
    /// Stable source evaluation identity.
    pub evaluation_id: String,
    /// Exact source evaluation event identity and hash.
    pub evaluation_event_id: String,
    pub evaluation_event_hash: String,
    /// Immutable World identity bound to the source evaluation.
    pub world_id: String,
    /// Parent Genome identity in the source paired evaluation.
    pub parent_genome_id: String,
    /// Candidate Genome identity whose failed trials were clustered.
    pub candidate_genome_id: String,
    /// The candidate's own `agent.prompt` reference operation at analysis
    /// time, supplied by the control plane (which alone can resolve a
    /// Genome's registered artifacts) rather than derived here. Drives
    /// `failure-cluster-v2`'s per-operation suggestion rule; absent for a
    /// `failure-cluster-v1` analysis, whose suggestions never depended on
    /// it. This field was added after `schema_version` 1 shipped; it
    /// defaults to `None` (and is omitted from canonical bytes when absent)
    /// so a `failure-cluster-v1` receipt recomputes byte-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_operation: Option<String>,
    /// Number of failed visible trials.
    pub total_visible_failed_trials: u32,
    /// Number of failed sealed trials (aggregate only).
    pub total_sealed_failed_trials: u32,
    /// Clusters in stable signature order; only non-empty clusters are present.
    pub clusters: Vec<FailureCluster>,
}

/// Canonical metadata for one durable cluster-analysis ledger event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterEvent {
    /// Canonical global event sequence.
    pub sequence: u64,
    /// Deterministic event identity.
    pub event_id: String,
    /// Analysis-bound aggregate identity.
    pub aggregate_id: String,
    /// Durable event type.
    pub event_type: String,
    /// Fixed trusted writer identity.
    pub actor: String,
    /// Event-chain hash in canonical lowercase hex.
    pub event_hash: String,
    /// Content address of canonical `ClusterAnalysis` bytes.
    pub analysis_artifact_id: String,
}

/// Operator-side trusted cluster analysis retaining evaluator store capability.
pub struct OperatorClusterAnalysis {
    analysis: ClusterAnalysis,
    event: ClusterEvent,
    stores: EvaluationStores,
}

impl OperatorClusterAnalysis {
    /// Canonical cluster analysis without task identities or raw outputs.
    #[must_use]
    pub const fn analysis(&self) -> &ClusterAnalysis {
        &self.analysis
    }

    /// Canonical event metadata binding the analysis into the ledger.
    #[must_use]
    pub const fn event(&self) -> &ClusterEvent {
        &self.event
    }

    /// Returns the evaluator-owned stores for the next trusted operation.
    #[must_use]
    pub fn into_stores(self) -> EvaluationStores {
        self.stores
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ClusterEventPayload {
    schema_version: u16,
    analysis_id: String,
    evaluation_id: String,
    world_id: String,
    analysis_artifact_id: String,
}

/// One authenticated visible trial's classification-relevant evidence. Only
/// visible-task content ever reaches this type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisibleTrial {
    /// The candidate's authenticated terminal completion reason.
    pub completion_reason: RunCompletionReason,
    /// The task's expected output, from the visible manifest.
    pub expected_output: String,
    /// The candidate's authenticated stdout bytes.
    pub actual_output: Vec<u8>,
}

/// One sealed trial reduced to what clustering may see: its completion
/// reason and whether its output matched, never the content itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SealedTrial {
    /// The candidate's authenticated terminal outcome.
    pub completion_reason: RunCompletionReason,
    /// Whether the authenticated output equalled the sealed expected output.
    pub output_matches: bool,
}

/// Groups a candidate's failed trials into deterministic clusters using the
/// historical `failure-cluster-v1` algorithm: every suggestion depends only
/// on the cluster's own shape signature. `current_operation` is accepted for
/// call-site symmetry with [`cluster_trials_v2`] but is never read.
///
/// `visible` carries per-trial content for visible tasks only. `sealed`
/// carries only a completion reason and a match flag per sealed task; sealed
/// inputs, expected outputs, and outputs are structurally absent from this
/// signature and cannot leak through it.
#[cfg(test)]
#[must_use]
fn cluster_trials(visible: &[VisibleTrial], sealed: &[SealedTrial]) -> Vec<FailureCluster> {
    cluster_trials_for(ClusterAlgorithm::V1, visible, sealed, None)
}

/// Groups a candidate's failed trials into deterministic clusters using the
/// current `failure-cluster-v2` algorithm. See [`describe_v2`] for the
/// per-operation suggestion rule driven by `current_operation`.
#[must_use]
pub fn cluster_trials_v2(
    visible: &[VisibleTrial],
    sealed: &[SealedTrial],
    current_operation: Option<&str>,
) -> Vec<FailureCluster> {
    cluster_trials_for(ClusterAlgorithm::V2, visible, sealed, current_operation)
}

fn cluster_trials_for(
    algorithm: ClusterAlgorithm,
    visible: &[VisibleTrial],
    sealed: &[SealedTrial],
    current_operation: Option<&str>,
) -> Vec<FailureCluster> {
    let mut clusters: BTreeMap<&'static str, (u32, u32)> = BTreeMap::new();
    let mut bump = |key: &'static str, is_sealed: bool| {
        let entry = clusters.entry(key).or_insert((0, 0));
        if is_sealed {
            entry.1 = entry.1.saturating_add(1);
        } else {
            entry.0 = entry.0.saturating_add(1);
        }
    };

    for trial in visible {
        match completion_signature(trial.completion_reason) {
            Some(key) => bump(key, false),
            None => bump(
                shape_signature(&trial.expected_output, &trial.actual_output),
                false,
            ),
        }
    }
    for trial in sealed {
        if let Some(key) = sealed_failure_signature(*trial) {
            bump(key, true);
        }
    }

    clusters
        .into_iter()
        .map(|(signature, (visible_count, sealed_count))| {
            let total_count = visible_count.saturating_add(sealed_count);
            let (hypothesis, suggested_mutation) = match algorithm {
                ClusterAlgorithm::V1 => {
                    describe(signature, visible_count, sealed_count, total_count)
                }
                ClusterAlgorithm::V2 => describe_v2(
                    signature,
                    visible_count,
                    sealed_count,
                    total_count,
                    current_operation,
                ),
            };
            FailureCluster {
                signature: signature.to_owned(),
                visible_count,
                sealed_count,
                total_count,
                hypothesis,
                suggested_mutation,
            }
        })
        .collect()
}

/// A sealed trial fails on a non-success outcome or on a successful run whose
/// output did not match; only a correct successful run contributes nothing.
fn sealed_failure_signature(trial: SealedTrial) -> Option<&'static str> {
    completion_signature(trial.completion_reason)
        .or((!trial.output_matches).then_some("sealed_incorrect_output"))
}

/// Returns the completion-reason cluster key for a non-success outcome, or
/// `None` when the outcome is `Success` (handled by shape analysis instead).
fn completion_signature(reason: RunCompletionReason) -> Option<&'static str> {
    match reason {
        RunCompletionReason::Success => None,
        RunCompletionReason::ProviderFailure => Some("completion_provider_failure"),
        RunCompletionReason::OperatorInterrupt => Some("completion_operator_interrupt"),
        RunCompletionReason::IoFailure => Some("completion_io_failure"),
        RunCompletionReason::WallBudgetExceeded => Some("budget_wall_exceeded"),
        RunCompletionReason::OutputBudgetExceeded => Some("budget_output_exceeded"),
    }
}

/// Classifies the shape relation between a successfully-completed candidate
/// output and its expected output on one visible task. Only reached for
/// `RunCompletionReason::Success` trials whose output does not equal the
/// expected output; an exact match is not a failure and is never classified.
fn shape_signature(expected: &str, actual: &[u8]) -> &'static str {
    let Ok(actual) = std::str::from_utf8(actual) else {
        return "shape_other";
    };
    if actual == expected {
        // Only reachable if a caller passes a trial that is not actually a
        // failure; treat it conservatively as "other" rather than panic.
        return "shape_other";
    }
    if actual.is_empty() {
        return "shape_empty_output";
    }
    if actual.eq_ignore_ascii_case(expected) {
        return "shape_case_mismatch";
    }
    if actual.split_whitespace().eq(expected.split_whitespace()) {
        return "shape_whitespace";
    }
    if expected.starts_with(actual) && actual.len() < expected.len() {
        return "shape_truncation";
    }
    "shape_other"
}

fn describe(
    signature: &'static str,
    visible_count: u32,
    sealed_count: u32,
    total_count: u32,
) -> (String, Option<SuggestedMutation>) {
    match signature {
        "completion_provider_failure" => (
            format!(
                "The candidate reported a provider failure on {total_count} task(s) \
                 ({visible_count} visible, {sealed_count} sealed); no supported mutation \
                 targets provider errors."
            ),
            None,
        ),
        "completion_operator_interrupt" => (
            format!(
                "The candidate's execution was interrupted on {total_count} task(s) \
                 ({visible_count} visible, {sealed_count} sealed); no supported mutation \
                 targets interrupted execution."
            ),
            None,
        ),
        "completion_io_failure" => (
            format!(
                "The candidate hit an I/O failure delivering input or output on {total_count} \
                 task(s) ({visible_count} visible, {sealed_count} sealed); no supported mutation \
                 targets I/O failures."
            ),
            None,
        ),
        "budget_wall_exceeded" => (
            format!(
                "The candidate exhausted its wall-clock budget on {total_count} task(s) \
                 ({visible_count} visible, {sealed_count} sealed); no supported mutation \
                 changes the runtime budget."
            ),
            None,
        ),
        "budget_output_exceeded" => (
            format!(
                "The candidate exceeded its output-byte budget on {total_count} task(s) \
                 ({visible_count} visible, {sealed_count} sealed); no supported mutation \
                 changes the runtime budget."
            ),
            None,
        ),
        "sealed_incorrect_output" => (
            format!(
                "The candidate completed {sealed_count} sealed task(s) with incorrect output; \
                 sealed content is not inspected, so no supported mutation is suggested."
            ),
            None,
        ),
        "shape_case_mismatch" => (
            format!(
                "The candidate's output differs from the expected output only in letter \
                 case on {visible_count} visible task(s); flipping the parent's reference \
                 operation should correct the case mismatch."
            ),
            Some(SuggestedMutation::ReferenceOperationFlip),
        ),
        "shape_whitespace" => (
            format!(
                "The candidate's output differs from the expected output only in \
                 whitespace on {visible_count} visible task(s); no supported mutation \
                 targets whitespace differences."
            ),
            None,
        ),
        "shape_empty_output" => (
            format!(
                "The candidate produced no output on {visible_count} visible task(s); no \
                 supported mutation targets empty output."
            ),
            None,
        ),
        "shape_truncation" => (
            format!(
                "The candidate's output appears truncated relative to the expected output \
                 on {visible_count} visible task(s); no supported mutation targets \
                 truncation."
            ),
            None,
        ),
        _ => (
            format!(
                "The candidate's output does not match the expected output for another \
                 reason on {visible_count} visible task(s); no supported mutation applies."
            ),
            None,
        ),
    }
}

/// `failure-cluster-v2`'s deterministic per-operation suggestion rule
/// (roadmap items 8, 10, 13). Unlike v1, the suggestion depends primarily on
/// `current_operation` — the analyzed candidate's own reference operation at
/// analysis time — not on the cluster's shape signature, because most
/// Gauntlet bad/fix pairs produce completely different output shapes (not a
/// case/whitespace/truncation variant of each other):
///
/// - If `current_operation` is a Gauntlet family's "bad" operation, **every**
///   failure cluster (any signature) suggests that family's "fix" operation:
///   the candidate is already known to be running the wrong transform, so
///   any failure at all is explained by it and corrected by switching to the
///   fix.
/// - If `current_operation` is one of the two casing operations
///   (`identity`/`ascii_uppercase`), behavior matches v1 exactly: a
///   `shape_case_mismatch` cluster suggests the casing flip; every other
///   shape or completion cluster suggests nothing.
/// - If `current_operation` is a Gauntlet family's "fix" operation, no
///   cluster ever suggests a mutation (never propose a regression back to
///   the paired "bad" operation).
/// - If `current_operation` is absent or unrecognized, no cluster suggests a
///   mutation (a safe default: no derivable operation to build a target
///   from).
fn describe_v2(
    signature: &'static str,
    visible_count: u32,
    sealed_count: u32,
    total_count: u32,
    current_operation: Option<&str>,
) -> (String, Option<SuggestedMutation>) {
    if let Some(current) = current_operation {
        if let Some(fix) = mutation_family_fix_for(current) {
            return (
                format!(
                    "The candidate's current reference operation ({current}) is a known \
                     Gauntlet regression; this failure cluster ({total_count} task(s), \
                     {visible_count} visible, {sealed_count} sealed) is explained by it, so \
                     switching to its paired fix ({fix}) should correct it."
                ),
                Some(SuggestedMutation::ReferenceOperation {
                    operation_after: fix.to_owned(),
                }),
            );
        }
        if let Some(other) = mutation_casing_flip(current) {
            return if signature == "shape_case_mismatch" {
                (
                    format!(
                        "The candidate's output differs from the expected output only in \
                         letter case on {visible_count} visible task(s); flipping the parent's \
                         reference operation to {other} should correct the case mismatch."
                    ),
                    Some(SuggestedMutation::ReferenceOperation {
                        operation_after: other.to_owned(),
                    }),
                )
            } else {
                let (hypothesis, _) = describe(signature, visible_count, sealed_count, total_count);
                (hypothesis, None)
            };
        }
        // A Gauntlet family's "fix" operation: never propose a regression.
        return (
            format!(
                "The candidate's current reference operation ({current}) is already a known \
                 Gauntlet fix; this failure cluster ({total_count} task(s)) does not suggest \
                 regressing back to its paired bad operation."
            ),
            None,
        );
    }
    let (hypothesis, _) = describe(signature, visible_count, sealed_count, total_count);
    (hypothesis, None)
}

/// Checks authenticated candidate evidence and durably records one
/// deterministic cluster-analysis receipt. An exact retry with the same
/// `analysis_id` and `evaluation_id` recomputes and returns the prior event
/// and analysis.
///
/// `current_operation` is the analyzed candidate's own `agent.prompt`
/// reference operation at analysis time, resolved by the caller (this crate
/// has no Genome-registry capability of its own) and recorded verbatim as
/// `ClusterAnalysis.candidate_operation`; it drives `failure-cluster-v2`'s
/// per-operation suggestion rule (see [`SuggestedMutation`]). Pass `None`
/// when it cannot be resolved; every cluster then suggests nothing.
///
/// # Errors
///
/// Rejects an unknown evaluation, unverified run evidence, a World mismatch,
/// or a conflicting deterministic retry (same `analysis_id`, different
/// recomputed content).
pub fn check_failure_clusters(
    stores: EvaluationStores,
    analysis_id: &str,
    evaluation_id: &str,
    world: &CompiledWorld,
    current_operation: Option<&str>,
    timestamp_millis: i64,
) -> Result<OperatorClusterAnalysis, ArenaError> {
    check(
        stores,
        analysis_id,
        evaluation_id,
        world,
        current_operation,
        timestamp_millis,
        true,
    )
}

/// Recomputes an existing cluster analysis without appending an event.
///
/// # Errors
///
/// Fails when the analysis is absent, noncanonical, tampered, or inconsistent
/// with authenticated source evaluation evidence.
pub fn load_failure_clusters(
    stores: EvaluationStores,
    analysis_id: &str,
    evaluation_id: &str,
    world: &CompiledWorld,
    current_operation: Option<&str>,
) -> Result<OperatorClusterAnalysis, ArenaError> {
    check(
        stores,
        analysis_id,
        evaluation_id,
        world,
        current_operation,
        0,
        false,
    )
}

/// Verifies a supplied durable `forge.clustered` event against canonical
/// history and recomputed evidence.
///
/// # Errors
///
/// Rejects a malformed event envelope, a noncanonical hash-chain snapshot, or
/// analysis content that does not recompute from the signed candidate trial
/// evidence.
pub fn verify_cluster_event(
    stores: EvaluationStores,
    event: &StoredEvent,
    world: &CompiledWorld,
    current_operation: Option<&str>,
) -> Result<OperatorClusterAnalysis, ArenaError> {
    let (analysis_id, evaluation_id, world_id) = cluster_event_references(event)?;
    if world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch("cluster analysis world"));
    }
    let check = load_failure_clusters(
        stores,
        &analysis_id,
        &evaluation_id,
        world,
        current_operation,
    )?;
    let canonical = check
        .stores
        .events
        .replay_verified()?
        .into_iter()
        .find(|stored| stored.event_id == event.event_id)
        .ok_or(ArenaError::InvalidClusterEvent)?;
    if canonical != *event {
        return Err(ArenaError::InvalidClusterEvent);
    }
    Ok(check)
}

/// Read-only, history-borrowing counterpart to [`OperatorClusterAnalysis`],
/// with no evaluator-store capability of its own. Produced by
/// [`verify_cluster_event_in`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClusterView {
    analysis: ClusterAnalysis,
    event: ClusterEvent,
}

impl ClusterView {
    /// Canonical cluster analysis without task identities or raw outputs.
    #[must_use]
    pub const fn analysis(&self) -> &ClusterAnalysis {
        &self.analysis
    }

    /// Canonical event metadata binding the analysis into the ledger.
    #[must_use]
    pub const fn event(&self) -> &ClusterEvent {
        &self.event
    }
}

/// History-borrowing counterpart to [`verify_cluster_event`]: verifies
/// against a caller-supplied history index and artifact store instead of
/// opening fresh evaluator-owned stores and replaying the ledger again. See
/// [`crate::load_operator_evaluation_in`] for the trust requirement on
/// `index` and why callers verifying many events should build it once.
///
/// # Errors
///
/// Rejects a malformed event envelope, a noncanonical hash-chain snapshot, or
/// analysis content that does not recompute from the signed candidate trial
/// evidence.
pub fn verify_cluster_event_in(
    index: &EventIndex<'_>,
    artifacts: &ArtifactStore,
    event: &StoredEvent,
    world: &CompiledWorld,
    current_operation: Option<&str>,
) -> Result<ClusterView, ArenaError> {
    let (analysis_id, evaluation_id, world_id) = cluster_event_references(event)?;
    if world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch("cluster analysis world"));
    }
    crate::validate_id("analysis_id", &analysis_id)?;
    let operator = load_operator_evaluation_in(index, artifacts, &evaluation_id)?;
    if operator.operator_receipt.world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch("cluster analysis world"));
    }
    let evaluation_event = index
        .get(&operator.recorded.event.event_id)
        .ok_or_else(|| ArenaError::UnknownEvaluation(evaluation_id.clone()))?;
    if crate::encode_hash(evaluation_event.hash) != crate::encode_hash(operator.event_hash) {
        return Err(ArenaError::InvalidStoredReceipt("cluster evaluation event"));
    }
    let event_id = cluster_event_id(&analysis_id);
    let algorithm = index.get(&event_id).map_or(ClusterAlgorithm::V2, |stored| {
        peek_recorded_algorithm(std::slice::from_ref(stored), artifacts, &event_id)
    });
    let analysis = compute_analysis(
        algorithm,
        &analysis_id,
        &operator.operator_receipt,
        artifacts,
        index,
        evaluation_event,
        world,
        current_operation,
    )?;
    let stored = index
        .get(&event_id)
        .ok_or_else(|| ArenaError::UnknownClusterAnalysis(analysis_id.clone()))?;
    if stored != event {
        return Err(ArenaError::InvalidClusterEvent);
    }
    let rehydrated = rehydrate_event(artifacts, stored, &analysis)?;
    Ok(ClusterView {
        analysis,
        event: cluster_event(stored, &rehydrated.analysis_artifact_id),
    })
}

fn check(
    stores: EvaluationStores,
    analysis_id: &str,
    evaluation_id: &str,
    world: &CompiledWorld,
    current_operation: Option<&str>,
    timestamp_millis: i64,
    append_missing: bool,
) -> Result<OperatorClusterAnalysis, ArenaError> {
    crate::validate_id("analysis_id", analysis_id)?;
    let mut operator = load_operator_evaluation(stores, evaluation_id)?;
    if operator.operator_receipt.world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch("cluster analysis world"));
    }
    let history = operator.stores.events.replay_verified()?;
    let evaluation_event = history
        .iter()
        .find(|event| event.event_id == operator.recorded.event.event_id)
        .ok_or_else(|| ArenaError::UnknownEvaluation(evaluation_id.to_owned()))?;
    if crate::encode_hash(evaluation_event.hash) != crate::encode_hash(operator.event_hash) {
        return Err(ArenaError::InvalidStoredReceipt("cluster evaluation event"));
    }
    let index = EventIndex::build(&history);
    let event_id = cluster_event_id(analysis_id);
    let algorithm = peek_recorded_algorithm(&history, &operator.stores.artifacts, &event_id);
    let analysis = compute_analysis(
        algorithm,
        analysis_id,
        &operator.operator_receipt,
        &operator.stores.artifacts,
        &index,
        evaluation_event,
        world,
        current_operation,
    )?;
    if let Some(event) = history.iter().find(|event| event.event_id == event_id) {
        let rehydrated = rehydrate_event(&operator.stores.artifacts, event, &analysis)?;
        return Ok(OperatorClusterAnalysis {
            analysis,
            event: cluster_event(event, &rehydrated.analysis_artifact_id),
            stores: operator.into_stores(),
        });
    }
    if !append_missing {
        return Err(ArenaError::UnknownClusterAnalysis(analysis_id.to_owned()));
    }
    let analysis_bytes = serde_json::to_vec(&analysis)?;
    let analysis_artifact_id = operator.stores.artifacts.put(&analysis_bytes)?;
    let payload = ClusterEventPayload {
        schema_version: RECEIPT_SCHEMA_VERSION,
        analysis_id: analysis_id.to_owned(),
        evaluation_id: evaluation_id.to_owned(),
        world_id: world.id().to_owned(),
        analysis_artifact_id: analysis_artifact_id.as_str().to_owned(),
    };
    let event = operator.stores.events.append(EventInput::new(
        event_id,
        cluster_aggregate_id(analysis_id),
        EVENT_TYPE,
        EVENT_ACTOR,
        timestamp_millis,
        serde_json::to_vec(&payload)?,
    ))?;
    Ok(OperatorClusterAnalysis {
        analysis,
        event: cluster_event(&event, analysis_artifact_id.as_str()),
        stores: operator.into_stores(),
    })
}

/// Best-effort peek at the algorithm already recorded for `event_id`, used
/// only to choose which algorithm to recompute with (never to skip
/// verification): the full canonical recompute-and-compare that follows
/// still fails closed if this guess were ever wrong. Defaults to
/// [`ClusterAlgorithm::V2`] when no matching event exists yet (a brand-new
/// analysis) or its stored content cannot be read.
fn peek_recorded_algorithm(
    history: &[StoredEvent],
    artifacts: &ArtifactStore,
    event_id: &str,
) -> ClusterAlgorithm {
    #[derive(Deserialize)]
    struct AlgorithmPeek {
        algorithm: String,
    }
    let Some(event) = history.iter().find(|event| event.event_id == event_id) else {
        return ClusterAlgorithm::V2;
    };
    let Ok(payload) = serde_json::from_slice::<ClusterEventPayload>(&event.payload) else {
        return ClusterAlgorithm::V2;
    };
    let Ok(artifact_id) = ArtifactId::parse(payload.analysis_artifact_id) else {
        return ClusterAlgorithm::V2;
    };
    let Ok(bytes) = artifacts.get(&artifact_id) else {
        return ClusterAlgorithm::V2;
    };
    let Ok(peek) = serde_json::from_slice::<AlgorithmPeek>(&bytes) else {
        return ClusterAlgorithm::V2;
    };
    ClusterAlgorithm::from_str(&peek.algorithm).unwrap_or(ClusterAlgorithm::V2)
}

#[allow(clippy::too_many_arguments)]
fn compute_analysis(
    algorithm: ClusterAlgorithm,
    analysis_id: &str,
    receipt: &OperatorReceipt,
    artifacts: &ArtifactStore,
    index: &EventIndex<'_>,
    evaluation_event: &StoredEvent,
    world: &CompiledWorld,
    current_operation: Option<&str>,
) -> Result<ClusterAnalysis, ArenaError> {
    let visible_bytes =
        crate::verify_operator_artifact(artifacts, &receipt.visible_manifest_artifact_id)?;
    let sealed_bytes =
        crate::verify_operator_artifact(artifacts, &receipt.sealed_manifest_artifact_id)?;
    let visible = TrustedManifest::from_canonical_bytes(&visible_bytes, Visibility::Visible)?;
    let sealed = TrustedManifest::from_canonical_bytes(&sealed_bytes, Visibility::Sealed)?;

    let mut task_inputs = BTreeMap::new();
    let mut visible_expected = BTreeMap::new();
    let mut sealed_expected = BTreeMap::new();
    for task in &visible.tasks {
        task_inputs.insert(task.task_id.clone(), task.input.clone());
        visible_expected.insert(task.task_id.clone(), task.expected_output.clone());
    }
    for task in &sealed.tasks {
        if task_inputs
            .insert(task.task_id.clone(), task.input.clone())
            .is_some()
        {
            return Err(ArenaError::InvalidStoredReceipt("cluster task set"));
        }
        sealed_expected.insert(task.task_id.clone(), task.expected_output.clone());
    }

    let candidate = verified_submission_outputs(
        artifacts,
        index,
        receipt,
        &receipt.candidate_submission_artifact_id,
        &receipt.candidate_genome_id,
        &task_inputs,
        world,
        evaluation_event,
    )?;

    let mut visible_trials = Vec::new();
    let mut sealed_trials = Vec::new();
    for (task_id, trial) in &candidate {
        if let Some(expected) = visible_expected.get(task_id) {
            visible_trials.push(VisibleTrial {
                completion_reason: trial.completion_reason,
                expected_output: expected.clone(),
                actual_output: trial.stdout.clone(),
            });
        } else if let Some(expected) = sealed_expected.get(task_id) {
            sealed_trials.push(SealedTrial {
                completion_reason: trial.completion_reason,
                output_matches: trial.stdout == expected.as_bytes(),
            });
        } else {
            return Err(ArenaError::InvalidStoredReceipt("cluster task binding"));
        }
    }
    // Successful visible trials whose output matches expectation are not
    // failures; drop them before clustering so counts reflect failed trials
    // only.
    visible_trials.retain(|trial| {
        trial.completion_reason != RunCompletionReason::Success
            || std::str::from_utf8(&trial.actual_output) != Ok(trial.expected_output.as_str())
    });

    let clusters = cluster_trials_for(
        algorithm,
        &visible_trials,
        &sealed_trials,
        current_operation,
    );
    let total_visible_failed_trials =
        u32::try_from(visible_trials.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let total_sealed_failed_trials = u32::try_from(
        sealed_trials
            .iter()
            .filter(|trial| sealed_failure_signature(**trial).is_some())
            .count(),
    )
    .map_err(|_| ArenaError::TooManyTasks)?;

    Ok(ClusterAnalysis {
        schema_version: RECEIPT_SCHEMA_VERSION,
        algorithm: algorithm.as_str().to_owned(),
        candidate_operation: current_operation.map(str::to_owned),
        analysis_id: analysis_id.to_owned(),
        evaluation_id: receipt.evaluation_id.clone(),
        evaluation_event_id: evaluation_event.event_id.clone(),
        evaluation_event_hash: crate::encode_hash(evaluation_event.hash),
        world_id: receipt.world_id.clone(),
        parent_genome_id: receipt.parent_genome_id.clone(),
        candidate_genome_id: receipt.candidate_genome_id.clone(),
        total_visible_failed_trials,
        total_sealed_failed_trials,
        clusters,
    })
}

/// Reads cluster-event routing identities only after strict envelope
/// validation. The returned identities remain untrusted until verification.
///
/// # Errors
///
/// Returns [`ArenaError::InvalidClusterEvent`] for malformed, noncanonical, or
/// incorrectly identified event envelopes.
pub fn cluster_event_references(
    event: &StoredEvent,
) -> Result<(String, String, String), ArenaError> {
    let payload: ClusterEventPayload =
        serde_json::from_slice(&event.payload).map_err(|_| ArenaError::InvalidClusterEvent)?;
    if serde_json::to_vec(&payload).map_err(|_| ArenaError::InvalidClusterEvent)? != event.payload
        || payload.schema_version != RECEIPT_SCHEMA_VERSION
        || event.event_type != EVENT_TYPE
        || event.actor != EVENT_ACTOR
        || event.event_id != cluster_event_id(&payload.analysis_id)
        || event.aggregate_id != cluster_aggregate_id(&payload.analysis_id)
        || ArtifactId::parse(payload.analysis_artifact_id.clone()).is_err()
    {
        return Err(ArenaError::InvalidClusterEvent);
    }
    Ok((payload.analysis_id, payload.evaluation_id, payload.world_id))
}

fn rehydrate_event(
    artifacts: &crate::ArtifactStore,
    event: &StoredEvent,
    expected: &ClusterAnalysis,
) -> Result<ClusterEventPayload, ArenaError> {
    let (analysis_id, evaluation_id, world_id) = cluster_event_references(event)?;
    let payload: ClusterEventPayload =
        serde_json::from_slice(&event.payload).map_err(|_| ArenaError::InvalidClusterEvent)?;
    if analysis_id != expected.analysis_id
        || evaluation_id != expected.evaluation_id
        || world_id != expected.world_id
    {
        return Err(ArenaError::ClusterConflict(expected.analysis_id.clone()));
    }
    let bytes = crate::verify_operator_artifact(artifacts, &payload.analysis_artifact_id)?;
    let analysis: ClusterAnalysis = serde_json::from_slice(&bytes)
        .map_err(|_| ArenaError::InvalidStoredReceipt("cluster analysis"))?;
    if serde_json::to_vec(&analysis)
        .map_err(|_| ArenaError::InvalidStoredReceipt("cluster analysis"))?
        != bytes
        || analysis != *expected
    {
        return Err(ArenaError::ClusterConflict(expected.analysis_id.clone()));
    }
    Ok(payload)
}

fn cluster_event(event: &StoredEvent, analysis_artifact_id: &str) -> ClusterEvent {
    ClusterEvent {
        sequence: event.sequence,
        event_id: event.event_id.clone(),
        aggregate_id: event.aggregate_id.clone(),
        event_type: event.event_type.clone(),
        actor: event.actor.clone(),
        event_hash: crate::encode_hash(event.hash),
        analysis_artifact_id: analysis_artifact_id.to_owned(),
    }
}

fn cluster_event_id(analysis_id: &str) -> String {
    format!("{EVENT_ID_PREFIX}{analysis_id}:clustered")
}

fn cluster_aggregate_id(analysis_id: &str) -> String {
    format!("{EVENT_ID_PREFIX}{analysis_id}")
}

/// Stable ledger-event ID/aggregate-ID prefix used to detect a rewritten
/// `forge.clustered` event type during history verification, mirroring the
/// `arena:invariants:` prefix check used for invariant events.
pub const CLUSTER_EVENT_PREFIX: &str = EVENT_ID_PREFIX;

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(
        completion_reason: RunCompletionReason,
        expected: &str,
        actual: &str,
    ) -> VisibleTrial {
        VisibleTrial {
            completion_reason,
            expected_output: expected.to_owned(),
            actual_output: actual.as_bytes().to_owned(),
        }
    }

    #[test]
    fn case_mismatch_cluster_suggests_reference_operation_flip() {
        let trials = vec![visible(RunCompletionReason::Success, "HELLO", "hello")];
        let clusters = cluster_trials(&trials, &[]);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].signature, "shape_case_mismatch");
        assert_eq!(clusters[0].visible_count, 1);
        assert_eq!(clusters[0].sealed_count, 0);
        assert_eq!(
            clusters[0].suggested_mutation,
            Some(SuggestedMutation::ReferenceOperationFlip)
        );
    }

    #[test]
    fn whitespace_difference_has_no_supported_mutation() {
        let trials = vec![visible(
            RunCompletionReason::Success,
            "hello world",
            "hello  world\n",
        )];
        let clusters = cluster_trials(&trials, &[]);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].signature, "shape_whitespace");
        assert_eq!(clusters[0].suggested_mutation, None);
    }

    #[test]
    fn truncated_output_is_classified_and_unsupported() {
        let trials = vec![visible(
            RunCompletionReason::Success,
            "hello world",
            "hello",
        )];
        let clusters = cluster_trials(&trials, &[]);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].signature, "shape_truncation");
        assert_eq!(clusters[0].suggested_mutation, None);
    }

    #[test]
    fn empty_output_is_its_own_cluster() {
        let trials = vec![visible(RunCompletionReason::Success, "hello", "")];
        let clusters = cluster_trials(&trials, &[]);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].signature, "shape_empty_output");
        assert_eq!(clusters[0].suggested_mutation, None);
    }

    #[test]
    fn other_shape_mismatch_falls_back_to_other() {
        let trials = vec![visible(RunCompletionReason::Success, "hello", "goodbye")];
        let clusters = cluster_trials(&trials, &[]);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].signature, "shape_other");
        assert_eq!(clusters[0].suggested_mutation, None);
    }

    #[test]
    fn completion_failures_are_bucketed_by_reason_and_never_shape_analyzed() {
        let trials = vec![
            visible(RunCompletionReason::ProviderFailure, "hello", ""),
            visible(RunCompletionReason::IoFailure, "hello", ""),
            visible(RunCompletionReason::OperatorInterrupt, "hello", ""),
        ];
        let clusters = cluster_trials(&trials, &[]);
        let signatures: Vec<_> = clusters.iter().map(|c| c.signature.as_str()).collect();
        assert_eq!(
            signatures,
            vec![
                "completion_io_failure",
                "completion_operator_interrupt",
                "completion_provider_failure",
            ]
        );
        assert!(clusters.iter().all(|c| c.suggested_mutation.is_none()));
    }

    #[test]
    fn budget_exhaustion_is_distinct_from_completion_failure() {
        let trials = vec![
            visible(RunCompletionReason::WallBudgetExceeded, "hello", ""),
            visible(RunCompletionReason::OutputBudgetExceeded, "hello", ""),
        ];
        let clusters = cluster_trials(&trials, &[]);
        let signatures: Vec<_> = clusters.iter().map(|c| c.signature.as_str()).collect();
        assert_eq!(
            signatures,
            vec!["budget_output_exceeded", "budget_wall_exceeded"]
        );
    }

    #[test]
    fn sealed_trials_contribute_only_aggregate_counts() {
        let visible_trials = vec![visible(RunCompletionReason::ProviderFailure, "hello", "")];
        let sealed_trial = |completion_reason, output_matches| SealedTrial {
            completion_reason,
            output_matches,
        };
        let sealed = vec![
            sealed_trial(RunCompletionReason::ProviderFailure, false),
            sealed_trial(RunCompletionReason::Success, true),
            sealed_trial(RunCompletionReason::Success, false),
        ];
        let clusters = cluster_trials(&visible_trials, &sealed);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0].signature, "completion_provider_failure");
        assert_eq!(clusters[0].visible_count, 1);
        assert_eq!(clusters[0].sealed_count, 1);
        assert_eq!(clusters[0].total_count, 2);
        // A correct sealed run is not a failure; an incorrect one counts,
        // with no content and no suggested mutation.
        assert_eq!(clusters[1].signature, "sealed_incorrect_output");
        assert_eq!(
            (clusters[1].visible_count, clusters[1].sealed_count),
            (0, 1)
        );
        assert_eq!(clusters[1].suggested_mutation, None);
    }

    #[test]
    fn clustering_is_deterministic_across_input_order() {
        let a = vec![
            visible(RunCompletionReason::Success, "HELLO", "hello"),
            visible(RunCompletionReason::ProviderFailure, "x", ""),
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(cluster_trials(&a, &[]), cluster_trials(&b, &[]));
    }

    #[test]
    fn empty_input_produces_no_clusters() {
        assert!(cluster_trials(&[], &[]).is_empty());
    }

    fn sample_analysis(analysis_id: &str, clusters: Vec<FailureCluster>) -> ClusterAnalysis {
        ClusterAnalysis {
            schema_version: RECEIPT_SCHEMA_VERSION,
            algorithm: ALGORITHM_V1.to_owned(),
            analysis_id: analysis_id.to_owned(),
            evaluation_id: "evaluation-001".to_owned(),
            evaluation_event_id: "arena:evaluation:evaluation-001:recorded".to_owned(),
            evaluation_event_hash: "a".repeat(64),
            world_id: format!("hephaestus:world:{}", "1".repeat(64)),
            parent_genome_id: format!("hephaestus:genome:{}", "2".repeat(64)),
            candidate_genome_id: format!("hephaestus:genome:{}", "3".repeat(64)),
            candidate_operation: None,
            total_visible_failed_trials: 1,
            total_sealed_failed_trials: 0,
            clusters,
        }
    }

    #[test]
    fn rehydrate_event_conflicts_when_stored_analysis_differs_from_recomputed() {
        let directory = tempfile::tempdir().unwrap();
        let mut stores = EvaluationStores::open(
            directory.path().join("events.sqlite3"),
            directory.path().join("blobs"),
        )
        .unwrap();
        let expected = sample_analysis(
            "analysis-001",
            vec![FailureCluster {
                signature: "shape_case_mismatch".to_owned(),
                visible_count: 1,
                sealed_count: 0,
                total_count: 1,
                hypothesis: "expected hypothesis".to_owned(),
                suggested_mutation: Some(SuggestedMutation::ReferenceOperationFlip),
            }],
        );
        // The stored artifact recomputes to different cluster content under
        // the exact same analysis_id, evaluation_id, and world_id: this must
        // be rejected as a conflict, not silently accepted.
        let differing = sample_analysis(
            "analysis-001",
            vec![FailureCluster {
                signature: "shape_case_mismatch".to_owned(),
                visible_count: 2,
                sealed_count: 0,
                total_count: 2,
                hypothesis: "different hypothesis".to_owned(),
                suggested_mutation: Some(SuggestedMutation::ReferenceOperationFlip),
            }],
        );
        let analysis_artifact_id = stores
            .artifacts
            .put(&serde_json::to_vec(&differing).unwrap())
            .unwrap();
        let payload = ClusterEventPayload {
            schema_version: RECEIPT_SCHEMA_VERSION,
            analysis_id: expected.analysis_id.clone(),
            evaluation_id: expected.evaluation_id.clone(),
            world_id: expected.world_id.clone(),
            analysis_artifact_id: analysis_artifact_id.as_str().to_owned(),
        };
        let event = stores
            .events
            .append(EventInput::new(
                cluster_event_id(&expected.analysis_id),
                cluster_aggregate_id(&expected.analysis_id),
                EVENT_TYPE,
                EVENT_ACTOR,
                0,
                serde_json::to_vec(&payload).unwrap(),
            ))
            .unwrap();
        assert!(matches!(
            rehydrate_event(&stores.artifacts, &event, &expected),
            Err(ArenaError::ClusterConflict(_))
        ));
    }

    /// Pins the exact canonical bytes a `failure-cluster-v1` analysis
    /// serialized to before `candidate_operation` existed. A v1 receipt
    /// recorded by any prior build of this module must still decode from,
    /// and re-serialize to, exactly these bytes: `candidate_operation`'s
    /// `skip_serializing_if` keeps it absent, and `#[serde(default)]` fills
    /// it in as `None` on decode.
    #[test]
    fn v1_receipt_bytes_are_unchanged_by_the_new_optional_candidate_operation_field() {
        let historical = format!(
            concat!(
                "{{\"schema_version\":1,\"algorithm\":\"failure-cluster-v1\",",
                "\"analysis_id\":\"analysis-001\",\"evaluation_id\":\"evaluation-001\",",
                "\"evaluation_event_id\":\"arena:evaluation:evaluation-001:recorded\",",
                "\"evaluation_event_hash\":\"{hash}\",",
                "\"world_id\":\"hephaestus:world:{world}\",",
                "\"parent_genome_id\":\"hephaestus:genome:{parent}\",",
                "\"candidate_genome_id\":\"hephaestus:genome:{candidate}\",",
                "\"total_visible_failed_trials\":1,\"total_sealed_failed_trials\":0,",
                "\"clusters\":[{{\"signature\":\"shape_case_mismatch\",\"visible_count\":1,",
                "\"sealed_count\":0,\"total_count\":1,\"hypothesis\":\"h\",",
                "\"suggested_mutation\":\"reference_operation_flip\"}}]}}"
            ),
            hash = "a".repeat(64),
            world = "1".repeat(64),
            parent = "2".repeat(64),
            candidate = "3".repeat(64),
        );
        let decoded: ClusterAnalysis =
            serde_json::from_str(&historical).expect("historical v1 bytes decode");
        assert_eq!(decoded.algorithm, ALGORITHM_V1);
        assert_eq!(decoded.candidate_operation, None);
        assert_eq!(
            decoded.clusters[0].suggested_mutation,
            Some(SuggestedMutation::ReferenceOperationFlip)
        );
        let reencoded = serde_json::to_string(&decoded).expect("re-encode historical v1 analysis");
        assert_eq!(
            reencoded, historical,
            "a v1 receipt must re-serialize to exactly its historical bytes"
        );
    }

    fn v2_trial(actual: &str) -> VisibleTrial {
        visible(RunCompletionReason::Success, "IRRELEVANT-EXPECTED", actual)
    }

    #[test]
    fn v2_gauntlet_bad_operation_suggests_its_family_fix_regardless_of_shape() {
        // "UNKNOWN" vs "the fact" shares no shape relation at all (not a
        // case/whitespace/truncation variant), so v1 would call this
        // "shape_other" with no suggestion. v2 suggests the family fix
        // purely from `current_operation`.
        let trials = vec![v2_trial("UNKNOWN")];
        let clusters = cluster_trials_v2(&trials, &[], Some("context_loss_naive"));
        assert_eq!(clusters.len(), 1);
        assert_eq!(
            clusters[0].suggested_mutation,
            Some(SuggestedMutation::ReferenceOperation {
                operation_after: "context_loss_aware".to_owned()
            })
        );
    }

    #[test]
    fn v2_gauntlet_fix_operation_never_suggests_a_regression() {
        let trials = vec![v2_trial("some wrong output")];
        let clusters = cluster_trials_v2(&trials, &[], Some("context_loss_aware"));
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].suggested_mutation, None);
    }

    #[test]
    fn v2_casing_operation_behaves_like_v1_on_shape_case_mismatch_only() {
        let case_mismatch = vec![visible(RunCompletionReason::Success, "HELLO", "hello")];
        let clusters = cluster_trials_v2(&case_mismatch, &[], Some("identity"));
        assert_eq!(clusters.len(), 1);
        assert_eq!(
            clusters[0].suggested_mutation,
            Some(SuggestedMutation::ReferenceOperation {
                operation_after: "ascii_uppercase".to_owned()
            })
        );

        let truncated = vec![visible(
            RunCompletionReason::Success,
            "hello world",
            "hello",
        )];
        let clusters = cluster_trials_v2(&truncated, &[], Some("ascii_uppercase"));
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].suggested_mutation, None);
    }

    #[test]
    fn v2_unknown_current_operation_never_suggests_a_mutation() {
        let trials = vec![visible(RunCompletionReason::Success, "HELLO", "hello")];
        let clusters = cluster_trials_v2(&trials, &[], None);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].suggested_mutation, None);
        let clusters = cluster_trials_v2(&trials, &[], Some("not-a-real-operation"));
        assert_eq!(clusters[0].suggested_mutation, None);
    }

    #[test]
    fn v2_analysis_records_candidate_operation() {
        // `cluster_trials_v2` alone doesn't populate `ClusterAnalysis.candidate_operation`
        // (that only happens in `compute_analysis`); this asserts the plain
        // struct field plumbing instead, since a full end-to-end analysis
        // needs a real evaluation fixture (covered by the control-plane's
        // per-mode evolve tests).
        let analysis = sample_analysis("analysis-002", Vec::new());
        assert_eq!(analysis.candidate_operation, None);
        let with_operation = ClusterAnalysis {
            candidate_operation: Some("context_loss_naive".to_owned()),
            ..analysis
        };
        let bytes = serde_json::to_vec(&with_operation).unwrap();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("\"candidate_operation\":\"context_loss_naive\"")
        );
    }
}
