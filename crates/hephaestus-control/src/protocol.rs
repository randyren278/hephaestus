use hephaestus_arena::{InvariantEvent, InvariantReceipt, SelectionReceipt};
use hephaestus_experience::RunBudgetReceipt;
pub use hephaestus_experience::RunCompletionReason;
pub use hephaestus_genome::{GenomeRecord, WorldRecord};
use serde::{Deserialize, Serialize};

/// The only local operator API version accepted by this release.
pub const API_VERSION: u16 = 1;

/// One authenticated request over the owner-only local socket.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRequest {
    /// Protocol version.
    pub version: u16,
    /// Caller-generated correlation identifier.
    pub request_id: String,
    /// Secret read from the owner-only daemon token file.
    pub token: String,
    /// Typed operator command.
    pub command: Command,
}

/// Operator actions implemented by the first control plane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Inspect the current canonical projection.
    Status,
    /// Stop new evolution work.
    Freeze,
    /// Resume evolution through the external operator boundary.
    Unfreeze,
    /// Terminate all active work represented by canonical events.
    KillAll,
    /// Inspect one immutable Genome record.
    GenomeShow {
        /// Content-derived Genome identity.
        genome_id: String,
    },
    /// Read the verified reserved prompt of one registered Markdown Genome.
    GenomePrompt {
        /// Content-derived Genome identity.
        genome_id: String,
    },
    /// List every registered immutable Genome record.
    GenomeList,
    /// Compile one Genome source file under a registered World and register it.
    GenomeRegister {
        /// Absolute path to a JSON, YAML, or Markdown Genome source readable by the daemon.
        path: String,
        /// Content-derived registered World identity governing the Genome.
        world_id: String,
    },
    /// Propose one compiler-validated prompt mutation from a trusted selection.
    GenomePropose {
        /// Stable idempotency key for this proposal.
        proposal_id: String,
        /// Exact canonical selection event to which the proposal is bound.
        selection_event_id: String,
        /// Parent Genome; must be the selected candidate.
        parent_genome_id: String,
        /// Operator-authored, bounded hypothesis for the one prompt change.
        hypothesis: String,
    },
    /// Assess one proposed child against a verified Arena selection receipt.
    GenomeAssess {
        /// Stable idempotency key for the assessment event.
        assessment_id: String,
        /// Durable Forge proposal being assessed.
        proposal_id: String,
        /// Exact child-selection event whose receipt supplies the metrics outcome.
        selection_event_id: String,
    },
    /// Inspect one immutable World record.
    WorldShow {
        /// Content-derived World identity.
        world_id: String,
    },
    /// List every registered immutable World record.
    WorldList,
    /// Compile one World source file and register it.
    WorldRegister {
        /// Absolute path to a JSON or YAML World source readable by the daemon.
        path: String,
    },
    /// Canonicalize one Arena task manifest and store it as an artifact.
    ManifestPut {
        /// Absolute path to a JSON manifest readable by the daemon.
        path: String,
    },
    /// Store one file in the content-addressed artifact store.
    ArtifactPut {
        /// Absolute path to a file readable by the daemon.
        path: String,
    },
    /// Publish the daemon's runtime-result verifier public key as an artifact.
    VerifierShow,
    /// Admit one bounded asynchronous direct reference run.
    RunSubmit {
        /// Caller-selected idempotency key, unique for one immutable Genome.
        job_id: String,
        /// Content-derived registered Genome identity.
        genome_id: String,
    },
    /// Inspect one admitted asynchronous run.
    JobStatus {
        /// Stable job identity returned by submission.
        job_id: String,
    },
    /// Request cancellation of one active asynchronous run.
    JobKill {
        /// Stable job identity returned by submission.
        job_id: String,
    },
    /// Execute the offline deterministic reference runtime synchronously.
    RunReference {
        /// Content-derived registered Genome identity.
        genome_id: String,
    },
    /// Execute one World-bound task with daemon-owned runtime provenance.
    RunEvaluation {
        /// Content-derived registered Genome identity.
        genome_id: String,
        /// Exact World task identity.
        task_id: String,
        /// Exact provider input committed by the runtime.
        input: String,
        /// Deterministic evaluation seed.
        seed: u64,
        /// Hard wall deadline in milliseconds.
        wall_millis: u64,
        /// Maximum combined output bytes.
        maximum_output_bytes: u64,
        /// Maximum provider spend in micro-US dollars.
        maximum_cost_microusd: u64,
    },
    /// Run one trusted parent-versus-candidate evaluation from daemon-owned inputs.
    EvaluatePair {
        /// Stable caller-selected evaluation identity.
        evaluation_id: String,
        /// Immutable registered parent Genome identity.
        parent_genome_id: String,
        /// Immutable registered candidate Genome identity.
        candidate_genome_id: String,
    },
    /// Select from one exact persisted Arena evaluation using its registered World's policy.
    ArenaSelect {
        /// Stable Arena evaluation identity.
        evaluation_id: String,
    },
    /// Check and persist aggregate reference-output invariants for one Arena evaluation.
    ArenaInvariants {
        /// Stable Arena evaluation identity whose authenticated outputs are checked.
        evaluation_id: String,
    },
    /// Bootstrap the first Champion of a World by explicit operator authority.
    ChampionSeed {
        /// Stable idempotency key for this Champion transition.
        transition_id: String,
        /// Registered World whose Champion is seeded.
        world_id: String,
        /// Registered Genome compiled under that World.
        genome_id: String,
        /// Operator-authored, bounded reason for the bootstrap.
        reason: String,
    },
    /// Promote a Forge child whose assessment and invariant evidence pass World policy.
    ChampionPromote {
        /// Stable idempotency key for this Champion transition.
        transition_id: String,
        /// Durable Forge assessment of the child against the current Champion.
        assessment_id: String,
    },
    /// Restore the previous Champion of a World and quarantine the current one.
    ChampionRollback {
        /// Stable idempotency key for this Champion transition.
        transition_id: String,
        /// World whose current Champion is rolled back.
        world_id: String,
        /// Operator-authored, bounded reason for the rollback.
        reason: String,
    },
    /// Inspect the Champion projection and transition history of one World.
    ChampionShow {
        /// Registered World identity.
        world_id: String,
    },
    /// Verify and replay canonical history into a fresh projection.
    Replay,
    /// List recent direct runs and jobs, newest first, bounded by `limit`.
    RunList {
        /// Maximum number of entries returned; capped at 200.
        limit: u32,
    },
    /// List recent Arena evaluations, newest first, bounded by `limit`.
    EvaluationList {
        /// Maximum number of entries returned; capped at 200.
        limit: u32,
    },
    /// List recent refused operator requests and recorded runtime denials, newest first.
    DenialList {
        /// Maximum number of entries returned; capped at 200.
        limit: u32,
    },
    /// Stop the local daemon after acknowledging the audited request.
    DaemonStop,
}

