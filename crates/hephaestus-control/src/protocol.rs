use hephaestus_arena::{
    ClusterAnalysis, ClusterEvent, InvariantEvent, InvariantReceipt, SelectionReceipt,
};
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
    ///
    /// Exactly one of `hypothesis` or (`analysis_id` and `cluster_index`) must
    /// be supplied. The operator-hypothesis path is unchanged; supplying an
    /// analysis binding instead derives the hypothesis and confirms the
    /// mutation from one verified `forge.clustered` cluster.
    GenomePropose {
        /// Stable idempotency key for this proposal.
        proposal_id: String,
        /// Exact canonical selection event to which the proposal is bound.
        selection_event_id: String,
        /// Parent Genome; must be the selected candidate.
        parent_genome_id: String,
        /// Operator-authored, bounded hypothesis for the one prompt change.
        #[serde(default)]
        hypothesis: Option<String>,
        /// Verified `forge.clustered` analysis supplying the hypothesis and mutation.
        #[serde(default)]
        analysis_id: Option<String>,
        /// Index into that analysis's `clusters`, in canonical signature order.
        #[serde(default)]
        cluster_index: Option<u32>,
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
    /// Cluster one candidate's failed trials and suggest hypotheses and mutations.
    ForgeAnalyze {
        /// Stable idempotency key for this analysis.
        analysis_id: String,
        /// Stable Arena evaluation identity whose authenticated candidate trials are clustered.
        evaluation_id: String,
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
        /// Opt-in, recorded into the admission record: when true, each
        /// reference-role (role, task) trial is leased to a remote worker
        /// over `worker.sock` instead of executed in a locally sandboxed
        /// subprocess. A provider-role trial is unaffected either way.
        /// Defaults to `false` so every existing caller's wire shape is
        /// unchanged.
        #[serde(default)]
        remote: bool,
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
    /// Record one drift observation derived from verified evidence. Drift
    /// never directly replaces a Champion.
    DriftRecord {
        /// Stable caller-selected idempotency key.
        drift_id: String,
        /// Registered World the drift was observed under.
        world_id: String,
        /// Kind of shift the evidence must cite.
        kind: DriftKind,
        /// Evaluation identity supplying the cited `SelectionReceipt`.
        evidence_evaluation_id: String,
    },
    /// Inspect one recorded drift observation.
    DriftShow {
        /// Stable drift idempotency key.
        drift_id: String,
    },
    /// List recent drift records, newest first, bounded by `limit`.
    DriftList {
        /// Maximum number of entries returned; capped at 200.
        limit: u32,
    },
    /// Start (or idempotently re-admit) a staged canary rollout bound to a
    /// candidate Genome and a passing Forge assessment. The prior Champion
    /// stays Champion until the canary completes.
    CanaryStart {
        /// Stable caller-selected idempotency key for the canary.
        canary_id: String,
        /// Registered World the canary runs under.
        world_id: String,
        /// Candidate Genome this canary rolls out.
        candidate_genome_id: String,
        /// Durable Forge assessment whose parent is the current Champion and
        /// whose child is the candidate; the shadow evaluation of this canary.
        assessment_id: String,
    },
    /// Advance one canary on staged health evidence. A regression in the
    /// evidence automatically aborts the canary instead of advancing it.
    /// Refused while frozen unless the evidence shows a regression.
    CanaryAdvance {
        /// Stable canary idempotency key.
        canary_id: String,
        /// Evaluation identity supplying this stage's `SelectionReceipt`.
        evidence_evaluation_id: String,
    },
    /// Check one completed canary's Champion against the previous Champion.
    /// A regression automatically triggers a Champion rollback through the
    /// existing rollback transition. Allowed while frozen.
    CanaryLiveCheck {
        /// Stable canary idempotency key.
        canary_id: String,
        /// Evaluation identity supplying the live `SelectionReceipt`.
        evidence_evaluation_id: String,
    },
    /// Inspect one canary's projection and transition history.
    CanaryShow {
        /// Stable canary idempotency key.
        canary_id: String,
    },
    /// List recent canaries, newest first, bounded by `limit`.
    CanaryList {
        /// Maximum number of entries returned; capped at 200.
        limit: u32,
    },
    /// Extract a Gene from one promoted, evidence-bound Champion transition.
    GeneExtract {
        /// Stable idempotency key for this Gene.
        gene_id: String,
        /// Champion transition that promoted the origin child; must be `Promoted`.
        promotion_transition_id: String,
    },
    /// Apply one Gene's mutation to another lineage's Genome through the
    /// ordinary compiler, producing an unevaluated transfer child.
    GeneTransfer {
        /// Stable idempotency key for this transfer trial.
        trial_id: String,
        /// Gene being transferred.
        gene_id: String,
        /// Recipient Genome the Gene's mutation is applied to.
        to_genome_id: String,
    },
    /// Record one transfer trial's effect from a verified paired evaluation
    /// and selection of the recipient versus the transfer child.
    GeneRecord {
        /// Transfer trial being recorded; must already be applied.
        trial_id: String,
        /// Exact Arena evaluation identity of the recipient-versus-child pair.
        evaluation_id: String,
    },
    /// Inspect one Gene, its transfer trials, and any contradiction or species.
    GeneShow {
        /// Gene identity.
        gene_id: String,
    },
    /// List every extracted Gene with its aggregate transfer counts.
    GeneList,
    /// Create a specialist species from persistent, statistically significant
    /// domain advantage recorded across a Gene's transfer trials.
    GeneSpeciate {
        /// Stable idempotency key for this species.
        species_id: String,
        /// Gene whose domain advantage is being formalized.
        gene_id: String,
        /// Registered World (domain) the species specializes in.
        domain_world_id: String,
    },
    /// Start (or idempotently re-admit) an unattended, budget-bounded,
    /// multi-generation evolution run owned by the daemon's reconciliation loop.
    EvolveStart {
        /// Stable caller-selected idempotency key for the run.
        run_id: String,
        /// Registered World the run evolves within.
        world_id: String,
        /// Genome seeded (or already installed) as the World's Champion at generation zero.
        from_genome_id: String,
        /// Hard ceiling on the number of generations this run may complete.
        generations: u32,
        /// Hard ceiling on the number of paired Arena evaluations (trials) this run may submit.
        budget: u64,
        /// Optional registered Evolver strategy (roadmap item 13) steering
        /// which failure-cluster-suggested mutation each generation
        /// proposes. Without one, a generation falls back to today's
        /// default: the `identity`/`ascii_uppercase` flip when the Champion runs
        /// one of those two operations, otherwise no candidate.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        strategy_id: Option<String>,
    },
    /// Inspect one evolution run's durable, replay-verified progress.
    EvolveStatus {
        /// Stable run identity returned by `EvolveStart`.
        run_id: String,
    },
    /// Request cooperative cancellation of one active evolution run.
    EvolveCancel {
        /// Stable run identity returned by `EvolveStart`.
        run_id: String,
    },
    /// Register an Evolver strategy Genome: a versioned, content-addressed
    /// bundle of the knobs that steer the evolve engine's own admission
    /// policy (generation ceiling, paired-trial budget, and forward-looking
    /// prioritization fields). Registration never touches Laws, evaluators,
    /// receipts, or budgets; it only records the content and derives its
    /// identity, exactly like `GenomeRegister`.
    MetaStrategyRegister {
        /// Local path to a JSON [`EvolverStrategyConfig`] document.
        path: String,
    },
    /// Inspect one registered Evolver strategy Genome.
    MetaStrategyShow {
        /// Content-derived strategy identity.
        strategy_id: String,
    },
    /// List every registered Evolver strategy Genome, oldest first.
    MetaStrategyList,
    /// Run a paired meta-evaluation of two Evolver strategies over held-out
    /// base lineages, driving the existing evolve engine once per strategy
    /// per lineage, then record a replay-verified meta-receipt with a
    /// bootstrap confidence interval over the per-lineage paired deltas.
    MetaEvaluate {
        /// Stable caller-selected idempotency key for this meta-evaluation.
        meta_run_id: String,
        /// Registered Evolver strategy Genome, the "A" side of the comparison.
        strategy_a_id: String,
        /// Registered Evolver strategy Genome, the "B" side of the comparison.
        strategy_b_id: String,
        /// Held-out base lineages, each a registered World with its
        /// generation-zero Genome; every lineage's World must already carry
        /// a second registered Genome to serve as the evolve engine's fixed
        /// baseline, exactly like `EvolveStart`.
        lineages: Vec<MetaLineageSpec>,
        /// Bootstrap confidence, in basis points (for example `9_500` for 95%).
        confidence_bps: u16,
        /// Deterministic bootstrap resampling seed.
        bootstrap_seed: u64,
    },
    /// Inspect one meta-evaluation's durable, replay-verified receipt.
    MetaShow {
        /// Stable meta-evaluation identity returned by `MetaEvaluate`.
        meta_run_id: String,
    },
    /// List recent meta-evaluation receipts, newest first, bounded by `limit`.
    MetaList {
        /// Maximum number of entries returned; capped at 200.
        limit: u32,
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
    /// Route one versioned MCP tool call through the ordinary authenticated
    /// operator API. The MCP gateway makes the capability-policy decision
    /// before sending this; the daemon ledgers the decision unconditionally
    /// (including a denial) and, only when allowed, dispatches the wrapped
    /// command through this exact same authenticated path.
    McpCall {
        /// Stable identity of the connected MCP client, from its capability policy.
        client_id: String,
        /// Versioned tool name the client invoked.
        tool: String,
        /// Schema version of the invoked tool.
        tool_version: u16,
        /// The gateway's capability decision for this call.
        decision: McpDecision,
    },
    /// Mint one scoped, expiring credential for a remote worker. The raw
    /// token is returned exactly once and is never itself persisted; only
    /// its content-derived identity is ledgered.
    WorkerCredentialMint {
        /// Operator-chosen stable identity for the worker holding this credential.
        worker_id: String,
        /// Time-to-live for the minted credential, in seconds.
        ttl_seconds: u64,
    },
    /// Revoke one previously minted worker credential; fails closed on future use.
    WorkerCredentialRevoke {
        /// Content-derived credential identity returned by `WorkerCredentialMint`.
        credential_id: String,
    },
    /// Admit one bounded direct reference run for remote-worker execution.
    RemoteRunSubmit {
        /// Caller-selected idempotency key, unique for one immutable Genome.
        job_id: String,
        /// Content-derived registered Genome identity.
        genome_id: String,
    },
    /// Inspect one admitted remote-worker job.
    RemoteJobStatus {
        /// Stable job identity returned by `RemoteRunSubmit`.
        job_id: String,
    },
}

/// The MCP gateway's capability-policy decision for one tool call, ledgered
/// unconditionally as part of the wrapping `McpCall` command.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpDecision {
    /// The calling client's capability policy refused this tool call.
    Denied {
        /// Non-sensitive, stable reason the call was refused.
        reason: String,
    },
    /// The calling client's capability policy allowed this tool call; the
    /// wrapped command is dispatched through the ordinary authenticated path.
    Allowed {
        /// The exact operator command this tool call maps to.
        command: Box<Command>,
    },
}

/// Scope granted to a remote worker credential. This slice supports exactly
/// one job kind.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerScope {
    /// Lease and execute one direct reference run per job.
    RemoteReferenceRun,
}

