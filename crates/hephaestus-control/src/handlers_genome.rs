//! Command handlers for genome/champion/drift/canary/gene commands, split out
//! of server.rs's dispatch table.

use super::{
    CHAMPION_EVENT_TYPE, CONTRADICTION_EVENT_TYPE, CanaryRequest, CanaryStage,
    CanaryTransitionKind, ChampionRequest, Command, ControlPlane, DriftKind, EventInput,
    EventLedger, ExecuteError, GENE_EVENT_TYPE, OPERATOR_ACTOR, ResponseData, SPECIES_EVENT_TYPE,
    TRANSFER_APPLIED_EVENT_TYPE, TRANSFER_RECORDED_EVENT_TYPE, canary, canary_transition_payload,
    champion_aggregate_id, champion_event_id, champion_projection, champion_transition_payload,
    champion_transition_record, contradiction_event_id, decode_transfer_applied,
    detect_contradiction, drift, drift_event_input, drift_record_payload,
    existing_canary_transition, existing_champion_transition, existing_contradiction,
    existing_drift_record, existing_gene, existing_species, existing_transfer_applied,
    existing_transfer_recorded, gene_aggregate, gene_aggregate_id, gene_event_id,
    gene_extraction_payload, gene_record, gene_summaries, hex_encode, speciation_payload,
    species_aggregate_id, species_event_id, species_record, timestamp_millis,
    transfer_aggregate_id, transfer_applied_event_id, transfer_applied_payload, transfer_record,
    transfer_recorded_event_id, transfer_recorded_payload, validate_job_id, verify_canary_history,
    verify_champion_history, verify_drift_history,
};

use super::verification::ForgeHypothesisSource;

impl ControlPlane {
    pub(super) fn propose_genome_command(
        &mut self,
        command: Command,
    ) -> Result<ResponseData, ExecuteError> {
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
                use_secondary: false,
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

    pub(super) fn assess_genome_command(
        &mut self,
        command: Command,
    ) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn champion_transition_command(
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

    pub(super) fn transition_champion(
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

    pub(super) fn champion_show(&self, world_id: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn record_drift(
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
            Some(canary::CURRENT_LATENCY_RULE),
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

    pub(super) fn drift_show(&self, drift_id: &str) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn drift_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let drifts = drift::drift_list(&history, limit).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::DriftList { drifts })
    }

    pub(super) fn canary_transition_command(
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
    pub(super) fn transition_canary(
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
            Some(canary::CURRENT_LATENCY_RULE),
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

    pub(super) fn canary_show(&self, canary_id: &str) -> Result<ResponseData, ExecuteError> {
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
    pub(super) fn canary_list(&self, limit: u32) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let canaries = canary::canary_list(&history, limit).map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::CanaryList { canaries })
    }

    pub(super) fn gene_extract(
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

    pub(super) fn gene_transfer_apply(
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

    pub(super) fn gene_transfer_record(
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

    pub(super) fn gene_show(&self, gene_id: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn gene_list(&self) -> Result<ResponseData, ExecuteError> {
        let storage = self.storage.as_ref().ok_or(ExecuteError::Internal)?;
        let history = storage
            .ledger
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        Ok(ResponseData::Genes {
            genes: gene_summaries(&history)?,
        })
    }

    pub(super) fn gene_speciate(
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
}
