use std::{
    collections::{BTreeMap, BTreeSet},
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
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use hephaestus_arena::{
    ArenaError, CLUSTER_EVENT_PREFIX, ClusterAnalysis, ClusterEvent, EvaluationBinding,
    EvaluationInputs, EvaluationSources, EvaluationStores, InvariantEvent, InvariantReceipt,
    IsolatedEvaluator, OperatorClusterAnalysis, OperatorInvariantCheck, ReceiptContext,
    ScoredEvaluation, SelectionEvent, SelectionReceipt, SuggestedMutation, TrialPlan,
    TrustedManifest, Visibility, check_failure_clusters, check_reference_output_invariants,
    cluster_event_references, evaluate_and_record_scored, invariant_event_references,
    load_failure_clusters, load_operator_evaluation, load_recorded_evaluation,
    load_reference_output_invariants, prepare_evaluation, select_and_record,
    selection_event_references, verify_cluster_event, verify_reference_output_invariant_event,
    verify_selection_event,
};
use hephaestus_core::authority::{CapabilitySet, FreezeState, OperatorToken};
use hephaestus_experience::{
    EvidenceRecorder, EvidenceRequest, RUN_RESULT_SCHEMA_VERSION, RecordedRuntime, RedactionPolicy,
    RetentionLimits, RunBudgetReceipt, RunResultReceipt, RunResultSigner, RunResultVerifier,
    TraceKind, TraceReceipt,
};
use hephaestus_genome::{
    CompiledWorld, RegisteredObjects, RegistrationError, SourceFormat, compile_genome,
    compile_markdown_genome, compile_world,
};
use hephaestus_ledger::{ArtifactId, ArtifactStore, EventInput, EventStore, StoredEvent};
use hephaestus_runtime::{
    Budget, CapabilityToken, CompletionReason, DeterministicRuntime, ExperimentContext,
    IsolationPolicy, ReferenceInstruction, RunSpec, RunStatus, RuntimeAdapter, Sandbox,
    SandboxManager, SupervisedRuntime, WorkerLimits,
};
use serde::{Deserialize, Serialize};
use tempfile::{Builder as TempDirBuilder, TempDir};

use crate::protocol::{ArenaJobPhase, ArenaJobProgress};
use crate::{
    API_VERSION, ApiErrorCode, ApiRequest, ApiResponse, ChampionTransitionPayload, Command,
    ControlError, DenialEntry, DenialKind, EvaluationEventRecord, EvaluationForgeSummary,
    EvaluationInvariantSummary, EvaluationListEntry, EvaluationRecord, EvaluationSelectionSummary,
    ForgeAnalysisBinding, ForgeAnalysisRecord, ForgeAssessmentEventRecord, ForgeAssessmentOutcome,
    ForgeAssessmentPayload, ForgeAssessmentRecord, ForgeProposalEventRecord, ForgeProposalPayload,
    ForgeProposalRecord, GenomeRecord, InvariantRecord, JobProgress, JobRecord, JobState,
    JobTerminal, MAX_LIST_LIMIT, ResponseData, RunCompletionReason, RunListEntry,
    SelectionEventRecord, SelectionRecord, WorldRecord,
};

// A 1 MiB Markdown body can expand to six JSON bytes per escaped control
// character. Keep enough bounded headroom for that representation.
const MAX_REQUEST_BYTES: usize = 7 * 1_048_576;
#[cfg(feature = "test-support")]
const TEST_ARENA_OVERALL_WALL_ENV: &str = "HEPHAESTUS_TEST_ARENA_OVERALL_WALL_MILLIS";
const CONTROL_AGGREGATE: &str = "hephaestus-control";
const OPERATOR_ACTOR: &str = "local-operator";
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
    data_dir: PathBuf,
    source_repository: PathBuf,
    evaluator_executable: PathBuf,
    reference_worker_executable: PathBuf,
    reference_worker_digest: String,
    guardian_executable: PathBuf,
    token_hex: String,
    operator_token: OperatorToken,
    run_result_signer: RunResultSigner,
    run_result_verifier: RunResultVerifier,
    storage: Option<CanonicalStorage>,
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
}

struct CanonicalStorage {
    ledger: EventStore,
    artifacts: ArtifactStore,
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
    environment_id: String,
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
}

