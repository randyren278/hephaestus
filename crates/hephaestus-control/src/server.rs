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
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use hephaestus_arena::{
    ArenaError, EvaluationBinding, EvaluationInputs, EvaluationStores, IsolatedEvaluator,
    ReceiptContext, SelectionEvent, SelectionReceipt, TrialPlan, TrustedManifest, Visibility,
    evaluate_and_record, load_operator_evaluation, select_and_record, selection_event_references,
    verify_selection_event,
};
use hephaestus_core::authority::{CapabilitySet, FreezeState, OperatorToken};
use hephaestus_experience::{
    EvidenceRecorder, RUN_RESULT_SCHEMA_VERSION, RecordedRuntime, RedactionPolicy, RetentionLimits,
    RunBudgetReceipt, RunResultReceipt, RunResultSigner, RunResultVerifier, TraceKind,
    TraceReceipt,
};
use hephaestus_genome::{
    CompiledWorld, RegisteredObjects, RegistrationError, SourceFormat, compile_genome,
    compile_markdown_genome, compile_world,
};
use hephaestus_ledger::{ArtifactId, ArtifactStore, EventInput, EventStore, StoredEvent};
use hephaestus_runtime::{
    Budget, CapabilityToken, CompletionReason, DeterministicRuntime, ExperimentContext,
    IsolationPolicy, RunSpec, RunStatus, RuntimeAdapter, Sandbox, SandboxManager,
    SupervisedRuntime, WorkerLimits,
};
use serde::{Deserialize, Serialize};

use crate::{
    API_VERSION, ApiErrorCode, ApiRequest, ApiResponse, Command, ControlError,
    EvaluationEventRecord, EvaluationRecord, GenomeRecord, ResponseData, RunCompletionReason,
    SelectionEventRecord, SelectionRecord, WorldRecord,
};

const MAX_REQUEST_BYTES: usize = 65_536;
const MAX_REQUEST_READ_BYTES: u64 = 65_537;
const CONTROL_AGGREGATE: &str = "hephaestus-control";
const OPERATOR_ACTOR: &str = "local-operator";
const MAX_EVALUATION_WALL_MILLIS: u64 = 86_400_000;
const MAX_EVALUATION_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EVALUATION_COST_MICROUSD: u64 = 1_000_000_000;
const PAIRED_EVALUATION_SEED: u64 = 42;
const PAIRED_EVALUATION_WALL_MILLIS: u64 = 10_000;
const PAIRED_EVALUATION_OUTPUT_BYTES: u64 = 1_048_576;
const MAX_SOURCE_FILE_BYTES: u64 = 1_048_576;
const MAX_ARTIFACT_FILE_BYTES: u64 = 64 * 1024 * 1024;

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
    token_hex: String,
    operator_token: OperatorToken,
    run_result_signer: RunResultSigner,
    run_result_verifier: RunResultVerifier,
    storage: Option<CanonicalStorage>,
    state: ControlState,
    _lock: File,
    shutdown_requested: bool,
}

