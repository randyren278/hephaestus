//! Reference/candidate/provider evaluation execution, arena job submission,
//! and selection/invariant/cluster checks for `ControlPlane`, split out of
//! server.rs.

use super::{
    ActiveArenaJob, Arc, ArenaError, ArenaJobPhase, ArenaJobProgress, ArenaJobRecord,
    ArenaTrialSpec, ArtifactBackend, ArtifactId, AsyncArenaTrialLaunch, CanonicalStorage,
    CapabilitySet, CompiledGenome, CompiledWorld, ControlPlane, Duration, EvaluationBinding,
    EvaluationStores, EventInput, EventLedger, EvidenceRecorder, ExecuteError, ExperimentContext,
    GenomeRecord, Instant, IsolatedEvaluator, JobState, JobTerminal, MAX_EVALUATION_WALL_MILLIS,
    OperatorClusterAnalysis, OperatorInvariantCheck, PAIRED_EVALUATION_OUTPUT_BYTES,
    PAIRED_EVALUATION_SEED, PAIRED_EVALUATION_WALL_MILLIS, PROVIDER_RUN_WALL_MILLIS, PathBuf,
    PermissionsExt, PinnedReferenceWorker, Provider, RUN_RESULT_SCHEMA_VERSION, RUNTIME_ACTOR,
    ReceiptContext, RedactionPolicy, ReferenceInstruction, ResponseData, RetentionLimits,
    RunBudgetReceipt, RunResultReceipt, RunSpec, SandboxCleanupGuard, SandboxManager,
    SupervisedRuntime, TempDirBuilder, TrialPlan, TrustedManifest, Visibility, WorkerLimits,
    candidate_isolation, check_failure_clusters, check_reference_output_invariants, env,
    executable_digest, execute_async_arena_trials, execute_candidate_runtime,
    execute_provider_runtime, execute_reference_runtime, fs, genome_reference_instruction,
    invariant_record, load_failure_clusters, load_operator_evaluation,
    load_reference_output_invariants, map_cluster_error, map_invariant_error, map_selection_error,
    mpsc, paired_run_id, paired_run_prefix, persist_reference_output,
    provider_execution_environment, reference_environment_id, resolve_provider_extra_env,
    resolve_source_revision, select_and_record, selection_record, timestamp_millis,
    validate_job_id, validated_evaluation_budget, with_provider_login,
};

#[cfg(not(test))]
use std::thread;

#[cfg(feature = "test-support")]
use super::{IsolationPolicy, TEST_ARENA_OVERALL_WALL_ENV, test_overall_wall};

#[cfg(test)]
use super::spawn_named_thread;

use super::verification::forge_analysis_record;

