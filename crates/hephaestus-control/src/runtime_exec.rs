//! Async reference/candidate/provider execution helpers, executable/digest
//! resolution, and run-id/hex encoding utilities, split out of server.rs.

use super::{
    Arc, ArenaTrialSpec, ArenaWorkerMessage, ArtifactBackend, AsyncArenaTrialLaunch,
    AsyncReferenceLaunch, CapabilityToken, CompletionReason, ControlError, DeterministicRuntime,
    Duration, EvidenceRecorder, ExecuteError, GenomeRecord, Instant, IsolationPolicy, Ordering,
    Path, PathBuf, PermissionsExt, ProcessCommand, Provider, RecordedRuntime, RedactionPolicy,
    RemoteArenaLeaseQueue, RemoteCompletion, ResponseData, RunCompletionReason, RunSpec, RunStatus,
    RuntimeAdapter, Sandbox, SandboxCleanupGuard, SandboxManager, SandboxManagerSource,
    SourceFormat, SupervisedRuntime, env, extract_actual_cost_microusd, extract_final_answer, fs,
    mpsc, run_completion_reason, thread, trace_artifacts_for_run,
};

pub(super) fn source_format(path: &str) -> Result<SourceFormat, ExecuteError> {
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

pub(super) fn read_source_text(path: &str, limit: u64) -> Result<String, ExecuteError> {
    String::from_utf8(read_bounded_file(path, limit)?)
        .map_err(|_| ExecuteError::Invalid("source file is not UTF-8 text"))
}

pub(super) fn read_bounded_file(path: &str, limit: u64) -> Result<Vec<u8>, ExecuteError> {
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

pub(super) struct ReferenceExecution {
    completion_reason: RunCompletionReason,
    latency_millis: u64,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    trace_artifact_ids: Vec<String>,
    /// Exact cost the provider reported, in micro-US-dollars. Always zero for
    /// the reference worker and for a provider stream that reports none.
    actual_cost_microusd: u64,
}

pub(super) fn execute_async_reference(
    launch: AsyncReferenceLaunch,
    spec: &RunSpec,
    evidence: hephaestus_experience::ChannelEvidenceSink,
    initial_sequence: u64,
) -> Result<ReferenceExecution, String> {
    let AsyncReferenceLaunch {
        sandboxes,
        guardian,
        protected_paths,
        worker,
        cancel,
    } = launch;
    worker
        .verify()
        .map_err(|_| "reference worker identity check failed".to_owned())?;
    let manager = sandboxes.resolve()?;
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
                    actual_cost_microusd: 0,
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

pub(super) struct AsyncProviderLaunch {
    pub(super) sandboxes: SandboxManagerSource,
    pub(super) guardian: PathBuf,
    pub(super) protected_paths: Vec<PathBuf>,
    pub(super) provider: Provider,
    pub(super) executable: PathBuf,
    pub(super) extra_env: Vec<(String, String)>,
    pub(super) redaction: RedactionPolicy,
    pub(super) cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// Async counterpart of `execute_async_reference` for a Codex/Claude adapter:
/// same private sandbox, process-group supervision, cancellation polling, and
/// streamed evidence sink, but the terminal stdout goes through
/// `extract_final_answer`/`extract_actual_cost_microusd` and redaction the
/// same way the synchronous `execute_provider_runtime` path does.
pub(super) fn execute_async_provider(
    launch: AsyncProviderLaunch,
    spec: &RunSpec,
    evidence: hephaestus_experience::ChannelEvidenceSink,
    initial_sequence: u64,
) -> Result<ReferenceExecution, String> {
    let AsyncProviderLaunch {
        sandboxes,
        guardian,
        protected_paths,
        provider,
        executable,
        extra_env,
        redaction,
        cancel,
    } = launch;
    let manager = sandboxes.resolve()?;
    let (sandbox, token) = manager
        .create(spec)
        .map_err(|_| "sandbox could not be created".to_owned())?;
    let sandbox = SandboxCleanupGuard::new(sandbox);
    let runtime = SupervisedRuntime::provider_guarded(
        candidate_isolation(protected_paths),
        provider,
        executable,
        &guardian,
        extra_env,
    )
    .and_then(|runtime| with_provider_login(runtime, provider))
    .map_err(|_| "guarded provider could not be configured".to_owned())?;
    let mut runtime = RecordedRuntime::with_sink(runtime, evidence, initial_sequence);
    let result = (|| {
        runtime
            .start(
                spec,
                sandbox.sandbox().map_err(|_| "sandbox unavailable")?,
                &token,
            )
            .map_err(|_| "guarded provider did not start".to_owned())?;
        loop {
            if cancel.load(Ordering::Acquire) {
                runtime
                    .interrupt(spec.run_id())
                    .map_err(|_| "guarded provider did not confirm cancellation".to_owned())?;
            }
            let snapshot = runtime
                .snapshot(spec.run_id())
                .map_err(|error| format!("guarded provider status failed: {error}"))?;
            if snapshot.status != RunStatus::Running {
                let completion_reason = snapshot
                    .completion_reason
                    .ok_or_else(|| "terminal provider omitted completion reason".to_owned())?;
                let raw_stdout = fs::read(&snapshot.stdout_path)
                    .map_err(|_| "provider output could not be read".to_owned())?;
                let raw_stderr = fs::read(&snapshot.stderr_path)
                    .map_err(|_| "provider diagnostics could not be read".to_owned())?;
                let latency_millis = u64::try_from(snapshot.elapsed.as_millis())
                    .map_err(|_| "provider latency is invalid".to_owned())?;
                let final_answer = extract_final_answer(provider, &raw_stdout);
                let actual_cost_microusd = extract_actual_cost_microusd(provider, &raw_stdout);
                let stdout = redact_bytes(&redaction, &final_answer);
                let stderr = redact_bytes(&redaction, &raw_stderr);
                return Ok(ReferenceExecution {
                    completion_reason: map_run_completion_reason(completion_reason),
                    latency_millis,
                    stdout,
                    stderr,
                    trace_artifact_ids: runtime.trace_artifact_ids().to_vec(),
                    actual_cost_microusd,
                });
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        let _ignored = runtime.interrupt(spec.run_id());
    }
    drop(runtime);
    sandbox
        .cleanup()
        .map_err(|_| "sandbox cleanup failed".to_owned())?;
    result
}

/// Opens one `SandboxManager` to share across every local trial of a paired
/// Arena evaluation, unless the job runs on a remote lease (which never
/// touches a local sandbox). `SandboxManager::open` re-validates the sandbox
/// root's device/inode identity and re-applies its permissions on every
/// call; a paired evaluation's handful of trials share the same root for the
/// life of one job, so opening it once instead of once per trial drops that
/// repeated, always-identical setup work from the critical path. `create()`
/// -- which still runs once per trial, exactly as before -- keeps its own
/// per-trial `validate_root` check, so every trial still gets its own
/// independently validated, freshly created sandbox. If this open fails
/// (never observed in practice: the sandbox root lives under the daemon's
/// own data directory), each trial falls back to opening its own manager
/// exactly as it did previously, so that unreachable path's behavior is
/// unchanged.
fn open_shared_sandboxes(
    needs_local_sandbox: bool,
    data_dir: &Path,
) -> Option<Arc<SandboxManager>> {
    needs_local_sandbox
        .then(|| SandboxManager::open(data_dir.join("sandboxes"), Duration::from_secs(30)).ok())
        .flatten()
        .map(Arc::new)
}

pub(super) fn execute_async_arena_trials(launch: AsyncArenaTrialLaunch) {
    let AsyncArenaTrialLaunch {
        data_dir,
        guardian,
        protected_paths,
        worker,
        codex_executable,
        claude_executable,
        provider_extra_env,
        redaction,
        cancel,
        trials,
        evidence,
        messages,
        mut initial_sequence,
        job_id,
        remote_lease,
        overall_deadline,
    } = launch;
    let mut outcome = Ok(());
    let shared_sandboxes = open_shared_sandboxes(remote_lease.is_none(), &data_dir);
    for (index, trial) in trials.iter().enumerate() {
        if cancel.load(Ordering::Acquire) {
            outcome = Err("paired evaluation was cancelled".to_owned());
            break;
        }
        let sandboxes = shared_sandboxes.clone().map_or_else(
            || SandboxManagerSource::OpenFresh(data_dir.clone()),
            SandboxManagerSource::Shared,
        );
        let output = if let Some(provider) = trial.provider {
            let executable = match provider {
                Provider::Codex => codex_executable.clone(),
                Provider::Claude => claude_executable.clone(),
                Provider::Deterministic => {
                    outcome = Err("paired trial has an invalid provider binding".to_owned());
                    break;
                }
            };
            execute_async_provider(
                AsyncProviderLaunch {
                    sandboxes,
                    guardian: guardian.clone(),
                    protected_paths: protected_paths.clone(),
                    provider,
                    executable,
                    extra_env: provider_extra_env.clone(),
                    redaction: redaction.clone(),
                    cancel: Arc::clone(&cancel),
                },
                &trial.spec,
                evidence.clone(),
                initial_sequence,
            )
        } else if let Some(lease) = remote_lease.as_deref() {
            execute_arena_trial_remotely(lease, &job_id, index, trial, &cancel, overall_deadline)
        } else if let Some(worker) = worker.as_ref() {
            execute_async_reference(
                AsyncReferenceLaunch {
                    sandboxes,
                    guardian: guardian.clone(),
                    protected_paths: protected_paths.clone(),
                    worker: Arc::clone(worker),
                    cancel: Arc::clone(&cancel),
                },
                &trial.spec,
                evidence.clone(),
                initial_sequence,
            )
        } else {
            // Unreachable in practice: `submit_arena_job` only ever admits a
            // provider-less (reference-role) trial after pinning a worker for
            // it. Fail the trial rather than panic if that invariant is ever
            // broken.
            Err("reference-role Arena trial has no pinned reference worker".to_owned())
        };
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

/// Executes one reference-role Arena trial by leasing it to a remote
/// worker (TD-12) instead of running a local sandbox. The frame is the
/// exact same `frame_reference_instruction` construction the direct-run
/// remote-worker path uses, over the trial's own already-bound
/// [`ReferenceInstruction`] and prompt, so a remote worker cannot tell this
/// apart from a direct reference run. Blocks this background thread (never
/// the daemon's single control-loop thread) until a worker submits a
/// result, the trial is cancelled, or `deadline` passes.
fn execute_arena_trial_remotely(
    lease: &RemoteArenaLeaseQueue,
    job_id: &str,
    index: usize,
    trial: &ArenaTrialSpec,
    cancel: &std::sync::atomic::AtomicBool,
    deadline: Instant,
) -> Result<ReferenceExecution, String> {
    let instruction = trial
        .spec
        .reference_instruction()
        .ok_or_else(|| "remote Arena trial has no reference instruction".to_owned())?;
    let frame = hephaestus_runtime::frame_reference_instruction(
        instruction,
        trial.spec.prompt().as_bytes(),
    )
    .map_err(|_| "remote Arena trial frame could not be built".to_owned())?;
    let trial_job_id = format!("arena:{job_id}:trial:{index}");
    let outcome = lease.submit_and_wait(
        &trial_job_id,
        &trial.genome.genome_id,
        frame,
        cancel,
        deadline,
    )?;
    Ok(ReferenceExecution {
        completion_reason: match outcome.completion {
            RemoteCompletion::Success => RunCompletionReason::Success,
            RemoteCompletion::ProviderFailure => RunCompletionReason::ProviderFailure,
        },
        latency_millis: outcome.latency_millis,
        stdout: outcome.output,
        stderr: Vec::new(),
        trace_artifact_ids: Vec::new(),
        actual_cost_microusd: 0,
    })
}

fn map_run_completion_reason(reason: CompletionReason) -> RunCompletionReason {
    reason.into()
}

pub(super) fn execute_reference_runtime(
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
            actual_cost_microusd: 0,
        })
    })();
    let (_, recorder) = runtime.into_parts();
    (execution, recorder)
}

pub(super) fn execute_candidate_runtime(
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
            actual_cost_microusd: 0,
        })
    })();
    let (_, recorder) = runtime.into_parts();
    (execution, recorder)
}

/// Runs a Codex or Claude Code adapter to completion and maps its NDJSON
/// stream onto a signed run result: the extracted final answer becomes
/// stdout, the reported cost (when any) becomes `actual_cost_microusd`, and
/// both stdout and stderr are redacted before they ever reach the artifact
/// store. Structured observations (tool calls, denials, cost) are recorded
/// as evidence traces by `RecordedRuntime` via `drain_observations`, through
/// the same redaction and evidence pipeline the reference worker uses.
pub(super) fn execute_provider_runtime(
    runtime: SupervisedRuntime,
    recorder: EvidenceRecorder,
    spec: &RunSpec,
    sandbox: &Sandbox,
    token: &CapabilityToken,
    run_id: &str,
    redaction: &RedactionPolicy,
) -> (Result<ReferenceExecution, ExecuteError>, EvidenceRecorder) {
    let provider = runtime.provider();
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
        let raw_stdout = fs::read(&snapshot.stdout_path).map_err(|_| ExecuteError::Internal)?;
        let raw_stderr = fs::read(&snapshot.stderr_path).map_err(|_| ExecuteError::Internal)?;
        let final_answer = extract_final_answer(provider, &raw_stdout);
        let actual_cost_microusd = extract_actual_cost_microusd(provider, &raw_stdout);
        let stdout = redact_bytes(redaction, &final_answer);
        let stderr = redact_bytes(redaction, &raw_stderr);
        let history = runtime
            .evidence()
            .replay_verified()
            .map_err(|_| ExecuteError::Internal)?;
        let trace_ids = trace_artifacts_for_run(&history, run_id)?;
        Ok(ReferenceExecution {
            completion_reason,
            latency_millis,
            stdout,
            stderr,
            trace_artifact_ids: trace_ids,
            actual_cost_microusd,
        })
    })();
    let (_, recorder) = runtime.into_parts();
    (execution, recorder)
}

