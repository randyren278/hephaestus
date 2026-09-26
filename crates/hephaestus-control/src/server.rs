use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
#[cfg(test)]
use hephaestus_arena::load_recorded_evaluation;
use hephaestus_arena::{
    ArenaError, CLUSTER_EVENT_PREFIX, ClusterAnalysis, ClusterEvent, EvaluationBinding,
    EvaluationInputs, EvaluationSources, EvaluationStores, FailureCluster, InvariantEvent,
    InvariantReceipt, IsolatedEvaluator, OperatorClusterAnalysis, OperatorInvariantCheck,
    ReceiptContext, ScoredEvaluation, SelectionEvent, SelectionReceipt, SuggestedMutation,
    TrialPlan, TrustedManifest, Visibility, check_failure_clusters,
    check_reference_output_invariants, cluster_event_references, evaluate_and_record_scored,
    invariant_event_references, load_failure_clusters, load_operator_evaluation,
    load_recorded_evaluation_in, load_reference_output_invariants, prepare_evaluation,
    select_and_record, selection_event_references, verify_cluster_event_in,
    verify_reference_output_invariant_event, verify_reference_output_invariant_event_in,
    verify_selection_event, verify_selection_event_in,
};
use hephaestus_core::authority::{CapabilitySet, FreezeState, OperatorToken};
use hephaestus_core::domain::MutationTarget;
use hephaestus_experience::{
    EvidenceRecorder, EvidenceRequest, RUN_RESULT_SCHEMA_VERSION, RecordedRuntime, RedactionPolicy,
    RetentionLimits, RunBudgetReceipt, RunResultReceipt, RunResultSigner, RunResultVerifier,
    TraceKind, TraceReceipt, count_records_per_run,
};
use hephaestus_genome::{
    CompiledGenome, CompiledWorld, RegisteredObjects, RegistrationError, SourceFormat,
    compile_genome, compile_markdown_genome, compile_world,
};
use hephaestus_ledger::{
    ArtifactBackend, ArtifactId, ArtifactStore, EventIndex, EventInput, EventLedger, EventStore,
    StoredEvent,
};
#[cfg(test)]
use hephaestus_ledger::{FileEventLedger, MemoryArtifactBackend};
use hephaestus_runtime::{
    Budget, CapabilityToken, CompletionReason, DeterministicRuntime, ExperimentContext,
    IsolationPolicy, MUTATION_CATALOG_VERSION, Provider, ReferenceInstruction, RunSpec, RunStatus,
    RuntimeAdapter, Sandbox, SandboxManager, SupervisedRuntime, WorkerLimits,
    extract_actual_cost_microusd, extract_final_answer, is_catalog_edge, mutation_edge_kind,
};
use serde::{Deserialize, Serialize};
use tempfile::{Builder as TempDirBuilder, TempDir};

use crate::protocol::{ArenaJobPhase, ArenaJobProgress};
use crate::{
    API_VERSION, ApiErrorCode, ApiRequest, ApiResponse, CanaryStage, CanaryTransitionKind,
    ChampionTransitionPayload, Command, ControlError, DenialEntry, DenialKind,
    DriftAdaptationFinishReason, DriftAdaptationFinishedPayload, DriftAdaptationStartedPayload,
    DriftKind, EvaluationEventRecord, EvaluationForgeSummary, EvaluationInvariantSummary,
    EvaluationListEntry, EvaluationRecord, EvaluationSelectionSummary, EvolutionCancelPayload,
    EvolutionCandidateRecord, EvolutionFinishReason, EvolutionFinishedPayload,
    EvolutionGenerationPayload, EvolutionRunRecord, EvolutionRunState, EvolutionStartedPayload,
    EvolverStrategyConfig, ForgeAnalysisBinding, ForgeAnalysisRecord, ForgeAssessmentEventRecord,
    ForgeAssessmentOutcome, ForgeAssessmentPayload, ForgeAssessmentRecord,
    ForgeProposalEventRecord, ForgeProposalPayload, ForgeProposalRecord, GeneSelectionPolicy,
    GeneTransferOutcome, GenomeRecord, InvariantRecord, JobProgress, JobRecord, JobState,
    JobTerminal, MAX_LIST_LIMIT, McpDecision, MetaEvaluationAdmittedPayload, MetaEvaluationPayload,
    MetaEvaluationStatus, MetaLineageOutcome, MetaLineageProgress, MetaStrategyRegisteredPayload,
    MutationPrioritization, MutationSlot, RemoteJobState, ResponseData, RunCompletionReason,
    RunListEntry, SelectionEventRecord, SelectionRecord, WorkerScope, WorldRecord,
};

// A 1 MiB Markdown body can expand to six JSON bytes per escaped control
// character. Keep enough bounded headroom for that representation.
const MAX_REQUEST_BYTES: usize = 7 * 1_048_576;
#[cfg(feature = "test-support")]
const TEST_ARENA_OVERALL_WALL_ENV: &str = "HEPHAESTUS_TEST_ARENA_OVERALL_WALL_MILLIS";
const CONTROL_AGGREGATE: &str = "hephaestus-control";
const OPERATOR_ACTOR: &str = "local-operator";
/// Hard ceiling on `WorkerCredentialMint`'s `ttl_seconds`: 30 days.
const MAX_WORKER_TTL_SECONDS: u64 = 30 * 24 * 60 * 60;
const RUNTIME_ACTOR: &str = "daemon-runtime";
const MAX_EVALUATION_WALL_MILLIS: u64 = 86_400_000;
const MAX_EVALUATION_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EVALUATION_COST_MICROUSD: u64 = 1_000_000_000;
const PAIRED_EVALUATION_SEED: u64 = 42;
const PAIRED_EVALUATION_WALL_MILLIS: u64 = 10_000;
const PAIRED_EVALUATION_OUTPUT_BYTES: u64 = 1_048_576;
const MAX_SOURCE_FILE_BYTES: u64 = 1_048_576;
const MAX_ARTIFACT_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SOCKET_HANDLERS: usize = 16;
const MAX_QUEUED_REQUESTS: usize = 16;
// Every per-generation Arena evaluation, proposal, assessment, and promotion
// identity is derived from the run_id with a short suffix; this cap leaves
// enough headroom under `validate_job_id`'s 128-byte limit for any generation
// index up to `u32::MAX`.
const MAX_EVOLUTION_RUN_ID_BYTES: usize = 100;
const REMOTE_REFERENCE_PROMPT: &str =
    "Inventory the isolated repository without modifying it or using the network.";
const MAX_WORKER_MESSAGE_BYTES: usize = 2 * 1_048_576;
const REMOTE_LEASE_TIMEOUT: Duration = Duration::from_secs(60);

/// One authenticated request from a remote worker over the dedicated
/// `worker.sock`. Every variant carries the worker's scoped credential; an
/// invalid, expired, or revoked credential fails closed before any lease or
/// result is processed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerRequest {
    /// Ask for one pending remote job, if any is available.
    Lease {
        /// Operator-chosen worker identity presenting this credential.
        worker_id: String,
        /// Raw credential secret, hex-encoded.
        token: String,
    },
    /// Return the signed result of one previously leased job.
    SubmitResult {
        /// Operator-chosen worker identity presenting this credential.
        worker_id: String,
        /// Raw credential secret, hex-encoded.
        token: String,
        /// Job identity being completed.
        job_id: String,
        /// Hex-encoded raw output bytes from `execute_reference_worker_request`.
        output_hex: String,
        /// Worker-observed completion outcome.
        completion: RemoteCompletion,
    },
}

/// Coarse, worker-observed completion outcome for one leased job. The
/// daemon, not the worker, is the sole signer of the canonical result this
/// produces.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteCompletion {
    /// The reference worker transform completed successfully.
    Success,
    /// The reference worker transform failed.
    ProviderFailure,
}

/// The daemon's reply to one `WorkerRequest`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerReply {
    /// One job leased to the requesting worker.
    Leased {
        /// Job identity to return a result for.
        job_id: String,
        /// Immutable Genome identity executed by the runtime.
        genome_id: String,
        /// Hex-encoded exact bytes for `execute_reference_worker_request`.
        frame_hex: String,
    },
    /// No pending job is currently available to lease.
    NoWork,
    /// The result was accepted; a canonical signed result now exists (or
    /// already existed, for a duplicate delivery).
    ResultAccepted {
        /// The completed job identity.
        job_id: String,
    },
    /// The request was refused; nothing was recorded.
    Error {
        /// Non-sensitive, stable refusal reason.
        reason: String,
    },
}

/// Coordinates the background Arena-trial execution thread with the
/// daemon's `worker.sock` `Lease`/`SubmitResult` loop (TD-12), so one
/// reference-role trial admitted with the remote opt-in is executed by a
/// remote worker exactly like a locally sandboxed one: the background
/// thread registers the trial's framed request here and blocks on
/// [`Self::submit_and_wait`]; `ControlPlane::lease_remote_job` and
/// `record_remote_job_result` (the exact same handlers a direct remote-run
/// job uses) serve it from the daemon's single control-loop thread.
///
/// Ephemeral and non-canonical, exactly like `ControlPlane::remote_leases`:
/// nothing here is ledgered directly. The eventual `run.result_recorded`
/// event is ledgered by the same code path a local trial's result takes,
/// so it is byte-for-byte indistinguishable from local execution. A lease
/// that a worker never returns simply times out and becomes leasable again
/// (see [`REMOTE_LEASE_TIMEOUT`]); the Arena job's own overall deadline and
/// operator cancellation both still terminalize the job because both set
/// the same `cancel` flag [`Self::submit_and_wait`] polls.
struct RemoteArenaLeaseQueue {
    inner: Mutex<RemoteArenaLeaseQueueState>,
}

#[derive(Default)]
struct RemoteArenaLeaseQueueState {
    pending: BTreeMap<String, PendingArenaTrialJob>,
    leased: HashMap<String, Instant>,
    /// `job_id`s that already received a result, so a worker's retried
    /// `SubmitResult` for the same trial (duplicate delivery) is accepted
    /// idempotently instead of failing with "not recognized".
    completed: BTreeSet<String>,
}

struct PendingArenaTrialJob {
    genome_id: String,
    frame: Vec<u8>,
    result_sender: mpsc::SyncSender<RemoteArenaTrialOutcome>,
}

/// One remote worker's raw result for a leased Arena trial, handed back to
/// the blocked background execution thread to turn into the same
/// [`ReferenceExecution`] a local trial would have produced.
struct RemoteArenaTrialOutcome {
    output: Vec<u8>,
    completion: RemoteCompletion,
    latency_millis: u64,
}

impl RemoteArenaLeaseQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(RemoteArenaLeaseQueueState::default()),
        })
    }

    /// Registers one leasable trial and blocks the calling (background)
    /// thread until a worker submits its result, `cancel` is set (operator
    /// cancellation or the job's overall deadline), or `deadline` passes.
    ///
    /// # Errors
    ///
    /// Fails when cancelled or the deadline passes before a worker submits
    /// a result; the pending entry is removed either way so a late,
    /// stray `SubmitResult` for it is refused rather than silently
    /// accepted.
    fn submit_and_wait(
        &self,
        job_id: &str,
        genome_id: &str,
        frame: Vec<u8>,
        cancel: &std::sync::atomic::AtomicBool,
        deadline: Instant,
    ) -> Result<RemoteArenaTrialOutcome, String> {
        const POLL_INTERVAL: Duration = Duration::from_millis(200);
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        {
            let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            state.pending.insert(
                job_id.to_owned(),
                PendingArenaTrialJob {
                    genome_id: genome_id.to_owned(),
                    frame,
                    result_sender,
                },
            );
        }
        let outcome = loop {
            if cancel.load(Ordering::Acquire) {
                break Err("paired evaluation was cancelled".to_owned());
            }
            let now = Instant::now();
            if now >= deadline {
                break Err("remote Arena trial lease deadline expired".to_owned());
            }
            match result_receiver.recv_timeout(POLL_INTERVAL.min(deadline - now)) {
                Ok(outcome) => break Ok(outcome),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err("remote Arena trial lease queue was dropped".to_owned());
                }
            }
        };
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending.remove(job_id);
        state.leased.remove(job_id);
        outcome
    }

    /// Leases the oldest unleased pending trial, if any.
    fn lease(&self) -> Option<(String, String, Vec<u8>)> {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        state
            .leased
            .retain(|_, leased_at| now.duration_since(*leased_at) < REMOTE_LEASE_TIMEOUT);
        let job_id = state
            .pending
            .keys()
            .find(|job_id| !state.leased.contains_key(job_id.as_str()))?
            .clone();
        state.leased.insert(job_id.clone(), now);
        let job = state.pending.get(&job_id)?;
        Some((job_id, job.genome_id.clone(), job.frame.clone()))
    }

    /// Reports whether `job_id` names a trial this queue currently owns
    /// (pending or already completed), so the daemon's worker-request
    /// dispatcher can route to this queue instead of the canonical
    /// direct-run `remote_jobs` map.
    fn owns(&self, job_id: &str) -> bool {
        let state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        state.pending.contains_key(job_id) || state.completed.contains(job_id)
    }

    /// Delivers one worker's raw result for `job_id`.
    ///
    /// # Errors
    ///
    /// Fails only when `job_id` names neither a pending nor an already
    /// completed trial this queue owns; the caller should treat that as
    /// "`job_id` is not recognized", exactly like the direct-run path.
    fn submit_result(
        &self,
        job_id: &str,
        output: Vec<u8>,
        completion: RemoteCompletion,
    ) -> Result<(), &'static str> {
        let mut state = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if state.completed.contains(job_id) {
            return Ok(());
        }
        let Some(sender) = state
            .pending
            .get(job_id)
            .map(|job| job.result_sender.clone())
        else {
            return Err("job_id is not recognized");
        };
        let latency_millis = state.leased.get(job_id).map_or(0, |leased_at| {
            u64::try_from(leased_at.elapsed().as_millis()).unwrap_or(u64::MAX)
        });
        state.completed.insert(job_id.to_owned());
        drop(state);
        let _ignored = sender.send(RemoteArenaTrialOutcome {
            output,
            completion,
            latency_millis,
        });
        Ok(())
    }
}

// Meta-evaluation run IDs derive two per-lineage evolve run IDs each
// (`meta-{id}-a-{index}` / `meta-{id}-b-{index}`), which themselves derive
// per-generation identifiers under `MAX_EVOLUTION_RUN_ID_BYTES`. Leaving 40
// bytes for the caller-selected `meta_run_id` keeps every derived evolve
// run_id comfortably inside that ceiling.
const MAX_META_RUN_ID_BYTES: usize = 40;

/// Resolves `HEPHAESTUS_HOME`, then the conventional per-user data directory.
///
/// # Errors
///
/// Fails if neither `HEPHAESTUS_HOME` nor `HOME` names a usable directory.
pub fn data_dir_from_environment() -> Result<PathBuf, ControlError> {
    if let Some(path) = env::var_os("HEPHAESTUS_HOME") {
        return Ok(PathBuf::from(path));
    }
    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".hephaestus"))
        .ok_or(ControlError::Protocol("no data directory configured"))
}

/// Single-writer daemon state and local operator API.
pub struct ControlPlane {
    /// Evidence already verified by projection refresh in this process; see
    /// [`EvidenceCache`].
    evidence_cache: EvidenceCache,
    data_dir: PathBuf,
    source_repository: PathBuf,
    evaluator_executable: PathBuf,
    reference_worker_executable: PathBuf,
    reference_worker_digest: String,
    /// Cached private snapshot of the reference worker executable, pinned
    /// once per daemon lifetime and shared by every direct reference run,
    /// synchronous `RunEvaluation`, and Arena job admission. `RefCell`
    /// interior mutability keeps `pin_reference_worker` callable from `&self`
    /// call sites that predate this cache; `ControlPlane` is owned by a
    /// single thread for its whole lifetime, so no synchronization is needed.
    /// A failed re-verification (see [`PinnedReferenceWorker::verify`]) both
    /// fails this call closed and clears the cache, so the *next* call pins a
    /// fresh snapshot rather than silently reusing or masking a corrupted one.
    pinned_reference_worker: RefCell<Option<Arc<PinnedReferenceWorker>>>,
    guardian_executable: PathBuf,
    /// Operator-configured Codex CLI binary. A daemon flag or environment
    /// variable, never a hardcoded path, so offline tests can point it at a
    /// fake and a live operator can point it at their own install.
    codex_executable: PathBuf,
    /// Operator-configured Claude Code CLI binary. Same configuration story
    /// as `codex_executable`.
    claude_executable: PathBuf,
    /// Names of environment variables explicitly copied into a provider
    /// child process on top of its fixed `PATH`/`HOME`/`TMPDIR`. Empty by
    /// default: nothing is inherited unless an operator names it here.
    provider_env_allowlist: Vec<String>,
    token_hex: String,
    operator_token: OperatorToken,
    run_result_signer: RunResultSigner,
    run_result_verifier: RunResultVerifier,
    storage: Option<CanonicalStorage>,
    /// Reopens an independent ledger handle onto the same canonical storage:
    /// for the default SQLite backend, a fresh connection to the same
    /// database path (today's behavior, unchanged); for a caller-supplied
    /// backend passed to [`Self::open_with_backends`], whatever that caller
    /// gave to reconstruct or share a handle (e.g. reopening a JSONL path,
    /// or cloning an `Arc` around an in-memory backend). Used both to
    /// restore `storage` after an operation consumes it and fails, and by
    /// read-only Arena verification helpers that need their own throwaway
    /// handle without touching `storage`.
    open_ledger: Arc<dyn Fn() -> Result<Box<dyn EventLedger + Send>, ControlError> + Send + Sync>,
    /// Same reopening contract as `open_ledger`, for the artifact backend.
    open_artifacts:
        Arc<dyn Fn() -> Result<Box<dyn ArtifactBackend + Send + Sync>, ControlError> + Send + Sync>,
    state: ControlState,
    _lock: File,
    shutdown_requested: bool,
    active_job: Option<ActiveJob>,
    job_evidence_receiver: Option<mpsc::Receiver<EvidenceRequest>>,
    job_result_receiver: Option<mpsc::Receiver<AsyncJobResult>>,
    // Exercise durable recovery when the real executor exits after cleanup but loses its result.
    #[cfg(test)]
    drop_next_direct_result_after_execution: bool,
    #[cfg(test)]
    thread_spawn_failures: TestThreadSpawnFailures,
    active_arena_job: Option<ActiveArenaJob>,
    arena_message_receiver: Option<mpsc::Receiver<ArenaWorkerMessage>>,
    arena_message_sender: Option<mpsc::SyncSender<ArenaWorkerMessage>>,
    // Ephemeral, non-canonical: which remote job a lease currently claims and
    // when that lease was granted. Never survives a restart; a lease that a
    // worker never returns simply becomes eligible for another worker after
    // `REMOTE_LEASE_TIMEOUT`, and the daemon's signed result stays the only
    // durable fact.
    remote_leases: HashMap<String, Instant>,
    /// Leasable reference-role Arena trials for evaluations admitted with
    /// the remote opt-in (TD-12). Fresh and empty for every process; see
    /// [`RemoteArenaLeaseQueue`].
    remote_arena_lease: Arc<RemoteArenaLeaseQueue>,
    /// Per-run trace/experience record counts, threaded through the durable writer
    /// loop's per-event [`EvidenceRecorder`] reconstructions via
    /// [`EvidenceRecorder::from_stores_with_run_counts`]/`into_stores_with_run_counts`
    /// so retention enforcement stays O(1) per event instead of replaying the whole
    /// ledger to recount every time (see `TECH_DEBT.md` TD-20). Seeded once from the
    /// startup replay; kept current by every evidence request the writer loop persists.
    evidence_run_record_counts: BTreeMap<String, usize>,
}

/// Canonical durable storage behind [`EventLedger`]/[`ArtifactBackend`] trait
/// objects. The default daemon (every `ControlPlane::open*` constructor)
/// still boxes the SQLite [`EventStore`] and filesystem [`ArtifactStore`];
/// [`ControlPlane::open_with_backends`] accepts any other backend pair (e.g.
/// the JSONL ledger and in-memory artifact backend).
struct CanonicalStorage {
    ledger: Box<dyn EventLedger + Send>,
    artifacts: Box<dyn ArtifactBackend + Send + Sync>,
}