/// Hard ceiling on any bounded list command's `limit` field.
pub const MAX_LIST_LIMIT: u32 = 200;

/// Stable machine-readable local API response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiResponse {
    /// Protocol version emitted by the daemon.
    pub version: u16,
    /// Correlation identifier copied from the request when available.
    pub request_id: String,
    /// Successful response body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
    /// Stable failure body with no internal error disclosure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl ApiResponse {
    pub(crate) fn success(request_id: String, data: ResponseData) -> Self {
        Self {
            version: API_VERSION,
            request_id,
            data: Some(data),
            error: None,
        }
    }

    pub(crate) fn failure(
        request_id: impl Into<String>,
        code: ApiErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            version: API_VERSION,
            request_id: request_id.into(),
            data: None,
            error: Some(ApiError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Successful typed response data.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseData {
    /// Current desired and observed control state.
    Status {
        /// Whether evolution is frozen.
        frozen: bool,
        /// Number of canonically active runs.
        active_runs: usize,
        /// Number of canonical events after auditing this request.
        event_count: u64,
        /// Number of registered immutable Genomes.
        genome_count: usize,
    },
    /// Deterministic acknowledgement of a consequential operator action.
    Acknowledged {
        /// Freeze state after the action.
        frozen: bool,
        /// Runs terminated by this action.
        killed_runs: usize,
    },
    /// One registered immutable Genome.
    Genome {
        /// Canonical projection record.
        genome: GenomeRecord,
    },
    /// One durable, compiler-backed Forge child proposal. It is not a promotion.
    ForgeProposal {
        /// Proposal fields and compiler-verified child Genome registration.
        proposal: Box<ForgeProposalRecord>,
    },
    /// One evidence-bound Forge assessment. It never authorizes promotion.
    ForgeAssessment {
        /// Assessment payload and canonical event metadata.
        assessment: Box<ForgeAssessmentRecord>,
    },
    /// Exact UTF-8 body bytes of a registered Genome's reserved prompt.
    GenomePrompt {
        /// Content-derived Genome identity.
        genome_id: String,
        /// Prompt body, without Markdown frontmatter.
        prompt: String,
    },
    /// Every registered immutable Genome in canonical identity order.
    Genomes {
        /// Canonical projection records.
        genomes: Vec<GenomeRecord>,
    },
    /// One registered immutable World.
    World {
        /// Canonical projection record.
        world: WorldRecord,
    },
    /// Every registered immutable World in canonical identity order.
    Worlds {
        /// Canonical projection records.
        worlds: Vec<WorldRecord>,
    },
    /// One content-addressed artifact.
    Artifact {
        /// BLAKE3 artifact address.
        artifact_id: String,
        /// Stored size in bytes.
        bytes: u64,
    },
    /// The daemon's Ed25519 runtime-result verifier.
    Verifier {
        /// CAS address of the raw 32-byte public key, usable as `arena.runtime_verifier`.
        artifact_id: String,
        /// Hex-encoded public key.
        public_key_hex: String,
    },
    /// Current durable state of one asynchronous direct run.
    Job {
        /// Canonical job projection.
        job: JobRecord,
        /// Durable trace progress projected from the canonical event stream.
        progress: JobProgress,
    },
    /// Durable paired Arena progress with no task-level or sealed content.
    ArenaJob {
        /// Publicly scoped progress projection.
        job: ArenaJobProgress,
    },
    /// Terminal result from the offline deterministic reference runtime.
    Run {
        /// Stable run identity.
        run_id: String,
        /// Immutable Genome identity executed by the runtime.
        genome_id: String,
        /// Immutable registered World identity governing the run.
        world_id: String,
        /// Exact Git commit inventoried by the isolated runtime.
        source_revision: String,
        /// Exact runtime-owned completion reason.
        completion_reason: RunCompletionReason,
        /// Runtime-owned terminal latency.
        latency_millis: u64,
        /// Exact deterministic provider cost in micro-US dollars.
        actual_cost_microusd: u64,
        /// CAS address of the bounded inventory output.
        stdout_artifact_id: String,
        /// CAS address of the bounded diagnostic output.
        stderr_artifact_id: String,
        /// CAS addresses of the redacted trace artifacts.
        trace_artifact_ids: Vec<String>,
    },
    /// Visible aggregate result from a trusted paired evaluation.
    Evaluation {
        /// Visible summary and payload-free canonical event metadata.
        evaluation: EvaluationRecord,
    },
    /// Operator-only metrics and fail-closed promotion state for one selection.
    Selection {
        /// Aggregate measurements and payload-free canonical selection event metadata.
        selection: Box<SelectionRecord>,
    },
    /// Operator-only aggregate reference-output invariant receipt and event.
    ArenaInvariants {
        /// Aggregate checks and payload-free canonical invariant event metadata.
        invariants: Box<InvariantRecord>,
    },
    /// One durable, policy-checked Champion transition.
    ChampionTransition {
        /// Transition payload and canonical event metadata.
        transition: Box<ChampionTransitionRecord>,
    },
    /// Champion projection of one World reconstructed from verified history.
    Champion {
        /// Current Champion, archived predecessors, and transition history.
        champion: Box<ChampionRecord>,
    },
    /// Result of a fresh verified replay.
    Replay {
        /// Number of verified canonical events.
        event_count: u64,
        /// Freeze state reconstructed from history.
        frozen: bool,
        /// Active run count reconstructed from history.
        active_runs: usize,
        /// Stable BLAKE3 hash of the reconstructed projection.
        projection_hash: String,
    },
    /// Recent direct runs and jobs, newest first and bounded.
    RunList {
        /// Entries in newest-first order.
        runs: Vec<RunListEntry>,
    },
    /// Recent Arena evaluations, newest first and bounded.
    EvaluationList {
        /// Entries in newest-first order.
        evaluations: Vec<EvaluationListEntry>,
    },
    /// Recent refused operator requests and recorded runtime denials, newest first and bounded.
    DenialList {
        /// Entries in newest-first order.
        denials: Vec<DenialEntry>,
    },
}

/// One direct run or job entry, combining `state.jobs` lifecycle with a verified run result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunListEntry {
    /// Deterministic underlying runtime identity.
    pub run_id: String,
    /// Caller-selected job idempotency key, present only for `RunSubmit` jobs.
    pub job_id: Option<String>,
    /// Immutable Genome identity executed by the runtime.
    pub genome_id: String,
    /// Immutable registered World identity, when known.
    pub world_id: Option<String>,
    /// Current bounded lifecycle state.
    pub state: JobState,
    /// Runtime-owned terminal reason, present only after a verified result.
    pub completion_reason: Option<RunCompletionReason>,
    /// Runtime-owned terminal latency, present only after a verified result.
    pub latency_millis: Option<u64>,
    /// Exact deterministic provider cost in micro-US dollars, present only after a verified result.
    pub actual_cost_microusd: Option<u64>,
}

/// Visible aggregate selection outcome for one evaluation, boundary-matched to `Selection`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationSelectionSummary {
    /// Whether measured confidence and Pareto dimensions pass their gates.
    pub metrics_eligible: bool,
    /// Paired correctness mean estimate in basis points.
    pub estimate_bps: i64,
    /// Lower percentile bound of the paired correctness delta.
    pub lower_bps: i64,
    /// Upper percentile bound of the paired correctness delta.
    pub upper_bps: i64,
    /// Parent aggregate cost in micro-US dollars.
    pub parent_cost_microusd: u64,
    /// Candidate aggregate cost in micro-US dollars.
    pub candidate_cost_microusd: u64,
    /// Parent aggregate terminal latency in milliseconds.
    pub parent_latency_millis: u64,
    /// Candidate aggregate terminal latency in milliseconds.
    pub candidate_latency_millis: u64,
    /// Whether an independent trusted invariant evaluator supplied invariant evidence.
    pub invariant_gate_verified: bool,
    /// Whether the deterministic promotion policy is satisfied.
    pub promotion_eligible: bool,
}