/// Durable, replay-verified lifecycle state of one remote-worker job.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteJobState {
    /// Admitted and waiting for a worker to lease it.
    Pending,
    /// A worker has returned a signed successful result.
    Succeeded,
    /// A worker has returned a signed failed result.
    Failed,
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
    /// One deterministic failure-cluster analysis. Models may recommend a
    /// mutation from it; nothing here proposes, mutates, or promotes.
    ForgeAnalysis {
        /// Canonical clusters and their canonical event metadata.
        analysis: Box<ForgeAnalysisRecord>,
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
    /// One durable drift record derived from verified evidence.
    Drift {
        /// Drift payload and canonical event metadata.
        drift: Box<DriftRecord>,
    },
    /// Recent drift records, newest first and bounded.
    DriftList {
        /// Entries in newest-first order.
        drifts: Vec<DriftRecord>,
    },
    /// One durable, policy-checked canary transition.
    CanaryTransition {
        /// Transition payload and canonical event metadata.
        transition: Box<CanaryTransitionRecord>,
    },
    /// Canary projection reconstructed from verified history.
    Canary {
        /// Current stage and transition history.
        canary: Box<CanaryRecord>,
    },
    /// Recent canaries, newest first and bounded.
    CanaryList {
        /// Entries in newest-first order.
        canaries: Vec<CanaryRecord>,
    },
    /// One extracted Gene bound to its origin evidence.
    Gene {
        /// Canonical Gene payload and event metadata.
        gene: Box<GeneRecord>,
    },
    /// Every extracted Gene with its aggregate transfer counts.
    Genes {
        /// Canonical Gene summaries in ledger order.
        genes: Vec<GeneSummary>,
    },
    /// One Gene transfer trial: applied, and recorded once evaluated.
    GeneTransfer {
        /// Canonical transfer trial payload and event metadata.
        trial: Box<GeneTransferRecord>,
    },
    /// One specialist species created from persistent domain advantage.
    GeneSpecies {
        /// Canonical species payload and event metadata.
        species: Box<GeneSpeciesRecord>,
    },
    /// Full Gene aggregate: origin evidence, every transfer trial, any
    /// contradiction, and any species created from it.
    GeneAggregate {
        /// Canonical aggregate projection.
        aggregate: Box<GeneAggregateRecord>,
    },
    /// Durable, replay-verified progress of one evolution run.
    Evolution {
        /// Run configuration, completed generations, and terminal state.
        run: Box<EvolutionRunRecord>,
    },
    /// One registered Evolver strategy Genome.
    MetaStrategy {
        /// Canonical strategy content and event metadata.
        strategy: Box<MetaStrategyRecord>,
    },
    /// Every registered Evolver strategy Genome, oldest first.
    MetaStrategies {
        /// Canonical strategy records.
        strategies: Vec<MetaStrategyRecord>,
    },
    /// Durable, replay-verified receipt of one meta-evaluation.
    MetaEvaluation {
        /// Per-lineage outcomes and bootstrapped quality/cost deltas.
        receipt: Box<MetaReceiptRecord>,
    },
    /// Recent meta-evaluation receipts, newest first and bounded.
    MetaEvaluationList {
        /// Entries in newest-first order.
        receipts: Vec<MetaReceiptRecord>,
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
    /// An MCP tool call was refused by the gateway's capability policy.
    /// The `mcp.call` ledger event recording this denial always exists;
    /// no wrapped command was ever dispatched.
    McpDenied {
        /// Non-sensitive, stable reason the call was refused.
        reason: String,
    },
    /// One freshly minted remote worker credential. The raw `token` is
    /// returned exactly this once.
    WorkerCredential {
        /// Content-derived credential identity, safe to log and to revoke by.
        credential_id: String,
        /// Raw secret token; distribute it to the worker out of band.
        token: String,
        /// Operator-chosen worker identity this credential authenticates.
        worker_id: String,
        /// Absolute expiry, in milliseconds since the Unix epoch.
        expires_at_millis: i64,
        /// Granted scope.
        scope: WorkerScope,
    },
    /// Durable, replay-verified progress of one remote-worker job.
    RemoteJob {
        /// Stable job identity.
        job_id: String,
        /// Immutable Genome identity executed by the runtime.
        genome_id: String,
        /// Current durable state.
        state: RemoteJobState,
        /// Runtime-owned terminal reason, present only once a signed result exists.
        completion_reason: Option<RunCompletionReason>,
        /// Runtime-owned terminal latency, present only once a signed result exists.
        latency_millis: Option<u64>,
        /// CAS address of the bounded output, present only once a signed result exists.
        stdout_artifact_id: Option<String>,
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
    /// The MCP gateway's capability policy refused a tool call.
    McpCallDenied,
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
    /// Connected MCP client identity, present only for `McpCallDenied`.
    #[serde(default)]
    pub client_id: Option<String>,
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
    /// Explicit hypothesis: operator-authored, or derived from a bound cluster.
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
    /// The verified `forge.clustered` analysis and cluster this proposal was
    /// derived from, when one was supplied. Absent for an operator-authored
    /// hypothesis. This field was added after `schema_version` 1 shipped; it
    /// defaults to `None` so every previously recorded proposal event still
    /// replays byte-for-byte.
    #[serde(default)]
    pub analysis_binding: Option<ForgeAnalysisBinding>,
    /// The mutation catalog version this proposal's `operation_before` ->
    /// `operation_after` edge was classified under (roadmap items 8, 10,
    /// 13). This field was added after `schema_version` 1 shipped; it
    /// defaults to `None` (and is omitted from canonical bytes when absent)
    /// so every previously recorded proposal event still replays
    /// byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_version: Option<u16>,
    /// The catalog's classification of this edge (`"fix"`, `"regress"`,
    /// `"flip"`, or `"cross_family"`), recorded for operator visibility.
    /// Same backward-compatibility treatment as `catalog_version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_kind: Option<String>,
}

/// Binds one Forge proposal to the exact verified cluster analysis and
/// cluster it was derived from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAnalysisBinding {
    /// The bound analysis's idempotency key.
    pub analysis_id: String,
    /// Exact verified `forge.clustered` event identity.
    pub analysis_event_id: String,
    /// Hash of the exact analysis event.
    pub analysis_event_hash: String,
    /// Index into the analysis's `clusters`, in canonical signature order.
    pub cluster_index: u32,
    /// The bound cluster's stable signature, for operator-visible confirmation.
    pub cluster_signature: String,
}

/// Operator-visible cluster analysis and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeAnalysisRecord {
    /// Canonical clustered failure analysis.
    pub analysis: ClusterAnalysis,
    /// Canonical event metadata.
    pub event: ClusterEvent,
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

/// Kind of environmental shift a drift record cites.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftKind {
    /// Aggregate terminal latency shifted beyond the documented threshold.
    Latency,
    /// Aggregate provider cost shifted beyond the documented threshold.
    Cost,
    /// Paired correctness fitness shifted beyond the documented threshold.
    Correctness,
    /// Reliable-trial proportion shifted beyond the documented threshold,
    /// the signal used to detect workload-induced drift.
    Workload,
}