#[derive(Clone)]
struct ActiveJob {
    record: JobRecord,
    genome: GenomeRecord,
    spec: RunSpec,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

struct AsyncJobResult {
    job_id: String,
    output: Result<ReferenceExecution, String>,
}

#[cfg(test)]
#[derive(Default)]
struct TestThreadSpawnFailures {
    direct: bool,
    arena: bool,
    arena_scoring: bool,
}

#[cfg(test)]
pub(super) fn spawn_named_thread<T, F>(
    name: String,
    fail: bool,
    task: F,
) -> std::io::Result<thread::JoinHandle<T>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    if fail {
        Err(std::io::Error::other(
            "injected thread launch failure for admission recovery test",
        ))
    } else {
        thread::Builder::new().name(name).spawn(task)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ArenaJobRecord {
    job_id: String,
    evaluation_id: String,
    parent_genome_id: String,
    candidate_genome_id: String,
    world_id: String,
    visible_manifest_id: String,
    sealed_manifest_id: String,
    evaluator_id: String,
    source_revision: String,
    worker_digest: String,
    /// Parent's execution-environment identity, and the candidate's too
    /// unless `candidate_environment_id` is set.
    environment_id: String,
    /// Set only for a mixed pair permitted by the World's Law: the
    /// candidate's own distinct execution-environment identity. Omitted
    /// (`None`) for a homogeneous pair, so every previously admitted job
    /// still round-trips byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_environment_id: Option<String>,
    seed: u64,
    trial_budget: RunBudgetReceipt,
    overall_budget: RunBudgetReceipt,
    ordered_trial_run_ids: Vec<String>,
    parent_trial_count: u32,
    total_trials: u32,
    plan_commitment: String,
    caller_id: String,
    receipt_timestamp_millis: i64,
    completed_trials: u32,
    phase: ArenaJobPhase,
    state: JobState,
    terminal: Option<JobTerminal>,
    evaluation: Option<EvaluationRecord>,
    /// Per-evaluation opt-in, recorded at admission (TD-12): when true,
    /// every reference-role trial in this evaluation is leased to a remote
    /// worker instead of run in a local sandbox. `#[serde(default)]` keeps
    /// every previously admitted job (all local) round-tripping unchanged.
    #[serde(default)]
    remote: bool,
}

impl ArenaJobRecord {
    /// The candidate's execution-environment identity: its own distinct one
    /// for a mixed pair, otherwise the same one the parent uses.
    fn effective_candidate_environment_id(&self) -> &str {
        self.candidate_environment_id
            .as_deref()
            .unwrap_or(&self.environment_id)
    }
}

#[derive(Clone)]
struct ArenaTrialSpec {
    genome: GenomeRecord,
    spec: RunSpec,
    provider: Option<Provider>,
}

struct ActiveArenaJob {
    record: ArenaJobRecord,
    world: CompiledWorld,
    visible: TrustedManifest,
    sealed: TrustedManifest,
    binding: EvaluationBinding,
    parent: TrialPlan,
    candidate: TrialPlan,
    receipt_context: ReceiptContext,
    trials: Vec<ArenaTrialSpec>,
    evaluator: Arc<IsolatedEvaluator>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    overall_deadline: Instant,
    overall_timed_out: bool,
}

enum ArenaWorkerMessage {
    Trial {
        job_id: String,
        index: usize,
        output: Result<ReferenceExecution, String>,
        reply: mpsc::Sender<Result<u64, String>>,
    },
    Trials {
        job_id: String,
        result: Result<(), String>,
    },
    Scoring {
        job_id: String,
        result: Result<ScoredEvaluation, String>,
    },
}

struct AsyncArenaTrialLaunch {
    data_dir: PathBuf,
    guardian: PathBuf,
    protected_paths: Vec<PathBuf>,
    /// `None` when neither role in this pair is reference-shaped (a fully
    /// provider pair never executes a reference-role trial, so
    /// `submit_arena_job` skips pinning one at all -- TD-15).
    worker: Option<Arc<PinnedReferenceWorker>>,
    codex_executable: PathBuf,
    claude_executable: PathBuf,
    provider_extra_env: Vec<(String, String)>,
    redaction: RedactionPolicy,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    trials: Vec<ArenaTrialSpec>,
    evidence: hephaestus_experience::ChannelEvidenceSink,
    messages: mpsc::SyncSender<ArenaWorkerMessage>,
    initial_sequence: u64,
    job_id: String,
    /// `Some` when this evaluation was admitted with the remote opt-in
    /// (TD-12): every reference-role trial is leased to a remote worker
    /// through this queue instead of run in a local sandbox. `None` keeps
    /// the pre-existing, fully local execution path.
    remote_lease: Option<Arc<RemoteArenaLeaseQueue>>,
    overall_deadline: Instant,
}

struct AsyncReferenceLaunch {
    sandboxes: SandboxManagerSource,
    guardian: PathBuf,
    protected_paths: Vec<PathBuf>,
    worker: Arc<PinnedReferenceWorker>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// Where a trial gets its `SandboxManager` from: opened fresh (the original,
/// still-used behavior for the one-off single-run path), or an already-open
/// manager shared across every trial of one paired Arena evaluation.
/// `SandboxManager::open` re-validates the sandbox root's device/inode
/// identity and re-applies its permissions on every call; a paired
/// evaluation's handful of trials share the same root for the life of one
/// job, so opening it once instead of once per trial drops that repeated,
/// always-identical setup work from the critical path. `SandboxManager` has
/// no interior mutability, so sharing one across trials changes nothing
/// about isolation: every trial still calls `create()` itself and gets its
/// own fresh, independently validated `git worktree add --detach`.
enum SandboxManagerSource {
    OpenFresh(PathBuf),
    Shared(Arc<SandboxManager>),
}

impl SandboxManagerSource {
    fn resolve(self) -> Result<Arc<SandboxManager>, String> {
        match self {
            Self::OpenFresh(data_dir) => {
                SandboxManager::open(data_dir.join("sandboxes"), Duration::from_secs(30))
                    .map(Arc::new)
                    .map_err(|_| "sandbox could not be opened".to_owned())
            }
            Self::Shared(manager) => Ok(manager),
        }
    }
}

struct PinnedReferenceWorker {
    directory: TempDir,
    executable: PathBuf,
    digest: String,
}

impl PinnedReferenceWorker {
    fn verify(&self) -> Result<(), ExecuteError> {
        if self.executable.parent() != Some(self.directory.path()) {
            return Err(ExecuteError::Rejected(
                "pinned reference worker escaped its private directory".to_owned(),
            ));
        }
        let actual = executable_digest(&self.executable).map_err(|_| ExecuteError::Internal)?;
        if actual != self.digest {
            return Err(ExecuteError::Rejected(
                "pinned reference worker identity changed during execution".to_owned(),
            ));
        }
        Ok(())
    }
}

struct SandboxCleanupGuard {
    sandbox: Option<Sandbox>,
}

impl SandboxCleanupGuard {
    const fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox: Some(sandbox),
        }
    }

    fn sandbox(&self) -> Result<&Sandbox, ExecuteError> {
        self.sandbox.as_ref().ok_or(ExecuteError::Internal)
    }

    fn cleanup(mut self) -> Result<(), ExecuteError> {
        let sandbox = self.sandbox.take().ok_or(ExecuteError::Internal)?;
        sandbox.cleanup().map_err(|_| ExecuteError::Internal)
    }
}

impl Drop for SandboxCleanupGuard {
    fn drop(&mut self) {
        if let Some(sandbox) = self.sandbox.take() {
            let _ = sandbox.cleanup();
        }
    }
}

struct QueuedRequest {
    request: ApiRequest,
    reply: mpsc::SyncSender<ApiResponse>,
}

struct QueuedWorkerRequest {
    request: WorkerRequest,
    reply: mpsc::SyncSender<WorkerReply>,
}

fn serve_worker_connection(
    mut stream: UnixStream,
    sender: &mpsc::SyncSender<QueuedWorkerRequest>,
    active_handlers: Arc<AtomicUsize>,
) {
    let _count = HandlerCount(active_handlers);
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _read_timeout_error = stream.set_read_timeout(Some(Duration::from_secs(2))).err();
    let _write_timeout_error = stream.set_write_timeout(Some(Duration::from_secs(2))).err();
    let mut bytes = Vec::new();
    let response = match std::io::Read::by_ref(&mut stream)
        .take(u64::try_from(MAX_WORKER_MESSAGE_BYTES).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
    {
        Ok(_) if bytes.len() > MAX_WORKER_MESSAGE_BYTES => WorkerReply::Error {
            reason: "request exceeds limit".to_owned(),
        },
        Err(_) => WorkerReply::Error {
            reason: "request could not be read".to_owned(),
        },
        Ok(_) => match serde_json::from_slice::<WorkerRequest>(&bytes) {
            Ok(request) => {
                let (reply, response) = mpsc::sync_channel(1);
                match sender.try_send(QueuedWorkerRequest { request, reply }) {
                    Ok(()) => response
                        .recv_timeout(Duration::from_secs(15))
                        .unwrap_or_else(|_| WorkerReply::Error {
                            reason: "canonical operation failed".to_owned(),
                        }),
                    Err(mpsc::TrySendError::Full(_)) => WorkerReply::Error {
                        reason: "daemon worker queue is full".to_owned(),
                    },
                    Err(mpsc::TrySendError::Disconnected(_)) => WorkerReply::Error {
                        reason: "daemon is stopping".to_owned(),
                    },
                }
            }
            Err(_) => WorkerReply::Error {
                reason: "request does not match the declared schema".to_owned(),
            },
        },
    };
    if let Ok(bytes) = serde_json::to_vec(&response) {
        let _ignored = write_bounded_response(&mut stream, &bytes);
    }
}

struct HandlerCount(Arc<AtomicUsize>);

impl Drop for HandlerCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn serve_connection(
    mut stream: UnixStream,
    sender: &mpsc::SyncSender<QueuedRequest>,
    active_handlers: Arc<AtomicUsize>,
) {
    let _count = HandlerCount(active_handlers);
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let _read_timeout_error = stream.set_read_timeout(Some(Duration::from_secs(2))).err();
    let _write_timeout_error = stream.set_write_timeout(Some(Duration::from_secs(2))).err();
    let response = match read_bounded_request(&mut stream) {
        Ok(None) => ApiResponse::failure("", ApiErrorCode::InvalidRequest, "request exceeds limit"),
        Err(_) => ApiResponse::failure(
            "",
            ApiErrorCode::InvalidRequest,
            "request could not be read",
        ),
        Ok(Some(bytes)) => match serde_json::from_slice::<ApiRequest>(&bytes) {
            Ok(request) => {
                let (reply, response) = mpsc::sync_channel(1);
                match sender.try_send(QueuedRequest { request, reply }) {
                    Ok(()) => response
                        .recv_timeout(Duration::from_secs(15))
                        .unwrap_or_else(|_| {
                            ApiResponse::failure(
                                "",
                                ApiErrorCode::Internal,
                                "canonical operation failed",
                            )
                        }),
                    Err(mpsc::TrySendError::Full(_)) => {
                        ApiResponse::failure("", ApiErrorCode::Busy, "daemon request queue is full")
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        ApiResponse::failure("", ApiErrorCode::Internal, "daemon is stopping")
                    }
                }
            }
            Err(_) => ApiResponse::failure(
                "",
                ApiErrorCode::InvalidRequest,
                "request does not match the declared schema",
            ),
        },
    };
    if let Ok(bytes) = serde_json::to_vec(&response) {
        let _ignored = write_bounded_response(&mut stream, &bytes);
    }
}

fn write_bounded_response(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut offset = 0;
    while offset < bytes.len() {
        match stream.write(&bytes[offset..]) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(written) => offset += written,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_bounded_request(stream: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8_192];
    let mut total_bytes = 0_usize;
    let mut too_large = false;
    loop {
        let read = match stream.read(&mut buffer) {
            Ok(read) => read,
            Err(_) if too_large => return Ok(None),
            Err(error) => return Err(error),
        };
        if read == 0 {
            return Ok((!too_large).then_some(bytes));
        }
        total_bytes = total_bytes.saturating_add(read);
        if total_bytes > MAX_REQUEST_BYTES {
            too_large = true;
            bytes.clear();
        } else if !too_large {
            bytes.extend_from_slice(&buffer[..read]);
        }
        if too_large && total_bytes >= MAX_REQUEST_BYTES.saturating_mul(2) {
            return Ok(None);
        }
    }
}

fn reject_busy_stream(mut stream: UnixStream) {
    let response = ApiResponse::failure("", ApiErrorCode::Busy, "daemon connection limit reached");
    if let Ok(bytes) = serde_json::to_vec(&response) {
        let _ignored = stream.write_all(&bytes);
    }
}

impl ControlPlane {
    /// Opens canonical storage, takes the single-writer lock, and replays state.
    ///
    /// # Errors
    ///
    /// Fails closed for unsafe storage paths, another writer, corrupt tokens,
    /// ledger integrity failures, or invalid projection events.
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self, ControlError> {
        Self::open_with_repository(data_dir, env::current_dir()?)
    }

    /// Opens canonical storage with an explicit repository for isolated reference runs.
    ///
    /// # Errors
    ///
    /// In addition to [`Self::open`] failures, rejects a path that is not a Git
    /// worktree. The canonicalized path is fixed for the daemon lifetime.
    pub fn open_with_repository(
        data_dir: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
    ) -> Result<Self, ControlError> {
        Self::open_with_repository_and_evaluator(
            data_dir,
            source_repository,
            default_evaluator_executable()?,
        )
    }

    /// Opens storage with an explicit reference worker and default evaluator.
    ///
    /// # Errors
    ///
    /// Applies the same fail-closed storage and repository checks as
    /// [`Self::open_with_repository`].
    pub fn open_with_repository_and_reference_worker(
        data_dir: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
        reference_worker_executable: impl Into<PathBuf>,
    ) -> Result<Self, ControlError> {
        Self::open_with_repository_evaluator_and_reference_worker(
            data_dir,
            source_repository,
            default_evaluator_executable()?,
            reference_worker_executable,
        )
    }

    /// Opens canonical storage with explicit source and evaluator executables.
    ///
    /// The evaluator path is identity-checked against each World at evaluation
    /// time, so opening the daemon does not grant an unregistered executable.
    ///
    /// # Errors
    ///
    /// Applies the same fail-closed storage and repository checks as
    /// [`Self::open_with_repository`].
    pub fn open_with_repository_and_evaluator(
        data_dir: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
        evaluator_executable: impl Into<PathBuf>,
    ) -> Result<Self, ControlError> {
        Self::open_with_repository_evaluator_and_reference_worker(
            data_dir,
            source_repository,
            evaluator_executable,
            default_reference_worker_executable()?,
        )
    }

    /// Opens canonical storage with explicit evaluator and reference-worker executables.
    ///
    /// # Errors
    ///
    /// Applies the same fail-closed storage and repository checks as
    /// [`Self::open_with_repository`].
    #[allow(clippy::too_many_lines)]
    pub fn open_with_repository_evaluator_and_reference_worker(
        data_dir: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
        evaluator_executable: impl Into<PathBuf>,
        reference_worker_executable: impl Into<PathBuf>,
    ) -> Result<Self, ControlError> {
        let data_dir = data_dir.into();
        let ledger_dir = data_dir.clone();
        let open_ledger = move || -> Result<Box<dyn EventLedger + Send>, ControlError> {
            let database_path = ledger_dir.join("events.sqlite3");
            prepare_private_directory(&ledger_dir)?;
            prepare_private_file(&database_path)?;
            Ok(Box::new(EventStore::open(&database_path)?))
        };
        let artifacts_dir = data_dir.clone();
        let open_artifacts =
            move || -> Result<Box<dyn ArtifactBackend + Send + Sync>, ControlError> {
                let artifacts_path = artifacts_dir.join("blobs");
                prepare_private_directory(&artifacts_path)?;
                Ok(Box::new(ArtifactStore::open(artifacts_path)?))
            };
        Self::open_with_backends(
            data_dir,
            source_repository,
            evaluator_executable,
            reference_worker_executable,
            open_ledger,
            open_artifacts,
        )
    }

    /// Opens canonical storage over any [`EventLedger`]/[`ArtifactBackend`]
    /// pair, e.g. the JSONL ledger and in-memory artifact backend, instead of
    /// the default SQLite/CAS backends `open_with_repository_evaluator_and_reference_worker`
    /// uses. Every other constructor above delegates to this one after
    /// building its own SQLite/CAS opener closures, so behavior is identical
    /// for the default daemon: this method's storage-agnostic checks and
    /// recovery are the sole source of truth for what "opening the control
    /// plane" does.
    ///
    /// `open_ledger`/`open_artifacts` are called once here for the initial
    /// handles, and stored to reopen an independent handle later: to restore
    /// `storage` after an operation consumes it and fails, and for read-only
    /// Arena verification helpers that need their own throwaway handle. For a
    /// backend with a durable path (SQLite, the JSONL ledger, the filesystem
    /// CAS) the closure simply reopens that path. For a backend with no
    /// durable path of its own (e.g. an in-memory artifact backend), it
    /// should close over an `Arc` and clone it, which is a valid
    /// [`ArtifactBackend`] itself (see the blanket impl in
    /// `hephaestus_ledger::storage`).
    ///
    /// # Errors
    ///
    /// Applies the same fail-closed repository, ledger-integrity, and
    /// projection checks as [`Self::open_with_repository`].
    #[allow(clippy::too_many_lines)]
    pub fn open_with_backends(
        data_dir: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
        evaluator_executable: impl Into<PathBuf>,
        reference_worker_executable: impl Into<PathBuf>,
        open_ledger: impl Fn() -> Result<Box<dyn EventLedger + Send>, ControlError>
        + Send
        + Sync
        + 'static,
        open_artifacts: impl Fn() -> Result<Box<dyn ArtifactBackend + Send + Sync>, ControlError>
        + Send
        + Sync
        + 'static,
    ) -> Result<Self, ControlError> {
        let open_ledger: Arc<
            dyn Fn() -> Result<Box<dyn EventLedger + Send>, ControlError> + Send + Sync,
        > = Arc::new(open_ledger);
        let open_artifacts: Arc<
            dyn Fn() -> Result<Box<dyn ArtifactBackend + Send + Sync>, ControlError> + Send + Sync,
        > = Arc::new(open_artifacts);
        let mut ledger = open_ledger()?;
        let artifacts = open_artifacts()?;
        let data_dir = data_dir.into();
        let source_repository = validate_source_repository(&source_repository.into())?;
        let evaluator_executable = evaluator_executable.into();
        let reference_worker_executable = reference_worker_executable.into();
        let reference_worker_digest = executable_digest(&reference_worker_executable)?;
        let guardian_executable = default_process_guardian_executable()?;
        let codex_executable =
            provider_executable_from_environment("HEPHAESTUS_CODEX_EXECUTABLE", "codex");
        let claude_executable =
            provider_executable_from_environment("HEPHAESTUS_CLAUDE_EXECUTABLE", "claude");
        let provider_env_allowlist = provider_env_allowlist_from_environment();
        prepare_private_directory(&data_dir)?;
        let lock = take_writer_lock(&data_dir.join("daemon.lock"))?;
        let (token_hex, token_bytes) = load_or_create_token(&data_dir.join("operator.token"))?;
        let operator_token = OperatorToken::from_bytes(token_bytes);
        let history = ledger.replay_verified()?;
        reject_legacy_run_result_history(&history)?;
        let evidence_run_record_counts = count_records_per_run(&history);
        let registered =
            RegisteredObjects::replay(&history, &artifacts).map_err(registration_control_error)?;
        verify_selection_history(&artifacts, &history, &registered)?;
        verify_forge_history(&artifacts, &history, &registered)?;
        verify_forge_assessment_history(&artifacts, &history, &registered)?;
        verify_invariant_history(&artifacts, &history, &registered)?;
        verify_cluster_history(&artifacts, &history, &registered)?;
        verify_champion_history(&artifacts, &history, &registered)?;
        verify_gene_bank_history(&artifacts, &history, &registered)?;
        verify_evolution_history(&history, &registered)?;
        verify_meta_evolution_history(&history)?;
        verify_drift_history(&artifacts, &history, &registered)?;
        verify_canary_history(&artifacts, &history, &registered)?;
        verify_drift_adaptation_history(&history)?;
        let has_run_results = history
            .iter()
            .any(|event| event.event_type == "run.result_recorded");
        let anchored_verifier = anchored_world_verifier(&registered, &artifacts)?;
        let run_result_signer = load_or_create_run_result_signer(
            &data_dir.join("runtime-producer.key"),
            !has_run_results && anchored_verifier.is_none(),
        )?;
        let run_result_verifier = run_result_signer.verifier();
        if anchored_verifier
            .as_ref()
            .is_some_and(|key| key != &run_result_verifier.public_key_bytes())
        {
            return Err(ControlError::Protocol(
                "runtime producer key does not match registered World verifier",
            ));
        }
        let mut state =
            ControlState::from_events(&history, registered, &operator_token, &run_result_verifier)?;
        ControlState::verify_artifacts(&history, &artifacts, &run_result_verifier)?;
        verify_arena_evaluation_records(&artifacts, &history, &state)?;
        // The prior guardian owns any process group left at crash time; recovery
        // persists an outcome only from already verified canonical evidence.
        recover_unfinished_jobs(
            &mut ledger,
            &mut state,
            &operator_token,
            &run_result_verifier,
        )?;
        recover_unfinished_arena_jobs(
            &mut ledger,
            &artifacts,
            &mut state,
            &operator_token,
            &run_result_verifier,
        )?;
        Ok(Self {
            evidence_cache: EvidenceCache::default(),
            data_dir,
            source_repository,
            evaluator_executable,
            reference_worker_executable,
            reference_worker_digest,
            pinned_reference_worker: RefCell::new(None),
            guardian_executable,
            codex_executable,
            claude_executable,
            provider_env_allowlist,
            token_hex,
            operator_token,
            run_result_signer,
            run_result_verifier,
            storage: Some(CanonicalStorage { ledger, artifacts }),
            open_ledger,
            open_artifacts,
            state,
            _lock: lock,
            shutdown_requested: false,
            active_job: None,
            job_evidence_receiver: None,
            job_result_receiver: None,
            #[cfg(test)]
            drop_next_direct_result_after_execution: false,
            #[cfg(test)]
            thread_spawn_failures: TestThreadSpawnFailures::default(),
            active_arena_job: None,
            arena_message_receiver: None,
            arena_message_sender: None,
            remote_leases: HashMap::new(),
            remote_arena_lease: RemoteArenaLeaseQueue::new(),
            evidence_run_record_counts,
        })
    }

    /// Test-only override of the provider adapter binaries and environment
    /// allowlist. Bypasses process environment variables entirely, so
    /// parallel tests never race on shared global state.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_provider_executables_for_testing(
        mut self,
        codex_executable: impl Into<PathBuf>,
        claude_executable: impl Into<PathBuf>,
        provider_env_allowlist: Vec<String>,
    ) -> Self {
        self.codex_executable = codex_executable.into();
        self.claude_executable = claude_executable.into();
        self.provider_env_allowlist = provider_env_allowlist;
        self
    }

    /// Serves authenticated one-request connections until the process is stopped.
    ///
    /// # Errors
    ///
    /// Fails if the socket cannot be bound securely or an accepted request cannot
    /// be read or answered.
    pub fn serve(mut self) -> Result<(), ControlError> {
        let socket_path = self.data_dir.join("control.sock");
        remove_stale_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let (request_sender, request_receiver) =
            mpsc::sync_channel::<QueuedRequest>(MAX_QUEUED_REQUESTS);
        let active_handlers = Arc::new(AtomicUsize::new(0));

        let worker_socket_path = self.data_dir.join("worker.sock");
        remove_stale_socket(&worker_socket_path)?;
        let worker_listener = UnixListener::bind(&worker_socket_path)?;
        fs::set_permissions(&worker_socket_path, fs::Permissions::from_mode(0o600))?;
        worker_listener.set_nonblocking(true)?;
        let (worker_sender, worker_receiver) =
            mpsc::sync_channel::<QueuedWorkerRequest>(MAX_QUEUED_REQUESTS);
        let active_worker_handlers = Arc::new(AtomicUsize::new(0));

        while !self.shutdown_requested {
            self.service_async_messages()?;
            if let Ok(queued) = request_receiver.try_recv() {
                let response = self.handle(queued.request);
                let _ignored = queued.reply.send(response);
            }
            if let Ok(queued) = worker_receiver.try_recv() {
                let response = self.handle_worker_request(queued.request);
                let _ignored = queued.reply.send(response);
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    let current = active_handlers.fetch_add(1, Ordering::AcqRel);
                    if current >= MAX_SOCKET_HANDLERS {
                        active_handlers.fetch_sub(1, Ordering::AcqRel);
                        reject_busy_stream(stream);
                    } else {
                        let sender = request_sender.clone();
                        let handlers = Arc::clone(&active_handlers);
                        thread::spawn(move || serve_connection(stream, &sender, handlers));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error.into()),
            }
            match worker_listener.accept() {
                Ok((stream, _)) => {
                    let current = active_worker_handlers.fetch_add(1, Ordering::AcqRel);
                    if current >= MAX_SOCKET_HANDLERS {
                        active_worker_handlers.fetch_sub(1, Ordering::AcqRel);
                        reject_busy_stream(stream);
                    } else {
                        let sender = worker_sender.clone();
                        let handlers = Arc::clone(&active_worker_handlers);
                        thread::spawn(move || serve_worker_connection(stream, &sender, handlers));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn handle(&mut self, request: ApiRequest) -> ApiResponse {
        if request.version != API_VERSION {
            return ApiResponse::failure(
                request.request_id,
                ApiErrorCode::UnsupportedVersion,
                "unsupported API version",
            );
        }
        if !constant_time_equal(request.token.as_bytes(), self.token_hex.as_bytes()) {
            return ApiResponse::failure(
                request.request_id,
                ApiErrorCode::Unauthorized,
                "authentication failed",
            );
        }

        let request_id = request.request_id;
        if request_id.trim().is_empty() {
            return match self.append_audit(
                &request_id,
                &request.command,
                "control.request_rejected",
            ) {
                Ok(()) => {
                    ApiResponse::failure("", ApiErrorCode::InvalidRequest, "request_id is required")
                }
                Err(_) => {
                    ApiResponse::failure("", ApiErrorCode::Internal, "canonical operation failed")
                }
            };
        }
        match self.execute(&request_id, request.command) {
            Ok(data) => ApiResponse::success(request_id, data),
            Err(ExecuteError::Invalid(message)) => {
                ApiResponse::failure(request_id, ApiErrorCode::InvalidRequest, message)
            }
            Err(ExecuteError::Rejected(message)) => {
                ApiResponse::failure(request_id, ApiErrorCode::InvalidRequest, message)
            }
            Err(ExecuteError::NotFound) => ApiResponse::failure(
                request_id,
                ApiErrorCode::NotFound,
                "canonical record not found",
            ),
            Err(ExecuteError::Internal) => ApiResponse::failure(
                request_id,
                ApiErrorCode::Internal,
                "canonical operation failed",
            ),
            Err(ExecuteError::Busy) => ApiResponse::failure(
                request_id,
                ApiErrorCode::Busy,
                "another bounded job is active",
            ),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute(
        &mut self,
        request_id: &str,
        command: Command,
    ) -> Result<ResponseData, ExecuteError> {
        require_command_fields(&command)?;
        self.require_no_active_job_for_sync_work(&command)?;
        self.require_no_active_evolution_for_external_command(&command)?;
        self.append_audit(request_id, &command, event_type(&command))
            .map_err(|_| ExecuteError::Internal)?;

        match command {
            Command::Status => Ok(self.state.status()),
            Command::Freeze | Command::Unfreeze => Ok(ResponseData::Acknowledged {
                frozen: self.state.freeze.is_frozen(),
                killed_runs: 0,
            }),
            Command::KillAll => {
                self.request_active_job_cancellation()?;
                Ok(ResponseData::Acknowledged {
                    frozen: self.state.freeze.is_frozen(),
                    killed_runs: 0,
                })
            }
            Command::GenomeShow { genome_id } => self
                .state
                .registered
                .genome(&genome_id)
                .map(|genome| genome.record().clone())
                .map(|genome| ResponseData::Genome { genome })
                .ok_or(ExecuteError::NotFound),
            Command::GenomePrompt { genome_id } => self.genome_prompt(&genome_id),
            Command::GenomeList => Ok(ResponseData::Genomes {
                genomes: self
                    .state
                    .registered
                    .genome_records()
                    .into_values()
                    .collect(),
            }),
            Command::GenomeRegister { path, world_id } => self.register_genome(&path, &world_id),
            command @ Command::GenomePropose { .. } => self.propose_genome_command(command),
            command @ Command::GenomeAssess { .. } => self.assess_genome_command(command),
            Command::ForgeAnalyze {
                analysis_id,
                evaluation_id,
            } => self.analyze_forge_clusters(&analysis_id, &evaluation_id),
            Command::WorldShow { world_id } => self
                .state
                .registered
                .world(&world_id)
                .map(|world| world.record().clone())
                .map(|world| ResponseData::World { world })
                .ok_or(ExecuteError::NotFound),
            Command::WorldList => Ok(ResponseData::Worlds {
                worlds: self
                    .state
                    .registered
                    .world_records()
                    .into_values()
                    .collect(),
            }),
            Command::WorldRegister { path } => self.register_world(&path),
            Command::ManifestPut { path } => self.put_manifest(&path),
            Command::ArtifactPut { path } => self.put_artifact(&path),
            Command::VerifierShow => self.verifier_show(),
            Command::RunSubmit { job_id, genome_id } => self.submit_job(&job_id, &genome_id),
            Command::JobStatus { job_id } => self.job_status(&job_id),
            Command::JobKill { job_id } => self.kill_job(&job_id),
            Command::RunReference { genome_id } => {
                let run_id = format!("reference-{}", self.state.event_count);
                self.run_reference(&run_id, &genome_id)
            }
            Command::RunEvaluation {
                genome_id,
                task_id,
                input,
                seed,
                wall_millis,
                maximum_output_bytes,
                maximum_cost_microusd,
            } => {
                let run_id = format!("evaluation-{}", self.state.event_count);
                self.run_evaluation(
                    &run_id,
                    &genome_id,
                    &task_id,
                    &input,
                    seed,
                    wall_millis,
                    maximum_output_bytes,
                    maximum_cost_microusd,
                )
            }
            Command::EvaluatePair {
                evaluation_id,
                parent_genome_id,
                candidate_genome_id,
                remote,
            } => self.submit_arena_job(
                &evaluation_id,
                &parent_genome_id,
                &candidate_genome_id,
                remote,
            ),
            Command::ArenaSelect { evaluation_id } => self.select_arena_evaluation(&evaluation_id),
            Command::ArenaInvariants { evaluation_id } => {
                self.check_arena_invariants(&evaluation_id)
            }
            command @ (Command::ChampionSeed { .. }
            | Command::ChampionPromote { .. }
            | Command::ChampionRollback { .. }) => self.champion_transition_command(command),
            Command::ChampionShow { world_id } => self.champion_show(&world_id),
            Command::DriftRecord {
                drift_id,
                world_id,
                kind,
                evidence_evaluation_id,
            } => self.record_drift(&drift_id, &world_id, kind, &evidence_evaluation_id),
            Command::DriftShow { drift_id } => self.drift_show(&drift_id),
            Command::DriftList { limit } => self.drift_list(limit),
            command @ (Command::CanaryStart { .. }
            | Command::CanaryAdvance { .. }
            | Command::CanaryLiveCheck { .. }) => self.canary_transition_command(command),
            Command::CanaryShow { canary_id } => self.canary_show(&canary_id),
            Command::CanaryList { limit } => self.canary_list(limit),
            Command::GeneExtract {
                gene_id,
                promotion_transition_id,
            } => self.gene_extract(&gene_id, &promotion_transition_id),
            Command::GeneTransfer {
                trial_id,
                gene_id,
                to_genome_id,
            } => self.gene_transfer_apply(&trial_id, &gene_id, &to_genome_id),
            Command::GeneRecord {
                trial_id,
                evaluation_id,
            } => self.gene_transfer_record(&trial_id, &evaluation_id),
            Command::GeneShow { gene_id } => self.gene_show(&gene_id),
            Command::GeneList => self.gene_list(),
            Command::GeneSpeciate {
                species_id,
                gene_id,
                domain_world_id,
            } => self.gene_speciate(&species_id, &gene_id, &domain_world_id),
            command @ Command::EvolveStart { .. } => self.evolve_start(command),
            Command::EvolveStatus { run_id } => self.evolve_status(&run_id),
            Command::EvolveCancel { run_id } => self.evolve_cancel(&run_id),
            Command::MetaStrategyRegister { path } => self.meta_strategy_register(&path),
            Command::MetaStrategyShow { strategy_id } => self.meta_strategy_show(&strategy_id),
            Command::MetaStrategyList => self.meta_strategy_list_response(),
            command @ Command::MetaEvaluate { .. } => self.meta_evaluate(command),
            Command::MetaShow { meta_run_id } => self.meta_show(&meta_run_id),
            Command::MetaStatus { meta_run_id } => self.meta_status(&meta_run_id),
            Command::MetaList { limit } => self.meta_list(limit),
            Command::Replay => self.replay_response(),
            Command::RunList { limit } => self.run_list(limit),
            Command::EvaluationList { limit } => self.evaluation_list(limit),
            Command::DenialList { limit } => self.denial_list(limit),
            Command::DaemonStop => self.request_daemon_stop(),
            Command::McpCall { decision, .. } => match decision {
                McpDecision::Denied { reason } => Ok(ResponseData::McpDenied { reason }),
                McpDecision::Allowed { command } => self.execute(request_id, *command),
            },
            Command::WorkerCredentialMint {
                worker_id,
                ttl_seconds,
            } => self.worker_credential_mint(&worker_id, ttl_seconds),
            Command::WorkerCredentialRevoke { credential_id } => {
                self.worker_credential_revoke(&credential_id)
            }
            Command::RemoteRunSubmit { job_id, genome_id } => {
                self.remote_run_submit(&job_id, &genome_id)
            }
            Command::RemoteJobStatus { job_id } => self.remote_job_status(&job_id),
        }
    }
}

#[derive(Serialize)]
struct AuditedCommand<'a> {
    request_id: &'a str,
    command: &'a Command,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedCommand {
    request_id: String,
    command: Command,
}

#[derive(Debug)]
enum ExecuteError {
    Invalid(&'static str),
    Rejected(String),
    NotFound,
    Internal,
    Busy,
}

fn map_selection_error(error: &ArenaError) -> ExecuteError {
    match error {
        ArenaError::UnknownEvaluation(_) => ExecuteError::NotFound,
        ArenaError::UnsupportedSelectionConfidence(confidence) => ExecuteError::Rejected(format!(
            "registered World confidence {confidence} bps is unsupported for Arena selection"
        )),
        ArenaError::BootstrapWorkExceeded => ExecuteError::Rejected(
            "Arena selection exceeds its deterministic bootstrap work limit".to_owned(),
        ),
        _ => ExecuteError::Internal,
    }
}

fn map_invariant_error(error: ArenaError) -> ExecuteError {
    match error {
        ArenaError::UnknownEvaluation(_) | ArenaError::UnknownInvariantCheck(_) => {
            ExecuteError::NotFound
        }
        ArenaError::MissingWorldArtifact("arena.invariant_manifest") => ExecuteError::Rejected(
            "registered World has no reference-output invariant profile".to_owned(),
        ),
        ArenaError::InvariantConflict(message) => ExecuteError::Rejected(message),
        _ => ExecuteError::Internal,
    }
}

fn map_cluster_error(error: ArenaError) -> ExecuteError {
    match error {
        ArenaError::UnknownEvaluation(_) | ArenaError::UnknownClusterAnalysis(_) => {
            ExecuteError::NotFound
        }
        ArenaError::ClusterConflict(message) => ExecuteError::Rejected(message),
        ArenaError::InvalidId { .. } => ExecuteError::Invalid("analysis_id is invalid"),
        _ => ExecuteError::Internal,
    }
}

fn selection_record(
    world_id: &str,
    receipt: &SelectionReceipt,
    event: &SelectionEvent,
) -> SelectionRecord {
    SelectionRecord {
        evaluation_id: receipt.evaluation_id().to_owned(),
        world_id: world_id.to_owned(),
        receipt: receipt.clone(),
        event: SelectionEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_type: event.event_type.clone(),
            actor: event.actor.clone(),
            event_hash: event.event_hash.clone(),
            receipt_artifact_id: event.receipt_artifact_id.clone(),
        },
    }
}

fn invariant_record(receipt: &InvariantReceipt, event: &InvariantEvent) -> InvariantRecord {
    InvariantRecord {
        receipt: receipt.clone(),
        event: event.clone(),
    }
}

fn forge_analysis_record(analysis: &ClusterAnalysis, event: &ClusterEvent) -> ForgeAnalysisRecord {
    ForgeAnalysisRecord {
        analysis: analysis.clone(),
        event: event.clone(),
    }
}

fn forge_assessment_summary(
    history: &[StoredEvent],
    evaluation_id: &str,
) -> Option<EvaluationForgeSummary> {
    history
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
        .filter_map(|event| decode_forge_assessment(event).ok())
        .find(|payload| payload.evaluation_id == evaluation_id)
        .map(|payload| EvaluationForgeSummary {
            assessment_id: payload.assessment_id,
            outcome: payload.outcome,
        })
}

fn champion_transition_ids_for(history: &[StoredEvent], evaluation_id: &str) -> Vec<String> {
    history
        .iter()
        .filter(|event| event.event_type == CHAMPION_EVENT_TYPE)
        .filter_map(|event| {
            serde_json::from_slice::<ChampionTransitionPayload>(&event.payload).ok()
        })
        .filter(|payload| {
            payload
                .promotion
                .as_ref()
                .is_some_and(|promotion| promotion.evaluation_id == evaluation_id)
        })
        .map(|payload| payload.transition_id)
        .collect()
}

fn evaluation_record_from_operator(
    operator: &hephaestus_arena::OperatorEvaluation,
) -> EvaluationRecord {
    let recorded = operator.candidate_result();
    EvaluationRecord {
        evaluation_id: recorded.summary.evaluation_id.clone(),
        world_id: recorded.summary.world_id.clone(),
        parent_genome_id: recorded.summary.parent_genome_id.clone(),
        candidate_genome_id: recorded.summary.candidate_genome_id.clone(),
        parent_visible_correct: recorded.summary.parent_visible_correct,
        candidate_visible_correct: recorded.summary.candidate_visible_correct,
        visible_total: recorded.summary.visible_total,
        event: EvaluationEventRecord {
            sequence: recorded.event.sequence,
            event_id: recorded.event.event_id.clone(),
            aggregate_id: recorded.event.aggregate_id.clone(),
            event_type: recorded.event.event_type.clone(),
            actor: recorded.event.actor.clone(),
            timestamp_millis: recorded.event.timestamp_millis,
        },
    }
}

fn evaluation_record_from_recorded(
    recorded: &hephaestus_arena::RecordedEvaluation,
) -> EvaluationRecord {
    EvaluationRecord {
        evaluation_id: recorded.summary.evaluation_id.clone(),
        world_id: recorded.summary.world_id.clone(),
        parent_genome_id: recorded.summary.parent_genome_id.clone(),
        candidate_genome_id: recorded.summary.candidate_genome_id.clone(),
        parent_visible_correct: recorded.summary.parent_visible_correct,
        candidate_visible_correct: recorded.summary.candidate_visible_correct,
        visible_total: recorded.summary.visible_total,
        event: EvaluationEventRecord {
            sequence: recorded.event.sequence,
            event_id: recorded.event.event_id.clone(),
            aggregate_id: recorded.event.aggregate_id.clone(),
            event_type: recorded.event.event_type.clone(),
            actor: recorded.event.actor.clone(),
            timestamp_millis: recorded.event.timestamp_millis,
        },
    }
}

/// Remembers evidence a live daemon has already verified during projection
/// refresh, so each refresh re-verifies only events appended or changed
/// since the last one instead of the whole history every time.
///
/// A key embeds the event's chain hash (or, for an Arena job terminal, a
/// digest of the recorded summary), which commits to the event and its
/// entire ledger prefix; every refresh still re-verifies the hash chain
/// first via `EventLedger::replay_verified()`, so a rewritten ledger prefix
/// changes every later event's hash and misses the cache. Startup, `replay`,
/// and every direct verifier call use a fresh, empty cache and verify
/// everything. See `TECH_DEBT.md` TD-16: unlike before this cache existed,
/// a cache hit no longer implies reopening stores or replaying the ledger —
/// [`load_operator_evaluation_in`] and its siblings verify a cache *miss*
/// against the already-replayed `history` in one pass, so a cold cache (a
/// fresh daemon, or many events appended between refreshes) is itself linear
/// in history size rather than quadratic. The cache remains because a
/// warm-cache refresh is still cheaper than any full linear pass: it makes
/// the total cost of many refreshes over a growing history linear in the
/// number of *new* events, not the square of history length. A CAS blob
/// tampered with after its event was verified is not re-detected until the
/// cache is empty again (the next daemon start or `replay`).
#[derive(Default)]
struct EvidenceCache {
    verified: HashSet<String>,
}

impl EvidenceCache {
    fn event_key(kind: &str, event: &StoredEvent) -> String {
        format!(
            "{kind}:{}:{}",
            event.event_id,
            blake3::Hash::from(event.hash).to_hex()
        )
    }

    fn contains(&self, kind: &str, event: &StoredEvent) -> bool {
        self.verified.contains(&Self::event_key(kind, event))
    }

    fn insert(&mut self, kind: &str, event: &StoredEvent) {
        self.verified.insert(Self::event_key(kind, event));
    }

    fn contains_key(&self, key: &str) -> bool {
        self.verified.contains(key)
    }

    fn insert_key(&mut self, key: String) {
        self.verified.insert(key);
    }
}

/// Verifies every succeeded Arena job's terminal summary against the trusted
/// evaluation evidence in `history`, using the daemon's already-open
/// `artifacts` store and the already-replayed `history` instead of reopening
/// stores per job (see `TECH_DEBT.md` TD-16).
fn verify_arena_evaluation_records(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    state: &ControlState,
) -> Result<(), ControlError> {
    verify_arena_evaluation_records_with(artifacts, history, state, &mut EvidenceCache::default())
}

/// Cache-aware counterpart of [`verify_arena_evaluation_records`] used by a
/// live daemon's projection refresh: `cache` remembers, by evaluation id and
/// a digest of the recorded summary, which terminals this process has
/// already verified this run, so a repeated refresh skips re-verifying a job
/// whose terminal has not changed since. Every event is still fully
/// re-verified against `history`'s freshly re-verified hash chain the first
/// time (or after its recorded summary changes), so this is an optimization,
/// not a relaxation: `verify_arena_evaluation_records`, startup, and
/// `replay` always use a fresh cache and verify everything.
fn verify_arena_evaluation_records_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    state: &ControlState,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for job in state.arena_jobs.values().filter(|job| {
        job.state == JobState::Succeeded && job.terminal == Some(JobTerminal::Succeeded)
    }) {
        let key = arena_record_cache_key(&job.evaluation_id, job.evaluation.as_ref())?;
        if cache.contains_key(&key) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let recorded =
            load_recorded_evaluation_in(index, artifacts, &job.evaluation_id).map_err(|_| {
                ControlError::Projection("Arena terminal lacks trusted evaluation evidence".into())
            })?;
        if job.evaluation.as_ref() != Some(&evaluation_record_from_recorded(&recorded)) {
            return Err(ControlError::Projection(
                "Arena terminal differs from trusted evaluation evidence".to_owned(),
            ));
        }
        cache.insert_key(key);
    }
    Ok(())
}

fn arena_record_cache_key(
    evaluation_id: &str,
    evaluation: Option<&EvaluationRecord>,
) -> Result<String, ControlError> {
    let digest = blake3::hash(&serde_json::to_vec(&evaluation)?);
    Ok(format!("arena_record:{evaluation_id}:{}", digest.to_hex()))
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `TECH_DEBT.md` TD-16).
fn verify_forge_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_forge_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_forge_history`]; see [`EvidenceCache`].
fn verify_forge_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    let mut proposal_ids = BTreeSet::new();
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.proposed")
    {
        let payload = decode_forge_proposal(event)?;
        validate_forge_event(event, &payload)?;
        if !proposal_ids.insert(payload.proposal_id.clone()) {
            return Err(ControlError::Projection(
                "Forge proposal id was recorded more than once".to_owned(),
            ));
        }
        if cache.contains("forge", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let selection_event = index
            .get(&payload.selection_event_id)
            .filter(|selection| selection.sequence < event.sequence)
            .ok_or_else(|| {
                ControlError::Projection(
                    "Forge proposal source selection is missing or out of order".to_owned(),
                )
            })?;
        let (_evaluation_id, selection_world_id) = selection_event_references(selection_event)
            .map_err(|_| {
                ControlError::Projection("Forge source selection is invalid".to_owned())
            })?;
        let world = registered.world(&selection_world_id).ok_or_else(|| {
            ControlError::Projection("Forge source World is not registered".to_owned())
        })?;
        let selected =
            verify_selection_event_in(index, artifacts, selection_event, world.compiled())
                .map_err(|_| {
                    ControlError::Projection("Forge source selection is unverified".to_owned())
                })?;
        let receipt = selected.receipt().clone();
        let expected_hash = selected.event().event_hash.clone();
        if payload.selection_event_hash != expected_hash
            || payload.evaluation_id != receipt.evaluation_id()
            || payload.world_id != receipt.world_id()
            || payload.parent_genome_id != receipt.candidate_genome_id()
        {
            return Err(ControlError::Projection(
                "Forge proposal is not bound to its selected candidate".to_owned(),
            ));
        }
        verify_forge_child(artifacts, registered, event, &payload, world.compiled())?;
        validate_hypothesis(&payload.hypothesis)
            .map_err(|_| ControlError::Projection("Forge hypothesis is invalid".to_owned()))?;
        cache.insert("forge", event);
    }
    Ok(())
}

fn verify_forge_child(
    artifacts: &dyn ArtifactBackend,
    registered: &RegisteredObjects,
    event: &StoredEvent,
    payload: &ForgeProposalPayload,
    world: &CompiledWorld,
) -> Result<(), ControlError> {
    let parent = registered
        .genome(&payload.parent_genome_id)
        .ok_or_else(|| {
            ControlError::Projection("Forge proposal parent is not registered".to_owned())
        })?;
    let child = registered.genome(&payload.child.genome_id).ok_or_else(|| {
        ControlError::Projection("Forge proposal child is not registered".to_owned())
    })?;
    if parent.registration_sequence() >= event.sequence
        || child.registration_sequence() != event.sequence
        || child.record() != &payload.child
        || child.record().world_id != payload.world_id
        || child.compiled().parents() != [payload.parent_genome_id.clone()]
        || parent.record().world_id != payload.world_id
        || parent.compiled().artifact_id("agent.prompt")
            != Some(payload.prompt_artifact_before.as_str())
        || child.compiled().artifact_id("agent.prompt")
            != Some(payload.prompt_artifact_after.as_str())
        || payload.artifact_name != "agent.prompt"
    {
        return Err(ControlError::Projection(
            "Forge proposal child differs from its registered lineage".to_owned(),
        ));
    }
    verify_forge_prompt(artifacts, payload)?;
    verify_forge_child_compiles(artifacts, registered, payload, world)
}

fn verify_forge_prompt(
    artifacts: &dyn ArtifactBackend,
    payload: &ForgeProposalPayload,
) -> Result<(), ControlError> {
    let before_bytes = verified_prompt_bytes(artifacts, &payload.prompt_artifact_before)?;
    let after_bytes = verified_prompt_bytes(artifacts, &payload.prompt_artifact_after)?;
    let before_text = std::str::from_utf8(&before_bytes)
        .map_err(|_| ControlError::Protocol("Forge prompt is not UTF-8"))?;
    let after_text = std::str::from_utf8(&after_bytes)
        .map_err(|_| ControlError::Protocol("Forge prompt is not UTF-8"))?;
    let before = ReferenceInstruction::parse(before_text)
        .map_err(|_| ControlError::Protocol("Forge parent prompt is unsupported"))?;
    let after = ReferenceInstruction::parse(after_text)
        .map_err(|_| ControlError::Protocol("Forge child prompt is unsupported"))?;
    // Replay never re-derives an "expected" target the way a new proposal
    // does: it only checks that the recorded `before -> after` edge is a
    // representable catalog edge (roadmap items 8, 10, 13), so every
    // previously recordable receipt (the identity/uppercase flip) still
    // verifies, and a cluster- or Gene-derived proposal targeting any of the
    // 16 reference operations verifies the same way. World mutation-scope
    // authorization is a propose-time-only check (see `forge_prompt_mutation`).
    if mutation_edge_kind(before.operation_name(), after.operation_name()).is_none() {
        return Err(ControlError::Protocol(
            "Forge prompt mutation is not a representable catalog edge",
        ));
    }
    let edge_kind =
        mutation_edge_kind(before.operation_name(), after.operation_name()).expect("checked above");
    let expected_text = mutate_reference_instruction_document(before_text, before, after)
        .map_err(|()| ControlError::Protocol("Forge prompt is outside mutation scope"))?;
    if after_text != expected_text
        || payload.operation_before != reference_instruction_operation(before)
        || payload.operation_after != reference_instruction_operation(after)
        || payload
            .catalog_version
            .is_some_and(|version| version != MUTATION_CATALOG_VERSION)
        || payload
            .mutation_kind
            .as_deref()
            .is_some_and(|kind| kind != edge_kind.as_str())
    {
        return Err(ControlError::Projection(
            "Forge prompt mutation is not a representable one-step catalog edge".to_owned(),
        ));
    }
    Ok(())
}

fn verify_forge_child_compiles(
    artifacts: &dyn ArtifactBackend,
    registered: &RegisteredObjects,
    payload: &ForgeProposalPayload,
    world: &CompiledWorld,
) -> Result<(), ControlError> {
    let parent = registered
        .genome(&payload.parent_genome_id)
        .ok_or_else(|| {
            ControlError::Projection("Forge proposal parent is not registered".to_owned())
        })?;
    let child = registered.genome(&payload.child.genome_id).ok_or_else(|| {
        ControlError::Projection("Forge proposal child is not registered".to_owned())
    })?;
    let mut expected_source: serde_json::Value =
        serde_json::from_slice(parent.compiled().canonical_json())?;
    let object = expected_source
        .as_object_mut()
        .ok_or(ControlError::Protocol("Forge parent Genome is invalid"))?;
    object.insert(
        "name".to_owned(),
        serde_json::Value::String(format!(
            "{}-forge-{}",
            parent.record().name,
            payload.proposal_id
        )),
    );
    object.insert(
        "parents".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(
            payload.parent_genome_id.clone(),
        )]),
    );
    object
        .get_mut("artifacts")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(ControlError::Protocol("Forge parent artifacts are invalid"))?
        .insert(
            "agent.prompt".to_owned(),
            serde_json::Value::String(payload.prompt_artifact_after.clone()),
        );
    let source = serde_json::to_string(&expected_source)?;
    let parents = registered
        .genomes()
        .filter(|genome| genome.record().world_id == payload.world_id)
        .map(|genome| (genome.record().genome_id.clone(), genome.compiled().clone()))
        .collect::<BTreeMap<_, _>>();
    let expected = compile_genome(&source, SourceFormat::Json, world, &parents, artifacts)
        .map_err(|_| {
            ControlError::Projection("Forge child source no longer compiles".to_owned())
        })?;
    if expected.id() != child.record().genome_id
        || expected.canonical_json() != child.compiled().canonical_json()
    {
        return Err(ControlError::Projection(
            "Forge child changes more than its single proposed prompt mutation".to_owned(),
        ));
    }
    Ok(())
}

fn existing_forge_response(
    history: &[StoredEvent],
    payload: &ForgeProposalPayload,
) -> Result<Option<ResponseData>, ExecuteError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.proposed")
    {
        let existing = decode_forge_proposal(event).map_err(|_| ExecuteError::Internal)?;
        if existing.proposal_id == payload.proposal_id {
            if existing != *payload {
                return Err(ExecuteError::Rejected(
                    "proposal_id is already bound to different proposal content".to_owned(),
                ));
            }
            return Ok(Some(ResponseData::ForgeProposal {
                proposal: Box::new(forge_proposal_record(existing, event)),
            }));
        }
    }
    Ok(None)
}

fn validate_forge_event(
    event: &StoredEvent,
    payload: &ForgeProposalPayload,
) -> Result<(), ControlError> {
    if payload.schema_version != 1
        || event.actor != OPERATOR_ACTOR
        || event.event_type != "forge.proposed"
        || event.event_id != forge_event_id(&payload.proposal_id)
        || event.aggregate_id != forge_aggregate_id(&payload.proposal_id)
    {
        return Err(ControlError::Projection(
            "Forge proposal event identity is invalid".to_owned(),
        ));
    }
    validate_job_id(&payload.proposal_id)
        .map_err(|_| ControlError::Projection("Forge proposal id is invalid".to_owned()))?;
    validate_hypothesis(&payload.hypothesis)
        .map_err(|_| ControlError::Projection("Forge hypothesis is invalid".to_owned()))?;
    Ok(())
}

fn decode_forge_proposal(event: &StoredEvent) -> Result<ForgeProposalPayload, ControlError> {
    let payload = serde_json::from_slice::<ForgeProposalPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("Forge proposal payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "Forge proposal payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `TECH_DEBT.md` TD-16).
fn verify_forge_assessment_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_forge_assessment_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_forge_assessment_history`]; see [`EvidenceCache`].
fn verify_forge_assessment_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
    {
        if cache.contains("forge_assessment", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let payload = decode_forge_assessment(event)?;
        if payload.schema_version != 1
            || event.actor != OPERATOR_ACTOR
            || event.event_id != forge_assessment_event_id(&payload.assessment_id)
            || event.aggregate_id != forge_aggregate_id(&payload.proposal_id)
        {
            return Err(ControlError::Projection(
                "Forge assessment event identity is invalid".to_owned(),
            ));
        }
        validate_job_id(&payload.assessment_id)
            .map_err(|_| ControlError::Projection("Forge assessment id is invalid".to_owned()))?;
        validate_job_id(&payload.proposal_id).map_err(|_| {
            ControlError::Projection("Forge assessment proposal id is invalid".to_owned())
        })?;
        let expected = forge_assessment_payload(
            artifacts,
            index,
            registered,
            &payload.assessment_id,
            &payload.proposal_id,
            &payload.selection_event_id,
        )
        .map_err(|_| ControlError::Projection("Forge assessment evidence is invalid".to_owned()))?;
        let selection_event = index.get(&payload.selection_event_id).ok_or_else(|| {
            ControlError::Projection("Forge assessment selection is missing".to_owned())
        })?;
        if selection_event.sequence >= event.sequence || payload != expected {
            return Err(ControlError::Projection(
                "Forge assessment differs from verified evidence".to_owned(),
            ));
        }
        cache.insert("forge_assessment", event);
    }
    Ok(())
}

fn decode_forge_assessment(event: &StoredEvent) -> Result<ForgeAssessmentPayload, ControlError> {
    let payload = serde_json::from_slice::<ForgeAssessmentPayload>(&event.payload)
        .map_err(|_| ControlError::Projection("Forge assessment payload is invalid".to_owned()))?;
    let canonical_value = serde_json::to_value(&payload)?;
    if serde_json::to_vec(&canonical_value)? != event.payload {
        return Err(ControlError::Projection(
            "Forge assessment payload is not canonical".to_owned(),
        ));
    }
    Ok(payload)
}

fn forge_proposal_record(
    payload: ForgeProposalPayload,
    event: &StoredEvent,
) -> ForgeProposalRecord {
    ForgeProposalRecord {
        payload,
        event: ForgeProposalEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_hash: hex_encode(&event.hash),
        },
        promotion_eligible: false,
    }
}

fn forge_assessment_event_id(assessment_id: &str) -> String {
    format!("forge-assessment:{assessment_id}:recorded")
}

fn forge_assessment_record(
    payload: ForgeAssessmentPayload,
    event: &StoredEvent,
) -> ForgeAssessmentRecord {
    ForgeAssessmentRecord {
        payload,
        event: ForgeAssessmentEventRecord {
            sequence: event.sequence,
            event_id: event.event_id.clone(),
            aggregate_id: event.aggregate_id.clone(),
            event_hash: hex_encode(&event.hash),
        },
    }
}

fn existing_forge_assessment_response(
    history: &[StoredEvent],
    assessment_id: &str,
    proposal_id: &str,
    selection_event_id: &str,
) -> Result<Option<ResponseData>, ExecuteError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
    {
        let existing = decode_forge_assessment(event).map_err(|_| ExecuteError::Internal)?;
        if existing.assessment_id == assessment_id {
            if existing.proposal_id != proposal_id
                || existing.selection_event_id != selection_event_id
            {
                return Err(ExecuteError::Rejected(
                    "assessment_id is already bound to different assessment content".to_owned(),
                ));
            }
            return Ok(Some(ResponseData::ForgeAssessment {
                assessment: Box::new(forge_assessment_record(existing, event)),
            }));
        }
    }
    Ok(None)
}

fn forge_assessment_payload(
    artifacts: &dyn ArtifactBackend,
    index: &EventIndex<'_>,
    registered: &RegisteredObjects,
    assessment_id: &str,
    proposal_id: &str,
    selection_event_id: &str,
) -> Result<ForgeAssessmentPayload, ExecuteError> {
    let proposal_event_id = forge_event_id(proposal_id);
    let proposal_event = index
        .get(&proposal_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let proposal = decode_forge_proposal(proposal_event).map_err(|_| ExecuteError::Internal)?;
    validate_forge_event(proposal_event, &proposal).map_err(|_| ExecuteError::Internal)?;
    if proposal.proposal_id != proposal_id {
        return Err(ExecuteError::Internal);
    }

    let selection_event = index
        .get(selection_event_id)
        .ok_or(ExecuteError::NotFound)?;
    if selection_event.event_type != "selection.recorded" {
        return Err(ExecuteError::Rejected(
            "selection_event_id does not identify a selection".to_owned(),
        ));
    }
    let (routed_evaluation_id, routed_world_id) =
        selection_event_references(selection_event).map_err(|_| ExecuteError::Internal)?;
    let world = registered
        .world(&routed_world_id)
        .ok_or(ExecuteError::Internal)?;
    let selected = verify_selection_event_in(index, artifacts, selection_event, world.compiled())
        .map_err(|_| ExecuteError::Internal)?;
    let receipt = selected.receipt().clone();
    let verified_selection_event_id = selected.event().event_id.clone();
    let selection_event_hash = selected.event().event_hash.clone();
    let selection_receipt_artifact_id = selected.event().receipt_artifact_id.clone();
    let selection_sequence = selected.event().sequence;

    let evaluation_event = index
        .get(receipt.evaluation_event_id())
        .ok_or(ExecuteError::Internal)?;
    if evaluation_event.event_type != "evaluation.recorded"
        || hex_encode(&evaluation_event.hash) != receipt.evaluation_event_hash()
    {
        return Err(ExecuteError::Internal);
    }
    if proposal_event.sequence >= evaluation_event.sequence
        || evaluation_event.sequence >= selection_sequence
    {
        return Err(ExecuteError::Rejected(
            "Forge assessment evidence is out of order".to_owned(),
        ));
    }
    if routed_evaluation_id != receipt.evaluation_id()
        || routed_world_id != receipt.world_id()
        || receipt.world_id() != proposal.world_id
        || receipt.parent_genome_id() != proposal.parent_genome_id
        || receipt.candidate_genome_id() != proposal.child.genome_id
    {
        return Err(ExecuteError::Rejected(
            "selection evidence does not match the proposed child".to_owned(),
        ));
    }

    Ok(ForgeAssessmentPayload {
        schema_version: 1,
        assessment_id: assessment_id.to_owned(),
        proposal_id: proposal_id.to_owned(),
        proposal_event_id: proposal_event.event_id.clone(),
        proposal_event_hash: hex_encode(&proposal_event.hash),
        selection_event_id: verified_selection_event_id,
        selection_event_hash,
        selection_receipt_artifact_id,
        evaluation_id: receipt.evaluation_id().to_owned(),
        evaluation_event_id: receipt.evaluation_event_id().to_owned(),
        evaluation_event_hash: receipt.evaluation_event_hash().to_owned(),
        world_id: receipt.world_id().to_owned(),
        parent_genome_id: receipt.parent_genome_id().to_owned(),
        child_genome_id: receipt.candidate_genome_id().to_owned(),
        outcome: if receipt.metrics_eligible() {
            ForgeAssessmentOutcome::MetricsPassed
        } else {
            ForgeAssessmentOutcome::MetricsRejected
        },
        invariant_gate_verified: false,
        promotion_eligible: false,
    })
}

fn forge_event_id(proposal_id: &str) -> String {
    format!("forge:{proposal_id}:proposed")
}

fn forge_aggregate_id(proposal_id: &str) -> String {
    format!("forge:{proposal_id}")
}

// Every identifier one evolution generation derives is `evolve-{run_id}-g{n}-<tag>`,
// which satisfies `validate_job_id` (alphanumeric, `-`, `_`, `.` only) exactly
// like any other caller-selected idempotency key, so each of these can be
// submitted through the ordinary Arena, Forge, and Champion command paths.
fn evolution_diagnostic_evaluation_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-d")
}

fn evolution_child_evaluation_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-c")
}

fn evolution_proposal_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-p")
}

fn evolution_assessment_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-a")
}

fn evolution_promotion_transition_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-x")
}