/// Visible aggregate invariant outcome for one evaluation, boundary-matched to `ArenaInvariants`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationInvariantSummary {
    /// Number of individual parent/candidate predicate checks.
    pub total_checks: u32,
    /// Sum of candidate violations across all predicates and trials.
    pub total_candidate_violations: u32,
    /// Sum of paired regressions across all predicates and trials.
    pub total_paired_regressions: u32,
    /// World policy ceiling for paired regressions.
    pub maximum_regressions: u32,
    /// Sum of paired regressions across all predicates is within the World policy.
    pub regressions_within_budget: bool,
    /// True exactly when candidate outputs have no invariant violations.
    pub candidate_contract_satisfied: bool,
}

/// Reference to a Forge assessment recorded against one evaluation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationForgeSummary {
    /// Stable caller-selected idempotency key of the assessment.
    pub assessment_id: String,
    /// Metrics-only outcome recomputed from the verified `SelectionReceipt`.
    pub outcome: ForgeAssessmentOutcome,
}

/// One Arena evaluation entry with its visible aggregates and evidence references.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationListEntry {
    /// Candidate-safe visible aggregate and ledger metadata.
    pub evaluation: EvaluationRecord,
    /// Selection outcome, present only once `ArenaSelect` has recorded one.
    pub selection: Option<EvaluationSelectionSummary>,
    /// Invariant outcome, present only once `ArenaInvariants` has recorded one.
    pub invariants: Option<EvaluationInvariantSummary>,
    /// Forge assessment reference, present only once one has been recorded against this evaluation.
    pub forge_assessment: Option<EvaluationForgeSummary>,
    /// Champion transition idempotency keys whose promotion evidence cites this evaluation.
    pub champion_transition_ids: Vec<String>,
}