/// Canonical payload of one `drift.recorded` event. Drift never directly
/// replaces a Champion; it only cites verified evidence that a fixed,
/// documented threshold was crossed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriftRecordPayload {
    /// Drift payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub drift_id: String,
    /// World the drift was observed under.
    pub world_id: String,
    /// Kind of shift this record cites.
    pub kind: DriftKind,
    /// Evaluation identity supplying the cited `SelectionReceipt`.
    pub evidence_evaluation_id: String,
    /// Exact verified selection event identity.
    pub selection_event_id: String,
    /// Hash of the exact verified selection event.
    pub selection_event_hash: String,
    /// The World's Champion at the time of this record; the evidence's parent
    /// (baseline) side.
    pub baseline_genome_id: String,
    /// The evidence's candidate side, whose metrics are compared to the baseline.
    pub shifted_genome_id: String,
    /// Fixed, documented threshold in basis points this record crossed.
    pub threshold_bps: u32,
    /// Signed measured shift in basis points for the cited kind; a magnitude
    /// at or beyond `threshold_bps` in the regressive direction is required.
    pub observed_delta_bps: i64,
}

/// Canonical event metadata accompanying a drift record.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriftEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic idempotent event identity.
    pub event_id: String,
    /// Per-World drift aggregate identity.
    pub aggregate_id: String,
    /// Event-chain hash.
    pub event_hash: String,
}

