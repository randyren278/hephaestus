//! Trusted deterministic evaluation with sealed task boundaries and durable receipts.

mod clusters;
mod error;
#[doc(hidden)]
pub mod evaluator_protocol;
mod invariants;
mod isolated_evaluator;
mod selection;

use std::collections::{BTreeMap, BTreeSet};

pub use clusters::{
    CLUSTER_EVENT_PREFIX, ClusterAnalysis, ClusterEvent, ClusterView, FailureCluster,
    OperatorClusterAnalysis, SealedTrial, SuggestedMutation, VisibleTrial, check_failure_clusters,
    cluster_event_references, cluster_trials, load_failure_clusters, verify_cluster_event,
    verify_cluster_event_in,
};
pub use error::ArenaError;
use hephaestus_experience::{
    RunBudgetReceipt, RunCompletionReason, RunResultReceipt, RunResultVerifier,
};
use hephaestus_genome::CompiledWorld;
use hephaestus_ledger::{
    ArtifactId, ArtifactStore, EventIndex, EventInput, EventStore, StoredEvent,
};
pub use invariants::{
    InvariantEvent, InvariantPredicateResult, InvariantReceipt, InvariantView,
    OperatorInvariantCheck, check_reference_output_invariants, invariant_event_references,
    load_reference_output_invariants, verify_reference_output_invariant_event,
    verify_reference_output_invariant_event_in,
};
pub use isolated_evaluator::IsolatedEvaluator;
pub use selection::{
    OperatorSelection, SelectionEvent, SelectionReceipt, SelectionView, load_selection,
    select_and_record, selection_event_references, verify_selection_event,
    verify_selection_event_in,
};
use serde::{Deserialize, Serialize};

use crate::evaluator_protocol::{EvaluatorRequest, EvaluatorScores, EvaluatorTrial};

const EVENT_TYPE: &str = "evaluation.recorded";
const EVENT_ACTOR: &str = "arena-plane";
const VISIBLE_MANIFEST_KEY: &str = "arena.visible_manifest";
const SEALED_MANIFEST_KEY: &str = "arena.sealed_manifest";
const EVALUATOR_KEY: &str = "arena.evaluator";
const RUN_RESULT_VERIFIER_KEY: &str = "arena.runtime_verifier";
const MAX_TASKS: usize = 1_000;
const MAX_TASK_TEXT_BYTES: usize = 64 * 1024;
const MAX_SUBMISSION_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// Exact environment and evaluator provenance shared by every evaluation input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EvaluationBinding {
    /// Immutable compiled World identity.
    world_id: String,
    /// Explicit deterministic seed.
    seed: u64,
    /// Immutable execution-environment identity used by every parent trial,
    /// and by every candidate trial too unless `candidate_environment_id` is
    /// set.
    environment_id: String,
    /// Set only for a mixed pair: the candidate's own execution-environment
    /// identity, distinct from the parent's. `None` means the pair is
    /// homogeneous (the schema-v1 shape every existing evaluation used).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    candidate_environment_id: Option<String>,
    /// Immutable evaluator identity.
    evaluator_id: String,
    /// Exact hard budget tuple required for every paired trial.
    budget: RunBudgetReceipt,
}

impl EvaluationBinding {
    /// Creates and validates a homogeneous evaluation binding, where the
    /// parent and candidate run under the same execution environment.
    ///
    /// # Errors
    ///
    /// Rejects malformed World, environment, or evaluator identities.
    pub fn new(
        world_id: impl Into<String>,
        seed: u64,
        environment_id: impl Into<String>,
        evaluator_id: impl Into<String>,
        budget: RunBudgetReceipt,
    ) -> Result<Self, ArenaError> {
        let binding = Self {
            world_id: world_id.into(),
            seed,
            environment_id: environment_id.into(),
            candidate_environment_id: None,
            evaluator_id: evaluator_id.into(),
            budget,
        };
        validate_world_id(&binding.world_id)?;
        validate_id("environment_id", &binding.environment_id)?;
        ArtifactId::parse(binding.evaluator_id.clone())?;
        Ok(binding)
    }

    /// Rebinds the candidate to a distinct execution-environment identity,
    /// producing a mixed-pair binding (for example, a reference-worker
    /// parent compared against a provider-adapter candidate). Callers must
    /// only do this when the World's Law permits mixed environments.
    ///
    /// # Errors
    ///
    /// Rejects a malformed candidate environment identity.
    pub fn with_candidate_environment(
        mut self,
        candidate_environment_id: impl Into<String>,
    ) -> Result<Self, ArenaError> {
        let candidate_environment_id = candidate_environment_id.into();
        validate_id("candidate_environment_id", &candidate_environment_id)?;
        self.candidate_environment_id = Some(candidate_environment_id);
        Ok(self)
    }

    /// Returns the immutable compiled World identity.
    #[must_use]
    pub fn world_id(&self) -> &str {
        &self.world_id
    }

    /// Returns the explicit deterministic seed.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns the parent's immutable execution-environment identity.
    #[must_use]
    pub fn environment_id(&self) -> &str {
        &self.environment_id
    }

    /// Returns the candidate's immutable execution-environment identity: its
    /// own distinct identity for a mixed pair, otherwise the same identity
    /// the parent uses.
    #[must_use]
    pub fn candidate_environment_id(&self) -> &str {
        self.candidate_environment_id
            .as_deref()
            .unwrap_or(&self.environment_id)
    }

    /// Whether the parent and candidate run under distinct execution
    /// environments.
    #[must_use]
    pub const fn is_mixed_environment(&self) -> bool {
        self.candidate_environment_id.is_some()
    }

    /// Returns the immutable evaluator identity.
    #[must_use]
    pub fn evaluator_id(&self) -> &str {
        &self.evaluator_id
    }

    /// Returns the exact hard budget tuple required for paired trials.
    #[must_use]
    pub const fn budget(&self) -> RunBudgetReceipt {
        self.budget
    }
}

/// Whether a trusted task manifest may be shown to a candidate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Inputs are available before evaluation.
    Visible,
    /// Inputs and expectations remain evaluator-only.
    Sealed,
}

/// Candidate-safe task input. It contains no expected output.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CandidateTask {
    /// Stable task identity.
    pub task_id: String,
    /// Input presented to the candidate.
    pub input: String,
}

/// Trusted scheduler view of one task, deliberately excluding its expectation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OperatorTask {
    /// Stable task identity used to bind the signed runtime result.
    pub task_id: String,
    /// Exact provider input committed by the runtime.
    pub input: String,
}

/// Evaluator-owned task definition. Expected outputs have no public accessor.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct TrustedTask {
    task_id: String,
    input: String,
    expected_output: String,
}

impl TrustedTask {
    /// Creates an evaluator-only task definition.
    ///
    /// # Errors
    ///
    /// Rejects malformed task identifiers.
    pub fn new(
        task_id: impl Into<String>,
        input: impl Into<String>,
        expected_output: impl Into<String>,
    ) -> Result<Self, ArenaError> {
        let task_id = task_id.into();
        validate_id("task_id", &task_id)?;
        let input = input.into();
        let expected_output = expected_output.into();
        validate_text("task.input", &input)?;
        validate_text("task.expected_output", &expected_output)?;
        Ok(Self {
            task_id,
            input,
            expected_output,
        })
    }
}