fn evolution_analysis_id(run_id: &str, generation_index: u32) -> String {
    format!("evolve-{run_id}-g{generation_index}-analysis")
}

// TD-17 (roadmap items 10, 13): a generation's rank-0 candidate keeps
// exactly today's unsuffixed id (so a single-candidate generation --
// candidate_count == 1, or no bound strategy -- is byte-for-byte identical
// to a generation recorded before multi-candidate proposals existed); every
// other ranked candidate gets a distinct, still `validate_job_id`-legal
// suffix.
fn evolution_candidate_proposal_id(run_id: &str, generation_index: u32, rank: u32) -> String {
    let base = evolution_proposal_id(run_id, generation_index);
    if rank == 0 {
        base
    } else {
        format!("{base}-c{rank}")
    }
}

fn evolution_candidate_child_evaluation_id(
    run_id: &str,
    generation_index: u32,
    rank: u32,
) -> String {
    let base = evolution_child_evaluation_id(run_id, generation_index);
    if rank == 0 {
        base
    } else {
        format!("{base}-c{rank}")
    }
}

fn evolution_candidate_assessment_id(run_id: &str, generation_index: u32, rank: u32) -> String {
    let base = evolution_assessment_id(run_id, generation_index);
    if rank == 0 {
        base
    } else {
        format!("{base}-c{rank}")
    }
}