/// Redacts textual provider output through the existing secret-redaction
/// path. Bytes that are not valid UTF-8 are passed through unchanged: the
/// redaction rules match literal tokens and known-secret strings, which are
/// only ever meaningful in text.
fn redact_bytes(policy: &RedactionPolicy, bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) => policy.redact_text(text).into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}

#[cfg(feature = "test-support")]
pub(super) fn candidate_isolation(_protected_paths: Vec<PathBuf>) -> IsolationPolicy {
    IsolationPolicy::unconfined_for_testing()
}

#[cfg(not(feature = "test-support"))]
pub(super) fn candidate_isolation(protected_paths: Vec<PathBuf>) -> IsolationPolicy {
    IsolationPolicy::detect(protected_paths)
}

pub(super) fn validate_job_id(job_id: &str) -> Result<(), ExecuteError> {
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

pub(super) fn job_run_id(job_id: &str) -> String {
    let digest = blake3::hash(job_id.as_bytes()).to_hex().to_string();
    format!("async-{}", &digest[..32])
}

pub(super) fn remote_job_run_id(job_id: &str) -> String {
    let digest = blake3::hash(job_id.as_bytes()).to_hex().to_string();
    format!("remote-{}", &digest[..32])
}

pub(super) fn hex_encode_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

pub(super) fn hex_decode_bytes(value: &str) -> Result<Vec<u8>, ExecuteError> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ExecuteError::Invalid("value is not valid hex"));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| ExecuteError::Invalid("value is not valid hex"))
        })
        .collect()
}

