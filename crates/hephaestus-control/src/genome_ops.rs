//! World/genome registration, proposal, and assessment command handlers, plus
//! projection refresh, split out of server.rs.

use super::{
    ArtifactBackend, ArtifactId, BTreeMap, ControlPlane, ControlState, EventIndex, EventInput,
    EventLedger, ExecuteError, ForgeProposalPayload, GenomeRecord, MAX_ARTIFACT_FILE_BYTES,
    MAX_SOURCE_FILE_BYTES, MUTATION_CATALOG_VERSION, OPERATOR_ACTOR, Path, RegisteredObjects,
    ResponseData, TrustedManifest, WorldRecord, compile_forge_child, compile_genome,
    compile_markdown_genome, compile_world, existing_forge_assessment_response,
    existing_forge_response, forge_aggregate_id, forge_assessment_event_id,
    forge_assessment_payload, forge_assessment_record, forge_event_id, forge_prompt_mutation,
    forge_proposal_record, hex_encode, mutation_edge_kind, read_bounded_file, read_source_text,
    reference_instruction_operation, resolve_forge_hypothesis, source_format, timestamp_millis,
    validate_job_id, verified_forge_source, verify_arena_evaluation_records_with,
    verify_canary_history_with, verify_champion_history_with, verify_cluster_history,
    verify_drift_adaptation_history, verify_drift_history_with, verify_evolution_history,
    verify_forge_assessment_history, verify_forge_assessment_history_with, verify_forge_history,
    verify_forge_history_with, verify_gene_bank_history_with, verify_invariant_history_with,
    verify_meta_evolution_history, verify_selection_history_with,
};

use super::verification::ForgeHypothesisSource;

impl ControlPlane {
    pub(super) fn register_world(&mut self, path: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn register_genome(
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
    pub(super) fn propose_genome(
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
    pub(super) fn propose_genome_from_source(
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

    pub(super) fn assess_genome(
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

    pub(super) fn genome_prompt(&self, genome_id: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn put_manifest(&mut self, path: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn put_artifact(&mut self, path: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn verifier_show(&mut self) -> Result<ResponseData, ExecuteError> {
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
    ///
    /// Rebuilding `ControlState`/`RegisteredObjects` incrementally from only the
    /// new tail of `history` (instead of `from_events`'s full rebuild) was tried
    /// and reverted: several call sites elsewhere in this file append one event
    /// and apply it to `self.state` directly (`append_run_result`,
    /// `append_job_record`, `append_arena_job_record`, `append_audit`,
    /// `worker_credential_mint`, remote-job admission) without going through a
    /// refresh first, including after the synchronous run path holds `storage`
    /// (and so cannot refresh) through an entire run's worth of recorder-written
    /// trace events. That advances `self.state.event_count` past those trace
    /// events without ever applying them, so a later refresh keyed off
    /// `event_count` treats them as already covered and never applies them
    /// (`completed_runs` observably never gains the run - see the reverted
    /// commit's failing `synchronous_*_run_*_persists_signed_provenance_and_replays`
    /// tests). Naively replaying the "new" tail again isn't safe either: several
    /// of those same event types are rejected as duplicates or invalid
    /// transitions on a second `apply` (e.g. `remote_worker.job_admitted`'s
    /// duplicate-job-id check), so double-applying an already-directly-applied
    /// event errors instead of being a no-op. A safe incremental rebuild needs a
    /// cursor that is provably contiguous despite those shortcuts (or removing
    /// the shortcuts), which is a bigger, separate change (TD-20).
    pub(super) fn refresh_projection(&mut self) -> Result<(), ExecuteError> {
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
