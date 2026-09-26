//! `ControlState`, the daemon's in-memory projection of the event ledger
//! (jobs, arena jobs, freeze state, worker credentials, remote job leases),
//! and unfinished-job recovery on daemon startup, split out of server.rs.

use super::{
    ArenaJobPhase, ArenaJobRecord, ArtifactBackend, ArtifactId, BTreeMap, BTreeSet,
    CONTROL_AGGREGATE, ControlError, Deserialize, EventIndex, EventInput, EventLedger,
    EvidenceCache, FreezeState, JobProgress, JobRecord, JobState, JobTerminal, OPERATOR_ACTOR,
    OperatorToken, ProjectionSnapshot, RUNTIME_ACTOR, RecordedCommand, RegisteredObjects,
    ResponseData, RunBudgetReceipt, RunCompletionReason, RunRecord, RunResultReceipt,
    RunResultVerifier, Serialize, StoredEvent, TraceKind, TraceReceipt, WorkerScope,
    arena_job_immutable_fields_match, evaluation_record_from_recorded, event_type, job_run_id,
    load_recorded_evaluation_in, receipt_matches_job, require_projection_text, timestamp_millis,
    trace_phase, validate_arena_job_record, validate_content_id, validate_trace_receipt,
};

pub(super) struct ControlState {
    pub(super) freeze: FreezeState,
    pub(super) active_runs: BTreeSet<String>,
    pub(super) jobs: BTreeMap<String, JobRecord>,
    pub(super) arena_jobs: BTreeMap<String, ArenaJobRecord>,
    pub(super) job_progress: BTreeMap<String, JobProgress>,
    pub(super) evaluation_events: BTreeMap<String, u64>,
    pub(super) run_results: BTreeMap<String, RunResultReceipt>,
    pub(super) completed_runs: BTreeSet<String>,
    pub(super) registered: RegisteredObjects,
    pub(super) event_count: u64,
    pub(super) worker_credentials: BTreeMap<String, WorkerCredentialRecord>,
    pub(super) remote_jobs: BTreeMap<String, RemoteJobRecord>,
}

/// Durable, replay-verified projection of one minted worker credential. The
/// raw secret is never stored; `credential_id` is its content-derived,
/// safe-to-log identity (`blake3(secret)[..32]`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkerCredentialRecord {
    pub(super) schema_version: u16,
    pub(super) credential_id: String,
    pub(super) worker_id: String,
    pub(super) scope: WorkerScope,
    pub(super) expires_at_millis: i64,
    pub(super) revoked: bool,
}

/// Durable, replay-verified admission of one remote-worker job. Terminal
/// state is derived by joining `run_id` against `ControlState::run_results`,
/// the same signed-result table local runs populate, so a remote result is
/// indistinguishable from a local one once recorded.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteJobRecord {
    pub(super) schema_version: u16,
    pub(super) job_id: String,
    pub(super) genome_id: String,
    pub(super) run_id: String,
}

impl ControlState {
    pub(super) fn from_events(
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
    pub(super) fn apply(
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

    pub(super) fn apply_job_record(&mut self, event: &StoredEvent) -> Result<(), ControlError> {
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

    pub(super) fn advance_arena_trial(
        &mut self,
        receipt: &RunResultReceipt,
    ) -> Result<(), ControlError> {
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

    pub(super) fn apply_arena_job_record(
        &mut self,
        event: &StoredEvent,
    ) -> Result<(), ControlError> {
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

    pub(super) fn arena_job_transition_is_valid(
        &self,
        event: &StoredEvent,
        record: &ArenaJobRecord,
    ) -> bool {
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

    pub(super) fn validate_job_record(
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

    pub(super) fn job_transition_is_valid(&self, event: &StoredEvent, record: &JobRecord) -> bool {
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

    pub(super) fn commit_job_record(&mut self, record: JobRecord) {
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

    pub(super) fn status(&self) -> ResponseData {
        ResponseData::Status {
            frozen: self.freeze.is_frozen(),
            active_runs: self.active_runs.len(),
            event_count: self.event_count,
            genome_count: self.registered.genomes().count(),
        }
    }

    pub(super) fn snapshot(&self) -> ProjectionSnapshot {
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

    pub(super) fn verify_artifacts(
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

    pub(super) fn verify_artifacts_with(
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

pub(super) fn recover_unfinished_jobs(
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

pub(super) fn recover_unfinished_arena_jobs(
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