/// Operator-visible drift record and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DriftRecord {
    /// Canonical drift payload.
    pub payload: DriftRecordPayload,
    /// Canonical event metadata.
    pub event: DriftEventRecord,
}

/// Staged rollout progress of one canary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanaryStage {
    /// Bound to a candidate and assessment; not yet receiving traffic.
    Pending,
    /// Advanced on healthy evidence to 5%.
    Stage5,
    /// Advanced on healthy evidence to 25%.
    Stage25,
    /// Advanced on healthy evidence to 50%.
    Stage50,
    /// Reached 100% and promoted the candidate through the Champion path.
    Completed,
    /// Automatically aborted after regression evidence at a staged health gate.
    Aborted,
}

/// Kind of one canary transition event.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanaryTransitionKind {
    /// Canary bound to a candidate and a passing Forge assessment.
    Started,
    /// Staged health evidence admitted advancement to the next stage.
    Advanced,
    /// Staged health evidence showed a regression; the canary aborted automatically.
    Aborted,
    /// Live evidence after completion showed the Champion regressed against
    /// the previous Champion; this automatically triggered a Champion rollback.
    LiveRegressionDetected,
}

/// Measured staged or live-check evidence joined into one canary transition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanaryEvidence {
    /// Evaluation identity supplying the cited `SelectionReceipt`.
    pub evidence_evaluation_id: String,
    /// Exact verified selection event identity.
    pub selection_event_id: String,
    /// Hash of the exact verified selection event.
    pub selection_event_hash: String,
    /// Signed latency shift in basis points (candidate vs. parent; positive is worse).
    pub latency_delta_bps: i64,
    /// Signed cost shift in basis points (candidate vs. parent; positive is worse).
    pub cost_delta_bps: i64,
    /// Signed correctness shift in basis points (candidate vs. parent; negative is worse).
    pub correctness_delta_bps: i64,
    /// Signed reliability shift in basis points (candidate vs. parent; negative is worse).
    pub reliability_delta_bps: i64,
    /// Whether any measured dimension crossed its fixed, documented regression threshold.
    pub regressed: bool,
}

/// Canonical payload of one canary transition event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanaryTransitionPayload {
    /// Transition payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key for the canary.
    pub canary_id: String,
    /// World the canary runs under.
    pub world_id: String,
    /// Transition kind.
    pub kind: CanaryTransitionKind,
    /// Stage after this transition.
    pub stage: CanaryStage,
    /// Candidate Genome this canary is rolling out.
    pub candidate_genome_id: String,
    /// The Champion immediately before this canary started.
    pub previous_champion_genome_id: String,
    /// Durable Forge assessment bound at `Started`, reused unchanged for the
    /// Champion promotion evidence at completion.
    pub assessment_id: String,
    /// Staged or live-check evidence; present for `Advanced`, `Aborted`, and
    /// `LiveRegressionDetected`.
    pub evidence: Option<CanaryEvidence>,
    /// Champion promotion evidence, present exactly when `stage` becomes `Completed`.
    pub champion_promotion: Option<ChampionPromotionEvidence>,
    /// Exact Champion rollback transition event this triggered, present exactly
    /// for `LiveRegressionDetected`.
    pub champion_rollback_event_id: Option<String>,
    /// Hash of the exact Champion rollback transition event.
    pub champion_rollback_event_hash: Option<String>,
    /// Deterministic reason, present exactly for `Aborted` and `LiveRegressionDetected`.
    pub reason: Option<String>,
}

/// Canonical event metadata accompanying a canary transition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanaryEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic idempotent event identity.
    pub event_id: String,
    /// Per-canary aggregate identity.
    pub aggregate_id: String,
    /// Event-chain hash.
    pub event_hash: String,
}

/// Operator-visible canary transition and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanaryTransitionRecord {
    /// Canonical transition payload.
    pub payload: CanaryTransitionPayload,
    /// Canonical event metadata.
    pub event: CanaryEventRecord,
}

/// Canary projection reconstructed from verified history.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CanaryRecord {
    /// Stable caller-selected idempotency key for the canary.
    pub canary_id: String,
    /// World the canary runs under.
    pub world_id: String,
    /// Candidate Genome this canary is rolling out.
    pub candidate_genome_id: String,
    /// The Champion immediately before this canary started.
    pub previous_champion_genome_id: String,
    /// Current stage.
    pub stage: CanaryStage,
    /// Every transition of this canary in ledger order.
    pub transitions: Vec<CanaryTransitionRecord>,
}