impl ControlPlane {
    pub(super) fn arena_selection_show(
        &self,
        evaluation_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let event_id = format!("arena:selection:{evaluation_id}:selected");
        let event = history
            .iter()
            .find(|event| event.event_id == event_id)
            .ok_or(ExecuteError::NotFound)?;
        let (actual_id, world_id) = hephaestus_arena::selection_event_references(event)
            .map_err(|_| ExecuteError::Internal)?;
        if actual_id != evaluation_id {
            return Err(ExecuteError::Internal);
        }
        let world = self
            .state
            .registered
            .world(&world_id)
            .ok_or(ExecuteError::Internal)?;
        let selected = hephaestus_arena::verify_selection_event_in(
            &hephaestus_ledger::EventIndex::build(&history),
            &storage.artifacts,
            event,
            world.compiled(),
        )
        .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Selection {
            selection: Box::new(selection_record(
                &world_id,
                selected.receipt(),
                selected.event(),
            )),
        })
    }

    pub(super) fn submit_confirmed_arena_job(
        &mut self,
        evaluation_id: &str,
        parent_genome_id: &str,
        candidate_genome_id: &str,
        expected: &super::GenomeProfileRecord,
    ) -> Result<ResponseData, ExecuteError> {
        // A retry reads the original admission, including its original limits.
        // Profile changes must never turn an existing job into a fresh refusal.
        if self.state.arena_jobs.contains_key(evaluation_id) {
            return self.submit_arena_job(
                evaluation_id,
                parent_genome_id,
                candidate_genome_id,
                false,
            );
        }
        let ResponseData::GenomeProfile { profile } = self.genome_profile(candidate_genome_id)?
        else {
            return Err(ExecuteError::Internal);
        };
        if profile.as_ref() != expected {
            return Err(ExecuteError::Rejected(
                "execution settings changed; refresh and confirm the current profile".to_owned(),
            ));
        }
        let ResponseData::GenomeProfile { profile: parent } =
            self.genome_profile(parent_genome_id)?
        else {
            return Err(ExecuteError::Internal);
        };
        if parent.provider != profile.provider
            || parent.family != profile.family
            || parent.workspace_write != profile.workspace_write
            || parent.network != profile.network
            || parent.world != profile.world
            || parent.visible_tasks != profile.visible_tasks
            || parent.sealed_tasks != profile.sealed_tasks
            || parent.paired_trial_wall_millis != profile.paired_trial_wall_millis
            || parent.paired_trial_output_bytes != profile.paired_trial_output_bytes
            || parent.paired_total_wall_millis != profile.paired_total_wall_millis
            || parent.reported_cost_limit_microusd != profile.reported_cost_limit_microusd
        {
            return Err(ExecuteError::Rejected("confirmed comparisons require the same model, authority, World and limits for both roles".to_owned()));
        }
        self.submit_arena_job(evaluation_id, parent_genome_id, candidate_genome_id, false)
    }

    /// Shared admission/inspection budgets for the two roles of a paired evaluation.
    pub(super) fn paired_budget_plan(
        hosted: bool,
        world_cost_limit: u64,
        task_count: usize,
    ) -> Result<(RunBudgetReceipt, RunBudgetReceipt), ExecuteError> {
        if task_count == 0 || task_count > 1000 {
            return Err(ExecuteError::Invalid(
                "paired task count is outside the bounded range",
            ));
        }
        let trial = RunBudgetReceipt {
            wall_millis: if hosted {
                PROVIDER_RUN_WALL_MILLIS
            } else {
                PAIRED_EVALUATION_WALL_MILLIS
            },
            maximum_output_bytes: PAIRED_EVALUATION_OUTPUT_BYTES,
            maximum_cost_microusd: if hosted { world_cost_limit } else { 0 },
        };
        validated_evaluation_budget(
            trial.wall_millis,
            trial.maximum_output_bytes,
            trial.maximum_cost_microusd,
        )?;
        let total_trials = u64::try_from(task_count)
            .map_err(|_| ExecuteError::Internal)?
            .checked_mul(2)
            .ok_or(ExecuteError::Internal)?;
        let maximum_overall_wall = trial
            .wall_millis
            .checked_mul(total_trials)
            .and_then(|wall| wall.checked_add(PAIRED_EVALUATION_WALL_MILLIS))
            .ok_or(ExecuteError::Internal)?;
        if maximum_overall_wall > MAX_EVALUATION_WALL_MILLIS {
            return Err(ExecuteError::Rejected(
                "paired evaluation exceeds the one-day wall limit; use a smaller task pack"
                    .to_owned(),
            ));
        }
        #[cfg(feature = "test-support")]
        let overall_wall = test_overall_wall(
            maximum_overall_wall,
            env::var(TEST_ARENA_OVERALL_WALL_ENV).ok().as_deref(),
        )
        .map_err(|()| ExecuteError::Invalid("test Arena wall budget must only lower the bound"))?;
        #[cfg(not(feature = "test-support"))]
        let overall_wall = maximum_overall_wall;
        let overall = RunBudgetReceipt {
            wall_millis: overall_wall,
            maximum_output_bytes: trial
                .maximum_output_bytes
                .checked_mul(total_trials)
                .ok_or(ExecuteError::Internal)?,
            maximum_cost_microusd: trial
                .maximum_cost_microusd
                .checked_mul(total_trials)
                .ok_or(ExecuteError::Internal)?,
        };
        Ok((trial, overall))
    }

    pub(super) fn run_reference(
        &mut self,
        run_id: &str,
        genome_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
        let genome = self.runnable_genome(genome_id)?;
        let result = self.run_reference_inner(run_id, &genome);
        self.refresh_projection()?;
        result
    }

    pub(super) fn compiled_genome(&self, genome_id: &str) -> Result<CompiledGenome, ExecuteError> {
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
    pub(super) fn selected_run_provider(
        &self,
        genome_id: &str,
    ) -> Result<Option<Provider>, ExecuteError> {
        let compiled = self.compiled_genome(genome_id)?;
        Ok(match compiled.model_provider() {
            "codex" => Some(Provider::Codex),
            "claude" => Some(Provider::Claude),
            _ => None,
        })
    }

    pub(super) fn runnable_genome(&self, genome_id: &str) -> Result<GenomeRecord, ExecuteError> {
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

    pub(super) fn run_reference_inner(
        &mut self,
        run_id: &str,
        genome: &GenomeRecord,
    ) -> Result<ResponseData, ExecuteError> {
        let prompt = "Inventory the isolated repository without modifying it or using the network.";
        // Deterministic runs never report cost, so a zero ceiling is exact for
        // them; a provider genome is bounded by its own World's approved Law
        // instead of an arbitrary fixed figure.
        let (maximum_cost_microusd, wall_millis) =
            match self.selected_run_provider(&genome.genome_id)? {
                Some(_) => (
                    self.registered_world_cost_ceiling(&genome.world_id)?,
                    PROVIDER_RUN_WALL_MILLIS,
                ),
                None => (0, 10_000),
            };
        self.run_with_context(
            run_id,
            genome,
            "repository-inventory-v1",
            prompt,
            0,
            wall_millis,
            1_048_576,
            maximum_cost_microusd,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_evaluation(
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
    pub(super) fn submit_arena_job(
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
        let parent_provider = self.selected_run_provider(parent_genome_id)?;
        let candidate_provider = self.selected_run_provider(candidate_genome_id)?;
        let parent_model: Option<Arc<str>> = parent_provider
            .map(|_| {
                self.compiled_genome(parent_genome_id)
                    .map(|genome| Arc::from(genome.model_family()))
            })
            .transpose()?;
        let candidate_model: Option<Arc<str>> = candidate_provider
            .map(|_| {
                self.compiled_genome(candidate_genome_id)
                    .map(|genome| Arc::from(genome.model_family()))
            })
            .transpose()?;
        let parent_instruction = if parent_provider.is_some() {
            self.provider_instruction(parent_genome_id)?
        } else {
            self.reference_instruction(parent_genome_id)?;
            None
        };
        let candidate_instruction = if candidate_provider.is_some() {
            self.provider_instruction(candidate_genome_id)?
        } else {
            self.reference_instruction(candidate_genome_id)?;
            None
        };
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
        if tasks
            .iter()
            .any(|task| task.input.len() > hephaestus_runtime::MAX_TASK_INPUT_BYTES)
        {
            return Err(ExecuteError::Invalid("paired task input is oversized"));
        }
        let total_trials = tasks.len().checked_mul(2).ok_or(ExecuteError::Internal)?;
        let total_trials_u32 = u32::try_from(total_trials).map_err(|_| ExecuteError::Internal)?;
        let (trial_budget, overall_budget) = Self::paired_budget_plan(
            parent_provider.is_some() || candidate_provider.is_some(),
            world.evaluation_policy().maximum_cost_microusd(),
            tasks.len(),
        )?;
        let per_trial_wall = trial_budget.wall_millis;
        let per_trial_cost_ceiling = trial_budget.maximum_cost_microusd;
        let overall_wall = overall_budget.wall_millis;
        let budget = validated_evaluation_budget(
            per_trial_wall,
            trial_budget.maximum_output_bytes,
            per_trial_cost_ceiling,
        )?;
        let evaluator_limits = WorkerLimits::new(
            Duration::from_millis(PAIRED_EVALUATION_WALL_MILLIS),
            16 * 1024 * 1024,
            128 * 1024,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let evaluator = Arc::new(self.open_evaluator(&evaluator_id, evaluator_limits)?);
        // The reference worker is only pinned (a private snapshot write plus
        // a digest re-verification) when a role actually needs it: a fully
        // provider pair never executes it, so skip the work entirely rather
        // than pinning-then-discarding it (TD-15). `self.reference_worker_digest`
        // alone is enough to fill `worker_digest` below in that case -- it is
        // exactly what a pin's own digest would re-verify to anyway.
        let worker = if parent_provider.is_none() || candidate_provider.is_none() {
            Some(self.pin_reference_worker()?)
        } else {
            None
        };
        let reference_environment_id = worker
            .as_ref()
            .map(|worker| Self::reference_execution_environment(worker));
        let provider_environment_id = |provider: Provider| -> Result<String, ExecuteError> {
            let executable = self.provider_executable(provider)?;
            let digest = executable_digest(&executable).map_err(|_| ExecuteError::Internal)?;
            Ok(Self::provider_job_environment(provider, &digest))
        };
        let parent_environment_id = match parent_provider {
            Some(provider) => provider_environment_id(provider)?,
            None => reference_environment_id
                .clone()
                .ok_or(ExecuteError::Internal)?,
        };
        let candidate_environment_id = match candidate_provider {
            Some(provider) => provider_environment_id(provider)?,
            None => reference_environment_id
                .clone()
                .ok_or(ExecuteError::Internal)?,
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
        for (role, genome, plan, provider, role_environment_id, agent_instruction, model) in [
            (
                "parent",
                &parent_genome,
                &mut parent_plan,
                parent_provider,
                &parent_environment_id,
                &parent_instruction,
                &parent_model,
            ),
            (
                "candidate",
                &candidate_genome,
                &mut candidate_plan,
                candidate_provider,
                &candidate_environment_id,
                &candidate_instruction,
                &candidate_model,
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
                if let Some(model) = model {
                    spec = spec.with_provider_model(Arc::clone(model)).map_err(|_| {
                        ExecuteError::Rejected("registered provider model is invalid".to_owned())
                    })?;
                }
                if let Some(instruction) = agent_instruction {
                    spec = spec
                        .with_agent_instruction(Arc::clone(instruction))
                        .map_err(|_| {
                            ExecuteError::Rejected(
                                "registered provider instruction is invalid".to_owned(),
                            )
                        })?;
                }
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
            worker_digest: worker.as_ref().map_or_else(
                || self.reference_worker_digest.clone(),
                |worker| worker.digest.clone(),
            ),
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
            worker: worker.clone(),
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

    pub(super) fn arena_job_response(record: &ArenaJobRecord) -> ResponseData {
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

    pub(super) fn append_arena_job_record(
        &mut self,
        record: &ArenaJobRecord,
    ) -> Result<(), ExecuteError> {
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

    pub(super) fn select_arena_evaluation(
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

    pub(super) fn check_arena_invariants(
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

    pub(super) fn finish_invariant_check(
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

    pub(super) fn analyze_forge_clusters(
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

    pub(super) fn finish_cluster_check(
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

    pub(super) fn open_arena_stores(&self) -> Result<EvaluationStores, ExecuteError> {
        let events = (self.open_ledger)().map_err(|_| ExecuteError::Internal)?;
        let artifacts = (self.open_artifacts)().map_err(|_| ExecuteError::Internal)?;
        Ok(EvaluationStores { events, artifacts })
    }

    pub(super) fn registered_world(&self, world_id: &str) -> Result<CompiledWorld, ExecuteError> {
        self.state
            .registered
            .world(world_id)
            .map(|world| world.compiled().clone())
            .ok_or(ExecuteError::Internal)
    }

    pub(super) fn reference_instruction(
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

    /// Loads the registered prose bytes once; trial specs share the immutable
    /// instruction while preserving each task's separate input commitment.
    pub(super) fn provider_instruction(
        &self,
        genome_id: &str,
    ) -> Result<Option<Arc<str>>, ExecuteError> {
        let genome = self.compiled_genome(genome_id)?;
        let Some(artifact) = genome.artifact_id("agent.prompt") else {
            return Ok(None);
        };
        let bytes = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .artifacts
            .get(&ArtifactId::parse(artifact.to_owned()).map_err(|_| ExecuteError::Internal)?)
            .map_err(|_| ExecuteError::Internal)?;
        if bytes.len() > hephaestus_runtime::MAX_TASK_INPUT_BYTES {
            return Err(ExecuteError::Rejected(
                "registered provider instruction is invalid".to_owned(),
            ));
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            ExecuteError::Rejected("registered provider instruction is invalid".to_owned())
        })?;
        if text.trim().is_empty() {
            return Err(ExecuteError::Rejected(
                "registered provider instruction is invalid".to_owned(),
            ));
        }
        Ok(Some(Arc::from(text)))
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
    pub(super) fn pin_reference_worker(&self) -> Result<Arc<PinnedReferenceWorker>, ExecuteError> {
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
    pub(super) fn pin_fresh_reference_worker(&self) -> Result<PinnedReferenceWorker, ExecuteError> {
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

    pub(super) fn reference_execution_environment(worker: &PinnedReferenceWorker) -> String {
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
    pub(super) fn provider_job_environment(provider: Provider, executable_digest: &str) -> String {
        let version = hephaestus_runtime::PROVIDER_INPUT_VERSION;
        let name = match provider {
            Provider::Codex => "codex-cli",
            Provider::Claude => "claude-cli",
            Provider::Deterministic => "deterministic",
        };
        let identity = format!(
            "{name}-v{version}.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git|provider-instruction-frame-v{version}|exe-{executable_digest}",
            env!("CARGO_PKG_VERSION"),
            RUN_RESULT_SCHEMA_VERSION,
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        format!(
            "provider-v{version}.{}",
            blake3::hash(identity.as_bytes()).to_hex()
        )
    }

    /// Resolves the exact executable path currently configured for `provider`.
    pub(super) fn provider_executable(&self, provider: Provider) -> Result<PathBuf, ExecuteError> {
        match provider {
            Provider::Codex => Ok(self.codex_executable.clone()),
            Provider::Claude => Ok(self.claude_executable.clone()),
            Provider::Deterministic => Err(ExecuteError::Internal),
        }
    }

    pub(super) fn world_manifest(
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
        let manifest = TrustedManifest::from_canonical_bytes(&bytes, visibility)
            .map_err(|_| ExecuteError::Internal)?;
        manifest
            .validate_scoring(world.evaluation_policy().output_scoring())
            .map_err(|_| {
                ExecuteError::Rejected(
                    "World task expectations do not match its output scoring policy".to_owned(),
                )
            })?;
        Ok(manifest)
    }

    #[cfg(feature = "test-support")]
    pub(super) fn open_evaluator(
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
    pub(super) fn open_evaluator(
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

    pub(super) fn protected_runtime_paths(&self) -> Vec<PathBuf> {
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
    pub(super) fn protected_evaluator_paths(&self) -> Vec<PathBuf> {
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

    pub(super) fn paired_revision(&self, evaluation_id: &str) -> Result<String, ExecuteError> {
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

    pub(super) fn reopen_storage(&mut self) -> Result<(), ExecuteError> {
        let ledger = (self.open_ledger)().map_err(|_| ExecuteError::Internal)?;
        let artifacts = (self.open_artifacts)().map_err(|_| ExecuteError::Internal)?;
        self.storage = Some(CanonicalStorage { ledger, artifacts });
        Ok(())
    }

    pub(super) fn registered_world_cost_ceiling(
        &self,
        world_id: &str,
    ) -> Result<u64, ExecuteError> {
        Ok(self
            .registered_world(world_id)?
            .evaluation_policy()
            .maximum_cost_microusd())
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    pub(super) fn run_with_context(
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
        if prompt.len() > hephaestus_runtime::MAX_TASK_INPUT_BYTES {
            return Err(ExecuteError::Invalid("task input is oversized"));
        }
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
        if selected_provider.is_some() {
            spec = spec
                .with_provider_model(self.compiled_genome(&genome.genome_id)?.model_family())
                .map_err(|_| {
                    ExecuteError::Rejected("registered provider model is invalid".to_owned())
                })?;
            if let Some(agent_instruction) = self.provider_instruction(&genome.genome_id)? {
                spec = spec
                    .with_agent_instruction(agent_instruction)
                    .map_err(|_| {
                        ExecuteError::Rejected(
                            "registered provider instruction is invalid".to_owned(),
                        )
                    })?;
            }
        }
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
                    .and_then(|runtime| with_provider_login(runtime, provider))
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

    pub(super) fn append_run_result(
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
}