struct CanonicalStorage {
    ledger: EventStore,
    artifacts: ArtifactStore,
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
        let data_dir = data_dir.into();
        let source_repository = validate_source_repository(&source_repository.into())?;
        let evaluator_executable = evaluator_executable.into();
        prepare_private_directory(&data_dir)?;
        let lock = take_writer_lock(&data_dir.join("daemon.lock"))?;
        let (token_hex, token_bytes) = load_or_create_token(&data_dir.join("operator.token"))?;
        let operator_token = OperatorToken::from_bytes(token_bytes);
        let database_path = data_dir.join("events.sqlite3");
        prepare_private_file(&database_path)?;
        let ledger = EventStore::open(&database_path)?;
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
        let state =
            ControlState::from_events(&history, registered, &operator_token, &run_result_verifier)?;
        ControlState::verify_artifacts(&history, &artifacts, &run_result_verifier)?;
        Ok(Self {
            data_dir,
            source_repository,
            evaluator_executable,
            token_hex,
            operator_token,
            run_result_signer,
            run_result_verifier,
            storage: Some(CanonicalStorage { ledger, artifacts }),
            state,
            _lock: lock,
            shutdown_requested: false,
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
        for connection in listener.incoming() {
            let mut stream = connection?;
            let _read_timeout_error = stream.set_read_timeout(Some(Duration::from_secs(2))).err();
            let _write_timeout_error = stream.set_write_timeout(Some(Duration::from_secs(2))).err();
            let _connection_error = self.serve_one(&mut stream).err();
            if self.shutdown_requested {
                break;
            }
        }
        Ok(())
    }

    fn serve_one(&mut self, stream: &mut UnixStream) -> Result<(), ControlError> {
        let mut bytes = Vec::new();
        stream
            .take(MAX_REQUEST_READ_BYTES)
            .read_to_end(&mut bytes)?;
        let response = if bytes.len() > MAX_REQUEST_BYTES {
            ApiResponse::failure("", ApiErrorCode::InvalidRequest, "request exceeds limit")
        } else {
            match serde_json::from_slice::<ApiRequest>(&bytes) {
                Ok(request) => self.handle(request),
                Err(_) => ApiResponse::failure(
                    "",
                    ApiErrorCode::InvalidRequest,
                    "request does not match the declared schema",
                ),
            }
        };
        stream.write_all(&serde_json::to_vec(&response)?)?;
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
        }
    }

