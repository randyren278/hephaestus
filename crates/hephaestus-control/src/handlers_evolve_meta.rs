//! Command handlers and reconciliation-loop advancers for evolution runs,
//! meta-evaluations, and drift adaptations, split out of server.rs's dispatch
//! table.

use super::{
    BOOTSTRAP_ALGORITHM, BTreeMap, BTreeSet, CanaryRequest, CanaryStage, CanaryTransitionKind,
    ChampionRequest, Command, ControlPlane, DRIFT_EVENT_TYPE, DenialEntry, DenialKind,
    DriftAdaptationFinishReason, DriftAdaptationFinishedPayload, DriftAdaptationStartedPayload,
    DriftKind, EVOLUTION_CANCEL_TYPE, EVOLUTION_FINISHED_TYPE, EVOLUTION_GENERATION_TYPE,
    EVOLUTION_STARTED_TYPE, EvaluationInvariantSummary, EvaluationListEntry,
    EvaluationSelectionSummary, EventInput, EventLedger, EvolutionCancelPayload,
    EvolutionCandidateRecord, EvolutionFinishReason, EvolutionFinishedPayload,
    EvolutionGenerationPayload, EvolutionRunRecord, EvolutionRunState, EvolutionStartedPayload,
    EvolverStrategyConfig, ExecuteError, FailureCluster, ForgeAssessmentOutcome,
    GeneSelectionPolicy, JobRecord, JobState, JobTerminal, MAX_LIST_LIMIT, MAX_SOURCE_FILE_BYTES,
    META_EVALUATION_ADMITTED_EVENT_TYPE, META_EVALUATION_EVENT_TYPE, META_STRATEGY_EVENT_TYPE,
    McpDecision, MetaEvaluationAdmittedPayload, MetaEvaluationPayload, MetaEvaluationStatus,
    MetaLineageOutcome, MetaLineageProgress, MetaStrategyRegisteredPayload, MutationPrioritization,
    OPERATOR_ACTOR, RESAMPLES, RecordedCommand, ReferenceInstruction, RegisteredObjects,
    ResponseData, RunCompletionReason, RunListEntry, RunResultReceipt, StoredEvent,
    SuggestedMutation, TRIALS_PER_GENERATION, TraceKind, TraceReceipt, active_evolution_run_id,
    adaptation, adaptation_analysis_id, adaptation_assessment_id, adaptation_canary_id,
    adaptation_diagnostic_evaluation_id, adaptation_projection, adaptation_proposal_id,
    adaptation_shadow_evaluation_id, adaptation_stage_evaluation_id, best_gene_target_operation,
    canary, champion_after, champion_projection, champion_transition_ids_for, decode_drift_record,
    decode_forge_proposal, descendant_verdict, drift, evaluation_record_from_operator, event_type,
    evolution_aggregate_id, evolution_analysis_id, evolution_cancel_event_id,
    evolution_candidate_assessment_id, evolution_candidate_child_evaluation_id,
    evolution_candidate_proposal_id, evolution_diagnostic_evaluation_id,
    evolution_finished_event_id, evolution_generation_event_id, evolution_projection,
    evolution_promotion_transition_id, evolution_started_event_id, finished_event_input,
    forge_assessment_event_id, forge_assessment_summary, forge_event_id,
    invariant_event_references, load_operator_evaluation, meta_evaluation_admitted_event_id,
    meta_evaluation_admitted_ids, meta_evaluation_admitted_projection,
    meta_evaluation_aggregate_id, meta_evaluation_event_id, meta_evaluation_list,
    meta_evaluation_projection, meta_strategy_aggregate_id, meta_strategy_event_id,
    meta_strategy_id, meta_strategy_list, meta_strategy_projection, paired_bootstrap,
    promotions_of, read_source_text, selection_event_references, started_event_input,
    timestamp_millis, verify_drift_adaptation_history, verify_reference_output_invariant_event,
    verify_selection_event,
};

use super::verification::ForgeHypothesisSource;

