//! Run-record and arena-job validation: RunRecord/ProjectionSnapshot types,
//! arena job admission checks, trace/run-result receipt validation, and
//! run-completion-reason mapping, split out of server.rs.

use super::{
    ArenaJobPhase, ArenaJobRecord, ArtifactBackend, ArtifactId, BTreeMap, Budget, Command,
    CompletionReason, ControlError, Deserialize, Duration, ExecuteError, File, FileExt,
    FileTypeExt, GenomeRecord, JobProgress, JobRecord, JobState, MAX_EVALUATION_COST_MICROUSD,
    MAX_EVALUATION_OUTPUT_BYTES, MAX_EVALUATION_WALL_MILLIS, OpenOptions, OpenOptionsExt,
    PAIRED_EVALUATION_SEED, Path, PathBuf, PermissionsExt, ProcessCommand, Provider,
    RUN_RESULT_SCHEMA_VERSION, RUNTIME_ACTOR, Read, RegisteredObjects, RegistrationError,
    RunCompletionReason, RunResultReceipt, RunResultSigner, RunResultVerifier, Serialize,
    StoredEvent, SystemTime, TraceKind, TraceReceipt, UNIX_EPOCH, WorldRecord, Write, env, fs,
    paired_run_id, validate_job_id,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunRecord {
    pub(super) run_id: String,
}

#[derive(Eq, PartialEq, Serialize)]
pub(super) struct ProjectionSnapshot {
    pub(super) frozen: bool,
    pub(super) active_runs: Vec<String>,
    pub(super) genomes: BTreeMap<String, GenomeRecord>,
    pub(super) worlds: BTreeMap<String, WorldRecord>,
    pub(super) jobs: BTreeMap<String, JobRecord>,
    pub(super) arena_jobs: BTreeMap<String, ArenaJobRecord>,
    pub(super) job_progress: BTreeMap<String, JobProgress>,
    pub(super) evaluation_events: BTreeMap<String, u64>,
    pub(super) run_results: BTreeMap<String, RunResultReceipt>,
    pub(super) completed_runs: Vec<String>,
    pub(super) event_count: u64,
}

#[allow(clippy::too_many_lines)]
pub(super) fn validate_arena_job_record(
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
    validate_arena_environment_id(&record.environment_id)?;
    if let Some(candidate_environment_id) = &record.candidate_environment_id {
        if candidate_environment_id == &record.environment_id {
            // A mixed-pair field must actually be distinct; use `None`
            // instead of restating the shared environment.
            return Err(ControlError::Projection(
                "Arena candidate environment must differ from the parent's".to_owned(),
            ));
        }
        validate_arena_environment_id(candidate_environment_id)?;
    }
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
        &record.effective_candidate_environment_id(),
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
pub(super) fn test_overall_wall(maximum: u64, requested: Option<&str>) -> Result<u64, ()> {
    let Some(requested) = requested else {
        return Ok(maximum);
    };
    let requested = requested.parse::<u64>().map_err(|_| ())?;
    if requested == 0 || requested > maximum {
        return Err(());
    }
    Ok(requested)
}

pub(super) fn arena_job_immutable_fields_match(old: &ArenaJobRecord, new: &ArenaJobRecord) -> bool {
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
        && old.candidate_environment_id == new.candidate_environment_id
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

pub(super) fn validate_trace_receipt(
    event: &StoredEvent,
    receipt: &TraceReceipt,
) -> Result<(), ControlError> {
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

pub(super) const fn trace_phase(kind: TraceKind) -> &'static str {
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
pub(super) fn validate_run_result(
    event: &StoredEvent,
    run_result_verifier: &RunResultVerifier,
) -> Result<(), ControlError> {
    RunResultReceipt::parse_from_event(event, run_result_verifier)
        .map(|_| ())
        .map_err(|_| ControlError::Projection("canonical run result is invalid".to_owned()))
}

pub(super) fn receipt_matches_job(receipt: &RunResultReceipt, job: &JobRecord) -> bool {
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

pub(super) fn trace_artifacts_for_run(
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

pub(super) fn run_completion_reason(reason: CompletionReason) -> RunCompletionReason {
    reason.into()
}

/// Validates a job/Arena environment identity's versioned shape: either the
/// reference worker's `reference-v1.<digest>` or a provider's
/// `provider-v1.<digest>` or `provider-v2.<digest>` (see `provider_job_environment`).
fn validate_arena_environment_id(environment_id: &str) -> Result<(), ControlError> {
    let digest = environment_id
        .strip_prefix("reference-v1.")
        .or_else(|| provider_environment_digest(environment_id))
        .ok_or_else(|| ControlError::Projection("Arena environment is invalid".to_owned()))?;
    ArtifactId::parse(digest.to_owned())?;
    Ok(())
}

/// Historical provider records retain v1; new instruction delivery uses v2.
pub(super) fn provider_environment_digest(environment_id: &str) -> Option<&str> {
    environment_id
        .strip_prefix("provider-v1.")
        .or_else(|| environment_id.strip_prefix("provider-v2."))
}

pub(super) fn validate_content_id<'a>(
    value: &'a str,
    namespace: &str,
) -> Result<&'a str, ControlError> {
    let prefix = format!("hephaestus:{namespace}:");
    let Some(hash) = value.strip_prefix(&prefix) else {
        return Err(ControlError::Projection(format!(
            "canonical {namespace} identity is malformed"
        )));
    };
    ArtifactId::parse(hash.to_owned())?;
    Ok(hash)
}

pub(super) fn require_projection_text(value: &str, field: &str) -> Result<(), ControlError> {
    if value.trim().is_empty() {
        return Err(ControlError::Projection(format!(
            "canonical {field} is empty"
        )));
    }
    Ok(())
}

pub(super) fn event_type(command: &Command) -> &'static str {
    match command {
        Command::Status => "control.status",
        Command::Freeze => "control.freeze",
        Command::Unfreeze => "control.unfreeze",
        Command::KillAll => "control.kill_all",
        Command::GenomeShow { .. } => "control.genome_show",
        Command::GenomePrompt { .. } => "control.genome_prompt",
        Command::GenomeProfile { .. } => "control.genome_profile",
        Command::GenomeProposalShow { .. } => "control.genome_proposal_show",
        Command::GenomeList => "control.genome_list",
        Command::GenomeRegister { .. } => "control.genome_register",
        Command::GenomePropose { .. } => "control.genome_propose",
        Command::GenomeRevise { .. } => "control.genome_revise",
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
        Command::EvaluatePairConfirmed { .. } => "control.evaluate_pair_confirmed",
        Command::ArenaSelect { .. } => "control.arena_select",
        Command::ArenaSelectionShow { .. } => "control.arena_selection_show",
        Command::ArenaInvariants { .. } => "control.arena_invariants",
        Command::ChampionSeed { .. } => "control.champion_seed",
        Command::ChampionPromote { .. } => "control.champion_promote",
        Command::ChampionRollback { .. } => "control.champion_rollback",
        Command::ChampionShow { .. } => "control.champion_show",
        Command::DriftRecord { .. } => "control.drift_record",
        Command::DriftShow { .. } => "control.drift_show",
        Command::DriftList { .. } => "control.drift_list",
        Command::CanaryStart { .. } => "control.canary_start",
        Command::CanaryAdvance { .. } => "control.canary_advance",
        Command::CanaryLiveCheck { .. } => "control.canary_live_check",
        Command::CanaryShow { .. } => "control.canary_show",
        Command::CanaryList { .. } => "control.canary_list",
        Command::GeneExtract { .. } => "control.gene_extract",
        Command::GeneTransfer { .. } => "control.gene_transfer",
        Command::GeneRecord { .. } => "control.gene_record",
        Command::GeneShow { .. } => "control.gene_show",
        Command::GeneList => "control.gene_list",
        Command::GeneSpeciate { .. } => "control.gene_speciate",
        Command::EvolveStart { .. } => "control.evolve_start",
        Command::EvolveStatus { .. } => "control.evolve_status",
        Command::EvolveCancel { .. } => "control.evolve_cancel",
        Command::MetaStrategyRegister { .. } => "control.meta_strategy_register",
        Command::MetaStrategyShow { .. } => "control.meta_strategy_show",
        Command::MetaStrategyList => "control.meta_strategy_list",
        Command::MetaEvaluate { .. } => "control.meta_evaluate",
        Command::MetaShow { .. } => "control.meta_show",
        Command::MetaStatus { .. } => "control.meta_status",
        Command::MetaList { .. } => "control.meta_list",
        Command::Replay => "control.replay",
        Command::RunList { .. } => "control.run_list",
        Command::EvaluationList { .. } => "control.evaluation_list",
        Command::DenialList { .. } => "control.denial_list",
        Command::DaemonStop => "control.daemon_stop",
        Command::McpCall { .. } => "mcp.call",
        Command::WorkerCredentialMint { .. } => "control.worker_credential_mint",
        Command::WorkerCredentialRevoke { .. } => "control.worker_credential_revoke",
        Command::RemoteRunSubmit { .. } => "control.remote_run_submit",
        Command::RemoteJobStatus { .. } => "control.remote_job_status",
    }
}

pub(super) fn provider_execution_environment(provider: Provider) -> String {
    let version = hephaestus_runtime::PROVIDER_INPUT_VERSION;
    let name = match provider {
        Provider::Codex => "codex-cli",
        Provider::Claude => "claude-cli",
        Provider::Deterministic => "deterministic",
    };
    format!(
        "{name}-v{version}.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git",
        env!("CARGO_PKG_VERSION"),
        RUN_RESULT_SCHEMA_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

pub(super) fn reference_environment_id() -> String {
    format!(
        "deterministic-v1.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git",
        env!("CARGO_PKG_VERSION"),
        RUN_RESULT_SCHEMA_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

pub(super) fn validated_evaluation_budget(
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

pub(super) fn validate_source_repository(path: &Path) -> Result<PathBuf, ControlError> {
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

pub(super) fn timestamp_millis() -> Result<i64, ControlError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ControlError::Protocol("system clock precedes Unix epoch"))?
        .as_millis();
    i64::try_from(millis).map_err(|_| ControlError::Protocol("timestamp exceeds i64"))
}

pub(super) fn prepare_private_directory(path: &Path) -> Result<(), ControlError> {
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

pub(super) fn prepare_private_file(path: &Path) -> Result<(), ControlError> {
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

pub(super) fn take_writer_lock(path: &Path) -> Result<File, ControlError> {
    prepare_private_file(path)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.try_lock_exclusive()
        .map_err(|_| ControlError::AlreadyRunning)?;
    Ok(file)
}

pub(super) fn load_or_create_token(path: &Path) -> Result<(String, [u8; 32]), ControlError> {
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

pub(super) fn load_or_create_run_result_signer(
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

pub(super) fn reject_legacy_run_result_history(
    history: &[StoredEvent],
) -> Result<(), ControlError> {
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

pub(super) fn anchored_world_verifier(
    registered: &RegisteredObjects,
    artifacts: &dyn ArtifactBackend,
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

pub(super) fn registration_control_error(error: RegistrationError) -> ControlError {
    match error {
        RegistrationError::Ledger(error) => ControlError::Ledger(error),
        error => ControlError::Projection(error.to_string()),
    }
}

pub(super) fn hex_encode(bytes: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(super) fn hex_decode(value: &str) -> Result<[u8; 32], ControlError> {
    if value.len() != 64 {
        return Err(ControlError::Protocol("operator token is malformed"));
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

pub(super) fn hex_nibble(byte: u8) -> Result<u8, ControlError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ControlError::Protocol("operator token is malformed")),
    }
}

pub(super) fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
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

pub(super) fn remove_stale_socket(path: &Path) -> Result<(), ControlError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => return Err(ControlError::Protocol("control socket path is unsafe")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod provider_version_tests {
    use super::*;
    use crate::ControlPlane;

    #[test]
    fn provider_environment_versions_accept_legacy_and_current_reject_unknown() {
        let digest = ArtifactId::for_bytes(b"provider executable fixture");
        let legacy = format!("provider-v1.{}", digest.as_str());
        assert_eq!(provider_environment_digest(&legacy), Some(digest.as_str()));
        assert!(validate_arena_environment_id(&legacy).is_ok());
        for provider in [Provider::Codex, Provider::Claude] {
            let current = ControlPlane::provider_job_environment(provider, digest.as_str());
            assert!(current.starts_with("provider-v2."));
            assert!(provider_environment_digest(&current).is_some());
            assert!(validate_arena_environment_id(&current).is_ok());
        }
        for invalid in [
            format!("provider-v0.{}", digest.as_str()),
            format!("provider-v3.{}", digest.as_str()),
            "provider-v2.invalid".to_owned(),
        ] {
            assert!(validate_arena_environment_id(&invalid).is_err());
        }
    }
}