fn validate_hypothesis(hypothesis: &str) -> Result<(), ExecuteError> {
    if hypothesis.trim().is_empty()
        || hypothesis.len() > 512
        || hypothesis.chars().any(char::is_control)
    {
        return Err(ExecuteError::Invalid(
            "hypothesis must be 1 to 512 printable UTF-8 bytes",
        ));
    }
    Ok(())
}

fn verified_prompt_bytes(
    artifacts: &dyn ArtifactBackend,
    artifact_id: &str,
) -> Result<Vec<u8>, ControlError> {
    let id = ArtifactId::parse(artifact_id.to_owned())?;
    Ok(artifacts.get(&id)?)
}

fn reference_instruction_operation(instruction: ReferenceInstruction) -> &'static str {
    instruction.operation_name()
}

fn reference_instruction_document(instruction: ReferenceInstruction) -> String {
    format!(
        "```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{}\"}}\n```",
        reference_instruction_operation(instruction)
    )
}

fn compile_forge_child(
    registered: &RegisteredObjects,
    world: &CompiledWorld,
    artifact_store: &dyn ArtifactBackend,
    parent_genome_id: &str,
    world_id: &str,
    proposal_id: &str,
    prompt_artifact: &str,
) -> Result<GenomeRecord, ExecuteError> {
    let parent = registered
        .genome(parent_genome_id)
        .ok_or(ExecuteError::NotFound)?;
    let mut source: serde_json::Value = serde_json::from_slice(parent.compiled().canonical_json())
        .map_err(|_| ExecuteError::Internal)?;
    let object = source.as_object_mut().ok_or(ExecuteError::Internal)?;
    object.insert(
        "name".to_owned(),
        serde_json::Value::String(format!("{}-forge-{proposal_id}", parent.record().name)),
    );
    object.insert(
        "parents".to_owned(),
        serde_json::Value::Array(vec![serde_json::Value::String(parent_genome_id.to_owned())]),
    );
    object
        .get_mut("artifacts")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(ExecuteError::Internal)?
        .insert(
            "agent.prompt".to_owned(),
            serde_json::Value::String(prompt_artifact.to_owned()),
        );
    let source = serde_json::to_string(&source).map_err(|_| ExecuteError::Internal)?;
    let parents = registered
        .genomes()
        .filter(|genome| genome.record().world_id == world_id)
        .map(|genome| (genome.record().genome_id.clone(), genome.compiled().clone()))
        .collect::<BTreeMap<_, _>>();
    let child = compile_genome(&source, SourceFormat::Json, world, &parents, artifact_store)
        .map_err(|error| ExecuteError::Rejected(format!("Forge child rejected: {error}")))?;
    let artifact = artifact_store
        .put(child.canonical_json())
        .map_err(|_| ExecuteError::Internal)?;
    Ok(GenomeRecord {
        genome_id: child.id().to_owned(),
        name: child.name().to_owned(),
        world_id: world_id.to_owned(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: child.parents().to_vec(),
    })
}

fn verified_forge_source(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    selection_event_id: &str,
    parent_genome_id: &str,
) -> Result<(String, String, String), ExecuteError> {
    let event = history
        .iter()
        .find(|event| event.event_id == selection_event_id)
        .ok_or(ExecuteError::NotFound)?;
    if event.event_type != "selection.recorded" {
        return Err(ExecuteError::Rejected(
            "selection_event_id does not identify a selection".to_owned(),
        ));
    }
    let (evaluation_id, world_id) =
        selection_event_references(event).map_err(|_| ExecuteError::Internal)?;
    let world = registered.world(&world_id).ok_or(ExecuteError::Internal)?;
    let selection = verify_selection_event_in(
        &EventIndex::build(history),
        artifacts,
        event,
        world.compiled(),
    )
    .map_err(|_| ExecuteError::Internal)?;
    let receipt = selection.receipt().clone();
    let selection_hash = selection.event().event_hash.clone();
    if receipt.evaluation_id() != evaluation_id
        || receipt.world_id() != world_id
        || receipt.candidate_genome_id() != parent_genome_id
    {
        return Err(ExecuteError::Rejected(
            "the parent must be the selected candidate under the same World".to_owned(),
        ));
    }
    Ok((selection_hash, evaluation_id, world_id))
}

/// The two mutually exclusive ways a Forge proposal supplies its hypothesis.
enum ForgeHypothesisSource {
    /// The unchanged operator-authored path.
    Operator(String),
    /// A verified `forge.clustered` analysis and cluster index within it.
    Analysis {
        analysis_id: String,
        cluster_index: u32,
        /// Use the bound cluster's `secondary_suggested_mutation` instead of
        /// its primary `suggested_mutation` (TD-17, roadmap items 10, 13).
        /// Always `false` outside a `candidate_count > 1` generation's
        /// non-highest-ranked candidates.
        use_secondary: bool,
    },
}

/// Parses a bare reference-operation name (as recorded in a `ForgeProposalPayload`,
/// `SuggestedMutation::ReferenceOperation`, or a Gene's `operation_after`)
/// into a [`ReferenceInstruction`] by round-tripping it through the same
/// strict document parser every prompt artifact uses.
fn parse_reference_operation_name(operation: &str) -> Result<ReferenceInstruction, ExecuteError> {
    let document = format!(
        "```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{operation}\"}}\n```"
    );
    ReferenceInstruction::parse(&document).map_err(|_| ExecuteError::Internal)
}

/// Best-effort lookup of a registered Genome's `agent.prompt` reference
/// operation. Returns `None` for any Genome without a supported prompt
/// (unknown Genome, no `agent.prompt` artifact, unreadable or unparsable
/// content) rather than failing: callers use this only as a deterministic
/// hint for choosing a mutation target, and every choice it feeds into is
/// independently re-verified against catalog edges and canonical bytes.
fn genome_reference_instruction(
    registered: &RegisteredObjects,
    artifacts: &dyn ArtifactBackend,
    genome_id: &str,
) -> Option<ReferenceInstruction> {
    let genome = registered.genome(genome_id)?;
    let artifact = genome.compiled().artifact_id("agent.prompt")?;
    let id = ArtifactId::parse(artifact.to_owned()).ok()?;
    let bytes = artifacts.get(&id).ok()?;
    let body = std::str::from_utf8(&bytes).ok()?;
    ReferenceInstruction::parse(body).ok()
}

/// Deterministic Gene Bank lookup for `EvolverStrategyConfig::gene_selection
/// == HighestTransferEffect` (roadmap items 8, 10, 13): among every
/// extracted Gene whose `operation_before` equals `current_operation`,
/// returns the `operation_after` of the one with the highest mean
/// `estimate_bps` across its `Positive`-outcome transfer trials, requiring
/// at least one such trial. Ties break on the lexicographically smallest
/// `gene_id` for determinism. Returns `None` when no such Gene exists.
fn best_gene_target_operation(history: &[StoredEvent], current_operation: &str) -> Option<String> {
    let mut best: Option<(i64, String, String)> = None; // (mean_bps, gene_id, operation_after)
    for gene_event in history
        .iter()
        .filter(|event| event.event_type == GENE_EVENT_TYPE)
    {
        let Ok(gene) = decode_gene_extracted(gene_event) else {
            continue;
        };
        if gene.operation_before != current_operation {
            continue;
        }
        let mut total: i64 = 0;
        let mut count: i64 = 0;
        for transfer_event in history
            .iter()
            .filter(|event| event.event_type == TRANSFER_RECORDED_EVENT_TYPE)
        {
            let Ok(recorded) = decode_transfer_recorded(transfer_event) else {
                continue;
            };
            if recorded.gene_id != gene.gene_id || recorded.outcome != GeneTransferOutcome::Positive
            {
                continue;
            }
            total += recorded.estimate_bps;
            count += 1;
        }
        if count == 0 {
            continue;
        }
        let mean = total / count;
        let better = best.as_ref().is_none_or(|(best_mean, best_gene_id, _)| {
            mean > *best_mean || (mean == *best_mean && gene.gene_id < *best_gene_id)
        });
        if better {
            best = Some((mean, gene.gene_id, gene.operation_after));
        }
    }
    best.map(|(_, _, operation_after)| operation_after)
}

/// Resolves the hypothesis text and, for an analysis-derived proposal, the
/// binding recorded in the proposal payload plus an explicit mutation target
/// when the bound cluster names one. This never proposes, mutates, or
/// promotes anything; it only reads and verifies already-recorded evidence.
fn resolve_forge_hypothesis(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    world: &CompiledWorld,
    evaluation_id: &str,
    parent_genome_id: &str,
    source: ForgeHypothesisSource,
) -> Result<
    (
        String,
        Option<ForgeAnalysisBinding>,
        Option<ReferenceInstruction>,
    ),
    ExecuteError,
> {
    match source {
        ForgeHypothesisSource::Operator(hypothesis) => {
            validate_hypothesis(&hypothesis)?;
            Ok((hypothesis, None, None))
        }
        ForgeHypothesisSource::Analysis {
            analysis_id,
            cluster_index,
            use_secondary,
        } => {
            let event_id = format!("{CLUSTER_EVENT_PREFIX}{analysis_id}:clustered");
            let event = history
                .iter()
                .find(|event| event.event_id == event_id)
                .ok_or(ExecuteError::NotFound)?;
            let current_operation =
                genome_reference_instruction(registered, artifacts, parent_genome_id)
                    .map(ReferenceInstruction::operation_name);
            let verified = verify_cluster_event_in(
                &EventIndex::build(history),
                artifacts,
                event,
                world,
                current_operation,
            )
            .map_err(|_| ExecuteError::Internal)?;
            let analysis = verified.analysis().clone();
            let analysis_event_hash = verified.event().event_hash.clone();
            if analysis.evaluation_id != evaluation_id
                || analysis.candidate_genome_id != parent_genome_id
            {
                // Forge's "parent" for this proposal is the winning candidate
                // from the analyzed evaluation: the same role `verified_forge_source`
                // requires the selection's `candidate_genome_id` to match.
                return Err(ExecuteError::Rejected(
                    "the bound analysis must have clustered the exact same selected candidate"
                        .to_owned(),
                ));
            }
            let cluster = analysis
                .clusters
                .get(usize::try_from(cluster_index).map_err(|_| ExecuteError::NotFound)?)
                .ok_or(ExecuteError::NotFound)?;
            let chosen_mutation = if use_secondary {
                cluster.secondary_suggested_mutation.as_ref()
            } else {
                cluster.suggested_mutation.as_ref()
            };
            let target = match chosen_mutation {
                Some(SuggestedMutation::ReferenceOperationFlip) => None,
                Some(SuggestedMutation::ReferenceOperation { operation_after }) => {
                    Some(parse_reference_operation_name(operation_after)?)
                }
                None => {
                    return Err(ExecuteError::Rejected(
                        "the selected cluster has no supported mutation".to_owned(),
                    ));
                }
            };
            let mutation_slot = use_secondary.then_some(MutationSlot::Secondary);
            Ok((
                cluster.hypothesis.clone(),
                Some(ForgeAnalysisBinding {
                    analysis_id,
                    analysis_event_id: event_id,
                    analysis_event_hash,
                    cluster_index,
                    cluster_signature: cluster.signature.clone(),
                    mutation_slot,
                }),
                target,
            ))
        }
    }
}

/// Proposes (or validates an explicit) one-step mutation of `parent_genome_id`'s
/// `agent.prompt` artifact.
///
/// Authorization is enforced here, on the propose path only: a proposal for
/// `agent.prompt` requires `MutationTarget::Harness` in the World's
/// `mutation_scope` (roadmap items 8, 10, 13). Replay of an already-recorded
/// `forge.proposed` event does not re-check World scope (see
/// `verify_forge_prompt`), only that the recorded edge is a representable
/// catalog edge, so proposals recorded before this field existed still
/// verify.
///
/// `target`, when supplied, must be a representable [`mutation_catalog`]
/// edge from the parent's current operation (any of the 16 reference
/// operations); this is how a cluster- or Gene-derived hypothesis reaches a
/// Gauntlet fix, not only the casing flip. Without `target`, the default
/// one-step mutation is the historical `identity`/`ascii_uppercase` flip; any
/// other current operation is rejected as outside Forge's default mutation
/// (an explicit `target` is required to mutate a Gauntlet operation).
fn forge_prompt_mutation(
    artifacts: &dyn ArtifactBackend,
    registered: &RegisteredObjects,
    parent_genome_id: &str,
    world: &CompiledWorld,
    target: Option<ReferenceInstruction>,
) -> Result<(String, ReferenceInstruction, ReferenceInstruction, String), ExecuteError> {
    if !world.mutation_scope().contains(&MutationTarget::Harness) {
        return Err(ExecuteError::Rejected(
            "World mutation scope does not authorize harness mutations".to_owned(),
        ));
    }
    let parent = registered
        .genome(parent_genome_id)
        .filter(|genome| genome.record().world_id == world.id())
        .ok_or(ExecuteError::NotFound)?;
    let prompt_before = parent
        .compiled()
        .artifact_id("agent.prompt")
        .ok_or_else(|| {
            ExecuteError::Rejected(
                "the selected candidate has no supported prompt to mutate".to_owned(),
            )
        })?
        .to_owned();
    let prompt_id = ArtifactId::parse(prompt_before.clone()).map_err(|_| ExecuteError::Internal)?;
    let prompt_bytes = artifacts
        .get(&prompt_id)
        .map_err(|_| ExecuteError::Internal)?;
    let prompt_text = std::str::from_utf8(&prompt_bytes).map_err(|_| ExecuteError::Internal)?;
    let before = ReferenceInstruction::parse(prompt_text).map_err(|_| {
        ExecuteError::Rejected(
            "the selected candidate prompt is outside the supported mutation language".to_owned(),
        )
    })?;
    let after = match target {
        Some(explicit) => {
            if !is_catalog_edge(before.operation_name(), explicit.operation_name()) {
                return Err(ExecuteError::Rejected(
                    "the requested mutation target is not a representable catalog edge".to_owned(),
                ));
            }
            explicit
        }
        None => match before {
            ReferenceInstruction::Identity => ReferenceInstruction::AsciiUppercase,
            ReferenceInstruction::AsciiUppercase => ReferenceInstruction::Identity,
            // Every Gauntlet-mode operation (roadmap item 10) needs an
            // explicit catalog-derived target; there is no default flip for
            // it.
            _ => {
                return Err(ExecuteError::Rejected(
                    "the selected candidate prompt is outside the Forge mutation scope".to_owned(),
                ));
            }
        },
    };
    let after_text =
        mutate_reference_instruction_document(prompt_text, before, after).map_err(|()| {
            ExecuteError::Rejected(
                "the selected candidate prompt is outside the Forge mutation scope".to_owned(),
            )
        })?;
    Ok((prompt_before, before, after, after_text))
}

fn mutate_reference_instruction_document(
    prompt_text: &str,
    before: ReferenceInstruction,
    after: ReferenceInstruction,
) -> Result<String, ()> {
    let normalized = prompt_text.replace("\r\n", "\n");
    let canonical = reference_instruction_document(before);
    if normalized != canonical && normalized != format!("{canonical}\n") {
        return Err(());
    }
    let old = format!(
        "\"operation\":\"{}\"",
        reference_instruction_operation(before)
    );
    let new = format!(
        "\"operation\":\"{}\"",
        reference_instruction_operation(after)
    );
    if prompt_text.matches(&old).count() != 1 {
        return Err(());
    }
    Ok(prompt_text.replacen(&old, &new, 1))
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `TECH_DEBT.md` TD-16).
fn verify_selection_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_selection_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_selection_history`]; see [`EvidenceCache`].
fn verify_selection_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for event in history
        .iter()
        .filter(|event| event.event_type == "selection.recorded")
    {
        if cache.contains("selection", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        // The World identity in the event envelope is only a routing hint. Arena
        // independently recomputes the evaluation receipt and compares the full
        // canonical selection event against this registered World's policy.
        let (_evaluation_id, world_id) = selection_event_references(event).map_err(|_| {
            ControlError::Projection("canonical selection event is invalid".to_owned())
        })?;
        let world = registered.world(&world_id).ok_or_else(|| {
            ControlError::Projection("selection World is not registered".to_owned())
        })?;
        verify_selection_event_in(index, artifacts, event, world.compiled()).map_err(|_| {
            ControlError::Projection("canonical selection receipt is invalid".to_owned())
        })?;
        cache.insert("selection", event);
    }
    Ok(())
}

/// `invariant_event_references` already canonically validates the full
/// envelope (schema version, identity fields, artifact-id shape); this only
/// pulls `receipt_artifact_id` back out for the reference-level cross-check
/// in `verify_invariant_history_with` (see `TECH_DEBT.md` TD-4).
#[derive(Deserialize)]
struct InvariantEventEnvelope {
    receipt_artifact_id: String,
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `TECH_DEBT.md` TD-16).
fn verify_invariant_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    verify_invariant_history_with(
        artifacts,
        history,
        registered,
        &mut EvidenceCache::default(),
    )
}

/// Cache-aware counterpart of [`verify_invariant_history`]; see [`EvidenceCache`].
fn verify_invariant_history_with(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    cache: &mut EvidenceCache,
) -> Result<(), ControlError> {
    // Built only on a cache miss, so a fully warm refresh (the common case)
    // never pays the O(history length) cost of indexing it.
    let mut index = None;
    for event in history.iter().filter(|event| {
        event.event_type == "invariants.recorded"
            || event.event_id.starts_with("arena:invariants:")
            || event.aggregate_id.starts_with("arena:invariants:")
    }) {
        if cache.contains("invariant", event) {
            continue;
        }
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let (evaluation_id, world_id) = invariant_event_references(event).map_err(|_| {
            ControlError::Projection("canonical invariant event envelope is invalid".to_owned())
        })?;
        let envelope: InvariantEventEnvelope =
            serde_json::from_slice(&event.payload).map_err(|_| {
                ControlError::Projection("canonical invariant event envelope is invalid".to_owned())
            })?;
        let world = registered.world(&world_id).ok_or_else(|| {
            ControlError::Projection("invariant World is not registered".to_owned())
        })?;
        let verified =
            verify_reference_output_invariant_event_in(index, artifacts, event, world.compiled())
                .map_err(|_| {
                ControlError::Projection("canonical invariant receipt is invalid".to_owned())
            })?;
        if verified.receipt().evaluation_id != evaluation_id
            || verified.receipt().world_id != world_id
            || verified.event().receipt_artifact_id != envelope.receipt_artifact_id
        {
            return Err(ControlError::Projection(
                "canonical invariant event differs from verified receipt".to_owned(),
            ));
        }
        cache.insert("invariant", event);
    }
    Ok(())
}

/// `cluster_event_references` already canonically validates the full
/// envelope (schema version, identity fields, artifact-id shape); this only
/// pulls `analysis_artifact_id` back out for the reference-level cross-check
/// in `verify_cluster_history` (see `TECH_DEBT.md` TD-4).
#[derive(Deserialize)]
struct ClusterEventEnvelope {
    analysis_artifact_id: String,
}

/// `artifacts` is the daemon's already-open artifact store, reused for every
/// event instead of reopening it (see `TECH_DEBT.md` TD-16).
fn verify_cluster_history(
    artifacts: &dyn ArtifactBackend,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    // Built only when at least one cluster event exists, so the common case
    // (no cluster analyses recorded yet) never pays the O(history length)
    // cost of indexing it.
    let mut index = None;
    for event in history.iter().filter(|event| {
        event.event_type == "forge.clustered"
            || event.event_id.starts_with(CLUSTER_EVENT_PREFIX)
            || event.aggregate_id.starts_with(CLUSTER_EVENT_PREFIX)
    }) {
        let index = index.get_or_insert_with(|| EventIndex::build(history));
        let (_analysis_id, evaluation_id, world_id) =
            cluster_event_references(event).map_err(|_| {
                ControlError::Projection("canonical cluster event envelope is invalid".to_owned())
            })?;
        let envelope: ClusterEventEnvelope =
            serde_json::from_slice(&event.payload).map_err(|_| {
                ControlError::Projection("canonical cluster event envelope is invalid".to_owned())
            })?;
        let world = registered.world(&world_id).ok_or_else(|| {
            ControlError::Projection("cluster World is not registered".to_owned())
        })?;
        // Re-derive the candidate's current operation the same deterministic
        // way the control plane did when the analysis was first computed
        // (roadmap items 8, 10, 13), so a `failure-cluster-v2` analysis
        // recomputes byte-identically on replay.
        let current_operation = load_recorded_evaluation_in(index, artifacts, &evaluation_id)
            .ok()
            .and_then(|recorded| {
                genome_reference_instruction(
                    registered,
                    artifacts,
                    &recorded.summary.candidate_genome_id,
                )
            })
            .map(ReferenceInstruction::operation_name);
        let verified =
            verify_cluster_event_in(index, artifacts, event, world.compiled(), current_operation)
                .map_err(|_| {
                ControlError::Projection("canonical cluster analysis is invalid".to_owned())
            })?;
        if verified.analysis().evaluation_id != evaluation_id
            || verified.analysis().world_id != world_id
            || verified.event().analysis_artifact_id != envelope.analysis_artifact_id
        {
            return Err(ControlError::Projection(
                "canonical cluster event differs from verified analysis".to_owned(),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn require_command_fields(command: &Command) -> Result<(), ExecuteError> {
    if let Command::GenomeShow { genome_id } | Command::GenomePrompt { genome_id } = command
        && genome_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("genome_id is required"));
    }
    if let Command::WorldShow { world_id } | Command::GenomeRegister { world_id, .. } = command
        && world_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("world_id is required"));
    }
    if let Command::WorldRegister { path }
    | Command::GenomeRegister { path, .. }
    | Command::ArtifactPut { path } = command
        && path.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("path is required"));
    }
    if let Command::RunSubmit { job_id, genome_id } = command
        && (job_id.trim().is_empty() || genome_id.trim().is_empty())
    {
        return Err(ExecuteError::Invalid("job_id and genome_id are required"));
    }
    if let Command::GenomePropose {
        proposal_id,
        selection_event_id,
        parent_genome_id,
        hypothesis,
        analysis_id,
        cluster_index,
    } = command
    {
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        if selection_event_id.trim().is_empty() || parent_genome_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "selection_event_id and parent_genome_id are required",
            ));
        }
        match (hypothesis, analysis_id, cluster_index) {
            (Some(hypothesis), None, None) => validate_hypothesis(hypothesis)?,
            (None, Some(analysis_id), Some(_)) => {
                validate_job_id(analysis_id)
                    .map_err(|_| ExecuteError::Invalid("analysis_id is invalid"))?;
            }
            _ => {
                return Err(ExecuteError::Invalid(
                    "exactly one of hypothesis or (analysis_id and cluster_index) is required",
                ));
            }
        }
    }
    if let Command::GenomeAssess {
        assessment_id,
        proposal_id,
        selection_event_id,
    } = command
    {
        validate_job_id(assessment_id)
            .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))?;
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        if selection_event_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("selection_event_id is required"));
        }
    }
    if let Command::JobStatus { job_id } | Command::JobKill { job_id } = command
        && job_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("job_id is required"));
    }
    if let Command::RunReference { genome_id } | Command::RunEvaluation { genome_id, .. } = &command
        && genome_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("genome_id is required"));
    }
    if let Command::EvaluatePair {
        evaluation_id,
        parent_genome_id,
        candidate_genome_id,
        remote: _,
    } = command
        && (evaluation_id.trim().is_empty()
            || parent_genome_id.trim().is_empty()
            || candidate_genome_id.trim().is_empty())
    {
        return Err(ExecuteError::Invalid(
            "evaluation and Genome identifiers are required",
        ));
    }
    if let Command::ArenaSelect { evaluation_id } = command
        && evaluation_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("evaluation_id is required"));
    }
    if let Command::ArenaInvariants { evaluation_id } = command
        && evaluation_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("evaluation_id is required"));
    }
    if let Command::ForgeAnalyze {
        analysis_id,
        evaluation_id,
    } = command
    {
        validate_job_id(analysis_id)
            .map_err(|_| ExecuteError::Invalid("analysis_id is invalid"))?;
        if evaluation_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("evaluation_id is required"));
        }
    }
    if let Command::RunList { limit }
    | Command::EvaluationList { limit }
    | Command::DenialList { limit }
    | Command::MetaList { limit }
    | Command::DriftList { limit }
    | Command::CanaryList { limit } = command
        && (*limit == 0 || *limit > MAX_LIST_LIMIT)
    {
        return Err(ExecuteError::Invalid("limit must be between 1 and 200"));
    }
    if let Command::EvolveStart {
        run_id,
        world_id,
        from_genome_id,
        generations,
        budget,
        strategy_id: _,
    } = command
    {
        validate_job_id(run_id).map_err(|_| ExecuteError::Invalid("run_id is invalid"))?;
        if run_id.len() > MAX_EVOLUTION_RUN_ID_BYTES {
            return Err(ExecuteError::Invalid(
                "run_id must leave room for the run's derived identifiers",
            ));
        }
        if world_id.trim().is_empty() || from_genome_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "world_id and from_genome_id are required",
            ));
        }
        if *generations == 0 {
            return Err(ExecuteError::Invalid("generations must be positive"));
        }
        if *budget < TRIALS_PER_GENERATION {
            return Err(ExecuteError::Invalid(
                "budget must allow at least one generation",
            ));
        }
    }
    if let Command::EvolveStatus { run_id } | Command::EvolveCancel { run_id } = command
        && validate_job_id(run_id).is_err()
    {
        return Err(ExecuteError::Invalid("run_id is invalid"));
    }
    if let Command::McpCall {
        client_id,
        tool,
        decision,
        ..
    } = command
    {
        if client_id.trim().is_empty() || tool.trim().is_empty() {
            return Err(ExecuteError::Invalid("client_id and tool are required"));
        }
        if let McpDecision::Allowed { command: inner } = decision
            && matches!(**inner, Command::McpCall { .. })
        {
            return Err(ExecuteError::Invalid("mcp_call must not nest mcp_call"));
        }
    }
    if let Command::WorkerCredentialMint {
        worker_id,
        ttl_seconds,
    } = command
        && (worker_id.trim().is_empty()
            || *ttl_seconds == 0
            || *ttl_seconds > MAX_WORKER_TTL_SECONDS)
    {
        return Err(ExecuteError::Invalid(
            "worker_id is required and ttl_seconds must be between 1 and the maximum",
        ));
    }
    if let Command::WorkerCredentialRevoke { credential_id } = command
        && credential_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("credential_id is required"));
    }
    if let Command::RemoteRunSubmit { job_id, genome_id } = command {
        validate_job_id(job_id).map_err(|_| ExecuteError::Invalid("job_id is invalid"))?;
        if genome_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("genome_id is required"));
        }
    }
    if let Command::RemoteJobStatus { job_id } = command
        && job_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("job_id is required"));
    }
    if let Command::MetaStrategyRegister { path } = command
        && path.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("path is required"));
    }
    if let Command::MetaStrategyShow { strategy_id } = command
        && strategy_id.trim().is_empty()
    {
        return Err(ExecuteError::Invalid("strategy_id is required"));
    }
    if let Command::MetaEvaluate {
        meta_run_id,
        strategy_a_id,
        strategy_b_id,
        lineages,
        confidence_bps,
        bootstrap_seed: _,
    } = command
    {
        validate_job_id(meta_run_id)
            .map_err(|_| ExecuteError::Invalid("meta_run_id is invalid"))?;
        if meta_run_id.len() > MAX_META_RUN_ID_BYTES {
            return Err(ExecuteError::Invalid(
                "meta_run_id must leave room for its derived run identifiers",
            ));
        }
        if strategy_a_id.trim().is_empty() || strategy_b_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "strategy_a_id and strategy_b_id are required",
            ));
        }
        if strategy_a_id == strategy_b_id {
            return Err(ExecuteError::Invalid(
                "strategy_a_id and strategy_b_id must differ",
            ));
        }
        if lineages.len() < 2 {
            return Err(ExecuteError::Invalid(
                "at least two held-out lineages are required for a bootstrap comparison",
            ));
        }
        let mut worlds = std::collections::BTreeSet::new();
        for lineage in lineages {
            if lineage.world_id.trim().is_empty() || lineage.from_genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "lineage world_id and from_genome_id are required",
                ));
            }
            if !worlds.insert(lineage.world_id.clone()) {
                return Err(ExecuteError::Invalid(
                    "held-out lineages must use distinct Worlds",
                ));
            }
        }
        if *confidence_bps == 0 || *confidence_bps >= 10_000 {
            return Err(ExecuteError::Invalid(
                "confidence_bps must be between 1 and 9999",
            ));
        }
    }
    if let Command::MetaShow { meta_run_id } | Command::MetaStatus { meta_run_id } = command
        && validate_job_id(meta_run_id).is_err()
    {
        return Err(ExecuteError::Invalid("meta_run_id is invalid"));
    }
    require_champion_fields(command)?;
    require_drift_and_canary_fields(command)?;
    require_gene_fields(command)
}

fn require_gene_fields(command: &Command) -> Result<(), ExecuteError> {
    match command {
        Command::GeneExtract {
            gene_id,
            promotion_transition_id,
        } => {
            validate_job_id(gene_id).map_err(|_| ExecuteError::Invalid("gene_id is invalid"))?;
            validate_job_id(promotion_transition_id)
                .map_err(|_| ExecuteError::Invalid("promotion_transition_id is invalid"))?;
        }
        Command::GeneTransfer {
            trial_id,
            gene_id,
            to_genome_id,
        } => {
            validate_job_id(trial_id).map_err(|_| ExecuteError::Invalid("trial_id is invalid"))?;
            if gene_id.trim().is_empty() || to_genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "gene_id and to_genome_id are required",
                ));
            }
        }
        Command::GeneRecord {
            trial_id,
            evaluation_id,
        } => {
            validate_job_id(trial_id).map_err(|_| ExecuteError::Invalid("trial_id is invalid"))?;
            if evaluation_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("evaluation_id is required"));
            }
        }
        Command::GeneShow { gene_id } if gene_id.trim().is_empty() => {
            return Err(ExecuteError::Invalid("gene_id is required"));
        }
        Command::GeneSpeciate {
            species_id,
            gene_id,
            domain_world_id,
        } => {
            validate_job_id(species_id)
                .map_err(|_| ExecuteError::Invalid("species_id is invalid"))?;
            if gene_id.trim().is_empty() || domain_world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "gene_id and domain_world_id are required",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn require_champion_fields(command: &Command) -> Result<(), ExecuteError> {
    let transition_id = match command {
        Command::ChampionSeed {
            transition_id,
            world_id,
            genome_id,
            reason,
        } => {
            if world_id.trim().is_empty() || genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id and genome_id are required"));
            }
            validate_reason(reason)?;
            transition_id
        }
        Command::ChampionPromote {
            transition_id,
            assessment_id,
        } => {
            validate_job_id(assessment_id)
                .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))?;
            transition_id
        }
        Command::ChampionRollback {
            transition_id,
            world_id,
            reason,
        } => {
            if world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id is required"));
            }
            validate_reason(reason)?;
            transition_id
        }
        Command::ChampionShow { world_id } => {
            if world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id is required"));
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    validate_job_id(transition_id).map_err(|_| ExecuteError::Invalid("transition_id is invalid"))
}

fn require_drift_and_canary_fields(command: &Command) -> Result<(), ExecuteError> {
    match command {
        Command::DriftRecord {
            drift_id,
            world_id,
            evidence_evaluation_id,
            ..
        } => {
            validate_job_id(drift_id).map_err(|_| ExecuteError::Invalid("drift_id is invalid"))?;
            if world_id.trim().is_empty() {
                return Err(ExecuteError::Invalid("world_id is required"));
            }
            validate_job_id(evidence_evaluation_id)
                .map_err(|_| ExecuteError::Invalid("evidence_evaluation_id is invalid"))
        }
        Command::DriftShow { drift_id } => {
            validate_job_id(drift_id).map_err(|_| ExecuteError::Invalid("drift_id is invalid"))
        }
        Command::CanaryStart {
            canary_id,
            world_id,
            candidate_genome_id,
            assessment_id,
        } => {
            validate_job_id(canary_id)
                .map_err(|_| ExecuteError::Invalid("canary_id is invalid"))?;
            if world_id.trim().is_empty() || candidate_genome_id.trim().is_empty() {
                return Err(ExecuteError::Invalid(
                    "world_id and candidate_genome_id are required",
                ));
            }
            validate_job_id(assessment_id)
                .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))
        }
        Command::CanaryAdvance {
            canary_id,
            evidence_evaluation_id,
        }
        | Command::CanaryLiveCheck {
            canary_id,
            evidence_evaluation_id,
        } => {
            validate_job_id(canary_id)
                .map_err(|_| ExecuteError::Invalid("canary_id is invalid"))?;
            validate_job_id(evidence_evaluation_id)
                .map_err(|_| ExecuteError::Invalid("evidence_evaluation_id is invalid"))
        }
        Command::CanaryShow { canary_id } => {
            validate_job_id(canary_id).map_err(|_| ExecuteError::Invalid("canary_id is invalid"))
        }
        _ => Ok(()),
    }
}

fn source_format(path: &str) -> Result<SourceFormat, ExecuteError> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("json") => Ok(SourceFormat::Json),
        Some("yaml" | "yml") => Ok(SourceFormat::Yaml),
        _ => Err(ExecuteError::Invalid(
            "source path must end in .json, .yaml, or .yml",
        )),
    }
}

fn read_source_text(path: &str, limit: u64) -> Result<String, ExecuteError> {
    String::from_utf8(read_bounded_file(path, limit)?)
        .map_err(|_| ExecuteError::Invalid("source file is not UTF-8 text"))
}

fn read_bounded_file(path: &str, limit: u64) -> Result<Vec<u8>, ExecuteError> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(ExecuteError::Invalid("path must be absolute"));
    }
    let metadata = fs::metadata(path).map_err(|_| ExecuteError::Invalid("file is not readable"))?;
    if !metadata.is_file() {
        return Err(ExecuteError::Invalid("path is not a regular file"));
    }
    if metadata.len() > limit {
        return Err(ExecuteError::Invalid("file exceeds the size limit"));
    }
    fs::read(path).map_err(|_| ExecuteError::Invalid("file is not readable"))
}

struct ReferenceExecution {
    completion_reason: RunCompletionReason,
    latency_millis: u64,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    trace_artifact_ids: Vec<String>,
    /// Exact cost the provider reported, in micro-US-dollars. Always zero for
    /// the reference worker and for a provider stream that reports none.
    actual_cost_microusd: u64,
}

fn execute_async_reference(
    launch: AsyncReferenceLaunch,
    spec: &RunSpec,
    evidence: hephaestus_experience::ChannelEvidenceSink,
    initial_sequence: u64,
) -> Result<ReferenceExecution, String> {
    let AsyncReferenceLaunch {
        sandboxes,
        guardian,
        protected_paths,
        worker,
        cancel,
    } = launch;
    worker
        .verify()
        .map_err(|_| "reference worker identity check failed".to_owned())?;
    let manager = sandboxes.resolve()?;
    let (sandbox, token) = manager
        .create(spec)
        .map_err(|_| "sandbox could not be created".to_owned())?;
    let sandbox = SandboxCleanupGuard::new(sandbox);
    let runtime = SupervisedRuntime::deterministic_guarded(
        candidate_isolation(protected_paths),
        &worker.executable,
        [],
        &guardian,
    )
    .map_err(|_| "guarded worker could not be configured".to_owned())?;
    let mut runtime = RecordedRuntime::with_sink(runtime, evidence, initial_sequence);
    let result = (|| {
        runtime
            .start(
                spec,
                sandbox.sandbox().map_err(|_| "sandbox unavailable")?,
                &token,
            )
            .map_err(|_| "guarded worker did not start".to_owned())?;
        loop {
            if cancel.load(Ordering::Acquire) {
                runtime
                    .interrupt(spec.run_id())
                    .map_err(|_| "guarded worker did not confirm cancellation".to_owned())?;
            }
            let snapshot = runtime
                .snapshot(spec.run_id())
                .map_err(|_| "guarded worker status failed".to_owned())?;
            if snapshot.status != RunStatus::Running {
                let completion_reason = snapshot
                    .completion_reason
                    .ok_or_else(|| "terminal worker omitted completion reason".to_owned())?;
                let stdout = fs::read(&snapshot.stdout_path)
                    .map_err(|_| "worker output could not be read".to_owned())?;
                let stderr = fs::read(&snapshot.stderr_path)
                    .map_err(|_| "worker diagnostics could not be read".to_owned())?;
                let latency_millis = u64::try_from(snapshot.elapsed.as_millis())
                    .map_err(|_| "worker latency is invalid".to_owned())?;
                return Ok(ReferenceExecution {
                    completion_reason: map_run_completion_reason(completion_reason),
                    latency_millis,
                    stdout,
                    stderr,
                    trace_artifact_ids: runtime.trace_artifact_ids().to_vec(),
                    actual_cost_microusd: 0,
                });
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        // Contain every post-start failure before removing the private workspace.
        // `interrupt` waits for the guardian's worker process group to exit;
        // dropping the supervisor repeats this best-effort containment if trace
        // persistence itself prevented the normal interruption trace.
        let _ignored = runtime.interrupt(spec.run_id());
    }
    drop(runtime);
    sandbox
        .cleanup()
        .map_err(|_| "sandbox cleanup failed".to_owned())?;
    worker
        .verify()
        .map_err(|_| "reference worker identity changed".to_owned())?;
    result
}

struct AsyncProviderLaunch {
    sandboxes: SandboxManagerSource,
    guardian: PathBuf,
    protected_paths: Vec<PathBuf>,
    provider: Provider,
    executable: PathBuf,
    extra_env: Vec<(String, String)>,
    redaction: RedactionPolicy,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// Async counterpart of `execute_async_reference` for a Codex/Claude adapter:
/// same private sandbox, process-group supervision, cancellation polling, and
/// streamed evidence sink, but the terminal stdout goes through
/// `extract_final_answer`/`extract_actual_cost_microusd` and redaction the
/// same way the synchronous `execute_provider_runtime` path does.
fn execute_async_provider(
    launch: AsyncProviderLaunch,
    spec: &RunSpec,
    evidence: hephaestus_experience::ChannelEvidenceSink,
    initial_sequence: u64,
) -> Result<ReferenceExecution, String> {
    let AsyncProviderLaunch {
        sandboxes,
        guardian,
        protected_paths,
        provider,
        executable,
        extra_env,
        redaction,
        cancel,
    } = launch;
    let manager = sandboxes.resolve()?;
    let (sandbox, token) = manager
        .create(spec)
        .map_err(|_| "sandbox could not be created".to_owned())?;
    let sandbox = SandboxCleanupGuard::new(sandbox);
    let runtime = SupervisedRuntime::provider_guarded(
        candidate_isolation(protected_paths),
        provider,
        executable,
        &guardian,
        extra_env,
    )
    .map_err(|_| "guarded provider could not be configured".to_owned())?;
    let mut runtime = RecordedRuntime::with_sink(runtime, evidence, initial_sequence);
    let result = (|| {
        runtime
            .start(
                spec,
                sandbox.sandbox().map_err(|_| "sandbox unavailable")?,
                &token,
            )
            .map_err(|_| "guarded provider did not start".to_owned())?;
        loop {
            if cancel.load(Ordering::Acquire) {
                runtime
                    .interrupt(spec.run_id())
                    .map_err(|_| "guarded provider did not confirm cancellation".to_owned())?;
            }
            let snapshot = runtime
                .snapshot(spec.run_id())
                .map_err(|_| "guarded provider status failed".to_owned())?;
            if snapshot.status != RunStatus::Running {
                let completion_reason = snapshot
                    .completion_reason
                    .ok_or_else(|| "terminal provider omitted completion reason".to_owned())?;
                let raw_stdout = fs::read(&snapshot.stdout_path)
                    .map_err(|_| "provider output could not be read".to_owned())?;
                let raw_stderr = fs::read(&snapshot.stderr_path)
                    .map_err(|_| "provider diagnostics could not be read".to_owned())?;
                let latency_millis = u64::try_from(snapshot.elapsed.as_millis())
                    .map_err(|_| "provider latency is invalid".to_owned())?;
                let final_answer = extract_final_answer(provider, &raw_stdout);
                let actual_cost_microusd = extract_actual_cost_microusd(provider, &raw_stdout);
                let stdout = redact_bytes(&redaction, &final_answer);
                let stderr = redact_bytes(&redaction, &raw_stderr);
                return Ok(ReferenceExecution {
                    completion_reason: map_run_completion_reason(completion_reason),
                    latency_millis,
                    stdout,
                    stderr,
                    trace_artifact_ids: runtime.trace_artifact_ids().to_vec(),
                    actual_cost_microusd,
                });
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        let _ignored = runtime.interrupt(spec.run_id());
    }
    drop(runtime);
    sandbox
        .cleanup()
        .map_err(|_| "sandbox cleanup failed".to_owned())?;
    result
}

/// Opens one `SandboxManager` to share across every local trial of a paired
/// Arena evaluation, unless the job runs on a remote lease (which never
/// touches a local sandbox). `SandboxManager::open` re-validates the sandbox
/// root's device/inode identity and re-applies its permissions on every
/// call; a paired evaluation's handful of trials share the same root for the
/// life of one job, so opening it once instead of once per trial drops that
/// repeated, always-identical setup work from the critical path. `create()`
/// -- which still runs once per trial, exactly as before -- keeps its own
/// per-trial `validate_root` check, so every trial still gets its own
/// independently validated, freshly created sandbox. If this open fails
/// (never observed in practice: the sandbox root lives under the daemon's
/// own data directory), each trial falls back to opening its own manager
/// exactly as it did previously, so that unreachable path's behavior is
/// unchanged.
fn open_shared_sandboxes(
    needs_local_sandbox: bool,
    data_dir: &Path,
) -> Option<Arc<SandboxManager>> {
    needs_local_sandbox
        .then(|| SandboxManager::open(data_dir.join("sandboxes"), Duration::from_secs(30)).ok())
        .flatten()
        .map(Arc::new)
}

fn execute_async_arena_trials(launch: AsyncArenaTrialLaunch) {
    let AsyncArenaTrialLaunch {
        data_dir,
        guardian,
        protected_paths,
        worker,
        codex_executable,
        claude_executable,
        provider_extra_env,
        redaction,
        cancel,
        trials,
        evidence,
        messages,
        mut initial_sequence,
        job_id,
        remote_lease,
        overall_deadline,
    } = launch;
    let mut outcome = Ok(());
    let shared_sandboxes = open_shared_sandboxes(remote_lease.is_none(), &data_dir);
    for (index, trial) in trials.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            outcome = Err("paired evaluation was cancelled".to_owned());
            break;
        }
        let sandboxes = shared_sandboxes.clone().map_or_else(
            || SandboxManagerSource::OpenFresh(data_dir.clone()),
            SandboxManagerSource::Shared,
        );
        let output = if let Some(provider) = trial.provider {
            let executable = match provider {
                Provider::Codex => codex_executable.clone(),
                Provider::Claude => claude_executable.clone(),
                Provider::Deterministic => {
                    outcome = Err("paired trial has an invalid provider binding".to_owned());
                    break;
                }
            };
            execute_async_provider(
                AsyncProviderLaunch {
                    sandboxes,
                    guardian: guardian.clone(),
                    protected_paths: protected_paths.clone(),
                    provider,
                    executable,
                    extra_env: provider_extra_env.clone(),
                    redaction: redaction.clone(),
                    cancel: Arc::clone(&cancel),
                },
                &trial.spec,
                evidence.clone(),
                initial_sequence,
            )
        } else if let Some(lease) = remote_lease.as_deref() {
            execute_arena_trial_remotely(lease, &job_id, index, trial, &cancel, overall_deadline)
        } else if let Some(worker) = worker.as_ref() {
            execute_async_reference(
                AsyncReferenceLaunch {
                    sandboxes,
                    guardian: guardian.clone(),
                    protected_paths: protected_paths.clone(),
                    worker: Arc::clone(worker),
                    cancel: Arc::clone(&cancel),
                },
                &trial.spec,
                evidence.clone(),
                initial_sequence,
            )
        } else {
            // Unreachable in practice: `submit_arena_job` only ever admits a
            // provider-less (reference-role) trial after pinning a worker for
            // it. Fail the trial rather than panic if that invariant is ever
            // broken.
            Err("reference-role Arena trial has no pinned reference worker".to_owned())
        };
        let (reply, response) = mpsc::channel();
        if messages
            .send(ArenaWorkerMessage::Trial {
                job_id: job_id.clone(),
                index,
                output,
                reply,
            })
            .is_err()
        {
            outcome = Err("canonical writer is unavailable".to_owned());
            break;
        }
        match response.recv() {
            Ok(Ok(sequence)) => initial_sequence = sequence,
            Ok(Err(error)) => {
                outcome = Err(error);
                break;
            }
            Err(_) => {
                outcome = Err("canonical writer did not acknowledge the trial".to_owned());
                break;
            }
        }
    }
    let _ignored = messages.send(ArenaWorkerMessage::Trials {
        job_id,
        result: outcome,
    });
}

/// Executes one reference-role Arena trial by leasing it to a remote
/// worker (TD-12) instead of running a local sandbox. The frame is the
/// exact same `frame_reference_instruction` construction the direct-run
/// remote-worker path uses, over the trial's own already-bound
/// [`ReferenceInstruction`] and prompt, so a remote worker cannot tell this
/// apart from a direct reference run. Blocks this background thread (never
/// the daemon's single control-loop thread) until a worker submits a
/// result, the trial is cancelled, or `deadline` passes.
fn execute_arena_trial_remotely(
    lease: &RemoteArenaLeaseQueue,
    job_id: &str,
    index: usize,
    trial: &ArenaTrialSpec,
    cancel: &std::sync::atomic::AtomicBool,
    deadline: Instant,
) -> Result<ReferenceExecution, String> {
    let instruction = trial
        .spec
        .reference_instruction()
        .ok_or_else(|| "remote Arena trial has no reference instruction".to_owned())?;
    let frame = hephaestus_runtime::frame_reference_instruction(
        instruction,
        trial.spec.prompt().as_bytes(),
    )
    .map_err(|_| "remote Arena trial frame could not be built".to_owned())?;
    let trial_job_id = format!("arena:{job_id}:trial:{index}");
    let outcome = lease.submit_and_wait(
        &trial_job_id,
        &trial.genome.genome_id,
        frame,
        cancel,
        deadline,
    )?;
    Ok(ReferenceExecution {
        completion_reason: match outcome.completion {
            RemoteCompletion::Success => RunCompletionReason::Success,
            RemoteCompletion::ProviderFailure => RunCompletionReason::ProviderFailure,
        },
        latency_millis: outcome.latency_millis,
        stdout: outcome.output,
        stderr: Vec::new(),
        trace_artifact_ids: Vec::new(),
        actual_cost_microusd: 0,
    })
}

fn map_run_completion_reason(reason: CompletionReason) -> RunCompletionReason {
    reason.into()
}

fn execute_reference_runtime(
    recorder: EvidenceRecorder,
    spec: &RunSpec,
    sandbox: &Sandbox,
    token: &CapabilityToken,
    run_id: &str,
) -> (Result<ReferenceExecution, ExecuteError>, EvidenceRecorder) {
    let mut runtime =
        match RecordedRuntime::new_recoverable(DeterministicRuntime::default(), recorder) {
            Ok(runtime) => runtime,
            Err(recovery) => {
                let (_, _, recorder) = *recovery;
                return (Err(ExecuteError::Internal), recorder);
            }
        };
    let execution = (|| {
        runtime
            .start(spec, sandbox, token)
            .map_err(|_| ExecuteError::Internal)?;
        let snapshot = runtime
            .snapshot(run_id)
            .map_err(|_| ExecuteError::Internal)?;
        if snapshot.status == RunStatus::Running {
            return Err(ExecuteError::Internal);
        }
        let completion_reason = snapshot
            .completion_reason
            .ok_or(ExecuteError::Internal)
            .map(run_completion_reason)?;
        let latency_millis =
            u64::try_from(snapshot.elapsed.as_millis()).map_err(|_| ExecuteError::Internal)?;
        let stdout = fs::read(&snapshot.stdout_path).map_err(|_| ExecuteError::Internal)?;
        let stderr = fs::read(&snapshot.stderr_path).map_err(|_| ExecuteError::Internal)?;
        let history = runtime
            .evidence()
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ReferenceExecution {
            completion_reason,
            latency_millis,
            stdout,
            stderr,
            trace_artifact_ids: trace_artifacts_for_run(&history, run_id)?,
            actual_cost_microusd: 0,
        })
    })();
    let (_, recorder) = runtime.into_parts();
    (execution, recorder)
}

fn execute_candidate_runtime(
    runtime: SupervisedRuntime,
    recorder: EvidenceRecorder,
    spec: &RunSpec,
    sandbox: &Sandbox,
    token: &CapabilityToken,
    run_id: &str,
) -> (Result<ReferenceExecution, ExecuteError>, EvidenceRecorder) {
    let mut runtime = match RecordedRuntime::new_recoverable(runtime, recorder) {
        Ok(runtime) => runtime,
        Err(recovery) => {
            let (_, _, recorder) = *recovery;
            return (Err(ExecuteError::Internal), recorder);
        }
    };
    let execution = (|| {
        runtime
            .start(spec, sandbox, token)
            .map_err(|_| ExecuteError::Internal)?;
        let snapshot = loop {
            let snapshot = runtime
                .snapshot(run_id)
                .map_err(|_| ExecuteError::Internal)?;
            if snapshot.status != RunStatus::Running {
                break snapshot;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let completion_reason = snapshot
            .completion_reason
            .ok_or(ExecuteError::Internal)
            .map(run_completion_reason)?;
        let latency_millis =
            u64::try_from(snapshot.elapsed.as_millis()).map_err(|_| ExecuteError::Internal)?;
        let stdout = fs::read(&snapshot.stdout_path).map_err(|_| ExecuteError::Internal)?;
        let stderr = fs::read(&snapshot.stderr_path).map_err(|_| ExecuteError::Internal)?;
        let history = runtime
            .evidence()
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ReferenceExecution {
            completion_reason,
            latency_millis,
            stdout,
            stderr,
            trace_artifact_ids: trace_artifacts_for_run(&history, run_id)?,
            actual_cost_microusd: 0,
        })
    })();
    let (_, recorder) = runtime.into_parts();
    (execution, recorder)
}

/// Runs a Codex or Claude Code adapter to completion and maps its NDJSON
/// stream onto a signed run result: the extracted final answer becomes
/// stdout, the reported cost (when any) becomes `actual_cost_microusd`, and
/// both stdout and stderr are redacted before they ever reach the artifact
/// store. Structured observations (tool calls, denials, cost) are recorded
/// as evidence traces by `RecordedRuntime` via `drain_observations`, through
/// the same redaction and evidence pipeline the reference worker uses.
fn execute_provider_runtime(
    runtime: SupervisedRuntime,
    recorder: EvidenceRecorder,
    spec: &RunSpec,
    sandbox: &Sandbox,
    token: &CapabilityToken,
    run_id: &str,
    redaction: &RedactionPolicy,
) -> (Result<ReferenceExecution, ExecuteError>, EvidenceRecorder) {
    let provider = runtime.provider();
    let mut runtime = match RecordedRuntime::new_recoverable(runtime, recorder) {
        Ok(runtime) => runtime,
        Err(recovery) => {
            let (_, _, recorder) = *recovery;
            return (Err(ExecuteError::Internal), recorder);
        }
    };
    let execution = (|| {
        runtime
            .start(spec, sandbox, token)
            .map_err(|_| ExecuteError::Internal)?;
        let snapshot = loop {
            let snapshot = runtime
                .snapshot(run_id)
                .map_err(|_| ExecuteError::Internal)?;
            if snapshot.status != RunStatus::Running {
                break snapshot;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let completion_reason = snapshot
            .completion_reason
            .ok_or(ExecuteError::Internal)
            .map(run_completion_reason)?;
        let latency_millis =
            u64::try_from(snapshot.elapsed.as_millis()).map_err(|_| ExecuteError::Internal)?;
        let raw_stdout = fs::read(&snapshot.stdout_path).map_err(|_| ExecuteError::Internal)?;
        let raw_stderr = fs::read(&snapshot.stderr_path).map_err(|_| ExecuteError::Internal)?;
        let final_answer = extract_final_answer(provider, &raw_stdout);
        let actual_cost_microusd = extract_actual_cost_microusd(provider, &raw_stdout);
        let stdout = redact_bytes(redaction, &final_answer);
        let stderr = redact_bytes(redaction, &raw_stderr);
        let history = runtime
            .evidence()
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let trace_ids = trace_artifacts_for_run(&history, run_id)?;
        Ok(ReferenceExecution {
            completion_reason,
            latency_millis,
            stdout,
            stderr,
            trace_artifact_ids: trace_ids,
            actual_cost_microusd,
        })
    })();
    let (_, recorder) = runtime.into_parts();
    (execution, recorder)
}

/// Redacts textual provider output through the existing secret-redaction
/// path. Bytes that are not valid UTF-8 are passed through unchanged: the
/// redaction rules match literal tokens and known-secret strings, which are
/// only ever meaningful in text.
fn redact_bytes(policy: &RedactionPolicy, bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) => policy.redact_text(text).into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}

#[cfg(feature = "test-support")]
fn candidate_isolation(_protected_paths: Vec<PathBuf>) -> IsolationPolicy {
    IsolationPolicy::unconfined_for_testing()
}

#[cfg(not(feature = "test-support"))]
fn candidate_isolation(protected_paths: Vec<PathBuf>) -> IsolationPolicy {
    IsolationPolicy::detect(protected_paths)
}

fn validate_job_id(job_id: &str) -> Result<(), ExecuteError> {
    if job_id.is_empty()
        || job_id.len() > 128
        || !job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(ExecuteError::Invalid("job_id is invalid"));
    }
    Ok(())
}

fn job_run_id(job_id: &str) -> String {
    let digest = blake3::hash(job_id.as_bytes()).to_hex().to_string();
    format!("async-{}", &digest[..32])
}

fn remote_job_run_id(job_id: &str) -> String {
    let digest = blake3::hash(job_id.as_bytes()).to_hex().to_string();
    format!("remote-{}", &digest[..32])
}

fn hex_encode_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn hex_decode_bytes(value: &str) -> Result<Vec<u8>, ExecuteError> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ExecuteError::Invalid("value is not valid hex"));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| ExecuteError::Invalid("value is not valid hex"))
        })
        .collect()
}

fn paired_run_prefix(evaluation_id: &str) -> String {
    let digest = blake3::hash(evaluation_id.as_bytes()).to_hex().to_string();
    format!("paired-{}", &digest[..24])
}

fn paired_run_id(evaluation_id: &str, role: &str, index: usize) -> String {
    format!("{}-{role}-{index}", paired_run_prefix(evaluation_id))
}

fn resolve_source_revision(repository: &Path) -> Result<String, ExecuteError> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", "--verify", "HEAD^{commit}"])
        .output()
        .map_err(|_| ExecuteError::Internal)?;
    if !output.status.success() {
        return Err(ExecuteError::Internal);
    }
    let revision = std::str::from_utf8(&output.stdout)
        .map_err(|_| ExecuteError::Internal)?
        .trim();
    if !matches!(revision.len(), 40 | 64) || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ExecuteError::Internal);
    }
    Ok(revision.to_ascii_lowercase())
}

fn default_evaluator_executable() -> Result<PathBuf, ControlError> {
    let current = env::current_exe()?;
    let directory = current
        .parent()
        .ok_or(ControlError::Protocol("daemon executable has no directory"))?;
    let sibling = directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    if sibling.exists() {
        return Ok(sibling);
    }
    // Cargo places unit-test executables under `target/debug/deps`, while the
    // installed daemon and evaluator are siblings under `bin` or `target/debug`.
    if let Some(parent) = directory.parent() {
        let cargo_sibling = parent.join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
        if cargo_sibling.exists() {
            return Ok(cargo_sibling);
        }
    }
    Ok(sibling)
}

fn default_reference_worker_executable() -> Result<PathBuf, ControlError> {
    let current = env::current_exe()?;
    let directory = current
        .parent()
        .ok_or(ControlError::Protocol("daemon executable has no directory"))?;
    let sibling = directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    if sibling.exists() {
        return Ok(sibling);
    }
    // Cargo places unit-test executables under `target/debug/deps`, while the
    // installed daemon and worker are siblings under `bin` or `target/debug`.
    if let Some(parent) = directory.parent() {
        let cargo_sibling = parent.join(format!(
            "hephaestus-reference-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
        if cargo_sibling.exists() {
            return Ok(cargo_sibling);
        }
    }
    Ok(sibling)
}

/// Resolves an operator-configured provider CLI path from `variable`, falling
/// back to `default_name` for `PATH`-relative lookup by the isolated child's
/// own restored `PATH` (never this process's full environment).
fn provider_executable_from_environment(variable: &str, default_name: &str) -> PathBuf {
    env::var_os(variable).map_or_else(|| PathBuf::from(default_name), PathBuf::from)
}

/// Reads the operator-named allowlist of environment variables a provider
/// child may see, from `HEPHAESTUS_PROVIDER_ENV_ALLOWLIST` (comma-separated
/// names). Empty when unset, so nothing beyond `PATH`/`HOME`/`TMPDIR` reaches
/// a provider child unless an operator explicitly names it.
fn provider_env_allowlist_from_environment() -> Vec<String> {
    env::var("HEPHAESTUS_PROVIDER_ENV_ALLOWLIST")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Copies only the named, currently-set variables from this process's own
/// environment. A name that is not set is silently skipped rather than
/// passed through as empty.
fn resolve_provider_extra_env(allowlist: &[String]) -> Vec<(String, String)> {
    allowlist
        .iter()
        .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
        .collect()
}

fn default_process_guardian_executable() -> Result<PathBuf, ControlError> {
    let current = env::current_exe()?;
    let directory = current
        .parent()
        .ok_or(ControlError::Protocol("daemon executable has no directory"))?;
    let sibling = directory.join(format!(
        "hephaestus-process-guardian{}",
        std::env::consts::EXE_SUFFIX
    ));
    if sibling.exists() {
        return Ok(sibling);
    }
    if let Some(parent) = directory.parent() {
        let cargo_sibling = parent.join(format!(
            "hephaestus-process-guardian{}",
            std::env::consts::EXE_SUFFIX
        ));
        if cargo_sibling.exists() {
            return Ok(cargo_sibling);
        }
    }
    Ok(sibling)
}

fn executable_digest(path: &Path) -> Result<String, ControlError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(ControlError::Protocol(
            "reference worker must be an executable regular file",
        ));
    }
    let bytes = fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn persist_reference_output(
    artifacts: &dyn ArtifactBackend,
    run_id: &str,
    genome: &GenomeRecord,
    source_revision: &str,
    output: ReferenceExecution,
) -> Result<ResponseData, ExecuteError> {
    let stdout_artifact_id = artifacts
        .put(&output.stdout)
        .map_err(|_| ExecuteError::Internal)?
        .as_str()
        .to_owned();
    let stderr_artifact_id = artifacts
        .put(&output.stderr)
        .map_err(|_| ExecuteError::Internal)?
        .as_str()
        .to_owned();
    Ok(ResponseData::Run {
        run_id: run_id.to_owned(),
        genome_id: genome.genome_id.clone(),
        world_id: genome.world_id.clone(),
        source_revision: source_revision.to_owned(),
        completion_reason: output.completion_reason,
        latency_millis: output.latency_millis,
        actual_cost_microusd: output.actual_cost_microusd,
        stdout_artifact_id,
        stderr_artifact_id,
        trace_artifact_ids: output.trace_artifact_ids,
    })
}

struct ControlState {
    freeze: FreezeState,
    active_runs: BTreeSet<String>,
    jobs: BTreeMap<String, JobRecord>,
    arena_jobs: BTreeMap<String, ArenaJobRecord>,
    job_progress: BTreeMap<String, JobProgress>,
    evaluation_events: BTreeMap<String, u64>,
    run_results: BTreeMap<String, RunResultReceipt>,
    completed_runs: BTreeSet<String>,
    registered: RegisteredObjects,
    event_count: u64,
    worker_credentials: BTreeMap<String, WorkerCredentialRecord>,
    remote_jobs: BTreeMap<String, RemoteJobRecord>,
}

/// Durable, replay-verified projection of one minted worker credential. The
/// raw secret is never stored; `credential_id` is its content-derived,
/// safe-to-log identity (`blake3(secret)[..32]`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkerCredentialRecord {
    schema_version: u16,
    credential_id: String,
    worker_id: String,
    scope: WorkerScope,
    expires_at_millis: i64,
    revoked: bool,
}

/// Durable, replay-verified admission of one remote-worker job. Terminal
/// state is derived by joining `run_id` against `ControlState::run_results`,
/// the same signed-result table local runs populate, so a remote result is
/// indistinguishable from a local one once recorded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteJobRecord {
    schema_version: u16,
    job_id: String,
    genome_id: String,
    run_id: String,
}

impl ControlState {
    fn from_events(
        events: &[StoredEvent],
        registered: RegisteredObjects,
        operator_token: &OperatorToken,
        run_result_verifier: &RunResultVerifier,
    ) -> Result<Self, ControlError> {
        let mut state = Self {
            freeze: FreezeState::frozen(operator_token),
            active_runs: BTreeSet::new(),
            jobs: BTreeMap::new(),
            arena_jobs: BTreeMap::new(),
            job_progress: BTreeMap::new(),
            evaluation_events: BTreeMap::new(),
            run_results: BTreeMap::new(),
            completed_runs: BTreeSet::new(),
            registered,
            event_count: 0,
            worker_credentials: BTreeMap::new(),
            remote_jobs: BTreeMap::new(),
        };
        for event in events {
            state.apply(event, operator_token, run_result_verifier)?;
        }
        Ok(state)
    }

    #[allow(clippy::too_many_lines)]
    fn apply(
        &mut self,
        event: &StoredEvent,
        operator_token: &OperatorToken,
        run_result_verifier: &RunResultVerifier,
    ) -> Result<(), ControlError> {
        if event.event_type.starts_with("control.") {
            if event.aggregate_id != CONTROL_AGGREGATE || event.actor != OPERATOR_ACTOR {
                return Err(ControlError::Projection(
                    "control event crossed the operator boundary".to_owned(),
                ));
            }
            let recorded: RecordedCommand = serde_json::from_slice(&event.payload)?;
            if event.event_type != "control.request_rejected" {
                require_projection_text(&recorded.request_id, "request_id")?;
                if event_type(&recorded.command) != event.event_type {
                    return Err(ControlError::Projection(
                        "control event type does not match its command".to_owned(),
                    ));
                }
            }
        }
        match event.event_type.as_str() {
            "control.freeze" => self.freeze = FreezeState::frozen(operator_token),
            "control.unfreeze" => self
                .freeze
                .unfreeze(operator_token)
                .map_err(|_| ControlError::Projection("operator proof rejected".to_owned()))?,
            "run.started" => {
                let run: RunRecord = serde_json::from_slice(&event.payload)?;
                require_projection_text(&run.run_id, "run_id")?;
                self.active_runs.insert(run.run_id);
            }
            "run.completed" => {
                let run: RunRecord = serde_json::from_slice(&event.payload)?;
                require_projection_text(&run.run_id, "run_id")?;
                self.active_runs.remove(&run.run_id);
            }
            "trace.recorded" => {
                let receipt: TraceReceipt = serde_json::from_slice(&event.payload)?;
                validate_trace_receipt(event, &receipt)?;
                let owner = self
                    .jobs
                    .iter()
                    .find(|(_, job)| job.run_id == receipt.provenance.run_id())
                    .map(|(job_id, _)| job_id.clone());
                if let Some(job_id) = owner {
                    let progress = self.job_progress.entry(job_id).or_default();
                    progress.trace_events =
                        progress.trace_events.checked_add(1).ok_or_else(|| {
                            ControlError::Projection("job trace count overflow".to_owned())
                        })?;
                    progress.last_event_sequence = Some(event.sequence);
                    progress.last_phase = Some(trace_phase(receipt.kind).to_owned());
                }
                match receipt.kind {
                    TraceKind::LifecycleStarted | TraceKind::LifecycleResumed => {
                        self.active_runs
                            .insert(receipt.provenance.run_id().to_owned());
                    }
                    TraceKind::LifecycleCompleted => {
                        self.active_runs.remove(receipt.provenance.run_id());
                        self.completed_runs
                            .insert(receipt.provenance.run_id().to_owned());
                    }
                    _ => {}
                }
            }
            "run.result_recorded" => {
                let receipt = RunResultReceipt::parse_from_event(event, run_result_verifier)
                    .map_err(|_| {
                        ControlError::Projection("canonical run result is invalid".to_owned())
                    })?;
                self.advance_arena_trial(&receipt)?;
                self.run_results.insert(receipt.run_id.clone(), receipt);
            }
            "job.admitted" | "job.running" | "job.cancellation_requested" | "job.terminal" => {
                self.apply_job_record(event)?;
            }
            "arena.job.admitted"
            | "arena.job.running"
            | "arena.job.cancellation_requested"
            | "arena.job.scoring"
            | "arena.job.committing"
            | "arena.job.terminal" => self.apply_arena_job_record(event)?,
            "evaluation.recorded" => {
                let value: serde_json::Value = serde_json::from_slice(&event.payload)?;
                let evaluation_id = value
                    .get("evaluation_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ControlError::Projection("Arena receipt identity is invalid".to_owned())
                    })?;
                if self
                    .evaluation_events
                    .insert(evaluation_id.to_owned(), event.sequence)
                    .is_some()
                {
                    return Err(ControlError::Projection(
                        "duplicate Arena receipt identity".to_owned(),
                    ));
                }
            }
            "worker.credential_minted" => {
                let record: WorkerCredentialRecord = serde_json::from_slice(&event.payload)?;
                require_projection_text(&record.credential_id, "credential_id")?;
                require_projection_text(&record.worker_id, "worker_id")?;
                if record.revoked || self.worker_credentials.contains_key(&record.credential_id) {
                    return Err(ControlError::Projection(
                        "worker credential mint is invalid".to_owned(),
                    ));
                }
                self.worker_credentials
                    .insert(record.credential_id.clone(), record);
            }
            "worker.credential_revoked" => {
                let value: serde_json::Value = serde_json::from_slice(&event.payload)?;
                let credential_id = value
                    .get("credential_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ControlError::Projection(
                            "worker credential revocation is invalid".to_owned(),
                        )
                    })?;
                let record = self
                    .worker_credentials
                    .get_mut(credential_id)
                    .ok_or_else(|| {
                        ControlError::Projection(
                            "worker credential revocation names an unknown credential".to_owned(),
                        )
                    })?;
                record.revoked = true;
            }
            "remote_worker.job_admitted" => {
                let record: RemoteJobRecord = serde_json::from_slice(&event.payload)?;
                require_projection_text(&record.job_id, "job_id")?;
                require_projection_text(&record.run_id, "run_id")?;
                if self.remote_jobs.contains_key(&record.job_id) {
                    return Err(ControlError::Projection(
                        "remote job admission is a duplicate".to_owned(),
                    ));
                }
                self.remote_jobs.insert(record.job_id.clone(), record);
            }
            _ => {}
        }
        self.event_count = event.sequence;
        Ok(())
    }

    fn apply_job_record(&mut self, event: &StoredEvent) -> Result<(), ControlError> {
        let record: JobRecord = serde_json::from_slice(&event.payload)?;
        self.validate_job_record(event, &record)?;
        if !self.job_transition_is_valid(event, &record) {
            return Err(ControlError::Projection(
                "job lifecycle transition is invalid".to_owned(),
            ));
        }
        self.commit_job_record(record);
        Ok(())
    }

    fn advance_arena_trial(&mut self, receipt: &RunResultReceipt) -> Result<(), ControlError> {
        let Some((job_id, trial_index)) = self.arena_jobs.iter().find_map(|(job_id, job)| {
            job.ordered_trial_run_ids
                .iter()
                .position(|expected| expected == &receipt.run_id)
                .map(|index| (job_id.clone(), index))
        }) else {
            return Ok(());
        };
        let job = self.arena_jobs.get(&job_id).ok_or_else(|| {
            ControlError::Projection("Arena job disappeared during trial replay".to_owned())
        })?;
        if trial_index != usize::try_from(job.completed_trials).unwrap_or(usize::MAX)
            || !matches!(
                job.state,
                JobState::Running | JobState::CancellationRequested
            )
        {
            return Err(ControlError::Projection(
                "Arena trial result is out of admitted order".to_owned(),
            ));
        }
        let is_parent_trial =
            trial_index < usize::try_from(job.parent_trial_count).unwrap_or(usize::MAX);
        let expected_genome = if is_parent_trial {
            &job.parent_genome_id
        } else {
            &job.candidate_genome_id
        };
        let expected_environment_id = if is_parent_trial {
            job.environment_id.as_str()
        } else {
            job.effective_candidate_environment_id()
        };
        if receipt.genome_id != *expected_genome
            || receipt.world_id != job.world_id
            || receipt.source_revision != job.source_revision
            || receipt.seed != job.seed
            || receipt.environment_id != expected_environment_id
            || receipt.budget != job.trial_budget
        {
            return Err(ControlError::Projection(
                "Arena run receipt differs from its admitted source".to_owned(),
            ));
        }
        let job = self.arena_jobs.get_mut(&job_id).ok_or_else(|| {
            ControlError::Projection("Arena job disappeared during trial replay".to_owned())
        })?;
        job.completed_trials = job
            .completed_trials
            .checked_add(1)
            .ok_or_else(|| ControlError::Projection("Arena trial count overflow".to_owned()))?;
        if job.completed_trials == job.parent_trial_count {
            job.phase = ArenaJobPhase::CandidateTrials;
        }
        Ok(())
    }

    fn apply_arena_job_record(&mut self, event: &StoredEvent) -> Result<(), ControlError> {
        let record: ArenaJobRecord = serde_json::from_slice(&event.payload)?;
        validate_arena_job_record(event, &record)?;
        let world = self.registered.world(&record.world_id).ok_or_else(|| {
            ControlError::Projection("Arena job World is not registered".to_owned())
        })?;
        let parent = self
            .registered
            .genome(&record.parent_genome_id)
            .ok_or_else(|| {
                ControlError::Projection("Arena parent Genome is not registered".to_owned())
            })?;
        let candidate = self
            .registered
            .genome(&record.candidate_genome_id)
            .ok_or_else(|| {
                ControlError::Projection("Arena candidate Genome is not registered".to_owned())
            })?;
        if parent.record().world_id != record.world_id
            || candidate.record().world_id != record.world_id
            || world.compiled().id() != record.world_id
            || world
                .compiled()
                .evaluator_artifact("arena.visible_manifest")
                != Some(record.visible_manifest_id.as_str())
            || world.compiled().evaluator_artifact("arena.sealed_manifest")
                != Some(record.sealed_manifest_id.as_str())
            || world.compiled().evaluator_artifact("arena.evaluator")
                != Some(record.evaluator_id.as_str())
        {
            return Err(ControlError::Projection(
                "Arena job differs from registered World and Genome bindings".to_owned(),
            ));
        }
        // The environment shape bound for each role must match that role's
        // own Genome provider configuration, and a mixed pair (parent and
        // candidate under distinct environments) is only ever valid when the
        // World's Law opted in.
        let parent_selects_provider =
            matches!(parent.compiled().model_provider(), "codex" | "claude");
        let candidate_selects_provider =
            matches!(candidate.compiled().model_provider(), "codex" | "claude");
        let parent_environment_is_provider = record.environment_id.starts_with("provider-v1.");
        let candidate_environment_is_provider = record
            .effective_candidate_environment_id()
            .starts_with("provider-v1.");
        if parent_environment_is_provider != parent_selects_provider
            || candidate_environment_is_provider != candidate_selects_provider
        {
            return Err(ControlError::Projection(
                "Arena job environment does not match a Genome's provider configuration".to_owned(),
            ));
        }
        if record.candidate_environment_id.is_some()
            && !world
                .compiled()
                .evaluation_policy()
                .allow_mixed_environments()
        {
            return Err(ControlError::Projection(
                "Arena job pairs distinct environments but the World does not permit it".to_owned(),
            ));
        }
        if !self.arena_job_transition_is_valid(event, &record) {
            return Err(ControlError::Projection(
                "Arena job lifecycle transition is invalid".to_owned(),
            ));
        }
        self.arena_jobs.insert(record.evaluation_id.clone(), record);
        Ok(())
    }

    fn arena_job_transition_is_valid(&self, event: &StoredEvent, record: &ArenaJobRecord) -> bool {
        let previous = self.arena_jobs.get(&record.evaluation_id);
        let transition = match event.event_type.as_str() {
            "arena.job.admitted" => {
                previous.is_none()
                    && record.state == JobState::Admitted
                    && record.phase == ArenaJobPhase::Preparing
                    && record.terminal.is_none()
            }
            "arena.job.running" => {
                previous.is_some_and(|old| old.state == JobState::Admitted)
                    && record.state == JobState::Running
                    && record.phase == ArenaJobPhase::ParentTrials
                    && record.terminal.is_none()
            }
            "arena.job.cancellation_requested" => {
                previous
                    .is_some_and(|old| matches!(old.state, JobState::Running | JobState::Admitted))
                    && record.state == JobState::CancellationRequested
                    && record.terminal.is_none()
            }
            "arena.job.scoring" => {
                previous.is_some_and(|old| {
                    old.state == JobState::Running && old.completed_trials == old.total_trials
                }) && record.state == JobState::Running
                    && record.phase == ArenaJobPhase::Scoring
                    && record.terminal.is_none()
            }
            "arena.job.committing" => {
                previous.is_some_and(|old| {
                    old.state == JobState::Running && old.phase == ArenaJobPhase::Scoring
                }) && record.state == JobState::Running
                    && record.phase == ArenaJobPhase::Committing
                    && record.terminal.is_none()
            }
            "arena.job.terminal" => {
                let prior_active = previous.is_some_and(|old| {
                    matches!(
                        old.state,
                        JobState::Running | JobState::Admitted | JobState::CancellationRequested
                    )
                });
                let matching_evaluation =
                    self.evaluation_events.contains_key(&record.evaluation_id);
                let valid_terminal = matches!(
                    (record.state, record.terminal),
                    (JobState::Succeeded, Some(JobTerminal::Succeeded))
                        | (JobState::Failed, Some(JobTerminal::Failed))
                        | (
                            JobState::Interrupted,
                            Some(JobTerminal::Cancelled | JobTerminal::Interrupted)
                        )
                );
                prior_active
                    && valid_terminal
                    && (record.state != JobState::Succeeded
                        || (previous.is_some_and(|old| {
                            old.state == JobState::Running && old.phase == ArenaJobPhase::Committing
                        }) && matching_evaluation
                            && record.completed_trials == record.total_trials
                            && record.evaluation.as_ref().is_some_and(|evaluation| {
                                evaluation.evaluation_id == record.evaluation_id
                                    && evaluation.world_id == record.world_id
                                    && evaluation.parent_genome_id == record.parent_genome_id
                                    && evaluation.candidate_genome_id == record.candidate_genome_id
                            })))
                    && (record.state == JobState::Succeeded || record.evaluation.is_none())
            }
            _ => false,
        };
        transition
            && previous.map_or(record.completed_trials == 0, |old| {
                arena_job_immutable_fields_match(old, record)
                    && old.completed_trials == record.completed_trials
            })
    }

    fn validate_job_record(
        &self,
        event: &StoredEvent,
        record: &JobRecord,
    ) -> Result<(), ControlError> {
        require_projection_text(&record.job_id, "job_id")?;
        require_projection_text(&record.run_id, "run_id")?;
        require_projection_text(&record.task_id, "task_id")?;
        require_projection_text(&record.environment_id, "environment_id")?;
        require_projection_text(&record.source_revision, "source_revision")?;
        validate_content_id(&record.genome_id, "genome")?;
        validate_content_id(&record.world_id, "world")?;
        let is_provider_environment =
            if let Some(digest) = record.environment_id.strip_prefix("reference-v1.") {
                ArtifactId::parse(digest.to_owned())?;
                false
            } else if let Some(digest) = record.environment_id.strip_prefix("provider-v1.") {
                ArtifactId::parse(digest.to_owned())?;
                true
            } else {
                return Err(ControlError::Projection(
                    "job environment is invalid".to_owned(),
                ));
            };
        let genome = self
            .registered
            .genome(&record.genome_id)
            .ok_or_else(|| ControlError::Projection("job Genome is unregistered".to_owned()))?;
        // The environment shape must match what the Genome's own `model.provider`
        // selects: a reference-shaped identity for a deterministic Genome, a
        // provider-shaped one for a `codex`/`claude` Genome. This is the same
        // binding `selected_run_provider` enforces live at admission.
        let genome_selects_provider =
            matches!(genome.compiled().model_provider(), "codex" | "claude");
        if is_provider_environment != genome_selects_provider {
            return Err(ControlError::Projection(
                "job environment does not match the Genome's provider configuration".to_owned(),
            ));
        }
        let expected_cost_microusd = if is_provider_environment {
            self.registered
                .world(&genome.record().world_id)
                .ok_or_else(|| ControlError::Projection("job World is unregistered".to_owned()))?
                .compiled()
                .evaluation_policy()
                .maximum_cost_microusd()
        } else {
            0
        };
        let fixed_input =
            "Inventory the isolated repository without modifying it or using the network.";
        if record.run_id != job_run_id(&record.job_id)
            || record.world_id != genome.record().world_id
            || record.task_id != "repository-inventory-v1"
            || record.input_commitment != blake3::hash(fixed_input.as_bytes()).to_hex().to_string()
            || record.seed != 0
            || record.budget
                != (RunBudgetReceipt {
                    wall_millis: 10_000,
                    maximum_output_bytes: 1_048_576,
                    maximum_cost_microusd: expected_cost_microusd,
                })
        {
            return Err(ControlError::Projection(
                "job spec binding is invalid".to_owned(),
            ));
        }
        if event.actor != RUNTIME_ACTOR
            || event.aggregate_id != format!("job:{}", record.job_id)
            || event.event_id
                != format!(
                    "job:{}:{}",
                    record.job_id,
                    event.event_type.strip_prefix("job.").unwrap_or("invalid")
                )
        {
            return Err(ControlError::Projection(
                "job event crossed its runtime boundary".to_owned(),
            ));
        }
        // A successful `job.terminal` record's binding to its signed result and
        // completed lifecycle trace is `job_transition_is_valid`'s sole
        // responsibility (its `Some(JobTerminal::Succeeded)` arm below re-derives
        // the identical `receipt_matches && completed_runs.contains(..) &&
        // completion_reason == Success` conjunction); duplicating it here made
        // that arm's own invariant unfalsifiable, since this check always ran
        // first and rejected every input the other check could otherwise catch.
        Ok(())
    }

    fn job_transition_is_valid(&self, event: &StoredEvent, record: &JobRecord) -> bool {
        let previous = self.jobs.get(&record.job_id);
        let valid = match event.event_type.as_str() {
            "job.admitted" => {
                previous.is_none()
                    && record.state == JobState::Admitted
                    && record.terminal.is_none()
            }
            "job.running" => {
                previous.is_some_and(|old| old.state == JobState::Admitted)
                    && record.state == JobState::Running
                    && record.terminal.is_none()
            }
            "job.cancellation_requested" => {
                previous
                    .is_some_and(|old| matches!(old.state, JobState::Running | JobState::Admitted))
                    && record.state == JobState::CancellationRequested
                    && record.terminal.is_none()
            }
            "job.terminal" => {
                let previous_active = previous.is_some_and(|old| {
                    matches!(
                        old.state,
                        JobState::Running | JobState::Admitted | JobState::CancellationRequested
                    )
                });
                let state_matches_terminal = matches!(
                    (record.state, record.terminal),
                    (JobState::Succeeded, Some(JobTerminal::Succeeded))
                        | (JobState::Failed, Some(JobTerminal::Failed))
                        | (
                            JobState::Interrupted,
                            Some(JobTerminal::Cancelled | JobTerminal::Interrupted),
                        )
                );
                let signed_result = self.run_results.get(&record.run_id);
                let receipt_matches =
                    signed_result.is_some_and(|receipt| receipt_matches_job(receipt, record));
                let terminal_proven = match record.terminal {
                    Some(JobTerminal::Succeeded) => {
                        receipt_matches
                            && self.completed_runs.contains(&record.run_id)
                            && signed_result.is_some_and(|receipt| {
                                receipt.completion_reason == RunCompletionReason::Success
                            })
                    }
                    Some(JobTerminal::Failed) => signed_result.is_none_or(|receipt| {
                        receipt_matches && receipt.completion_reason != RunCompletionReason::Success
                    }),
                    Some(JobTerminal::Cancelled) => {
                        previous.is_some_and(|old| old.state == JobState::CancellationRequested)
                            && signed_result.is_none_or(|receipt| {
                                receipt_matches
                                    && receipt.completion_reason != RunCompletionReason::Success
                            })
                    }
                    Some(JobTerminal::Interrupted) => signed_result.is_none_or(|receipt| {
                        receipt_matches && receipt.completion_reason != RunCompletionReason::Success
                    }),
                    None => false,
                };
                previous_active && state_matches_terminal && terminal_proven
            }
            _ => false,
        };
        valid
            && !previous.is_some_and(|old| {
                old.genome_id != record.genome_id
                    || old.run_id != record.run_id
                    || old.source_revision != record.source_revision
                    || old.world_id != record.world_id
                    || old.task_id != record.task_id
                    || old.input_commitment != record.input_commitment
                    || old.seed != record.seed
                    || old.environment_id != record.environment_id
                    || old.budget != record.budget
            })
    }

    fn commit_job_record(&mut self, record: JobRecord) {
        if record.state == JobState::Running {
            self.active_runs.insert(record.run_id.clone());
        }
        if record.state == JobState::Admitted {
            self.job_progress
                .insert(record.job_id.clone(), JobProgress::default());
        }
        if matches!(
            record.state,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        ) {
            self.active_runs.remove(&record.run_id);
        }
        self.jobs.insert(record.job_id.clone(), record);
    }

    fn status(&self) -> ResponseData {
        ResponseData::Status {
            frozen: self.freeze.is_frozen(),
            active_runs: self.active_runs.len(),
            event_count: self.event_count,
            genome_count: self.registered.genomes().count(),
        }
    }

    fn snapshot(&self) -> ProjectionSnapshot {
        ProjectionSnapshot {
            frozen: self.freeze.is_frozen(),
            active_runs: self.active_runs.iter().cloned().collect(),
            genomes: self.registered.genome_records(),
            worlds: self.registered.world_records(),
            jobs: self.jobs.clone(),
            arena_jobs: self.arena_jobs.clone(),
            job_progress: self.job_progress.clone(),
            evaluation_events: self.evaluation_events.clone(),
            run_results: self.run_results.clone(),
            completed_runs: self.completed_runs.iter().cloned().collect(),
            event_count: self.event_count,
        }
    }

    fn verify_artifacts(
        history: &[StoredEvent],
        artifacts: &dyn ArtifactBackend,
        run_result_verifier: &RunResultVerifier,
    ) -> Result<(), ControlError> {
        Self::verify_artifacts_with(
            history,
            artifacts,
            run_result_verifier,
            &mut EvidenceCache::default(),
        )
    }

    fn verify_artifacts_with(
        history: &[StoredEvent],
        artifacts: &dyn ArtifactBackend,
        run_result_verifier: &RunResultVerifier,
        cache: &mut EvidenceCache,
    ) -> Result<(), ControlError> {
        for event in history {
            if cache.contains("artifacts", event) {
                continue;
            }
            if event.event_type == "trace.recorded" {
                let receipt: TraceReceipt = serde_json::from_slice(&event.payload)?;
                artifacts.get(&ArtifactId::parse(receipt.artifact_id)?)?;
            } else if event.event_type == "run.result_recorded" {
                let receipt = RunResultReceipt::parse_from_event(event, run_result_verifier)
                    .map_err(|_| {
                        ControlError::Projection("canonical run result is invalid".to_owned())
                    })?;
                artifacts.get(&ArtifactId::parse(receipt.stdout_artifact_id)?)?;
                artifacts.get(&ArtifactId::parse(receipt.stderr_artifact_id)?)?;
                for artifact in receipt.trace_artifact_ids {
                    artifacts.get(&ArtifactId::parse(artifact)?)?;
                }
            }
            cache.insert("artifacts", event);
        }
        Ok(())
    }
}