/// Stable, non-sensitive category of one recorded denial.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenialKind {
    /// The operator request was refused before any command could be attempted.
    RequestRejected,
    /// The runtime authority boundary denied one capability during a run.
    RuntimeCapabilityDenied,
}

/// One recorded refusal, drawn only from canonical events that ledger a denial.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DenialEntry {
    /// Stable non-sensitive category of this denial.
    pub kind: DenialKind,
    /// Caller-observed Unix timestamp in milliseconds.
    pub timestamp_millis: i64,
    /// Correlation identifier of the refused request, present only for `RequestRejected`.
    pub request_id: Option<String>,
    /// Stable command tag that was refused, present only for `RequestRejected`.
    pub command: Option<String>,
    /// Deterministic underlying runtime identity, present only for `RuntimeCapabilityDenied`.
    pub run_id: Option<String>,
    /// Immutable Genome identity, present only for `RuntimeCapabilityDenied`.
    pub genome_id: Option<String>,
    /// Immutable World identity, present only for `RuntimeCapabilityDenied`.
    pub world_id: Option<String>,
}

/// Canonical payload of one durable Forge proposal event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeProposalPayload {
    /// Proposal payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub proposal_id: String,
    /// Exact verified selection event ID.
    pub selection_event_id: String,
    /// Hash of the exact selection event in the canonical ledger.
    pub selection_event_hash: String,
    /// Stable Arena evaluation identity from the selection receipt.
    pub evaluation_id: String,
    /// Exact registered World under which parent and child compile.
    pub world_id: String,
    /// Selected candidate Genome used as the sole parent.
    pub parent_genome_id: String,
    /// Child registration compiled by the ordinary Genome compiler.
    pub child: GenomeRecord,
    /// Explicit operator-authored hypothesis.
    pub hypothesis: String,
    /// Reserved artifact mutated by this proposal; currently always `agent.prompt`.
    pub artifact_name: String,
    /// Exact parent prompt artifact address.
    pub prompt_artifact_before: String,
    /// Exact child prompt artifact address.
    pub prompt_artifact_after: String,
    /// Reference operation found in the parent prompt.
    pub operation_before: String,
    /// Single operation proposed for the child prompt.
    pub operation_after: String,
}