/// Evaluator-owned immutable task manifest.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct TrustedManifest {
    schema_version: u16,
    manifest_id: String,
    visibility: Visibility,
    tasks: Vec<TrustedTask>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestWire {
    schema_version: u16,
    manifest_id: String,
    visibility: Visibility,
    tasks: Vec<TaskWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskWire {
    task_id: String,
    input: String,
    expected_output: String,
}

impl TrustedManifest {
    /// Creates a canonical manifest after sorting tasks by identity.
    ///
    /// # Errors
    ///
    /// Rejects malformed or duplicate task identities and empty manifests.
    pub fn new(
        manifest_id: impl Into<String>,
        visibility: Visibility,
        mut tasks: Vec<TrustedTask>,
    ) -> Result<Self, ArenaError> {
        let manifest_id = manifest_id.into();
        validate_id("manifest_id", &manifest_id)?;
        if tasks.is_empty() {
            return Err(ArenaError::EmptyManifest);
        }
        if tasks.len() > MAX_TASKS {
            return Err(ArenaError::TooManyTasks);
        }
        tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
        for pair in tasks.windows(2) {
            if pair[0].task_id == pair[1].task_id {
                return Err(ArenaError::DuplicateTaskId(pair[0].task_id.clone()));
            }
        }
        Ok(Self {
            schema_version: 1,
            manifest_id,
            visibility,
            tasks,
        })
    }

    /// Rehydrates exact canonical manifest bytes through all constructor checks.
    ///
    /// # Errors
    ///
    /// Rejects unknown fields, unsupported schemas, a visibility mismatch,
    /// invalid task content, or any non-canonical JSON representation.
    pub fn from_canonical_bytes(
        bytes: &[u8],
        expected_visibility: Visibility,
    ) -> Result<Self, ArenaError> {
        let wire: ManifestWire = serde_json::from_slice(bytes)?;
        if wire.schema_version != 1 {
            return Err(ArenaError::UnsupportedManifestSchema(wire.schema_version));
        }
        if wire.visibility != expected_visibility {
            return Err(ArenaError::VisibilityMismatch);
        }
        let tasks = wire
            .tasks
            .into_iter()
            .map(|task| TrustedTask::new(task.task_id, task.input, task.expected_output))
            .collect::<Result<Vec<_>, _>>()?;
        let manifest = Self::new(wire.manifest_id, wire.visibility, tasks)?;
        if serde_json::to_vec(&manifest)? != bytes {
            return Err(ArenaError::NonCanonicalManifest);
        }
        Ok(manifest)
    }

    /// Compiles operator-authored manifest JSON into its canonical form.
    ///
    /// Unlike [`Self::from_canonical_bytes`], whitespace, key order, and task
    /// order are normalized; every other constructor check still applies.
    ///
    /// # Errors
    ///
    /// Rejects unknown fields, unsupported schemas, invalid identifiers, empty
    /// or duplicate tasks, and oversized text.
    pub fn from_source_json(bytes: &[u8]) -> Result<Self, ArenaError> {
        let wire: ManifestWire = serde_json::from_slice(bytes)?;
        if wire.schema_version != 1 {
            return Err(ArenaError::UnsupportedManifestSchema(wire.schema_version));
        }
        let tasks = wire
            .tasks
            .into_iter()
            .map(|task| TrustedTask::new(task.task_id, task.input, task.expected_output))
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(wire.manifest_id, wire.visibility, tasks)
    }

    /// Returns the exact canonical bytes accepted by [`Self::from_canonical_bytes`].
    ///
    /// # Errors
    ///
    /// Serialization of a constructed manifest cannot fail in practice; the
    /// error is propagated rather than hidden.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ArenaError> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Returns candidate-safe tasks only for a visible manifest.
    ///
    /// # Errors
    ///
    /// A sealed manifest never releases its inputs through this API.
    pub fn candidate_tasks(&self) -> Result<Vec<CandidateTask>, ArenaError> {
        if self.visibility != Visibility::Visible {
            return Err(ArenaError::VisibilityMismatch);
        }
        Ok(self
            .tasks
            .iter()
            .map(|task| CandidateTask {
                task_id: task.task_id.clone(),
                input: task.input.clone(),
            })
            .collect())
    }

    /// Returns the trusted daemon scheduling view for visible or sealed tasks.
    ///
    /// Expected outputs are structurally absent from the returned type. Callers
    /// must keep this operator-scoped view outside candidate-facing APIs.
    #[must_use]
    pub fn operator_tasks(&self) -> Vec<OperatorTask> {
        self.tasks
            .iter()
            .map(|task| OperatorTask {
                task_id: task.task_id.clone(),
                input: task.input.clone(),
            })
            .collect()
    }
}

/// A caller-visible plan that binds each task to one canonical runtime result event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialPlan {
    trials: BTreeMap<String, String>,
}

impl TrialPlan {
    /// Creates a canonical task-to-runtime-event plan.
    ///
    /// # Errors
    ///
    /// Rejects malformed or duplicate task and event identifiers.
    pub fn new(trials: impl IntoIterator<Item = (String, String)>) -> Result<Self, ArenaError> {
        let mut canonical = BTreeMap::new();
        let mut run_events = BTreeSet::new();
        for (task_id, event_id) in trials {
            validate_id("task_id", &task_id)?;
            validate_run_event_id(&event_id)?;
            if !run_events.insert(event_id.clone()) {
                return Err(ArenaError::DuplicateRunEvent(event_id));
            }
            if canonical.insert(task_id.clone(), event_id).is_some() {
                return Err(ArenaError::DuplicateTaskId(task_id));
            }
            if canonical.len() > MAX_TASKS {
                return Err(ArenaError::TooManyTasks);
            }
        }
        Ok(Self { trials: canonical })
    }
}

/// Caller-provided evaluation identity and observation metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptContext {
    /// Globally unique event identifier.
    pub event_id: String,
    /// Stable evaluation aggregate identifier.
    pub evaluation_id: String,
    /// Caller metadata recorded only in the receipt payload.
    pub caller_id: String,
    /// Caller-observed Unix timestamp in milliseconds.
    pub timestamp_millis: i64,
}

impl ReceiptContext {
    fn validate(&self) -> Result<(), ArenaError> {
        validate_id("evaluation_id", &self.evaluation_id)?;
        validate_id("caller_id", &self.caller_id)?;
        let expected = canonical_event_id(&self.evaluation_id);
        if self.event_id != expected {
            return Err(ArenaError::InvalidId {
                field: "event_id",
                value: self.event_id.clone(),
            });
        }
        Ok(())
    }
}

/// Deterministic aggregate comparison with visible/sealed separation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationScores {
    /// Parent correct answers on candidate-visible tasks.
    pub parent_visible_correct: u32,
    /// Candidate correct answers on candidate-visible tasks.
    pub candidate_visible_correct: u32,
    /// Parent correct answers on evaluator-only tasks.
    pub parent_sealed_correct: u32,
    /// Candidate correct answers on evaluator-only tasks.
    pub candidate_sealed_correct: u32,
    /// Tasks the parent passed and candidate failed.
    pub regressions: u32,
    /// Tasks the parent failed and candidate passed.
    pub improvements: u32,
    /// Number of visible tasks.
    pub visible_total: u32,
    /// Number of sealed tasks.
    pub sealed_total: u32,
}

/// Paired correctness deltas without task identity, order, or sealed payloads.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct OutcomeHistogram {
    regressions: u32,
    unchanged: u32,
    improvements: u32,
}

impl OutcomeHistogram {
    /// Tasks the parent passed and candidate failed.
    #[must_use]
    pub const fn regressions(self) -> u32 {
        self.regressions
    }

    /// Tasks on which parent and candidate had the same correctness outcome.
    #[must_use]
    pub const fn unchanged(self) -> u32 {
        self.unchanged
    }

    /// Tasks the parent failed and candidate passed.
    #[must_use]
    pub const fn improvements(self) -> u32 {
        self.improvements
    }
}

/// Aggregate fitness dimensions derived from authenticated paired run receipts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct FitnessEvidence {
    correct_trials: u32,
    reliable_trials: u32,
    total_trials: u32,
    total_cost_microusd: u64,
    total_latency_millis: u64,
}

impl FitnessEvidence {
    /// Correct task count across visible and sealed trials.
    #[must_use]
    pub const fn correct_trials(self) -> u32 {
        self.correct_trials
    }

    /// Successfully completed task count.
    #[must_use]
    pub const fn reliable_trials(self) -> u32 {
        self.reliable_trials
    }

    /// Total paired task count.
    #[must_use]
    pub const fn total_trials(self) -> u32 {
        self.total_trials
    }

    /// Authenticated aggregate provider cost.
    #[must_use]
    pub const fn total_cost_microusd(self) -> u64 {
        self.total_cost_microusd
    }

    /// Authenticated aggregate terminal latency.
    #[must_use]
    pub const fn total_latency_millis(self) -> u64 {
        self.total_latency_millis
    }
}

/// Trusted aggregate input for statistical selection.
///
/// This type has no public constructor and is minted only by an
/// [`OperatorEvaluation`]. It intentionally omits task identities, task order,
/// inputs, expectations, outputs, and evaluator-owned artifact addresses.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SelectionEvidence {
    schema_version: u16,
    evaluation_id: String,
    evaluation_event_id: String,
    evaluation_event_hash: String,
    world_id: String,
    seed: u64,
    environment_id: String,
    evaluator_id: String,
    budget: RunBudgetReceipt,
    parent_genome_id: String,
    candidate_genome_id: String,
    visible_total: u32,
    sealed_total: u32,
    parent_visible_correct: u32,
    candidate_visible_correct: u32,
    parent_sealed_correct: u32,
    candidate_sealed_correct: u32,
    correctness_outcomes: OutcomeHistogram,
    parent_fitness: FitnessEvidence,
    candidate_fitness: FitnessEvidence,
}

impl SelectionEvidence {
    /// Selection evidence schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Stable Arena evaluation identity.
    #[must_use]
    pub fn evaluation_id(&self) -> &str {
        &self.evaluation_id
    }

    /// Canonical evaluation event identity.
    #[must_use]
    pub fn evaluation_event_id(&self) -> &str {
        &self.evaluation_event_id
    }

    /// Lowercase hash of the exact verified ledger event.
    #[must_use]
    pub fn evaluation_event_hash(&self) -> &str {
        &self.evaluation_event_hash
    }

    /// Exact World identity.
    #[must_use]
    pub fn world_id(&self) -> &str {
        &self.world_id
    }

    /// Paired bootstrap seed.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Exact paired execution environment identity.
    #[must_use]
    pub fn environment_id(&self) -> &str {
        &self.environment_id
    }