fn recover_unfinished_jobs(
    ledger: &mut dyn EventLedger,
    state: &mut ControlState,
    operator_token: &OperatorToken,
    run_result_verifier: &RunResultVerifier,
) -> Result<(), ControlError> {
    let unfinished_jobs: Vec<_> = state
        .jobs
        .values()
        .filter(|job| {
            matches!(
                job.state,
                JobState::Admitted | JobState::Running | JobState::CancellationRequested
            )
        })
        .cloned()
        .collect();
    for mut job in unfinished_jobs {
        let cancelled = job.state == JobState::CancellationRequested;
        let receipt = state.run_results.get(&job.run_id);
        if receipt.is_some_and(|receipt| !receipt_matches_job(receipt, &job)) {
            return Err(ControlError::Projection(
                "signed job result differs from its admitted spec".to_owned(),
            ));
        }
        if receipt.is_some_and(|receipt| {
            receipt.completion_reason == RunCompletionReason::Success
                && !state.completed_runs.contains(&job.run_id)
        }) {
            return Err(ControlError::Projection(
                "successful job result lacks a completed lifecycle trace".to_owned(),
            ));
        }
        (job.state, job.terminal) = if cancelled {
            (JobState::Interrupted, Some(JobTerminal::Cancelled))
        } else {
            match receipt.map(|receipt| receipt.completion_reason) {
                Some(RunCompletionReason::Success) => {
                    (JobState::Succeeded, Some(JobTerminal::Succeeded))
                }
                Some(RunCompletionReason::OperatorInterrupt) => {
                    (JobState::Interrupted, Some(JobTerminal::Interrupted))
                }
                Some(_) => (JobState::Failed, Some(JobTerminal::Failed)),
                None => (JobState::Interrupted, Some(JobTerminal::Interrupted)),
            }
        };
        let payload = serde_json::to_vec(&job)?;
        let event = ledger.append(EventInput::new(
            format!("job:{}:terminal", job.job_id),
            format!("job:{}", job.job_id),
            "job.terminal",
            RUNTIME_ACTOR,
            timestamp_millis()?,
            payload,
        ))?;
        state.apply(&event, operator_token, run_result_verifier)?;
    }
    Ok(())
}