/// Canonical event metadata accompanying a Forge proposal response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeProposalEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic idempotent event identity.
    pub event_id: String,
    /// Forge proposal aggregate identity.
    pub aggregate_id: String,
    /// Event-chain hash.
    pub event_hash: String,
}

/// Operator-visible proposal and durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeProposalRecord {
    /// Canonical proposal event payload.
    pub payload: ForgeProposalPayload,
    /// Canonical event metadata.
    pub event: ForgeProposalEventRecord,
    /// This milestone never authorizes or performs promotion.
    pub promotion_eligible: bool,
}

/// Metrics-only Forge assessment outcome derived from a verified `SelectionReceipt`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeAssessmentOutcome {
    /// The verified receipt passed its metrics eligibility policy.
    MetricsPassed,
    /// The verified receipt did not pass its metrics eligibility policy.
    MetricsRejected,
}

/// Canonical payload of one evidence-bound Forge assessment event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAssessmentPayload {
    /// Assessment payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub assessment_id: String,
    /// Forge proposal identifier being assessed.
    pub proposal_id: String,
    /// Exact prior Forge proposal event identity and hash.
    pub proposal_event_id: String,
    /// Hash of the exact Forge proposal event.
    pub proposal_event_hash: String,
    /// Exact child-selection event identity and hash.
    pub selection_event_id: String,
    /// Hash of the verified child-selection event.
    pub selection_event_hash: String,
    /// Content address of the exact verified `SelectionReceipt`.
    pub selection_receipt_artifact_id: String,
    /// Stable child evaluation identity from the verified receipt.
    pub evaluation_id: String,
    /// Exact source evaluation event identity and hash.
    pub evaluation_event_id: String,
    /// Hash of the exact source evaluation event.
    pub evaluation_event_hash: String,
    /// World identity shared by proposal and child evaluation.
    pub world_id: String,
    /// Parent Genome identity in the evaluated directed pair.
    pub parent_genome_id: String,
    /// Proposed child Genome identity in the evaluated directed pair.
    pub child_genome_id: String,
    /// Metrics-only outcome recomputed from the verified `SelectionReceipt`.
    pub outcome: ForgeAssessmentOutcome,
    /// This assessment does not include independent invariant evidence.
    pub invariant_gate_verified: bool,
    /// This assessment does not authorize or perform promotion.
    pub promotion_eligible: bool,
}