    /// Exact World-bound evaluator identity.
    #[must_use]
    pub fn evaluator_id(&self) -> &str {
        &self.evaluator_id
    }

    /// Exact paired hard budget.
    #[must_use]
    pub const fn budget(&self) -> RunBudgetReceipt {
        self.budget
    }

    /// Immutable parent Genome identity.
    #[must_use]
    pub fn parent_genome_id(&self) -> &str {
        &self.parent_genome_id
    }

    /// Immutable candidate Genome identity.
    #[must_use]
    pub fn candidate_genome_id(&self) -> &str {
        &self.candidate_genome_id
    }

    /// Candidate-visible task count.
    #[must_use]
    pub const fn visible_total(&self) -> u32 {
        self.visible_total
    }

    /// Evaluator-only task count, without payloads or task identities.
    #[must_use]
    pub const fn sealed_total(&self) -> u32 {
        self.sealed_total
    }

    /// Parent correct count on candidate-visible tasks.
    #[must_use]
    pub const fn parent_visible_correct(&self) -> u32 {
        self.parent_visible_correct
    }

    /// Candidate correct count on candidate-visible tasks.
    #[must_use]
    pub const fn candidate_visible_correct(&self) -> u32 {
        self.candidate_visible_correct
    }

    /// Parent correct count on evaluator-only tasks.
    #[must_use]
    pub const fn parent_sealed_correct(&self) -> u32 {
        self.parent_sealed_correct
    }

    /// Candidate correct count on evaluator-only tasks.
    #[must_use]
    pub const fn candidate_sealed_correct(&self) -> u32 {
        self.candidate_sealed_correct
    }

    /// Aggregate paired correctness outcomes used by bootstrap selection.
    #[must_use]
    pub const fn correctness_outcomes(&self) -> OutcomeHistogram {
        self.correctness_outcomes
    }

    /// Parent aggregate fitness evidence.
    #[must_use]
    pub const fn parent_fitness(&self) -> FitnessEvidence {
        self.parent_fitness
    }

    /// Candidate aggregate fitness evidence.
    #[must_use]
    pub const fn candidate_fitness(&self) -> FitnessEvidence {
        self.candidate_fitness
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorScores {
    parent_visible_correct: u32,
    candidate_visible_correct: u32,
    parent_sealed_correct: u32,
    candidate_sealed_correct: u32,
    regressions: u32,
    improvements: u32,
    visible_total: u32,
    sealed_total: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorMetrics {
    reliable_trials: u32,
    total_trials: u32,
    total_cost_microusd: u64,
    total_latency_millis: u64,
}

impl From<&OperatorScores> for EvaluationScores {
    fn from(scores: &OperatorScores) -> Self {
        Self {
            parent_visible_correct: scores.parent_visible_correct,
            candidate_visible_correct: scores.candidate_visible_correct,
            parent_sealed_correct: scores.parent_sealed_correct,
            candidate_sealed_correct: scores.candidate_sealed_correct,
            regressions: scores.regressions,
            improvements: scores.improvements,
            visible_total: scores.visible_total,
            sealed_total: scores.sealed_total,
        }
    }
}

impl From<EvaluatorScores> for OperatorScores {
    fn from(scores: EvaluatorScores) -> Self {
        Self {
            parent_visible_correct: scores.parent_visible_correct,
            candidate_visible_correct: scores.candidate_visible_correct,
            parent_sealed_correct: scores.parent_sealed_correct,
            candidate_sealed_correct: scores.candidate_sealed_correct,
            regressions: scores.regressions,
            improvements: scores.improvements,
            visible_total: scores.visible_total,
            sealed_total: scores.sealed_total,
        }
    }
}

/// Candidate-safe, visible-only evaluation result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSummary {
    /// Summary schema version.
    pub schema_version: u16,
    /// Stable evaluation identity.
    pub evaluation_id: String,
    /// Exact World identity.
    pub world_id: String,
    /// Immutable parent Genome identity.
    pub parent_genome_id: String,
    /// Immutable candidate Genome identity.
    pub candidate_genome_id: String,
    /// Parent correct answers on candidate-visible tasks.
    pub parent_visible_correct: u32,
    /// Candidate correct answers on candidate-visible tasks.
    pub candidate_visible_correct: u32,
    /// Number of candidate-visible tasks.
    pub visible_total: u32,
}

/// Evaluator-only durable wire format. Never expose or serialize this through public APIs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorReceipt {
    schema_version: u16,
    evaluation_id: String,
    caller_id: String,
    timestamp_millis: i64,
    world_id: String,
    seed: u64,
    environment_id: String,
    /// Set only for a mixed pair (`schema_version` 3): the candidate's own
    /// distinct execution-environment identity. Omitted from the wire
    /// encoding for a homogeneous pair (`schema_version` 2), so every
    /// previously recorded receipt still verifies byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_environment_id: Option<String>,
    evaluator_id: String,
    budget: RunBudgetReceipt,
    parent_submission_id: String,
    parent_genome_id: String,
    candidate_submission_id: String,
    candidate_genome_id: String,
    /// CAS address of the evaluator-only canonical visible manifest.
    visible_manifest_artifact_id: String,
    /// CAS address of the evaluator-only canonical sealed manifest.
    sealed_manifest_artifact_id: String,
    /// CAS address of candidate-safe visible task inputs.
    visible_inputs_artifact_id: String,
    /// CAS address of the evaluator-only canonical parent submission.
    parent_submission_artifact_id: String,
    /// CAS address of the evaluator-only canonical candidate submission.
    candidate_submission_artifact_id: String,
    scores: OperatorScores,
    parent_metrics: OperatorMetrics,
    candidate_metrics: OperatorMetrics,
}

/// Durable stores owned by one evaluator composition root.
pub struct EvaluationStores {
    /// Single-writer tamper-evident event store.
    pub events: EventStore,
    /// Content-addressed artifact store.
    pub artifacts: ArtifactStore,
}

impl EvaluationStores {
    /// Opens evaluator-owned durable stores.
    ///
    /// # Errors
    ///
    /// Returns storage or integrity failures.
    pub fn open(
        database: impl AsRef<std::path::Path>,
        artifact_root: impl Into<std::path::PathBuf>,
    ) -> Result<Self, ArenaError> {
        Ok(Self {
            events: EventStore::open(database)?,
            artifacts: ArtifactStore::open(artifact_root)?,
        })
    }
}

/// Payload-free ledger metadata safe to expose to callers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationEvent {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic event identity.
    pub event_id: String,
    /// Deterministic evaluation aggregate identity.
    pub aggregate_id: String,
    /// Stable event type.
    pub event_type: String,
    /// Fixed trusted actor.
    pub actor: String,
    /// Caller-observed time.
    pub timestamp_millis: i64,
}

impl From<&StoredEvent> for EvaluationEvent {
    fn from(event: &StoredEvent) -> Self {
        Self {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_type: event.event_type.clone(),
            actor: event.actor.clone(),
            timestamp_millis: event.timestamp_millis,
        }
    }
}

/// Candidate-facing evaluation result containing no raw stores or sealed evidence.
///
/// Raw ledger and artifact access is deliberately owned by [`OperatorEvaluation`].
/// Candidate-facing code cannot reach it through this type:
///
/// ```compile_fail
/// use hephaestus_arena::RecordedEvaluation;
///
/// fn raw_stores_are_not_candidate_visible(result: RecordedEvaluation) {
///     let _ = result.stores;
/// }
/// ```
///
/// Evaluator-only evidence access is absent from this type as well:
///
/// ```compile_fail
/// use hephaestus_arena::RecordedEvaluation;
///
/// fn sealed_evidence_is_not_candidate_visible(result: &RecordedEvaluation) {
///     let _ = result.operator_parent_submission();
/// }
/// ```
///
/// Statistical selection evidence is operator-only too:
///
/// ```compile_fail
/// use hephaestus_arena::RecordedEvaluation;
///
/// fn selection_evidence_is_not_candidate_visible(result: &RecordedEvaluation) {
///     let _ = result.selection_evidence();
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedEvaluation {
    /// Candidate-safe visible-only result.
    pub summary: EvaluationSummary,
    /// Payload-free canonical ledger metadata.
    pub event: EvaluationEvent,
}

/// Trusted evaluator-owned result, receipt, and durable-store capability.
///
/// Only this operator-side wrapper can inspect sealed aggregates or reclaim the
/// stores required for subsequent single-writer operations. Candidate-facing
/// adapters should return [`Self::into_candidate_result`] instead.
pub struct OperatorEvaluation {
    recorded: RecordedEvaluation,
    stores: EvaluationStores,
    operator_receipt: OperatorReceipt,
    event_hash: [u8; 32],
}

impl OperatorEvaluation {
    /// Borrows the candidate-safe result without exposing operator capabilities.
    #[must_use]
    pub const fn candidate_result(&self) -> &RecordedEvaluation {
        &self.recorded
    }