impl ControlPlane {
    /// Admits (or idempotently re-admits) one autonomous evolution run. All
    /// subsequent progress is made by `advance_evolution`, called every tick
    /// of the daemon's own reconciliation loop, never synchronously here.
    #[allow(clippy::too_many_lines)]
    pub(super) fn evolve_start(&mut self, command: Command) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn evolve_status(&self, run_id: &str) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn evolve_cancel(&mut self, run_id: &str) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn meta_strategy_register(
        &mut self,
        path: &str,
    ) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn meta_strategy_show(
        &self,
        strategy_id: &str,
    ) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn meta_strategy_list_response(&self) -> Result<ResponseData, ExecuteError> {
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

    /// Cooperatively rolls a World's Champion back to `target_genome_id`,
    /// one promotion at a time, so a second strategy's run starts from
    /// exactly the same lineage state as the first. Used only between and
    /// after a meta-evaluation's own paired runs; it never touches a
    /// Champion an operator did not already hand this lineage to `evolve`.
    pub(super) fn rollback_champion_to(
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

    /// Admits a paired meta-evaluation of two Evolver strategies over the
    /// requested held-out base lineages (TD-14). Idempotent on
    /// `meta_run_id`: never blocks on any lineage's evolve run. The daemon's
    /// own reconciliation loop (`advance_meta_evaluations`, called every
    /// tick alongside `evolve` and drift adaptation) drives the existing
    /// evolve engine unmodified and records one replay-verified receipt once
    /// every lineage finishes; poll this command again, or `MetaStatus`, for
    /// progress.
    pub(super) fn meta_evaluate(&mut self, command: Command) -> Result<ResponseData, ExecuteError> {
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
        if let Some(existing) = meta_evaluation_admitted_projection(&history, &meta_run_id)
            .map_err(|_| ExecuteError::Internal)?
        {
            if existing.strategy_a_id != strategy_a_id
                || existing.strategy_b_id != strategy_b_id
                || existing.lineages != lineages
                || existing.confidence_bps != confidence_bps
                || existing.bootstrap_seed != bootstrap_seed
            {
                return Err(ExecuteError::Rejected(
                    "meta_run_id is already bound to a different meta-evaluation".to_owned(),
                ));
            }
            return self.meta_status(&meta_run_id);
        }
        meta_strategy_projection(&history, &strategy_a_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        meta_strategy_projection(&history, &strategy_b_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;

        let payload = MetaEvaluationAdmittedPayload {
            schema_version: 1,
            meta_run_id: meta_run_id.clone(),
            strategy_a_id,
            strategy_b_id,
            lineages,
            confidence_bps,
            bootstrap_seed,
        };
        let payload_value = serde_json::to_value(&payload).map_err(|_| ExecuteError::Internal)?;
        let payload_bytes =
            serde_json::to_vec(&payload_value).map_err(|_| ExecuteError::Internal)?;
        self.storage
            .as_mut()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .append(EventInput::new(
                meta_evaluation_admitted_event_id(&meta_run_id),
                meta_evaluation_aggregate_id(&meta_run_id),
                META_EVALUATION_ADMITTED_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()?;
        self.meta_status(&meta_run_id)
    }

    /// Read-only, replay-verified lookup of one meta-evaluation receipt.
    pub(super) fn meta_show(&self, meta_run_id: &str) -> Result<ResponseData, ExecuteError> {
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

    /// Read-only, replay-verified progress of one meta-evaluation (TD-14):
    /// the recorded receipt once finished, or per-lineage progress while the
    /// daemon's reconciliation loop is still driving it.
    #[allow(clippy::similar_names)]
    pub(super) fn meta_status(&self, meta_run_id: &str) -> Result<ResponseData, ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if let Some(receipt) =
            meta_evaluation_projection(&history, meta_run_id).map_err(|_| ExecuteError::Internal)?
        {
            let lineage_count = receipt.payload.lineages.len();
            return Ok(ResponseData::MetaStatus {
                status: Box::new(MetaEvaluationStatus {
                    meta_run_id: meta_run_id.to_owned(),
                    strategy_a_id: receipt.payload.strategy_a_id.clone(),
                    strategy_b_id: receipt.payload.strategy_b_id.clone(),
                    lineages_total: u32::try_from(lineage_count)
                        .map_err(|_| ExecuteError::Internal)?,
                    lineages_completed: u32::try_from(lineage_count)
                        .map_err(|_| ExecuteError::Internal)?,
                    lineage_progress: vec![MetaLineageProgress::Done; lineage_count],
                    receipt: Some(Box::new(receipt)),
                }),
            });
        }
        let admitted = meta_evaluation_admitted_projection(&history, meta_run_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::NotFound)?;
        let mut lineage_progress = Vec::with_capacity(admitted.lineages.len());
        let mut lineages_completed: u32 = 0;
        for index in 0..admitted.lineages.len() {
            let run_a_state =
                evolution_projection(&history, &format!("meta-{meta_run_id}-a-{index}"))
                    .map_err(|_| ExecuteError::Internal)?
                    .map(|run| run.state);
            let run_b_state =
                evolution_projection(&history, &format!("meta-{meta_run_id}-b-{index}"))
                    .map_err(|_| ExecuteError::Internal)?
                    .map(|run| run.state);
            let progress = match (run_a_state, run_b_state) {
                (None, _) => MetaLineageProgress::Pending,
                (Some(EvolutionRunState::Running), _) => MetaLineageProgress::RunningStrategyA,
                (Some(EvolutionRunState::Finished), None | Some(EvolutionRunState::Running)) => {
                    MetaLineageProgress::RunningStrategyB
                }
                (Some(EvolutionRunState::Finished), Some(EvolutionRunState::Finished)) => {
                    lineages_completed += 1;
                    MetaLineageProgress::Done
                }
            };
            lineage_progress.push(progress);
        }
        Ok(ResponseData::MetaStatus {
            status: Box::new(MetaEvaluationStatus {
                meta_run_id: meta_run_id.to_owned(),
                strategy_a_id: admitted.strategy_a_id,
                strategy_b_id: admitted.strategy_b_id,
                lineages_total: u32::try_from(admitted.lineages.len())
                    .map_err(|_| ExecuteError::Internal)?,
                lineages_completed,
                lineage_progress,
                receipt: None,
            }),
        })
    }

    /// Drives every admitted-but-unfinished meta-evaluation forward, one
    /// durable step at a time, exactly like `advance_evolution` and
    /// `advance_drift_adaptations` (TD-14). Never called from a client
    /// connection.
    pub(super) fn advance_meta_evaluations(&mut self) {
        if self.active_job.is_some() || self.active_arena_job.is_some() {
            return;
        }
        let Some(storage) = self.storage.as_ref() else {
            return;
        };
        let Ok(history) = storage.ledger.replay_verified() else {
            return;
        };
        let Some(meta_run_id) = Self::next_meta_evaluation_needing_advancement(&history) else {
            return;
        };
        if self.state.freeze.is_frozen() {
            return;
        }
        // Busy means another admitted meta-evaluation's lineage run (or an
        // unrelated evolve/Arena job) already claimed this tick's one active
        // job slot; a genuine failure has no terminal "failed" receipt to
        // record today, so it is left to retry on a later tick rather than
        // silently discarding the admission.
        let _ = self.advance_one_meta_evaluation(&meta_run_id);
    }

    /// The oldest admitted meta-evaluation with no recorded receipt yet.
    pub(super) fn next_meta_evaluation_needing_advancement(
        history: &[StoredEvent],
    ) -> Option<String> {
        for meta_run_id in meta_evaluation_admitted_ids(history).ok()? {
            if meta_evaluation_projection(history, &meta_run_id)
                .ok()?
                .is_none()
            {
                return Some(meta_run_id);
            }
        }
        None
    }

    /// Drives one meta-evaluation forward by exactly one durable step
    /// (TD-14): admits the next held-out lineage's next strategy run
    /// (returning `Ok(())` to let `advance_evolution` drive it to completion
    /// on later ticks), rolls a finished run's lineage Champion back to its
    /// starting Genome, or -- once every lineage's paired runs are finished
    /// and rolled back -- computes and appends the final replay-verified
    /// receipt, exactly the computation the previous synchronous
    /// implementation performed inline. Every gate re-derives from
    /// `history`, so a daemon restart mid-evaluation resumes idempotently,
    /// exactly like `evolve` and the drift adaptation pipeline.
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    pub(super) fn advance_one_meta_evaluation(
        &mut self,
        meta_run_id: &str,
    ) -> Result<(), ExecuteError> {
        let history = self
            .storage
            .as_ref()
            .ok_or(ExecuteError::Internal)?
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        if meta_evaluation_projection(&history, meta_run_id)
            .map_err(|_| ExecuteError::Internal)?
            .is_some()
        {
            return Ok(());
        }
        let admitted = meta_evaluation_admitted_projection(&history, meta_run_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::Internal)?;
        let strategy_a = meta_strategy_projection(&history, &admitted.strategy_a_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::Internal)?;
        let strategy_b = meta_strategy_projection(&history, &admitted.strategy_b_id)
            .map_err(|_| ExecuteError::Internal)?
            .ok_or(ExecuteError::Internal)?;

        let mut outcomes = Vec::with_capacity(admitted.lineages.len());
        for (index, lineage) in admitted.lineages.iter().enumerate() {
            let run_a_id = format!("meta-{meta_run_id}-a-{index}");
            let run_b_id = format!("meta-{meta_run_id}-b-{index}");

            let run_a = match evolution_projection(&history, &run_a_id)
                .map_err(|_| ExecuteError::Internal)?
            {
                None => {
                    self.evolve_start(Command::EvolveStart {
                        run_id: run_a_id,
                        world_id: lineage.world_id.clone(),
                        from_genome_id: lineage.from_genome_id.clone(),
                        generations: strategy_a.config.generation_count,
                        budget: strategy_a.config.experiment_allocation,
                        strategy_id: Some(admitted.strategy_a_id.clone()),
                    })?;
                    return Ok(());
                }
                Some(run) if run.state != EvolutionRunState::Finished => return Ok(()),
                Some(run) => run,
            };
            self.rollback_champion_to(
                &run_a_id,
                &lineage.world_id,
                &lineage.from_genome_id,
                strategy_a.config.generation_count,
            )?;

            let run_b = match evolution_projection(&history, &run_b_id)
                .map_err(|_| ExecuteError::Internal)?
            {
                None => {
                    self.evolve_start(Command::EvolveStart {
                        run_id: run_b_id,
                        world_id: lineage.world_id.clone(),
                        from_genome_id: lineage.from_genome_id.clone(),
                        generations: strategy_b.config.generation_count,
                        budget: strategy_b.config.experiment_allocation,
                        strategy_id: Some(admitted.strategy_b_id.clone()),
                    })?;
                    return Ok(());
                }
                Some(run) if run.state != EvolutionRunState::Finished => return Ok(()),
                Some(run) => run,
            };
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
        let quality_delta = paired_bootstrap(
            &quality_deltas,
            admitted.bootstrap_seed,
            admitted.confidence_bps,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let cost_delta = paired_bootstrap(
            &cost_deltas,
            admitted.bootstrap_seed,
            admitted.confidence_bps,
        )
        .map_err(|_| ExecuteError::Internal)?;
        let descendant_cheaper_at_equal_quality = descendant_verdict(
            &admitted.strategy_a_id,
            &strategy_a.config,
            &admitted.strategy_b_id,
            &strategy_b.config,
            &quality_delta,
            &cost_delta,
        );

        let payload = MetaEvaluationPayload {
            schema_version: 1,
            meta_run_id: meta_run_id.to_owned(),
            strategy_a_id: admitted.strategy_a_id,
            strategy_b_id: admitted.strategy_b_id,
            confidence_bps: admitted.confidence_bps,
            bootstrap_seed: admitted.bootstrap_seed,
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
                meta_evaluation_event_id(meta_run_id),
                meta_evaluation_aggregate_id(meta_run_id),
                META_EVALUATION_EVENT_TYPE,
                OPERATOR_ACTOR,
                timestamp_millis().map_err(|_| ExecuteError::Internal)?,
                payload_bytes,
            ))
            .map_err(|_| ExecuteError::Internal)?;
        self.refresh_projection()
    }

    /// Recent meta-evaluation receipts, newest first, bounded by `limit`.
    pub(super) fn meta_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn advance_evolution(&mut self) {
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
    /// propose up to `candidate_count` ranked children (TD-17, roadmap items
    /// 10, 13; exactly one without a bound strategy), evaluate each against
    /// the Champion, check invariants, assess, and promote the
    /// highest-ranked `metrics_passed` child when the deterministic policy
    /// admits it. Every sub-step is idempotent, so re-entering this function
    /// on a later tick (or after a daemon restart) safely resumes exactly
    /// where a prior call left off.
    #[allow(clippy::too_many_lines)]
    pub(super) fn advance_evolution_generation(
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

        let mut sources = self.choose_evolution_hypothesis_sources(
            run,
            generation_index,
            &diagnostic_id,
            &champion_before,
        )?;
        if sources.is_empty() {
            // A strategy-bound run whose failure-cluster analysis suggested
            // no mutation, and the Champion isn't the casing pair either
            // (roadmap items 8, 10, 13): stop rather than proposing an
            // unfounded mutation.
            self.finish_evolution_run(&run_id, EvolutionFinishReason::NoCandidateMutation)?;
            return Ok(());
        }
        // Never propose more candidates than the run's remaining budget can
        // afford (the diagnostic trial already run this generation counts
        // against it): admission already guarantees room for at least one.
        let remaining_after_diagnostic = run
            .max_paired_trials
            .saturating_sub(run.trials_consumed)
            .saturating_sub(1);
        let max_candidates = usize::try_from(remaining_after_diagnostic)
            .unwrap_or(usize::MAX)
            .max(1);
        sources.truncate(max_candidates);

        let mut candidates: Vec<EvolutionCandidateRecord> = Vec::new();
        for (rank_index, source) in sources.into_iter().enumerate() {
            let rank = u32::try_from(rank_index).map_err(|_| ExecuteError::Internal)?;
            let proposal_id = evolution_candidate_proposal_id(&run_id, generation_index, rank);
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

            let child_evaluation_id =
                evolution_candidate_child_evaluation_id(&run_id, generation_index, rank);
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

            let assessment_id = evolution_candidate_assessment_id(&run_id, generation_index, rank);
            let ResponseData::ForgeAssessment { assessment } =
                self.assess_genome(&assessment_id, &proposal_id, &child_selection_event_id)?
            else {
                return Err(ExecuteError::Internal);
            };

            candidates.push(EvolutionCandidateRecord {
                rank,
                proposal_id,
                child_genome_id,
                child_evaluation_id,
                assessment_id,
                outcome: assessment.payload.outcome,
            });
        }

        // The highest-ranked (lowest rank index) `metrics_passed` candidate
        // is promoted, unchanged Champion policy otherwise.
        let promoted_index = candidates
            .iter()
            .position(|candidate| candidate.outcome == ForgeAssessmentOutcome::MetricsPassed);
        let mut promoted = false;
        let mut champion_after = champion_before.clone();
        if let Some(index) = promoted_index {
            let transition_id = evolution_promotion_transition_id(&run_id, generation_index);
            if self
                .transition_champion(
                    &transition_id,
                    &ChampionRequest::Promote {
                        assessment_id: candidates[index].assessment_id.clone(),
                    },
                )
                .is_ok()
            {
                promoted = true;
                champion_after.clone_from(&candidates[index].child_genome_id);
            }
        }

        // The top-level fields always describe the promoted candidate, or
        // rank 0 when none was promoted, whether or not `candidates` is
        // populated (kept unpopulated -- and the top-level fields exactly
        // today's single-candidate ids -- whenever exactly one candidate was
        // proposed, so that generation's payload stays byte-for-byte
        // identical to today's behavior).
        let chosen_index = if promoted {
            promoted_index.unwrap_or(0)
        } else {
            0
        };
        let chosen = candidates[chosen_index].clone();
        let candidates_field = if candidates.len() <= 1 {
            Vec::new()
        } else {
            candidates
        };

        self.record_evolution_generation(&EvolutionGenerationPayload {
            schema_version: 1,
            run_id,
            generation_index,
            champion_before,
            diagnostic_evaluation_id: diagnostic_id,
            proposal_id: chosen.proposal_id,
            child_genome_id: chosen.child_genome_id,
            child_evaluation_id: chosen.child_evaluation_id,
            assessment_id: chosen.assessment_id,
            promoted,
            champion_after,
            candidates: candidates_field,
        })
    }

    /// Chooses this generation's ranked, distinct-by-target-operation Forge
    /// hypothesis sources, highest priority first (rank `0`). Without a
    /// bound strategy, this is always exactly one source: the historical
    /// default, an operator-authored hypothesis proposing the
    /// `identity`/`ascii_uppercase` flip (unchanged behavior for every
    /// existing evolve run).
    ///
    /// With a bound strategy (roadmap items 8, 10, 13): runs `forge analyze`
    /// on the diagnostic evaluation (the Champion is the analyzed
    /// candidate), orders its failure clusters by the strategy's
    /// `mutation_prioritization` (`Fifo` keeps the clusters' stable
    /// signature order; `CostWeighted` sorts by descending `total_count`,
    /// ties by signature), and puts first whichever cluster's suggestion the
    /// Gene Bank prefers when `gene_selection == HighestTransferEffect`.
    /// Every cluster's primary `suggested_mutation` is then considered in
    /// that order, followed by every cluster's `secondary_suggested_mutation`
    /// (TD-17); a target operation already chosen at a higher rank is
    /// skipped, so no two returned sources ever propose the same operation.
    /// The result is truncated to the strategy's `candidate_count` (`1`
    /// without a strategy). An empty result means no cluster suggested a
    /// mutation and the Champion is not the casing pair either, so the
    /// caller finishes the run with `NoCandidateMutation` instead of
    /// proposing anything.
    #[allow(clippy::too_many_lines)]
    pub(super) fn choose_evolution_hypothesis_sources(
        &mut self,
        run: &EvolutionRunRecord,
        generation_index: u32,
        diagnostic_id: &str,
        champion_before: &str,
    ) -> Result<Vec<ForgeHypothesisSource>, ExecuteError> {
        let run_id = run.run_id.clone();
        let default_hypothesis = || {
            ForgeHypothesisSource::Operator(format!(
                "Evolve run {run_id} generation {generation_index}: flip the reference \
                 operation of Champion {champion_before} to explore the paired instruction \
                 space."
            ))
        };
        let Some(strategy_id) = run.strategy_id.clone() else {
            return Ok(vec![default_hypothesis()]);
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

        let suggestion_of = |mutation: Option<&SuggestedMutation>| match mutation {
            Some(SuggestedMutation::ReferenceOperation { operation_after }) => {
                Some(operation_after.clone())
            }
            _ => None,
        };

        let champion_operation = self
            .reference_instruction(champion_before)?
            .map(ReferenceInstruction::operation_name);

        // Ordered, deduplicated-by-target-operation candidates: (cluster
        // index, whether the secondary suggestion is used).
        let mut ranked: Vec<(u32, bool)> = Vec::new();
        let mut seen_operations: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        let mut push = |index: u32, use_secondary: bool, operation: String| {
            if seen_operations.insert(operation) {
                ranked.push((index, use_secondary));
            }
        };

        if strategy.config.gene_selection == GeneSelectionPolicy::HighestTransferEffect {
            let preferred = champion_operation
                .and_then(|operation| best_gene_target_operation(&history, operation));
            if let Some(preferred_op) = preferred {
                if let Some((index, _)) = clusters.iter().find(|(_, cluster)| {
                    suggestion_of(cluster.suggested_mutation.as_ref()).as_deref()
                        == Some(preferred_op.as_str())
                }) {
                    push(*index, false, preferred_op);
                }
            }
        }
        for (index, cluster) in &clusters {
            if let Some(operation) = suggestion_of(cluster.suggested_mutation.as_ref()) {
                push(*index, false, operation);
            }
        }
        for (index, cluster) in &clusters {
            if let Some(operation) = suggestion_of(cluster.secondary_suggested_mutation.as_ref()) {
                push(*index, true, operation);
            }
        }

        if ranked.is_empty() {
            // No cluster suggested anything: fall back to today's casing
            // flip when the Champion runs one of the two casing operations,
            // exactly like a strategy-less run would.
            if matches!(champion_operation, Some("identity" | "ascii_uppercase")) {
                return Ok(vec![default_hypothesis()]);
            }
            return Ok(Vec::new());
        }

        let candidate_count = usize::try_from(strategy.config.candidate_count)
            .unwrap_or(usize::MAX)
            .max(1);
        Ok(ranked
            .into_iter()
            .take(candidate_count)
            .map(
                |(cluster_index, use_secondary)| ForgeHypothesisSource::Analysis {
                    analysis_id: analysis_id.clone(),
                    cluster_index,
                    use_secondary,
                },
            )
            .collect())
    }

    pub(super) fn record_evolution_generation(
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

    pub(super) fn finish_evolution_run(
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
        // `run.trials_consumed` only sums recorded `evolution.generation`
        // events. A `NoCandidateMutation` finish records no generation event
        // for the attempt that diagnosed there was no candidate mutation, so
        // its one diagnostic trial is added back here (TD-22).
        let trials_consumed = if reason == EvolutionFinishReason::NoCandidateMutation {
            run.trials_consumed.saturating_add(1)
        } else {
            run.trials_consumed
        };
        let payload = EvolutionFinishedPayload {
            schema_version: 1,
            run_id: run_id.to_owned(),
            generations_completed,
            trials_consumed,
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
    pub(super) fn advance_drift_adaptations(&mut self) {
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
    pub(super) fn next_drift_needing_adaptation(
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

    /// Chooses the Forge hypothesis source a drift-triggered adaptation
    /// proposes from, deriving it from the diagnostic evaluation's own
    /// failure-cluster analysis through the same mutation catalog path a
    /// strategy-bound `evolve` run uses (TD-18; see
    /// `choose_evolution_hypothesis_sources`): the first cluster (in stable
    /// signature order) with a `SuggestedMutation::ReferenceOperation`, or
    /// else the historical `identity`/`ascii_uppercase` flip when the
    /// Champion runs one of those two operations and no cluster suggested
    /// anything. `Ok(None)` means no candidate mutation exists at all.
    pub(super) fn choose_adaptation_hypothesis_source(
        &mut self,
        drift_id: &str,
        diagnostic_id: &str,
        drift_kind: DriftKind,
        champion_before: &str,
    ) -> Result<Option<ForgeHypothesisSource>, ExecuteError> {
        let analysis_id = adaptation_analysis_id(drift_id);
        let ResponseData::ForgeAnalysis { analysis } =
            self.analyze_forge_clusters(&analysis_id, diagnostic_id)?
        else {
            return Err(ExecuteError::Internal);
        };

        let is_operation_suggestion = |mutation: Option<&SuggestedMutation>| {
            matches!(mutation, Some(SuggestedMutation::ReferenceOperation { .. }))
        };

        let mut chosen: Option<(u32, bool)> = None;
        for (index, cluster) in analysis.analysis.clusters.iter().enumerate() {
            if is_operation_suggestion(cluster.suggested_mutation.as_ref()) {
                chosen = Some((
                    u32::try_from(index).map_err(|_| ExecuteError::Internal)?,
                    false,
                ));
                break;
            }
        }
        if chosen.is_none() {
            for (index, cluster) in analysis.analysis.clusters.iter().enumerate() {
                if is_operation_suggestion(cluster.secondary_suggested_mutation.as_ref()) {
                    chosen = Some((
                        u32::try_from(index).map_err(|_| ExecuteError::Internal)?,
                        true,
                    ));
                    break;
                }
            }
        }

        let Some((cluster_index, use_secondary)) = chosen else {
            let champion_operation = self
                .reference_instruction(champion_before)?
                .map(ReferenceInstruction::operation_name);
            if matches!(champion_operation, Some("identity" | "ascii_uppercase")) {
                return Ok(Some(ForgeHypothesisSource::Operator(format!(
                    "Drift adaptation for drift {drift_id} ({drift_kind:?}): flip the reference \
                     operation of Champion {champion_before} to address the recorded drift."
                ))));
            }
            return Ok(None);
        };
        Ok(Some(ForgeHypothesisSource::Analysis {
            analysis_id,
            cluster_index,
            use_secondary,
        }))
    }

    /// Advances one drift's adaptation by exactly one durable step: submits
    /// at most one fresh Arena job (returning `Ok(())` to wait for its
    /// completion on a later tick), or appends at most one new event. Every
    /// gate re-derives from `history`, so a daemon restart mid-pipeline
    /// resumes idempotently.
    #[allow(clippy::too_many_lines)]
    pub(super) fn advance_one_drift_adaptation(
        &mut self,
        drift_id: &str,
    ) -> Result<(), ExecuteError> {
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
            let Some(hypothesis_source) = self.choose_adaptation_hypothesis_source(
                drift_id,
                &diagnostic_id,
                drift.payload.kind,
                &started.champion_genome_id,
            )?
            else {
                return self.finish_drift_adaptation(
                    drift_id,
                    DriftAdaptationFinishReason::NoCandidateMutation,
                    None,
                );
            };
            let ResponseData::ForgeProposal { proposal } = self.propose_genome_from_source(
                &started.proposal_id,
                &selection_event_id,
                &started.champion_genome_id,
                hypothesis_source,
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
    pub(super) fn finish_drift_adaptation(
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
    pub(super) fn run_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn evaluation_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn evaluation_selection_summary(
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

    pub(super) fn evaluation_invariant_summary(
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
    pub(super) fn denial_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
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
}