/// Canonical event metadata shared by every Gene Bank event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic idempotent event identity.
    pub event_id: String,
    /// Gene Bank aggregate identity.
    pub aggregate_id: String,
    /// Event-chain hash.
    pub event_hash: String,
}

/// Canonical payload of one `gene.extracted` event. A Gene is the minimal
/// mutation (reference operation flip) plus its origin evidence and
/// World/domain scope; it can only be extracted from a promoted,
/// evidence-bound Champion transition that meets the deterministic minimum
/// evidence threshold.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneExtractedPayload {
    /// Gene payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub gene_id: String,
    /// The Champion promotion this Gene was extracted from.
    pub promotion_transition_id: String,
    /// Exact Champion transition event identity and hash.
    pub promotion_event_id: String,
    /// Hash of the exact Champion transition event.
    pub promotion_event_hash: String,
    /// Forge assessment joined by the promotion.
    pub assessment_id: String,
    /// Exact Forge assessment event identity and hash.
    pub assessment_event_id: String,
    /// Hash of the exact Forge assessment event.
    pub assessment_event_hash: String,
    /// Forge proposal that produced the origin child.
    pub proposal_id: String,
    /// Exact Forge proposal event identity and hash.
    pub proposal_event_id: String,
    /// Hash of the exact Forge proposal event.
    pub proposal_event_hash: String,
    /// Exact verified child-selection event identity and hash.
    pub selection_event_id: String,
    /// Hash of the exact verified selection event.
    pub selection_event_hash: String,
    /// Exact invariant event identity and hash joined by the promotion.
    pub invariant_event_id: String,
    /// Hash of the exact invariant event.
    pub invariant_event_hash: String,
    /// World/domain this Gene was extracted under.
    pub world_id: String,
    /// Origin parent Genome (pre-mutation).
    pub origin_parent_genome_id: String,
    /// Origin child Genome (post-mutation, promoted to Champion).
    pub origin_child_genome_id: String,
    /// Reference operation before the mutation.
    pub operation_before: String,
    /// Reference operation after the mutation.
    pub operation_after: String,
    /// Measured paired trial count backing the origin promotion.
    pub evidence_trials: u32,
    /// Deterministic minimum paired trial count a Gene requires.
    pub evidence_threshold: u32,
}

/// Operator-visible Gene and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneRecord {
    /// Canonical Gene payload.
    pub payload: GeneExtractedPayload,
    /// Canonical event metadata.
    pub event: GeneEventRecord,
}

/// Measured effect of one transfer trial, classified from the paired
/// selection receipt's confidence bounds on the correctness delta.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneTransferOutcome {
    /// The lower confidence bound of the correctness delta is above zero.
    Positive,
    /// Neither bound crosses zero: no statistically significant effect.
    Neutral,
    /// The upper confidence bound of the correctness delta is below zero.
    /// Negative transfer is retained, never dropped.
    Negative,
}

/// Canonical payload of one `gene.transfer_applied` event: a Gene's mutation
/// compiled onto a recipient Genome through the ordinary compiler.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneTransferAppliedPayload {
    /// Transfer payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub trial_id: String,
    /// Gene being transferred.
    pub gene_id: String,
    /// Exact Gene event identity and hash this trial is bound to.
    pub gene_event_id: String,
    /// Hash of the exact Gene event.
    pub gene_event_hash: String,
    /// Recipient Genome the Gene's mutation was applied to.
    pub to_genome_id: String,
    /// World/domain of the recipient lineage.
    pub world_id: String,
    /// Compiled, registered transfer child.
    pub child: GenomeRecord,
    /// Exact recipient prompt artifact address before the mutation.
    pub prompt_artifact_before: String,
    /// Exact transfer child prompt artifact address after the mutation.
    pub prompt_artifact_after: String,
}

/// Canonical payload of one `gene.transfer_recorded` event: the measured
/// effect of a transfer trial from a verified paired evaluation and selection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneTransferRecordedPayload {
    /// Transfer payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key, shared with the applied event.
    pub trial_id: String,
    /// Gene being transferred.
    pub gene_id: String,
    /// Exact `gene.transfer_applied` event identity and hash.
    pub applied_event_id: String,
    /// Hash of the exact `gene.transfer_applied` event.
    pub applied_event_hash: String,
    /// Stable Arena evaluation identity of the recipient-versus-child pair.
    pub evaluation_id: String,
    /// Exact verified selection event identity and hash.
    pub selection_event_id: String,
    /// Hash of the exact verified selection event.
    pub selection_event_hash: String,
    /// Content address of the verified `SelectionReceipt`.
    pub selection_receipt_artifact_id: String,
    /// Measured effect classification.
    pub outcome: GeneTransferOutcome,
    /// Paired correctness mean estimate in basis points.
    pub estimate_bps: i64,
    /// Lower confidence bound in basis points.
    pub lower_bps: i64,
    /// Upper confidence bound in basis points.
    pub upper_bps: i64,
}

/// One Gene transfer trial: applied, and recorded once a paired evaluation
/// and selection exist for it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneTransferRecord {
    /// Canonical applied payload.
    pub applied: GeneTransferAppliedPayload,
    /// Canonical applied event metadata.
    pub applied_event: GeneEventRecord,
    /// Canonical recorded payload and event metadata, present once recorded.
    pub recorded: Option<GeneTransferRecordedPayload>,
    /// Canonical recorded event metadata, present once recorded.
    pub recorded_event: Option<GeneEventRecord>,
}

/// Canonical payload of one `gene.contradiction` event: a Gene measured
/// positive in one lineage and negative in another. Recorded once per Gene
/// and never overwritten.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneContradictionPayload {
    /// Contradiction payload schema.
    pub schema_version: u16,
    /// Gene the contradiction was detected for.
    pub gene_id: String,
    /// First positive transfer trial, in ledger order.
    pub positive_trial_id: String,
    /// World/domain of the positive trial's recipient lineage.
    pub positive_world_id: String,
    /// Exact `gene.transfer_recorded` event identity and hash of the positive trial.
    pub positive_event_id: String,
    /// Hash of the positive trial's recorded event.
    pub positive_event_hash: String,
    /// First negative transfer trial, in ledger order.
    pub negative_trial_id: String,
    /// World/domain of the negative trial's recipient lineage.
    pub negative_world_id: String,
    /// Exact `gene.transfer_recorded` event identity and hash of the negative trial.
    pub negative_event_id: String,
    /// Hash of the negative trial's recorded event.
    pub negative_event_hash: String,
}