    fn execute(
        &mut self,
        request_id: &str,
        command: Command,
    ) -> Result<ResponseData, ExecuteError> {
        let killed_runs = if command == Command::KillAll {
            self.state.active_runs.len()
        } else {
            0
        };
        self.append_audit(request_id, &command, event_type(&command))
            .map_err(|_| ExecuteError::Internal)?;
        require_command_fields(&command)?;

        match command {
            Command::Status => Ok(self.state.status()),
            Command::Freeze | Command::Unfreeze => Ok(ResponseData::Acknowledged {
                frozen: self.state.freeze.is_frozen(),
                killed_runs: 0,
            }),
            Command::KillAll => Ok(ResponseData::Acknowledged {
                frozen: self.state.freeze.is_frozen(),
                killed_runs,
            }),
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
            } => {
                self.run_paired_evaluation(&evaluation_id, &parent_genome_id, &candidate_genome_id)
            }
            Command::ArenaSelect { evaluation_id } => self.select_arena_evaluation(&evaluation_id),
            Command::Replay => self.replay_response(),
            Command::DaemonStop => {
                self.shutdown_requested = true;
                Ok(ResponseData::Acknowledged {
                    frozen: self.state.freeze.is_frozen(),
                    killed_runs: 0,
                })
            }
        }
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
    fn run_paired_evaluation(
        &mut self,
        evaluation_id: &str,
        parent_genome_id: &str,
        candidate_genome_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        if parent_genome_id == candidate_genome_id {
            return Err(ExecuteError::Invalid(
                "parent and candidate Genomes must differ",
            ));
        }
        let parent = self.runnable_genome(parent_genome_id)?;
        let candidate = self.runnable_genome(candidate_genome_id)?;
        if parent.world_id != candidate.world_id {
            return Err(ExecuteError::Invalid(
                "paired Genomes must share one registered World",
            ));
        }
        let world = self.registered_world(&parent.world_id)?;
        let visible = self.world_manifest(&world, "arena.visible_manifest", Visibility::Visible)?;
        let sealed = self.world_manifest(&world, "arena.sealed_manifest", Visibility::Sealed)?;
        let evaluator_id = world.evaluator_artifact("arena.evaluator").ok_or_else(|| {
            ExecuteError::Rejected(
                "World does not declare the arena.evaluator artifact required for paired evaluation"
                    .to_owned(),
            )
        })?;
        let budget = validated_evaluation_budget(
            PAIRED_EVALUATION_WALL_MILLIS,
            PAIRED_EVALUATION_OUTPUT_BYTES,
            0,
        )?;
        let budget_receipt = RunBudgetReceipt {
            wall_millis: PAIRED_EVALUATION_WALL_MILLIS,
            maximum_output_bytes: PAIRED_EVALUATION_OUTPUT_BYTES,
            maximum_cost_microusd: 0,
        };
        let evaluator_limits = WorkerLimits::new(
            Duration::from_millis(PAIRED_EVALUATION_WALL_MILLIS),
            16 * 1024 * 1024,
            128 * 1024,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let evaluator = self.open_evaluator(evaluator_id, evaluator_limits)?;
        let environment_id = reference_environment_id();
        let revision = self.paired_revision(evaluation_id)?;
        let parent_plan = self.schedule_submission(
            evaluation_id,
            "parent",
            &parent,
            &visible,
            &sealed,
            &revision,
            &environment_id,
            budget,
        )?;
        let candidate_plan = self.schedule_submission(
            evaluation_id,
            "candidate",
            &candidate,
            &visible,
            &sealed,
            &revision,
            &environment_id,
            budget,
        )?;
        let binding = EvaluationBinding::new(
            world.id(),
            PAIRED_EVALUATION_SEED,
            &environment_id,
            evaluator_id,
            budget_receipt,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let storage = self.storage.take().ok_or(ExecuteError::Internal)?;
        let result = evaluate_and_record(
            EvaluationStores {
                events: storage.ledger,
                artifacts: storage.artifacts,
            },
            ReceiptContext {
                event_id: format!("arena:evaluation:{evaluation_id}:recorded"),
                evaluation_id: evaluation_id.to_owned(),
                caller_id: "control-daemon".to_owned(),
                timestamp_millis: timestamp_millis().map_err(|_| ExecuteError::Internal)?,
            },
            &world,
            EvaluationInputs {
                binding: &binding,
                visible: &visible,
                sealed: &sealed,
                parent: &parent_plan,
                candidate: &candidate_plan,
                evaluator: &evaluator,
            },
        );
        let Ok(operator) = result else {
            self.reopen_storage()?;
            self.refresh_projection()?;
            return Err(ExecuteError::Internal);
        };
        let recorded = operator.candidate_result();
        let response = ResponseData::Evaluation {
            evaluation: EvaluationRecord {
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
            },
        };
        let stores = operator.into_stores();
        self.storage = Some(CanonicalStorage {
            ledger: stores.events,
            artifacts: stores.artifacts,
        });
        self.refresh_projection()?;
        Ok(response)
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

    #[allow(clippy::too_many_arguments)]
    fn schedule_submission(
        &mut self,
        evaluation_id: &str,
        role: &str,
        genome: &GenomeRecord,
        visible: &TrustedManifest,
        sealed: &TrustedManifest,
        revision: &str,
        environment_id: &str,
        budget: Budget,
    ) -> Result<TrialPlan, ExecuteError> {
        let tasks = visible
            .operator_tasks()
            .into_iter()
            .chain(sealed.operator_tasks())
            .collect::<Vec<_>>();
        let mut trials = Vec::with_capacity(tasks.len());
        for (index, task) in tasks.into_iter().enumerate() {
            let run_id = paired_run_id(evaluation_id, role, index);
            let event_id = format!("result:{run_id}");
            if !self.has_event(&event_id)? {
                self.run_candidate_at_revision(
                    &run_id,
                    genome,
                    &task.task_id,
                    &task.input,
                    revision,
                    environment_id,
                    budget,
                )?;
            }
            trials.push((task.task_id, event_id));
        }
        TrialPlan::new(trials).map_err(|_| ExecuteError::Internal)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_candidate_at_revision(
        &mut self,
        run_id: &str,
        genome: &GenomeRecord,
        task_id: &str,
        input: &str,
        revision: &str,
        environment_id: &str,
        budget: Budget,
    ) -> Result<ResponseData, ExecuteError> {
        let experiment = ExperimentContext::new(
            task_id,
            input.as_bytes(),
            PAIRED_EVALUATION_SEED,
            environment_id,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let spec = RunSpec::new_for_experiment_at_revision(
            run_id,
            &genome.genome_id,
            &genome.world_id,
            &self.source_repository,
            revision,
            input,
            CapabilitySet::new(false, false),
            budget,
            experiment,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let manager =
            SandboxManager::open(self.data_dir.join("sandboxes"), Duration::from_secs(30))
                .map_err(|_| ExecuteError::Internal)?;
        let (sandbox, token) = manager.create(&spec).map_err(|_| ExecuteError::Internal)?;
        let sandbox = SandboxCleanupGuard::new(sandbox);
        let isolation = candidate_isolation(self.protected_runtime_paths());
        let runtime = SupervisedRuntime::deterministic(isolation, "/bin/cat", [])
            .map_err(|_| ExecuteError::Internal)?;
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
            let (execution, recorder) = execute_candidate_runtime(
                runtime,
                recorder,
                &spec,
                sandbox.sandbox()?,
                &token,
                run_id,
            );
            let (ledger, artifacts) = recorder.into_stores();
            let execution = execution.and_then(|output| {
                persist_reference_output(&artifacts, run_id, genome, spec.source_revision(), output)
            });
            self.storage = Some(CanonicalStorage { ledger, artifacts });
            execution
        })();
        sandbox.cleanup()?;
        let response = execution?;
        self.append_run_result(&spec, &response)?;
        self.refresh_projection()?;
        Ok(response)
    }

    fn registered_world(&self, world_id: &str) -> Result<CompiledWorld, ExecuteError> {
        self.state
            .registered
            .world(world_id)
            .map(|world| world.compiled().clone())
            .ok_or(ExecuteError::Internal)
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

    fn has_event(&self, event_id: &str) -> Result<bool, ExecuteError> {
        self.storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map(|history| history.iter().any(|event| event.event_id == event_id))
            .map_err(|_| ExecuteError::Internal)
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
        let environment_id = reference_environment_id();
        let experiment = ExperimentContext::new(task_id, prompt.as_bytes(), seed, environment_id)
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
            let (execution, recorder) =
                execute_reference_runtime(recorder, &spec, sandbox.sandbox()?, &token, run_id);
            let (ledger, artifacts) = recorder.into_stores();
            let execution = execution.and_then(|output| {
                persist_reference_output(&artifacts, run_id, genome, spec.source_revision(), output)
            });
            self.storage = Some(CanonicalStorage { ledger, artifacts });
            execution
        })();
        sandbox.cleanup()?;
        let response = execution?;
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
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(event)
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
            registered,
            event_count: 0,
        };
        for event in events {
            state.apply(event, operator_token, run_result_verifier)?;
        }
        Ok(state)
    }

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
            "control.kill_all" => self.active_runs.clear(),
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
                match receipt.kind {
                    TraceKind::LifecycleStarted | TraceKind::LifecycleResumed => {
                        self.active_runs
                            .insert(receipt.provenance.run_id().to_owned());
                    }
                    TraceKind::LifecycleCompleted => {
                        self.active_runs.remove(receipt.provenance.run_id());
                    }
                    _ => {}
                }
            }
            "run.result_recorded" => validate_run_result(event, run_result_verifier)?,
            _ => {}
        }
        self.event_count = event.sequence;
        Ok(())
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
    event_count: u64,
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

fn validate_run_result(
    event: &StoredEvent,
    run_result_verifier: &RunResultVerifier,
) -> Result<(), ControlError> {
    RunResultReceipt::parse_from_event(event, run_result_verifier)
        .map(|_| ())
        .map_err(|_| ControlError::Projection("canonical run result is invalid".to_owned()))
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
    use std::{fs, os::unix::fs::symlink};

    use hephaestus_experience::{Provenance, TraceKind, TraceReceipt};
    use hephaestus_genome::{SourceFormat, compile_world};
    use hephaestus_ledger::{EventInput, EventStore};
    use tempfile::tempdir;

    use super::*;

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