/// Canonical event metadata accompanying a Forge assessment response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAssessmentEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic idempotent assessment event identity.
    pub event_id: String,
    /// Forge proposal aggregate identity.
    pub aggregate_id: String,
    /// Event-chain hash.
    pub event_hash: String,
}

/// Operator-visible assessment and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAssessmentRecord {
    /// Canonical assessment event payload.
    pub payload: ForgeAssessmentPayload,
    /// Canonical event metadata.
    pub event: ForgeAssessmentEventRecord,
}

/// Durable lifecycle projection for one direct asynchronous reference run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobRecord {
    /// Stable caller-selected idempotency key.
    pub job_id: String,
    /// Immutable Genome identity captured at admission.
    pub genome_id: String,
    /// Deterministic underlying runtime identity.
    pub run_id: String,
    /// Exact Git commit paired with this run.
    pub source_revision: String,
    /// Immutable World identity bound at admission.
    pub world_id: String,
    /// Immutable task and input commitment bound at admission.
    pub task_id: String,
    /// BLAKE3 commitment to the exact reference-task input.
    pub input_commitment: String,
    /// Runtime-owned seed bound at admission.
    pub seed: u64,
    /// Worker and instruction-engine identity bound at admission.
    pub environment_id: String,
    /// Hard resource budget bound at admission.
    pub budget: RunBudgetReceipt,
    /// Current durable state.
    pub state: JobState,
    /// Fixed terminal outcome, present only after process confirmation.
    pub terminal: Option<JobTerminal>,
}

/// Live durable progress for one job, derived from its acknowledged trace events.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobProgress {
    /// Number of persisted runtime trace events.
    pub trace_events: u64,
    /// Sequence of the most recent trace event, when any exists.
    pub last_event_sequence: Option<u64>,
    /// Stable label for the most recent trace phase.
    pub last_phase: Option<String>,
}

/// Public progress for one admitted paired Arena evaluation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArenaJobProgress {
    /// Stable evaluation identity, also used as its idempotency key.
    pub evaluation_id: String,
    /// Immutable parent Genome identity.
    pub parent_genome_id: String,
    /// Immutable candidate Genome identity.
    pub candidate_genome_id: String,
    /// Current bounded lifecycle state.
    pub state: JobState,
    /// Coarse phase derived from durable events.
    pub phase: ArenaJobPhase,
    /// Number of candidate executions committed to canonical history.
    pub completed_trials: u32,
    /// Total candidate executions in the admitted plan.
    pub total_trials: u32,
    /// Candidate-safe evaluation output after its canonical receipt is recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evaluation: Option<EvaluationRecord>,
}

/// Coarse, payload-free paired Arena job phase.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArenaJobPhase {
    Preparing,
    ParentTrials,
    CandidateTrials,
    Scoring,
    Committing,
    Terminal,
}

/// Durable bounded-job lifecycle states.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Admitted,
    Running,
    CancellationRequested,
    Succeeded,
    Failed,
    Interrupted,
}

/// Terminal outcome persisted after supervised process termination.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobTerminal {
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

/// Candidate-safe visible aggregate and ledger metadata for one evaluation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRecord {
    /// Stable evaluation identity.
    pub evaluation_id: String,
    /// Exact immutable World identity shared by both Genomes.
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
    /// Payload-free canonical event metadata.
    pub event: EvaluationEventRecord,
}

/// Payload-free canonical ledger metadata for an evaluation receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationEventRecord {
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
    /// Caller-observed Unix timestamp in milliseconds.
    pub timestamp_millis: i64,
}