/// Operator-visible Gene contradiction and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneContradictionRecord {
    /// Canonical contradiction payload.
    pub payload: GeneContradictionPayload,
    /// Canonical event metadata.
    pub event: GeneEventRecord,
}

/// Canonical payload of one `gene.species_created` event: a specialist
/// species admitted only from persistent, statistically significant domain
/// advantage recorded across a Gene's transfer trials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneSpeciesPayload {
    /// Species payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub species_id: String,
    /// Gene this species specializes.
    pub gene_id: String,
    /// Exact Gene event identity and hash.
    pub gene_event_id: String,
    /// Hash of the exact Gene event.
    pub gene_event_hash: String,
    /// World/domain this species specializes in.
    pub domain_world_id: String,
    /// Distinct recipient lineages (Genome identities) supporting the advantage.
    pub lineage_genome_ids: Vec<String>,
    /// Exact `gene.transfer_recorded` event identities and hashes supporting the advantage.
    pub supporting_event_ids: Vec<String>,
    /// Mean measured effect across supporting trials, in basis points.
    pub average_estimate_bps: i64,
    /// Deterministic minimum distinct-lineage count required.
    pub minimum_lineages: u32,
    /// Deterministic minimum mean effect size required, in basis points.
    pub minimum_effect_bps: i64,
}

/// Operator-visible species and its durable event identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneSpeciesRecord {
    /// Canonical species payload.
    pub payload: GeneSpeciesPayload,
    /// Canonical event metadata.
    pub event: GeneEventRecord,
}

/// Aggregate transfer counts for one Gene, reported across every recorded
/// transfer trial regardless of lineage count.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneSummary {
    /// Canonical Gene payload.
    pub payload: GeneExtractedPayload,
    /// Canonical event metadata.
    pub event: GeneEventRecord,
    /// Distinct recipient lineages with a recorded transfer trial.
    pub lineages: u32,
    /// Recorded trials classified positive.
    pub positive: u32,
    /// Recorded trials classified neutral.
    pub neutral: u32,
    /// Recorded trials classified negative.
    pub negative: u32,
    /// Whether a contradiction has been recorded for this Gene.
    pub contradiction: bool,
    /// Species created from this Gene, if any.
    pub species_ids: Vec<String>,
}

/// Full Gene aggregate: origin evidence, every transfer trial in ledger
/// order, any contradiction, and any species created from it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneAggregateRecord {
    /// The extracted Gene.
    pub gene: GeneRecord,
    /// Every transfer trial in ledger order.
    pub transfers: Vec<GeneTransferRecord>,
    /// The Gene's contradiction record, if one has been detected.
    pub contradiction: Option<GeneContradictionRecord>,
    /// Species created from this Gene, in ledger order.
    pub species: Vec<GeneSpeciesRecord>,
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

/// Durable lifecycle of one autonomous evolution run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionRunState {
    /// The daemon's reconciliation loop is still admitted to advance this run.
    Running,
    /// The run reached a terminal state; `finish_reason` explains why.
    Finished,
}

/// Why one evolution run stopped advancing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvolutionFinishReason {
    /// The run completed its configured `max_generations`.
    GenerationsExhausted,
    /// The run would have exceeded its configured `max_paired_trials`.
    BudgetExhausted,
    /// An operator requested cancellation through `EvolveCancel`.
    Cancelled,
    /// A generation's paired evaluation did not terminate successfully
    /// (for example, a daemon restart recovered it as failed or interrupted).
    Interrupted,
    /// A strategy-bound run's failure-cluster analysis suggested no
    /// mutation for the current Champion and the Champion is not the
    /// casing pair (so no default fallback flip applies either); the run
    /// stops rather than proposing an unfounded mutation (roadmap items 8,
    /// 10, 13).
    NoCandidateMutation,
}

/// Canonical payload of the one `evolution.started` event for a run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionStartedPayload {
    /// Run payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key.
    pub run_id: String,
    /// Registered World the run evolves within.
    pub world_id: String,
    /// Genome installed as generation zero's Champion.
    pub from_genome_id: String,
    /// Registered comparison Genome used as the fixed "parent" side of every
    /// generation's diagnostic evaluation. Chosen deterministically as the
    /// lexicographically smallest other Genome registered under the World.
    pub baseline_genome_id: String,
    /// Hard ceiling on the number of generations this run may complete.
    pub max_generations: u32,
    /// Hard ceiling on the number of paired Arena evaluations (trials) this run may submit.
    pub max_paired_trials: u64,
    /// Registered Evolver strategy (roadmap item 13) steering this run's
    /// mutation choice, if one was bound at start. This field was added
    /// after `schema_version` 1 shipped; it defaults to `None` (and is
    /// omitted from canonical bytes when absent) so every previously
    /// recorded `evolution.started` event still replays byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
}

/// Canonical payload of one `evolution.generation` event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionGenerationPayload {
    /// Generation payload schema.
    pub schema_version: u16,
    /// Owning run identity.
    pub run_id: String,
    /// Zero-based index of this generation within the run.
    pub generation_index: u32,
    /// World Champion at the start of this generation.
    pub champion_before: String,
    /// Paired evaluation identity establishing `champion_before` as the
    /// selected candidate that Forge mutates.
    pub diagnostic_evaluation_id: String,
    /// Durable Forge proposal mutating `champion_before`.
    pub proposal_id: String,
    /// Proposed child Genome identity.
    pub child_genome_id: String,
    /// Paired evaluation identity of `champion_before` versus `child_genome_id`.
    pub child_evaluation_id: String,
    /// Evidence-only Forge assessment of the child against the Champion.
    pub assessment_id: String,
    /// Whether the assessed child was promoted to Champion.
    pub promoted: bool,
    /// World Champion after this generation (equal to `champion_before` unless promoted).
    pub champion_after: String,
}

