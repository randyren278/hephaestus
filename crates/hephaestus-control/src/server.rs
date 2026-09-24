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
    ArenaError, EvaluationBinding, EvaluationInputs, EvaluationSources, EvaluationStores,
    IsolatedEvaluator, ReceiptContext, ScoredEvaluation, SelectionEvent, SelectionReceipt,
    TrialPlan, TrustedManifest, Visibility, evaluate_and_record_scored, load_operator_evaluation,
    load_recorded_evaluation, prepare_evaluation, select_and_record, selection_event_references,
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
    API_VERSION, ApiErrorCode, ApiRequest, ApiResponse, Command, ControlError,
    EvaluationEventRecord, EvaluationRecord, GenomeRecord, JobProgress, JobRecord, JobState,
    JobTerminal, ResponseData, RunCompletionReason, SelectionEventRecord, SelectionRecord,
    WorldRecord,
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
            Command::Replay => self.replay_response(),
            Command::DaemonStop => self.request_daemon_stop(),
        }
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
                | Command::GenomeRegister { .. }
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
        let spawn_result = thread::Builder::new()
            .name(format!(
                "hephaestus-job-{}",
                &blake3::hash(job_id.as_bytes()).to_hex()[..8]
            ))
            .spawn(move || {
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
                let _ignored = result_sender.send(AsyncJobResult {
                    job_id: thread_job_id,
                    output,
                });
                drop(genome_copy);
            });
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
            active.overall_timed_out = true;
            active.cancel.store(true, Ordering::Release);
            let mut record = active.record.clone();
            record.state = JobState::CancellationRequested;
            record
        };
        self.append_arena_job_record(&record).map_err(|_| {
            ControlError::Projection("Arena deadline could not be persisted".to_owned())
        })?;
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
        thread::Builder::new()
            .name(format!(
                "hephaestus-score-{}",
                &blake3::hash(job_id.as_bytes()).to_hex()[..8]
            ))
            .spawn(move || {
                let result = prepared
                    .score_guarded(&evaluator, &guardian, cancel)
                    .map_err(|_| "protected evaluator failed".to_owned());
                let _ignored = sender.send(ArenaWorkerMessage::Scoring { job_id, result });
            })
            .map_err(|_| {
                ControlError::Projection("Arena scorer could not be started".to_owned())
            })?;
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
                JobTerminal::Cancelled
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
        let spawn = thread::Builder::new()
            .name(format!(
                "hephaestus-arena-{}",
                &blake3::hash(evaluation_id.as_bytes()).to_hex()[..8]
            ))
            .spawn(move || execute_async_arena_trials(launch));
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
    Ok(())
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
        Command::Replay => "control.replay",
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
mod tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    use hephaestus_experience::{Provenance, TraceKind, TraceReceipt};
    use hephaestus_genome::{SourceFormat, compile_world};
    use hephaestus_ledger::{EventInput, EventStore};
    use tempfile::tempdir;

    use super::*;

    #[cfg(feature = "test-support")]
    #[test]
    fn test_arena_wall_override_only_lowers_the_admitted_bound() {
        assert_eq!(test_overall_wall(50_000, None), Ok(50_000));
        assert_eq!(test_overall_wall(50_000, Some("1000")), Ok(1_000));
        assert!(test_overall_wall(50_000, Some("50001")).is_err());
        assert!(test_overall_wall(50_000, Some("0")).is_err());
        assert!(test_overall_wall(50_000, Some("invalid")).is_err());
    }

    #[test]
    fn progress_phase_names_cover_each_persisted_trace_kind() {
        let kinds = [
            (TraceKind::LifecycleStarted, "started"),
            (TraceKind::LifecycleResumed, "resumed"),
            (TraceKind::LifecycleCompleted, "completed"),
            (TraceKind::ToolCalled, "tool_called"),
            (TraceKind::ToolResult, "tool_result"),
            (TraceKind::ContextComposed, "context_composed"),
            (TraceKind::MemoryRetrieved, "memory_retrieved"),
            (TraceKind::SubagentSpawned, "subagent_spawned"),
            (TraceKind::FileRead, "file_read"),
            (TraceKind::FileChanged, "file_changed"),
            (TraceKind::TestExecuted, "test_executed"),
            (TraceKind::CapabilityDenied, "capability_denied"),
            (TraceKind::CostObserved, "cost_observed"),
            (TraceKind::CheckpointCreated, "checkpoint_created"),
            (TraceKind::Error, "error"),
            (TraceKind::Retry, "retry"),
            (TraceKind::ModelResponse, "model_response"),
        ];
        for (kind, expected) in kinds {
            assert_eq!(trace_phase(kind), expected);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn command_audit_types_and_source_extensions_are_stable() {
        let commands = [
            (Command::Status, "control.status"),
            (Command::Freeze, "control.freeze"),
            (Command::Unfreeze, "control.unfreeze"),
            (Command::KillAll, "control.kill_all"),
            (
                Command::GenomeShow {
                    genome_id: "g".into(),
                },
                "control.genome_show",
            ),
            (
                Command::GenomePrompt {
                    genome_id: "g".into(),
                },
                "control.genome_prompt",
            ),
            (Command::GenomeList, "control.genome_list"),
            (
                Command::GenomeRegister {
                    path: "a.md".into(),
                    world_id: "w".into(),
                },
                "control.genome_register",
            ),
            (
                Command::WorldShow {
                    world_id: "w".into(),
                },
                "control.world_show",
            ),
            (Command::WorldList, "control.world_list"),
            (
                Command::WorldRegister {
                    path: "a.json".into(),
                },
                "control.world_register",
            ),
            (
                Command::ManifestPut {
                    path: "a.json".into(),
                },
                "control.manifest_put",
            ),
            (
                Command::ArtifactPut {
                    path: "a.bin".into(),
                },
                "control.artifact_put",
            ),
            (Command::VerifierShow, "control.verifier_show"),
            (
                Command::RunSubmit {
                    job_id: "j".into(),
                    genome_id: "g".into(),
                },
                "control.run_submit",
            ),
            (
                Command::JobStatus { job_id: "j".into() },
                "control.job_status",
            ),
            (Command::JobKill { job_id: "j".into() }, "control.job_kill"),
            (
                Command::RunReference {
                    genome_id: "g".into(),
                },
                "control.run_reference",
            ),
            (
                Command::RunEvaluation {
                    genome_id: "g".into(),
                    task_id: "t".into(),
                    input: "i".into(),
                    seed: 1,
                    wall_millis: 1,
                    maximum_output_bytes: 1,
                    maximum_cost_microusd: 0,
                },
                "control.run_evaluation",
            ),
            (
                Command::EvaluatePair {
                    evaluation_id: "e".into(),
                    parent_genome_id: "p".into(),
                    candidate_genome_id: "c".into(),
                },
                "control.evaluate_pair",
            ),
            (
                Command::ArenaSelect {
                    evaluation_id: "e".into(),
                },
                "control.arena_select",
            ),
            (Command::Replay, "control.replay"),
            (Command::DaemonStop, "control.daemon_stop"),
        ];
        for (command, expected) in commands {
            assert_eq!(event_type(&command), expected);
            assert!(require_command_fields(&command).is_ok());
        }
        assert_eq!(source_format("world.json").unwrap(), SourceFormat::Json);
        assert_eq!(source_format("world.yaml").unwrap(), SourceFormat::Yaml);
        assert_eq!(source_format("world.yml").unwrap(), SourceFormat::Yaml);
        assert!(source_format("agent.md").is_err());
    }

    #[test]
    fn bounded_source_reader_rejects_invalid_utf8_and_oversized_files() {
        let directory = tempdir().expect("source directory");
        let source = directory.path().join("source.yaml");
        fs::write(&source, b"key: value\n").expect("write source");
        assert_eq!(
            read_source_text(source.to_str().unwrap(), 32).unwrap(),
            "key: value\n"
        );
        assert!(read_bounded_file(source.to_str().unwrap(), 2).is_err());
        fs::write(&source, [0xff]).expect("write invalid UTF-8");
        assert!(read_source_text(source.to_str().unwrap(), 32).is_err());
        assert!(read_bounded_file(directory.path().to_str().unwrap(), 32).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn bounded_socket_handler_routes_valid_requests_and_rejects_bad_or_saturated_clients() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let (server, mut client) = UnixStream::pair().expect("create local socket pair");
        let active = Arc::new(AtomicUsize::new(1));
        let handler_active = Arc::clone(&active);
        let handler = thread::spawn(move || serve_connection(server, &sender, handler_active));
        let request = ApiRequest {
            version: API_VERSION,
            request_id: "socket-request".to_owned(),
            token: "token".to_owned(),
            command: Command::Status,
        };
        client
            .write_all(&serde_json::to_vec(&request).expect("encode request"))
            .expect("write request");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("finish request frame");
        let queued = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("writer receives request");
        assert_eq!(queued.request, request);
        queued
            .reply
            .send(ApiResponse::success(
                request.request_id.clone(),
                ResponseData::Status {
                    frozen: true,
                    active_runs: 0,
                    event_count: 1,
                    genome_count: 0,
                },
            ))
            .expect("reply to socket handler");
        let mut response_bytes = Vec::new();
        client
            .read_to_end(&mut response_bytes)
            .expect("read socket response");
        handler.join().expect("join socket handler");
        assert_eq!(active.load(Ordering::Acquire), 0);
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&response_bytes)
                .expect("decode socket response")
                .request_id,
            "socket-request"
        );

        let (sender, _receiver) = mpsc::sync_channel(1);
        let malformed = serve_test_connection(&sender, b"{");
        assert_eq!(
            malformed.error.expect("malformed response").code,
            ApiErrorCode::InvalidRequest
        );

        let oversized = vec![b'x'; MAX_REQUEST_BYTES * 2];
        let response = serve_test_connection(&sender, &oversized);
        assert_eq!(
            response.error.expect("oversized response"),
            crate::ApiError {
                code: ApiErrorCode::InvalidRequest,
                message: "request exceeds limit".to_owned(),
            }
        );
        let just_over_limit = vec![b'x'; MAX_REQUEST_BYTES + 1];
        let response = serve_test_connection(&sender, &just_over_limit);
        assert_eq!(
            response.error.expect("boundary response").message,
            "request exceeds limit"
        );

        let (sender, receiver) = mpsc::sync_channel(1);
        let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
        sender
            .try_send(QueuedRequest {
                request: request.clone(),
                reply: reply_sender,
            })
            .expect("saturate writer queue");
        let full = serve_test_connection(&sender, &serde_json::to_vec(&request).unwrap());
        assert_eq!(full.error.expect("busy response").code, ApiErrorCode::Busy);
        drop(receiver);
        drop(reply_receiver);

        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let stopped = serve_test_connection(&sender, &serde_json::to_vec(&request).unwrap());
        assert_eq!(
            stopped.error.expect("stopped response").code,
            ApiErrorCode::Internal
        );

        let (sender, _receiver) = mpsc::sync_channel(1);
        let timeout = serve_silent_test_connection(&sender);
        assert_eq!(
            timeout.error.expect("timeout response").code,
            ApiErrorCode::InvalidRequest
        );

        let (sender, receiver) = mpsc::sync_channel(1);
        let (server, mut client) = UnixStream::pair().expect("create disconnect socket pair");
        let active = Arc::new(AtomicUsize::new(1));
        let handler_active = Arc::clone(&active);
        let handler_sender = sender.clone();
        let handler =
            thread::spawn(move || serve_connection(server, &handler_sender, handler_active));
        client
            .write_all(&serde_json::to_vec(&request).expect("encode disconnect request"))
            .expect("write disconnect request");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("finish disconnect request");
        let queued = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("writer receives disconnect request");
        drop(client);
        queued
            .reply
            .send(ApiResponse::success(
                request.request_id,
                ResponseData::Status {
                    frozen: true,
                    active_runs: 0,
                    event_count: 1,
                    genome_count: 0,
                },
            ))
            .expect("reply remains independent of client disconnect");
        handler.join().expect("join disconnected handler");
        assert_eq!(active.load(Ordering::Acquire), 0);
    }

    fn serve_test_connection(
        sender: &mpsc::SyncSender<QueuedRequest>,
        bytes: &[u8],
    ) -> ApiResponse {
        let (server, mut client) = UnixStream::pair().expect("create local socket pair");
        let active = Arc::new(AtomicUsize::new(1));
        let handler_active = Arc::clone(&active);
        let handler_sender = sender.clone();
        let handler =
            thread::spawn(move || serve_connection(server, &handler_sender, handler_active));
        client.write_all(bytes).expect("write request bytes");
        client
            .shutdown(std::net::Shutdown::Write)
            .expect("finish request frame");
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .expect("read error response");
        handler.join().expect("join socket handler");
        assert_eq!(active.load(Ordering::Acquire), 0);
        serde_json::from_slice(&response).expect("decode error response")
    }

    fn serve_silent_test_connection(sender: &mpsc::SyncSender<QueuedRequest>) -> ApiResponse {
        let (server, mut client) = UnixStream::pair().expect("create silent socket pair");
        let active = Arc::new(AtomicUsize::new(1));
        let handler_active = Arc::clone(&active);
        let handler_sender = sender.clone();
        let handler =
            thread::spawn(move || serve_connection(server, &handler_sender, handler_active));
        let mut response = Vec::new();
        client
            .read_to_end(&mut response)
            .expect("read timeout response");
        handler.join().expect("join timed out handler");
        assert_eq!(active.load(Ordering::Acquire), 0);
        serde_json::from_slice(&response).expect("decode timeout response")
    }

    #[test]
    fn real_listener_services_status_and_shutdown_through_the_socket_writer() {
        let directory = tempdir().expect("daemon directory");
        let data_dir = directory.path().join("daemon-data");
        let plane = ControlPlane::open(&data_dir).expect("open control plane");
        let server = thread::spawn(move || plane.serve());
        let socket = data_dir.join("control.sock");
        let deadline = Instant::now() + Duration::from_secs(2);
        let token = loop {
            if let Ok(token) = fs::read_to_string(data_dir.join("operator.token"))
                && UnixStream::connect(&socket).is_ok()
            {
                break token;
            }
            assert!(Instant::now() < deadline, "listener did not start");
            thread::sleep(Duration::from_millis(2));
        };
        let mut slow_clients: Vec<_> = (0..=MAX_SOCKET_HANDLERS)
            .map(|_| UnixStream::connect(&socket).expect("connect slow client"))
            .collect();
        for client in &slow_clients {
            client
                .set_nonblocking(true)
                .expect("make slow client nonblocking");
        }
        let mut response_bytes = vec![Vec::new(); slow_clients.len()];
        let saturation_deadline = Instant::now() + Duration::from_secs(1);
        while !response_bytes.iter().any(|bytes| {
            bytes
                .windows(b"daemon connection limit reached".len())
                .any(|window| window == b"daemon connection limit reached")
        }) {
            for (client, bytes) in slow_clients.iter_mut().zip(&mut response_bytes) {
                let mut buffer = [0_u8; 256];
                if let Ok(read) = client.read(&mut buffer) {
                    bytes.extend_from_slice(&buffer[..read]);
                }
            }
            assert!(
                Instant::now() < saturation_deadline,
                "handler limit was not enforced"
            );
            thread::sleep(Duration::from_millis(1));
        }
        drop(slow_clients);
        let status = send_test_api_request(&socket, &token, "status-1", Command::Status);
        assert!(matches!(status.data, Some(ResponseData::Status { .. })));
        let stopped = send_test_api_request(&socket, &token, "stop-1", Command::DaemonStop);
        assert!(matches!(
            stopped.data,
            Some(ResponseData::Acknowledged { .. })
        ));
        server
            .join()
            .expect("join listener thread")
            .expect("serve requests");
    }

    #[test]
    fn authenticated_command_dispatch_covers_safe_read_and_control_paths() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let token = plane.token_hex.clone();
        assert_dispatch_auth_and_control_paths(&mut plane, &token);
        assert_dispatch_missing_command_paths(&mut plane, &token, &directory);
        let (world, genome, prompt) = register_dispatch_objects(&mut plane, &token, &directory);
        assert_registered_dispatch_objects(&mut plane, &token, &world, &genome, prompt);
        exercise_dispatch_job(&mut plane, &genome.genome_id);
        assert!(matches!(
            dispatch_call(&mut plane, &token, "replay", Command::Replay).data,
            Some(ResponseData::Replay { .. })
        ));
        assert!(matches!(
            dispatch_call(&mut plane, &token, "stop", Command::DaemonStop).data,
            Some(ResponseData::Acknowledged { .. })
        ));
    }

    fn dispatch_call(
        plane: &mut ControlPlane,
        token: &str,
        request_id: &str,
        command: Command,
    ) -> ApiResponse {
        plane.handle(ApiRequest {
            version: API_VERSION,
            request_id: request_id.to_owned(),
            token: token.to_owned(),
            command,
        })
    }

    fn assert_dispatch_auth_and_control_paths(plane: &mut ControlPlane, token: &str) {
        for (request, expected) in [
            (
                ApiRequest {
                    version: API_VERSION + 1,
                    request_id: "version".to_owned(),
                    token: token.to_owned(),
                    command: Command::Status,
                },
                ApiErrorCode::UnsupportedVersion,
            ),
            (
                ApiRequest {
                    version: API_VERSION,
                    request_id: "auth".to_owned(),
                    token: "wrong-token".to_owned(),
                    command: Command::Status,
                },
                ApiErrorCode::Unauthorized,
            ),
        ] {
            assert_eq!(
                plane.handle(request).error.expect("request rejection").code,
                expected
            );
        }
        assert_eq!(
            dispatch_call(plane, token, "", Command::Status)
                .error
                .expect("request ID error")
                .code,
            ApiErrorCode::InvalidRequest
        );
        for (request_id, command) in [
            ("status", Command::Status),
            ("freeze", Command::Freeze),
            ("unfreeze", Command::Unfreeze),
            ("kill-all", Command::KillAll),
            ("genomes", Command::GenomeList),
            ("worlds", Command::WorldList),
        ] {
            assert!(
                dispatch_call(plane, token, request_id, command)
                    .error
                    .is_none()
            );
        }
    }

    fn assert_dispatch_missing_command_paths(
        plane: &mut ControlPlane,
        token: &str,
        directory: &TempDir,
    ) {
        for (index, command) in [
            Command::GenomeShow {
                genome_id: "hephaestus:genome:missing".to_owned(),
            },
            Command::WorldShow {
                world_id: "hephaestus:world:missing".to_owned(),
            },
            Command::JobStatus {
                job_id: "missing".to_owned(),
            },
            Command::JobKill {
                job_id: "missing".to_owned(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                dispatch_call(plane, token, &format!("not-found-{index}"), command)
                    .error
                    .expect("not-found response")
                    .code,
                ApiErrorCode::NotFound
            );
        }
        let absent_genome = format!("hephaestus:genome:{}", "f".repeat(64));
        let missing = |name: &str| directory.path().join(name).display().to_string();
        let commands = [
            Command::GenomePrompt {
                genome_id: absent_genome.clone(),
            },
            Command::RunSubmit {
                job_id: "missing-genome-job".to_owned(),
                genome_id: absent_genome.clone(),
            },
            Command::RunReference {
                genome_id: absent_genome.clone(),
            },
            Command::RunEvaluation {
                genome_id: absent_genome.clone(),
                task_id: "task".to_owned(),
                input: "input".to_owned(),
                seed: 0,
                wall_millis: 10_000,
                maximum_output_bytes: 1_048_576,
                maximum_cost_microusd: 0,
            },
            Command::EvaluatePair {
                evaluation_id: "missing-pair".to_owned(),
                parent_genome_id: absent_genome.clone(),
                candidate_genome_id: format!("hephaestus:genome:{}", "e".repeat(64)),
            },
            Command::ArenaSelect {
                evaluation_id: "missing-evaluation".to_owned(),
            },
            Command::GenomeRegister {
                path: missing("missing.md"),
                world_id: format!("hephaestus:world:{}", "d".repeat(64)),
            },
            Command::WorldRegister {
                path: missing("missing.json"),
            },
            Command::ManifestPut {
                path: missing("missing-manifest.json"),
            },
            Command::ArtifactPut {
                path: missing("missing-artifact"),
            },
        ];
        for (index, command) in commands.into_iter().enumerate() {
            assert!(
                dispatch_call(plane, token, &format!("invalid-{index}"), command)
                    .error
                    .is_some()
            );
        }
    }

    fn register_dispatch_objects(
        plane: &mut ControlPlane,
        token: &str,
        directory: &TempDir,
    ) -> (WorldRecord, GenomeRecord, &'static str) {
        let world_path = directory.path().join("world.json");
        fs::write(
            &world_path,
            r#"{"schema_version":1,"name":"dispatch-world","laws":{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0},"authority_ceiling":{"workspace_write":false,"network":false},"mutation_scope":[],"promotion":{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500},"objectives":["correctness"],"evaluator_artifacts":{}}"#,
        )
        .expect("write World source");
        let Some(ResponseData::World { world }) = dispatch_call(
            plane,
            token,
            "register-world",
            Command::WorldRegister {
                path: world_path.display().to_string(),
            },
        )
        .data
        else {
            panic!("World registration should succeed");
        };
        let prompt =
            "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n";
        let genome_path = directory.path().join("agent.md");
        fs::write(
            &genome_path,
            format!(
                "---\nschema_version: 1\nname: dispatch-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n{prompt}"
            ),
        )
        .expect("write Genome source");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            token,
            "register-genome",
            Command::GenomeRegister {
                path: genome_path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("Genome registration should succeed");
        };
        (world, genome, prompt)
    }

    fn assert_registered_dispatch_objects(
        plane: &mut ControlPlane,
        token: &str,
        world: &WorldRecord,
        genome: &GenomeRecord,
        prompt: &str,
    ) {
        assert!(matches!(
            dispatch_call(
                plane,
                token,
                "world-show",
                Command::WorldShow {
                    world_id: world.world_id.clone(),
                }
            )
            .data,
            Some(ResponseData::World { .. })
        ));
        assert!(matches!(
            dispatch_call(
                plane,
                token,
                "genome-show",
                Command::GenomeShow {
                    genome_id: genome.genome_id.clone(),
                }
            )
            .data,
            Some(ResponseData::Genome { .. })
        ));
        assert!(matches!(
            dispatch_call(plane, token, "genome-prompt", Command::GenomePrompt {
                genome_id: genome.genome_id.clone(),
            }).data,
            Some(ResponseData::GenomePrompt { prompt: actual, .. }) if actual == prompt
        ));
        assert!(matches!(
            dispatch_call(plane, token, "verifier", Command::VerifierShow).data,
            Some(ResponseData::Verifier { .. })
        ));
    }

    fn exercise_dispatch_job(plane: &mut ControlPlane, genome_id: &str) {
        assert!(matches!(
            plane.submit_job("dispatch-run", genome_id)
                .expect("admit bounded reference job"),
            ResponseData::Job { job, .. } if job.state == JobState::Running
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        while plane.active_job.is_some() {
            plane
                .service_async_messages()
                .expect("persist worker evidence");
            assert!(Instant::now() < deadline, "direct reference job stalled");
            if plane.active_job.is_some() {
                thread::sleep(Duration::from_millis(2));
            }
        }
        assert_eq!(plane.state.jobs["dispatch-run"].state, JobState::Succeeded);
    }

    #[test]
    fn canonical_writer_rejects_evidence_without_an_admitted_job() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let (sender, receiver) = mpsc::sync_channel(1);
        let (reply, result) = mpsc::channel();
        sender
            .send(EvidenceRequest::EnsureCapacity {
                run_id: "unadmitted-run".to_owned(),
                needed: 2,
                reply,
            })
            .expect("queue unadmitted evidence");
        plane.job_evidence_receiver = Some(receiver);
        plane
            .service_async_messages()
            .expect("reject unadmitted evidence safely");
        assert!(
            result
                .recv_timeout(Duration::from_secs(1))
                .expect("writer returns evidence rejection")
                .is_err()
        );
        assert!(
            plane.storage.is_some(),
            "canonical storage remains available"
        );
    }

    #[test]
    fn canonical_writer_unavailable_cancels_and_fails_an_admitted_direct_job() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let token = plane.token_hex.clone();
        let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
        assert!(
            dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
                .error
                .is_none()
        );
        plane
            .submit_job("writer-unavailable", &genome.genome_id)
            .expect("admit direct reference job");
        let run_id = plane
            .active_job
            .as_ref()
            .expect("active direct job")
            .spec
            .run_id()
            .to_owned();
        let (sender, receiver) = mpsc::sync_channel(1);
        let (reply, result) = mpsc::channel();
        sender
            .send(EvidenceRequest::EnsureCapacity {
                run_id,
                needed: 2,
                reply,
            })
            .expect("queue admitted evidence request");
        plane.job_evidence_receiver = Some(receiver);
        let storage = plane.storage.take().expect("canonical storage");
        plane
            .service_async_messages()
            .expect("reject evidence while canonical writer is unavailable");
        assert!(
            result
                .recv_timeout(Duration::from_secs(1))
                .expect("executor receives writer rejection")
                .is_err()
        );
        plane.storage = Some(storage);
        assert!(
            plane
                .active_job
                .as_ref()
                .expect("job remains active until executor unwinds")
                .cancel
                .load(Ordering::Acquire)
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while plane.active_job.is_some() {
            plane
                .service_async_messages()
                .expect("persist cancelled direct job terminal");
            assert!(Instant::now() < deadline, "writer cancellation stalled");
            if plane.active_job.is_some() {
                thread::sleep(Duration::from_millis(2));
            }
        }
        let terminal = &plane.state.jobs["writer-unavailable"];
        assert_eq!(terminal.state, JobState::Failed);
        assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
        assert!(matches!(
            plane.replay_response().expect("replay failed direct job"),
            ResponseData::Replay { .. }
        ));
    }

    #[test]
    fn arena_job_record_validation_rejects_plan_and_event_tampering() {
        let directory = tempdir().expect("fixture directory");
        let artifacts = ArtifactStore::open(directory.path().join("blobs"))
            .expect("open fixture artifact store");
        let artifact = |contents: &[u8]| {
            artifacts
                .put(contents)
                .expect("write fixture artifact")
                .as_str()
                .to_owned()
        };
        let evaluation_id = "validated-pair";
        let parent_genome_id = format!("hephaestus:genome:{}", "1".repeat(64));
        let candidate_genome_id = format!("hephaestus:genome:{}", "2".repeat(64));
        let world_id = format!("hephaestus:world:{}", "3".repeat(64));
        let visible_manifest_id = artifact(b"visible");
        let sealed_manifest_id = artifact(b"sealed");
        let evaluator_id = artifact(b"evaluator");
        let environment_digest = artifact(b"environment");
        let ordered_trial_run_ids = vec![
            paired_run_id(evaluation_id, "parent", 0),
            paired_run_id(evaluation_id, "candidate", 0),
        ];
        let source_revision = "4".repeat(40);
        let plan_commitment = blake3::hash(
            &serde_json::to_vec(&(
                evaluation_id,
                &parent_genome_id,
                &candidate_genome_id,
                &world_id,
                &visible_manifest_id,
                &sealed_manifest_id,
                &source_revision,
                &format!("reference-v1.{environment_digest}"),
                &ordered_trial_run_ids,
            ))
            .expect("serialize committed plan"),
        )
        .to_hex()
        .to_string();
        let record = ArenaJobRecord {
            job_id: evaluation_id.to_owned(),
            evaluation_id: evaluation_id.to_owned(),
            parent_genome_id,
            candidate_genome_id,
            world_id,
            visible_manifest_id,
            sealed_manifest_id,
            evaluator_id,
            source_revision,
            worker_digest: "5".repeat(64),
            environment_id: format!("reference-v1.{environment_digest}"),
            seed: PAIRED_EVALUATION_SEED,
            trial_budget: RunBudgetReceipt {
                wall_millis: 10_000,
                maximum_output_bytes: 1_048_576,
                maximum_cost_microusd: 0,
            },
            overall_budget: RunBudgetReceipt {
                wall_millis: 50_000,
                maximum_output_bytes: 4_194_304,
                maximum_cost_microusd: 0,
            },
            ordered_trial_run_ids,
            parent_trial_count: 1,
            total_trials: 2,
            plan_commitment,
            caller_id: "control-daemon".to_owned(),
            receipt_timestamp_millis: 1,
            completed_trials: 0,
            phase: ArenaJobPhase::Preparing,
            state: JobState::Admitted,
            terminal: None,
            evaluation: None,
        };
        let event_for = |record: &ArenaJobRecord| StoredEvent {
            sequence: 1,
            event_id: format!("arena-job:{}:admitted", record.evaluation_id),
            aggregate_id: format!("arena-job:{}", record.evaluation_id),
            event_type: "arena.job.admitted".to_owned(),
            actor: RUNTIME_ACTOR.to_owned(),
            timestamp_millis: 1,
            payload: serde_json::to_vec(record).expect("serialize Arena job"),
            previous_hash: [0; 32],
            hash: [0; 32],
        };

        validate_arena_job_record(&event_for(&record), &record)
            .expect("canonical admitted plan validates");

        let mut wrong_order = record.clone();
        wrong_order.ordered_trial_run_ids.swap(0, 1);
        assert!(validate_arena_job_record(&event_for(&wrong_order), &wrong_order).is_err());

        let mut wrong_commitment = record.clone();
        wrong_commitment.plan_commitment = "6".repeat(64);
        assert!(
            validate_arena_job_record(&event_for(&wrong_commitment), &wrong_commitment).is_err()
        );

        let mut wrong_event = event_for(&record);
        wrong_event.actor = "operator".to_owned();
        assert!(validate_arena_job_record(&wrong_event, &record).is_err());
    }

    #[test]
    fn late_arena_worker_messages_are_ignored_or_rejected_after_job_closes() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let (sender, receiver) = mpsc::sync_channel(2);
        let (reply, response) = mpsc::channel();
        sender
            .send(ArenaWorkerMessage::Trial {
                job_id: "closed-pair".to_owned(),
                index: 0,
                output: Err("late trial output".to_owned()),
                reply,
            })
            .expect("queue late trial");
        plane.arena_message_receiver = Some(receiver);
        plane
            .service_arena_message()
            .expect("reject a late trial without failing the daemon");
        assert_eq!(
            response
                .recv_timeout(Duration::from_secs(1))
                .expect("late worker receives rejection")
                .expect_err("closed Arena job cannot accept a trial"),
            "paired job is no longer active"
        );

        sender
            .send(ArenaWorkerMessage::Trials {
                job_id: "closed-pair".to_owned(),
                result: Err("late completion".to_owned()),
            })
            .expect("queue late completion");
        plane
            .service_arena_message()
            .expect("ignore a completion after job close");

        sender
            .send(ArenaWorkerMessage::Scoring {
                job_id: "closed-pair".to_owned(),
                result: Err("late score".to_owned()),
            })
            .expect("queue late score");
        assert!(plane.service_arena_message().is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn injected_arena_scoring_failure_persists_failed_terminal_and_replays() {
        let directory = tempdir().expect("fixture directory");
        let data_dir = directory.path().join("data");
        let repository = directory.path().join("repository");
        fs::create_dir_all(&repository).expect("create source repository");
        let git = ProcessCommand::new("git")
            .args([
                "-C",
                repository.to_str().expect("repository path"),
                "init",
                "-q",
            ])
            .status()
            .expect("run git init");
        assert!(git.success(), "initialize source repository");
        fs::write(repository.join("fixture.txt"), b"Arena scoring fixture\n")
            .expect("write repository fixture");
        for args in [
            vec![
                "-C",
                repository.to_str().expect("repository path"),
                "config",
                "user.name",
                "Hephaestus Test",
            ],
            vec![
                "-C",
                repository.to_str().expect("repository path"),
                "config",
                "user.email",
                "hephaestus@example.invalid",
            ],
            vec![
                "-C",
                repository.to_str().expect("repository path"),
                "add",
                ".",
            ],
            vec![
                "-C",
                repository.to_str().expect("repository path"),
                "commit",
                "-m",
                "fixture",
                "-q",
            ],
        ] {
            assert!(
                ProcessCommand::new("git")
                    .args(args)
                    .status()
                    .expect("run git fixture command")
                    .success(),
                "prepare git fixture"
            );
        }
        let bin_directory = env::current_exe()
            .expect("locate test executable")
            .parent()
            .and_then(Path::parent)
            .expect("locate Cargo binary directory")
            .to_owned();
        let cargo_evaluator = bin_directory.join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert!(
            cargo_evaluator.is_file(),
            "Cargo evaluator binary missing: {cargo_evaluator:?}"
        );
        let evaluator = directory.path().join("fixture-evaluator");
        fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into a private inode");
        fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
            .expect("mark fixture evaluator executable");
        let worker = bin_directory.join(format!(
            "hephaestus-reference-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
        assert!(worker.is_file(), "Cargo worker binary missing: {worker:?}");
        let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
            &data_dir,
            &repository,
            &evaluator,
            &worker,
        )
        .expect("open control plane");
        let token = plane.token_hex.clone();
        let (world, parent, candidate) =
            register_dispatch_arena_objects(&mut plane, &token, &directory);
        assert!(
            dispatch_call(&mut plane, &token, "unfreeze-arena", Command::Unfreeze)
                .error
                .is_none()
        );
        let registered_world = plane
            .registered_world(&world.world_id)
            .expect("registered Arena World");
        let evaluator_id = registered_world
            .evaluator_artifact("arena.evaluator")
            .expect("World evaluator artifact");
        plane
            .open_evaluator(
                evaluator_id,
                WorkerLimits::new(
                    Duration::from_millis(PAIRED_EVALUATION_WALL_MILLIS),
                    16 * 1024 * 1024,
                    128 * 1024,
                )
                .expect("evaluator limits"),
            )
            .expect("preflight evaluator identity and sandbox");
        plane
            .pin_reference_worker()
            .expect("preflight reference worker snapshot");
        plane
            .paired_revision("admission-preflight")
            .expect("preflight pinned Git revision");
        assert!(matches!(
            plane
                .submit_arena_job("channel-drop", &parent.genome_id, &candidate.genome_id)
                .expect("admit channel-drop job"),
            ResponseData::ArenaJob { job } if job.state == JobState::Running
        ));
        let (trial_reply, trial_response) = mpsc::channel();
        plane
            .arena_message_sender
            .as_ref()
            .expect("Arena worker channel")
            .send(ArenaWorkerMessage::Trial {
                job_id: "channel-drop".to_owned(),
                index: 1,
                output: Err("out of order fixture trial".to_owned()),
                reply: trial_reply,
            })
            .expect("queue out-of-order trial");
        plane
            .service_arena_message()
            .expect("reject out-of-order Arena trial");
        assert_eq!(
            trial_response
                .recv_timeout(Duration::from_secs(1))
                .expect("worker receives order rejection")
                .expect_err("out-of-order trial must fail closed"),
            "paired trial arrived outside admitted order"
        );
        plane
            .active_arena_job
            .as_ref()
            .expect("active channel-drop job")
            .cancel
            .store(true, Ordering::Release);
        let (closed_sender, closed_receiver) = mpsc::sync_channel(1);
        drop(closed_sender);
        plane.arena_message_receiver = Some(closed_receiver);
        plane
            .service_arena_message()
            .expect("record unexpected worker disconnect");
        assert_eq!(
            plane.state.arena_jobs["channel-drop"].terminal,
            Some(JobTerminal::Interrupted)
        );
        assert!(matches!(
            plane
                .submit_arena_job("scoring-failure", &parent.genome_id, &candidate.genome_id)
                .expect("admit Arena job"),
            ResponseData::ArenaJob { job } if job.state == JobState::Running
        ));
        let cancel = Arc::clone(
            &plane
                .active_arena_job
                .as_ref()
                .expect("active Arena job")
                .cancel,
        );
        plane
            .arena_message_sender
            .as_ref()
            .expect("Arena worker channel")
            .send(ArenaWorkerMessage::Trials {
                job_id: "other-pair".to_owned(),
                result: Ok(()),
            })
            .expect("queue mismatched worker completion");
        assert!(plane.service_arena_message().is_err());

        // Exercise the scorer completion boundary with a genuine admitted job;
        // the injected failure represents the worker's `Scoring::Err` message.
        plane
            .finish_arena_scoring("scoring-failure", Err("fixture scoring failure".to_owned()))
            .expect("record scorer failure as a terminal state");
        cancel.store(true, Ordering::Release);
        let failed = plane
            .state
            .arena_jobs
            .get("scoring-failure")
            .expect("failed Arena job remains projected");
        assert_eq!(failed.state, JobState::Failed);
        assert_eq!(failed.terminal, Some(JobTerminal::Failed));
        assert!(failed.evaluation.is_none());
        let history = plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify failed-job history");
        assert!(history.iter().any(|event| {
            event.event_id == "arena-job:scoring-failure:terminal"
                && event.event_type == "arena.job.terminal"
        }));
        assert!(!history.iter().any(|event| {
            event.event_id == "arena:evaluation:scoring-failure:recorded"
                && event.event_type == "evaluation.recorded"
        }));
        assert!(matches!(
            plane
                .replay_response()
                .expect("replay failed Arena terminal"),
            ResponseData::Replay { .. }
        ));
        assert_eq!(world.world_id, failed.world_id);

        assert!(matches!(
            plane
                .submit_arena_job("cancelled-scoring", &parent.genome_id, &candidate.genome_id)
                .expect("admit cancellation job"),
            ResponseData::ArenaJob { job } if job.state == JobState::Running
        ));
        plane
            .kill_job("cancelled-scoring")
            .expect("request Arena cancellation");
        plane
            .finish_arena_scoring(
                "cancelled-scoring",
                Err("scorer completed after cancellation".to_owned()),
            )
            .expect("cancellation wins over late scorer failure");
        let cancelled = &plane.state.arena_jobs["cancelled-scoring"];
        assert_eq!(cancelled.state, JobState::Interrupted);
        assert_eq!(cancelled.terminal, Some(JobTerminal::Cancelled));
        assert!(cancelled.evaluation.is_none());
        assert!(matches!(
            plane
                .replay_response()
                .expect("replay cancelled Arena terminal"),
            ResponseData::Replay { .. }
        ));

        assert!(matches!(
            plane
                .submit_arena_job("scoring-timeout", &parent.genome_id, &candidate.genome_id)
                .expect("admit scoring-timeout job"),
            ResponseData::ArenaJob { job } if job.state == JobState::Running
        ));
        plane
            .active_arena_job
            .as_mut()
            .expect("active scoring-timeout job")
            .overall_deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock supports one millisecond lookback");
        plane
            .finish_arena_scoring(
                "scoring-timeout",
                Err("scorer completed after the overall deadline".to_owned()),
            )
            .expect("persist deadline terminal after late scorer result");
        let timed_out = &plane.state.arena_jobs["scoring-timeout"];
        assert_eq!(timed_out.state, JobState::Failed);
        assert_eq!(timed_out.terminal, Some(JobTerminal::Failed));
        assert!(timed_out.evaluation.is_none());
        assert!(matches!(
            plane
                .replay_response()
                .expect("replay timed-out Arena terminal"),
            ResponseData::Replay { .. }
        ));

        assert!(matches!(
            plane
                .submit_arena_job("scoring-success", &parent.genome_id, &candidate.genome_id)
                .expect("admit successful Arena job"),
            ResponseData::ArenaJob { job } if job.state == JobState::Running
        ));
        assert!(plane.start_arena_scoring().is_err());
        assert!(
            plane
                .finish_arena_scoring("other-pair", Err("wrong identity".to_owned()))
                .is_err()
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while plane.active_arena_job.is_some() {
            plane
                .service_async_messages()
                .expect("persist Arena trials and scoring result");
            assert!(
                Instant::now() < deadline,
                "successful Arena scoring did not complete"
            );
            if plane.active_arena_job.is_some() {
                thread::sleep(Duration::from_millis(2));
            }
        }
        let succeeded = &plane.state.arena_jobs["scoring-success"];
        assert_eq!(succeeded.state, JobState::Succeeded);
        assert_eq!(succeeded.terminal, Some(JobTerminal::Succeeded));
        assert_eq!(succeeded.completed_trials, succeeded.total_trials);
        assert!(succeeded.evaluation.is_some());
        assert!(matches!(
            plane
                .replay_response()
                .expect("replay successful Arena commit"),
            ResponseData::Replay { .. }
        ));

        assert!(matches!(
            plane
                .submit_arena_job("scoring-commit-failure", &parent.genome_id, &candidate.genome_id)
                .expect("admit scoring-commit-failure job"),
            ResponseData::ArenaJob { job } if job.state == JobState::Running
        ));
        let deadline = Instant::now() + Duration::from_secs(20);
        while plane
            .active_arena_job
            .as_ref()
            .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
        {
            plane
                .service_async_messages()
                .expect("persist trials before scorer completion");
            assert!(
                Instant::now() < deadline,
                "Arena trials did not reach scoring"
            );
            if plane
                .active_arena_job
                .as_ref()
                .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
            {
                thread::sleep(Duration::from_millis(2));
            }
        }
        let scored = loop {
            let message = plane
                .arena_message_receiver
                .as_ref()
                .expect("Arena scorer channel")
                .try_recv();
            match message {
                Ok(ArenaWorkerMessage::Scoring { job_id, result }) => break (job_id, result),
                Ok(_) => panic!("unexpected message after scoring began"),
                Err(mpsc::TryRecvError::Disconnected) => panic!("Arena scorer disconnected"),
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "Arena scoring did not finish");
                    thread::sleep(Duration::from_millis(2));
                }
            }
        };
        assert_eq!(scored.0, "scoring-commit-failure");
        assert!(
            scored.1.is_ok(),
            "fixture evaluator must produce valid scores"
        );
        let blobs = plane.data_dir.join("blobs");
        let saved_blobs = plane.data_dir.join("blobs-before-commit-failure");
        fs::rename(&blobs, &saved_blobs)
            .expect("temporarily hide fixture blobs to force receipt commit failure");
        plane
            .finish_arena_scoring(&scored.0, scored.1)
            .expect("persist failed commit terminal after reopening storage");
        fs::remove_dir_all(&blobs).expect("remove reopened empty blob directory");
        fs::rename(saved_blobs, blobs).expect("restore fixture blobs for verified replay");
        let commit_failed = &plane.state.arena_jobs["scoring-commit-failure"];
        assert_eq!(commit_failed.state, JobState::Failed);
        assert_eq!(commit_failed.terminal, Some(JobTerminal::Failed));
        assert!(commit_failed.evaluation.is_none());
        assert!(matches!(
            plane
                .replay_response()
                .expect("replay failed Arena receipt commit"),
            ResponseData::Replay { .. }
        ));
    }

    fn register_dispatch_arena_objects(
        plane: &mut ControlPlane,
        token: &str,
        directory: &TempDir,
    ) -> (WorldRecord, GenomeRecord, GenomeRecord) {
        let artifacts =
            ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
        let visible = TrustedManifest::new(
            "dispatch-visible",
            Visibility::Visible,
            vec![
                hephaestus_arena::TrustedTask::new("visible-task", "visible", "VISIBLE")
                    .expect("visible task"),
            ],
        )
        .expect("visible manifest");
        let sealed = TrustedManifest::new(
            "dispatch-sealed",
            Visibility::Sealed,
            vec![
                hephaestus_arena::TrustedTask::new("sealed-task", "sealed", "SEALED")
                    .expect("sealed task"),
            ],
        )
        .expect("sealed manifest");
        let visible_id = artifacts
            .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
            .expect("store visible manifest");
        let sealed_id = artifacts
            .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
            .expect("store sealed manifest");
        let evaluator = env::current_exe()
            .expect("locate test executable")
            .parent()
            .and_then(Path::parent)
            .expect("locate Cargo binary directory")
            .join(format!(
                "hephaestus-reference-evaluator{}",
                std::env::consts::EXE_SUFFIX
            ));
        let evaluator_id = artifacts
            .put(&fs::read(evaluator).expect("read reference evaluator"))
            .expect("store evaluator identity");
        let verifier_id = artifacts
            .put(&plane.run_result_verifier.public_key_bytes())
            .expect("store result verifier");
        drop(artifacts);
        let world_path = directory.path().join("arena-world.json");
        fs::write(
            &world_path,
            format!(
                r#"{{"schema_version":1,"name":"dispatch-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
                visible_id.as_str(),
                sealed_id.as_str(),
                evaluator_id.as_str(),
                verifier_id.as_str(),
            ),
        )
        .expect("write Arena World");
        let Some(ResponseData::World { world }) = dispatch_call(
            plane,
            token,
            "arena-world",
            Command::WorldRegister {
                path: world_path.display().to_string(),
            },
        )
        .data
        else {
            panic!("Arena World registration should succeed");
        };
        let register_genome = |plane: &mut ControlPlane, token: &str, name: &str, parents: &str| {
            let path = directory.path().join(format!("{name}.md"));
            fs::write(
                &path,
                format!(
                    "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
                ),
            )
            .expect("write Genome source");
            let Some(ResponseData::Genome { genome }) = dispatch_call(
                plane,
                token,
                name,
                Command::GenomeRegister {
                    path: path.display().to_string(),
                    world_id: world.world_id.clone(),
                },
            )
            .data
            else {
                panic!("Arena Genome registration should succeed");
            };
            genome
        };
        let parent = register_genome(plane, token, "arena-parent", "[]");
        let candidate = register_genome(
            plane,
            token,
            "arena-candidate",
            &format!("[\"{}\"]", parent.genome_id),
        );
        (world, parent, candidate)
    }

    #[test]
    fn terminal_job_kill_is_idempotent_and_selection_errors_map_to_safe_api_states() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let job = JobRecord {
            job_id: "completed".to_owned(),
            genome_id: format!("hephaestus:genome:{}", "1".repeat(64)),
            run_id: "async-completed".to_owned(),
            source_revision: "2".repeat(40),
            world_id: format!("hephaestus:world:{}", "3".repeat(64)),
            task_id: "repository-inventory-v1".to_owned(),
            input_commitment: "4".repeat(64),
            seed: 0,
            environment_id: format!("reference-v1.{}", "5".repeat(64)),
            budget: RunBudgetReceipt {
                wall_millis: 10_000,
                maximum_output_bytes: 1_048_576,
                maximum_cost_microusd: 0,
            },
            state: JobState::Succeeded,
            terminal: Some(JobTerminal::Succeeded),
        };
        plane.state.jobs.insert(job.job_id.clone(), job);
        assert!(matches!(
            plane.job_status("missing"),
            Err(ExecuteError::NotFound)
        ));
        assert!(matches!(
            plane.kill_job("missing"),
            Err(ExecuteError::NotFound)
        ));
        assert!(matches!(
            plane.kill_job("completed").expect("idempotent terminal kill"),
            ResponseData::Job { job, .. } if job.terminal == Some(JobTerminal::Succeeded)
        ));
        assert!(matches!(
            map_selection_error(&ArenaError::UnknownEvaluation("missing".to_owned())),
            ExecuteError::NotFound
        ));
        assert!(matches!(
            map_selection_error(&ArenaError::UnsupportedSelectionConfidence(10_000)),
            ExecuteError::Rejected(_)
        ));
        assert!(matches!(
            map_selection_error(&ArenaError::BootstrapWorkExceeded),
            ExecuteError::Rejected(_)
        ));
        assert!(matches!(
            map_selection_error(&ArenaError::UnsupportedEvaluator),
            ExecuteError::Internal
        ));
    }

    fn send_test_api_request(
        socket_path: &Path,
        token: &str,
        request_id: &str,
        command: Command,
    ) -> ApiResponse {
        let mut stream = UnixStream::connect(socket_path).expect("connect to control socket");
        let request = ApiRequest {
            version: API_VERSION,
            request_id: request_id.to_owned(),
            token: token.to_owned(),
            command,
        };
        stream
            .write_all(&serde_json::to_vec(&request).expect("encode request"))
            .expect("write request");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("finish request");
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).expect("read response");
        serde_json::from_slice(&bytes).expect("decode response")
    }

    #[test]
    fn projection_identifiers_text_and_trace_artifact_selection_fail_closed() {
        let hash = "a".repeat(64);
        assert_eq!(
            validate_content_id(&format!("hephaestus:genome:{hash}"), "genome").unwrap(),
            hash
        );
        assert!(validate_content_id("foreign:genome:abc", "genome").is_err());
        assert!(validate_content_id("hephaestus:genome:abc", "genome").is_err());
        assert!(require_projection_text("run-1", "run_id").is_ok());
        assert!(require_projection_text(" \t", "run_id").is_err());

        let make_trace = |run_id: &str, event_id: &str, artifact_id: &str| TraceReceipt {
            schema_version: 1,
            event_id: event_id.to_owned(),
            provenance: Provenance::new(
                run_id,
                format!("hephaestus:genome:{}", "1".repeat(64)),
                format!("hephaestus:world:{}", "2".repeat(64)),
            )
            .unwrap(),
            kind: TraceKind::LifecycleStarted,
            artifact_id: artifact_id.to_owned(),
            redacted_fields: 0,
        };
        let selected = make_trace("run-1", "trace-1", &"3".repeat(64));
        let unrelated = make_trace("run-2", "trace-2", &"4".repeat(64));
        let history = [
            stored_event(
                1,
                "trace.recorded",
                "run:run-1",
                "experience-plane",
                &serde_json::to_vec(&selected).unwrap(),
            ),
            stored_event(
                2,
                "trace.recorded",
                "run:run-2",
                "experience-plane",
                &serde_json::to_vec(&unrelated).unwrap(),
            ),
            stored_event(
                3,
                "trace.recorded",
                "run:run-1",
                "experience-plane",
                b"invalid",
            ),
            stored_event(4, "other.event", "run:run-1", "test", b"{}"),
        ];
        assert_eq!(
            trace_artifacts_for_run(&history[..2], "run-1").unwrap(),
            vec!["3".repeat(64)]
        );
        assert!(trace_artifacts_for_run(&history, "run-1").is_err());

        let receipt = TraceReceipt {
            schema_version: 1,
            event_id: "trace-1".to_owned(),
            provenance: Provenance::new(
                "run-1",
                format!("hephaestus:genome:{}", "1".repeat(64)),
                format!("hephaestus:world:{}", "2".repeat(64)),
            )
            .unwrap(),
            kind: TraceKind::LifecycleStarted,
            artifact_id: "6".repeat(64),
            redacted_fields: 0,
        };
        let mut trace_event =
            stored_event(1, "trace.recorded", "run:run-1", "experience-plane", b"{}");
        trace_event.event_id = receipt.event_id.clone();
        assert!(validate_trace_receipt(&trace_event, &receipt).is_ok());
        trace_event.actor = "runtime-plane".to_owned();
        assert!(validate_trace_receipt(&trace_event, &receipt).is_err());
        trace_event.actor = "experience-plane".to_owned();
        let mut malformed_receipt = receipt;
        malformed_receipt.artifact_id = "not-a-content-address".to_owned();
        assert!(validate_trace_receipt(&trace_event, &malformed_receipt).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn job_transition_rules_cover_admission_running_cancellation_and_terminal_edges() {
        fn record(state: JobState, terminal: Option<JobTerminal>) -> JobRecord {
            JobRecord {
                job_id: "job-1".to_owned(),
                genome_id: format!("hephaestus:genome:{}", "1".repeat(64)),
                run_id: "async-1".to_owned(),
                source_revision: "2".repeat(40),
                world_id: format!("hephaestus:world:{}", "3".repeat(64)),
                task_id: "repository-inventory-v1".to_owned(),
                input_commitment: "4".repeat(64),
                seed: 0,
                environment_id: format!("reference-v1.{}", "5".repeat(64)),
                budget: RunBudgetReceipt {
                    wall_millis: 10_000,
                    maximum_output_bytes: 1_048_576,
                    maximum_cost_microusd: 0,
                },
                state,
                terminal,
            }
        }
        fn state_with(previous: Option<JobRecord>) -> ControlState {
            let mut jobs = BTreeMap::new();
            if let Some(previous) = previous {
                jobs.insert(previous.job_id.clone(), previous);
            }
            ControlState {
                freeze: FreezeState::frozen(&OperatorToken::from_bytes([1; 32])),
                active_runs: BTreeSet::new(),
                jobs,
                arena_jobs: BTreeMap::new(),
                job_progress: BTreeMap::new(),
                evaluation_events: BTreeMap::new(),
                run_results: BTreeMap::new(),
                completed_runs: BTreeSet::new(),
                registered: RegisteredObjects::default(),
                event_count: 0,
            }
        }
        let event =
            |event_type: &str| stored_event(1, event_type, "job:job-1", RUNTIME_ACTOR, b"{}");

        assert!(
            state_with(None)
                .job_transition_is_valid(&event("job.admitted"), &record(JobState::Admitted, None))
        );
        assert!(
            state_with(Some(record(JobState::Admitted, None)))
                .job_transition_is_valid(&event("job.running"), &record(JobState::Running, None))
        );
        assert!(
            state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
                &event("job.cancellation_requested"),
                &record(JobState::CancellationRequested, None)
            )
        );
        assert!(
            state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
                &event("job.terminal"),
                &record(JobState::Failed, Some(JobTerminal::Failed))
            )
        );
        assert!(
            state_with(Some(record(JobState::CancellationRequested, None)))
                .job_transition_is_valid(
                    &event("job.terminal"),
                    &record(JobState::Interrupted, Some(JobTerminal::Cancelled))
                )
        );
        assert!(
            state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
                &event("job.terminal"),
                &record(JobState::Interrupted, Some(JobTerminal::Interrupted))
            )
        );
        assert!(
            !state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
                &event("job.terminal"),
                &record(JobState::Succeeded, Some(JobTerminal::Succeeded))
            )
        );
        assert!(
            !state_with(Some(record(JobState::Running, None)))
                .job_transition_is_valid(&event("job.unknown"), &record(JobState::Running, None))
        );
        let mut conflicting = record(JobState::Running, None);
        conflicting.world_id = format!("hephaestus:world:{}", "6".repeat(64));
        assert!(
            !state_with(Some(record(JobState::Running, None)))
                .job_transition_is_valid(&event("job.terminal"), &conflicting)
        );
    }

    #[test]
    fn command_field_validation_rejects_empty_ids_and_paths() {
        let invalid = [
            Command::GenomeShow {
                genome_id: " ".to_owned(),
            },
            Command::GenomePrompt {
                genome_id: String::new(),
            },
            Command::WorldShow {
                world_id: String::new(),
            },
            Command::WorldRegister {
                path: " ".to_owned(),
            },
            Command::GenomeRegister {
                path: "genome.md".to_owned(),
                world_id: " ".to_owned(),
            },
            Command::GenomeRegister {
                path: String::new(),
                world_id: "world".to_owned(),
            },
            Command::ArtifactPut {
                path: String::new(),
            },
            Command::RunSubmit {
                job_id: String::new(),
                genome_id: "genome".to_owned(),
            },
            Command::RunSubmit {
                job_id: "job".to_owned(),
                genome_id: String::new(),
            },
            Command::JobStatus {
                job_id: String::new(),
            },
            Command::JobKill {
                job_id: " ".to_owned(),
            },
            Command::RunReference {
                genome_id: String::new(),
            },
            Command::RunEvaluation {
                genome_id: " ".to_owned(),
                task_id: "task".to_owned(),
                input: "input".to_owned(),
                seed: 0,
                wall_millis: 1,
                maximum_output_bytes: 1,
                maximum_cost_microusd: 0,
            },
            Command::EvaluatePair {
                evaluation_id: String::new(),
                parent_genome_id: "parent".to_owned(),
                candidate_genome_id: "candidate".to_owned(),
            },
            Command::EvaluatePair {
                evaluation_id: "evaluation".to_owned(),
                parent_genome_id: " ".to_owned(),
                candidate_genome_id: "candidate".to_owned(),
            },
            Command::EvaluatePair {
                evaluation_id: "evaluation".to_owned(),
                parent_genome_id: "parent".to_owned(),
                candidate_genome_id: String::new(),
            },
            Command::ArenaSelect {
                evaluation_id: String::new(),
            },
        ];
        assert!(invalid.into_iter().all(|command| {
            matches!(
                require_command_fields(&command),
                Err(ExecuteError::Invalid(_))
            )
        }));
        assert!(require_command_fields(&Command::Status).is_ok());
    }

    #[test]
    fn reference_worker_identity_requires_an_executable_regular_file() {
        let directory = tempdir().expect("temporary directory");
        let worker = directory.path().join("worker");
        fs::write(&worker, b"worker bytes").expect("write worker");
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).expect("make executable");
        assert_eq!(
            executable_digest(&worker).expect("hash executable"),
            blake3::hash(b"worker bytes").to_hex().to_string()
        );
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o600))
            .expect("remove execute permission");
        assert!(executable_digest(&worker).is_err());
        assert!(executable_digest(directory.path()).is_err());
    }

    #[test]
    fn pinned_reference_worker_rejects_mutated_snapshot_bytes() {
        let directory = tempdir().expect("temporary directory");
        let snapshot = tempfile::Builder::new()
            .prefix("pinned-worker-")
            .tempdir_in(directory.path())
            .expect("private worker directory");
        let executable = snapshot.path().join("worker");
        fs::write(&executable, b"pinned worker").expect("write worker");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("make worker executable");
        let digest = executable_digest(&executable).expect("hash worker");
        let worker = PinnedReferenceWorker {
            directory: snapshot,
            executable: executable.clone(),
            digest,
        };
        worker.verify().expect("initial pinned worker is valid");

        fs::write(&executable, b"replaced worker").expect("replace snapshot bytes");
        assert!(matches!(
            worker.verify(),
            Err(ExecuteError::Rejected(message))
                if message == "pinned reference worker identity changed during execution"
        ));

        let private_directory = tempfile::Builder::new()
            .prefix("private-worker-")
            .tempdir_in(directory.path())
            .expect("private worker directory");
        let external_worker = directory.path().join("external-worker");
        fs::write(&external_worker, b"external worker").expect("write external worker");
        fs::set_permissions(&external_worker, fs::Permissions::from_mode(0o700))
            .expect("make external worker executable");
        let escaped = PinnedReferenceWorker {
            directory: private_directory,
            digest: executable_digest(&external_worker).expect("hash external worker"),
            executable: external_worker,
        };
        assert!(matches!(
            escaped.verify(),
            Err(ExecuteError::Rejected(message))
                if message == "pinned reference worker escaped its private directory"
        ));
    }

    #[test]
    fn admitted_job_projection_binds_the_runtime_actor_and_immutable_spec() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let token = plane.token_hex.clone();
        let (world, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
        let input = "Inventory the isolated repository without modifying it or using the network.";
        let job = JobRecord {
            job_id: "validated-job".to_owned(),
            genome_id: genome.genome_id,
            run_id: job_run_id("validated-job"),
            source_revision: "2".repeat(40),
            world_id: world.world_id,
            task_id: "repository-inventory-v1".to_owned(),
            input_commitment: blake3::hash(input.as_bytes()).to_hex().to_string(),
            seed: 0,
            environment_id: format!("reference-v1.{}", "5".repeat(64)),
            budget: RunBudgetReceipt {
                wall_millis: 10_000,
                maximum_output_bytes: 1_048_576,
                maximum_cost_microusd: 0,
            },
            state: JobState::Admitted,
            terminal: None,
        };
        let mut event = stored_event(1, "job.admitted", "job:validated-job", RUNTIME_ACTOR, b"{}");
        event.event_id = "job:validated-job:admitted".to_owned();
        assert!(plane.state.validate_job_record(&event, &job).is_ok());
        event.actor = "untrusted-actor".to_owned();
        assert!(plane.state.validate_job_record(&event, &job).is_err());
    }

    #[test]
    fn async_reference_spec_uses_read_only_offline_authority() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let token = plane.token_hex.clone();
        let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
        let worker = plane.pin_reference_worker().expect("pin reference worker");
        let spec = plane
            .async_reference_spec("authority-check", &genome, &worker)
            .expect("build direct reference spec");
        assert_eq!(spec.capabilities(), CapabilitySet::new(false, false));
    }

    #[test]
    fn daemon_stop_cancels_active_job_before_acknowledging_shutdown() {
        let directory = tempdir().expect("daemon directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let token = plane.token_hex.clone();
        let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
        assert!(
            dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
                .error
                .is_none()
        );
        plane
            .submit_job("stop-active", &genome.genome_id)
            .expect("submit active job");
        assert!(matches!(
            plane.request_daemon_stop(),
            Err(ExecuteError::Busy)
        ));
        assert!(!plane.shutdown_requested);
        assert_eq!(
            plane.state.jobs["stop-active"].state,
            JobState::CancellationRequested
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while plane.active_job.is_some() {
            plane
                .service_async_messages()
                .expect("persist cancellation");
            assert!(Instant::now() < deadline, "job cancellation stalled");
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(plane.state.jobs["stop-active"].state, JobState::Interrupted);
        assert!(plane.request_daemon_stop().is_ok());
        assert!(plane.shutdown_requested);
    }

    #[test]
    fn explicit_reference_worker_open_uses_the_supplied_executable_identity() {
        let directory = tempdir().expect("temporary directory");
        let worker = std::env::current_exe().expect("test executable");
        let plane = ControlPlane::open_with_repository_and_reference_worker(
            directory.path(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
            worker,
        )
        .expect("open with explicit worker");
        assert_eq!(plane.reference_worker_digest.len(), 64);
    }

    #[test]
    fn evaluation_worker_snapshot_survives_deployment_path_replacement() {
        let directory = tempdir().expect("temporary directory");
        let worker = directory.path().join("deployed-worker");
        fs::copy(std::env::current_exe().expect("test executable"), &worker)
            .expect("copy worker executable");
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
            .expect("make worker executable");
        let plane = ControlPlane::open_with_repository_and_reference_worker(
            directory.path().join("daemon-data"),
            Path::new(env!("CARGO_MANIFEST_DIR")),
            &worker,
        )
        .expect("open control plane");
        let pinned = plane.pin_reference_worker().expect("pin worker");
        let environment = ControlPlane::reference_execution_environment(&pinned);
        let pinned_bytes = fs::read(&pinned.executable).expect("read pinned worker");
        assert_eq!(
            fs::metadata(pinned.directory.path())
                .expect("read snapshot directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&pinned.executable)
                .expect("read snapshot executable metadata")
                .permissions()
                .mode()
                & 0o777,
            0o500
        );

        fs::write(&worker, b"replacement at deployment path").expect("replace worker path");
        assert_ne!(
            executable_digest(&worker).expect("replacement stays executable"),
            plane.reference_worker_digest
        );
        pinned.verify().expect("pinned executable remains valid");
        assert_eq!(
            fs::read(&pinned.executable).expect("read pinned worker after replacement"),
            pinned_bytes
        );
        assert_eq!(
            ControlPlane::reference_execution_environment(&pinned),
            environment
        );
        fs::set_permissions(&pinned.executable, fs::Permissions::from_mode(0o700))
            .expect("make snapshot writable for tamper test");
        fs::write(&pinned.executable, b"tampered pinned worker").expect("tamper pinned snapshot");
        assert!(pinned.verify().is_err());
    }

    #[test]
    fn authenticated_failures_are_safe_and_replay_divergence_is_detected() {
        let directory = tempdir().expect("temporary directory");
        let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
        let blank = plane.handle(ApiRequest {
            version: API_VERSION,
            request_id: String::new(),
            token: plane.token_hex.clone(),
            command: Command::Status,
        });
        assert_eq!(
            blank.error.expect("blank request error").code,
            ApiErrorCode::InvalidRequest
        );
        let missing = plane.handle(ApiRequest {
            version: API_VERSION,
            request_id: "missing".to_owned(),
            token: plane.token_hex.clone(),
            command: Command::GenomeShow {
                genome_id: format!("hephaestus:genome:{}", "1".repeat(64)),
            },
        });
        assert_eq!(
            missing.error.expect("missing Genome error").code,
            ApiErrorCode::NotFound
        );

        let mut competing = EventStore::open(directory.path().join("events.sqlite3"))
            .expect("open competing store");
        competing
            .append(EventInput::new(
                "competing-event",
                "other",
                "other.event",
                "test",
                1,
                b"{}",
            ))
            .expect("advance canonical tail");
        let internal = plane.handle(ApiRequest {
            version: API_VERSION,
            request_id: "stale-head".to_owned(),
            token: plane.token_hex.clone(),
            command: Command::Status,
        });
        assert_eq!(
            internal.error.expect("internal error").code,
            ApiErrorCode::Internal
        );

        let clean_directory = tempdir().expect("temporary directory");
        let mut clean = ControlPlane::open(clean_directory.path()).expect("open clean plane");
        clean.state.event_count = 1;
        assert!(matches!(
            clean.replay_response(),
            Err(ExecuteError::Internal)
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn projection_rejects_mismatched_commands_and_mutable_genome_metadata() {
        let token = OperatorToken::from_bytes([7; 32]);
        let run_result_verifier = RunResultSigner::from_seed([8; 32]).verifier();
        let mismatched = stored_event(
            1,
            "control.freeze",
            CONTROL_AGGREGATE,
            OPERATOR_ACTOR,
            br#"{"request_id":"mismatch","command":{"command":"unfreeze"}}"#,
        );
        assert!(matches!(
            ControlState::from_events(
                &[mismatched],
                RegisteredObjects::default(),
                &token,
                &run_result_verifier,
            ),
            Err(ControlError::Projection(_))
        ));

        let lifecycle = [
            stored_event(1, "run.started", "run", "runtime", br#"{"run_id":"r1"}"#),
            stored_event(2, "run.completed", "run", "runtime", br#"{"run_id":"r1"}"#),
        ];
        let state = ControlState::from_events(
            &lifecycle,
            RegisteredObjects::default(),
            &token,
            &run_result_verifier,
        )
        .expect("replay run lifecycle");
        assert!(state.active_runs.is_empty());

        let provenance = Provenance::new(
            "r2",
            format!("hephaestus:genome:{}", "7".repeat(64)),
            format!("hephaestus:world:{}", "8".repeat(64)),
        )
        .expect("valid provenance");
        let started = TraceReceipt {
            schema_version: 1,
            event_id: "fixture-1".to_owned(),
            provenance: provenance.clone(),
            kind: TraceKind::LifecycleStarted,
            artifact_id: "9".repeat(64),
            redacted_fields: 0,
        };
        let completed = TraceReceipt {
            schema_version: 1,
            event_id: "fixture-2".to_owned(),
            provenance,
            kind: TraceKind::LifecycleCompleted,
            artifact_id: "a".repeat(64),
            redacted_fields: 0,
        };
        let traces = [
            stored_event(
                1,
                "trace.recorded",
                "run:r2",
                "experience-plane",
                &serde_json::to_vec(&started).expect("encode started trace"),
            ),
            stored_event(
                2,
                "trace.recorded",
                "run:r2",
                "experience-plane",
                &serde_json::to_vec(&completed).expect("encode completed trace"),
            ),
        ];
        let replayed_trace_state = ControlState::from_events(
            &traces,
            RegisteredObjects::default(),
            &token,
            &run_result_verifier,
        )
        .expect("replay trace lifecycle");
        assert!(replayed_trace_state.active_runs.is_empty());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn canonical_path_token_and_identity_helpers_fail_closed() {
        assert!(data_dir_from_environment().is_ok());
        assert!(!constant_time_equal(b"short", b"different"));
        assert!(matches!(
            hex_decode("short"),
            Err(ControlError::Protocol(_))
        ));
        assert!(matches!(
            hex_decode(&"g".repeat(64)),
            Err(ControlError::Protocol(_))
        ));

        let key_directory = tempdir().expect("producer key directory");
        let missing_producer_key = key_directory.path().join("missing-producer.key");
        assert!(matches!(
            load_or_create_run_result_signer(&missing_producer_key, false),
            Err(ControlError::Protocol(_))
        ));
        let producer_key = key_directory.path().join("producer.key");
        let first_verifier = load_or_create_run_result_signer(&producer_key, true)
            .expect("create producer key")
            .verifier();
        let reopened_verifier = load_or_create_run_result_signer(&producer_key, false)
            .expect("reload producer key")
            .verifier();
        assert_eq!(first_verifier, reopened_verifier);
        fs::set_permissions(&producer_key, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            load_or_create_run_result_signer(&producer_key, false),
            Err(ControlError::Protocol(_))
        ));

        let directory = tempdir().expect("temporary directory");
        let data = directory.path().join("data");
        fs::create_dir(&data).expect("create data directory");
        assert!(matches!(
            ControlPlane::open_with_repository(&data, directory.path()),
            Err(ControlError::Protocol(_))
        ));
        let ordinary_file = directory.path().join("ordinary");
        fs::write(&ordinary_file, b"file").expect("write ordinary file");
        assert!(matches!(
            validate_source_repository(&ordinary_file),
            Err(ControlError::Protocol(_))
        ));
        assert!(matches!(
            prepare_private_directory(&ordinary_file),
            Err(ControlError::Protocol(_))
        ));
        let link = directory.path().join("link");
        symlink(directory.path(), &link).expect("create symlink");
        assert!(matches!(
            prepare_private_directory(&link),
            Err(ControlError::Protocol(_))
        ));
        assert!(matches!(
            prepare_private_file(directory.path()),
            Err(ControlError::Protocol(_))
        ));
        assert!(matches!(
            load_or_create_token(directory.path()),
            Err(ControlError::Protocol(_))
        ));
        assert!(matches!(
            remove_stale_socket(&ordinary_file),
            Err(ControlError::Protocol(_))
        ));

        let provenance = Provenance::new(
            "run",
            format!("hephaestus:genome:{}", "8".repeat(64)),
            format!("hephaestus:world:{}", "7".repeat(64)),
        )
        .expect("valid provenance");
        let receipt = TraceReceipt {
            schema_version: 1,
            event_id: "fixture-1".to_owned(),
            provenance,
            kind: TraceKind::LifecycleStarted,
            artifact_id: "9".repeat(64),
            redacted_fields: 0,
        };
        let forged_trace = stored_event(
            1,
            "trace.recorded",
            "run:run",
            "forged",
            &serde_json::to_vec(&receipt).expect("encode receipt"),
        );
        assert!(matches!(
            validate_trace_receipt(&forged_trace, &receipt),
            Err(ControlError::Projection(_))
        ));

        let result_payload = serde_json::json!({
            "schema_version": 2,
            "run_id": "run",
            "genome_id": format!("hephaestus:genome:{}", "8".repeat(64)),
            "world_id": format!("hephaestus:world:{}", "7".repeat(64)),
            "source_revision": "6".repeat(40),
            "task_id": "task",
            "input_commitment": "5".repeat(64),
            "seed": 1,
            "environment_id": "environment-v1",
            "budget": {
                "wall_millis": 10_000,
                "maximum_output_bytes": 1_048_576,
                "maximum_cost_microusd": 0
            },
            "completion_reason": "success",
            "latency_millis": 1,
            "actual_cost_microusd": 0,
            "stdout_artifact_id": "a".repeat(64),
            "stderr_artifact_id": "b".repeat(64),
            "trace_artifact_ids": ["c".repeat(64)]
        });
        let forged_result = stored_event(
            1,
            "run.result_recorded",
            "run:run",
            "forged",
            &serde_json::to_vec(&result_payload).expect("encode result"),
        );
        let run_result_verifier = RunResultSigner::from_seed([8; 32]).verifier();
        assert!(matches!(
            validate_run_result(&forged_result, &run_result_verifier),
            Err(ControlError::Projection(_))
        ));
        for reason in [
            CompletionReason::ProviderFailure,
            CompletionReason::OperatorInterrupt,
            CompletionReason::WallBudgetExceeded,
            CompletionReason::OutputBudgetExceeded,
            CompletionReason::IoFailure,
        ] {
            assert_ne!(run_completion_reason(reason), RunCompletionReason::Success);
        }
    }

    #[test]
    fn legacy_unsigned_run_results_fail_with_actionable_incompatibility() {
        let directory = tempdir().expect("legacy directory");
        let database = directory.path().join("events.sqlite3");
        let mut ledger = EventStore::open(&database).expect("open legacy ledger");
        ledger
            .append(EventInput::new(
                "result:legacy",
                "run:legacy",
                "run.result_recorded",
                "runtime-plane",
                1,
                br#"{"schema_version":1,"run_id":"legacy"}"#,
            ))
            .expect("append legacy result");
        drop(ledger);
        let Err(error) = ControlPlane::open(directory.path()) else {
            panic!("legacy startup must fail");
        };
        assert!(format!("{error}").contains("back up the data directory and reinitialize"));
        assert!(!directory.path().join("runtime-producer.key").exists());
    }

    #[test]
    fn evaluation_budget_boundaries_are_validated_before_execution() {
        assert!(validated_evaluation_budget(10_001, 1_048_576, 0).is_ok());
        assert!(
            validated_evaluation_budget(
                MAX_EVALUATION_WALL_MILLIS,
                MAX_EVALUATION_OUTPUT_BYTES,
                MAX_EVALUATION_COST_MICROUSD,
            )
            .is_ok()
        );
        assert!(
            validated_evaluation_budget(MAX_EVALUATION_WALL_MILLIS + 1, 1_048_576, 0,).is_err()
        );
        assert!(
            validated_evaluation_budget(10_000, 1_048_576, MAX_EVALUATION_COST_MICROUSD + 1,)
                .is_err()
        );
    }

    #[test]
    fn world_verifier_and_producer_key_failures_are_covered() {
        let directory = tempdir().expect("fixture directory");
        let artifacts = ArtifactStore::open(directory.path().join("blobs")).expect("artifacts");

        let non_utf8 = artifacts.put(&[0xff]).expect("non-UTF-8 artifact");
        let non_utf8_world = world_registration_event(1, "non-utf8", non_utf8.as_str());
        assert!(RegisteredObjects::replay(&[non_utf8_world], &artifacts).is_err());

        let invalid_json = artifacts.put(b"{").expect("invalid World artifact");
        let invalid_world = world_registration_event(1, "invalid", invalid_json.as_str());
        assert!(RegisteredObjects::replay(&[invalid_world], &artifacts).is_err());

        let short_key = artifacts.put(b"short verifier").expect("short verifier");
        let (short_event, _) = compiled_world_registration(&artifacts, "short", short_key.as_str());
        let short_registered =
            RegisteredObjects::replay(&[short_event], &artifacts).expect("registered short key");
        assert!(anchored_world_verifier(&short_registered, &artifacts).is_err());

        let first_signer = RunResultSigner::from_seed([21; 32]);
        let second_signer = RunResultSigner::from_seed([22; 32]);
        let first_key = artifacts
            .put(&first_signer.verifier().public_key_bytes())
            .expect("first verifier");
        let second_key = artifacts
            .put(&second_signer.verifier().public_key_bytes())
            .expect("second verifier");
        let (first_event, first_world) =
            compiled_world_registration(&artifacts, "first", first_key.as_str());
        let (second_event, _) =
            compiled_world_registration(&artifacts, "second", second_key.as_str());
        let different_registered =
            RegisteredObjects::replay(&[first_event.clone(), second_event], &artifacts)
                .expect("register Worlds with different verifier keys");
        assert!(anchored_world_verifier(&different_registered, &artifacts).is_err());

        let mut mismatched = first_world;
        mismatched.name = "forged-name".to_owned();
        let mismatched_event = stored_event(
            1,
            "world.registered",
            &mismatched.world_id,
            "test-fixture",
            &serde_json::to_vec(&mismatched).expect("mismatched World"),
        );
        assert!(RegisteredObjects::replay(&[mismatched_event], &artifacts).is_err());

        let producer_key = directory.path().join("producer.key");
        fs::write(&producer_key, [24; 32]).expect("producer key");
        fs::set_permissions(&producer_key, fs::Permissions::from_mode(0o600)).unwrap();
        let producer_link = directory.path().join("producer-link.key");
        symlink(&producer_key, &producer_link).expect("producer key link");
        assert!(load_or_create_run_result_signer(&producer_link, false).is_err());
    }

    fn compiled_world_registration(
        artifacts: &ArtifactStore,
        name: &str,
        verifier_id: &str,
    ) -> (StoredEvent, WorldRecord) {
        let source = format!(
            r#"{{"schema_version":1,"name":"{name}","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["coverage"],"evaluator_artifacts":{{"arena.runtime_verifier":"{verifier_id}"}}}}"#
        );
        let compiled =
            compile_world(&source, SourceFormat::Json, artifacts).expect("compile World");
        let artifact = artifacts
            .put(compiled.canonical_json())
            .expect("canonical World artifact");
        let record = WorldRecord {
            world_id: compiled.id().to_owned(),
            name: compiled.name().to_owned(),
            artifact_id: artifact.as_str().to_owned(),
        };
        let event = stored_event(
            1,
            "world.registered",
            &record.world_id,
            "test-fixture",
            &serde_json::to_vec(&record).expect("World record"),
        );
        (event, record)
    }

    fn world_registration_event(sequence: u64, name: &str, artifact_id: &str) -> StoredEvent {
        let record = WorldRecord {
            world_id: format!("hephaestus:world:{}", "d".repeat(64)),
            name: name.to_owned(),
            artifact_id: artifact_id.to_owned(),
        };
        stored_event(
            sequence,
            "world.registered",
            &record.world_id,
            "test-fixture",
            &serde_json::to_vec(&record).expect("World record"),
        )
    }

    fn stored_event(
        sequence: u64,
        event_type: &str,
        aggregate_id: &str,
        actor: &str,
        payload: &[u8],
    ) -> StoredEvent {
        StoredEvent {
            sequence,
            event_id: format!("fixture-{sequence}"),
            aggregate_id: aggregate_id.to_owned(),
            event_type: event_type.to_owned(),
            actor: actor.to_owned(),
            timestamp_millis: 1,
            payload: payload.to_vec(),
            previous_hash: [0; 32],
            hash: [0; 32],
        }
    }
}