/// Operator-only full result of the trusted selection calculation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionRecord {
    /// Stable Arena evaluation identity.
    pub evaluation_id: String,
    /// Exact immutable registered World identity whose policy governed selection.
    pub world_id: String,
    /// Full deterministic receipt, including sealed-derived operator metrics and policy inputs.
    pub receipt: SelectionReceipt,
    /// Payload-free canonical event metadata binding the receipt artifact.
    pub event: SelectionEventRecord,
}

/// Kind of one deterministic Champion transition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChampionTransitionKind {
    /// Operator bootstrap of the first Champion of a World.
    Seeded,
    /// Evidence-backed replacement of the current Champion by a Forge child.
    Promoted,
    /// Restoration of the previous Champion; the replaced one is quarantined.
    RolledBack,
}

/// Exact evidence a promotion joined under the deterministic promotion policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChampionPromotionEvidence {
    /// Forge assessment whose metrics outcome passed.
    pub assessment_id: String,
    /// Exact Forge assessment event identity.
    pub assessment_event_id: String,
    /// Hash of the exact Forge assessment event.
    pub assessment_event_hash: String,
    /// Paired child evaluation shared by the assessment and invariant receipt.
    pub evaluation_id: String,
    /// Content address of the verified `SelectionReceipt`.
    pub selection_receipt_artifact_id: String,
    /// Exact invariant event identity for the same evaluation.
    pub invariant_event_id: String,
    /// Hash of the exact invariant event.
    pub invariant_event_hash: String,
    /// Content address of the verified `InvariantReceipt`.
    pub invariant_receipt_artifact_id: String,
}

/// Canonical payload of one Champion transition event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChampionTransitionPayload {
    /// Transition payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub transition_id: String,
    /// World whose Champion changed.
    pub world_id: String,
    /// Transition kind.
    pub kind: ChampionTransitionKind,
    /// Champion after this transition.
    pub champion_genome_id: String,
    /// Champion before this transition; absent only for a seed.
    pub previous_champion_genome_id: Option<String>,
    /// Prior transition of this World; absent only for a seed.
    pub previous_transition_event_id: Option<String>,
    /// Hash of the prior transition event; absent only for a seed.
    pub previous_transition_event_hash: Option<String>,
    /// Joined evidence; present exactly for a promotion.
    pub promotion: Option<ChampionPromotionEvidence>,
    /// Operator reason; present exactly for a seed or rollback.
    pub reason: Option<String>,
}

/// Canonical event metadata accompanying a Champion transition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChampionEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic idempotent event identity.
    pub event_id: String,
    /// Per-World Champion aggregate identity.
    pub aggregate_id: String,
    /// Event-chain hash.
    pub event_hash: String,
}

/// Operator-visible Champion transition and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChampionTransitionRecord {
    /// Canonical transition payload.
    pub payload: ChampionTransitionPayload,
    /// Canonical event metadata.
    pub event: ChampionEventRecord,
}

/// Champion projection of one World reconstructed from verified history.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChampionRecord {
    /// Registered World identity.
    pub world_id: String,
    /// Current Champion, absent until a seed.
    pub champion_genome_id: Option<String>,
    /// Superseded Champions that a rollback can restore, oldest first.
    pub standby_genome_ids: Vec<String>,
    /// Champions removed by rollback; they cannot be promoted again.
    pub quarantined_genome_ids: Vec<String>,
    /// Every transition of this World in ledger order.
    pub transitions: Vec<ChampionTransitionRecord>,
}

/// Operator-visible aggregate invariant receipt and its canonical event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvariantRecord {
    /// Deterministically recomputed aggregate predicates and counts.
    pub receipt: InvariantReceipt,
    /// Canonical event metadata and receipt content address.
    pub event: InvariantEvent,
}

/// Payload-free canonical ledger metadata for a selection receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic event identity.
    pub event_id: String,
    /// Stable evaluation selection aggregate identity.
    pub aggregate_id: String,
    /// Event type.
    pub event_type: String,
    /// Trusted actor.
    pub actor: String,
    /// Canonical event-chain hash.
    pub event_hash: String,
    /// BLAKE3 address of the canonical selection receipt.
    pub receipt_artifact_id: String,
}

/// Safe local API failure body.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    /// Stable error category.
    pub code: ApiErrorCode,
    /// Human-readable message without internal storage detail.
    pub message: String,
}

