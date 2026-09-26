//! Worker-credential, remote-job, and daemon-stop command handlers, split out
//! of server.rs's dispatch table.

use super::{
    ArtifactBackend, ArtifactId, Command, ControlPlane, EventInput, EventLedger, ExecuteError,
    File, Instant, OPERATOR_ACTOR, REMOTE_LEASE_TIMEOUT, REMOTE_REFERENCE_PROMPT,
    RUN_RESULT_SCHEMA_VERSION, Read, RemoteCompletion, RemoteJobRecord, RemoteJobState,
    ResponseData, RunBudgetReceipt, RunCompletionReason, RunResultReceipt, WorkerCredentialRecord,
    WorkerReply, WorkerRequest, WorkerScope, active_evolution_run_id, hex_decode, hex_decode_bytes,
    hex_encode, hex_encode_bytes, reference_environment_id, remote_job_run_id,
    resolve_source_revision, timestamp_millis,
};

impl ControlPlane {
    pub(super) fn request_daemon_stop(&mut self) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn worker_credential_mint(
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

    pub(super) fn worker_credential_revoke(
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

    pub(super) fn remote_run_submit(
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

    pub(super) fn remote_job_status(&self, job_id: &str) -> Result<ResponseData, ExecuteError> {
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

    pub(super) fn verify_worker_credential(
        &self,
        worker_id: &str,
        token: &str,
    ) -> Result<(), &'static str> {
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

    pub(super) fn lease_remote_job(&mut self) -> WorkerReply {
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
    pub(super) fn record_remote_job_result(
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

    pub(super) fn require_no_active_job_for_sync_work(
        &self,
        command: &Command,
    ) -> Result<(), ExecuteError> {
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
    pub(super) fn require_no_active_evolution_for_external_command(
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
}
