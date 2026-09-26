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
    TraceKind, TraceReceipt,
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
    EvolutionFinishReason, EvolutionFinishedPayload, EvolutionGenerationPayload,
    EvolutionRunRecord, EvolutionRunState, EvolutionStartedPayload, EvolverStrategyConfig,
    ForgeAnalysisBinding, ForgeAnalysisRecord, ForgeAssessmentEventRecord, ForgeAssessmentOutcome,
    ForgeAssessmentPayload, ForgeAssessmentRecord, ForgeProposalEventRecord, ForgeProposalPayload,
    ForgeProposalRecord, GeneSelectionPolicy, GeneTransferOutcome, GenomeRecord, InvariantRecord,
    JobProgress, JobRecord, JobState, JobTerminal, MAX_LIST_LIMIT, McpDecision,
    MetaEvaluationPayload, MetaLineageOutcome, MetaStrategyRegisteredPayload,
    MutationPrioritization, RemoteJobState, ResponseData, RunCompletionReason, RunListEntry,
    SelectionEventRecord, SelectionRecord, WorkerScope, WorldRecord,
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
fn spawn_named_thread<T, F>(
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
    worker: Arc<PinnedReferenceWorker>,
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
    data_dir: PathBuf,
    guardian: PathBuf,
    protected_paths: Vec<PathBuf>,
    worker: Arc<PinnedReferenceWorker>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
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

    fn propose_genome_command(&mut self, command: Command) -> Result<ResponseData, ExecuteError> {
        let Command::GenomePropose {
            proposal_id,
            selection_event_id,
            parent_genome_id,
            hypothesis,
            analysis_id,
            cluster_index,
        } = command
        else {
            return Err(ExecuteError::Internal);
        };
        let source = match (hypothesis, analysis_id, cluster_index) {
            (Some(hypothesis), None, None) => ForgeHypothesisSource::Operator(hypothesis),
            (None, Some(analysis_id), Some(cluster_index)) => ForgeHypothesisSource::Analysis {
                analysis_id,
                cluster_index,
            },
            _ => return Err(ExecuteError::Internal),
        };
        self.propose_genome_from_source(
            &proposal_id,
            &selection_event_id,
            &parent_genome_id,
            source,
        )
    }

    fn assess_genome_command(&mut self, command: Command) -> Result<ResponseData, ExecuteError> {
        let Command::GenomeAssess {
            assessment_id,
            proposal_id,
            selection_event_id,
        } = command
        else {
            return Err(ExecuteError::Internal);
        };
        self.assess_genome(&assessment_id, &proposal_id, &selection_event_id)
    }

    fn champion_transition_command(
        &mut self,
        command: Command,
    ) -> Result<ResponseData, ExecuteError> {
        let (transition_id, request) = match command {
            Command::ChampionSeed {
                transition_id,
                world_id,
                genome_id,
                reason,
            } => (
                transition_id,
                ChampionRequest::Seed {
                    world_id,
                    genome_id,
                    reason,
                },
            ),
            Command::ChampionPromote {
                transition_id,
                assessment_id,
            } => (transition_id, ChampionRequest::Promote { assessment_id }),
            Command::ChampionRollback {
                transition_id,
                world_id,
                reason,
            } => (
                transition_id,
                ChampionRequest::Rollback { world_id, reason },
            ),
            _ => return Err(ExecuteError::Internal),
        };
        self.transition_champion(&transition_id, &request)
    }

    fn transition_champion(
        &mut self,
        transition_id: &str,
        request: &ChampionRequest,
    ) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        validate_job_id(transition_id)
            .map_err(|_| ExecuteError::Invalid("transition_id is invalid"))?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        verify_champion_history(&storage.artifacts, &history, &self.state.registered)
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) = existing_champion_transition(&history, transition_id, request)? {
            return Ok(ResponseData::ChampionTransition {
                transition: Box::new(existing),
            });
        }
        let payload = champion_transition_payload(
            &storage.artifacts,
            &history,
            &self.state.registered,
            transition_id,
            request,
        )?;
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        let event = storage
            .ledger
            .append(EventInput::new(
                champion_event_id(transition_id),
                champion_aggregate_id(&payload.world_id),
                CHAMPION_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::ChampionTransition {
            transition: Box::new(champion_transition_record(payload, &event)),
        })
    }

    fn champion_show(&self, world_id: &str) -> Result<ResponseData, ExecuteError> {
        self.state
            .registered
            .world(world_id)
            .ok_or(ExecuteError::NotFound)?;
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let champion =
            champion_projection(&history, world_id).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Champion {
            champion: Box::new(champion),
        })
    }

    fn record_drift(
        &mut self,
        drift_id: &str,
        world_id: &str,
        kind: DriftKind,
        evidence_evaluation_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        validate_job_id(drift_id).map_err(|_| ExecuteError::Invalid("drift_id is invalid"))?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        verify_drift_history(&storage.artifacts, &history, &self.state.registered)
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) =
            existing_drift_record(&history, drift_id, world_id, kind, evidence_evaluation_id)?
        {
            return Ok(ResponseData::Drift {
                drift: Box::new(existing),
            });
        }
        let payload = drift_record_payload(
            &storage.artifacts,
            &history,
            &self.state.registered,
            drift_id,
            world_id,
            kind,
            evidence_evaluation_id,
        )?;
        let event = storage
            .ledger
            .append(drift_event_input(
                &payload,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
            )?)
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Drift {
            drift: Box::new(
                drift::decode_drift_record(&event)
                    .ok()
                    .and_then(|decoded| drift::drift_record(&history, decoded, &event).ok())
                    .ok_or(ExecuteError::Internal)?,
            ),
        })
    }

    fn drift_show(&self, drift_id: &str) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let drift = drift::drift_projection(&history, drift_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        Ok(ResponseData::Drift {
            drift: Box::new(drift),
        })
    }

    /// Recent drift records, newest first, bounded by `limit`.
    fn drift_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let drifts = drift::drift_list(&history, limit).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::DriftList { drifts })
    }

    fn canary_transition_command(
        &mut self,
        command: Command,
    ) -> Result<ResponseData, ExecuteError> {
        let (canary_id, request) = match command {
            Command::CanaryStart {
                canary_id,
                world_id,
                candidate_genome_id,
                assessment_id,
            } => (
                canary_id,
                CanaryRequest::Start {
                    world_id,
                    candidate_genome_id,
                    assessment_id,
                },
            ),
            Command::CanaryAdvance {
                canary_id,
                evidence_evaluation_id,
            } => (
                canary_id,
                CanaryRequest::Advance {
                    evidence_evaluation_id,
                },
            ),
            Command::CanaryLiveCheck {
                canary_id,
                evidence_evaluation_id,
            } => (
                canary_id,
                CanaryRequest::LiveCheck {
                    evidence_evaluation_id,
                },
            ),
            _ => return Err(ExecuteError::Internal),
        };
        self.transition_canary(&canary_id, &request)
    }

    #[allow(clippy::too_many_lines)]
    fn transition_canary(
        &mut self,
        canary_id: &str,
        request: &CanaryRequest,
    ) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        validate_job_id(canary_id).map_err(|_| ExecuteError::Invalid("canary_id is invalid"))?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        verify_canary_history(&storage.artifacts, &history, &self.state.registered)
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) = existing_canary_transition(&history, canary_id, request)? {
            return Ok(ResponseData::CanaryTransition {
                transition: Box::new(existing),
            });
        }
        let mut payload = canary_transition_payload(
            &storage.artifacts,
            &history,
            &self.state.registered,
            canary_id,
            request,
        )?;
        let timestamp = timestamp_millis().map_err(|_| ExecuteError::Internal)?;

        // A completing advance or a live regression check also appends the
        // one existing Champion transition event that policy already admits;
        // this reuses `champion::champion_transition_payload` rather than
        // duplicating promotion or rollback policy.
        if payload.kind == CanaryTransitionKind::Advanced && payload.stage == CanaryStage::Completed
        {
            let promotion_transition_id = canary::canary_id_promotion_transition_id(canary_id);
            let promotion_payload = champion_transition_payload(
                &storage.artifacts,
                &history,
                &self.state.registered,
                &promotion_transition_id,
                &ChampionRequest::Promote {
                    assessment_id: payload.assessment_id.clone(),
                },
            )?;
            let payload_value =
                serde_json::to_value(&promotion_payload).map_err(|_| ExecuteError::Internal)?;
            let payload_bytes =
                serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
            storage
                .ledger
                .append(EventInput::new(
                    champion_event_id(&promotion_transition_id),
                    champion_aggregate_id(&promotion_payload.world_id),
                    CHAMPION_EVENT_TYPE,
                    OPERATOR_ACTOR,
                    timestamp,
                    payload_bytes,
                ))
                .map_err(|_| ExecuteError::Internal)?;
        } else if payload.kind == CanaryTransitionKind::LiveRegressionDetected {
            let rollback_transition_id = canary::canary_id_rollback_transition_id(canary_id);
            let rollback_payload = champion_transition_payload(
                &storage.artifacts,
                &history,
                &self.state.registered,
                &rollback_transition_id,
                &ChampionRequest::Rollback {
                    world_id: payload.world_id.clone(),
                    reason: format!(
                        "canary {canary_id} automatic rollback: live evaluation {} regressed beyond the documented threshold",
                        payload
                            .evidence
                            .as_ref()
                            .map(|evidence| evidence.evidence_evaluation_id.as_str())
                            .unwrap_or_default()
                    ),
                },
            )?;
            let payload_value =
                serde_json::to_value(&rollback_payload).map_err(|_| ExecuteError::Internal)?;
            let payload_bytes =
                serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
            let rollback_event = storage
                .ledger
                .append(EventInput::new(
                    champion_event_id(&rollback_transition_id),
                    champion_aggregate_id(&rollback_payload.world_id),
                    CHAMPION_EVENT_TYPE,
                    OPERATOR_ACTOR,
                    timestamp,
                    payload_bytes,
                ))
                .map_err(|_| ExecuteError::Internal)?;
            payload.champion_rollback_event_hash = Some(hex_encode(&rollback_event.hash));
        }

        let event = storage
            .ledger
            .append(canary::canary_event_input(&payload, timestamp)?)
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::CanaryTransition {
            transition: Box::new(
                canary::decode_canary_transition(&event)
                    .ok()
                    .map(|decoded| canary::canary_transition_record(decoded, &event))
                    .ok_or(ExecuteError::Internal)?,
            ),
        })
    }

    fn canary_show(&self, canary_id: &str) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let canary = canary::canary_projection(&history, canary_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        Ok(ResponseData::Canary {
            canary: Box::new(canary),
        })
    }

    /// Recent canaries, newest first, bounded by `limit`.
    fn canary_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let canaries = canary::canary_list(&history, limit).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::CanaryList { canaries })
    }

    fn gene_extract(
        &mut self,
        gene_id: &str,
        promotion_transition_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        validate_job_id(gene_id).map_err(|_| ExecuteError::Invalid("gene_id is invalid"))?;
        validate_job_id(promotion_transition_id)
            .map_err(|_| ExecuteError::Invalid("promotion_transition_id is invalid"))?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) = existing_gene(&history, gene_id, promotion_transition_id)? {
            return Ok(ResponseData::Gene {
                gene: Box::new(existing),
            });
        }
        let payload = gene_extraction_payload(
            &storage.artifacts,
            &history,
            &self.state.registered,
            gene_id,
            promotion_transition_id,
        )?;
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        let event = storage
            .ledger
            .append(EventInput::new(
                gene_event_id(gene_id),
                gene_aggregate_id(gene_id),
                GENE_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::Gene {
            gene: Box::new(gene_record(payload, &event)),
        })
    }

    fn gene_transfer_apply(
        &mut self,
        trial_id: &str,
        gene_id: &str,
        to_genome_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if self.state.freeze.is_frozen() {
            return Err(ExecuteError::Invalid("evolution is frozen"));
        }
        validate_job_id(trial_id).map_err(|_| ExecuteError::Invalid("trial_id is invalid"))?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) =
            existing_transfer_applied(&history, trial_id, gene_id, to_genome_id)?
        {
            let event = history
                .iter()
                .find(|event| event.event_id == transfer_applied_event_id(trial_id))
                .ok_or(ExecuteError::Internal)?;
            return Ok(ResponseData::GeneTransfer {
                trial: Box::new(transfer_record(existing, event, None, None)),
            });
        }
        let payload = transfer_applied_payload(
            &self.state.registered,
            &storage.artifacts,
            &history,
            trial_id,
            gene_id,
            to_genome_id,
        )?;
        if self
            .state
            .registered
            .genome(&payload.child.genome_id)
            .is_some()
        {
            return Err(ExecuteError::Rejected(
                "derived transfer child identity is already registered".to_owned(),
            ));
        }
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        let event = storage
            .ledger
            .append(EventInput::new(
                transfer_applied_event_id(trial_id),
                transfer_aggregate_id(trial_id),
                TRANSFER_APPLIED_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::GeneTransfer {
            trial: Box::new(transfer_record(payload, &event, None, None)),
        })
    }

    fn gene_transfer_record(
        &mut self,
        trial_id: &str,
        evaluation_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        validate_job_id(trial_id).map_err(|_| ExecuteError::Invalid("trial_id is invalid"))?;
        if evaluation_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("evaluation_id is required"));
        }
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let applied_event = history
            .iter()
            .find(|event| event.event_id == transfer_applied_event_id(trial_id))
            .ok_or(ExecuteError::NotFound)?
            .clone();
        let applied =
            decode_transfer_applied(&applied_event).map_err(|_| ExecuteError::Internal)?;

        if let Some(existing) = existing_transfer_recorded(&history, trial_id, evaluation_id)? {
            let recorded_event = history
                .iter()
                .find(|event| event.event_id == transfer_recorded_event_id(trial_id))
                .ok_or(ExecuteError::Internal)?;
            return Ok(ResponseData::GeneTransfer {
                trial: Box::new(transfer_record(
                    applied,
                    &applied_event,
                    Some(existing),
                    Some(recorded_event),
                )),
            });
        }
        let payload = transfer_recorded_payload(
            &storage.artifacts,
            &history,
            &self.state.registered,
            trial_id,
            evaluation_id,
        )?;
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        let recorded_event = storage
            .ledger
            .append(EventInput::new(
                transfer_recorded_event_id(trial_id),
                transfer_aggregate_id(trial_id),
                TRANSFER_RECORDED_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;

        // A contradiction is an automatic, idempotent side effect of
        // recording a trial: the first time both a positive and a negative
        // outcome exist for this Gene, record it once and never overwrite it.
        let refreshed_history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if existing_contradiction(&refreshed_history, &applied.gene_id).is_none()
            && let Some(contradiction) = detect_contradiction(&refreshed_history, &applied.gene_id)
                .map_err(|_| ExecuteError::Internal)?
        {
            let contradiction_value =
                serde_json::to_value(&contradiction).map_err(|_| ExecuteError::Internal)?;
            let contradiction_bytes =
                serde_json::to_vec(&contradiction_value).map_err(|_| ExecuteError::Internal)?;
            storage
                .ledger
                .append(EventInput::new(
                    contradiction_event_id(&applied.gene_id),
                    gene_aggregate_id(&applied.gene_id),
                    CONTRADICTION_EVENT_TYPE,
                    OPERATOR_ACTOR,
                    timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                    contradiction_bytes,
                ))
                .map_err(|_| ExecuteError::Internal)?;
        }

        self.refresh_projection()?;
        Ok(ResponseData::GeneTransfer {
            trial: Box::new(transfer_record(
                applied,
                &applied_event,
                Some(payload),
                Some(&recorded_event),
            )),
        })
    }

    fn gene_show(&self, gene_id: &str) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let aggregate = gene_aggregate(&history, gene_id)?;
        Ok(ResponseData::GeneAggregate {
            aggregate: Box::new(aggregate),
        })
    }

    fn gene_list(&self) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Genes {
            genes: gene_summaries(&history)?,
        })
    }

    fn gene_speciate(
        &mut self,
        species_id: &str,
        gene_id: &str,
        domain_world_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        validate_job_id(species_id).map_err(|_| ExecuteError::Invalid("species_id is invalid"))?;
        self.state
            .registered
            .world(domain_world_id)
            .ok_or(ExecuteError::NotFound)?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) = existing_species(&history, species_id, gene_id, domain_world_id)? {
            return Ok(ResponseData::GeneSpecies {
                species: Box::new(existing),
            });
        }
        let payload = speciation_payload(&history, species_id, gene_id, domain_world_id)?;
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        let event = storage
            .ledger
            .append(EventInput::new(
                species_event_id(species_id),
                species_aggregate_id(species_id),
                SPECIES_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::GeneSpecies {
            species: Box::new(species_record(payload, &event)),
        })
    }

    /// Admits (or idempotently re-admits) one autonomous evolution run. All
    /// subsequent progress is made by `advance_evolution`, called every tick
    /// of the daemon's own reconciliation loop, never synchronously here.
    #[allow(clippy::too_many_lines)]
    fn evolve_start(&mut self, command: Command) -> Result<ResponseData, ExecuteError> {
        let Command::EvolveStart {
            run_id,
            world_id,
            from_genome_id,
            generations,
            budget,
            strategy_id,
        } = command
        else {
            return Err(ExecuteError::Internal);
        };
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if active_evolution_run_id(&history)
            .map_err(|_| ExecuteError::Internal)?
            .is_some_and(|active| active != run_id)
        {
            return Err(ExecuteError::Busy);
        }
        if let Some(existing) =
            evolution_projection(&history, &run_id).map_err(|_| ExecuteError::Internal)?
        {
            if existing.world_id != world_id
                || existing.from_genome_id != from_genome_id
                || existing.max_generations != generations
                || existing.max_paired_trials != budget
                || existing.strategy_id != strategy_id
            {
                return Err(ExecuteError::Rejected(
                    "run_id is already bound to a different evolution configuration".to_owned(),
                ));
            }
            return Ok(ResponseData::Evolution {
                run: Box::new(existing),
            });
        }
        if let Some(strategy_id) = &strategy_id {
            meta_strategy_projection(&history, strategy_id)
                .map_err(|_| ExecuteError::Internal)?
                .ok_or(ExecuteError::NotFound)?;
        }

        self.state
            .registered
            .world(&world_id)
            .ok_or(ExecuteError::NotFound)?;
        let from_genome_world_id = self
            .state
            .registered
            .genome(&from_genome_id)
            .ok_or(ExecuteError::NotFound)?
            .record()
            .world_id
            .clone();
        if from_genome_world_id != world_id {
            return Err(ExecuteError::Rejected(
                "from_genome_id is not compiled under the requested World".to_owned(),
            ));
        }
        let baseline_genome_id = self
            .state
            .registered
            .genomes()
            .filter(|genome| {
                genome.record().world_id == world_id && genome.record().genome_id != from_genome_id
            })
            .map(|genome| genome.record().genome_id.clone())
            .min()
            .ok_or_else(|| {
                ExecuteError::Rejected(
                    "World needs at least one other registered Genome to serve as the evolve \
                     engine's comparison baseline"
                        .to_owned(),
                )
            })?;

        let champion_genome_id = champion_projection(&history, &world_id)
            .map_err(|_| ExecuteError::Internal)?
            .champion_genome_id;
        match champion_genome_id {
            None => {
                self.transition_champion(
                    &format!("evolve-{run_id}-seed"),
                    &ChampionRequest::Seed {
                        world_id: world_id.clone(),
                        genome_id: from_genome_id.clone(),
                        reason: format!("evolve run {run_id} generation-zero genesis seed"),
                    },
                )?;
            }
            Some(current) if current == from_genome_id => {}
            Some(_) => {
                return Err(ExecuteError::Rejected(
                    "World Champion does not match from_genome_id".to_owned(),
                ));
            }
        }

        let payload = EvolutionStartedPayload {
            schema_version: 1,
            run_id: run_id.clone(),
            world_id,
            from_genome_id,
            baseline_genome_id,
            max_generations: generations,
            max_paired_trials: budget,
            strategy_id,
        };
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                evolution_started_event_id(&run_id),
                evolution_aggregate_id(&run_id),
                EVOLUTION_STARTED_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        self.evolve_status(&run_id)
    }

    /// Read-only, replay-verified progress of one evolution run.
    fn evolve_status(&self, run_id: &str) -> Result<ResponseData, ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let run = evolution_projection(&history, run_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        Ok(ResponseData::Evolution { run: Box::new(run) })
    }

    /// Requests cooperative cancellation of one active evolution run. An
    /// in-flight Arena job belonging to it is cancelled the same way
    /// `kill --all` cancels any other active job; the run itself finishes on
    /// a later reconciliation tick once that job reaches a terminal state.
    fn evolve_cancel(&mut self, run_id: &str) -> Result<ResponseData, ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let run = evolution_projection(&history, run_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        if run.state == EvolutionRunState::Finished || run.cancel_requested {
            return Ok(ResponseData::Evolution { run: Box::new(run) });
        }
        self.request_active_job_cancellation()?;
        let payload = EvolutionCancelPayload {
            schema_version: 1,
            run_id: run_id.to_owned(),
        };
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                evolution_cancel_event_id(run_id),
                evolution_aggregate_id(run_id),
                EVOLUTION_CANCEL_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        self.evolve_status(run_id)
    }

    /// Registers (or idempotently re-resolves) one Evolver strategy Genome.
    /// The strategy's identity is derived entirely from its canonical
    /// content, exactly like a compiled Genome; registration appends one
    /// `meta_strategy.registered` event and nothing else.
    fn meta_strategy_register(&mut self, path: &str) -> Result<ResponseData, ExecuteError> {
        let source = read_source_text(path, MAX_SOURCE_FILE_BYTES)?;
        let config: EvolverStrategyConfig = serde_json::from_str(&source).map_err(|error| {
            ExecuteError::Rejected(format!("strategy source rejected: {error}"))
        })?;
        if config.schema_version != 1 {
            return Err(ExecuteError::Rejected(
                "strategy schema_version must be 1".to_owned(),
            ));
        }
        if config.generation_count == 0 {
            return Err(ExecuteError::Rejected(
                "strategy generation_count must be positive".to_owned(),
            ));
        }
        if config.experiment_allocation < TRIALS_PER_GENERATION {
            return Err(ExecuteError::Rejected(
                "strategy experiment_allocation must allow at least one generation".to_owned(),
            ));
        }
        if config.candidate_count == 0 {
            return Err(ExecuteError::Rejected(
                "strategy candidate_count must be positive".to_owned(),
            ));
        }
        let strategy_id = meta_strategy_id(&config).map_err(|_| ExecuteError::Internal)?;
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) =
            meta_strategy_projection(&history, &strategy_id).map_err(|_| ExecuteError::Internal)?
        {
            return Ok(ResponseData::MetaStrategy {
                strategy: Box::new(existing),
            });
        }
        if let Some(parent_id) = config.parent_strategy_id.as_deref() {
            if parent_id == strategy_id {
                return Err(ExecuteError::Rejected(
                    "strategy cannot declare itself as its own parent".to_owned(),
                ));
            }
            if meta_strategy_projection(&history, parent_id)
                .map_err(|_| ExecuteError::Internal)?
                .is_none()
            {
                return Err(ExecuteError::NotFound);
            }
        }
        let payload = MetaStrategyRegisteredPayload {
            schema_version: 1,
            strategy_id: strategy_id.clone(),
            config,
        };
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                meta_strategy_event_id(&strategy_id),
                meta_strategy_aggregate_id(&strategy_id),
                META_STRATEGY_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        self.meta_strategy_show(&strategy_id)
    }

    /// Read-only lookup of one registered Evolver strategy Genome.
    fn meta_strategy_show(&self, strategy_id: &str) -> Result<ResponseData, ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let strategy = meta_strategy_projection(&history, strategy_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        Ok(ResponseData::MetaStrategy {
            strategy: Box::new(strategy),
        })
    }

    /// Every registered Evolver strategy Genome, oldest first.
    fn meta_strategy_list_response(&self) -> Result<ResponseData, ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let strategies = meta_strategy_list(&history).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::MetaStrategies { strategies })
    }

    /// Drives one lineage's evolve run to completion using exactly the
    /// existing evolve engine (`evolve_start` plus the same reconciliation
    /// step `service_async_messages` calls every tick), never a bespoke
    /// meta-only code path. Blocks the calling connection until the run
    /// finishes or a generous deadline elapses.
    fn drive_evolve_run(
        &mut self,
        run_id: &str,
        world_id: &str,
        from_genome_id: &str,
        generations: u32,
        budget: u64,
        strategy_id: &str,
    ) -> Result<EvolutionRunRecord, ExecuteError> {
        self.evolve_start(Command::EvolveStart {
            run_id: run_id.to_owned(),
            world_id: world_id.to_owned(),
            from_genome_id: from_genome_id.to_owned(),
            generations,
            budget,
            strategy_id: Some(strategy_id.to_owned()),
        })?;
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let history = self
                .storage
                .as_ref()
                .ok_or(ExecuteError::Internal)?
                .ledger
                .replay_verified()
                .map_err(|_| ExecuteError::Internal)?;
            let run = evolution_projection(&history, run_id)
                .map_err(|_| ExecuteError::Internal)?
                .ok_or(ExecuteError::Internal)?;
            if run.state == EvolutionRunState::Finished {
                return Ok(run);
            }
            if Instant::now() >= deadline {
                return Err(ExecuteError::Rejected(format!(
                    "meta-evaluation lineage run {run_id} did not finish before its deadline"
                )));
            }
            self.service_async_messages()
                .map_err(|_| ExecuteError::Internal)?;
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Cooperatively rolls a World's Champion back to `target_genome_id`,
    /// one promotion at a time, so a second strategy's run starts from
    /// exactly the same lineage state as the first. Used only between and
    /// after a meta-evaluation's own paired runs; it never touches a
    /// Champion an operator did not already hand this lineage to `evolve`.
    fn rollback_champion_to(
        &mut self,
        transition_id_prefix: &str,
        world_id: &str,
        target_genome_id: &str,
        max_attempts: u32,
    ) -> Result<(), ExecuteError> {
        for attempt in 0..=max_attempts {
            let history = self
                .storage
                .as_ref()
                .ok_or(ExecuteError::Internal)?
                .ledger
                .replay_verified()
                .map_err(|_| ExecuteError::Internal)?;
            let champion_id = champion_projection(&history, world_id)
                .map_err(|_| ExecuteError::Internal)?
                .champion_genome_id;
            if champion_id.as_deref() == Some(target_genome_id) {
                return Ok(());
            }
            self.transition_champion(
                &format!("{transition_id_prefix}-rollback-{attempt}"),
                &ChampionRequest::Rollback {
                    world_id: world_id.to_owned(),
                    reason: "meta-evaluation restoring the held-out lineage's starting Champion"
                        .to_owned(),
                },
            )?;
        }
        Err(ExecuteError::Internal)
    }

    /// Runs a paired meta-evaluation of two Evolver strategies over the
    /// requested held-out base lineages, driving the existing evolve engine
    /// unmodified and recording one replay-verified receipt. Idempotent on
    /// `meta_run_id`.
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    fn meta_evaluate(&mut self, command: Command) -> Result<ResponseData, ExecuteError> {
        let Command::MetaEvaluate {
            meta_run_id,
            strategy_a_id,
            strategy_b_id,
            lineages,
            confidence_bps,
            bootstrap_seed,
        } = command
        else {
            return Err(ExecuteError::Internal);
        };
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) = meta_evaluation_projection(&history, &meta_run_id)
            .map_err(|_| ExecuteError::Internal)?
        {
            return Ok(ResponseData::MetaEvaluation {
                receipt: Box::new(existing),
            });
        }
        let strategy_a = meta_strategy_projection(&history, &strategy_a_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        let strategy_b = meta_strategy_projection(&history, &strategy_b_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;

        let mut outcomes = Vec::with_capacity(lineages.len());
        for (index, lineage) in lineages.iter().enumerate() {
            let run_a_id = format!("meta-{meta_run_id}-a-{index}");
            let run_b_id = format!("meta-{meta_run_id}-b-{index}");
            let run_a = self.drive_evolve_run(
                &run_a_id,
                &lineage.world_id,
                &lineage.from_genome_id,
                strategy_a.config.generation_count,
                strategy_a.config.experiment_allocation,
                &strategy_a_id,
            )?;
            self.rollback_champion_to(
                &run_a_id,
                &lineage.world_id,
                &lineage.from_genome_id,
                strategy_a.config.generation_count,
            )?;
            let run_b = self.drive_evolve_run(
                &run_b_id,
                &lineage.world_id,
                &lineage.from_genome_id,
                strategy_b.config.generation_count,
                strategy_b.config.experiment_allocation,
                &strategy_b_id,
            )?;
            self.rollback_champion_to(
                &run_b_id,
                &lineage.world_id,
                &lineage.from_genome_id,
                strategy_b.config.generation_count,
            )?;

            outcomes.push(MetaLineageOutcome {
                world_id: lineage.world_id.clone(),
                from_genome_id: lineage.from_genome_id.clone(),
                strategy_a_run_id: run_a_id,
                strategy_b_run_id: run_b_id,
                strategy_a_champion_genome_id: champion_after(&run_a),
                strategy_b_champion_genome_id: champion_after(&run_b),
                strategy_a_promotions: promotions_of(&run_a),
                strategy_b_promotions: promotions_of(&run_b),
                strategy_a_trials_consumed: run_a.trials_consumed,
                strategy_b_trials_consumed: run_b.trials_consumed,
            });
        }

        let quality_deltas: Vec<i64> = outcomes
            .iter()
            .map(|outcome| {
                i64::from(outcome.strategy_b_promotions) - i64::from(outcome.strategy_a_promotions)
            })
            .collect();
        let cost_deltas: Vec<i64> = outcomes
            .iter()
            .map(|outcome| {
                let a = i64::try_from(outcome.strategy_a_trials_consumed).unwrap_or(i64::MAX);
                let b = i64::try_from(outcome.strategy_b_trials_consumed).unwrap_or(i64::MAX);
                b - a
            })
            .collect();
        let quality_delta = paired_bootstrap(&quality_deltas, bootstrap_seed, confidence_bps)
            .map_err(|_| ExecuteError::Internal)?;
        let cost_delta = paired_bootstrap(&cost_deltas, bootstrap_seed, confidence_bps)
            .map_err(|_| ExecuteError::Internal)?;
        let descendant_cheaper_at_equal_quality = descendant_verdict(
            &strategy_a_id,
            &strategy_a.config,
            &strategy_b_id,
            &strategy_b.config,
            &quality_delta,
            &cost_delta,
        );

        let payload = MetaEvaluationPayload {
            schema_version: 1,
            meta_run_id: meta_run_id.clone(),
            strategy_a_id,
            strategy_b_id,
            confidence_bps,
            bootstrap_seed,
            bootstrap_resamples: u32::try_from(RESAMPLES).map_err(|_| ExecuteError::Internal)?,
            algorithm: BOOTSTRAP_ALGORITHM.to_owned(),
            lineages: outcomes,
            quality_delta,
            cost_delta,
            descendant_cheaper_at_equal_quality,
        };
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                meta_evaluation_event_id(&meta_run_id),
                meta_evaluation_aggregate_id(&meta_run_id),
                META_EVALUATION_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        self.meta_show(&meta_run_id)
    }

    /// Read-only, replay-verified lookup of one meta-evaluation receipt.
    fn meta_show(&self, meta_run_id: &str) -> Result<ResponseData, ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let receipt = meta_evaluation_projection(&history, meta_run_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        Ok(ResponseData::MetaEvaluation {
            receipt: Box::new(receipt),
        })
    }

    /// Recent meta-evaluation receipts, newest first, bounded by `limit`.
    fn meta_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let receipts = meta_evaluation_list(&history, limit).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::MetaEvaluationList { receipts })
    }

    /// Called every `service_async_messages` tick. Advances the one active
    /// evolution run, if any, by exactly one bounded internal step: admitting
    /// or draining a generation's Arena evaluation, or completing a
    /// generation once both evaluations have succeeded. Never blocks: an
    /// in-flight Arena evaluation is left for later ticks to drain.
    fn advance_evolution(&mut self) {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return;
        }
        let Some(storage) = self.storage.as_ref() else {
            return;
        };
        let Ok(history) = storage.ledger.replay_verified() else {
            return;
        };
        let Ok(Some(run_id)) = active_evolution_run_id(&history) else {
            return;
        };
        if self.state.freeze.is_frozen() {
            return;
        }
        let Ok(Some(run)) = evolution_projection(&history, &run_id) else {
            return;
        };
        if run.state == EvolutionRunState::Finished {
            return;
        }
        if run.cancel_requested {
            let _ = self.finish_evolution_run(&run_id, EvolutionFinishReason::Cancelled);
            return;
        }
        let Ok(generation_index) = u32::try_from(run.generations.len()) else {
            return;
        };
        if generation_index >= run.max_generations {
            let _ = self.finish_evolution_run(&run_id, EvolutionFinishReason::GenerationsExhausted);
            return;
        }
        if run.trials_consumed.saturating_add(TRIALS_PER_GENERATION) > run.max_paired_trials {
            let _ = self.finish_evolution_run(&run_id, EvolutionFinishReason::BudgetExhausted);
            return;
        }
        match self.advance_evolution_generation(&run, generation_index) {
            Ok(()) | Err(ExecuteError::Busy) => {}
            Err(_) => {
                let _ = self.finish_evolution_run(&run_id, EvolutionFinishReason::Interrupted);
            }
        }
    }

    /// Drives one generation forward through the same primitives an operator
    /// uses directly: evaluate the Champion as the mutated candidate, select,
    /// propose one child, evaluate the child against the Champion, select,
    /// check invariants, assess, and promote when the deterministic policy
    /// admits it. Every sub-step is idempotent, so re-entering this function
    /// on a later tick (or after a daemon restart) safely resumes exactly
    /// where a prior call left off.
    #[allow(clippy::too_many_lines)]
    fn advance_evolution_generation(
        &mut self,
        run: &EvolutionRunRecord,
        generation_index: u32,
    ) -> Result<(), ExecuteError> {
        let run_id = run.run_id.clone();
        let champion_before = run.generations.last().map_or_else(
            || run.from_genome_id.clone(),
            |generation| generation.payload.champion_after.clone(),
        );

        let diagnostic_id = evolution_diagnostic_evaluation_id(&run_id, generation_index);
        match self
            .state
            .arena_jobs
            .get(&diagnostic_id)
            .and_then(|job| job.terminal)
        {
            None => {
                self.submit_arena_job(
                    &diagnostic_id,
                    &run.baseline_genome_id,
                    &champion_before,
                    false,
                )?;
                return Ok(());
            }
            Some(JobTerminal::Succeeded) => {}
            Some(_) => {
                return Err(ExecuteError::Rejected(
                    "diagnostic evaluation did not succeed".to_owned(),
                ));
            }
        }
        let ResponseData::Selection { selection } = self.select_arena_evaluation(&diagnostic_id)?
        else {
            return Err(ExecuteError::Internal);
        };
        let selection_event_id = selection.event.event_id.clone();

        let Some(source) = self.choose_evolution_hypothesis_source(
            run,
            generation_index,
            &diagnostic_id,
            &champion_before,
        )?
        else {
            // A strategy-bound run whose failure-cluster analysis suggested
            // no mutation, and the Champion isn't the casing pair either
            // (roadmap items 8, 10, 13): stop rather than proposing an
            // unfounded mutation.
            self.finish_evolution_run(&run_id, EvolutionFinishReason::NoCandidateMutation)?;
            return Ok(());
        };
        let proposal_id = evolution_proposal_id(&run_id, generation_index);
        let ResponseData::ForgeProposal { proposal } = self.propose_genome_from_source(
            &proposal_id,
            &selection_event_id,
            &champion_before,
            source,
        )?
        else {
            return Err(ExecuteError::Internal);
        };
        let child_genome_id = proposal.payload.child.genome_id.clone();

        let child_evaluation_id = evolution_child_evaluation_id(&run_id, generation_index);
        match self
            .state
            .arena_jobs
            .get(&child_evaluation_id)
            .and_then(|job| job.terminal)
        {
            None => {
                self.submit_arena_job(
                    &child_evaluation_id,
                    &champion_before,
                    &child_genome_id,
                    false,
                )?;
                return Ok(());
            }
            Some(JobTerminal::Succeeded) => {}
            Some(_) => {
                return Err(ExecuteError::Rejected(
                    "child evaluation did not succeed".to_owned(),
                ));
            }
        }
        let ResponseData::Selection {
            selection: child_selection,
        } = self.select_arena_evaluation(&child_evaluation_id)?
        else {
            return Err(ExecuteError::Internal);
        };
        let child_selection_event_id = child_selection.event.event_id.clone();
        self.check_arena_invariants(&child_evaluation_id)?;

        let assessment_id = evolution_assessment_id(&run_id, generation_index);
        let ResponseData::ForgeAssessment { assessment } =
            self.assess_genome(&assessment_id, &proposal_id, &child_selection_event_id)?
        else {
            return Err(ExecuteError::Internal);
        };

        let mut promoted = false;
        let mut champion_after = champion_before.clone();
        if assessment.payload.outcome == ForgeAssessmentOutcome::MetricsPassed {
            let transition_id = evolution_promotion_transition_id(&run_id, generation_index);
            if self
                .transition_champion(
                    &transition_id,
                    &ChampionRequest::Promote {
                        assessment_id: assessment_id.clone(),
                    },
                )
                .is_ok()
            {
                promoted = true;
                champion_after.clone_from(&child_genome_id);
            }
        }

        self.record_evolution_generation(&EvolutionGenerationPayload {
            schema_version: 1,
            run_id,
            generation_index,
            champion_before,
            diagnostic_evaluation_id: diagnostic_id,
            proposal_id,
            child_genome_id,
            child_evaluation_id,
            assessment_id,
            promoted,
            champion_after,
        })
    }

    /// Chooses this generation's Forge hypothesis source. Without a bound
    /// strategy, this is always the historical default: an operator-authored
    /// hypothesis proposing the `identity`/`ascii_uppercase` flip (unchanged
    /// behavior for every existing evolve run).
    ///
    /// With a bound strategy (roadmap items 8, 10, 13): runs `forge analyze`
    /// on the diagnostic evaluation (the Champion is the analyzed
    /// candidate), orders its failure clusters by the strategy's
    /// `mutation_prioritization` (`Fifo` keeps the clusters' stable
    /// signature order; `CostWeighted` sorts by descending `total_count`,
    /// ties by signature), prefers a Gene Bank suggestion when
    /// `gene_selection == HighestTransferEffect` names one of the available
    /// suggested operations, and binds the proposal to whichever cluster
    /// produced the chosen suggestion. `Ok(None)` means no cluster suggested
    /// a mutation and the Champion is not the casing pair either, so the
    /// caller finishes the run with `NoCandidateMutation` instead of
    /// proposing anything.
    ///
    /// `candidate_count` above `1` and multi-candidate trial accounting are
    /// not implemented: exactly one candidate is ever proposed per
    /// generation (see `TECH_DEBT.md`).
    fn choose_evolution_hypothesis_source(
        &mut self,
        run: &EvolutionRunRecord,
        generation_index: u32,
        diagnostic_id: &str,
        champion_before: &str,
    ) -> Result<Option<ForgeHypothesisSource>, ExecuteError> {
        let run_id = run.run_id.clone();
        let default_hypothesis = || {
            ForgeHypothesisSource::Operator(format!(
                "Evolve run {run_id} generation {generation_index}: flip the reference \
                 operation of Champion {champion_before} to explore the paired instruction \
                 space."
            ))
        };
        let Some(strategy_id) = run.strategy_id.clone() else {
            return Ok(Some(default_hypothesis()));
        };
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let strategy = meta_strategy_projection(&history, &strategy_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::Internal)?;

        let analysis_id = evolution_analysis_id(&run.run_id, generation_index);
        let ResponseData::ForgeAnalysis { analysis } =
            self.analyze_forge_clusters(&analysis_id, diagnostic_id)?
        else {
            return Err(ExecuteError::Internal);
        };

        let mut clusters: Vec<(u32, &FailureCluster)> = analysis
            .analysis
            .clusters
            .iter()
            .enumerate()
            .filter_map(|(index, cluster)| Some((u32::try_from(index).ok()?, cluster)))
            .collect();
        if strategy.config.mutation_prioritization == MutationPrioritization::CostWeighted {
            clusters.sort_by(|(_, left), (_, right)| {
                right
                    .total_count
                    .cmp(&left.total_count)
                    .then_with(|| left.signature.cmp(&right.signature))
            });
        }
        // `Fifo` keeps the clusters' already-stable signature order.

        let suggestion_of = |cluster: &FailureCluster| match &cluster.suggested_mutation {
            Some(SuggestedMutation::ReferenceOperation { operation_after }) => {
                Some(operation_after.clone())
            }
            _ => None,
        };

        let champion_operation = self
            .reference_instruction(champion_before)?
            .map(ReferenceInstruction::operation_name);

        let chosen_index =
            if strategy.config.gene_selection == GeneSelectionPolicy::HighestTransferEffect {
                let preferred = champion_operation
                    .and_then(|operation| best_gene_target_operation(&history, operation));
                preferred
                    .as_deref()
                    .and_then(|preferred_op| {
                        clusters
                            .iter()
                            .find(|(_, cluster)| {
                                suggestion_of(cluster).as_deref() == Some(preferred_op)
                            })
                            .map(|(index, _)| *index)
                    })
                    .or_else(|| {
                        clusters
                            .iter()
                            .find(|(_, cluster)| suggestion_of(cluster).is_some())
                            .map(|(index, _)| *index)
                    })
            } else {
                clusters
                    .iter()
                    .find(|(_, cluster)| suggestion_of(cluster).is_some())
                    .map(|(index, _)| *index)
            };

        if let Some(cluster_index) = chosen_index {
            return Ok(Some(ForgeHypothesisSource::Analysis {
                analysis_id,
                cluster_index,
            }));
        }

        // No cluster suggested anything: fall back to today's casing flip
        // when the Champion runs one of the two casing operations, exactly
        // like a strategy-less run would.
        if matches!(champion_operation, Some("identity" | "ascii_uppercase")) {
            return Ok(Some(default_hypothesis()));
        }
        Ok(None)
    }

    fn record_evolution_generation(
        &mut self,
        payload: &EvolutionGenerationPayload,
    ) -> Result<(), ExecuteError> {
        let event_id = evolution_generation_event_id(&payload.run_id, payload.generation_index);
        let aggregate_id = evolution_aggregate_id(&payload.run_id);
        let payload_value = serde_json::to_value(payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                event_id,
                aggregate_id,
                EVOLUTION_GENERATION_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()
    }

    fn finish_evolution_run(
        &mut self,
        run_id: &str,
        reason: EvolutionFinishReason,
    ) -> Result<(), ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let run = evolution_projection(&history, run_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::Internal)?;
        if run.state == EvolutionRunState::Finished {
            return Ok(());
        }
        let generations_completed =
            u32::try_from(run.generations.len()).map_err(|_| ExecuteError::Internal)?;
        let payload = EvolutionFinishedPayload {
            schema_version: 1,
            run_id: run_id.to_owned(),
            generations_completed,
            trials_consumed: run.trials_consumed,
            reason,
        };
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                evolution_finished_event_id(run_id),
                evolution_aggregate_id(run_id),
                EVOLUTION_FINISHED_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()
    }

    /// Drives the automatic drift-to-canary adaptation pipeline (roadmap
    /// item 12) forward by exactly one step, resuming from durable history
    /// on every tick or restart, exactly like `advance_evolution`. Never
    /// called from a client connection. A World only ever contributes a
    /// drift here when its Law `auto_canary_on_drift` opted in.
    fn advance_drift_adaptations(&mut self) {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return;
        }
        let Some(storage) = self.storage.as_ref() else {
            return;
        };
        let Ok(history) = storage.ledger.replay_verified() else {
            return;
        };
        let Some(drift_id) = Self::next_drift_needing_adaptation(&history, &self.state.registered)
        else {
            return;
        };
        // Freeze halts advancement; it never clears an in-flight adaptation,
        // exactly like `evolve` and canary staged advancement.
        if self.state.freeze.is_frozen() {
            return;
        }
        match self.advance_one_drift_adaptation(&drift_id) {
            Ok(()) | Err(ExecuteError::Busy) => {}
            Err(_) => {
                let _ = self.finish_drift_adaptation(
                    &drift_id,
                    DriftAdaptationFinishReason::Interrupted,
                    None,
                );
            }
        }
    }

    /// The oldest `drift.recorded` event, in a World whose Law opted in, that
    /// has no `drift.adaptation_finished` event yet (whether or not it has
    /// started: an in-progress adaptation is picked again so it keeps moving).
    fn next_drift_needing_adaptation(
        history: &[StoredEvent],
        registered: &RegisteredObjects,
    ) -> Option<String> {
        let mut finished: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for event in history {
            if event.event_type == adaptation::DRIFT_ADAPTATION_FINISHED_TYPE {
                if let Ok(payload) = adaptation::decode_finished(event) {
                    finished.insert(payload.drift_id);
                }
            }
        }
        for event in history {
            if event.event_type != DRIFT_EVENT_TYPE {
                continue;
            }
            let Ok(payload) = decode_drift_record(event) else {
                continue;
            };
            if finished.contains(&payload.drift_id) {
                continue;
            }
            let Some(world) = registered.world(&payload.world_id) else {
                continue;
            };
            if world.compiled().evaluation_policy().auto_canary_on_drift() {
                return Some(payload.drift_id);
            }
        }
        None
    }

    /// Chooses the mutation Forge proposes for a drift-triggered adaptation
    /// of the Champion. Exists as its own function so a later Forge mutation
    /// catalog (selecting a mutation from failure clusters and the World's
    /// mutation scope) can be substituted here without changing the
    /// adaptation pipeline's control flow; today it is the same
    /// `identity`/`ascii_uppercase` flip every other proposal path (`evolve`,
    /// direct `genome propose`) performs. Read-only: never mutates anything.
    fn choose_adaptation_mutation(
        &self,
        champion_genome_id: &str,
        world_id: &str,
    ) -> Result<bool, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let world = self
            .state
            .registered
            .world(world_id)
            .ok_or(ExecuteError::Internal)?;
        match forge_prompt_mutation(
            &storage.artifacts,
            &self.state.registered,
            champion_genome_id,
            world.compiled(),
            None,
        ) {
            Ok(_) => Ok(true),
            Err(ExecuteError::Rejected(_) | ExecuteError::NotFound) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// Advances one drift's adaptation by exactly one durable step: submits
    /// at most one fresh Arena job (returning `Ok(())` to wait for its
    /// completion on a later tick), or appends at most one new event. Every
    /// gate re-derives from `history`, so a daemon restart mid-pipeline
    /// resumes idempotently.
    #[allow(clippy::too_many_lines)]
    fn advance_one_drift_adaptation(&mut self, drift_id: &str) -> Result<(), ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        verify_drift_adaptation_history(&history).map_err(|_| ExecuteError::Internal)?;

        let drift = drift::drift_projection(&history, drift_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::Internal)?;
        let projection =
            adaptation_projection(&history, drift_id).map_err(|_| ExecuteError::Internal)?;

        let Some(started) = projection.started.clone() else {
            let champion = champion_projection(&history, &drift.payload.world_id)
                .map_err(|_| ExecuteError::Internal)?;
            let champion_genome_id = champion.champion_genome_id.ok_or(ExecuteError::Internal)?;
            let payload = DriftAdaptationStartedPayload {
                schema_version: 1,
                drift_id: drift_id.to_owned(),
                world_id: drift.payload.world_id.clone(),
                drift_event_id: drift.event.event_id.clone(),
                drift_event_hash: drift.event.event_hash.clone(),
                champion_genome_id,
                proposal_id: adaptation_proposal_id(drift_id),
            };
            let timestamp = timestamp_millis().map_err(|_| ExecuteError::Internal)?;
            let event = started_event_input(&payload, timestamp)?;
            self.storage
                .as_mut()
                .ok_or(ExecuteError::Internal)?
                .ledger
                .append(event)
                .map_err(|_| ExecuteError::Internal)?;
            return self.refresh_projection();
        };

        if projection.finished.is_some() {
            return Ok(());
        }

        if !self.choose_adaptation_mutation(&started.champion_genome_id, &started.world_id)? {
            return self.finish_drift_adaptation(
                drift_id,
                DriftAdaptationFinishReason::NoCandidateMutation,
                None,
            );
        }

        // Diagnostic evaluation: establishes the Champion as the verified
        // selected candidate `propose_genome_from_source` requires, exactly
        // the role `evolve`'s own diagnostic evaluation plays. Paired
        // against this drift's own shifted Genome so nothing new needs
        // registering.
        let diagnostic_id = adaptation_diagnostic_evaluation_id(drift_id);
        let selection_event_id = match self
            .state
            .arena_jobs
            .get(&diagnostic_id)
            .and_then(|job| job.terminal)
        {
            None => {
                self.submit_arena_job(
                    &diagnostic_id,
                    &drift.payload.shifted_genome_id,
                    &started.champion_genome_id,
                    false,
                )?;
                return Ok(());
            }
            Some(JobTerminal::Succeeded) => {
                let ResponseData::Selection { selection } =
                    self.select_arena_evaluation(&diagnostic_id)?
                else {
                    return Err(ExecuteError::Internal);
                };
                selection.event.event_id.clone()
            }
            Some(_) => {
                return Err(ExecuteError::Rejected(
                    "diagnostic evaluation did not succeed".to_owned(),
                ));
            }
        };

        // Forge proposal of the current Champion (the chosen adaptation
        // branch), through the ordinary catalog.
        let proposal_event = history
            .iter()
            .find(|event| event.event_id == forge_event_id(&started.proposal_id));
        let child_genome_id = if let Some(event) = proposal_event {
            decode_forge_proposal(event)
                .map_err(|_| ExecuteError::Internal)?
                .child
                .genome_id
        } else {
            let hypothesis = format!(
                "Drift adaptation for drift {drift_id} ({:?}): flip the reference operation of \
                 Champion {} to address the recorded drift.",
                drift.payload.kind, started.champion_genome_id
            );
            let ResponseData::ForgeProposal { proposal } = self.propose_genome_from_source(
                &started.proposal_id,
                &selection_event_id,
                &started.champion_genome_id,
                ForgeHypothesisSource::Operator(hypothesis),
            )?
            else {
                return Err(ExecuteError::Internal);
            };
            proposal.payload.child.genome_id.clone()
        };

        // Shadow evaluation: Champion versus the proposed child. This is the
        // canary's shadow evaluation, exactly like a direct `canary start`.
        let shadow_id = adaptation_shadow_evaluation_id(drift_id);
        let shadow_selection_event_id = match self
            .state
            .arena_jobs
            .get(&shadow_id)
            .and_then(|job| job.terminal)
        {
            None => {
                self.submit_arena_job(
                    &shadow_id,
                    &started.champion_genome_id,
                    &child_genome_id,
                    false,
                )?;
                return Ok(());
            }
            Some(JobTerminal::Succeeded) => {
                let ResponseData::Selection { selection } =
                    self.select_arena_evaluation(&shadow_id)?
                else {
                    return Err(ExecuteError::Internal);
                };
                self.check_arena_invariants(&shadow_id)?;
                selection.event.event_id.clone()
            }
            Some(_) => {
                return Err(ExecuteError::Rejected(
                    "shadow evaluation did not succeed".to_owned(),
                ));
            }
        };

        // Evidence-only Forge assessment; its outcome does not gate whether
        // the canary starts, exactly like a direct `canary start`.
        let assessment_id = adaptation_assessment_id(drift_id);
        let assessment_exists = history
            .iter()
            .any(|event| event.event_id == forge_assessment_event_id(&assessment_id));
        if !assessment_exists {
            self.assess_genome(
                &assessment_id,
                &started.proposal_id,
                &shadow_selection_event_id,
            )?;
            return Ok(());
        }

        // Start (or resume) the canary through the existing transition
        // policy; never reimplemented here.
        let canary_id = adaptation_canary_id(drift_id);
        let canary =
            canary::canary_projection(&history, &canary_id).map_err(|_| ExecuteError::Internal)?;
        let Some(canary) = canary else {
            self.transition_canary(
                &canary_id,
                &CanaryRequest::Start {
                    world_id: started.world_id.clone(),
                    candidate_genome_id: child_genome_id.clone(),
                    assessment_id: assessment_id.clone(),
                },
            )?;
            return Ok(());
        };

        match canary.stage {
            CanaryStage::Aborted => {
                return self.finish_drift_adaptation(
                    drift_id,
                    DriftAdaptationFinishReason::CanaryAborted,
                    Some(&canary_id),
                );
            }
            CanaryStage::Completed => {
                return self.finish_drift_adaptation(
                    drift_id,
                    DriftAdaptationFinishReason::Promoted,
                    Some(&canary_id),
                );
            }
            CanaryStage::Pending
            | CanaryStage::Stage5
            | CanaryStage::Stage25
            | CanaryStage::Stage50 => {}
        }

        // One fresh paired evaluation per staged advance, mirroring exactly
        // what a direct `canary advance` needs as its evidence.
        let stage_index = u32::try_from(
            canary
                .transitions
                .iter()
                .filter(|transition| {
                    matches!(
                        transition.payload.kind,
                        CanaryTransitionKind::Advanced | CanaryTransitionKind::Aborted
                    )
                })
                .count(),
        )
        .map_err(|_| ExecuteError::Internal)?;
        let stage_eval_id = adaptation_stage_evaluation_id(drift_id, stage_index);
        match self
            .state
            .arena_jobs
            .get(&stage_eval_id)
            .and_then(|job| job.terminal)
        {
            None => {
                self.submit_arena_job(
                    &stage_eval_id,
                    &started.champion_genome_id,
                    &child_genome_id,
                    false,
                )?;
                Ok(())
            }
            Some(JobTerminal::Succeeded) => {
                let ResponseData::Selection { selection: _ } =
                    self.select_arena_evaluation(&stage_eval_id)?
                else {
                    return Err(ExecuteError::Internal);
                };
                self.transition_canary(
                    &canary_id,
                    &CanaryRequest::Advance {
                        evidence_evaluation_id: stage_eval_id.clone(),
                    },
                )?;
                Ok(())
            }
            Some(_) => Err(ExecuteError::Rejected(
                "stage evaluation did not succeed".to_owned(),
            )),
        }
    }

    /// Appends the terminal `drift.adaptation_finished` event for one drift,
    /// cross-referencing whichever durable arena/selection/forge/canary
    /// events this adaptation already produced.
    fn finish_drift_adaptation(
        &mut self,
        drift_id: &str,
        reason: DriftAdaptationFinishReason,
        canary_id: Option<&str>,
    ) -> Result<(), ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let projection =
            adaptation_projection(&history, drift_id).map_err(|_| ExecuteError::Internal)?;
        let started = projection.started.ok_or(ExecuteError::Internal)?;

        let has_proposal = reason != DriftAdaptationFinishReason::NoCandidateMutation;
        let child_genome_id = if has_proposal {
            history
                .iter()
                .find(|event| event.event_id == forge_event_id(&started.proposal_id))
                .and_then(|event| decode_forge_proposal(event).ok())
                .map(|proposal| proposal.child.genome_id)
        } else {
            None
        };
        let shadow_evaluation_id = has_proposal.then(|| adaptation_shadow_evaluation_id(drift_id));
        let assessment_id = has_proposal.then(|| adaptation_assessment_id(drift_id));

        let (final_canary_stage, promotion_transition_id) = match canary_id {
            Some(canary_id) => {
                let canary = canary::canary_projection(&history, canary_id)
                    .map_err(|_| ExecuteError::Internal)?
                    .ok_or(ExecuteError::Internal)?;
                let promotion_transition_id = (reason == DriftAdaptationFinishReason::Promoted)
                    .then(|| canary::canary_id_promotion_transition_id(canary_id));
                (Some(canary.stage), promotion_transition_id)
            }
            None => (None, None),
        };

        let payload = DriftAdaptationFinishedPayload {
            schema_version: 1,
            drift_id: drift_id.to_owned(),
            world_id: started.world_id.clone(),
            reason,
            proposal_id: has_proposal.then(|| started.proposal_id.clone()),
            child_genome_id,
            shadow_evaluation_id,
            assessment_id,
            canary_id: canary_id.map(str::to_owned),
            final_canary_stage,
            promotion_transition_id,
        };
        let timestamp = timestamp_millis().map_err(|_| ExecuteError::Internal)?;
        let event = finished_event_input(&payload, timestamp)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(event)
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()
    }

    /// Recent direct runs and jobs, newest first, derived from `state.jobs` and verified
    /// `run.result_recorded` history. Bounded and read-only; never storage-taking.
    fn run_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let limit = limit.min(MAX_LIST_LIMIT) as usize;
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;

        let mut jobs: BTreeMap<String, (u64, JobRecord)> = BTreeMap::new();
        let mut results: BTreeMap<String, (u64, RunResultReceipt)> = BTreeMap::new();
        for event in &history {
            match event.event_type.as_str() {
                "job.admitted" | "job.running" | "job.cancellation_requested" | "job.terminal" => {
                    if let Ok(record) = serde_json::from_slice::<JobRecord>(&event.payload) {
                        jobs.insert(record.job_id.clone(), (event.sequence, record));
                    }
                }
                "run.result_recorded" => {
                    if let Ok(receipt) =
                        RunResultReceipt::parse_from_event(event, &self.run_result_verifier)
                    {
                        results.insert(receipt.run_id.clone(), (event.sequence, receipt));
                    }
                }
                _ => {}
            }
        }

        let mut entries: Vec<(u64, RunListEntry)> = Vec::new();
        let mut consumed_run_ids: BTreeSet<String> = BTreeSet::new();
        for (job_id, (job_sequence, job)) in &jobs {
            consumed_run_ids.insert(job.run_id.clone());
            let result = results.get(&job.run_id);
            let sequence =
                result.map_or(*job_sequence, |(sequence, _)| *sequence.max(job_sequence));
            entries.push((
                sequence,
                RunListEntry {
                    run_id: job.run_id.clone(),
                    job_id: Some(job_id.clone()),
                    genome_id: job.genome_id.clone(),
                    world_id: Some(job.world_id.clone()),
                    state: job.state,
                    completion_reason: result.map(|(_, receipt)| receipt.completion_reason),
                    latency_millis: result.map(|(_, receipt)| receipt.latency_millis),
                    actual_cost_microusd: result.map(|(_, receipt)| receipt.actual_cost_microusd),
                },
            ));
        }
        for (run_id, (sequence, receipt)) in &results {
            if consumed_run_ids.contains(run_id) {
                continue;
            }
            let state = match receipt.completion_reason {
                RunCompletionReason::Success => JobState::Succeeded,
                RunCompletionReason::OperatorInterrupt => JobState::Interrupted,
                _ => JobState::Failed,
            };
            entries.push((
                *sequence,
                RunListEntry {
                    run_id: run_id.clone(),
                    job_id: None,
                    genome_id: receipt.genome_id.clone(),
                    world_id: Some(receipt.world_id.clone()),
                    state,
                    completion_reason: Some(receipt.completion_reason),
                    latency_millis: Some(receipt.latency_millis),
                    actual_cost_microusd: Some(receipt.actual_cost_microusd),
                },
            ));
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        entries.truncate(limit);
        Ok(ResponseData::RunList {
            runs: entries.into_iter().map(|(_, entry)| entry).collect(),
        })
    }

    /// Recent Arena evaluations, newest first, with visible aggregates and evidence
    /// references. Never exposes sealed task identities, inputs, or raw outputs.
    fn evaluation_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let limit = limit.min(MAX_LIST_LIMIT) as usize;
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;

        let mut evaluation_ids: Vec<(u64, String)> = self
            .state
            .evaluation_events
            .iter()
            .map(|(evaluation_id, sequence)| (*sequence, evaluation_id.clone()))
            .collect();
        evaluation_ids.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        evaluation_ids.truncate(limit);

        let mut entries = Vec::with_capacity(evaluation_ids.len());
        for (_, evaluation_id) in evaluation_ids {
            let operator = load_operator_evaluation(self.open_arena_stores()?, &evaluation_id)
                .map_err(|_| ExecuteError::Internal)?;
            let evaluation = evaluation_record_from_operator(&operator);
            drop(operator.into_stores());

            let selection = self.evaluation_selection_summary(&history, &evaluation_id)?;
            let invariants = self.evaluation_invariant_summary(&history, &evaluation_id)?;
            let forge_assessment = forge_assessment_summary(&history, &evaluation_id);
            let champion_transition_ids = champion_transition_ids_for(&history, &evaluation_id);

            entries.push(EvaluationListEntry {
                evaluation,
                selection,
                invariants,
                forge_assessment,
                champion_transition_ids,
            });
        }
        Ok(ResponseData::EvaluationList {
            evaluations: entries,
        })
    }

    fn evaluation_selection_summary(
        &self,
        history: &[StoredEvent],
        evaluation_id: &str,
    ) -> Result<Option<EvaluationSelectionSummary>, ExecuteError> {
        for event in history
            .iter()
            .filter(|event| event.event_type == "selection.recorded")
        {
            // History was verified before this read; a failure here is internal.
            let (event_evaluation_id, world_id) =
                selection_event_references(event).map_err(|_| ExecuteError::Internal)?;
            if event_evaluation_id != evaluation_id {
                continue;
            }
            let world = self
                .state
                .registered
                .world(&world_id)
                .ok_or(ExecuteError::Internal)?;
            let verified =
                verify_selection_event(self.open_arena_stores()?, event, world.compiled())
                    .map_err(|_| ExecuteError::Internal)?;
            let receipt = verified.receipt().clone();
            drop(verified.into_stores());
            return Ok(Some(EvaluationSelectionSummary {
                metrics_eligible: receipt.metrics_eligible(),
                estimate_bps: receipt.estimate_bps(),
                lower_bps: receipt.lower_bps(),
                upper_bps: receipt.upper_bps(),
                parent_cost_microusd: receipt.parent_cost_microusd(),
                candidate_cost_microusd: receipt.candidate_cost_microusd(),
                parent_latency_millis: receipt.parent_latency_millis(),
                candidate_latency_millis: receipt.candidate_latency_millis(),
                invariant_gate_verified: receipt.invariant_gate_verified(),
                promotion_eligible: receipt.promotion_eligible(),
            }));
        }
        Ok(None)
    }

    fn evaluation_invariant_summary(
        &self,
        history: &[StoredEvent],
        evaluation_id: &str,
    ) -> Result<Option<EvaluationInvariantSummary>, ExecuteError> {
        for event in history
            .iter()
            .filter(|event| event.event_type == "invariants.recorded")
        {
            // History was verified before this read; a failure here is internal.
            let (event_evaluation_id, world_id) =
                invariant_event_references(event).map_err(|_| ExecuteError::Internal)?;
            if event_evaluation_id != evaluation_id {
                continue;
            }
            let world = self
                .state
                .registered
                .world(&world_id)
                .ok_or(ExecuteError::Internal)?;
            let verified = verify_reference_output_invariant_event(
                self.open_arena_stores()?,
                event,
                world.compiled(),
            )
            .map_err(|_| ExecuteError::Internal)?;
            let receipt = verified.receipt().clone();
            drop(verified.into_stores());
            return Ok(Some(EvaluationInvariantSummary {
                total_checks: receipt.total_checks,
                total_candidate_violations: receipt.total_candidate_violations,
                total_paired_regressions: receipt.total_paired_regressions,
                maximum_regressions: receipt.maximum_regressions,
                regressions_within_budget: receipt.regressions_within_budget,
                candidate_contract_satisfied: receipt.candidate_contract_satisfied,
            }));
        }
        Ok(None)
    }

    /// Recent refused operator requests and recorded runtime authority denials, newest
    /// first. Only denials that are actually ledgered are listed:
    /// `control.request_rejected` audit events (empty `request_id`), and
    /// `TraceKind::CapabilityDenied` runtime traces. Other `ExecuteError::Rejected`
    /// outcomes are returned to the caller but are not separately ledgered as denials.
    fn denial_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let limit = limit.min(MAX_LIST_LIMIT) as usize;
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;

        let mut entries: Vec<(u64, DenialEntry)> = Vec::new();
        for event in &history {
            if event.event_type == "control.request_rejected" {
                if let Ok(recorded) = serde_json::from_slice::<RecordedCommand>(&event.payload) {
                    let command = event_type(&recorded.command)
                        .strip_prefix("control.")
                        .unwrap_or("unknown")
                        .to_owned();
                    entries.push((
                        event.sequence,
                        DenialEntry {
                            kind: DenialKind::RequestRejected,
                            timestamp_millis: event.timestamp_millis,
                            request_id: Some(recorded.request_id),
                            command: Some(command),
                            run_id: None,
                            genome_id: None,
                            world_id: None,
                            client_id: None,
                        },
                    ));
                }
            } else if event.event_type == "trace.recorded"
                && let Ok(receipt) = serde_json::from_slice::<TraceReceipt>(&event.payload)
                && matches!(receipt.kind, TraceKind::CapabilityDenied)
            {
                entries.push((
                    event.sequence,
                    DenialEntry {
                        kind: DenialKind::RuntimeCapabilityDenied,
                        timestamp_millis: event.timestamp_millis,
                        request_id: None,
                        command: None,
                        run_id: Some(receipt.provenance.run_id().to_owned()),
                        genome_id: Some(receipt.provenance.genome_id().to_owned()),
                        world_id: Some(receipt.provenance.world_id().to_owned()),
                        client_id: None,
                    },
                ));
            } else if event.event_type == "mcp.call"
                && let Ok(recorded) = serde_json::from_slice::<RecordedCommand>(&event.payload)
                && let Command::McpCall {
                    client_id,
                    tool,
                    decision: McpDecision::Denied { .. },
                    ..
                } = &recorded.command
            {
                entries.push((
                    event.sequence,
                    DenialEntry {
                        kind: DenialKind::McpCallDenied,
                        timestamp_millis: event.timestamp_millis,
                        request_id: Some(recorded.request_id.clone()),
                        command: Some(tool.clone()),
                        run_id: None,
                        genome_id: None,
                        world_id: None,
                        client_id: Some(client_id.clone()),
                    },
                ));
            }
        }
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        entries.truncate(limit);
        Ok(ResponseData::DenialList {
            denials: entries.into_iter().map(|(_, entry)| entry).collect(),
        })
    }

    fn request_daemon_stop(&mut self) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            self.request_active_job_cancellation()?;
            return Err(ExecuteError::Busy);
        }
        self.shutdown_requested = true;
        Ok(ResponseData::Acknowledged {
            frozen: self.state.freeze.is_frozen(),
            killed_runs: 0,
        })
    }

    fn worker_credential_mint(
        &mut self,
        worker_id: &str,
        ttl_seconds: u64,
    ) -> Result<ResponseData, ExecuteError> {
        let mut secret = [0_u8; 32];
        File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut secret))
            .map_err(|_| ExecuteError::Internal)?;
        let token = hex_encode(&secret);
        let credential_id = blake3::hash(&secret).to_hex()[..32].to_owned();
        let now = timestamp_millis().map_err(|_| ExecuteError::Internal)?;
        let ttl_millis = i64::try_from(ttl_seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1000))
            .ok_or(ExecuteError::Internal)?;
        let expires_at_millis = now.checked_add(ttl_millis).ok_or(ExecuteError::Internal)?;
        let record = WorkerCredentialRecord {
            schema_version: 1,
            credential_id: credential_id.clone(),
            worker_id: worker_id.to_owned(),
            scope: WorkerScope::RemoteReferenceRun,
            expires_at_millis,
            revoked: false,
        };
        let payload = serde_json::to_vec(&record).map_err(|_| ExecuteError::Internal)?;
        let event = self
            .storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                format!("worker-credential:{credential_id}"),
                format!("worker-credential:{credential_id}"),
                "worker.credential_minted",
                OPERATOR_ACTOR,
                now,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.state
            .apply(&event, &self.operator_token, &self.run_result_verifier)
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::WorkerCredential {
            credential_id,
            token,
            worker_id: worker_id.to_owned(),
            expires_at_millis,
            scope: WorkerScope::RemoteReferenceRun,
        })
    }

    fn worker_credential_revoke(
        &mut self,
        credential_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        let existing = self
            .state
            .worker_credentials
            .get(credential_id)
            .ok_or(ExecuteError::NotFound)?;
        if existing.revoked {
            return Ok(ResponseData::Acknowledged {
                frozen: self.state.freeze.is_frozen(),
                killed_runs: 0,
            });
        }
        let payload = serde_json::to_vec(&serde_json::json!({ "credential_id": credential_id }))
            .map_err(|_| ExecuteError::Internal)?;
        let now = timestamp_millis().map_err(|_| ExecuteError::Internal)?;
        let event = self
            .storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                format!("worker-credential-revoke:{credential_id}"),
                format!("worker-credential:{credential_id}"),
                "worker.credential_revoked",
                OPERATOR_ACTOR,
                now,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.state
            .apply(&event, &self.operator_token, &self.run_result_verifier)
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Acknowledged {
            frozen: self.state.freeze.is_frozen(),
            killed_runs: 0,
        })
    }

    fn remote_run_submit(
        &mut self,
        job_id: &str,
        genome_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if let Some(existing) = self.state.remote_jobs.get(job_id) {
            if existing.genome_id != genome_id {
                return Err(ExecuteError::Rejected(
                    "job id is already bound to another Genome".to_owned(),
                ));
            }
            return self.remote_job_status(job_id);
        }
        let genome = self.runnable_genome(genome_id)?;
        self.reference_instruction(genome_id)?.ok_or_else(|| {
            ExecuteError::Rejected("Genome has no reference instruction".to_owned())
        })?;
        let run_id = remote_job_run_id(job_id);
        let record = RemoteJobRecord {
            schema_version: 1,
            job_id: job_id.to_owned(),
            genome_id: genome.genome_id.clone(),
            run_id,
        };
        let payload = serde_json::to_vec(&record).map_err(|_| ExecuteError::Internal)?;
        let now = timestamp_millis().map_err(|_| ExecuteError::Internal)?;
        let event = self
            .storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                format!("remote-job-admit:{job_id}"),
                format!("remote-job:{job_id}"),
                "remote_worker.job_admitted",
                OPERATOR_ACTOR,
                now,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.state
            .apply(&event, &self.operator_token, &self.run_result_verifier)
            .map_err(|_| ExecuteError::Internal)?;
        self.remote_job_status(job_id)
    }

    fn remote_job_status(&self, job_id: &str) -> Result<ResponseData, ExecuteError> {
        let record = self
            .state
            .remote_jobs
            .get(job_id)
            .ok_or(ExecuteError::NotFound)?;
        if let Some(result) = self.state.run_results.get(&record.run_id) {
            let state = if matches!(result.completion_reason, RunCompletionReason::Success) {
                RemoteJobState::Succeeded
            } else {
                RemoteJobState::Failed
            };
            return Ok(ResponseData::RemoteJob {
                job_id: record.job_id.clone(),
                genome_id: record.genome_id.clone(),
                state,
                completion_reason: Some(result.completion_reason),
                latency_millis: Some(result.latency_millis),
                stdout_artifact_id: Some(result.stdout_artifact_id.clone()),
            });
        }
        Ok(ResponseData::RemoteJob {
            job_id: record.job_id.clone(),
            genome_id: record.genome_id.clone(),
            state: RemoteJobState::Pending,
            completion_reason: None,
            latency_millis: None,
            stdout_artifact_id: None,
        })
    }

    /// Handles one authenticated message from a remote worker connecting
    /// over the dedicated `worker.sock`. Runs on the same single-writer
    /// thread as `handle`, so a lease and a result never race a concurrent
    /// operator command.
    pub(crate) fn handle_worker_request(&mut self, request: WorkerRequest) -> WorkerReply {
        match request {
            WorkerRequest::Lease { worker_id, token } => {
                if let Err(reason) = self.verify_worker_credential(&worker_id, &token) {
                    return WorkerReply::Error {
                        reason: reason.to_owned(),
                    };
                }
                self.lease_remote_job()
            }
            WorkerRequest::SubmitResult {
                worker_id,
                token,
                job_id,
                output_hex,
                completion,
            } => {
                if let Err(reason) = self.verify_worker_credential(&worker_id, &token) {
                    return WorkerReply::Error {
                        reason: reason.to_owned(),
                    };
                }
                match self.record_remote_job_result(&job_id, &output_hex, completion) {
                    Ok(()) => WorkerReply::ResultAccepted { job_id },
                    Err(reason) => WorkerReply::Error { reason },
                }
            }
        }
    }

    fn verify_worker_credential(&self, worker_id: &str, token: &str) -> Result<(), &'static str> {
        let secret = hex_decode(token).map_err(|_| "credential token is malformed")?;
        let credential_id = blake3::hash(&secret).to_hex()[..32].to_owned();
        let record = self
            .state
            .worker_credentials
            .get(&credential_id)
            .ok_or("credential is not recognized")?;
        if record.revoked {
            return Err("credential has been revoked");
        }
        if record.worker_id != worker_id {
            return Err("credential does not match worker_id");
        }
        let now = timestamp_millis().map_err(|_| "clock error")?;
        if now >= record.expires_at_millis {
            return Err("credential has expired");
        }
        Ok(())
    }

    fn lease_remote_job(&mut self) -> WorkerReply {
        let now = Instant::now();
        self.remote_leases
            .retain(|_, leased_at| now.duration_since(*leased_at) < REMOTE_LEASE_TIMEOUT);
        let direct_run_job = self
            .state
            .remote_jobs
            .iter()
            .find(|(job_id, record)| {
                !self.state.run_results.contains_key(&record.run_id)
                    && !self.remote_leases.contains_key(job_id.as_str())
            })
            .map(|(job_id, record)| (job_id.clone(), record.clone()));
        if let Some((job_id, record)) = direct_run_job {
            let Ok(Some(instruction)) = self.reference_instruction(&record.genome_id) else {
                return WorkerReply::NoWork;
            };
            let Ok(frame) = hephaestus_runtime::frame_reference_instruction(
                instruction,
                REMOTE_REFERENCE_PROMPT.as_bytes(),
            ) else {
                return WorkerReply::NoWork;
            };
            self.remote_leases.insert(job_id.clone(), now);
            return WorkerReply::Leased {
                job_id,
                genome_id: record.genome_id,
                frame_hex: hex_encode_bytes(&frame),
            };
        }
        // No direct-run job is pending; offer a leasable Arena reference
        // trial instead (TD-12). The exact same wire reply shape, so the
        // remote worker binary needs no change either way.
        if let Some((job_id, genome_id, frame)) = self.remote_arena_lease.lease() {
            return WorkerReply::Leased {
                job_id,
                genome_id,
                frame_hex: hex_encode_bytes(&frame),
            };
        }
        WorkerReply::NoWork
    }

    #[allow(clippy::too_many_lines)]
    fn record_remote_job_result(
        &mut self,
        job_id: &str,
        output_hex: &str,
        completion: RemoteCompletion,
    ) -> Result<(), String> {
        if !self.state.remote_jobs.contains_key(job_id) && self.remote_arena_lease.owns(job_id) {
            let output =
                hex_decode_bytes(output_hex).map_err(|_| "output is not valid hex".to_owned())?;
            if output.len() > 1_048_576 {
                return Err("output exceeds the byte limit".to_owned());
            }
            return self
                .remote_arena_lease
                .submit_result(job_id, output, completion)
                .map_err(ToOwned::to_owned);
        }
        let record = self
            .state
            .remote_jobs
            .get(job_id)
            .cloned()
            .ok_or_else(|| "job_id is not recognized".to_owned())?;
        if self.state.run_results.contains_key(&record.run_id) {
            self.remote_leases.remove(job_id);
            return Ok(());
        }
        let output =
            hex_decode_bytes(output_hex).map_err(|_| "output is not valid hex".to_owned())?;
        if output.len() > 1_048_576 {
            return Err("output exceeds the byte limit".to_owned());
        }
        let genome = self
            .state
            .registered
            .genome(&record.genome_id)
            .map(|genome| genome.record().clone())
            .ok_or_else(|| "Genome is no longer registered".to_owned())?;
        let source_revision = resolve_source_revision(&self.source_repository)
            .map_err(|_| "source revision could not be resolved".to_owned())?;
        let leased_millis = self
            .remote_leases
            .get(job_id)
            .map_or(0, |leased_at| leased_at.elapsed().as_millis());
        let latency_millis = u64::try_from(leased_millis).unwrap_or(u64::MAX);
        let now = timestamp_millis().map_err(|_| "clock error".to_owned())?;
        let (stdout_artifact_id, stderr_artifact_id) = {
            let storage = self
                .storage
                .as_mut()
                .ok_or_else(|| "storage unavailable".to_owned())?;
            let stdout = storage
                .artifacts
                .put(&output)
                .map_err(|_| "output could not be stored".to_owned())?
                .as_str()
                .to_owned();
            let stderr = storage
                .artifacts
                .put(&[])
                .map_err(|_| "diagnostic output could not be stored".to_owned())?
                .as_str()
                .to_owned();
            (stdout, stderr)
        };
        // The remote deterministic transform is free, exactly like the local
        // reference run; this is intentionally not the same source line as
        // `run_reference_inner`'s zero-cost field so the mutation guard's
        // per-line anchor for that invariant stays unambiguous.
        let remote_transform_cost_microusd: u64 = 0;
        let claims = RunResultReceipt {
            schema_version: RUN_RESULT_SCHEMA_VERSION,
            run_id: record.run_id.clone(),
            genome_id: genome.genome_id.clone(),
            world_id: genome.world_id.clone(),
            source_revision,
            task_id: "remote-reference-v1".to_owned(),
            input_commitment: ArtifactId::for_bytes(REMOTE_REFERENCE_PROMPT.as_bytes())
                .as_str()
                .to_owned(),
            seed: 0,
            environment_id: format!("{}-remote-worker", reference_environment_id()),
            budget: RunBudgetReceipt {
                wall_millis: 10_000,
                maximum_output_bytes: 1_048_576,
                maximum_cost_microusd: remote_transform_cost_microusd,
            },
            completion_reason: match completion {
                RemoteCompletion::Success => RunCompletionReason::Success,
                RemoteCompletion::ProviderFailure => RunCompletionReason::ProviderFailure,
            },
            latency_millis,
            actual_cost_microusd: remote_transform_cost_microusd,
            stdout_artifact_id,
            stderr_artifact_id,
            trace_artifact_ids: Vec::new(),
        };
        let event_input = self
            .run_result_signer
            .issue(claims, now)
            .map_err(|_| "result claims could not be signed".to_owned())?;
        let event = self
            .storage
            .as_mut()
            .ok_or_else(|| "storage unavailable".to_owned())?
            .ledger
            .append(event_input)
            .map_err(|_| "result could not be recorded".to_owned())?;
        self.state
            .apply(&event, &self.operator_token, &self.run_result_verifier)
            .map_err(|_| "result could not be applied".to_owned())?;
        self.remote_leases.remove(job_id);
        Ok(())
    }

    fn require_no_active_job_for_sync_work(&self, command: &Command) -> Result<(), ExecuteError> {
        let storage_taking = matches!(
            command,
            Command::RunReference { .. }
                | Command::RunEvaluation { .. }
                | Command::ArenaSelect { .. }
                | Command::ArenaInvariants { .. }
                | Command::GenomeRegister { .. }
                | Command::GenomePropose { .. }
                | Command::GenomeAssess { .. }
                | Command::ForgeAnalyze { .. }
                | Command::ChampionSeed { .. }
                | Command::ChampionPromote { .. }
                | Command::ChampionRollback { .. }
                | Command::DriftRecord { .. }
                | Command::CanaryStart { .. }
                | Command::CanaryAdvance { .. }
                | Command::CanaryLiveCheck { .. }
                | Command::GeneExtract { .. }
                | Command::GeneTransfer { .. }
                | Command::GeneRecord { .. }
                | Command::GeneSpeciate { .. }
                | Command::WorldRegister { .. }
                | Command::ManifestPut { .. }
                | Command::ArtifactPut { .. }
                | Command::Replay
                | Command::EvolveStart { .. }
                | Command::MetaStrategyRegister { .. }
                | Command::MetaEvaluate { .. }
        );
        if (self.active_job.is_some() || self.active_arena_job.is_some()) && storage_taking {
            Err(ExecuteError::Busy)
        } else {
            Ok(())
        }
    }

    /// While one evolution run is actively advancing, refuses the same
    /// storage-taking commands an operator could otherwise use to race the
    /// reconciliation loop's own internal calls into these primitives (for
    /// example promoting a different child mid-run). The reconciliation loop
    /// itself calls the underlying methods directly and never through
    /// `execute`, so this gate never blocks the run's own progress.
    fn require_no_active_evolution_for_external_command(
        &self,
        command: &Command,
    ) -> Result<(), ExecuteError> {
        let blocked = matches!(
            command,
            Command::EvaluatePair { .. }
                | Command::ArenaSelect { .. }
                | Command::ArenaInvariants { .. }
                | Command::GenomePropose { .. }
                | Command::GenomeAssess { .. }
                | Command::ForgeAnalyze { .. }
                | Command::ChampionSeed { .. }
                | Command::ChampionPromote { .. }
                | Command::ChampionRollback { .. }
                | Command::DriftRecord { .. }
                | Command::CanaryStart { .. }
                | Command::CanaryAdvance { .. }
                | Command::CanaryLiveCheck { .. }
                | Command::GeneExtract { .. }
                | Command::GeneTransfer { .. }
                | Command::GeneRecord { .. }
                | Command::GeneSpeciate { .. }
        );
        if !blocked {
            return Ok(());
        }
        let Some(storage) = self.storage.as_ref() else {
            return Ok(());
        };
        let Ok(history) = storage.ledger.replay_verified() else {
            return Ok(());
        };
        if active_evolution_run_id(&history).ok().flatten().is_some() {
            Err(ExecuteError::Busy)
        } else {
            Ok(())
        }
    }

    #[allow(clippy::too_many_lines)]
    fn submit_job(&mut self, job_id: &str, genome_id: &str) -> Result<ResponseData, ExecuteError> {
        validate_job_id(job_id)?;
        if let Some(existing) = self.state.jobs.get(job_id) {
            if existing.genome_id != genome_id {
                return Err(ExecuteError::Rejected(
                    "job id is already bound to another Genome".to_owned(),
                ));
            }
            return Ok(self.job_response(existing.clone()));
        }
        if self.active_arena_job.is_some()
            || self.state.jobs.values().any(|job| {
                matches!(
                    job.state,
                    JobState::Admitted | JobState::Running | JobState::CancellationRequested
                )
            })
        {
            return Err(ExecuteError::Busy);
        }
        let genome = self.runnable_genome(genome_id)?;
        let selected_provider = self.selected_run_provider(genome_id)?;
        let run_id = job_run_id(job_id);
        // A reference-worker job pins a private copy of the worker binary and
        // proves it never changed identity mid-run; a provider job instead
        // pins the operator-configured Codex/Claude executable's digest into
        // the environment identity (`provider_job_environment`) and bounds
        // its cost by the Genome's registered World Law, matching the
        // already-working synchronous `run` path (`run_with_context`).
        let worker = if selected_provider.is_none() {
            Some(self.pin_reference_worker()?)
        } else {
            None
        };
        let spec = if let Some(provider) = selected_provider {
            self.async_provider_spec(&run_id, &genome, provider)?
        } else {
            self.async_reference_spec(
                &run_id,
                &genome,
                worker.as_ref().ok_or(ExecuteError::Internal)?,
            )?
        };
        let budget = spec.budget();
        let admitted = JobRecord {
            job_id: job_id.to_owned(),
            genome_id: genome.genome_id.clone(),
            run_id,
            source_revision: spec.source_revision().to_owned(),
            world_id: spec.world_id().to_owned(),
            task_id: spec.experiment().task_id().to_owned(),
            input_commitment: spec.experiment().input_commitment().to_owned(),
            seed: spec.experiment().seed(),
            environment_id: spec.experiment().environment_id().to_owned(),
            budget: RunBudgetReceipt {
                wall_millis: u64::try_from(budget.wall().as_millis())
                    .map_err(|_| ExecuteError::Internal)?,
                maximum_output_bytes: u64::try_from(budget.maximum_output_bytes())
                    .map_err(|_| ExecuteError::Internal)?,
                maximum_cost_microusd: budget.maximum_cost_microusd(),
            },
            state: JobState::Admitted,
            terminal: None,
        };
        self.append_job_record(&admitted)?;
        let running = JobRecord {
            state: JobState::Running,
            ..admitted.clone()
        };
        self.append_job_record(&running)?;

        let (evidence_sink, evidence_receiver) = hephaestus_experience::bounded_evidence_channel(1);
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spec_copy = spec.clone();
        let genome_copy = genome.clone();
        let data_dir = self.data_dir.clone();
        let guardian = self.guardian_executable.clone();
        let protected = self.protected_runtime_paths();
        let initial_sequence = self.state.event_count;
        let thread_cancel = Arc::clone(&cancel);
        let thread_job_id = job_id.to_owned();
        #[cfg(test)]
        let drop_result_after_execution =
            std::mem::take(&mut self.drop_next_direct_result_after_execution);
        let thread_name = format!(
            "hephaestus-job-{}",
            &blake3::hash(job_id.as_bytes()).to_hex()[..8]
        );
        let task: Box<dyn FnOnce() + Send> = if let Some(provider) = selected_provider {
            let executable = self.provider_executable(provider)?;
            let extra_env = resolve_provider_extra_env(&self.provider_env_allowlist);
            let redaction = RedactionPolicy::new([self.token_hex.clone()]);
            Box::new(move || {
                let output = execute_async_provider(
                    AsyncProviderLaunch {
                        data_dir,
                        guardian,
                        protected_paths: protected,
                        provider,
                        executable,
                        extra_env,
                        redaction,
                        cancel: thread_cancel,
                    },
                    &spec_copy,
                    evidence_sink,
                    initial_sequence,
                );
                #[cfg(test)]
                if drop_result_after_execution {
                    return;
                }
                let _ignored = result_sender.send(AsyncJobResult {
                    job_id: thread_job_id,
                    output,
                });
                drop(genome_copy);
            })
        } else {
            let worker_copy = Arc::clone(worker.as_ref().ok_or(ExecuteError::Internal)?);
            Box::new(move || {
                let output = execute_async_reference(
                    AsyncReferenceLaunch {
                        data_dir,
                        guardian,
                        protected_paths: protected,
                        worker: worker_copy,
                        cancel: thread_cancel,
                    },
                    &spec_copy,
                    evidence_sink,
                    initial_sequence,
                );
                #[cfg(test)]
                if drop_result_after_execution {
                    return;
                }
                let _ignored = result_sender.send(AsyncJobResult {
                    job_id: thread_job_id,
                    output,
                });
                drop(genome_copy);
            })
        };
        #[cfg(test)]
        let spawn_result = spawn_named_thread(
            thread_name,
            std::mem::take(&mut self.thread_spawn_failures.direct),
            task,
        );
        #[cfg(not(test))]
        let spawn_result = thread::Builder::new().name(thread_name).spawn(task);
        if spawn_result.is_err() {
            let mut terminal = running;
            terminal.state = JobState::Interrupted;
            terminal.terminal = Some(JobTerminal::Interrupted);
            self.append_job_record(&terminal)?;
            return Err(ExecuteError::Internal);
        }
        self.active_job = Some(ActiveJob {
            record: running.clone(),
            genome,
            spec,
            cancel,
        });
        self.job_evidence_receiver = Some(evidence_receiver);
        self.job_result_receiver = Some(result_receiver);
        Ok(self.job_response(running))
    }

    fn async_reference_spec(
        &self,
        run_id: &str,
        genome: &GenomeRecord,
        worker: &PinnedReferenceWorker,
    ) -> Result<RunSpec, ExecuteError> {
        let instruction = self
            .reference_instruction(&genome.genome_id)?
            .ok_or_else(|| {
                ExecuteError::Rejected(
                    "async reference jobs require a supported reference instruction".to_owned(),
                )
            })?;
        worker.verify()?;
        let task_id = "repository-inventory-v1";
        let prompt = "Inventory the isolated repository without modifying it or using the network.";
        let budget = validated_evaluation_budget(10_000, 1_048_576, 0)?;
        let experiment = ExperimentContext::new(
            task_id,
            prompt.as_bytes(),
            0,
            Self::reference_execution_environment(worker),
        )
        .map_err(|_| ExecuteError::Invalid("evaluation context is invalid"))?;
        let spec = RunSpec::new_for_experiment(
            run_id,
            &genome.genome_id,
            &genome.world_id,
            &self.source_repository,
            prompt,
            CapabilitySet::new(false, false),
            budget,
            experiment,
        )
        .map_err(|_| ExecuteError::Invalid("run specification is invalid"))?;
        spec.with_reference_instruction(instruction)
            .map_err(|_| ExecuteError::Invalid("reference task input is oversized"))
    }

    /// Provider-adapter counterpart of `async_reference_spec`: same fixed
    /// task/prompt and seed (`submit`'s canonical job contract is unchanged),
    /// but the environment identity binds the configured provider
    /// executable's digest, the cost budget is bounded by the Genome's
    /// registered World Law instead of a fixed zero, and the Genome's own
    /// compiled authority ceiling is used instead of the reference worker's
    /// deliberately empty capability set.
    fn async_provider_spec(
        &self,
        run_id: &str,
        genome: &GenomeRecord,
        provider: Provider,
    ) -> Result<RunSpec, ExecuteError> {
        let executable = self.provider_executable(provider)?;
        let digest = executable_digest(&executable).map_err(|_| ExecuteError::Internal)?;
        let task_id = "repository-inventory-v1";
        let prompt = "Inventory the isolated repository without modifying it or using the network.";
        let cost_ceiling = self.registered_world_cost_ceiling(&genome.world_id)?;
        let budget = validated_evaluation_budget(10_000, 1_048_576, cost_ceiling)?;
        let environment_id = Self::provider_job_environment(provider, &digest);
        let experiment = ExperimentContext::new(task_id, prompt.as_bytes(), 0, environment_id)
            .map_err(|_| ExecuteError::Invalid("evaluation context is invalid"))?;
        let capabilities = self.compiled_genome(&genome.genome_id)?.authority();
        RunSpec::new_for_experiment(
            run_id,
            &genome.genome_id,
            &genome.world_id,
            &self.source_repository,
            prompt,
            capabilities,
            budget,
            experiment,
        )
        .map_err(|_| ExecuteError::Invalid("run specification is invalid"))
    }

    fn job_status(&self, job_id: &str) -> Result<ResponseData, ExecuteError> {
        if let Some(job) = self.state.arena_jobs.get(job_id) {
            return Ok(Self::arena_job_response(job));
        }
        self.state
            .jobs
            .get(job_id)
            .cloned()
            .map(|job| self.job_response(job))
            .ok_or(ExecuteError::NotFound)
    }

    fn job_response(&self, job: JobRecord) -> ResponseData {
        let progress = self
            .state
            .job_progress
            .get(&job.job_id)
            .cloned()
            .unwrap_or_default();
        ResponseData::Job { job, progress }
    }

    fn kill_job(&mut self, job_id: &str) -> Result<ResponseData, ExecuteError> {
        if let Some(job) = self.state.arena_jobs.get(job_id).cloned() {
            if matches!(
                job.state,
                JobState::Succeeded | JobState::Failed | JobState::Interrupted
            ) {
                return Ok(Self::arena_job_response(&job));
            }
            let active = self
                .active_arena_job
                .as_ref()
                .ok_or(ExecuteError::Internal)?;
            if active.record.evaluation_id != job_id {
                return Err(ExecuteError::Internal);
            }
            active.cancel.store(true, Ordering::Release);
            let mut record = job;
            if record.state != JobState::CancellationRequested {
                record.state = JobState::CancellationRequested;
                self.append_arena_job_record(&record)?;
            }
            return Ok(Self::arena_job_response(&record));
        }
        let mut record = self
            .state
            .jobs
            .get(job_id)
            .cloned()
            .ok_or(ExecuteError::NotFound)?;
        if matches!(
            record.state,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        ) {
            return Ok(self.job_response(record));
        }
        self.request_job_cancellation(job_id)?;
        record = self
            .state
            .jobs
            .get(job_id)
            .cloned()
            .ok_or(ExecuteError::Internal)?;
        Ok(self.job_response(record))
    }

    fn request_active_job_cancellation(&mut self) -> Result<(), ExecuteError> {
        if let Some(active) = self.active_job.as_ref() {
            let job_id = active.record.job_id.clone();
            return self.request_job_cancellation(&job_id);
        }
        if let Some(active) = self.active_arena_job.as_ref() {
            let job_id = active.record.evaluation_id.clone();
            active.cancel.store(true, Ordering::Release);
            let Some(mut record) = self.state.arena_jobs.get(&job_id).cloned() else {
                return Err(ExecuteError::Internal);
            };
            if record.state != JobState::CancellationRequested {
                record.state = JobState::CancellationRequested;
                self.append_arena_job_record(&record)?;
            }
        }
        Ok(())
    }

    fn request_job_cancellation(&mut self, job_id: &str) -> Result<(), ExecuteError> {
        let mut record = self
            .state
            .jobs
            .get(job_id)
            .cloned()
            .ok_or(ExecuteError::NotFound)?;
        let active = self.active_job.as_ref().ok_or(ExecuteError::Internal)?;
        if active.record.job_id != job_id {
            return Err(ExecuteError::Internal);
        }
        active.cancel.store(true, Ordering::Release);
        if record.state != JobState::CancellationRequested {
            record.state = JobState::CancellationRequested;
            self.append_job_record(&record)?;
        }
        Ok(())
    }

    fn append_job_record(&mut self, record: &JobRecord) -> Result<(), ExecuteError> {
        let event_type = match record.state {
            JobState::Admitted => "job.admitted",
            JobState::Running => "job.running",
            JobState::CancellationRequested => "job.cancellation_requested",
            JobState::Succeeded | JobState::Failed | JobState::Interrupted => "job.terminal",
        };
        let event_suffix = event_type
            .strip_prefix("job.")
            .ok_or(ExecuteError::Internal)?;
        let payload = serde_json::to_vec(record).map_err(|_| ExecuteError::Internal)?;
        let event = self
            .storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                format!("job:{}:{event_suffix}", record.job_id),
                format!("job:{}", record.job_id),
                event_type,
                RUNTIME_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.state
            .apply(&event, &self.operator_token, &self.run_result_verifier)
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(active) = self
            .active_job
            .as_mut()
            .filter(|active| active.record.job_id == record.job_id)
        {
            active.record = record.clone();
        }
        Ok(())
    }

    fn service_async_messages(&mut self) -> Result<(), ControlError> {
        self.enforce_arena_deadline()?;
        for _ in 0..8 {
            let request = self
                .job_evidence_receiver
                .as_ref()
                .and_then(|receiver| receiver.try_recv().ok());
            let Some(request) = request else { break };
            let valid_direct = self.active_job.as_ref().is_some_and(|active| {
                request.run_id() == active.spec.run_id()
                    && request.provenance().is_none_or(|provenance| {
                        provenance.genome_id() == active.spec.genome_id()
                            && provenance.world_id() == active.spec.world_id()
                            && provenance.run_id() == active.spec.run_id()
                    })
            });
            let valid_arena = self.active_arena_job.as_ref().is_some_and(|active| {
                active.trials.iter().any(|trial| {
                    request.run_id() == trial.spec.run_id()
                        && request.provenance().is_none_or(|provenance| {
                            provenance.genome_id() == trial.spec.genome_id()
                                && provenance.world_id() == trial.spec.world_id()
                                && provenance.run_id() == trial.spec.run_id()
                        })
                })
            });
            if !valid_direct && !valid_arena {
                request.reject("writer rejected evidence outside the admitted run");
                if let Some(active) = &self.active_job {
                    active.cancel.store(true, Ordering::Release);
                }
                if let Some(active) = &self.active_arena_job {
                    active.cancel.store(true, Ordering::Release);
                }
                continue;
            }
            let Some(storage) = self.storage.take() else {
                request.reject("canonical writer is unavailable");
                if let Some(active) = &self.active_job {
                    active.cancel.store(true, Ordering::Release);
                }
                if let Some(active) = &self.active_arena_job {
                    active.cancel.store(true, Ordering::Release);
                }
                continue;
            };
            let mut recorder = EvidenceRecorder::from_stores(
                storage.ledger,
                storage.artifacts,
                RedactionPolicy::new([self.token_hex.clone()]),
                RetentionLimits::new(10_000, 65_536)
                    .map_err(|_| ControlError::Protocol("trace limits are invalid"))?,
            );
            let result = request.persist(&mut recorder);
            let (ledger, artifacts) = recorder.into_stores();
            self.storage = Some(CanonicalStorage { ledger, artifacts });
            if result.is_err() {
                if let Some(active) = &self.active_job {
                    active.cancel.store(true, Ordering::Release);
                }
                if let Some(active) = &self.active_arena_job {
                    active.cancel.store(true, Ordering::Release);
                }
            } else {
                self.refresh_projection().map_err(|_| {
                    ControlError::Projection("evidence projection failed".to_owned())
                })?;
            }
        }
        if let Some(receiver) = &self.job_result_receiver {
            match receiver.try_recv() {
                Ok(completed) => {
                    if self.complete_async_job(completed).is_err() {
                        return Err(ControlError::Projection(
                            "asynchronous job completion failed".to_owned(),
                        ));
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The executor has fully unwound. SupervisedRuntime's Drop
                    // waits for guardian confirmation before this channel closes.
                    if let Some(active) = self.active_job.as_ref() {
                        let mut record = active.record.clone();
                        record.state = JobState::Interrupted;
                        record.terminal = Some(JobTerminal::Interrupted);
                        self.append_job_record(&record).map_err(|_| {
                            ControlError::Projection(
                                "interrupted job could not be recorded".to_owned(),
                            )
                        })?;
                        self.active_job = None;
                        self.job_evidence_receiver = None;
                        self.job_result_receiver = None;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        self.service_arena_message()?;
        self.advance_evolution();
        self.advance_drift_adaptations();
        Ok(())
    }

    fn enforce_arena_deadline(&mut self) -> Result<(), ControlError> {
        let expired = self.active_arena_job.as_ref().is_some_and(|active| {
            !active.overall_timed_out
                && active.record.state == JobState::Running
                && Instant::now() >= active.overall_deadline
        });
        if !expired {
            return Ok(());
        }
        let record = {
            let active = self.active_arena_job.as_mut().ok_or_else(|| {
                ControlError::Projection("Arena job disappeared at its deadline".to_owned())
            })?;
            active.cancel.store(true, Ordering::Release);
            let mut record = active.record.clone();
            record.state = JobState::CancellationRequested;
            record
        };
        self.append_arena_job_record(&record).map_err(|_| {
            ControlError::Projection("Arena deadline could not be persisted".to_owned())
        })?;
        if let Some(active) = self.active_arena_job.as_mut() {
            active.overall_timed_out = true;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn service_arena_message(&mut self) -> Result<(), ControlError> {
        let Some(receiver) = self.arena_message_receiver.as_ref() else {
            return Ok(());
        };
        let message = match receiver.try_recv() {
            Ok(message) => message,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                if let Some(active) = self.active_arena_job.as_ref() {
                    let mut terminal = active.record.clone();
                    terminal.state = JobState::Interrupted;
                    terminal.phase = ArenaJobPhase::Terminal;
                    terminal.terminal = Some(JobTerminal::Interrupted);
                    self.append_arena_job_record(&terminal).map_err(|_| {
                        ControlError::Projection(
                            "interrupted Arena job could not be recorded".to_owned(),
                        )
                    })?;
                    self.active_arena_job = None;
                    self.arena_message_receiver = None;
                    self.job_evidence_receiver = None;
                }
                return Ok(());
            }
        };
        match message {
            ArenaWorkerMessage::Trial {
                job_id,
                index,
                output,
                reply,
            } => {
                let Some(active) = self.active_arena_job.as_ref() else {
                    let _ignored = reply.send(Err("paired job is no longer active".to_owned()));
                    return Ok(());
                };
                if active.record.evaluation_id != job_id
                    || usize::try_from(active.record.completed_trials).ok() != Some(index)
                {
                    let _ignored =
                        reply.send(Err("paired trial arrived outside admitted order".to_owned()));
                    active.cancel.store(true, Ordering::Release);
                    return Ok(());
                }
                let trial = active.trials.get(index).cloned().ok_or_else(|| {
                    ControlError::Projection("paired trial index is invalid".to_owned())
                })?;
                let result = output.and_then(|output| {
                    let storage = self
                        .storage
                        .as_ref()
                        .ok_or_else(|| "canonical writer unavailable".to_owned())?;
                    let response = persist_reference_output(
                        &storage.artifacts,
                        trial.spec.run_id(),
                        &trial.genome,
                        trial.spec.source_revision(),
                        output,
                    )
                    .map_err(|_| "trial output could not be persisted".to_owned())?;
                    self.append_run_result(&trial.spec, &response)
                        .map_err(|_| "trial result could not be signed".to_owned())?;
                    if let Some(active) = self.active_arena_job.as_mut() {
                        active.record.completed_trials = self
                            .state
                            .arena_jobs
                            .get(&job_id)
                            .map_or(active.record.completed_trials, |record| {
                                record.completed_trials
                            });
                        if active.record.completed_trials >= active.record.parent_trial_count {
                            active.record.phase = ArenaJobPhase::CandidateTrials;
                        }
                    }
                    Ok(self.state.event_count)
                });
                if result.is_err() {
                    if let Some(active) = self.active_arena_job.as_ref() {
                        active.cancel.store(true, Ordering::Release);
                    }
                }
                let _ignored = reply.send(result);
            }
            ArenaWorkerMessage::Trials { job_id, result } => {
                let Some(active) = self.active_arena_job.as_ref() else {
                    return Ok(());
                };
                if active.record.evaluation_id != job_id {
                    return Err(ControlError::Projection(
                        "paired completion crossed evaluation identity".to_owned(),
                    ));
                }
                if active.cancel.load(Ordering::Acquire)
                    || active.record.state == JobState::CancellationRequested
                    || result.is_err()
                {
                    let mut terminal = active.record.clone();
                    terminal.state = if active.overall_timed_out {
                        JobState::Failed
                    } else if active.record.state == JobState::CancellationRequested {
                        JobState::Interrupted
                    } else {
                        JobState::Failed
                    };
                    terminal.phase = ArenaJobPhase::Terminal;
                    terminal.terminal = Some(match terminal.state {
                        JobState::Interrupted => JobTerminal::Cancelled,
                        _ => JobTerminal::Failed,
                    });
                    self.append_arena_job_record(&terminal).map_err(|_| {
                        ControlError::Projection(
                            "Arena terminal state could not be recorded".to_owned(),
                        )
                    })?;
                    self.active_arena_job = None;
                    self.arena_message_receiver = None;
                    self.job_evidence_receiver = None;
                } else {
                    self.start_arena_scoring()?;
                }
            }
            ArenaWorkerMessage::Scoring { job_id, result } => {
                self.finish_arena_scoring(&job_id, result)?;
            }
        }
        Ok(())
    }

    fn start_arena_scoring(&mut self) -> Result<(), ControlError> {
        let active = self.active_arena_job.as_ref().ok_or_else(|| {
            ControlError::Projection("Arena scorer lost its active job".to_owned())
        })?;
        if active.record.completed_trials != active.record.total_trials {
            return Err(ControlError::Projection(
                "Arena scorer observed incomplete trials".to_owned(),
            ));
        }
        let storage = self
            .storage
            .take()
            .ok_or_else(|| ControlError::Projection("canonical writer unavailable".to_owned()))?;
        let stores = EvaluationStores {
            events: storage.ledger,
            artifacts: storage.artifacts,
        };
        let prepared_result = prepare_evaluation(
            &stores,
            &active.receipt_context,
            &active.world,
            EvaluationSources {
                binding: &active.binding,
                visible: &active.visible,
                sealed: &active.sealed,
                parent: &active.parent,
                candidate: &active.candidate,
            },
        );
        self.storage = Some(CanonicalStorage {
            ledger: stores.events,
            artifacts: stores.artifacts,
        });
        let prepared = prepared_result
            .map_err(|_| ControlError::Projection("Arena scorer preparation failed".to_owned()))?;
        let mut record = active.record.clone();
        let evaluator = Arc::clone(&active.evaluator);
        let cancel = Arc::clone(&active.cancel);
        let job_id = record.evaluation_id.clone();
        record.phase = ArenaJobPhase::Scoring;
        self.append_arena_job_record(&record).map_err(|_| {
            ControlError::Projection("Arena scoring phase could not be recorded".to_owned())
        })?;
        let guardian = self.guardian_executable.clone();
        let sender = self
            .arena_message_sender
            .as_ref()
            .ok_or_else(|| {
                ControlError::Projection("Arena message channel is unavailable".to_owned())
            })?
            .clone();
        let thread_name = format!(
            "hephaestus-score-{}",
            &blake3::hash(job_id.as_bytes()).to_hex()[..8]
        );
        let task = move || {
            let result = prepared
                .score_guarded(&evaluator, &guardian, cancel)
                .map_err(|_| "protected evaluator failed".to_owned());
            let _ignored = sender.send(ArenaWorkerMessage::Scoring { job_id, result });
        };
        #[cfg(test)]
        let spawn = spawn_named_thread(
            thread_name,
            std::mem::take(&mut self.thread_spawn_failures.arena_scoring),
            task,
        );
        #[cfg(not(test))]
        let spawn = thread::Builder::new().name(thread_name).spawn(task);
        if spawn.is_err() {
            let mut terminal = self
                .active_arena_job
                .as_ref()
                .ok_or_else(|| {
                    ControlError::Projection("Arena scorer lost its active job".to_owned())
                })?
                .record
                .clone();
            terminal.state = JobState::Interrupted;
            terminal.phase = ArenaJobPhase::Terminal;
            terminal.terminal = Some(JobTerminal::Interrupted);
            self.append_arena_job_record(&terminal).map_err(|_| {
                ControlError::Projection(
                    "Arena scorer launch failure could not be recorded".to_owned(),
                )
            })?;
            self.active_arena_job = None;
            self.arena_message_receiver = None;
            self.arena_message_sender = None;
            self.job_evidence_receiver = None;
            return Ok(());
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn finish_arena_scoring(
        &mut self,
        job_id: &str,
        result: Result<ScoredEvaluation, String>,
    ) -> Result<(), ControlError> {
        self.enforce_arena_deadline()?;
        let active = self.active_arena_job.as_ref().ok_or_else(|| {
            ControlError::Projection("Arena scorer completion has no active job".to_owned())
        })?;
        if active.record.evaluation_id != job_id {
            return Err(ControlError::Projection(
                "Arena scorer completion crossed evaluation identity".to_owned(),
            ));
        }
        if active.overall_timed_out {
            let mut terminal = active.record.clone();
            terminal.state = JobState::Failed;
            terminal.phase = ArenaJobPhase::Terminal;
            terminal.terminal = Some(JobTerminal::Failed);
            self.append_arena_job_record(&terminal).map_err(|_| {
                ControlError::Projection(
                    "timed out Arena terminal could not be recorded".to_owned(),
                )
            })?;
            self.clear_active_arena_job();
            return Ok(());
        }
        if active.record.state == JobState::CancellationRequested
            || active.cancel.load(Ordering::Acquire)
        {
            let mut terminal = active.record.clone();
            terminal.state = JobState::Interrupted;
            terminal.phase = ArenaJobPhase::Terminal;
            terminal.terminal = Some(JobTerminal::Cancelled);
            self.append_arena_job_record(&terminal).map_err(|_| {
                ControlError::Projection(
                    "cancelled Arena terminal state could not be recorded".to_owned(),
                )
            })?;
            self.clear_active_arena_job();
            return Ok(());
        }
        let Ok(scored) = result else {
            let mut terminal = active.record.clone();
            terminal.state = JobState::Failed;
            terminal.phase = ArenaJobPhase::Terminal;
            terminal.terminal = Some(JobTerminal::Failed);
            self.append_arena_job_record(&terminal).map_err(|_| {
                ControlError::Projection(
                    "failed Arena terminal state could not be recorded".to_owned(),
                )
            })?;
            self.clear_active_arena_job();
            return Ok(());
        };
        let mut committing = active.record.clone();
        committing.phase = ArenaJobPhase::Committing;
        self.append_arena_job_record(&committing).map_err(|_| {
            ControlError::Projection("Arena commit phase could not be recorded".to_owned())
        })?;
        let active = self.active_arena_job.as_ref().ok_or_else(|| {
            ControlError::Projection("Arena job disappeared before commit".to_owned())
        })?;
        let context = active.receipt_context.clone();
        let world = active.world.clone();
        let binding = active.binding.clone();
        let visible = active.visible.clone();
        let sealed = active.sealed.clone();
        let parent = active.parent.clone();
        let candidate = active.candidate.clone();
        let evaluator = Arc::clone(&active.evaluator);
        let storage = self
            .storage
            .take()
            .ok_or_else(|| ControlError::Projection("canonical writer unavailable".to_owned()))?;
        let result = evaluate_and_record_scored(
            EvaluationStores {
                events: storage.ledger,
                artifacts: storage.artifacts,
            },
            context,
            &world,
            EvaluationInputs {
                binding: &binding,
                visible: &visible,
                sealed: &sealed,
                parent: &parent,
                candidate: &candidate,
                evaluator: &evaluator,
            },
            scored,
        );
        let Ok(operator) = result else {
            self.reopen_storage().map_err(|_| {
                ControlError::Projection("canonical writer could not be reopened".to_owned())
            })?;
            let active = self.active_arena_job.as_ref().ok_or_else(|| {
                ControlError::Projection("Arena job disappeared after failed commit".to_owned())
            })?;
            let mut terminal = active.record.clone();
            terminal.state = JobState::Failed;
            terminal.phase = ArenaJobPhase::Terminal;
            terminal.terminal = Some(JobTerminal::Failed);
            self.append_arena_job_record(&terminal).map_err(|_| {
                ControlError::Projection(
                    "failed Arena terminal state could not be recorded".to_owned(),
                )
            })?;
            self.clear_active_arena_job();
            return Ok(());
        };
        let evaluation = evaluation_record_from_operator(&operator);
        let stores = operator.into_stores();
        self.storage = Some(CanonicalStorage {
            ledger: stores.events,
            artifacts: stores.artifacts,
        });
        self.refresh_projection()
            .map_err(|_| ControlError::Projection("Arena receipt projection failed".to_owned()))?;
        let active = self.active_arena_job.as_ref().ok_or_else(|| {
            ControlError::Projection("Arena job disappeared after commit".to_owned())
        })?;
        let mut terminal = active.record.clone();
        terminal.state = JobState::Succeeded;
        terminal.phase = ArenaJobPhase::Terminal;
        terminal.terminal = Some(JobTerminal::Succeeded);
        terminal.evaluation = Some(evaluation);
        self.append_arena_job_record(&terminal).map_err(|_| {
            ControlError::Projection(
                "successful Arena terminal state could not be recorded".to_owned(),
            )
        })?;
        self.clear_active_arena_job();
        Ok(())
    }

    fn clear_active_arena_job(&mut self) {
        self.active_arena_job = None;
        self.arena_message_receiver = None;
        self.arena_message_sender = None;
        self.job_evidence_receiver = None;
    }

    fn complete_async_job(&mut self, completed: AsyncJobResult) -> Result<(), ExecuteError> {
        let active = self.active_job.clone().ok_or(ExecuteError::Internal)?;
        if active.record.job_id != completed.job_id {
            return Err(ExecuteError::Internal);
        }
        let mut record = active.record.clone();
        if record.state == JobState::CancellationRequested {
            record.state = JobState::Interrupted;
            record.terminal = Some(JobTerminal::Cancelled);
            self.append_job_record(&record)?;
            self.active_job = None;
            self.job_evidence_receiver = None;
            self.job_result_receiver = None;
            return Ok(());
        }
        let terminal = if let Ok(output) = completed.output {
            let response = {
                let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
                persist_reference_output(
                    &storage.artifacts,
                    active.spec.run_id(),
                    &active.genome,
                    active.spec.source_revision(),
                    output,
                )?
            };
            self.append_run_result(&active.spec, &response)?;
            let ResponseData::Run {
                completion_reason, ..
            } = response
            else {
                return Err(ExecuteError::Internal);
            };
            if completion_reason == RunCompletionReason::Success {
                record.state = JobState::Succeeded;
                JobTerminal::Succeeded
            } else if completion_reason == RunCompletionReason::OperatorInterrupt {
                record.state = JobState::Interrupted;
                JobTerminal::Interrupted
            } else {
                record.state = JobState::Failed;
                JobTerminal::Failed
            }
        } else {
            record.state = JobState::Failed;
            JobTerminal::Failed
        };
        record.terminal = Some(terminal);
        self.append_job_record(&record)?;
        self.active_job = None;
        self.job_evidence_receiver = None;
        self.job_result_receiver = None;
        Ok(())
    }

    fn append_audit(
        &mut self,
        request_id: &str,
        command: &Command,
        event_type: &str,
    ) -> Result<(), ExecuteError> {
        let payload = serde_json::to_vec(&AuditedCommand {
            request_id,
            command,
        })
        .map_err(|_| ExecuteError::Internal)?;
        let next_sequence = self.state.event_count + 1;
        let event = self
            .storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                format!("control:{next_sequence}:{request_id}"),
                CONTROL_AGGREGATE,
                event_type,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.state
            .apply(&event, &self.operator_token, &self.run_result_verifier)
            .map_err(|_| ExecuteError::Internal)?;
        Ok(())
    }

    fn replay_response(&self) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let registered = RegisteredObjects::replay(&history, &storage.artifacts)
            .map_err(|_| ExecuteError::Internal)?;
        verify_selection_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_assessment_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_invariant_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_cluster_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_champion_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_drift_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_canary_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_gene_bank_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        let replayed = ControlState::from_events(
            &history,
            registered,
            &self.operator_token,
            &self.run_result_verifier,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_arena_evaluation_records(&storage.artifacts, &history, &replayed)
            .map_err(|_| ExecuteError::Internal)?;
        verify_drift_adaptation_history(&history).map_err(|_| ExecuteError::Internal)?;
        if replayed.snapshot() != self.state.snapshot() {
            return Err(ExecuteError::Internal);
        }
        let canonical =
            serde_json::to_vec(&replayed.snapshot()).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Replay {
            event_count: replayed.event_count,
            frozen: replayed.freeze.is_frozen(),
            active_runs: replayed.active_runs.len(),
            projection_hash: blake3::hash(&canonical).to_hex().to_string(),
        })
    }

    fn run_reference(
        &mut self,
        run_id: &str,
        genome_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        let genome = self.runnable_genome(genome_id)?;
        let result = self.run_reference_inner(run_id, &genome);
        self.refresh_projection()?;
        result
    }

    fn compiled_genome(&self, genome_id: &str) -> Result<CompiledGenome, ExecuteError> {
        self.state
            .registered
            .genome(genome_id)
            .map(|genome| genome.compiled().clone())
            .ok_or(ExecuteError::NotFound)
    }

    /// Resolves which provider adapter a Genome's `model.provider` selects.
    /// `None` keeps the existing reference-worker path (covers `deterministic`
    /// and any other value, so unrecognized text fails closed to the safe,
    /// already-verified default rather than to an unconfigured adapter).
    fn selected_run_provider(&self, genome_id: &str) -> Result<Option<Provider>, ExecuteError> {
        let compiled = self.compiled_genome(genome_id)?;
        Ok(match compiled.model_provider() {
            "codex" => Some(Provider::Codex),
            "claude" => Some(Provider::Claude),
            _ => None,
        })
    }

    fn runnable_genome(&self, genome_id: &str) -> Result<GenomeRecord, ExecuteError> {
        if self.state.freeze.is_frozen() {
            return Err(ExecuteError::Invalid("evolution is frozen"));
        }
        let genome = self
            .state
            .registered
            .genome(genome_id)
            .map(|genome| genome.record().clone())
            .ok_or(ExecuteError::NotFound)?;
        Ok(genome)
    }

    fn run_reference_inner(
        &mut self,
        run_id: &str,
        genome: &GenomeRecord,
    ) -> Result<ResponseData, ExecuteError> {
        let prompt = "Inventory the isolated repository without modifying it or using the network.";
        // Deterministic runs never report cost, so a zero ceiling is exact for
        // them; a provider genome is bounded by its own World's approved Law
        // instead of an arbitrary fixed figure.
        let maximum_cost_microusd = match self.selected_run_provider(&genome.genome_id)? {
            Some(_) => self.registered_world_cost_ceiling(&genome.world_id)?,
            None => 0,
        };
        self.run_with_context(
            run_id,
            genome,
            "repository-inventory-v1",
            prompt,
            0,
            10_000,
            1_048_576,
            maximum_cost_microusd,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_evaluation(
        &mut self,
        run_id: &str,
        genome_id: &str,
        task_id: &str,
        input: &str,
        seed: u64,
        wall_millis: u64,
        maximum_output_bytes: u64,
        maximum_cost_microusd: u64,
    ) -> Result<ResponseData, ExecuteError> {
        let genome = self.runnable_genome(genome_id)?;
        let world_cost_ceiling = self.registered_world_cost_ceiling(&genome.world_id)?;
        if maximum_cost_microusd > world_cost_ceiling {
            return Err(ExecuteError::Invalid(
                "evaluation cost exceeds registered World Law",
            ));
        }
        let result = self.run_with_context(
            run_id,
            &genome,
            task_id,
            input,
            seed,
            wall_millis,
            maximum_output_bytes,
            maximum_cost_microusd,
        );
        self.refresh_projection()?;
        result
    }

    #[allow(clippy::too_many_lines)]
    fn submit_arena_job(
        &mut self,
        evaluation_id: &str,
        parent_genome_id: &str,
        candidate_genome_id: &str,
        remote: bool,
    ) -> Result<ResponseData, ExecuteError> {
        validate_job_id(evaluation_id)?;
        if let Some(existing) = self.state.arena_jobs.get(evaluation_id) {
            if existing.parent_genome_id != parent_genome_id
                || existing.candidate_genome_id != candidate_genome_id
            {
                return Err(ExecuteError::Rejected(
                    "evaluation id is already bound to another Genome pair".to_owned(),
                ));
            }
            return Ok(Self::arena_job_response(existing));
        }
        if self.active_job.is_some()
            || self.active_arena_job.is_some()
            || self.state.jobs.values().any(|job| {
                matches!(
                    job.state,
                    JobState::Admitted | JobState::Running | JobState::CancellationRequested
                )
            })
            || self.state.arena_jobs.values().any(|job| {
                matches!(
                    job.state,
                    JobState::Admitted | JobState::Running | JobState::CancellationRequested
                )
            })
        {
            return Err(ExecuteError::Busy);
        }
        if parent_genome_id == candidate_genome_id {
            return Err(ExecuteError::Invalid(
                "parent and candidate Genomes must differ",
            ));
        }
        let parent_genome = self.runnable_genome(parent_genome_id)?;
        let candidate_genome = self.runnable_genome(candidate_genome_id)?;
        if parent_genome.world_id != candidate_genome.world_id {
            return Err(ExecuteError::Invalid(
                "paired Genomes must share one registered World",
            ));
        }
        self.reference_instruction(parent_genome_id)?;
        self.reference_instruction(candidate_genome_id)?;
        let world = self.registered_world(&parent_genome.world_id)?;
        let visible_manifest_id = world
            .evaluator_artifact("arena.visible_manifest")
            .ok_or_else(|| {
                ExecuteError::Rejected("World does not declare arena.visible_manifest".to_owned())
            })?
            .to_owned();
        let sealed_manifest_id = world
            .evaluator_artifact("arena.sealed_manifest")
            .ok_or_else(|| {
                ExecuteError::Rejected("World does not declare arena.sealed_manifest".to_owned())
            })?
            .to_owned();
        let visible = self.world_manifest(&world, "arena.visible_manifest", Visibility::Visible)?;
        let sealed = self.world_manifest(&world, "arena.sealed_manifest", Visibility::Sealed)?;
        let evaluator_id = world
            .evaluator_artifact("arena.evaluator")
            .ok_or_else(|| {
                ExecuteError::Rejected("World does not declare arena.evaluator".to_owned())
            })?
            .to_owned();
        let parent_provider = self.selected_run_provider(parent_genome_id)?;
        let candidate_provider = self.selected_run_provider(candidate_genome_id)?;
        // A paired trial's cost ceiling is bounded by the World's own Law
        // exactly like a single provider `run`, rather than the reference
        // smoke test's fixed zero; a homogeneous reference-only pair keeps
        // that zero ceiling unchanged.
        let per_trial_cost_ceiling = if parent_provider.is_some() || candidate_provider.is_some() {
            self.registered_world_cost_ceiling(&parent_genome.world_id)?
        } else {
            0
        };
        let per_trial_wall = PAIRED_EVALUATION_WALL_MILLIS;
        let budget = validated_evaluation_budget(
            per_trial_wall,
            PAIRED_EVALUATION_OUTPUT_BYTES,
            per_trial_cost_ceiling,
        )?;
        let trial_budget = RunBudgetReceipt {
            wall_millis: per_trial_wall,
            maximum_output_bytes: PAIRED_EVALUATION_OUTPUT_BYTES,
            maximum_cost_microusd: per_trial_cost_ceiling,
        };
        let tasks = visible
            .operator_tasks()
            .into_iter()
            .chain(sealed.operator_tasks())
            .collect::<Vec<_>>();
        if tasks.is_empty() || tasks.len() > 1000 {
            return Err(ExecuteError::Invalid(
                "paired task count is outside the bounded range",
            ));
        }
        let total_trials = tasks.len().checked_mul(2).ok_or(ExecuteError::Internal)?;
        let total_trials_u32 = u32::try_from(total_trials).map_err(|_| ExecuteError::Internal)?;
        let maximum_overall_wall = per_trial_wall
            .checked_mul(u64::try_from(total_trials).map_err(|_| ExecuteError::Internal)?)
            .and_then(|wall| wall.checked_add(PAIRED_EVALUATION_WALL_MILLIS))
            .ok_or(ExecuteError::Internal)?;
        #[cfg(feature = "test-support")]
        let overall_wall = test_overall_wall(
            maximum_overall_wall,
            env::var(TEST_ARENA_OVERALL_WALL_ENV).ok().as_deref(),
        )
        .map_err(|()| ExecuteError::Invalid("test Arena wall budget must only lower the bound"))?;
        #[cfg(not(feature = "test-support"))]
        let overall_wall = maximum_overall_wall;
        let overall_budget = RunBudgetReceipt {
            wall_millis: overall_wall,
            maximum_output_bytes: PAIRED_EVALUATION_OUTPUT_BYTES
                .checked_mul(u64::try_from(total_trials).map_err(|_| ExecuteError::Internal)?)
                .ok_or(ExecuteError::Internal)?,
            maximum_cost_microusd: per_trial_cost_ceiling
                .checked_mul(u64::try_from(total_trials).map_err(|_| ExecuteError::Internal)?)
                .ok_or(ExecuteError::Internal)?,
        };
        let evaluator_limits = WorkerLimits::new(
            Duration::from_millis(PAIRED_EVALUATION_WALL_MILLIS),
            16 * 1024 * 1024,
            128 * 1024,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let evaluator = Arc::new(self.open_evaluator(&evaluator_id, evaluator_limits)?);
        // The reference worker is pinned unconditionally: even a fully
        // provider paired trial keeps the same admission shape, and a mixed
        // pair needs it for whichever role stays on the reference path.
        let worker = self.pin_reference_worker()?;
        let reference_environment_id = Self::reference_execution_environment(&worker);
        let provider_environment_id = |provider: Provider| -> Result<String, ExecuteError> {
            let executable = self.provider_executable(provider)?;
            let digest = executable_digest(&executable).map_err(|_| ExecuteError::Internal)?;
            Ok(Self::provider_job_environment(provider, &digest))
        };
        let parent_environment_id = match parent_provider {
            Some(provider) => provider_environment_id(provider)?,
            None => reference_environment_id.clone(),
        };
        let candidate_environment_id = match candidate_provider {
            Some(provider) => provider_environment_id(provider)?,
            None => reference_environment_id.clone(),
        };
        let mixed_environments = parent_environment_id != candidate_environment_id;
        if mixed_environments && !world.evaluation_policy().allow_mixed_environments() {
            return Err(ExecuteError::Rejected(
                "World does not permit a parent and candidate to run in distinct execution environments"
                    .to_owned(),
            ));
        }
        let revision = self.paired_revision(evaluation_id)?;
        let mut binding = EvaluationBinding::new(
            world.id(),
            PAIRED_EVALUATION_SEED,
            &parent_environment_id,
            &evaluator_id,
            trial_budget,
        )
        .map_err(|_| ExecuteError::Internal)?;
        if mixed_environments {
            binding = binding
                .with_candidate_environment(&candidate_environment_id)
                .map_err(|_| ExecuteError::Internal)?;
        }
        let mut trial_specs = Vec::with_capacity(total_trials);
        let mut parent_plan = Vec::with_capacity(tasks.len());
        let mut candidate_plan = Vec::with_capacity(tasks.len());
        for (role, genome, plan, provider, role_environment_id) in [
            (
                "parent",
                &parent_genome,
                &mut parent_plan,
                parent_provider,
                &parent_environment_id,
            ),
            (
                "candidate",
                &candidate_genome,
                &mut candidate_plan,
                candidate_provider,
                &candidate_environment_id,
            ),
        ] {
            for (index, task) in tasks.iter().enumerate() {
                let run_id = paired_run_id(evaluation_id, role, index);
                let event_id = format!("result:{run_id}");
                let experiment = ExperimentContext::new(
                    &task.task_id,
                    task.input.as_bytes(),
                    PAIRED_EVALUATION_SEED,
                    role_environment_id.clone(),
                )
                .map_err(|_| ExecuteError::Internal)?;
                let capabilities = match provider {
                    Some(_) => self.compiled_genome(&genome.genome_id)?.authority(),
                    None => CapabilitySet::new(false, false),
                };
                let mut spec = RunSpec::new_for_experiment_at_revision(
                    &run_id,
                    &genome.genome_id,
                    &genome.world_id,
                    &self.source_repository,
                    &revision,
                    &task.input,
                    capabilities,
                    budget,
                    experiment,
                )
                .map_err(|_| ExecuteError::Internal)?;
                if provider.is_none() {
                    let instruction = self
                        .reference_instruction(&genome.genome_id)?
                        .unwrap_or(ReferenceInstruction::Identity);
                    spec = spec
                        .with_reference_instruction(instruction)
                        .map_err(|_| ExecuteError::Internal)?;
                }
                plan.push((task.task_id.clone(), event_id));
                trial_specs.push(ArenaTrialSpec {
                    genome: genome.clone(),
                    spec,
                    provider,
                });
            }
        }
        let parent_trial_count =
            u32::try_from(parent_plan.len()).map_err(|_| ExecuteError::Internal)?;
        let parent = TrialPlan::new(parent_plan).map_err(|_| ExecuteError::Internal)?;
        let candidate = TrialPlan::new(candidate_plan).map_err(|_| ExecuteError::Internal)?;
        let run_ids = trial_specs
            .iter()
            .map(|trial| trial.spec.run_id().to_owned())
            .collect::<Vec<_>>();
        let plan_commitment = blake3::hash(
            &serde_json::to_vec(&(
                evaluation_id,
                parent_genome_id,
                candidate_genome_id,
                world.id(),
                &visible_manifest_id,
                &sealed_manifest_id,
                &revision,
                &parent_environment_id,
                &candidate_environment_id,
                &run_ids,
            ))
            .map_err(|_| ExecuteError::Internal)?,
        )
        .to_hex()
        .to_string();
        let receipt_context = ReceiptContext {
            event_id: format!("arena:evaluation:{evaluation_id}:recorded"),
            evaluation_id: evaluation_id.to_owned(),
            caller_id: "control-daemon".to_owned(),
            timestamp_millis: timestamp_millis().map_err(|_| ExecuteError::Internal)?,
        };
        let mut admitted = ArenaJobRecord {
            job_id: evaluation_id.to_owned(),
            evaluation_id: evaluation_id.to_owned(),
            parent_genome_id: parent_genome_id.to_owned(),
            candidate_genome_id: candidate_genome_id.to_owned(),
            world_id: world.id().to_owned(),
            visible_manifest_id,
            sealed_manifest_id,
            evaluator_id,
            source_revision: revision,
            worker_digest: worker.digest.clone(),
            environment_id: parent_environment_id.clone(),
            candidate_environment_id: mixed_environments.then(|| candidate_environment_id.clone()),
            seed: PAIRED_EVALUATION_SEED,
            trial_budget,
            overall_budget,
            ordered_trial_run_ids: run_ids,
            parent_trial_count,
            total_trials: total_trials_u32,
            plan_commitment,
            caller_id: receipt_context.caller_id.clone(),
            receipt_timestamp_millis: receipt_context.timestamp_millis,
            completed_trials: 0,
            phase: ArenaJobPhase::Preparing,
            state: JobState::Admitted,
            terminal: None,
            evaluation: None,
            remote,
        };
        self.append_arena_job_record(&admitted)?;
        admitted.phase = ArenaJobPhase::ParentTrials;
        admitted.state = JobState::Running;
        self.append_arena_job_record(&admitted)?;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (evidence, evidence_receiver) = hephaestus_experience::bounded_evidence_channel(1);
        let (message_sender, message_receiver) = mpsc::sync_channel(1);
        let scorer_sender = message_sender.clone();
        let active_trials = trial_specs.clone();
        let overall_deadline = Instant::now()
            .checked_add(Duration::from_millis(overall_wall))
            .ok_or(ExecuteError::Internal)?;
        let launch = AsyncArenaTrialLaunch {
            data_dir: self.data_dir.clone(),
            guardian: self.guardian_executable.clone(),
            protected_paths: self.protected_runtime_paths(),
            worker: Arc::clone(&worker),
            codex_executable: self.codex_executable.clone(),
            claude_executable: self.claude_executable.clone(),
            provider_extra_env: resolve_provider_extra_env(&self.provider_env_allowlist),
            redaction: RedactionPolicy::new([self.token_hex.clone()]),
            cancel: Arc::clone(&cancel),
            trials: trial_specs,
            evidence,
            messages: message_sender,
            initial_sequence: self.state.event_count,
            job_id: evaluation_id.to_owned(),
            remote_lease: remote.then(|| Arc::clone(&self.remote_arena_lease)),
            overall_deadline,
        };
        let thread_name = format!(
            "hephaestus-arena-{}",
            &blake3::hash(evaluation_id.as_bytes()).to_hex()[..8]
        );
        let task = move || execute_async_arena_trials(launch);
        #[cfg(test)]
        let spawn = spawn_named_thread(
            thread_name,
            std::mem::take(&mut self.thread_spawn_failures.arena),
            task,
        );
        #[cfg(not(test))]
        let spawn = thread::Builder::new().name(thread_name).spawn(task);
        if spawn.is_err() {
            admitted.state = JobState::Interrupted;
            admitted.phase = ArenaJobPhase::Terminal;
            admitted.terminal = Some(JobTerminal::Interrupted);
            self.append_arena_job_record(&admitted)?;
            return Err(ExecuteError::Internal);
        }
        self.active_arena_job = Some(ActiveArenaJob {
            record: admitted,
            world,
            visible,
            sealed,
            binding,
            parent,
            candidate,
            receipt_context,
            trials: active_trials,
            evaluator,
            cancel,
            overall_deadline,
            overall_timed_out: false,
        });
        // Keep the specs in the worker thread. The projection only needs their
        // immutable committed run identities and trusted plans.
        self.arena_message_receiver = Some(message_receiver);
        self.arena_message_sender = Some(scorer_sender);
        self.job_evidence_receiver = Some(evidence_receiver);
        let record = &self
            .active_arena_job
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .record;
        Ok(Self::arena_job_response(record))
    }

    fn arena_job_response(record: &ArenaJobRecord) -> ResponseData {
        ResponseData::ArenaJob {
            job: ArenaJobProgress {
                evaluation_id: record.evaluation_id.clone(),
                parent_genome_id: record.parent_genome_id.clone(),
                candidate_genome_id: record.candidate_genome_id.clone(),
                state: record.state,
                phase: record.phase,
                completed_trials: record.completed_trials,
                total_trials: record.total_trials,
                evaluation: record.evaluation.clone(),
            },
        }
    }

    fn append_arena_job_record(&mut self, record: &ArenaJobRecord) -> Result<(), ExecuteError> {
        let event_type = match record.state {
            JobState::Admitted => "arena.job.admitted",
            JobState::Running if record.phase == ArenaJobPhase::Scoring => "arena.job.scoring",
            JobState::Running if record.phase == ArenaJobPhase::Committing => {
                "arena.job.committing"
            }
            JobState::Running => "arena.job.running",
            JobState::CancellationRequested => "arena.job.cancellation_requested",
            JobState::Succeeded | JobState::Failed | JobState::Interrupted => "arena.job.terminal",
        };
        let suffix = event_type
            .strip_prefix("arena.job.")
            .ok_or(ExecuteError::Internal)?;
        let payload = serde_json::to_vec(record).map_err(|_| ExecuteError::Internal)?;
        let event = self
            .storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                format!("arena-job:{}:{suffix}", record.evaluation_id),
                format!("arena-job:{}", record.evaluation_id),
                event_type,
                RUNTIME_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        if let Err(_error) =
            self.state
                .apply(&event, &self.operator_token, &self.run_result_verifier)
        {
            return Err(ExecuteError::Internal);
        }
        if let Some(active) = self
            .active_arena_job
            .as_mut()
            .filter(|job| job.record.evaluation_id == record.evaluation_id)
        {
            active.record = record.clone();
        }
        Ok(())
    }

    fn select_arena_evaluation(
        &mut self,
        evaluation_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if evaluation_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("evaluation_id is required"));
        }

        // Derive the World from the verified persisted Arena receipt. There is no
        // caller-supplied policy selector on this command.
        let stores = self.open_arena_stores()?;
        let operator = load_operator_evaluation(stores, evaluation_id)
            .map_err(|error| map_selection_error(&error))?;
        let world_id = operator.selection_evidence().world_id().to_owned();
        drop(operator.into_stores());
        let world = self
            .state
            .registered
            .world(&world_id)
            .map(|registered| registered.compiled().clone())
            .ok_or(ExecuteError::NotFound)?;
        let timestamp = timestamp_millis().map_err(|_| ExecuteError::Internal)?;

        // Arena consumes stores on both success and error. Always restore the
        // daemon's handles before returning so one invalid select cannot brick it.
        let storage = self.storage.take().ok_or(ExecuteError::Internal)?;
        let selected = select_and_record(
            EvaluationStores {
                events: storage.ledger,
                artifacts: storage.artifacts,
            },
            evaluation_id,
            &world,
            timestamp,
        );
        let selected = match selected {
            Ok(selected) => selected,
            Err(error) => {
                self.reopen_storage()?;
                self.refresh_projection()?;
                return Err(map_selection_error(&error));
            }
        };
        let record = selection_record(&world_id, selected.receipt(), selected.event());
        let stores = selected.into_stores();
        self.storage = Some(CanonicalStorage {
            ledger: stores.events,
            artifacts: stores.artifacts,
        });

        self.refresh_projection()?;
        Ok(ResponseData::Selection {
            selection: Box::new(record),
        })
    }

    fn check_arena_invariants(
        &mut self,
        evaluation_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if evaluation_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("evaluation_id is required"));
        }

        // Resolve policy from the exact persisted evaluation receipt. The
        // caller cannot choose a World or substitute another evaluation.
        let operator = load_operator_evaluation(self.open_arena_stores()?, evaluation_id)
            .map_err(map_invariant_error)?;
        let world_id = operator.selection_evidence().world_id().to_owned();
        drop(operator.into_stores());
        let world = self
            .state
            .registered
            .world(&world_id)
            .map(|registered| registered.compiled().clone())
            .ok_or(ExecuteError::NotFound)?;

        // An existing deterministic receipt is a verified idempotent retry.
        match load_reference_output_invariants(self.open_arena_stores()?, evaluation_id, &world) {
            Ok(check) => return self.finish_invariant_check(check),
            Err(ArenaError::UnknownInvariantCheck(_)) => {}
            Err(error) => return Err(map_invariant_error(error)),
        }

        // Arena consumes storage on either outcome. Restore the daemon handles
        // before refreshing or returning an error.
        let storage = self.storage.take().ok_or(ExecuteError::Internal)?;
        let checked = check_reference_output_invariants(
            EvaluationStores {
                events: storage.ledger,
                artifacts: storage.artifacts,
            },
            evaluation_id,
            &world,
            timestamp_millis().map_err(|_| ExecuteError::Internal)?,
        );
        let check = match checked {
            Ok(check) => check,
            Err(error) => {
                self.reopen_storage()?;
                self.refresh_projection()?;
                return Err(map_invariant_error(error));
            }
        };
        self.finish_invariant_check(check)
    }

    fn finish_invariant_check(
        &mut self,
        check: OperatorInvariantCheck,
    ) -> Result<ResponseData, ExecuteError> {
        let record = invariant_record(check.receipt(), check.event());
        let stores = check.into_stores();
        self.storage = Some(CanonicalStorage {
            ledger: stores.events,
            artifacts: stores.artifacts,
        });
        self.refresh_projection()?;
        Ok(ResponseData::ArenaInvariants {
            invariants: Box::new(record),
        })
    }

    fn analyze_forge_clusters(
        &mut self,
        analysis_id: &str,
        evaluation_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if analysis_id.trim().is_empty() || evaluation_id.trim().is_empty() {
            return Err(ExecuteError::Invalid(
                "analysis_id and evaluation_id are required",
            ));
        }

        // Resolve policy from the exact persisted evaluation receipt. The
        // caller cannot choose a World or substitute another evaluation.
        let operator = load_operator_evaluation(self.open_arena_stores()?, evaluation_id)
            .map_err(map_cluster_error)?;
        let world_id = operator.selection_evidence().world_id().to_owned();
        let candidate_genome_id = operator
            .selection_evidence()
            .candidate_genome_id()
            .to_owned();
        drop(operator.into_stores());
        let world = self
            .state
            .registered
            .world(&world_id)
            .map(|registered| registered.compiled().clone())
            .ok_or(ExecuteError::NotFound)?;
        // The candidate's own current reference operation, resolved from its
        // registered Genome and the daemon's already-open artifact store
        // (roadmap items 8, 10, 13): `failure-cluster-v2`'s suggestion rule
        // reads this, and replay re-derives it the same deterministic way
        // (see `verify_cluster_history`).
        let current_operation = self
            .storage
            .as_ref()
            .and_then(|storage| {
                genome_reference_instruction(
                    &self.state.registered,
                    &storage.artifacts,
                    &candidate_genome_id,
                )
            })
            .map(ReferenceInstruction::operation_name);

        // An existing deterministic analysis is a verified idempotent retry.
        match load_failure_clusters(
            self.open_arena_stores()?,
            analysis_id,
            evaluation_id,
            &world,
            current_operation,
        ) {
            Ok(check) => return self.finish_cluster_check(check),
            Err(ArenaError::UnknownClusterAnalysis(_)) => {}
            Err(error) => return Err(map_cluster_error(error)),
        }

        // Arena consumes storage on either outcome. Restore the daemon handles
        // before refreshing or returning an error.
        let storage = self.storage.take().ok_or(ExecuteError::Internal)?;
        let checked = check_failure_clusters(
            EvaluationStores {
                events: storage.ledger,
                artifacts: storage.artifacts,
            },
            analysis_id,
            evaluation_id,
            &world,
            current_operation,
            timestamp_millis().map_err(|_| ExecuteError::Internal)?,
        );
        let check = match checked {
            Ok(check) => check,
            Err(error) => {
                self.reopen_storage()?;
                self.refresh_projection()?;
                return Err(map_cluster_error(error));
            }
        };
        self.finish_cluster_check(check)
    }

    fn finish_cluster_check(
        &mut self,
        check: OperatorClusterAnalysis,
    ) -> Result<ResponseData, ExecuteError> {
        let record = forge_analysis_record(check.analysis(), check.event());
        let stores = check.into_stores();
        self.storage = Some(CanonicalStorage {
            ledger: stores.events,
            artifacts: stores.artifacts,
        });
        self.refresh_projection()?;
        Ok(ResponseData::ForgeAnalysis {
            analysis: Box::new(record),
        })
    }

    fn open_arena_stores(&self) -> Result<EvaluationStores, ExecuteError> {
        let events = (self.open_ledger)().map_err(|_| ExecuteError::Internal)?;
        let artifacts = (self.open_artifacts)().map_err(|_| ExecuteError::Internal)?;
        Ok(EvaluationStores { events, artifacts })
    }

    fn registered_world(&self, world_id: &str) -> Result<CompiledWorld, ExecuteError> {
        self.state
            .registered
            .world(world_id)
            .map(|world| world.compiled().clone())
            .ok_or(ExecuteError::Internal)
    }

    fn reference_instruction(
        &self,
        genome_id: &str,
    ) -> Result<Option<ReferenceInstruction>, ExecuteError> {
        let genome = self
            .state
            .registered
            .genome(genome_id)
            .ok_or(ExecuteError::NotFound)?;
        let Some(artifact) = genome.compiled().artifact_id("agent.prompt") else {
            return Ok(None);
        };
        let id = ArtifactId::parse(artifact.to_owned()).map_err(|_| ExecuteError::Internal)?;
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let bytes = storage
            .artifacts
            .get(&id)
            .map_err(|_| ExecuteError::Internal)?;
        let body = std::str::from_utf8(&bytes).map_err(|_| {
            ExecuteError::Rejected("registered reference instruction is invalid".to_owned())
        })?;
        ReferenceInstruction::parse(body).map(Some).map_err(|_| {
            ExecuteError::Rejected("registered reference instruction is invalid".to_owned())
        })
    }

    /// Returns the daemon-lifetime pinned reference worker, creating it on
    /// first use and re-verifying it before every subsequent use.
    ///
    /// The private snapshot is content-addressed and pinned exactly once per
    /// daemon lifetime: every direct reference run, synchronous
    /// `RunEvaluation`, and Arena job admission shares the same `Arc`-held
    /// `TempDir` and executable copy instead of writing (and `fsync`-ing) a
    /// fresh one per call. A cached pin is re-verified — re-hashed and
    /// compared against `reference_worker_digest` — before every reuse; a
    /// mismatch fails this call closed with the existing rejection and clears
    /// the cache so the next call pins a fresh snapshot rather than reusing a
    /// tampered one.
    fn pin_reference_worker(&self) -> Result<Arc<PinnedReferenceWorker>, ExecuteError> {
        // The borrow is taken and released in its own statement (rather than
        // directly in an `if let` condition) so the immutable `Ref` is
        // dropped before a mismatch below needs `borrow_mut`; otherwise the
        // temporary's extended lifetime would panic with "already borrowed".
        let cached = self.pinned_reference_worker.borrow().clone();
        if let Some(existing) = cached {
            return match existing.verify() {
                Ok(()) => Ok(existing),
                Err(err) => {
                    *self.pinned_reference_worker.borrow_mut() = None;
                    Err(err)
                }
            };
        }
        let worker = Arc::new(self.pin_fresh_reference_worker()?);
        *self.pinned_reference_worker.borrow_mut() = Some(Arc::clone(&worker));
        Ok(worker)
    }

    /// Writes and verifies a brand-new private snapshot of the reference
    /// worker executable. Called only by `pin_reference_worker`, on first use
    /// or after a cached pin failed re-verification.
    fn pin_fresh_reference_worker(&self) -> Result<PinnedReferenceWorker, ExecuteError> {
        let bytes =
            fs::read(&self.reference_worker_executable).map_err(|_| ExecuteError::Internal)?;
        let digest = blake3::hash(&bytes).to_hex().to_string();
        if digest != self.reference_worker_digest {
            return Err(ExecuteError::Rejected(
                "reference worker identity changed after daemon startup".to_owned(),
            ));
        }
        let directory = TempDirBuilder::new()
            .prefix("reference-worker-")
            .tempdir_in(&self.data_dir)
            .map_err(|_| ExecuteError::Internal)?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .map_err(|_| ExecuteError::Internal)?;
        let executable = directory.path().join("worker");
        fs::write(&executable, bytes).map_err(|_| ExecuteError::Internal)?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))
            .map_err(|_| ExecuteError::Internal)?;
        let worker = PinnedReferenceWorker {
            directory,
            executable,
            digest,
        };
        worker.verify()?;
        Ok(worker)
    }

    fn reference_execution_environment(worker: &PinnedReferenceWorker) -> String {
        let identity = format!(
            "{}|reference-instruction-language-v1|{}",
            reference_environment_id(),
            worker.digest
        );
        format!(
            "reference-v1.{}",
            blake3::hash(identity.as_bytes()).to_hex()
        )
    }

    /// Versioned execution-environment identity for a job/Arena admission
    /// record that binds a Codex or Claude provider instead of the reference
    /// worker. Distinct from `provider_execution_environment` (used by the
    /// already-working synchronous `run`/`RunEvaluation` paths): this one is
    /// content-addressed like `reference_execution_environment` above so a
    /// canonical job or Arena record can carry it as an opaque, replay-stable
    /// string, and it binds the exact configured executable's digest rather
    /// than only the provider name — the identity a job/Arena admission needs
    /// to prove exactly which binary produced the receipt.
    fn provider_job_environment(provider: Provider, executable_digest: &str) -> String {
        let name = match provider {
            Provider::Codex => "codex-cli",
            Provider::Claude => "claude-cli",
            Provider::Deterministic => "deterministic",
        };
        let identity = format!(
            "{name}-v1.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git|provider-instruction-language-v1|exe-{executable_digest}",
            env!("CARGO_PKG_VERSION"),
            RUN_RESULT_SCHEMA_VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        format!("provider-v1.{}", blake3::hash(identity.as_bytes()).to_hex())
    }

    /// Resolves the exact executable path currently configured for `provider`.
    fn provider_executable(&self, provider: Provider) -> Result<PathBuf, ExecuteError> {
        match provider {
            Provider::Codex => Ok(self.codex_executable.clone()),
            Provider::Claude => Ok(self.claude_executable.clone()),
            Provider::Deterministic => Err(ExecuteError::Internal),
        }
    }

    fn world_manifest(
        &self,
        world: &CompiledWorld,
        name: &str,
        visibility: Visibility,
    ) -> Result<TrustedManifest, ExecuteError> {
        let id = world.evaluator_artifact(name).ok_or_else(|| {
            ExecuteError::Rejected(format!(
                "World does not declare the {name} evaluator artifact required for paired evaluation"
            ))
        })?;
        let id = ArtifactId::parse(id.to_owned()).map_err(|_| ExecuteError::Internal)?;
        let bytes = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .artifacts
            .get(&id)
            .map_err(|_| ExecuteError::Internal)?;
        TrustedManifest::from_canonical_bytes(&bytes, visibility)
            .map_err(|_| ExecuteError::Internal)
    }

    #[cfg(feature = "test-support")]
    fn open_evaluator(
        &self,
        evaluator_id: &str,
        limits: WorkerLimits,
    ) -> Result<IsolatedEvaluator, ExecuteError> {
        let root = self.data_dir.join("evaluator-runs");
        IsolatedEvaluator::open_with_policy(
            root,
            &self.evaluator_executable,
            evaluator_id,
            IsolationPolicy::unconfined_for_testing(),
            limits,
        )
        .map_err(|_| ExecuteError::Internal)
    }

    #[cfg(not(feature = "test-support"))]
    fn open_evaluator(
        &self,
        evaluator_id: &str,
        limits: WorkerLimits,
    ) -> Result<IsolatedEvaluator, ExecuteError> {
        let root = self.data_dir.join("evaluator-runs");
        IsolatedEvaluator::open(
            root,
            &self.evaluator_executable,
            evaluator_id,
            self.protected_evaluator_paths(),
            limits,
        )
        .map_err(|_| ExecuteError::Internal)
    }

    fn protected_runtime_paths(&self) -> Vec<PathBuf> {
        [
            "events.sqlite3",
            "blobs",
            "operator.token",
            "runtime-producer.key",
        ]
        .into_iter()
        .map(|name| self.data_dir.join(name))
        .chain(std::iter::once(self.evaluator_executable.clone()))
        .collect()
    }

    #[cfg(not(feature = "test-support"))]
    fn protected_evaluator_paths(&self) -> Vec<PathBuf> {
        [
            "events.sqlite3",
            "blobs",
            "operator.token",
            "runtime-producer.key",
            "sandboxes",
        ]
        .into_iter()
        .map(|name| self.data_dir.join(name))
        .chain(std::iter::once(self.source_repository.clone()))
        .collect()
    }

    fn paired_revision(&self, evaluation_id: &str) -> Result<String, ExecuteError> {
        let prefix = paired_run_prefix(evaluation_id);
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let mut pinned = None;
        for event in history.iter().filter(|event| {
            event.event_type == "run.result_recorded"
                && event.event_id.starts_with(&format!("result:{prefix}-"))
        }) {
            let receipt = RunResultReceipt::parse_from_event(event, &self.run_result_verifier)
                .map_err(|_| ExecuteError::Internal)?;
            if pinned
                .as_ref()
                .is_some_and(|revision| revision != &receipt.source_revision)
            {
                return Err(ExecuteError::Internal);
            }
            pinned = Some(receipt.source_revision);
        }
        pinned.map_or_else(|| resolve_source_revision(&self.source_repository), Ok)
    }

    fn reopen_storage(&mut self) -> Result<(), ExecuteError> {
        let ledger = (self.open_ledger)().map_err(|_| ExecuteError::Internal)?;
        let artifacts = (self.open_artifacts)().map_err(|_| ExecuteError::Internal)?;
        self.storage = Some(CanonicalStorage { ledger, artifacts });
        Ok(())
    }

    fn registered_world_cost_ceiling(&self, world_id: &str) -> Result<u64, ExecuteError> {
        Ok(self
            .registered_world(world_id)?
            .evaluation_policy()
            .maximum_cost_microusd())
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    fn run_with_context(
        &mut self,
        run_id: &str,
        genome: &GenomeRecord,
        task_id: &str,
        prompt: &str,
        seed: u64,
        wall_millis: u64,
        maximum_output_bytes: u64,
        maximum_cost_microusd: u64,
    ) -> Result<ResponseData, ExecuteError> {
        let budget =
            validated_evaluation_budget(wall_millis, maximum_output_bytes, maximum_cost_microusd)?;
        let selected_provider = self.selected_run_provider(&genome.genome_id)?;
        let instruction = if selected_provider.is_none() {
            self.reference_instruction(&genome.genome_id)?
        } else {
            None
        };
        let worker = instruction
            .is_some()
            .then(|| self.pin_reference_worker())
            .transpose()?;
        let environment_id = if let Some(provider) = selected_provider {
            provider_execution_environment(provider)
        } else {
            worker
                .as_ref()
                .map_or_else(reference_environment_id, |worker| {
                    Self::reference_execution_environment(worker)
                })
        };
        let capabilities = match selected_provider {
            // A real provider runs with the Genome's own compiled authority
            // ceiling; the reference-worker smoke test deliberately ignores it.
            Some(_) => self.compiled_genome(&genome.genome_id)?.authority(),
            None => CapabilitySet::new(false, false),
        };
        let experiment = ExperimentContext::new(task_id, prompt.as_bytes(), seed, environment_id)
            .map_err(|_| ExecuteError::Invalid("evaluation context is invalid"))?;
        let mut spec = RunSpec::new_for_experiment(
            run_id,
            &genome.genome_id,
            &genome.world_id,
            &self.source_repository,
            prompt,
            capabilities,
            budget,
            experiment,
        )
        .map_err(|_| ExecuteError::Invalid("run specification is invalid"))?;
        if let Some(instruction) = instruction {
            spec = spec
                .with_reference_instruction(instruction)
                .map_err(|_| ExecuteError::Invalid("reference task input is oversized"))?;
        }
        let supervised_runtime = if instruction.is_some() {
            let worker = worker.as_ref().ok_or(ExecuteError::Internal)?;
            worker.verify()?;
            Some(
                SupervisedRuntime::deterministic_guarded(
                    candidate_isolation(self.protected_runtime_paths()),
                    &worker.executable,
                    [],
                    &self.guardian_executable,
                )
                .map_err(|_| ExecuteError::Internal)?,
            )
        } else {
            None
        };
        let provider_runtime = match selected_provider {
            Some(provider) => {
                let executable = match provider {
                    Provider::Codex => self.codex_executable.clone(),
                    Provider::Claude => self.claude_executable.clone(),
                    Provider::Deterministic => {
                        return Err(ExecuteError::Internal);
                    }
                };
                let extra_env = resolve_provider_extra_env(&self.provider_env_allowlist);
                Some(
                    SupervisedRuntime::provider_guarded(
                        candidate_isolation(self.protected_runtime_paths()),
                        provider,
                        executable,
                        &self.guardian_executable,
                        extra_env,
                    )
                    .map_err(|_| ExecuteError::Internal)?,
                )
            }
            None => None,
        };
        let manager =
            SandboxManager::open(self.data_dir.join("sandboxes"), Duration::from_secs(30))
                .map_err(|_| ExecuteError::Internal)?;
        let (sandbox, token) = manager.create(&spec).map_err(|_| ExecuteError::Internal)?;
        let sandbox = SandboxCleanupGuard::new(sandbox);
        let execution = (|| {
            let limits =
                RetentionLimits::new(10_000, 65_536).map_err(|_| ExecuteError::Internal)?;
            let storage = self.storage.take().ok_or(ExecuteError::Internal)?;
            let redaction = RedactionPolicy::new([self.token_hex.clone()]);
            let recorder = EvidenceRecorder::from_stores(
                storage.ledger,
                storage.artifacts,
                redaction.clone(),
                limits,
            );
            let (execution, recorder) = if let Some(runtime) = provider_runtime {
                execute_provider_runtime(
                    runtime,
                    recorder,
                    &spec,
                    sandbox.sandbox()?,
                    &token,
                    run_id,
                    &redaction,
                )
            } else if let Some(runtime) = supervised_runtime {
                execute_candidate_runtime(
                    runtime,
                    recorder,
                    &spec,
                    sandbox.sandbox()?,
                    &token,
                    run_id,
                )
            } else {
                execute_reference_runtime(recorder, &spec, sandbox.sandbox()?, &token, run_id)
            };
            let worker_integrity = worker.as_ref().map_or(Ok(()), |worker| worker.verify());
            let (ledger, artifacts) = recorder.into_stores();
            let execution = execution.and_then(|output| {
                worker_integrity?;
                persist_reference_output(&artifacts, run_id, genome, spec.source_revision(), output)
            });
            self.storage = Some(CanonicalStorage { ledger, artifacts });
            execution
        })();
        sandbox.cleanup()?;
        let response = execution?;
        if let Some(worker) = &worker {
            worker.verify()?;
        }
        self.append_run_result(&spec, &response)?;
        Ok(response)
    }

    fn append_run_result(
        &mut self,
        spec: &RunSpec,
        response: &ResponseData,
    ) -> Result<(), ExecuteError> {
        let ResponseData::Run {
            run_id,
            genome_id,
            world_id,
            source_revision,
            completion_reason,
            latency_millis,
            actual_cost_microusd,
            stdout_artifact_id,
            stderr_artifact_id,
            trace_artifact_ids,
        } = response
        else {
            return Err(ExecuteError::Internal);
        };
        if run_id != spec.run_id()
            || genome_id != spec.genome_id()
            || world_id != spec.world_id()
            || source_revision != spec.source_revision()
        {
            return Err(ExecuteError::Internal);
        }
        let receipt = RunResultReceipt::from_run_spec(
            spec,
            *completion_reason,
            *latency_millis,
            *actual_cost_microusd,
            stdout_artifact_id.clone(),
            stderr_artifact_id.clone(),
            trace_artifact_ids.clone(),
        )
        .map_err(|_| ExecuteError::Internal)?;
        let timestamp = timestamp_millis().map_err(|_| ExecuteError::Internal)?;
        let event = self
            .run_result_signer
            .issue(receipt, timestamp)
            .map_err(|_| ExecuteError::Internal)?;
        let stored = self
            .storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(event)
            .map_err(|_| ExecuteError::Internal)?;
        self.state
            .apply(&stored, &self.operator_token, &self.run_result_verifier)
            .map_err(|_| ExecuteError::Internal)?;
        Ok(())
    }

    fn register_world(&mut self, path: &str) -> Result<ResponseData, ExecuteError> {
        let format = source_format(path)?;
        let source = read_source_text(path, MAX_SOURCE_FILE_BYTES)?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let compiled = compile_world(&source, format, &storage.artifacts)
            .map_err(|error| ExecuteError::Rejected(format!("World source rejected: {error}")))?;
        if let Some(existing) = self.state.registered.world(compiled.id()) {
            return Ok(ResponseData::World {
                world: existing.record().clone(),
            });
        }
        if let Some(verifier_id) = compiled.evaluator_artifact("arena.runtime_verifier") {
            let verifier_id =
                ArtifactId::parse(verifier_id.to_owned()).map_err(|_| ExecuteError::Internal)?;
            let verifier = storage
                .artifacts
                .get(&verifier_id)
                .map_err(|_| ExecuteError::Internal)?;
            if verifier != self.run_result_verifier.public_key_bytes() {
                return Err(ExecuteError::Rejected(
                    "World arena.runtime_verifier is not this daemon's runtime producer key; \
                     run `hephaestus verifier` and reference its artifact"
                        .to_owned(),
                ));
            }
        }
        let artifact = storage
            .artifacts
            .put(compiled.canonical_json())
            .map_err(|_| ExecuteError::Internal)?;
        let record = WorldRecord {
            world_id: compiled.id().to_owned(),
            name: compiled.name().to_owned(),
            artifact_id: artifact.as_str().to_owned(),
        };
        let payload = serde_json::to_vec(&record).map_err(|_| ExecuteError::Internal)?;
        storage
            .ledger
            .append(EventInput::new(
                format!("world:{}:registered", record.world_id),
                &record.world_id,
                "world.registered",
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::World { world: record })
    }

    fn register_genome(
        &mut self,
        path: &str,
        world_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        let source = read_source_text(path, MAX_SOURCE_FILE_BYTES)?;
        let world = self
            .state
            .registered
            .world(world_id)
            .map(|world| world.compiled().clone())
            .ok_or(ExecuteError::NotFound)?;
        let parents = self
            .state
            .registered
            .genomes()
            .filter(|genome| genome.record().world_id == world_id)
            .map(|genome| (genome.record().genome_id.clone(), genome.compiled().clone()))
            .collect::<BTreeMap<_, _>>();
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let compiled =
            if Path::new(path).extension().and_then(|value| value.to_str()) == Some("md") {
                compile_markdown_genome(&source, &world, &parents, &storage.artifacts)
            } else {
                compile_genome(
                    &source,
                    source_format(path)?,
                    &world,
                    &parents,
                    &storage.artifacts,
                )
            }
            .map_err(|error| ExecuteError::Rejected(format!("Genome source rejected: {error}")))?;
        if let Some(existing) = self.state.registered.genome(compiled.id()) {
            if existing.record().world_id != world_id {
                return Err(ExecuteError::Rejected(format!(
                    "Genome content is already registered under World {}",
                    existing.record().world_id
                )));
            }
            return Ok(ResponseData::Genome {
                genome: existing.record().clone(),
            });
        }
        let artifact = storage
            .artifacts
            .put(compiled.canonical_json())
            .map_err(|_| ExecuteError::Internal)?;
        let record = GenomeRecord {
            genome_id: compiled.id().to_owned(),
            name: compiled.name().to_owned(),
            world_id: world_id.to_owned(),
            artifact_id: artifact.as_str().to_owned(),
            parent_ids: compiled.parents().to_vec(),
        };
        let payload = serde_json::to_vec(&record).map_err(|_| ExecuteError::Internal)?;
        storage
            .ledger
            .append(EventInput::new(
                format!("genome:{}:registered", record.genome_id),
                &record.genome_id,
                "genome.registered",
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::Genome { genome: record })
    }

    /// Convenience wrapper over [`Self::propose_genome_from_source`] for the
    /// unchanged operator-authored hypothesis path used throughout the test
    /// suite and by any direct in-process caller.
    #[cfg(test)]
    fn propose_genome(
        &mut self,
        proposal_id: &str,
        selection_event_id: &str,
        parent_genome_id: &str,
        hypothesis: &str,
    ) -> Result<ResponseData, ExecuteError> {
        self.propose_genome_from_source(
            proposal_id,
            selection_event_id,
            parent_genome_id,
            ForgeHypothesisSource::Operator(hypothesis.to_owned()),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn propose_genome_from_source(
        &mut self,
        proposal_id: &str,
        selection_event_id: &str,
        parent_genome_id: &str,
        source: ForgeHypothesisSource,
    ) -> Result<ResponseData, ExecuteError> {
        if self.state.freeze.is_frozen() {
            return Err(ExecuteError::Invalid("evolution is frozen"));
        }
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let (selection_hash, evaluation_id, world_id) = verified_forge_source(
            &storage.artifacts,
            &history,
            &self.state.registered,
            selection_event_id,
            parent_genome_id,
        )?;
        let world = self
            .state
            .registered
            .world(&world_id)
            .ok_or(ExecuteError::Internal)?;
        let (hypothesis, analysis_binding, target_operation) = resolve_forge_hypothesis(
            &storage.artifacts,
            &history,
            &self.state.registered,
            world.compiled(),
            &evaluation_id,
            parent_genome_id,
            source,
        )?;
        let (prompt_before, before, after, prompt_after_text) = forge_prompt_mutation(
            &storage.artifacts,
            &self.state.registered,
            parent_genome_id,
            world.compiled(),
            target_operation,
        )?;
        let prompt_after = storage
            .artifacts
            .put(prompt_after_text.as_bytes())
            .map_err(|_| ExecuteError::Internal)?;
        let child_record = compile_forge_child(
            &self.state.registered,
            world.compiled(),
            &storage.artifacts,
            parent_genome_id,
            &world_id,
            proposal_id,
            prompt_after.as_str(),
        )?;
        let edge_kind = mutation_edge_kind(before.operation_name(), after.operation_name())
            .map(|kind| kind.as_str().to_owned());
        let payload = ForgeProposalPayload {
            schema_version: 1,
            proposal_id: proposal_id.to_owned(),
            selection_event_id: selection_event_id.to_owned(),
            selection_event_hash: selection_hash,
            evaluation_id,
            world_id,
            parent_genome_id: parent_genome_id.to_owned(),
            child: child_record,
            hypothesis,
            artifact_name: "agent.prompt".to_owned(),
            prompt_artifact_before: prompt_before,
            prompt_artifact_after: prompt_after.as_str().to_owned(),
            operation_before: reference_instruction_operation(before).to_owned(),
            operation_after: reference_instruction_operation(after).to_owned(),
            analysis_binding,
            catalog_version: Some(MUTATION_CATALOG_VERSION),
            mutation_kind: edge_kind,
        };
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) = existing_forge_response(&history, &payload)? {
            return Ok(existing);
        }
        if self
            .state
            .registered
            .genome(&payload.child.genome_id)
            .is_some()
        {
            return Err(ExecuteError::Rejected(
                "derived child identity is already registered".to_owned(),
            ));
        }
        let event = storage
            .ledger
            .append(EventInput::new(
                forge_event_id(proposal_id),
                forge_aggregate_id(proposal_id),
                "forge.proposed",
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::ForgeProposal {
            proposal: Box::new(forge_proposal_record(payload, &event)),
        })
    }

    fn assess_genome(
        &mut self,
        assessment_id: &str,
        proposal_id: &str,
        selection_event_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return Err(ExecuteError::Busy);
        }
        validate_job_id(assessment_id)
            .map_err(|_| ExecuteError::Invalid("assessment_id is invalid"))?;
        validate_job_id(proposal_id)
            .map_err(|_| ExecuteError::Invalid("proposal_id is invalid"))?;
        if selection_event_id.trim().is_empty() {
            return Err(ExecuteError::Invalid("selection_event_id is required"));
        }

        let storage = self.storage.as_mut().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_history(&storage.artifacts, &history, &self.state.registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_assessment_history(&storage.artifacts, &history, &self.state.registered)
            .map_err(|_| ExecuteError::Internal)?;

        if let Some(existing) = existing_forge_assessment_response(
            &history,
            assessment_id,
            proposal_id,
            selection_event_id,
        )? {
            return Ok(existing);
        }
        let payload = forge_assessment_payload(
            &storage.artifacts,
            &EventIndex::build(&history),
            &self.state.registered,
            assessment_id,
            proposal_id,
            selection_event_id,
        )?;

        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        let event = storage
            .ledger
            .append(EventInput::new(
                forge_assessment_event_id(assessment_id),
                forge_aggregate_id(proposal_id),
                "forge.assessed",
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        Ok(ResponseData::ForgeAssessment {
            assessment: Box::new(forge_assessment_record(payload, &event)),
        })
    }

    fn genome_prompt(&self, genome_id: &str) -> Result<ResponseData, ExecuteError> {
        let genome = self
            .state
            .registered
            .genome(genome_id)
            .ok_or(ExecuteError::NotFound)?;
        let prompt_artifact = genome
            .compiled()
            .artifact_id("agent.prompt")
            .ok_or(ExecuteError::NotFound)?;
        let prompt_id =
            ArtifactId::parse(prompt_artifact.to_owned()).map_err(|_| ExecuteError::Internal)?;
        let bytes = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .artifacts
            .get(&prompt_id)
            .map_err(|_| ExecuteError::Internal)?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_SOURCE_FILE_BYTES) {
            return Err(ExecuteError::Internal);
        }
        let prompt = String::from_utf8(bytes).map_err(|_| ExecuteError::Internal)?;
        if prompt.trim().is_empty() {
            return Err(ExecuteError::Internal);
        }
        Ok(ResponseData::GenomePrompt {
            genome_id: genome_id.to_owned(),
            prompt,
        })
    }

    fn put_manifest(&mut self, path: &str) -> Result<ResponseData, ExecuteError> {
        let bytes = read_bounded_file(path, MAX_SOURCE_FILE_BYTES)?;
        let canonical = TrustedManifest::from_source_json(&bytes)
            .and_then(|manifest| manifest.canonical_bytes())
            .map_err(|error| ExecuteError::Rejected(format!("manifest rejected: {error}")))?;
        let artifact = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .artifacts
            .put(&canonical)
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Artifact {
            artifact_id: artifact.as_str().to_owned(),
            bytes: canonical.len() as u64,
        })
    }

    fn put_artifact(&mut self, path: &str) -> Result<ResponseData, ExecuteError> {
        let bytes = read_bounded_file(path, MAX_ARTIFACT_FILE_BYTES)?;
        let artifact = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .artifacts
            .put(&bytes)
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Artifact {
            artifact_id: artifact.as_str().to_owned(),
            bytes: bytes.len() as u64,
        })
    }

    fn verifier_show(&mut self) -> Result<ResponseData, ExecuteError> {
        let public_key = self.run_result_verifier.public_key_bytes();
        let artifact = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .artifacts
            .put(&public_key)
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Verifier {
            artifact_id: artifact.as_str().to_owned(),
            public_key_hex: hex_encode(&public_key),
        })
    }

    /// Recomputes the whole projection from the ledger. `history` is
    /// replayed and hash-chain-verified exactly once here, and every
    /// `verify_*_history_with` call below re-verifies its evidence against
    /// that same `history` and the daemon's already-open
    /// `storage.artifacts`, instead of reopening the event store and
    /// re-replaying the ledger once per evidence event: even a fully cold
    /// verification pass is linear in history size, not quadratic (see
    /// `TECH_DEBT.md` TD-16). `self.evidence_cache` additionally skips
    /// events this process has already verified in an earlier refresh, so
    /// the total cost of many refreshes over a growing history stays linear
    /// in the number of *new* events rather than the square of history
    /// length; every event is still fully re-verified, against a freshly
    /// hash-chain-verified `history`, the first time it is seen or whenever
    /// its recorded content changes. Startup (`Self::open*`) and explicit
    /// `replay` use a fresh, empty cache and verify everything.
    fn refresh_projection(&mut self) -> Result<(), ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let registered = RegisteredObjects::replay(&history, &storage.artifacts)
            .map_err(|_| ExecuteError::Internal)?;
        verify_selection_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_forge_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_forge_assessment_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_invariant_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_cluster_history(&storage.artifacts, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_champion_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_gene_bank_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_evolution_history(&history, &registered).map_err(|_| ExecuteError::Internal)?;
        verify_meta_evolution_history(&history).map_err(|_| ExecuteError::Internal)?;
        verify_drift_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_canary_history_with(
            &storage.artifacts,
            &history,
            &registered,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_drift_adaptation_history(&history).map_err(|_| ExecuteError::Internal)?;
        let state = ControlState::from_events(
            &history,
            registered,
            &self.operator_token,
            &self.run_result_verifier,
        )
        .map_err(|_| ExecuteError::Internal)?;
        ControlState::verify_artifacts_with(
            &history,
            &storage.artifacts,
            &self.run_result_verifier,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_arena_evaluation_records_with(
            &storage.artifacts,
            &history,
            &state,
            &mut self.evidence_cache,
        )
        .map_err(|_| ExecuteError::Internal)?;
        self.state = state;
        Ok(())
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
            let target = match &cluster.suggested_mutation {
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
            Ok((
                cluster.hypothesis.clone(),
                Some(ForgeAnalysisBinding {
                    analysis_id,
                    analysis_event_id: event_id,
                    analysis_event_hash,
                    cluster_index,
                    cluster_signature: cluster.signature.clone(),
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InvariantEventEnvelope {
    schema_version: u16,
    evaluation_id: String,
    world_id: String,
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
        if envelope.schema_version != 1
            || envelope.evaluation_id != evaluation_id
            || envelope.world_id != world_id
            || envelope.receipt_artifact_id.trim().is_empty()
        {
            return Err(ControlError::Projection(
                "canonical invariant event identity is invalid".to_owned(),
            ));
        }
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

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClusterEventEnvelope {
    schema_version: u16,
    analysis_id: String,
    evaluation_id: String,
    world_id: String,
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
        let (analysis_id, evaluation_id, world_id) =
            cluster_event_references(event).map_err(|_| {
                ControlError::Projection("canonical cluster event envelope is invalid".to_owned())
            })?;
        let envelope: ClusterEventEnvelope =
            serde_json::from_slice(&event.payload).map_err(|_| {
                ControlError::Projection("canonical cluster event envelope is invalid".to_owned())
            })?;
        if envelope.schema_version != 1
            || envelope.analysis_id != analysis_id
            || envelope.evaluation_id != evaluation_id
            || envelope.world_id != world_id
            || envelope.analysis_artifact_id.trim().is_empty()
        {
            return Err(ControlError::Projection(
                "canonical cluster event identity is invalid".to_owned(),
            ));
        }
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
    if let Command::MetaShow { meta_run_id } = command
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
        data_dir,
        guardian,
        protected_paths,
        worker,
        cancel,
    } = launch;
    worker
        .verify()
        .map_err(|_| "reference worker identity check failed".to_owned())?;
    let manager = SandboxManager::open(data_dir.join("sandboxes"), Duration::from_secs(30))
        .map_err(|_| "sandbox could not be opened".to_owned())?;
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
    data_dir: PathBuf,
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
        data_dir,
        guardian,
        protected_paths,
        provider,
        executable,
        extra_env,
        redaction,
        cancel,
    } = launch;
    let manager = SandboxManager::open(data_dir.join("sandboxes"), Duration::from_secs(30))
        .map_err(|_| "sandbox could not be opened".to_owned())?;
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
    for (index, trial) in trials.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            outcome = Err("paired evaluation was cancelled".to_owned());
            break;
        }
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
                    data_dir: data_dir.clone(),
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
        } else {
            execute_async_reference(
                AsyncReferenceLaunch {
                    data_dir: data_dir.clone(),
                    guardian: guardian.clone(),
                    protected_paths: protected_paths.clone(),
                    worker: Arc::clone(&worker),
                    cancel: Arc::clone(&cancel),
                },
                &trial.spec,
                evidence.clone(),
                initial_sequence,
            )
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
        if event.event_type == "job.terminal" {
            let receipt = self.run_results.get(&record.run_id);
            let matches = receipt.is_some_and(|receipt| receipt_matches_job(receipt, record));
            if record.state == JobState::Succeeded
                && (!matches
                    || !self.completed_runs.contains(&record.run_id)
                    || !receipt.is_some_and(|receipt| {
                        receipt.completion_reason == RunCompletionReason::Success
                    }))
            {
                return Err(ControlError::Projection(
                    "successful job terminal lacks matching signed result and lifecycle completion"
                        .to_owned(),
                ));
            }
        }
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
    BOOTSTRAP_ALGORITHM, META_EVALUATION_EVENT_TYPE, META_STRATEGY_EVENT_TYPE, RESAMPLES,
    champion_after, descendant_verdict, meta_evaluation_aggregate_id, meta_evaluation_event_id,
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
    adaptation_assessment_id, adaptation_canary_id, adaptation_diagnostic_evaluation_id,
    adaptation_projection, adaptation_proposal_id, adaptation_shadow_evaluation_id,
    adaptation_stage_evaluation_id, finished_event_input, started_event_input,
    verify_drift_adaptation_history,
};