#[derive(Clone)]
struct ArenaTrialSpec {
    genome: GenomeRecord,
    spec: RunSpec,
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
    cancel: Arc<std::sync::atomic::AtomicBool>,
    trials: Vec<ArenaTrialSpec>,
    evidence: hephaestus_experience::ChannelEvidenceSink,
    messages: mpsc::SyncSender<ArenaWorkerMessage>,
    initial_sequence: u64,
    job_id: String,
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
    pub fn open_with_repository_evaluator_and_reference_worker(
        data_dir: impl Into<PathBuf>,
        source_repository: impl Into<PathBuf>,
        evaluator_executable: impl Into<PathBuf>,
        reference_worker_executable: impl Into<PathBuf>,
    ) -> Result<Self, ControlError> {
        let data_dir = data_dir.into();
        let source_repository = validate_source_repository(&source_repository.into())?;
        let evaluator_executable = evaluator_executable.into();
        let reference_worker_executable = reference_worker_executable.into();
        let reference_worker_digest = executable_digest(&reference_worker_executable)?;
        let guardian_executable = default_process_guardian_executable()?;
        prepare_private_directory(&data_dir)?;
        let lock = take_writer_lock(&data_dir.join("daemon.lock"))?;
        let (token_hex, token_bytes) = load_or_create_token(&data_dir.join("operator.token"))?;
        let operator_token = OperatorToken::from_bytes(token_bytes);
        let database_path = data_dir.join("events.sqlite3");
        prepare_private_file(&database_path)?;
        let mut ledger = EventStore::open(&database_path)?;
        let artifacts_path = data_dir.join("blobs");
        prepare_private_directory(&artifacts_path)?;
        let artifacts = ArtifactStore::open(artifacts_path)?;
        let history = ledger.replay_verified()?;
        reject_legacy_run_result_history(&history)?;
        let registered =
            RegisteredObjects::replay(&history, &artifacts).map_err(registration_control_error)?;
        verify_selection_history(&data_dir, &history, &registered)?;
        verify_forge_history(&data_dir, &history, &registered)?;
        verify_forge_assessment_history(&data_dir, &history, &registered)?;
        verify_invariant_history(&data_dir, &history, &registered)?;
        verify_cluster_history(&data_dir, &history, &registered)?;
        verify_champion_history(&data_dir, &history, &registered)?;
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
        verify_arena_evaluation_records(&data_dir, &state)?;
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
            &mut state,
            &data_dir,
            &operator_token,
            &run_result_verifier,
        )?;
        Ok(Self {
            data_dir,
            source_repository,
            evaluator_executable,
            reference_worker_executable,
            reference_worker_digest,
            guardian_executable,
            token_hex,
            operator_token,
            run_result_signer,
            run_result_verifier,
            storage: Some(CanonicalStorage { ledger, artifacts }),
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
        })
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
        while !self.shutdown_requested {
            self.service_async_messages()?;
            if let Ok(queued) = request_receiver.try_recv() {
                let response = self.handle(queued.request);
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
            } => self.submit_arena_job(&evaluation_id, &parent_genome_id, &candidate_genome_id),
            Command::ArenaSelect { evaluation_id } => self.select_arena_evaluation(&evaluation_id),
            Command::ArenaInvariants { evaluation_id } => {
                self.check_arena_invariants(&evaluation_id)
            }
            command @ (Command::ChampionSeed { .. }
            | Command::ChampionPromote { .. }
            | Command::ChampionRollback { .. }) => self.champion_transition_command(command),
            Command::ChampionShow { world_id } => self.champion_show(&world_id),
            Command::Replay => self.replay_response(),
            Command::RunList { limit } => self.run_list(limit),
            Command::EvaluationList { limit } => self.evaluation_list(limit),
            Command::DenialList { limit } => self.denial_list(limit),
            Command::DaemonStop => self.request_daemon_stop(),
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
        verify_champion_history(&self.data_dir, &history, &self.state.registered)
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(existing) = existing_champion_transition(&history, transition_id, request)? {
            return Ok(ResponseData::ChampionTransition {
                transition: Box::new(existing),
            });
        }
        let payload = champion_transition_payload(
            &self.data_dir,
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
                | Command::WorldRegister { .. }
                | Command::ManifestPut { .. }
                | Command::ArtifactPut { .. }
                | Command::Replay
        );
        if (self.active_job.is_some() || self.active_arena_job.is_some()) && storage_taking {
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
        let worker = Arc::new(self.pin_reference_worker()?);
        let run_id = job_run_id(job_id);
        let spec = self.async_reference_spec(&run_id, &genome, &worker)?;
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
        let worker_copy = Arc::clone(&worker);
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
        let task = move || {
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
        verify_selection_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_assessment_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_invariant_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_cluster_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_champion_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        let replayed = ControlState::from_events(
            &history,
            registered,
            &self.operator_token,
            &self.run_result_verifier,
        )
        .map_err(|_| ExecuteError::Internal)?;
        verify_arena_evaluation_records(&self.data_dir, &replayed)
            .map_err(|_| ExecuteError::Internal)?;
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
        self.run_with_context(
            run_id,
            genome,
            "repository-inventory-v1",
            prompt,
            0,
            10_000,
            1_048_576,
            0,
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
        let per_trial_wall = PAIRED_EVALUATION_WALL_MILLIS;
        let budget =
            validated_evaluation_budget(per_trial_wall, PAIRED_EVALUATION_OUTPUT_BYTES, 0)?;
        let trial_budget = RunBudgetReceipt {
            wall_millis: per_trial_wall,
            maximum_output_bytes: PAIRED_EVALUATION_OUTPUT_BYTES,
            maximum_cost_microusd: 0,
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
            maximum_cost_microusd: 0,
        };
        let evaluator_limits = WorkerLimits::new(
            Duration::from_millis(PAIRED_EVALUATION_WALL_MILLIS),
            16 * 1024 * 1024,
            128 * 1024,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let evaluator = Arc::new(self.open_evaluator(&evaluator_id, evaluator_limits)?);
        let worker = Arc::new(self.pin_reference_worker()?);
        let environment_id = Self::reference_execution_environment(&worker);
        let revision = self.paired_revision(evaluation_id)?;
        let binding = EvaluationBinding::new(
            world.id(),
            PAIRED_EVALUATION_SEED,
            &environment_id,
            &evaluator_id,
            trial_budget,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let mut trial_specs = Vec::with_capacity(total_trials);
        let mut parent_plan = Vec::with_capacity(tasks.len());
        let mut candidate_plan = Vec::with_capacity(tasks.len());
        for (role, genome, plan) in [
            ("parent", &parent_genome, &mut parent_plan),
            ("candidate", &candidate_genome, &mut candidate_plan),
        ] {
            for (index, task) in tasks.iter().enumerate() {
                let run_id = paired_run_id(evaluation_id, role, index);
                let event_id = format!("result:{run_id}");
                let experiment = ExperimentContext::new(
                    &task.task_id,
                    task.input.as_bytes(),
                    PAIRED_EVALUATION_SEED,
                    &environment_id,
                )
                .map_err(|_| ExecuteError::Internal)?;
                let instruction = self
                    .reference_instruction(&genome.genome_id)?
                    .unwrap_or(ReferenceInstruction::Identity);
                let spec = RunSpec::new_for_experiment_at_revision(
                    &run_id,
                    &genome.genome_id,
                    &genome.world_id,
                    &self.source_repository,
                    &revision,
                    &task.input,
                    CapabilitySet::new(false, false),
                    budget,
                    experiment,
                )
                .map_err(|_| ExecuteError::Internal)?
                .with_reference_instruction(instruction)
                .map_err(|_| ExecuteError::Internal)?;
                plan.push((task.task_id.clone(), event_id));
                trial_specs.push(ArenaTrialSpec {
                    genome: genome.clone(),
                    spec,
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
                &environment_id,
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
            environment_id: environment_id.clone(),
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
            cancel: Arc::clone(&cancel),
            trials: trial_specs,
            evidence,
            messages: message_sender,
            initial_sequence: self.state.event_count,
            job_id: evaluation_id.to_owned(),
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
        drop(operator.into_stores());
        let world = self
            .state
            .registered
            .world(&world_id)
            .map(|registered| registered.compiled().clone())
            .ok_or(ExecuteError::NotFound)?;

        // An existing deterministic analysis is a verified idempotent retry.
        match load_failure_clusters(
            self.open_arena_stores()?,
            analysis_id,
            evaluation_id,
            &world,
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
        EvaluationStores::open(
            self.data_dir.join("events.sqlite3"),
            self.data_dir.join("blobs"),
        )
        .map_err(|_| ExecuteError::Internal)
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

    fn pin_reference_worker(&self) -> Result<PinnedReferenceWorker, ExecuteError> {
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
        let ledger = EventStore::open(self.data_dir.join("events.sqlite3"))
            .map_err(|_| ExecuteError::Internal)?;
        let artifacts =
            ArtifactStore::open(self.data_dir.join("blobs")).map_err(|_| ExecuteError::Internal)?;
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
        let instruction = self.reference_instruction(&genome.genome_id)?;
        let worker = instruction
            .is_some()
            .then(|| self.pin_reference_worker())
            .transpose()?;
        let environment_id = worker
            .as_ref()
            .map_or_else(reference_environment_id, |worker| {
                Self::reference_execution_environment(worker)
            });
        let experiment = ExperimentContext::new(task_id, prompt.as_bytes(), seed, environment_id)
            .map_err(|_| ExecuteError::Invalid("evaluation context is invalid"))?;
        let mut spec = RunSpec::new_for_experiment(
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
        let manager =
            SandboxManager::open(self.data_dir.join("sandboxes"), Duration::from_secs(30))
                .map_err(|_| ExecuteError::Internal)?;
        let (sandbox, token) = manager.create(&spec).map_err(|_| ExecuteError::Internal)?;
        let sandbox = SandboxCleanupGuard::new(sandbox);
        let execution = (|| {
            let limits =
                RetentionLimits::new(10_000, 65_536).map_err(|_| ExecuteError::Internal)?;
            let storage = self.storage.take().ok_or(ExecuteError::Internal)?;
            let recorder = EvidenceRecorder::from_stores(
                storage.ledger,
                storage.artifacts,
                RedactionPolicy::new([self.token_hex.clone()]),
                limits,
            );
            let (execution, recorder) = if let Some(runtime) = supervised_runtime {
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
            let worker_integrity = worker
                .as_ref()
                .map_or(Ok(()), PinnedReferenceWorker::verify);
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
            &self.data_dir,
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
        let (hypothesis, analysis_binding) = resolve_forge_hypothesis(
            &self.data_dir,
            &history,
            world.compiled(),
            &evaluation_id,
            parent_genome_id,
            source,
        )?;
        let (prompt_before, before, after, prompt_after_text) = forge_prompt_mutation(
            &storage.artifacts,
            &self.state.registered,
            parent_genome_id,
            &world_id,
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
        verify_forge_history(&self.data_dir, &history, &self.state.registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_assessment_history(&self.data_dir, &history, &self.state.registered)
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
            &self.data_dir,
            &history,
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

    fn refresh_projection(&mut self) -> Result<(), ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let registered = RegisteredObjects::replay(&history, &storage.artifacts)
            .map_err(|_| ExecuteError::Internal)?;
        verify_selection_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_forge_assessment_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_invariant_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_cluster_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        verify_champion_history(&self.data_dir, &history, &registered)
            .map_err(|_| ExecuteError::Internal)?;
        let state = ControlState::from_events(
            &history,
            registered,
            &self.operator_token,
            &self.run_result_verifier,
        )
        .map_err(|_| ExecuteError::Internal)?;
        ControlState::verify_artifacts(&history, &storage.artifacts, &self.run_result_verifier)
            .map_err(|_| ExecuteError::Internal)?;
        verify_arena_evaluation_records(&self.data_dir, &state)
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

fn verify_arena_evaluation_records(
    data_dir: &Path,
    state: &ControlState,
) -> Result<(), ControlError> {
    for job in state.arena_jobs.values().filter(|job| {
        job.state == JobState::Succeeded && job.terminal == Some(JobTerminal::Succeeded)
    }) {
        let stores =
            EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
                .map_err(|_| {
                    ControlError::Projection("Arena evidence stores are unavailable".into())
                })?;
        let recorded = load_recorded_evaluation(stores, &job.evaluation_id).map_err(|_| {
            ControlError::Projection("Arena terminal lacks trusted evaluation evidence".into())
        })?;
        if job.evaluation.as_ref() != Some(&evaluation_record_from_recorded(&recorded)) {
            return Err(ControlError::Projection(
                "Arena terminal differs from trusted evaluation evidence".to_owned(),
            ));
        }
    }
    Ok(())
}

fn verify_forge_history(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    let artifacts = ArtifactStore::open(data_dir.join("blobs"))?;
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
        let selection_event = history
            .iter()
            .find(|candidate| candidate.event_id == payload.selection_event_id)
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
        let stores =
            EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
                .map_err(|_| {
                    ControlError::Projection("Forge source stores are unavailable".to_owned())
                })?;
        let selected =
            verify_selection_event(stores, selection_event, world.compiled()).map_err(|_| {
                ControlError::Projection("Forge source selection is unverified".to_owned())
            })?;
        let receipt = selected.receipt().clone();
        let expected_hash = selected.event().event_hash.clone();
        drop(selected.into_stores());
        if payload.selection_event_hash != expected_hash
            || payload.evaluation_id != receipt.evaluation_id()
            || payload.world_id != receipt.world_id()
            || payload.parent_genome_id != receipt.candidate_genome_id()
        {
            return Err(ControlError::Projection(
                "Forge proposal is not bound to its selected candidate".to_owned(),
            ));
        }
        verify_forge_child(&artifacts, registered, event, &payload, world.compiled())?;
        validate_hypothesis(&payload.hypothesis)
            .map_err(|_| ControlError::Projection("Forge hypothesis is invalid".to_owned()))?;
    }
    Ok(())
}

fn verify_forge_child(
    artifacts: &ArtifactStore,
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
    artifacts: &ArtifactStore,
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
    let expected_after = match before {
        ReferenceInstruction::Identity => ReferenceInstruction::AsciiUppercase,
        ReferenceInstruction::AsciiUppercase => ReferenceInstruction::Identity,
    };
    let expected_text = mutate_reference_instruction_document(before_text, before, expected_after)
        .map_err(|()| ControlError::Protocol("Forge prompt is outside mutation scope"))?;
    if after != expected_after
        || after_text != expected_text
        || payload.operation_before != reference_instruction_operation(before)
        || payload.operation_after != reference_instruction_operation(after)
    {
        return Err(ControlError::Projection(
            "Forge prompt mutation is not the supported one-step operation flip".to_owned(),
        ));
    }
    Ok(())
}

fn verify_forge_child_compiles(
    artifacts: &ArtifactStore,
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

fn verify_forge_assessment_history(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
    {
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
            data_dir,
            history,
            registered,
            &payload.assessment_id,
            &payload.proposal_id,
            &payload.selection_event_id,
        )
        .map_err(|_| ControlError::Projection("Forge assessment evidence is invalid".to_owned()))?;
        let selection_event = history
            .iter()
            .find(|candidate| candidate.event_id == payload.selection_event_id)
            .ok_or_else(|| {
                ControlError::Projection("Forge assessment selection is missing".to_owned())
            })?;
        if selection_event.sequence >= event.sequence || payload != expected {
            return Err(ControlError::Projection(
                "Forge assessment differs from verified evidence".to_owned(),
            ));
        }
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
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
    assessment_id: &str,
    proposal_id: &str,
    selection_event_id: &str,
) -> Result<ForgeAssessmentPayload, ExecuteError> {
    let proposal_event_id = forge_event_id(proposal_id);
    let proposal_event = history
        .iter()
        .find(|event| event.event_id == proposal_event_id)
        .ok_or(ExecuteError::NotFound)?;
    let proposal = decode_forge_proposal(proposal_event).map_err(|_| ExecuteError::Internal)?;
    validate_forge_event(proposal_event, &proposal).map_err(|_| ExecuteError::Internal)?;
    if proposal.proposal_id != proposal_id {
        return Err(ExecuteError::Internal);
    }

    let selection_event = history
        .iter()
        .find(|event| event.event_id == selection_event_id)
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
    let stores = EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
        .map_err(|_| ExecuteError::Internal)?;
    let selected = verify_selection_event(stores, selection_event, world.compiled())
        .map_err(|_| ExecuteError::Internal)?;
    let receipt = selected.receipt().clone();
    let verified_selection_event_id = selected.event().event_id.clone();
    let selection_event_hash = selected.event().event_hash.clone();
    let selection_receipt_artifact_id = selected.event().receipt_artifact_id.clone();
    let selection_sequence = selected.event().sequence;
    drop(selected.into_stores());

    let evaluation_event = history
        .iter()
        .find(|event| event.event_id == receipt.evaluation_event_id())
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
    artifacts: &ArtifactStore,
    artifact_id: &str,
) -> Result<Vec<u8>, ControlError> {
    let id = ArtifactId::parse(artifact_id.to_owned())?;
    Ok(artifacts.get(&id)?)
}

fn reference_instruction_operation(instruction: ReferenceInstruction) -> &'static str {
    match instruction {
        ReferenceInstruction::Identity => "identity",
        ReferenceInstruction::AsciiUppercase => "ascii_uppercase",
    }
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
    artifact_store: &ArtifactStore,
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
    data_dir: &Path,
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
    let stores = EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
        .map_err(|_| ExecuteError::Internal)?;
    let selection = verify_selection_event(stores, event, world.compiled())
        .map_err(|_| ExecuteError::Internal)?;
    let receipt = selection.receipt().clone();
    let selection_hash = selection.event().event_hash.clone();
    drop(selection.into_stores());
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

/// Resolves the hypothesis text and, for an analysis-derived proposal, the
/// binding recorded in the proposal payload. This never proposes, mutates, or
/// promotes anything; it only reads and verifies already-recorded evidence.
fn resolve_forge_hypothesis(
    data_dir: &Path,
    history: &[StoredEvent],
    world: &CompiledWorld,
    evaluation_id: &str,
    parent_genome_id: &str,
    source: ForgeHypothesisSource,
) -> Result<(String, Option<ForgeAnalysisBinding>), ExecuteError> {
    match source {
        ForgeHypothesisSource::Operator(hypothesis) => {
            validate_hypothesis(&hypothesis)?;
            Ok((hypothesis, None))
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
            let stores =
                EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
                    .map_err(|_| ExecuteError::Internal)?;
            let verified =
                verify_cluster_event(stores, event, world).map_err(|_| ExecuteError::Internal)?;
            let analysis = verified.analysis().clone();
            let analysis_event_hash = verified.event().event_hash.clone();
            drop(verified.into_stores());
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
            if cluster.suggested_mutation != Some(SuggestedMutation::ReferenceOperationFlip) {
                return Err(ExecuteError::Rejected(
                    "the selected cluster has no supported mutation".to_owned(),
                ));
            }
            Ok((
                cluster.hypothesis.clone(),
                Some(ForgeAnalysisBinding {
                    analysis_id,
                    analysis_event_id: event_id,
                    analysis_event_hash,
                    cluster_index,
                    cluster_signature: cluster.signature.clone(),
                }),
            ))
        }
    }
}

fn forge_prompt_mutation(
    artifacts: &ArtifactStore,
    registered: &RegisteredObjects,
    parent_genome_id: &str,
    world_id: &str,
) -> Result<(String, ReferenceInstruction, ReferenceInstruction, String), ExecuteError> {
    let parent = registered
        .genome(parent_genome_id)
        .filter(|genome| genome.record().world_id == world_id)
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
    let after = match before {
        ReferenceInstruction::Identity => ReferenceInstruction::AsciiUppercase,
        ReferenceInstruction::AsciiUppercase => ReferenceInstruction::Identity,
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

fn verify_selection_history(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    for event in history
        .iter()
        .filter(|event| event.event_type == "selection.recorded")
    {
        // The World identity in the event envelope is only a routing hint. Arena
        // independently recomputes the evaluation receipt and compares the full
        // canonical selection event against this registered World's policy.
        let (_evaluation_id, world_id) = selection_event_references(event).map_err(|_| {
            ControlError::Projection("canonical selection event is invalid".to_owned())
        })?;
        let world = registered.world(&world_id).ok_or_else(|| {
            ControlError::Projection("selection World is not registered".to_owned())
        })?;
        let stores =
            EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
                .map_err(|_| {
                    ControlError::Projection("selection stores could not be opened".to_owned())
                })?;
        let verified = verify_selection_event(stores, event, world.compiled()).map_err(|_| {
            ControlError::Projection("canonical selection receipt is invalid".to_owned())
        })?;
        drop(verified.into_stores());
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

fn verify_invariant_history(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    for event in history.iter().filter(|event| {
        event.event_type == "invariants.recorded"
            || event.event_id.starts_with("arena:invariants:")
            || event.aggregate_id.starts_with("arena:invariants:")
    }) {
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
        let stores =
            EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
                .map_err(|_| {
                    ControlError::Projection(
                        "invariant evidence stores could not be opened".to_owned(),
                    )
                })?;
        let verified = verify_reference_output_invariant_event(stores, event, world.compiled())
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
        drop(verified.into_stores());
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

fn verify_cluster_history(
    data_dir: &Path,
    history: &[StoredEvent],
    registered: &RegisteredObjects,
) -> Result<(), ControlError> {
    for event in history.iter().filter(|event| {
        event.event_type == "forge.clustered"
            || event.event_id.starts_with(CLUSTER_EVENT_PREFIX)
            || event.aggregate_id.starts_with(CLUSTER_EVENT_PREFIX)
    }) {
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
        let stores =
            EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
                .map_err(|_| {
                    ControlError::Projection(
                        "cluster evidence stores could not be opened".to_owned(),
                    )
                })?;
        let verified = verify_cluster_event(stores, event, world.compiled()).map_err(|_| {
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
        drop(verified.into_stores());
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
    | Command::DenialList { limit } = command
        && (*limit == 0 || *limit > MAX_LIST_LIMIT)
    {
        return Err(ExecuteError::Invalid("limit must be between 1 and 200"));
    }
    require_champion_fields(command)
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

fn execute_async_arena_trials(launch: AsyncArenaTrialLaunch) {
    let AsyncArenaTrialLaunch {
        data_dir,
        guardian,
        protected_paths,
        worker,
        cancel,
        trials,
        evidence,
        messages,
        mut initial_sequence,
        job_id,
    } = launch;
    let mut outcome = Ok(());
    for (index, trial) in trials.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            outcome = Err("paired evaluation was cancelled".to_owned());
            break;
        }
        let output = execute_async_reference(
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
        );
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
        })
    })();
    let (_, recorder) = runtime.into_parts();
    (execution, recorder)
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
    Ok(directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    )))
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
    artifacts: &ArtifactStore,
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
        actual_cost_microusd: 0,
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
        let expected_genome =
            if trial_index < usize::try_from(job.parent_trial_count).unwrap_or(usize::MAX) {
                &job.parent_genome_id
            } else {
                &job.candidate_genome_id
            };
        if receipt.genome_id != *expected_genome
            || receipt.world_id != job.world_id
            || receipt.source_revision != job.source_revision
            || receipt.seed != job.seed
            || receipt.environment_id != job.environment_id
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
        let environment_digest = record
            .environment_id
            .strip_prefix("reference-v1.")
            .ok_or_else(|| ControlError::Projection("job environment is invalid".to_owned()))?;
        ArtifactId::parse(environment_digest.to_owned())?;
        let genome = self
            .registered
            .genome(&record.genome_id)
            .ok_or_else(|| ControlError::Projection("job Genome is unregistered".to_owned()))?;
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
                    maximum_cost_microusd: 0,
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
        artifacts: &ArtifactStore,
        run_result_verifier: &RunResultVerifier,
    ) -> Result<(), ControlError> {
        for event in history {
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
        }
        Ok(())
    }
}

fn recover_unfinished_jobs(
    ledger: &mut EventStore,
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
    ledger: &mut EventStore,
    state: &mut ControlState,
    data_dir: &Path,
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
            let stores =
                EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs"))
                    .map_err(|_| {
                        ControlError::Projection("Arena evidence stores are unavailable".into())
                    })?;
            let recorded = load_recorded_evaluation(stores, &job.evaluation_id).map_err(|_| {
                ControlError::Projection("Arena recovery receipt failed verification".to_owned())
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
    let environment_digest = record
        .environment_id
        .strip_prefix("reference-v1.")
        .ok_or_else(|| ControlError::Projection("Arena environment is invalid".to_owned()))?;
    ArtifactId::parse(environment_digest.to_owned())?;
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
        Command::Replay => "control.replay",
        Command::RunList { .. } => "control.run_list",
        Command::EvaluationList { .. } => "control.evaluation_list",
        Command::DenialList { .. } => "control.denial_list",
        Command::DaemonStop => "control.daemon_stop",
    }
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
    artifacts: &ArtifactStore,
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
};