    /// Consumes the operator wrapper and returns only the candidate-safe result.
    #[must_use]
    pub fn into_candidate_result(self) -> RecordedEvaluation {
        self.recorded
    }

    /// Reclaims the evaluator-owned stores for the next trusted operation.
    #[must_use]
    pub fn into_stores(self) -> EvaluationStores {
        self.stores
    }

    /// Returns aggregate evaluator-only scores to a trusted operator.
    #[must_use]
    pub fn operator_scores(&self) -> EvaluationScores {
        EvaluationScores::from(&self.operator_receipt.scores)
    }

    /// Produces aggregate, event-bound evidence for trusted statistical selection.
    ///
    /// The result contains no task-level or artifact-store capabilities.
    ///
    /// # Panics
    ///
    /// Panics only if an internal receipt already validated during construction
    /// no longer satisfies the same aggregate invariants.
    #[must_use]
    pub fn selection_evidence(&self) -> SelectionEvidence {
        selection_evidence(
            &self.operator_receipt,
            &self.recorded.event.event_id,
            self.event_hash,
        )
        .expect("stored operator receipts are validated before construction")
    }

    /// Reads and verifies the evaluator-only parent submission evidence.
    ///
    /// # Errors
    ///
    /// Rejects missing, malformed, or corrupted receipt evidence.
    pub fn operator_parent_submission(&self) -> Result<Vec<u8>, ArenaError> {
        verify_operator_artifact(
            &self.stores.artifacts,
            &self.operator_receipt.parent_submission_artifact_id,
        )
    }

    /// Reads and verifies candidate-safe visible task inputs.
    ///
    /// # Errors
    ///
    /// Rejects missing, malformed, or corrupted receipt evidence.
    pub fn operator_visible_inputs(&self) -> Result<Vec<u8>, ArenaError> {
        verify_operator_artifact(
            &self.stores.artifacts,
            &self.operator_receipt.visible_inputs_artifact_id,
        )
    }
}

/// Exact evaluator-owned inputs for one paired comparison.
#[derive(Clone, Copy)]
pub struct EvaluationInputs<'a> {
    /// World, seed, environment, and evaluator provenance.
    pub binding: &'a EvaluationBinding,
    /// Candidate-visible task manifest.
    pub visible: &'a TrustedManifest,
    /// Evaluator-only sealed task manifest.
    pub sealed: &'a TrustedManifest,
    /// Baseline task-to-runtime-event plan.
    pub parent: &'a TrialPlan,
    /// Proposed replacement task-to-runtime-event plan.
    pub candidate: &'a TrialPlan,
    /// Process-backed trusted evaluator isolated from candidate execution and stores.
    pub evaluator: &'a IsolatedEvaluator,
}

/// Canonical, evaluator-only inputs for preparing paired evaluation scoring.
#[derive(Clone, Copy)]
pub struct EvaluationSources<'a> {
    /// World, seed, environment, and evaluator provenance.
    pub binding: &'a EvaluationBinding,
    /// Candidate-visible task manifest.
    pub visible: &'a TrustedManifest,
    /// Evaluator-only sealed task manifest.
    pub sealed: &'a TrustedManifest,
    /// Baseline task-to-runtime-event plan.
    pub parent: &'a TrialPlan,
    /// Proposed replacement task-to-runtime-event plan.
    pub candidate: &'a TrialPlan,
}

/// Prepared trusted evaluator request. It is an in-memory capability with no
/// public constructor or deserialization path; its sealed inputs must never be
/// included in job status or progress responses.
pub struct PreparedEvaluation {
    request: EvaluatorRequest,
    source_commitment: String,
}

/// Verified evaluator scores bound to the exact prepared request bytes.
pub struct ScoredEvaluation {
    evaluation_id: String,
    request_artifact_id: String,
    source_commitment: String,
    scores: OperatorScores,
}

impl PreparedEvaluation {
    /// Scores this sealed evaluator request with its registered isolated evaluator.
    ///
    /// # Errors
    ///
    /// Rejects evaluator execution, protocol, binding, or score validation errors.
    pub fn score(self, evaluator: &IsolatedEvaluator) -> Result<ScoredEvaluation, ArenaError> {
        self.score_with(|evaluation_id, request| evaluator.evaluate(evaluation_id, request))
    }