pub(super) fn paired_run_prefix(evaluation_id: &str) -> String {
    let digest = blake3::hash(evaluation_id.as_bytes()).to_hex().to_string();
    format!("paired-{}", &digest[..24])
}

pub(super) fn paired_run_id(evaluation_id: &str, role: &str, index: usize) -> String {
    format!("{}-{role}-{index}", paired_run_prefix(evaluation_id))
}

pub(super) fn resolve_source_revision(repository: &Path) -> Result<String, ExecuteError> {
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

pub(super) fn default_evaluator_executable() -> Result<PathBuf, ControlError> {
    let current = env::current_exe()?;
    let directory = current
        .parent()
        .ok_or(ControlError::Protocol("daemon executable has no directory"))?;
    let sibling = directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    if sibling.exists() {
        return Ok(sibling);
    }
    // Cargo places unit-test executables under `target/debug/deps`, while the
    // installed daemon and evaluator are siblings under `bin` or `target/debug`.
    if let Some(parent) = directory.parent() {
        let cargo_sibling = parent.join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
        if cargo_sibling.exists() {
            return Ok(cargo_sibling);
        }
    }
    Ok(sibling)
}

pub(super) fn default_reference_worker_executable() -> Result<PathBuf, ControlError> {
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

/// Resolves an operator-configured provider CLI path from `variable`, falling
/// back to `default_name` for `PATH`-relative lookup by the isolated child's
/// own restored `PATH` (never this process's full environment).
pub(super) fn provider_executable_from_environment(variable: &str, default_name: &str) -> PathBuf {
    env::var_os(variable).map_or_else(|| PathBuf::from(default_name), PathBuf::from)
}

/// Opt-in operator login for hosted CLIs. When the daemon's environment names
/// `HEPHAESTUS_CODEX_AUTH_FILE` (normally `~/.codex/auth.json`), that file is
/// copied into each Codex run's private `HOME` so a ChatGPT-subscription login
/// works inside the sandbox. Unset by default: no provider child ever sees the
/// operator's credentials unless the operator names them.
pub(super) fn with_provider_login(
    runtime: SupervisedRuntime,
    provider: Provider,
) -> Result<SupervisedRuntime, hephaestus_runtime::RuntimeError> {
    match (provider, env::var_os("HEPHAESTUS_CODEX_AUTH_FILE")) {
        (Provider::Codex, Some(path)) if !path.is_empty() => {
            runtime.with_home_file(PathBuf::from(path), ".codex/auth.json")
        }
        _ => Ok(runtime),
    }
}

/// Reads the operator-named allowlist of environment variables a provider
/// child may see, from `HEPHAESTUS_PROVIDER_ENV_ALLOWLIST` (comma-separated
/// names). Empty when unset, so nothing beyond `PATH`/`HOME`/`TMPDIR` reaches
/// a provider child unless an operator explicitly names it.
pub(super) fn provider_env_allowlist_from_environment() -> Vec<String> {
    env::var("HEPHAESTUS_PROVIDER_ENV_ALLOWLIST")
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Copies only the named, currently-set variables from this process's own
/// environment. A name that is not set is silently skipped rather than
/// passed through as empty.
pub(super) fn resolve_provider_extra_env(allowlist: &[String]) -> Vec<(String, String)> {
    allowlist
        .iter()
        .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
        .collect()
}

pub(super) fn default_process_guardian_executable() -> Result<PathBuf, ControlError> {
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

pub(super) fn executable_digest(path: &Path) -> Result<String, ControlError> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(ControlError::Protocol(
            "reference worker must be an executable regular file",
        ));
    }
    let bytes = fs::read(path)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

pub(super) fn persist_reference_output(
    artifacts: &dyn ArtifactBackend,
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
        actual_cost_microusd: output.actual_cost_microusd,
        stdout_artifact_id,
        stderr_artifact_id,
        trace_artifact_ids: output.trace_artifact_ids,
    })
}

#[cfg(test)]
mod runtime_exec_unit_tests {
    use super::{
        RedactionPolicy, default_evaluator_executable, default_process_guardian_executable,
        default_reference_worker_executable, hex_decode_bytes, redact_bytes,
    };

    /// Not wired to any `ControlPlane` fixture (every existing test supplies
    /// its own evaluator/worker/guardian executables explicitly), so the
    /// sibling-lookup fallback this exercises was otherwise never called at
    /// all. Running under `cargo test`, the current executable lives at
    /// `target/debug/deps/<test-binary>`, so `directory.parent()` (the
    /// "Cargo puts bins under `target/debug`, tests under
    /// `target/debug/deps`" fallback) finds the real sibling built by
    /// `cargo build --workspace --bins`.
    #[test]
    fn default_executables_resolve_the_cargo_target_debug_sibling() {
        let evaluator = default_evaluator_executable().expect("resolve evaluator executable");
        assert!(
            evaluator.ends_with(format!(
                "hephaestus-reference-evaluator{}",
                std::env::consts::EXE_SUFFIX
            )),
            "unexpected evaluator path: {evaluator:?}"
        );
        assert!(
            evaluator.exists(),
            "evaluator sibling should exist: {evaluator:?}"
        );

        let worker = default_reference_worker_executable().expect("resolve worker executable");
        assert!(
            worker.ends_with(format!(
                "hephaestus-reference-worker{}",
                std::env::consts::EXE_SUFFIX
            )),
            "unexpected worker path: {worker:?}"
        );
        assert!(worker.exists(), "worker sibling should exist: {worker:?}");

        let guardian =
            default_process_guardian_executable().expect("resolve process guardian executable");
        assert!(
            guardian.ends_with(format!(
                "hephaestus-process-guardian{}",
                std::env::consts::EXE_SUFFIX
            )),
            "unexpected guardian path: {guardian:?}"
        );
        assert!(
            guardian.exists(),
            "guardian sibling should exist: {guardian:?}"
        );
    }

    #[test]
    fn hex_decode_bytes_rejects_odd_length_and_non_hex_input() {
        assert!(hex_decode_bytes("abc").is_err());
        assert!(hex_decode_bytes("zz").is_err());
        assert_eq!(hex_decode_bytes("2a").unwrap(), vec![0x2a]);
    }

    #[test]
    fn redact_bytes_passes_through_non_utf8_bytes_unchanged() {
        let policy = RedactionPolicy::new(["secret".to_owned()]);
        let non_utf8 = vec![0xff, 0xfe, 0xfd];
        assert_eq!(redact_bytes(&policy, &non_utf8), non_utf8);
    }
}
