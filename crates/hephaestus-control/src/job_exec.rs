//! Job submission, async-message servicing, and arena-scoring reconciliation
//! for `ControlPlane`, split out of server.rs.

use super::{
    ActiveJob, Arc, ArenaJobPhase, ArenaWorkerMessage, AsyncJobResult, AsyncProviderLaunch,
    AsyncReferenceLaunch, AuditedCommand, CONTROL_AGGREGATE, CanonicalStorage, CapabilitySet,
    Command, ControlError, ControlPlane, ControlState, EvaluationInputs, EvaluationSources,
    EvaluationStores, EventInput, EventLedger, EvidenceRecorder, ExecuteError, ExperimentContext,
    GenomeRecord, Instant, JobRecord, JobState, JobTerminal, OPERATOR_ACTOR, Ordering,
    PROVIDER_RUN_WALL_MILLIS, PinnedReferenceWorker, Provider, RUNTIME_ACTOR, RedactionPolicy,
    RegisteredObjects, ResponseData, RetentionLimits, RunBudgetReceipt, RunCompletionReason,
    RunSpec, SandboxManagerSource, ScoredEvaluation, evaluate_and_record_scored,
    evaluation_record_from_operator, executable_digest, execute_async_provider,
    execute_async_reference, job_run_id, mpsc, persist_reference_output, prepare_evaluation,
    resolve_provider_extra_env, timestamp_millis, validate_job_id, validated_evaluation_budget,
    verify_arena_evaluation_records, verify_canary_history, verify_champion_history,
    verify_cluster_history, verify_drift_adaptation_history, verify_drift_history,
    verify_forge_assessment_history, verify_forge_history, verify_gene_bank_history,
    verify_invariant_history, verify_selection_history,
};

#[cfg(not(test))]
use std::thread;

#[cfg(test)]
use super::spawn_named_thread;

impl ControlPlane {
    #[allow(clippy::too_many_lines)]
    pub(super) fn submit_job(
        &mut self,
        job_id: &str,
        genome_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
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
                        sandboxes: SandboxManagerSource::OpenFresh(data_dir),
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
                        sandboxes: SandboxManagerSource::OpenFresh(data_dir),
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

    pub(super) fn async_reference_spec(
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
    pub(super) fn async_provider_spec(
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
        let budget =
            validated_evaluation_budget(PROVIDER_RUN_WALL_MILLIS, 1_048_576, cost_ceiling)?;
        let environment_id = Self::provider_job_environment(provider, &digest);
        let experiment = ExperimentContext::new(task_id, prompt.as_bytes(), 0, environment_id)
            .map_err(|_| ExecuteError::Invalid("evaluation context is invalid"))?;
        let capabilities = self.compiled_genome(&genome.genome_id)?.authority();
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
        spec = spec
            .with_provider_model(self.compiled_genome(&genome.genome_id)?.model_family())
            .map_err(|_| {
                ExecuteError::Rejected("registered provider model is invalid".to_owned())
            })?;
        if let Some(instruction) = self.provider_instruction(&genome.genome_id)? {
            spec = spec.with_agent_instruction(instruction).map_err(|_| {
                ExecuteError::Rejected("registered provider instruction is invalid".to_owned())
            })?;
        }
        Ok(spec)
    }

    pub(super) fn job_status(&self, job_id: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn job_response(&self, job: JobRecord) -> ResponseData {
        let progress = self
            .state
            .job_progress
            .get(&job.job_id)
            .cloned()
            .unwrap_or_default();
        ResponseData::Job { job, progress }
    }

    pub(super) fn kill_job(&mut self, job_id: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn request_active_job_cancellation(&mut self) -> Result<(), ExecuteError> {
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

    pub(super) fn request_job_cancellation(&mut self, job_id: &str) -> Result<(), ExecuteError> {
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

    pub(super) fn append_job_record(&mut self, record: &JobRecord) -> Result<(), ExecuteError> {
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

    /// Builds a per-event [`EvidenceRecorder`] around `storage`, threading through the
    /// per-run record-count cache (see [`Self::evidence_run_record_counts`]'s doc
    /// comment) instead of reseeding it with a full ledger replay every event.
    pub(super) fn evidence_recorder_for(
        &mut self,
        storage: CanonicalStorage,
    ) -> Result<EvidenceRecorder, ControlError> {
        Ok(EvidenceRecorder::from_stores_with_run_counts(
            storage.ledger,
            storage.artifacts,
            RedactionPolicy::new([self.token_hex.clone()]),
            RetentionLimits::new(10_000, 65_536)
                .map_err(|_| ControlError::Protocol("trace limits are invalid"))?,
            std::mem::take(&mut self.evidence_run_record_counts),
        ))
    }

    /// Returns `recorder`'s stores and updated per-run counts to `self` after a
    /// persisted (or rejected) evidence request, the counterpart to
    /// [`Self::evidence_recorder_for`].
    pub(super) fn restore_after_evidence_recorder(&mut self, recorder: EvidenceRecorder) {
        let (ledger, artifacts, run_record_counts) = recorder.into_stores_with_run_counts();
        self.storage = Some(CanonicalStorage { ledger, artifacts });
        self.evidence_run_record_counts = run_record_counts;
    }

    pub(super) fn service_async_messages(&mut self) -> Result<(), ControlError> {
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
            let mut recorder = self.evidence_recorder_for(storage)?;
            let result = request.persist(&mut recorder);
            self.restore_after_evidence_recorder(recorder);
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
        self.advance_meta_evaluations();
        self.advance_drift_adaptations();
        Ok(())
    }

    pub(super) fn enforce_arena_deadline(&mut self) -> Result<(), ControlError> {
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
    pub(super) fn service_arena_message(&mut self) -> Result<(), ControlError> {
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

    pub(super) fn start_arena_scoring(&mut self) -> Result<(), ControlError> {
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
    pub(super) fn finish_arena_scoring(
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

    pub(super) fn clear_active_arena_job(&mut self) {
        self.active_arena_job = None;
        self.arena_message_receiver = None;
        self.arena_message_sender = None;
        self.job_evidence_receiver = None;
    }

    pub(super) fn complete_async_job(
        &mut self,
        completed: AsyncJobResult,
    ) -> Result<(), ExecuteError> {
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
            if let Err(reason) = &completed.output {
                // The ledger records only the terminal state; name the cause for
                // the operator instead of failing silently.
                eprintln!("hephaestusd: job {} failed: {reason}", record.job_id);
            }
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

    pub(super) fn append_audit(
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

    pub(super) fn replay_response(&self) -> Result<ResponseData, ExecuteError> {
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
}