    /// Scores this request while binding process lifetime to its daemon guardian.
    ///
    /// # Errors
    ///
    /// Rejects evaluator execution, protocol, binding, or score validation errors.
    pub fn score_guarded(
        self,
        evaluator: &IsolatedEvaluator,
        guardian: &std::path::Path,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ScoredEvaluation, ArenaError> {
        self.score_with(|evaluation_id, request| {
            evaluator.evaluate_guarded(evaluation_id, request, guardian, cancel)
        })
    }

    fn score_with(
        self,
        evaluate: impl FnOnce(
            &str,
            &EvaluatorRequest,
        )
            -> Result<crate::evaluator_protocol::EvaluatorResponse, ArenaError>,
    ) -> Result<ScoredEvaluation, ArenaError> {
        let request_bytes = serde_json::to_vec(&self.request)?;
        let request_artifact_id = ArtifactId::for_bytes(&request_bytes).as_str().to_owned();
        let evaluation_id = self.request.evaluation_id.clone();
        let scores = evaluate(&evaluation_id, &self.request)?.scores.into();
        Ok(ScoredEvaluation {
            evaluation_id,
            request_artifact_id,
            source_commitment: self.source_commitment,
            scores,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmissionEvidence {
    schema_version: u16,
    genome_id: String,
    trials: BTreeMap<String, TrialEvidence>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrialEvidence {
    run_result_event_id: String,
    run_result_event_hash: String,
    source_revision: String,
    completion_reason: RunCompletionReason,
    latency_millis: u64,
    actual_cost_microusd: u64,
    stdout_artifact_id: String,
    stderr_artifact_id: String,
    trace_artifact_ids: Vec<String>,
}

struct ResolvedSubmission {
    id: String,
    genome_id: String,
    outputs: BTreeMap<String, String>,
    reliable: BTreeMap<String, bool>,
    revisions: BTreeMap<String, String>,
    metrics: OperatorMetrics,
    evidence: Vec<u8>,
}

struct ResolvedPair {
    parent: ResolvedSubmission,
    candidate: ResolvedSubmission,
}

struct PreparedArtifacts {
    visible_manifest: Vec<u8>,
    sealed_manifest: Vec<u8>,
    visible_inputs: Vec<u8>,
    parent_submission: Vec<u8>,
    candidate_submission: Vec<u8>,
    visible_manifest_id: ArtifactId,
    sealed_manifest_id: ArtifactId,
    visible_inputs_id: ArtifactId,
    parent_submission_id: ArtifactId,
    candidate_submission_id: ArtifactId,
}

impl PreparedArtifacts {
    fn publish(&self, artifacts: &ArtifactStore) -> Result<(), ArenaError> {
        for bytes in [
            &self.visible_manifest,
            &self.sealed_manifest,
            &self.visible_inputs,
            &self.parent_submission,
            &self.candidate_submission,
        ] {
            artifacts.put(bytes)?;
        }
        Ok(())
    }
}

/// Evaluates paired submissions and durably records exactly one receipt event.
///
/// # Errors
///
/// Fails closed on provenance, manifest, task-set, serialization, or storage errors.
#[allow(clippy::too_many_lines)]
pub fn evaluate_and_record(
    owned_stores: EvaluationStores,
    context: ReceiptContext,
    world: &CompiledWorld,
    inputs: EvaluationInputs<'_>,
) -> Result<OperatorEvaluation, ArenaError> {
    evaluate_and_record_inner(owned_stores, context, world, inputs, None)
}

/// Validates canonical paired-run evidence and prepares an opaque evaluator request.
///
/// The returned value may be moved to a bounded background task. It contains
/// evaluator-only task material and must never be serialized into public job state.
///
/// # Errors
///
/// Fails closed on provenance, manifest, task-set, artifact, or storage errors.
pub fn prepare_evaluation(
    stores: &EvaluationStores,
    context: &ReceiptContext,
    world: &CompiledWorld,
    sources: EvaluationSources<'_>,
) -> Result<PreparedEvaluation, ArenaError> {
    context.validate()?;
    let EvaluationSources {
        binding,
        visible,
        sealed,
        parent,
        candidate,
    } = sources;
    let task_inputs = validate_evaluation_inputs(world, binding, visible, sealed)?;
    validate_world_evaluator_artifacts(world, &stores.artifacts, binding, visible, sealed)?;
    let verifier = run_result_verifier(world, &stores.artifacts)?;
    let history = stores.events.replay_verified()?;
    let ResolvedPair { parent, candidate } = resolve_pair(
        parent,
        candidate,
        &task_inputs,
        binding,
        &history,
        &stores.artifacts,
        &verifier,
    )?;
    let artifacts = prepare_artifacts(visible, sealed, &parent, &candidate)?;
    let source_commitment = source_commitment(context, world, binding, &artifacts)?;
    Ok(PreparedEvaluation {
        request: make_evaluator_request(
            &context.evaluation_id,
            binding,
            visible,
            sealed,
            &parent,
            &candidate,
        ),
        source_commitment,
    })
}

/// Commits scores returned by [`PreparedEvaluation::score`] after revalidating
/// the request against current canonical evidence.
///
/// # Errors
///
/// Rejects stale or cross-bound evaluator output and all normal receipt failures.
pub fn evaluate_and_record_scored(
    owned_stores: EvaluationStores,
    context: ReceiptContext,
    world: &CompiledWorld,
    inputs: EvaluationInputs<'_>,
    scored: ScoredEvaluation,
) -> Result<OperatorEvaluation, ArenaError> {
    evaluate_and_record_inner(owned_stores, context, world, inputs, Some(scored))
}

#[allow(clippy::too_many_lines)]
fn evaluate_and_record_inner(
    mut owned_stores: EvaluationStores,
    context: ReceiptContext,
    world: &CompiledWorld,
    inputs: EvaluationInputs<'_>,
    scored: Option<ScoredEvaluation>,
) -> Result<OperatorEvaluation, ArenaError> {
    let EvaluationInputs {
        binding,
        visible,
        sealed,
        parent,
        candidate,
        evaluator,
    } = inputs;
    context.validate()?;
    let task_inputs = validate_evaluation_inputs(world, binding, visible, sealed)?;
    validate_world_evaluator_artifacts(world, &owned_stores.artifacts, binding, visible, sealed)?;
    let run_result_verifier = run_result_verifier(world, &owned_stores.artifacts)?;
    let history = owned_stores.events.replay_verified()?;
    let ResolvedPair { parent, candidate } = resolve_pair(
        parent,
        candidate,
        &task_inputs,
        binding,
        &history,
        &owned_stores.artifacts,
        &run_result_verifier,
    )?;

    let prepared = prepare_artifacts(visible, sealed, &parent, &candidate)?;

    let request = make_evaluator_request(
        &context.evaluation_id,
        binding,
        visible,
        sealed,
        &parent,
        &candidate,
    );
    let aggregate = if let Some(scored) = scored {
        let request_bytes = serde_json::to_vec(&request)?;
        if scored.evaluation_id != context.evaluation_id
            || scored.request_artifact_id != ArtifactId::for_bytes(&request_bytes).as_str()
        {
            return Err(ArenaError::BindingMismatch("prepared evaluator request"));
        }
        let current_source_commitment = source_commitment(&context, world, binding, &prepared)?;
        if scored.source_commitment != current_source_commitment {
            return Err(ArenaError::BindingMismatch("prepared evaluation sources"));
        }
        scored.scores
    } else {
        evaluator
            .evaluate(&context.evaluation_id, &request)?
            .scores
            .into()
    };
    let summary = EvaluationSummary {
        schema_version: 1,
        evaluation_id: context.evaluation_id.clone(),
        world_id: binding.world_id.clone(),
        parent_genome_id: parent.genome_id.clone(),
        candidate_genome_id: candidate.genome_id.clone(),
        parent_visible_correct: aggregate.parent_visible_correct,
        candidate_visible_correct: aggregate.candidate_visible_correct,
        visible_total: aggregate.visible_total,
    };
    let receipt = OperatorReceipt {
        schema_version: if binding.is_mixed_environment() { 3 } else { 2 },
        evaluation_id: context.evaluation_id.clone(),
        caller_id: context.caller_id.clone(),
        timestamp_millis: context.timestamp_millis,
        world_id: binding.world_id.clone(),
        seed: binding.seed,
        environment_id: binding.environment_id.clone(),
        candidate_environment_id: binding
            .is_mixed_environment()
            .then(|| binding.candidate_environment_id().to_owned()),
        evaluator_id: binding.evaluator_id.clone(),
        budget: binding.budget,
        parent_submission_id: parent.id.clone(),
        parent_genome_id: parent.genome_id.clone(),
        candidate_submission_id: candidate.id.clone(),
        candidate_genome_id: candidate.genome_id.clone(),
        visible_manifest_artifact_id: prepared.visible_manifest_id.as_str().to_owned(),
        sealed_manifest_artifact_id: prepared.sealed_manifest_id.as_str().to_owned(),
        visible_inputs_artifact_id: prepared.visible_inputs_id.as_str().to_owned(),
        parent_submission_artifact_id: prepared.parent_submission_id.as_str().to_owned(),
        candidate_submission_artifact_id: prepared.candidate_submission_id.as_str().to_owned(),
        scores: aggregate,
        parent_metrics: parent.metrics,
        candidate_metrics: candidate.metrics,
    };
    validate_operator_receipt(&receipt)?;
    let expected_event_id = canonical_event_id(&context.evaluation_id);
    let expected_aggregate_id = canonical_aggregate_id(&context.evaluation_id);

    if let Some(existing) = history
        .iter()
        .find(|event| event.event_id == expected_event_id)
    {
        let existing_receipt = rehydrate_operator_receipt(
            &owned_stores.artifacts,
            existing,
            &EventIndex::build(&history),
        )?;
        if existing.aggregate_id != expected_aggregate_id
            || !receipts_match_except_timestamp(&existing_receipt, &receipt)
        {
            return Err(ArenaError::EvaluationConflict(context.evaluation_id));
        }
        return Ok(OperatorEvaluation {
            recorded: RecordedEvaluation {
                summary: summary_from_receipt(&existing_receipt),
                event: EvaluationEvent::from(existing),
            },
            stores: owned_stores,
            operator_receipt: existing_receipt,
            event_hash: existing.hash,
        });
    }

    // All validation and conflict detection is complete. CAS publication can now begin.
    prepared.publish(&owned_stores.artifacts)?;
    let payload = serde_json::to_vec(&receipt)?;
    let event = owned_stores.events.append(EventInput::new(
        expected_event_id,
        expected_aggregate_id,
        EVENT_TYPE,
        EVENT_ACTOR,
        context.timestamp_millis,
        payload,
    ))?;
    Ok(OperatorEvaluation {
        recorded: RecordedEvaluation {
            summary,
            event: EvaluationEvent::from(&event),
        },
        stores: owned_stores,
        operator_receipt: receipt,
        event_hash: event.hash,
    })
}

/// Read-only, history-borrowing counterpart to [`OperatorEvaluation`], with no
/// evaluator-store capability of its own. Produced by [`load_operator_evaluation_in`].
pub struct OperatorEvaluationView {
    recorded: RecordedEvaluation,
    operator_receipt: OperatorReceipt,
    event_hash: [u8; 32],
}

impl OperatorEvaluationView {
    /// Borrows the candidate-safe result without exposing operator capabilities.
    #[must_use]
    pub const fn candidate_result(&self) -> &RecordedEvaluation {
        &self.recorded
    }

    /// Consumes the view and returns only the candidate-safe result.
    #[must_use]
    pub fn into_candidate_result(self) -> RecordedEvaluation {
        self.recorded
    }

    /// Produces aggregate, event-bound evidence for trusted statistical selection.
    ///
    /// # Panics
    ///
    /// Panics only if an internal receipt already validated during construction
    /// no longer satisfies the same aggregate invariants.
    #[must_use]
    pub fn selection_evidence(&self) -> SelectionEvidence {
        selection_evidence(
            &self.operator_receipt,
            &self.recorded.event.event_id,
            self.event_hash,
        )
        .expect("stored operator receipts are validated before construction")
    }
}

/// Rehydrates one trusted evaluation capability from verified canonical history.
///
/// # Errors
///
/// Fails closed when the identity is malformed or the event, receipt, or any
/// evaluator-owned evidence artifact is missing or inconsistent.
pub fn load_operator_evaluation(
    stores: EvaluationStores,
    evaluation_id: &str,
) -> Result<OperatorEvaluation, ArenaError> {
    let history = stores.events.replay_verified()?;
    let index = EventIndex::build(&history);
    let view = load_operator_evaluation_in(&index, &stores.artifacts, evaluation_id)?;
    Ok(OperatorEvaluation {
        recorded: view.recorded,
        stores,
        operator_receipt: view.operator_receipt,
        event_hash: view.event_hash,
    })
}

/// History-borrowing counterpart to [`load_operator_evaluation`]: verifies
/// against a caller-supplied history and artifact store instead of opening
/// fresh evaluator-owned stores and replaying the ledger again. Used by a
/// daemon projection refresh to verify every evidence event against the one
/// history it already replayed for this refresh, instead of reopening the
/// event store and re-replaying the whole ledger once per event (TD-16).
///
/// `index` lets a caller verifying many events reuse one O(history length)
/// index across all of them instead of each call re-scanning `history`, which
/// would make a full verification pass quadratic again; build it once with
/// [`EventIndex::build`].
///
/// # Trust
///
/// The caller must supply an index over history from
/// `EventLedger::replay_verified()` (or an equally verified source) obtained
/// in the *same* operation as this call. This function trusts that the hash
/// chain has already been verified and does not check it again itself.
///
/// # Errors
///
/// Fails closed exactly as [`load_operator_evaluation`] does: an invalid
/// identity, or a missing, malformed, or inconsistent evaluation event or any
/// evaluator-owned evidence artifact.
pub fn load_operator_evaluation_in(
    index: &EventIndex<'_>,
    artifacts: &ArtifactStore,
    evaluation_id: &str,
) -> Result<OperatorEvaluationView, ArenaError> {
    validate_id("evaluation_id", evaluation_id)?;
    let event_id = canonical_event_id(evaluation_id);
    let event = index
        .get(&event_id)
        .ok_or_else(|| ArenaError::UnknownEvaluation(evaluation_id.to_owned()))?;
    let receipt = rehydrate_operator_receipt(artifacts, event, index)?;
    Ok(OperatorEvaluationView {
        recorded: RecordedEvaluation {
            summary: summary_from_receipt(&receipt),
            event: EvaluationEvent::from(event),
        },
        operator_receipt: receipt,
        event_hash: event.hash,
    })
}

/// Rehydrates the candidate-safe record for one evaluation from verified
/// canonical history and its evaluator-owned evidence artifacts.
///
/// This deliberately returns no store capability or sealed receipt. Control
/// projections can use it to compare persisted public summaries with the
/// authenticated evaluation that produced them.
///
/// # Errors
///
/// Fails closed when the identity, event, receipt, or any evidence artifact is
/// missing or inconsistent.
pub fn load_recorded_evaluation(
    stores: EvaluationStores,
    evaluation_id: &str,
) -> Result<RecordedEvaluation, ArenaError> {
    load_operator_evaluation(stores, evaluation_id).map(OperatorEvaluation::into_candidate_result)
}

/// History-borrowing counterpart to [`load_recorded_evaluation`]. See
/// [`load_operator_evaluation_in`] for the trust requirement on `index` and
/// why callers verifying many events should build it once and reuse it.
///
/// # Errors
///
/// See [`load_operator_evaluation_in`].
pub fn load_recorded_evaluation_in(
    index: &EventIndex<'_>,
    artifacts: &ArtifactStore,
    evaluation_id: &str,
) -> Result<RecordedEvaluation, ArenaError> {
    load_operator_evaluation_in(index, artifacts, evaluation_id)
        .map(OperatorEvaluationView::into_candidate_result)
}

fn resolve_pair(
    parent_plan: &TrialPlan,
    candidate_plan: &TrialPlan,
    task_inputs: &BTreeMap<String, String>,
    binding: &EvaluationBinding,
    history: &[StoredEvent],
    artifacts: &ArtifactStore,
    run_result_verifier: &RunResultVerifier,
) -> Result<ResolvedPair, ArenaError> {
    let parent = resolve_plan(
        "parent",
        parent_plan,
        task_inputs,
        binding,
        history,
        artifacts,
        run_result_verifier,
    )?;
    let candidate = resolve_plan(
        "candidate",
        candidate_plan,
        task_inputs,
        binding,
        history,
        artifacts,
        run_result_verifier,
    )?;
    if parent.genome_id == candidate.genome_id {
        return Err(ArenaError::DuplicateGenomeId(parent.genome_id));
    }
    for task_id in task_inputs.keys() {
        if parent.revisions[task_id] != candidate.revisions[task_id] {
            return Err(ArenaError::SourceRevisionMismatch(task_id.clone()));
        }
    }

    Ok(ResolvedPair { parent, candidate })
}

fn prepare_artifacts(
    visible: &TrustedManifest,
    sealed: &TrustedManifest,
    parent: &ResolvedSubmission,
    candidate: &ResolvedSubmission,
) -> Result<PreparedArtifacts, ArenaError> {
    let visible_manifest = serde_json::to_vec(visible)?;
    let sealed_manifest = serde_json::to_vec(sealed)?;
    let visible_inputs = serde_json::to_vec(&visible.candidate_tasks()?)?;
    let parent_submission = parent.evidence.clone();
    let candidate_submission = candidate.evidence.clone();
    let prepared = PreparedArtifacts {
        visible_manifest_id: ArtifactId::for_bytes(&visible_manifest),
        sealed_manifest_id: ArtifactId::for_bytes(&sealed_manifest),
        visible_inputs_id: ArtifactId::for_bytes(&visible_inputs),
        parent_submission_id: ArtifactId::for_bytes(&parent_submission),
        candidate_submission_id: ArtifactId::for_bytes(&candidate_submission),
        visible_manifest,
        sealed_manifest,
        visible_inputs,
        parent_submission,
        candidate_submission,
    };
    Ok(prepared)
}

fn source_commitment(
    context: &ReceiptContext,
    world: &CompiledWorld,
    binding: &EvaluationBinding,
    artifacts: &PreparedArtifacts,
) -> Result<String, ArenaError> {
    // Submission artifact IDs commit the signed event IDs/hashes, Genome
    // identities, source revisions, outputs, reliability, and fitness metrics.
    // World and binding fields prevent score reuse across otherwise identical
    // evaluator requests with different execution provenance.
    let canonical = serde_json::to_vec(&(
        1_u16,
        &context.event_id,
        &context.evaluation_id,
        &context.caller_id,
        context.timestamp_millis,
        world.id(),
        binding,
        artifacts.visible_manifest_id.as_str(),
        artifacts.sealed_manifest_id.as_str(),
        artifacts.parent_submission_id.as_str(),
        artifacts.candidate_submission_id.as_str(),
    ))?;
    Ok(ArtifactId::for_bytes(&canonical).as_str().to_owned())
}

fn validate_world_evaluator_artifacts(
    world: &CompiledWorld,
    artifacts: &ArtifactStore,
    binding: &EvaluationBinding,
    visible: &TrustedManifest,
    sealed: &TrustedManifest,
) -> Result<(), ArenaError> {
    verify_world_artifact(
        world,
        artifacts,
        VISIBLE_MANIFEST_KEY,
        &ArtifactId::for_bytes(&serde_json::to_vec(visible)?),
    )?;
    verify_world_artifact(
        world,
        artifacts,
        SEALED_MANIFEST_KEY,
        &ArtifactId::for_bytes(&serde_json::to_vec(sealed)?),
    )?;
    let evaluator_id = required_world_artifact(world, EVALUATOR_KEY)?;
    if evaluator_id != binding.evaluator_id {
        return Err(ArenaError::WorldArtifactMismatch(EVALUATOR_KEY));
    }
    artifacts.get(&ArtifactId::parse(evaluator_id)?)?;
    Ok(())
}

fn canonical_event_id(evaluation_id: &str) -> String {
    format!("arena:evaluation:{evaluation_id}:recorded")
}

fn canonical_aggregate_id(evaluation_id: &str) -> String {
    format!("arena:evaluation:{evaluation_id}")
}

fn summary_from_receipt(receipt: &OperatorReceipt) -> EvaluationSummary {
    EvaluationSummary {
        schema_version: 1,
        evaluation_id: receipt.evaluation_id.clone(),
        world_id: receipt.world_id.clone(),
        parent_genome_id: receipt.parent_genome_id.clone(),
        candidate_genome_id: receipt.candidate_genome_id.clone(),
        parent_visible_correct: receipt.scores.parent_visible_correct,
        candidate_visible_correct: receipt.scores.candidate_visible_correct,
        visible_total: receipt.scores.visible_total,
    }
}

fn receipts_match_except_timestamp(left: &OperatorReceipt, right: &OperatorReceipt) -> bool {
    let mut normalized = right.clone();
    normalized.timestamp_millis = left.timestamp_millis;
    left == &normalized
}

fn rehydrate_operator_receipt(
    artifacts: &ArtifactStore,
    event: &StoredEvent,
    index: &EventIndex<'_>,
) -> Result<OperatorReceipt, ArenaError> {
    let receipt: OperatorReceipt = serde_json::from_slice(&event.payload)?;
    let schema_version_matches_shape = match receipt.schema_version {
        2 => receipt.candidate_environment_id.is_none(),
        3 => receipt.candidate_environment_id.is_some(),
        _ => false,
    };
    if !schema_version_matches_shape
        || serde_json::to_vec(&receipt)? != event.payload
        || event.event_id != canonical_event_id(&receipt.evaluation_id)
        || event.aggregate_id != canonical_aggregate_id(&receipt.evaluation_id)
        || event.event_type != EVENT_TYPE
        || event.actor != EVENT_ACTOR
        || event.timestamp_millis != receipt.timestamp_millis
    {
        return Err(ArenaError::InvalidStoredReceipt("event metadata"));
    }
    for artifact_id in [
        &receipt.visible_manifest_artifact_id,
        &receipt.sealed_manifest_artifact_id,
        &receipt.visible_inputs_artifact_id,
        &receipt.parent_submission_artifact_id,
        &receipt.candidate_submission_artifact_id,
    ] {
        verify_operator_artifact(artifacts, artifact_id)?;
    }
    verify_operator_artifact(artifacts, &receipt.evaluator_id)?;
    verify_submission_evidence(
        artifacts,
        index,
        &receipt.parent_submission_artifact_id,
        &receipt.parent_submission_id,
        &receipt.parent_genome_id,
        receipt.parent_metrics,
    )?;
    verify_submission_evidence(
        artifacts,
        index,
        &receipt.candidate_submission_artifact_id,
        &receipt.candidate_submission_id,
        &receipt.candidate_genome_id,
        receipt.candidate_metrics,
    )?;
    validate_operator_receipt(&receipt)?;
    Ok(receipt)
}

fn verify_submission_evidence(
    artifacts: &ArtifactStore,
    index: &EventIndex<'_>,
    artifact_id: &str,
    submission_id: &str,
    genome_id: &str,
    expected_metrics: OperatorMetrics,
) -> Result<(), ArenaError> {
    if submission_id != artifact_id {
        return Err(ArenaError::InvalidStoredReceipt("submission identity"));
    }
    let bytes = verify_operator_artifact(artifacts, artifact_id)?;
    let evidence: SubmissionEvidence = serde_json::from_slice(&bytes)?;
    if evidence.schema_version != 1
        || serde_json::to_vec(&evidence)? != bytes
        || evidence.genome_id != genome_id
    {
        return Err(ArenaError::InvalidStoredReceipt("submission evidence"));
    }
    let mut metrics = OperatorMetrics {
        reliable_trials: 0,
        total_trials: 0,
        total_cost_microusd: 0,
        total_latency_millis: 0,
    };
    for trial in evidence.trials.values() {
        let run_event = index
            .get(trial.run_result_event_id.as_str())
            .ok_or_else(|| ArenaError::UnknownRunEvent(trial.run_result_event_id.clone()))?;
        if trial.run_result_event_hash != encode_hash(run_event.hash) {
            return Err(ArenaError::InvalidStoredReceipt("run event hash"));
        }
        for referenced in std::iter::once(&trial.stdout_artifact_id)
            .chain(std::iter::once(&trial.stderr_artifact_id))
            .chain(trial.trace_artifact_ids.iter())
        {
            verify_operator_artifact(artifacts, referenced)?;
        }
        metrics.total_trials = metrics
            .total_trials
            .checked_add(1)
            .ok_or(ArenaError::InvalidStoredReceipt("submission metrics"))?;
        metrics.reliable_trials = metrics
            .reliable_trials
            .checked_add(u32::from(
                trial.completion_reason == RunCompletionReason::Success,
            ))
            .ok_or(ArenaError::InvalidStoredReceipt("submission metrics"))?;
        metrics.total_cost_microusd = metrics
            .total_cost_microusd
            .checked_add(trial.actual_cost_microusd)
            .ok_or(ArenaError::InvalidStoredReceipt("submission metrics"))?;
        metrics.total_latency_millis = metrics
            .total_latency_millis
            .checked_add(trial.latency_millis)
            .ok_or(ArenaError::InvalidStoredReceipt("submission metrics"))?;
    }
    if metrics != expected_metrics {
        return Err(ArenaError::InvalidStoredReceipt("submission metrics"));
    }
    Ok(())
}

fn validate_operator_receipt(receipt: &OperatorReceipt) -> Result<(), ArenaError> {
    let scores = &receipt.scores;
    let total = scores
        .visible_total
        .checked_add(scores.sealed_total)
        .ok_or(ArenaError::InvalidStoredReceipt("task total"))?;
    let parent_correct = scores
        .parent_visible_correct
        .checked_add(scores.parent_sealed_correct)
        .ok_or(ArenaError::InvalidStoredReceipt("parent correctness"))?;
    let candidate_correct = scores
        .candidate_visible_correct
        .checked_add(scores.candidate_sealed_correct)
        .ok_or(ArenaError::InvalidStoredReceipt("candidate correctness"))?;
    let paired_delta = i64::from(scores.improvements) - i64::from(scores.regressions);
    if scores.parent_visible_correct > scores.visible_total
        || scores.candidate_visible_correct > scores.visible_total
        || scores.parent_sealed_correct > scores.sealed_total
        || scores.candidate_sealed_correct > scores.sealed_total
        || scores.regressions > total
        || scores.improvements > total
        || scores
            .regressions
            .checked_add(scores.improvements)
            .is_none_or(|changed| changed > total)
        || receipt.parent_metrics.total_trials != total
        || receipt.candidate_metrics.total_trials != total
        || receipt.parent_metrics.reliable_trials > total
        || receipt.candidate_metrics.reliable_trials > total
        || parent_correct > receipt.parent_metrics.reliable_trials
        || candidate_correct > receipt.candidate_metrics.reliable_trials
        || i64::from(candidate_correct) - i64::from(parent_correct) != paired_delta
    {
        return Err(ArenaError::InvalidStoredReceipt("aggregate metrics"));
    }
    Ok(())
}

fn selection_evidence(
    receipt: &OperatorReceipt,
    event_id: &str,
    event_hash: [u8; 32],
) -> Result<SelectionEvidence, ArenaError> {
    validate_operator_receipt(receipt)?;
    let total = receipt
        .scores
        .visible_total
        .checked_add(receipt.scores.sealed_total)
        .ok_or(ArenaError::InvalidStoredReceipt("task total"))?;
    let changed = receipt
        .scores
        .regressions
        .checked_add(receipt.scores.improvements)
        .ok_or(ArenaError::InvalidStoredReceipt("outcome histogram"))?;
    let unchanged = total
        .checked_sub(changed)
        .ok_or(ArenaError::InvalidStoredReceipt("outcome histogram"))?;
    let parent_correct = receipt
        .scores
        .parent_visible_correct
        .checked_add(receipt.scores.parent_sealed_correct)
        .ok_or(ArenaError::InvalidStoredReceipt("parent correctness"))?;
    let candidate_correct = receipt
        .scores
        .candidate_visible_correct
        .checked_add(receipt.scores.candidate_sealed_correct)
        .ok_or(ArenaError::InvalidStoredReceipt("candidate correctness"))?;
    Ok(SelectionEvidence {
        schema_version: 1,
        evaluation_id: receipt.evaluation_id.clone(),
        evaluation_event_id: event_id.to_owned(),
        evaluation_event_hash: encode_hash(event_hash),
        world_id: receipt.world_id.clone(),
        seed: receipt.seed,
        environment_id: receipt.environment_id.clone(),
        evaluator_id: receipt.evaluator_id.clone(),
        budget: receipt.budget,
        parent_genome_id: receipt.parent_genome_id.clone(),
        candidate_genome_id: receipt.candidate_genome_id.clone(),
        visible_total: receipt.scores.visible_total,
        sealed_total: receipt.scores.sealed_total,
        parent_visible_correct: receipt.scores.parent_visible_correct,
        candidate_visible_correct: receipt.scores.candidate_visible_correct,
        parent_sealed_correct: receipt.scores.parent_sealed_correct,
        candidate_sealed_correct: receipt.scores.candidate_sealed_correct,
        correctness_outcomes: OutcomeHistogram {
            regressions: receipt.scores.regressions,
            unchanged,
            improvements: receipt.scores.improvements,
        },
        parent_fitness: fitness_evidence(parent_correct, receipt.parent_metrics),
        candidate_fitness: fitness_evidence(candidate_correct, receipt.candidate_metrics),
    })
}

const fn fitness_evidence(correct_trials: u32, metrics: OperatorMetrics) -> FitnessEvidence {
    FitnessEvidence {
        correct_trials,
        reliable_trials: metrics.reliable_trials,
        total_trials: metrics.total_trials,
        total_cost_microusd: metrics.total_cost_microusd,
        total_latency_millis: metrics.total_latency_millis,
    }
}

fn encode_hash(hash: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in hash {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn validate_evaluation_inputs(
    world: &CompiledWorld,
    binding: &EvaluationBinding,
    visible: &TrustedManifest,
    sealed: &TrustedManifest,
) -> Result<BTreeMap<String, String>, ArenaError> {
    if binding.world_id != world.id() {
        return Err(ArenaError::BindingMismatch("world"));
    }
    if visible.visibility != Visibility::Visible || sealed.visibility != Visibility::Sealed {
        return Err(ArenaError::VisibilityMismatch);
    }
    if visible.manifest_id == sealed.manifest_id {
        return Err(ArenaError::DuplicateManifestId(visible.manifest_id.clone()));
    }
    let mut task_inputs = BTreeMap::new();
    for task in visible.tasks.iter().chain(&sealed.tasks) {
        if task_inputs
            .insert(
                task.task_id.clone(),
                ArtifactId::for_bytes(task.input.as_bytes())
                    .as_str()
                    .to_owned(),
            )
            .is_some()
        {
            return Err(ArenaError::DuplicateTaskId(task.task_id.clone()));
        }
        if task_inputs.len() > MAX_TASKS {
            return Err(ArenaError::TooManyTasks);
        }
    }
    Ok(task_inputs)
}

#[allow(clippy::too_many_lines)]
fn resolve_plan(
    plan_name: &'static str,
    plan: &TrialPlan,
    expected_tasks: &BTreeMap<String, String>,
    binding: &EvaluationBinding,
    history: &[StoredEvent],
    artifacts: &ArtifactStore,
    run_result_verifier: &RunResultVerifier,
) -> Result<ResolvedSubmission, ArenaError> {
    if plan.trials.keys().ne(expected_tasks.keys()) {
        return Err(ArenaError::TaskSetMismatch {
            submission_id: plan_name.to_owned(),
        });
    }
    let events = history
        .iter()
        .map(|event| (event.event_id.as_str(), event))
        .collect::<BTreeMap<_, _>>();
    let mut genome_id = None;
    let mut outputs = BTreeMap::new();
    let mut reliable = BTreeMap::new();
    let mut revisions = BTreeMap::new();
    let mut trials = BTreeMap::new();
    let mut output_bytes = 0_usize;
    let mut reliable_trials = 0_u32;
    let mut total_cost_microusd = 0_u64;
    let mut total_latency_millis = 0_u64;
    for (task_id, event_id) in &plan.trials {
        let event = events
            .get(event_id.as_str())
            .ok_or_else(|| ArenaError::UnknownRunEvent(event_id.clone()))?;
        let receipt = RunResultReceipt::parse_from_event(event, run_result_verifier)?;
        if receipt.world_id != binding.world_id {
            return Err(ArenaError::RunWorldMismatch(event_id.clone()));
        }
        let expected_environment_id = if plan_name == "candidate" {
            binding.candidate_environment_id()
        } else {
            binding.environment_id()
        };
        if receipt.task_id != *task_id
            || receipt.input_commitment != expected_tasks[task_id]
            || receipt.seed != binding.seed
            || receipt.environment_id != expected_environment_id
            || receipt.budget != binding.budget
        {
            return Err(ArenaError::BindingMismatch("runtime experiment context"));
        }
        if genome_id
            .as_ref()
            .is_some_and(|genome| genome != &receipt.genome_id)
        {
            return Err(ArenaError::MixedSubmissionGenome);
        }
        genome_id.get_or_insert_with(|| receipt.genome_id.clone());
        let stdout = verify_operator_artifact(artifacts, &receipt.stdout_artifact_id)?;
        verify_operator_artifact(artifacts, &receipt.stderr_artifact_id)?;
        for artifact_id in &receipt.trace_artifact_ids {
            verify_operator_artifact(artifacts, artifact_id)?;
        }
        output_bytes = output_bytes
            .checked_add(stdout.len())
            .ok_or(ArenaError::TextTooLarge {
                field: "submission.outputs",
                limit: MAX_SUBMISSION_OUTPUT_BYTES,
            })?;
        if stdout.len() > MAX_TASK_TEXT_BYTES {
            return Err(ArenaError::TextTooLarge {
                field: "submission.output",
                limit: MAX_TASK_TEXT_BYTES,
            });
        }
        if output_bytes > MAX_SUBMISSION_OUTPUT_BYTES {
            return Err(ArenaError::TextTooLarge {
                field: "submission.outputs",
                limit: MAX_SUBMISSION_OUTPUT_BYTES,
            });
        }
        let completed_successfully = receipt.completion_reason == RunCompletionReason::Success;
        reliable_trials = reliable_trials
            .checked_add(u32::from(completed_successfully))
            .ok_or(ArenaError::MetricOverflow("reliable trials"))?;
        total_cost_microusd = total_cost_microusd
            .checked_add(receipt.actual_cost_microusd)
            .ok_or(ArenaError::MetricOverflow("cost"))?;
        total_latency_millis = total_latency_millis
            .checked_add(receipt.latency_millis)
            .ok_or(ArenaError::MetricOverflow("latency"))?;
        let output = if completed_successfully {
            String::from_utf8(stdout).map_err(|_| ArenaError::InvalidRunOutput(event_id.clone()))?
        } else {
            String::new()
        };
        outputs.insert(task_id.clone(), output);
        reliable.insert(task_id.clone(), completed_successfully);
        revisions.insert(task_id.clone(), receipt.source_revision.clone());
        trials.insert(
            task_id.clone(),
            TrialEvidence {
                run_result_event_id: event_id.clone(),
                run_result_event_hash: encode_hash(event.hash),
                source_revision: receipt.source_revision,
                completion_reason: receipt.completion_reason,
                latency_millis: receipt.latency_millis,
                actual_cost_microusd: receipt.actual_cost_microusd,
                stdout_artifact_id: receipt.stdout_artifact_id,
                stderr_artifact_id: receipt.stderr_artifact_id,
                trace_artifact_ids: receipt.trace_artifact_ids,
            },
        );
    }
    let total_trials = u32::try_from(trials.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let evidence = serde_json::to_vec(&SubmissionEvidence {
        schema_version: 1,
        genome_id: genome_id.clone().ok_or(ArenaError::EmptyManifest)?,
        trials,
    })?;
    Ok(ResolvedSubmission {
        id: ArtifactId::for_bytes(&evidence).as_str().to_owned(),
        genome_id: genome_id.ok_or(ArenaError::EmptyManifest)?,
        outputs,
        reliable,
        revisions,
        metrics: OperatorMetrics {
            reliable_trials,
            total_trials,
            total_cost_microusd,
            total_latency_millis,
        },
        evidence,
    })
}

fn required_world_artifact<'a>(
    world: &'a CompiledWorld,
    name: &'static str,
) -> Result<&'a str, ArenaError> {
    world
        .evaluator_artifact(name)
        .ok_or(ArenaError::MissingWorldArtifact(name))
}

fn run_result_verifier(
    world: &CompiledWorld,
    artifacts: &ArtifactStore,
) -> Result<RunResultVerifier, ArenaError> {
    let id = required_world_artifact(world, RUN_RESULT_VERIFIER_KEY)?;
    let bytes = artifacts.get(&ArtifactId::parse(id)?)?;
    let public_key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ArenaError::InvalidStoredReceipt("runtime verifier"))?;
    RunResultVerifier::from_public_key_bytes(public_key)
        .map_err(|_| ArenaError::InvalidStoredReceipt("runtime verifier"))
}

fn verify_world_artifact(
    world: &CompiledWorld,
    artifacts: &ArtifactStore,
    name: &'static str,
    actual: &ArtifactId,
) -> Result<(), ArenaError> {
    let expected = required_world_artifact(world, name)?;
    if expected != actual.as_str() {
        return Err(ArenaError::WorldArtifactMismatch(name));
    }
    artifacts.get(&ArtifactId::parse(expected)?)?;
    Ok(())
}

fn make_evaluator_request(
    evaluation_id: &str,
    binding: &EvaluationBinding,
    visible: &TrustedManifest,
    sealed: &TrustedManifest,
    parent: &ResolvedSubmission,
    candidate: &ResolvedSubmission,
) -> EvaluatorRequest {
    let make_trials = |manifest: &TrustedManifest| {
        manifest
            .tasks
            .iter()
            .map(|task| EvaluatorTrial {
                task_id: task.task_id.clone(),
                expected_output: task.expected_output.clone(),
                parent_output: parent.outputs[&task.task_id].clone(),
                candidate_output: candidate.outputs[&task.task_id].clone(),
                parent_reliable: parent.reliable[&task.task_id],
                candidate_reliable: candidate.reliable[&task.task_id],
            })
            .collect()
    };
    EvaluatorRequest {
        schema_version: 1,
        evaluation_id: evaluation_id.to_owned(),
        evaluator_id: binding.evaluator_id.clone(),
        visible: make_trials(visible),
        sealed: make_trials(sealed),
    }
}

fn validate_world_id(value: &str) -> Result<(), ArenaError> {
    let Some(hash) = value.strip_prefix("hephaestus:world:") else {
        return Err(ArenaError::InvalidId {
            field: "world_id",
            value: value.to_owned(),
        });
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ArenaError::InvalidId {
            field: "world_id",
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_text(field: &'static str, value: &str) -> Result<(), ArenaError> {
    if value.len() > MAX_TASK_TEXT_BYTES {
        return Err(ArenaError::TextTooLarge {
            field,
            limit: MAX_TASK_TEXT_BYTES,
        });
    }
    Ok(())
}

fn validate_id(field: &'static str, value: &str) -> Result<(), ArenaError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        return Err(ArenaError::InvalidId {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn validate_run_event_id(value: &str) -> Result<(), ArenaError> {
    let Some(run_id) = value.strip_prefix("result:") else {
        return Err(ArenaError::InvalidId {
            field: "run_event_id",
            value: value.to_owned(),
        });
    };
    if run_id.is_empty()
        || run_id.len() > 128
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ArenaError::InvalidId {
            field: "run_event_id",
            value: value.to_owned(),
        });
    }
    Ok(())
}

fn verify_operator_artifact(
    artifacts: &ArtifactStore,
    artifact_id: &str,
) -> Result<Vec<u8>, ArenaError> {
    let id = ArtifactId::parse(artifact_id)?;
    Ok(artifacts.get(&id)?)
}