/// Canonical payload of the one `evolution.finished` event for a run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionFinishedPayload {
    /// Finish payload schema.
    pub schema_version: u16,
    /// Owning run identity.
    pub run_id: String,
    /// Number of generations recorded before this run stopped.
    pub generations_completed: u32,
    /// Number of paired Arena evaluations (trials) this run consumed.
    pub trials_consumed: u64,
    /// Why the run stopped.
    pub reason: EvolutionFinishReason,
}

/// Canonical payload of one `evolution.cancel_requested` event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionCancelPayload {
    /// Cancel payload schema.
    pub schema_version: u16,
    /// Owning run identity.
    pub run_id: String,
}

/// Payload-free canonical ledger metadata for one evolution event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic idempotent event identity.
    pub event_id: String,
    /// Per-run evolution aggregate identity.
    pub aggregate_id: String,
    /// Stable event type.
    pub event_type: String,
    /// Fixed trusted actor.
    pub actor: String,
    /// Canonical event-chain hash.
    pub event_hash: String,
}

/// One completed generation, operator-visible.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionGenerationRecord {
    /// Canonical generation payload.
    pub payload: EvolutionGenerationPayload,
    /// Canonical event metadata.
    pub event: EvolutionEventRecord,
}

/// Durable, replay-verified projection of one evolution run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionRunRecord {
    /// Stable caller-selected idempotency key.
    pub run_id: String,
    /// Registered World the run evolves within.
    pub world_id: String,
    /// Genome installed as generation zero's Champion.
    pub from_genome_id: String,
    /// Fixed comparison Genome used by every generation's diagnostic evaluation.
    pub baseline_genome_id: String,
    /// Hard ceiling on the number of generations this run may complete.
    pub max_generations: u32,
    /// Hard ceiling on the number of paired Arena evaluations (trials) this run may submit.
    pub max_paired_trials: u64,
    /// Registered Evolver strategy steering this run's mutation choice, if
    /// one was bound at start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_id: Option<String>,
    /// Paired Arena evaluations (trials) consumed by completed generations.
    pub trials_consumed: u64,
    /// Current lifecycle state.
    pub state: EvolutionRunState,
    /// Whether an operator has requested cancellation.
    pub cancel_requested: bool,
    /// Present once `state` is `Finished`.
    pub finish_reason: Option<EvolutionFinishReason>,
    /// Completed generations, oldest first.
    pub generations: Vec<EvolutionGenerationRecord>,
    /// Canonical event metadata for `evolution.started`.
    pub started_event: EvolutionEventRecord,
    /// Canonical event metadata for `evolution.finished`, once finished.
    pub finished_event: Option<EvolutionEventRecord>,
}

// ---------------------------------------------------------------------------
// Recursive evolution of the Evolver (roadmap item 13).
//
// An Evolver strategy is a versioned, content-addressed bundle of the knobs
// that steer the evolve engine's own admission policy. It cannot name or
// alter a Law, an evaluator, a receipt, or a World's budget ceiling, because
// no such field exists on `EvolverStrategyConfig`: the invariant is enforced
// by construction, not by a runtime check. A meta-evaluation runs the
// existing, unmodified evolve engine once per strategy over each of a set of
// held-out base lineages (a registered World plus its generation-zero
// Genome) and records one replay-verified receipt comparing the Champion
// quality reached and the paired-trial cost spent, with a bootstrap
// confidence interval over the per-lineage deltas.
// ---------------------------------------------------------------------------

/// Deterministic priority order Forge considers candidate mutations in.
/// Only the reference-operation-flip mutation exists today, so every
/// variant currently produces identical Forge behavior; the field is
/// recorded now so a richer Forge can read it later without a schema change.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationPrioritization {
    /// Consider the oldest unresolved failure cluster first.
    Fifo,
    /// Consider the highest-cost failure cluster first.
    CostWeighted,
}

/// Gene Bank selection policy for seeding candidate mutations. Not yet wired
/// into Forge; recorded for forward compatibility.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneSelectionPolicy {
    /// Do not consult the Gene Bank.
    None,
    /// Prefer the Gene with the highest recorded transfer effect.
    HighestTransferEffect,
}

/// Versioned, content-addressed configuration of one Evolver strategy.
/// Registering a strategy never grants it any authority beyond these
/// fields.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvolverStrategyConfig {
    /// Strategy payload schema.
    pub schema_version: u16,
    /// Operator-authored label; part of the content identity but not
    /// otherwise interpreted.
    pub name: String,
    /// Order Forge considers candidate failure clusters in. See
    /// [`MutationPrioritization`].
    pub mutation_prioritization: MutationPrioritization,
    /// Passed straight through as `EvolveStart.generations` for every
    /// lineage this strategy evaluates.
    pub generation_count: u32,
    /// Passed straight through as `EvolveStart.budget` for every lineage
    /// this strategy evaluates.
    pub experiment_allocation: u64,
    /// Candidate mutations considered per generation. Forge proposes exactly
    /// one mutation today, so values above `1` are recorded but not yet
    /// actionable.
    pub candidate_count: u32,
    /// Gene Bank selection policy. See [`GeneSelectionPolicy`].
    pub gene_selection: GeneSelectionPolicy,
    /// Identity of the Evolver strategy Genome this strategy declares itself
    /// a descendant of, if any. Part of this strategy's own content
    /// identity: two strategies with identical knobs but different (or
    /// absent) `parent_strategy_id` are distinct registrations. The
    /// referenced strategy must already be registered at the time this one
    /// is registered, and every replay re-checks that lineage edge against
    /// the same history that preceded it. Declaring a parent grants no
    /// additional authority; it is bookkeeping only.
    #[serde(default)]
    pub parent_strategy_id: Option<String>,
}