/// Stable fail-closed local API categories.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    /// The request used an unsupported protocol version.
    UnsupportedVersion,
    /// Authentication failed.
    Unauthorized,
    /// A required identifier was empty or absent.
    InvalidRequest,
    /// The requested canonical record does not exist.
    NotFound,
    /// Canonical persistence or projection verification failed.
    Internal,
    /// Another bounded job is already active, or request capacity is full.
    Busy,
}

#[cfg(test)]
mod tests {
    use super::{
        API_VERSION, ApiRequest, ApiResponse, Command, EvaluationEventRecord, EvaluationRecord,
        ResponseData,
    };

    #[test]
    fn paired_evaluation_command_has_a_stable_wire_shape() {
        let request = ApiRequest {
            version: API_VERSION,
            request_id: "request-1".to_owned(),
            token: "secret".to_owned(),
            command: Command::EvaluatePair {
                evaluation_id: "evaluation-1".to_owned(),
                parent_genome_id: "parent-1".to_owned(),
                candidate_genome_id: "candidate-1".to_owned(),
            },
        };

        let encoded = serde_json::to_value(&request).expect("request serializes");
        assert_eq!(
            encoded["command"],
            serde_json::json!({
                "command": "evaluate_pair",
                "evaluation_id": "evaluation-1",
                "parent_genome_id": "parent-1",
                "candidate_genome_id": "candidate-1"
            })
        );
        assert_eq!(
            serde_json::from_value::<ApiRequest>(encoded).expect("request deserializes"),
            request
        );
    }

    #[test]
    fn evaluation_response_contains_only_visible_aggregates_and_event_metadata() {
        let response = ApiResponse::success(
            "request-1".to_owned(),
            ResponseData::Evaluation {
                evaluation: EvaluationRecord {
                    evaluation_id: "evaluation-1".to_owned(),
                    world_id: "world-1".to_owned(),
                    parent_genome_id: "parent-1".to_owned(),
                    candidate_genome_id: "candidate-1".to_owned(),
                    parent_visible_correct: 2,
                    candidate_visible_correct: 3,
                    visible_total: 4,
                    event: EvaluationEventRecord {
                        sequence: 9,
                        event_id: "evaluation:evaluation-1:recorded".to_owned(),
                        aggregate_id: "evaluation:evaluation-1".to_owned(),
                        event_type: "evaluation.recorded".to_owned(),
                        actor: "arena-plane".to_owned(),
                        timestamp_millis: 1_234,
                    },
                },
            },
        );

        let encoded = serde_json::to_value(&response).expect("response serializes");
        assert_eq!(encoded["data"]["type"], "evaluation");
        assert_eq!(encoded["data"]["evaluation"]["visible_total"], 4);
        assert_eq!(encoded["data"]["evaluation"]["event"]["sequence"], 9);
        let text = serde_json::to_string(&encoded).expect("JSON value serializes");
        assert!(!text.contains("sealed"));
        assert!(!text.contains("expected_output"));
        assert!(!text.contains("artifact_id"));
        assert_eq!(
            serde_json::from_value::<ApiResponse>(encoded).expect("response deserializes"),
            response
        );
    }

    #[test]
    fn evaluation_response_rejects_unknown_evidence_fields() {
        let value = serde_json::json!({
            "version": API_VERSION,
            "request_id": "request-1",
            "data": {
                "type": "evaluation",
                "evaluation": {
                    "evaluation_id": "evaluation-1",
                    "world_id": "world-1",
                    "parent_genome_id": "parent-1",
                    "candidate_genome_id": "candidate-1",
                    "parent_visible_correct": 2,
                    "candidate_visible_correct": 3,
                    "visible_total": 4,
                    "sealed_total": 5,
                    "event": {
                        "sequence": 9,
                        "event_id": "evaluation:evaluation-1:recorded",
                        "aggregate_id": "evaluation:evaluation-1",
                        "event_type": "evaluation.recorded",
                        "actor": "arena-plane",
                        "timestamp_millis": 1234
                    }
                }
            },
            "error": null
        });

        assert!(serde_json::from_value::<ApiResponse>(value).is_err());
    }
}