fn recover_unfinished_arena_jobs(
    ledger: &mut dyn EventLedger,
    artifacts: &dyn ArtifactBackend,
    state: &mut ControlState,
    operator_token: &OperatorToken,
    run_result_verifier: &RunResultVerifier,
) -> Result<(), ControlError> {
    let unfinished: Vec<_> = state
        .arena_jobs
        .values()
        .filter(|job| {
            matches!(
                job.state,
                JobState::Admitted | JobState::Running | JobState::CancellationRequested
            )
        })
        .cloned()
        .collect();
    for mut job in unfinished {
        let history = ledger.replay_verified()?;
        let has_receipt = history.iter().any(|event| {
            event.event_type == "evaluation.recorded"
                && event.event_id == format!("arena:evaluation:{}:recorded", job.evaluation_id)
        });
        let cancelled = job.state == JobState::CancellationRequested;
        if cancelled && has_receipt {
            return Err(ControlError::Projection(
                "cancelled Arena job has a committed evaluation receipt".to_owned(),
            ));
        }
        if has_receipt {
            if job.completed_trials != job.total_trials
                || job.ordered_trial_run_ids.iter().any(|run_id| {
                    !state.completed_runs.contains(run_id)
                        || state.run_results.get(run_id).is_none_or(|receipt| {
                            receipt.completion_reason != RunCompletionReason::Success
                        })
                })
            {
                return Err(ControlError::Projection(
                    "Arena receipt is missing complete signed trial evidence".to_owned(),
                ));
            }
            let index = EventIndex::build(&history);
            let recorded = load_recorded_evaluation_in(&index, artifacts, &job.evaluation_id)
                .map_err(|_| {
                    ControlError::Projection(
                        "Arena recovery receipt failed verification".to_owned(),
                    )
                })?;
            let evaluation = evaluation_record_from_recorded(&recorded);
            if evaluation.world_id != job.world_id
                || evaluation.parent_genome_id != job.parent_genome_id
                || evaluation.candidate_genome_id != job.candidate_genome_id
            {
                return Err(ControlError::Projection(
                    "Arena recovery receipt differs from admitted pair".to_owned(),
                ));
            }
            job.state = JobState::Succeeded;
            job.terminal = Some(JobTerminal::Succeeded);
            job.evaluation = Some(evaluation);
        } else {
            job.state = JobState::Interrupted;
            job.terminal = Some(if cancelled {
                JobTerminal::Cancelled
            } else {
                JobTerminal::Interrupted
            });
            job.evaluation = None;
        }
        job.phase = ArenaJobPhase::Terminal;
        let payload = serde_json::to_vec(&job)?;
        let event = ledger.append(EventInput::new(
            format!("arena-job:{}:terminal", job.evaluation_id),
            format!("arena-job:{}", job.evaluation_id),
            "arena.job.terminal",
            RUNTIME_ACTOR,
            timestamp_millis()?,
            payload,
        ))?;
        state.apply(&event, operator_token, run_result_verifier)?;
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRecord {
    run_id: String,
}

#[derive(Eq, PartialEq, Serialize)]
struct ProjectionSnapshot {
    frozen: bool,
    active_runs: Vec<String>,
    genomes: BTreeMap<String, GenomeRecord>,
    worlds: BTreeMap<String, WorldRecord>,
    jobs: BTreeMap<String, JobRecord>,
    arena_jobs: BTreeMap<String, ArenaJobRecord>,
    job_progress: BTreeMap<String, JobProgress>,
    evaluation_events: BTreeMap<String, u64>,
    run_results: BTreeMap<String, RunResultReceipt>,
    completed_runs: Vec<String>,
    event_count: u64,
}

#[allow(clippy::too_many_lines)]
fn validate_arena_job_record(
    event: &StoredEvent,
    record: &ArenaJobRecord,
) -> Result<(), ControlError> {
    validate_job_id(&record.evaluation_id)
        .map_err(|_| ControlError::Projection("Arena job identity is invalid".to_owned()))?;
    require_projection_text(&record.job_id, "job_id")?;
    require_projection_text(&record.environment_id, "environment_id")?;
    validate_content_id(&record.parent_genome_id, "genome")?;
    validate_content_id(&record.candidate_genome_id, "genome")?;
    validate_content_id(&record.world_id, "world")?;
    ArtifactId::parse(record.visible_manifest_id.clone())?;
    ArtifactId::parse(record.sealed_manifest_id.clone())?;
    ArtifactId::parse(record.evaluator_id.clone())?;
    validate_arena_environment_id(&record.environment_id)?;
    if let Some(candidate_environment_id) = &record.candidate_environment_id {
        if candidate_environment_id == &record.environment_id {
            // A mixed-pair field must actually be distinct; use `None`
            // instead of restating the shared environment.
            return Err(ControlError::Projection(
                "Arena candidate environment must differ from the parent's".to_owned(),
            ));
        }
        validate_arena_environment_id(candidate_environment_id)?;
    }
    let revision_valid = matches!(record.source_revision.len(), 40 | 64)
        && record
            .source_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit());
    if record.job_id != record.evaluation_id
        || record.parent_genome_id == record.candidate_genome_id
        || record.total_trials == 0
        || record.parent_trial_count == 0
        || record.parent_trial_count >= record.total_trials
        || record.ordered_trial_run_ids.len()
            != usize::try_from(record.total_trials).unwrap_or(usize::MAX)
        || record.completed_trials > record.total_trials
        || record.trial_budget.wall_millis == 0
        || record.overall_budget.wall_millis == 0
        || record.seed != PAIRED_EVALUATION_SEED
        || !revision_valid
        || record.caller_id != "control-daemon"
        || record.plan_commitment.len() != 64
        || !record
            .plan_commitment
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || record.worker_digest.len() != 64
        || !record
            .worker_digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ControlError::Projection(
            "Arena job plan is invalid".to_owned(),
        ));
    }
    let parent_count = usize::try_from(record.parent_trial_count).unwrap_or(usize::MAX);
    for (index, run_id) in record.ordered_trial_run_ids.iter().enumerate() {
        let (role, role_index) = if index < parent_count {
            ("parent", index)
        } else {
            ("candidate", index - parent_count)
        };
        if run_id != &paired_run_id(&record.evaluation_id, role, role_index) {
            return Err(ControlError::Projection(
                "Arena trial order is invalid".to_owned(),
            ));
        }
    }
    let expected_commitment = blake3::hash(&serde_json::to_vec(&(
        &record.evaluation_id,
        &record.parent_genome_id,
        &record.candidate_genome_id,
        &record.world_id,
        &record.visible_manifest_id,
        &record.sealed_manifest_id,
        &record.source_revision,
        &record.environment_id,
        &record.effective_candidate_environment_id(),
        &record.ordered_trial_run_ids,
    ))?);
    if expected_commitment.to_hex().as_str() != record.plan_commitment {
        return Err(ControlError::Projection(
            "Arena plan commitment is invalid".to_owned(),
        ));
    }
    let event_type = match record.state {
        JobState::Admitted => "arena.job.admitted",
        JobState::Running if record.phase == ArenaJobPhase::Scoring => "arena.job.scoring",
        JobState::Running if record.phase == ArenaJobPhase::Committing => "arena.job.committing",
        JobState::Running => "arena.job.running",
        JobState::CancellationRequested => "arena.job.cancellation_requested",
        JobState::Succeeded | JobState::Failed | JobState::Interrupted => "arena.job.terminal",
    };
    let suffix = event_type
        .strip_prefix("arena.job.")
        .ok_or_else(|| ControlError::Projection("Arena job event type is invalid".to_owned()))?;
    let canonical = serde_json::to_vec(record)?;
    if event.actor != RUNTIME_ACTOR
        || event.event_type != event_type
        || event.event_id != format!("arena-job:{}:{suffix}", record.evaluation_id)
        || event.aggregate_id != format!("arena-job:{}", record.evaluation_id)
        || event.payload != canonical
    {
        return Err(ControlError::Projection(
            "Arena job event crossed its canonical boundary".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(feature = "test-support")]
fn test_overall_wall(maximum: u64, requested: Option<&str>) -> Result<u64, ()> {
    let Some(requested) = requested else {
        return Ok(maximum);
    };
    let requested = requested.parse::<u64>().map_err(|_| ())?;
    if requested == 0 || requested > maximum {
        return Err(());
    }
    Ok(requested)
}

fn arena_job_immutable_fields_match(old: &ArenaJobRecord, new: &ArenaJobRecord) -> bool {
    old.job_id == new.job_id
        && old.evaluation_id == new.evaluation_id
        && old.parent_genome_id == new.parent_genome_id
        && old.candidate_genome_id == new.candidate_genome_id
        && old.world_id == new.world_id
        && old.visible_manifest_id == new.visible_manifest_id
        && old.sealed_manifest_id == new.sealed_manifest_id
        && old.evaluator_id == new.evaluator_id
        && old.source_revision == new.source_revision
        && old.worker_digest == new.worker_digest
        && old.environment_id == new.environment_id
        && old.candidate_environment_id == new.candidate_environment_id
        && old.seed == new.seed
        && old.trial_budget == new.trial_budget
        && old.overall_budget == new.overall_budget
        && old.ordered_trial_run_ids == new.ordered_trial_run_ids
        && old.parent_trial_count == new.parent_trial_count
        && old.total_trials == new.total_trials
        && old.plan_commitment == new.plan_commitment
        && old.caller_id == new.caller_id
        && old.receipt_timestamp_millis == new.receipt_timestamp_millis
}

fn validate_trace_receipt(event: &StoredEvent, receipt: &TraceReceipt) -> Result<(), ControlError> {
    require_projection_text(receipt.provenance.run_id(), "run_id")?;
    validate_content_id(receipt.provenance.genome_id(), "genome")?;
    validate_content_id(receipt.provenance.world_id(), "world")?;
    ArtifactId::parse(receipt.artifact_id.clone())?;
    if event.actor != "experience-plane"
        || event.event_id != receipt.event_id
        || event.aggregate_id != format!("run:{}", receipt.provenance.run_id())
    {
        return Err(ControlError::Projection(
            "trace receipt crossed its provenance boundary".to_owned(),
        ));
    }
    Ok(())
}

const fn trace_phase(kind: TraceKind) -> &'static str {
    match kind {
        TraceKind::LifecycleStarted => "started",
        TraceKind::LifecycleResumed => "resumed",
        TraceKind::LifecycleCompleted => "completed",
        TraceKind::ToolCalled => "tool_called",
        TraceKind::ToolResult => "tool_result",
        TraceKind::ContextComposed => "context_composed",
        TraceKind::MemoryRetrieved => "memory_retrieved",
        TraceKind::SubagentSpawned => "subagent_spawned",
        TraceKind::FileRead => "file_read",
        TraceKind::FileChanged => "file_changed",
        TraceKind::TestExecuted => "test_executed",
        TraceKind::CapabilityDenied => "capability_denied",
        TraceKind::CostObserved => "cost_observed",
        TraceKind::CheckpointCreated => "checkpoint_created",
        TraceKind::Error => "error",
        TraceKind::Retry => "retry",
        TraceKind::ModelResponse => "model_response",
    }
}

#[cfg(test)]
fn validate_run_result(
    event: &StoredEvent,
    run_result_verifier: &RunResultVerifier,
) -> Result<(), ControlError> {
    RunResultReceipt::parse_from_event(event, run_result_verifier)
        .map(|_| ())
        .map_err(|_| ControlError::Projection("canonical run result is invalid".to_owned()))
}

fn receipt_matches_job(receipt: &RunResultReceipt, job: &JobRecord) -> bool {
    receipt.run_id == job.run_id
        && receipt.genome_id == job.genome_id
        && receipt.world_id == job.world_id
        && receipt.source_revision == job.source_revision
        && receipt.task_id == job.task_id
        && receipt.input_commitment == job.input_commitment
        && receipt.seed == job.seed
        && receipt.environment_id == job.environment_id
        && receipt.budget == job.budget
}

fn trace_artifacts_for_run(
    history: &[StoredEvent],
    run_id: &str,
) -> Result<Vec<String>, ExecuteError> {
    history
        .iter()
        .filter(|event| event.event_type == "trace.recorded")
        .map(|event| {
            serde_json::from_slice::<TraceReceipt>(&event.payload)
                .map_err(|_| ExecuteError::Internal)
        })
        .filter_map(|receipt| match receipt {
            Ok(receipt) if receipt.provenance.run_id() == run_id => Some(Ok(receipt.artifact_id)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn run_completion_reason(reason: CompletionReason) -> RunCompletionReason {
    reason.into()
}

/// Validates a job/Arena environment identity's versioned shape: either the
/// reference worker's `reference-v1.<digest>` or a provider's
/// `provider-v1.<digest>` (see `provider_job_environment`).
fn validate_arena_environment_id(environment_id: &str) -> Result<(), ControlError> {
    let digest = environment_id
        .strip_prefix("reference-v1.")
        .or_else(|| environment_id.strip_prefix("provider-v1."))
        .ok_or_else(|| ControlError::Projection("Arena environment is invalid".to_owned()))?;
    ArtifactId::parse(digest.to_owned())?;
    Ok(())
}

fn validate_content_id<'a>(value: &'a str, namespace: &str) -> Result<&'a str, ControlError> {
    let prefix = format!("hephaestus:{namespace}:");
    let Some(hash) = value.strip_prefix(&prefix) else {
        return Err(ControlError::Projection(format!(
            "canonical {namespace} identity is malformed"
        )));
    };
    ArtifactId::parse(hash.to_owned())?;
    Ok(hash)
}

fn require_projection_text(value: &str, field: &str) -> Result<(), ControlError> {
    if value.trim().is_empty() {
        return Err(ControlError::Projection(format!(
            "canonical {field} is empty"
        )));
    }
    Ok(())
}

fn event_type(command: &Command) -> &'static str {
    match command {
        Command::Status => "control.status",
        Command::Freeze => "control.freeze",
        Command::Unfreeze => "control.unfreeze",
        Command::KillAll => "control.kill_all",
        Command::GenomeShow { .. } => "control.genome_show",
        Command::GenomePrompt { .. } => "control.genome_prompt",
        Command::GenomeList => "control.genome_list",
        Command::GenomeRegister { .. } => "control.genome_register",
        Command::GenomePropose { .. } => "control.genome_propose",
        Command::GenomeAssess { .. } => "control.genome_assess",
        Command::ForgeAnalyze { .. } => "control.forge_analyze",
        Command::WorldShow { .. } => "control.world_show",
        Command::WorldList => "control.world_list",
        Command::WorldRegister { .. } => "control.world_register",
        Command::ManifestPut { .. } => "control.manifest_put",
        Command::ArtifactPut { .. } => "control.artifact_put",
        Command::VerifierShow => "control.verifier_show",
        Command::RunSubmit { .. } => "control.run_submit",
        Command::JobStatus { .. } => "control.job_status",
        Command::JobKill { .. } => "control.job_kill",
        Command::RunReference { .. } => "control.run_reference",
        Command::RunEvaluation { .. } => "control.run_evaluation",
        Command::EvaluatePair { .. } => "control.evaluate_pair",
        Command::ArenaSelect { .. } => "control.arena_select",
        Command::ArenaInvariants { .. } => "control.arena_invariants",
        Command::ChampionSeed { .. } => "control.champion_seed",
        Command::ChampionPromote { .. } => "control.champion_promote",
        Command::ChampionRollback { .. } => "control.champion_rollback",
        Command::ChampionShow { .. } => "control.champion_show",
        Command::DriftRecord { .. } => "control.drift_record",
        Command::DriftShow { .. } => "control.drift_show",
        Command::DriftList { .. } => "control.drift_list",
        Command::CanaryStart { .. } => "control.canary_start",
        Command::CanaryAdvance { .. } => "control.canary_advance",
        Command::CanaryLiveCheck { .. } => "control.canary_live_check",
        Command::CanaryShow { .. } => "control.canary_show",
        Command::CanaryList { .. } => "control.canary_list",
        Command::GeneExtract { .. } => "control.gene_extract",
        Command::GeneTransfer { .. } => "control.gene_transfer",
        Command::GeneRecord { .. } => "control.gene_record",
        Command::GeneShow { .. } => "control.gene_show",
        Command::GeneList => "control.gene_list",
        Command::GeneSpeciate { .. } => "control.gene_speciate",
        Command::EvolveStart { .. } => "control.evolve_start",
        Command::EvolveStatus { .. } => "control.evolve_status",
        Command::EvolveCancel { .. } => "control.evolve_cancel",
        Command::MetaStrategyRegister { .. } => "control.meta_strategy_register",
        Command::MetaStrategyShow { .. } => "control.meta_strategy_show",
        Command::MetaStrategyList => "control.meta_strategy_list",
        Command::MetaEvaluate { .. } => "control.meta_evaluate",
        Command::MetaShow { .. } => "control.meta_show",
        Command::MetaStatus { .. } => "control.meta_status",
        Command::MetaList { .. } => "control.meta_list",
        Command::Replay => "control.replay",
        Command::RunList { .. } => "control.run_list",
        Command::EvaluationList { .. } => "control.evaluation_list",
        Command::DenialList { .. } => "control.denial_list",
        Command::DaemonStop => "control.daemon_stop",
        Command::McpCall { .. } => "mcp.call",
        Command::WorkerCredentialMint { .. } => "control.worker_credential_mint",
        Command::WorkerCredentialRevoke { .. } => "control.worker_credential_revoke",
        Command::RemoteRunSubmit { .. } => "control.remote_run_submit",
        Command::RemoteJobStatus { .. } => "control.remote_job_status",
    }
}

fn provider_execution_environment(provider: Provider) -> String {
    let name = match provider {
        Provider::Codex => "codex-cli",
        Provider::Claude => "claude-cli",
        Provider::Deterministic => "deterministic",
    };
    format!(
        "{name}-v1.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git",
        env!("CARGO_PKG_VERSION"),
        RUN_RESULT_SCHEMA_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn reference_environment_id() -> String {
    format!(
        "deterministic-v1.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git",
        env!("CARGO_PKG_VERSION"),
        RUN_RESULT_SCHEMA_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn validated_evaluation_budget(
    wall_millis: u64,
    maximum_output_bytes: u64,
    maximum_cost_microusd: u64,
) -> Result<Budget, ExecuteError> {
    if wall_millis == 0
        || wall_millis > MAX_EVALUATION_WALL_MILLIS
        || maximum_output_bytes == 0
        || maximum_output_bytes > MAX_EVALUATION_OUTPUT_BYTES
        || maximum_cost_microusd > MAX_EVALUATION_COST_MICROUSD
    {
        return Err(ExecuteError::Invalid("evaluation budget is invalid"));
    }
    let maximum_output_bytes = usize::try_from(maximum_output_bytes)
        .map_err(|_| ExecuteError::Invalid("evaluation output budget is invalid"))?;
    Budget::new(
        Duration::from_millis(wall_millis),
        maximum_output_bytes,
        maximum_cost_microusd,
    )
    .map_err(|_| ExecuteError::Invalid("evaluation budget is invalid"))
}

fn validate_source_repository(path: &Path) -> Result<PathBuf, ControlError> {
    let canonical = fs::canonicalize(path)?;
    if !canonical.is_dir() {
        return Err(ControlError::Protocol(
            "source repository is not a directory",
        ));
    }
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(&canonical)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()?;
    if !output.status.success() || output.stdout != b"true\n" {
        return Err(ControlError::Protocol(
            "source repository is not a Git worktree",
        ));
    }
    Ok(canonical)
}

fn timestamp_millis() -> Result<i64, ControlError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ControlError::Protocol("system clock precedes Unix epoch"))?
        .as_millis();
    i64::try_from(millis).map_err(|_| ControlError::Protocol("timestamp exceeds i64"))
}

fn prepare_private_directory(path: &Path) -> Result<(), ControlError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(ControlError::Protocol("data path cannot be a symlink"));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(ControlError::Protocol("data path is not a directory"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(path)?,
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn prepare_private_file(path: &Path) -> Result<(), ControlError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ControlError::Protocol("canonical file path is unsafe"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn take_writer_lock(path: &Path) -> Result<File, ControlError> {
    prepare_private_file(path)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.try_lock_exclusive()
        .map_err(|_| ControlError::AlreadyRunning)?;
    Ok(file)
}

fn load_or_create_token(path: &Path) -> Result<(String, [u8; 32]), ControlError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ControlError::Protocol("operator token path is unsafe"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut bytes = [0_u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut bytes)?;
            let token = hex_encode(&bytes);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(token.as_bytes())?;
            file.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    let token = fs::read_to_string(path)?;
    let bytes = hex_decode(&token)?;
    Ok((token, bytes))
}

fn load_or_create_run_result_signer(
    path: &Path,
    allow_create: bool,
) -> Result<RunResultSigner, ControlError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(ControlError::Protocol(
                "runtime producer key path is unsafe",
            ));
        }
        Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => {
            return Err(ControlError::Protocol(
                "runtime producer key permissions are unsafe",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && allow_create => {
            let mut seed = [0_u8; 32];
            File::open("/dev/urandom")?.read_exact(&mut seed)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?;
            file.write_all(&seed)?;
            file.sync_all()?;
            let parent = path
                .parent()
                .ok_or(ControlError::Protocol("runtime producer key has no parent"))?;
            File::open(parent)?.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ControlError::Protocol(
                "runtime producer key is missing for canonical results or a registered World verifier",
            ));
        }
        Err(error) => return Err(error.into()),
    }
    let bytes = fs::read(path)?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ControlError::Protocol("runtime producer key is malformed"))?;
    Ok(RunResultSigner::from_seed(seed))
}

fn reject_legacy_run_result_history(history: &[StoredEvent]) -> Result<(), ControlError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == "run.result_recorded")
    {
        let value: serde_json::Value = serde_json::from_slice(&event.payload)?;
        let signed = value.get("claims").is_some()
            && value.get("producer_key_id").is_some()
            && value.get("signature").is_some();
        if !signed {
            return Err(ControlError::Protocol(
                "legacy unsigned run-result history is incompatible; back up the data directory and reinitialize it before this release",
            ));
        }
    }
    Ok(())
}

fn anchored_world_verifier(
    registered: &RegisteredObjects,
    artifacts: &dyn ArtifactBackend,
) -> Result<Option<[u8; 32]>, ControlError> {
    let mut anchored = None;
    for world in registered.worlds() {
        let compiled = world.compiled();
        let Some(verifier_id) = compiled.evaluator_artifact("arena.runtime_verifier") else {
            continue;
        };
        let verifier_bytes = artifacts.get(&ArtifactId::parse(verifier_id)?)?;
        let verifier: [u8; 32] = verifier_bytes.try_into().map_err(|_| {
            ControlError::Projection("World runtime verifier is not an Ed25519 key".to_owned())
        })?;
        RunResultVerifier::from_public_key_bytes(verifier).map_err(|_| {
            ControlError::Projection("World runtime verifier is not an Ed25519 key".to_owned())
        })?;
        if anchored
            .as_ref()
            .is_some_and(|existing| existing != &verifier)
        {
            return Err(ControlError::Projection(
                "registered Worlds anchor different runtime verifiers".to_owned(),
            ));
        }
        anchored = Some(verifier);
    }
    Ok(anchored)
}

fn registration_control_error(error: RegistrationError) -> ControlError {
    match error {
        RegistrationError::Ledger(error) => ControlError::Ledger(error),
        error => ControlError::Projection(error.to_string()),
    }
}

fn hex_encode(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn hex_decode(value: &str) -> Result<[u8; 32], ControlError> {
    if value.len() != 64 {
        return Err(ControlError::Protocol("operator token is malformed"));
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

fn hex_nibble(byte: u8) -> Result<u8, ControlError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ControlError::Protocol("operator token is malformed")),
    }
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn remove_stale_socket(path: &Path) -> Result<(), ControlError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => return Err(ControlError::Protocol("control socket path is unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;

#[path = "handlers_genome.rs"]
mod handlers_genome;

#[path = "handlers_evolve_meta.rs"]
mod handlers_evolve_meta;

#[path = "worker_remote.rs"]
mod worker_remote;

#[path = "job_exec.rs"]
mod job_exec;

#[path = "run_exec.rs"]
mod run_exec;

#[path = "genome_ops.rs"]
mod genome_ops;

#[path = "champion.rs"]
mod champion;

use champion::{
    CHAMPION_EVENT_TYPE, ChampionRequest, champion_aggregate_id, champion_event_id,
    champion_projection, champion_transition_payload, champion_transition_record,
    existing_champion_transition, validate_reason, verify_champion_history,
    verify_champion_history_with,
};

#[path = "gene_bank.rs"]
mod gene_bank;

use gene_bank::{
    CONTRADICTION_EVENT_TYPE, GENE_EVENT_TYPE, SPECIES_EVENT_TYPE, TRANSFER_APPLIED_EVENT_TYPE,
    TRANSFER_RECORDED_EVENT_TYPE, contradiction_event_id, decode_gene_extracted,
    decode_transfer_applied, decode_transfer_recorded, detect_contradiction,
    existing_contradiction, existing_gene, existing_species, existing_transfer_applied,
    existing_transfer_recorded, gene_aggregate, gene_aggregate_id, gene_event_id,
    gene_extraction_payload, gene_record, gene_summaries, speciation_payload, species_aggregate_id,
    species_event_id, species_record, transfer_aggregate_id, transfer_applied_event_id,
    transfer_applied_payload, transfer_record, transfer_recorded_event_id,
    transfer_recorded_payload, verify_gene_bank_history, verify_gene_bank_history_with,
};
#[cfg(test)]
use gene_bank::{GENE_MIN_EVIDENCE_TRIALS, SPECIATION_MIN_EFFECT_BPS};

#[path = "evolve.rs"]
mod evolve;

use evolve::{
    EVOLUTION_CANCEL_TYPE, EVOLUTION_FINISHED_TYPE, EVOLUTION_GENERATION_TYPE,
    EVOLUTION_STARTED_TYPE, TRIALS_PER_GENERATION, active_evolution_run_id, evolution_aggregate_id,
    evolution_cancel_event_id, evolution_finished_event_id, evolution_generation_event_id,
    evolution_projection, evolution_started_event_id, verify_evolution_history,
};

#[path = "meta_evolve.rs"]
mod meta_evolve;

use meta_evolve::{
    BOOTSTRAP_ALGORITHM, META_EVALUATION_ADMITTED_EVENT_TYPE, META_EVALUATION_EVENT_TYPE,
    META_STRATEGY_EVENT_TYPE, RESAMPLES, champion_after, descendant_verdict,
    meta_evaluation_admitted_event_id, meta_evaluation_admitted_ids,
    meta_evaluation_admitted_projection, meta_evaluation_aggregate_id, meta_evaluation_event_id,
    meta_evaluation_list, meta_evaluation_projection, meta_strategy_aggregate_id,
    meta_strategy_event_id, meta_strategy_id, meta_strategy_list, meta_strategy_projection,
    paired_bootstrap, promotions_of, verify_meta_evolution_history,
};

#[path = "drift.rs"]
mod drift;

use drift::{
    DRIFT_EVENT_TYPE, decode_drift_record, drift_event_input, drift_record_payload,
    existing_drift_record, verify_drift_history, verify_drift_history_with,
};

#[path = "canary.rs"]
mod canary;

use canary::{
    CanaryRequest, canary_transition_payload, existing_canary_transition, verify_canary_history,
    verify_canary_history_with,
};

#[path = "adaptation.rs"]
mod adaptation;

use adaptation::{
    adaptation_analysis_id, adaptation_assessment_id, adaptation_canary_id,
    adaptation_diagnostic_evaluation_id, adaptation_projection, adaptation_proposal_id,
    adaptation_shadow_evaluation_id, adaptation_stage_evaluation_id, finished_event_input,
    started_event_input, verify_drift_adaptation_history,
};