/// Canonical payload of the one `meta_strategy.registered` event for a
/// strategy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaStrategyRegisteredPayload {
    /// Registration payload schema.
    pub schema_version: u16,
    /// Content-derived strategy identity.
    pub strategy_id: String,
    /// Exact registered configuration.
    pub config: EvolverStrategyConfig,
}

/// Durable, replay-verified projection of one registered Evolver strategy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaStrategyRecord {
    /// Content-derived strategy identity.
    pub strategy_id: String,
    /// Exact registered configuration.
    pub config: EvolverStrategyConfig,
    /// Canonical event metadata for `meta_strategy.registered`.
    pub event: EvolutionEventRecord,
}

/// One held-out lineage: a registered World plus the Genome installed as
/// generation zero's Champion for it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaLineageSpec {
    /// Registered World this held-out lineage evolves within.
    pub world_id: String,
    /// Genome installed as generation zero's Champion for this lineage.
    pub from_genome_id: String,
}

/// One held-out lineage's paired outcome: the same starting Genome and
/// World evaluated once by each strategy, using the existing evolve engine
/// unmodified. `promotions` is a coarse Champion-quality proxy (generations
/// where the strategy's run actually promoted a child); with only the
/// reference-operation-flip mutation available, this is the most Forge can
/// currently distinguish.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaLineageOutcome {
    /// Registered World this lineage evolves within.
    pub world_id: String,
    /// Genome installed as generation zero's Champion for this lineage.
    pub from_genome_id: String,
    /// Underlying evolve run identity strategy A completed for this lineage.
    pub strategy_a_run_id: String,
    /// Underlying evolve run identity strategy B completed for this lineage.
    pub strategy_b_run_id: String,
    /// Champion reached at the end of strategy A's run.
    pub strategy_a_champion_genome_id: String,
    /// Champion reached at the end of strategy B's run.
    pub strategy_b_champion_genome_id: String,
    /// Generations promoted during strategy A's run.
    pub strategy_a_promotions: u32,
    /// Generations promoted during strategy B's run.
    pub strategy_b_promotions: u32,
    /// Paired Arena evaluations (trials) strategy A's run consumed.
    pub strategy_a_trials_consumed: u64,
    /// Paired Arena evaluations (trials) strategy B's run consumed.
    pub strategy_b_trials_consumed: u64,
}

/// Bootstrap confidence interval over a paired per-lineage delta, scaled by
/// `10_000` for one fixed-point fractional digit of precision (the same
/// scaling convention as the Arena selection receipt's basis points, but
/// over a raw count rather than a ratio).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaBootstrapInterval {
    /// Sample mean of the paired deltas, times `10_000`.
    pub estimate_x10000: i64,
    /// Lower confidence bound, times `10_000`.
    pub lower_x10000: i64,
    /// Upper confidence bound, times `10_000`.
    pub upper_x10000: i64,
}

/// Canonical payload of the one `meta_evolution.evaluated` event for a
/// meta-evaluation run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaEvaluationPayload {
    /// Evaluation payload schema.
    pub schema_version: u16,
    /// Stable caller-selected idempotency key for this meta-evaluation.
    pub meta_run_id: String,
    /// Registered Evolver strategy Genome, the "A" side of the comparison.
    pub strategy_a_id: String,
    /// Registered Evolver strategy Genome, the "B" side of the comparison.
    pub strategy_b_id: String,
    /// Bootstrap confidence, in basis points.
    pub confidence_bps: u16,
    /// Deterministic bootstrap resampling seed.
    pub bootstrap_seed: u64,
    /// Number of bootstrap resamples.
    pub bootstrap_resamples: u32,
    /// Versioned deterministic bootstrap algorithm identity.
    pub algorithm: String,
    /// Every held-out lineage's paired outcome, in request order.
    pub lineages: Vec<MetaLineageOutcome>,
    /// Champion-quality delta (strategy B minus strategy A), bootstrapped
    /// over lineages.
    pub quality_delta: MetaBootstrapInterval,
    /// Experiment-cost delta in paired trials (strategy B minus strategy A),
    /// bootstrapped over lineages.
    pub cost_delta: MetaBootstrapInterval,
    /// Set only when exactly one of the two compared strategies declares
    /// the other as its `parent_strategy_id`. `Some(true)` means the
    /// descendant reached an equal-or-better Champion (its bootstrapped
    /// quality delta, oriented descendant-minus-ancestor, has a lower bound
    /// `>= 0`) at a statistically lower experiment cost (its bootstrapped
    /// cost delta, oriented descendant-minus-ancestor, has an upper bound
    /// `< 0`). `None` when neither strategy is the other's declared parent,
    /// so no lineage claim applies. Recomputed and cross-checked from
    /// `quality_delta`/`cost_delta` and the two strategies' recorded
    /// configs on every replay.
    pub descendant_cheaper_at_equal_quality: Option<bool>,
}

/// Durable, replay-verified projection of one meta-evaluation receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetaReceiptRecord {
    /// Exact recorded evaluation payload.
    pub payload: MetaEvaluationPayload,
    /// Canonical event metadata for `meta_evolution.evaluated`.
    pub event: EvolutionEventRecord,
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
                remote: false,
            },
        };

        let encoded = serde_json::to_value(&request).expect("request serializes");
        assert_eq!(
            encoded["command"],
            serde_json::json!({
                "command": "evaluate_pair",
                "evaluation_id": "evaluation-1",
                "parent_genome_id": "parent-1",
                "candidate_genome_id": "candidate-1",
                "remote": false
            })
        );
        assert_eq!(
            serde_json::from_value::<ApiRequest>(encoded).expect("request deserializes"),
            request
        );
        // An old caller's request, omitting `remote` entirely, must still
        // deserialize and default to local (non-remote) execution.
        let legacy = serde_json::json!({
            "version": API_VERSION,
            "request_id": "request-2",
            "token": "secret",
            "command": {
                "command": "evaluate_pair",
                "evaluation_id": "evaluation-1",
                "parent_genome_id": "parent-1",
                "candidate_genome_id": "candidate-1"
            }
        });
        let decoded_legacy =
            serde_json::from_value::<ApiRequest>(legacy).expect("legacy request deserializes");
        assert_eq!(decoded_legacy.command, request.command);
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
