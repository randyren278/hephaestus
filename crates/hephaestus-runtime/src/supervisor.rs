use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use hephaestus_core::authority::CapabilitySet;

use crate::{
    AdapterCapabilities, CapabilityToken, CompletionReason, IsolationPolicy, Provider,
    ProviderEventCursor, ProviderInvocation, RunHandle, RunSnapshot, RunSpec, RunStatus,
    RuntimeAdapter, RuntimeError, RuntimeObservation, Sandbox, guardian::GuardianLaunch,
};

/// Provider-neutral child-process supervisor used for non-billable local helpers.
///
/// Hosted providers will use the same monitor after credential and hard-cost
/// mediation are available; this constructor deliberately grants no network.
pub struct SupervisedRuntime {
    isolation: IsolationPolicy,
    provider: Provider,
    executable: PathBuf,
    arguments: Vec<String>,
    guardian_executable: Option<PathBuf>,
    /// Explicit, operator-supplied environment variables copied into the child
    /// process on top of the fixed `PATH`/`HOME`/`TMPDIR` allowlist. Never the
    /// daemon's own inherited environment: nothing here is copied unless a
    /// caller names it explicitly.
    extra_env: Vec<(String, String)>,
    runs: BTreeMap<String, SupervisedRun>,
}

impl Drop for SupervisedRuntime {
    fn drop(&mut self) {
        for run in self.runs.values() {
            let mut observed = run.shared.observed.lock().expect("run state lock poisoned");
            if observed.status == RunStatus::Running {
                run.shared.interrupt.store(true, Ordering::Release);
                request_guardian_cancel(&run.shared);
                while observed.status == RunStatus::Running {
                    observed = run
                        .shared
                        .changed
                        .wait(observed)
                        .expect("run state lock poisoned");
                }
            }
        }
    }
}

struct SupervisedRun {
    shared: Arc<SharedRun>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    capabilities: CapabilitySet,
    provider: Provider,
    observation_cursor: ProviderEventCursor,
    observed_bytes: u64,
}

struct SharedRun {
    observed: Mutex<ObservedRun>,
    changed: Condvar,
    interrupt: AtomicBool,
    output_exceeded: AtomicBool,
    io_failed: AtomicBool,
    /// The child closed its stdin (broken pipe) before the prompt was delivered.
    stdin_closed_early: AtomicBool,
    guarded: bool,
    cancel: Mutex<Option<SyncSender<GuardianControl>>>,
}

#[derive(Clone, Copy)]
struct ObservedRun {
    status: RunStatus,
    exit_code: Option<i32>,
    completion_reason: Option<CompletionReason>,
    elapsed: Duration,
}

pub(crate) struct ProcessOutput {
    pub(crate) completion_reason: CompletionReason,
    pub(crate) exit_code: Option<i32>,
    pub(crate) elapsed: Duration,
}

#[derive(Clone, Copy)]
enum GuardianControl {
    Cancel,
    Close,
}

#[derive(Clone, Copy)]
enum StopReason {
    Interrupted,
    TimedOut,
    OutputExceeded,
    IoFailed,
}

impl SupervisedRuntime {
    /// Creates an offline, non-shell process adapter for deterministic helpers.
    ///
    /// # Errors
    ///
    /// Rejects an empty executable path.
    pub fn deterministic(
        isolation: IsolationPolicy,
        executable: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = String>,
    ) -> Result<Self, RuntimeError> {
        let executable = executable.into();
        ProviderInvocation::deterministic(&executable, [], [])?;
        Ok(Self {
            isolation,
            provider: Provider::Deterministic,
            executable,
            arguments: arguments.into_iter().collect(),
            guardian_executable: None,
            extra_env: Vec::new(),
            runs: BTreeMap::new(),
        })
    }

    /// Creates a worker adapter whose OS guardian kills the worker process group
    /// whenever this daemon closes its control pipe.
    ///
    /// # Errors
    ///
    /// Rejects an empty worker or guardian executable path.
    pub fn deterministic_guarded(
        isolation: IsolationPolicy,
        executable: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = String>,
        guardian_executable: impl Into<PathBuf>,
    ) -> Result<Self, RuntimeError> {
        let mut runtime = Self::deterministic(isolation, executable, arguments)?;
        let guardian = guardian_executable.into();
        ProviderInvocation::deterministic(&guardian, [], [])?;
        runtime.guardian_executable = Some(guardian);
        Ok(runtime)
    }

    /// Creates a Codex or Claude Code adapter. `executable` is operator
    /// configuration (a daemon flag or environment variable naming the CLI
    /// binary), which is exactly how offline tests point this at a fake.
    ///
    /// `extra_env` is an explicit, named allowlist of environment variables
    /// copied into the child on top of `PATH`/`HOME`/`TMPDIR`; nothing from the
    /// daemon's own environment is inherited unless it is named here.
    ///
    /// # Errors
    ///
    /// Rejects `Provider::Deterministic` (use [`Self::deterministic`]) and an
    /// empty executable path.
    pub fn provider(
        isolation: IsolationPolicy,
        provider: Provider,
        executable: impl Into<PathBuf>,
        extra_env: Vec<(String, String)>,
    ) -> Result<Self, RuntimeError> {
        if provider == Provider::Deterministic {
            return Err(RuntimeError::InvalidSpec(
                "use SupervisedRuntime::deterministic for the reference provider",
            ));
        }
        let executable = executable.into();
        if executable.as_os_str().is_empty() {
            return Err(RuntimeError::InvalidSpec("provider executable is empty"));
        }
        Ok(Self {
            isolation,
            provider,
            executable,
            arguments: Vec::new(),
            guardian_executable: None,
            extra_env,
            runs: BTreeMap::new(),
        })
    }

    /// Creates a Codex or Claude Code adapter whose OS guardian kills the
    /// provider process group whenever this daemon closes its control pipe.
    ///
    /// # Errors
    ///
    /// Applies the same validation as [`Self::provider`], plus rejects an
    /// empty guardian executable path.
    pub fn provider_guarded(
        isolation: IsolationPolicy,
        provider: Provider,
        executable: impl Into<PathBuf>,
        guardian_executable: impl Into<PathBuf>,
        extra_env: Vec<(String, String)>,
    ) -> Result<Self, RuntimeError> {
        let mut runtime = Self::provider(isolation, provider, executable, extra_env)?;
        let guardian = guardian_executable.into();
        ProviderInvocation::deterministic(&guardian, [], [])?;
        runtime.guardian_executable = Some(guardian);
        Ok(runtime)
    }

    fn launch(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        token: &CapabilityToken,
    ) -> Result<RunHandle, RuntimeError> {
        sandbox.authorize_spec(token, spec)?;
        self.report_capabilities()
            .authority
            .derive_child(spec.capabilities())
            .map_err(|_| RuntimeError::CapabilityDenied)?;
        if self.runs.contains_key(spec.run_id()) {
            return Err(RuntimeError::InvalidSpec("run already exists"));
        }
        let started = Instant::now();
        let deadline = started
            .checked_add(spec.budget().wall())
            .ok_or(RuntimeError::InvalidSpec("wall budget exceeds clock range"))?;

        fs::create_dir_all(sandbox.execution_dir())?;
        let stdout_path = sandbox.execution_dir().join("stdout.log");
        let stderr_path = sandbox.execution_dir().join("stderr.log");
        let stdout_file = File::create(&stdout_path)?;
        let stderr_file = File::create(&stderr_path)?;
        let (mut command, guarded, launch_bytes) = self.worker_command(spec, sandbox)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = spawn_retrying_busy_executable(&mut command)?;
        let child_stdin = child
            .stdin
            .take()
            .ok_or(RuntimeError::InvalidSpec("child stdin was not piped"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or(RuntimeError::InvalidSpec("child stdout was not piped"))?;
        let child_stderr = child
            .stderr
            .take()
            .ok_or(RuntimeError::InvalidSpec("child stderr was not piped"))?;

        let (cancel_sender, cancel_receiver) = if guarded {
            let (sender, receiver) = mpsc::sync_channel(1);
            (Some(sender), Some(receiver))
        } else {
            (None, None)
        };
        let shared = Arc::new(SharedRun {
            observed: Mutex::new(ObservedRun {
                status: RunStatus::Running,
                exit_code: None,
                completion_reason: None,
                elapsed: Duration::ZERO,
            }),
            changed: Condvar::new(),
            interrupt: AtomicBool::new(false),
            output_exceeded: AtomicBool::new(false),
            io_failed: AtomicBool::new(false),
            stdin_closed_early: AtomicBool::new(false),
            guarded,
            cancel: Mutex::new(cancel_sender),
        });
        let stdin_writer = spawn_stdin_writer(
            child_stdin,
            launch_bytes,
            cancel_receiver,
            Arc::clone(&shared),
        );
        spawn_monitor(
            child,
            child_stdout,
            child_stderr,
            stdout_file,
            stderr_file,
            spec.budget().maximum_output_bytes(),
            deadline,
            started,
            stdin_writer,
            Arc::clone(&shared),
        );
        self.runs.insert(
            spec.run_id().to_owned(),
            SupervisedRun {
                shared,
                stdout_path,
                stderr_path,
                capabilities: spec.capabilities(),
                provider: self.provider,
                observation_cursor: ProviderEventCursor::new(),
                observed_bytes: 0,
            },
        );
        Ok(RunHandle {
            run_id: spec.run_id().to_owned(),
            provider: self.provider,
        })
    }

    fn worker_command(
        &self,
        spec: &RunSpec,
        sandbox: &Sandbox,
    ) -> Result<(Command, bool, Vec<u8>), RuntimeError> {
        let invocation = match self.provider {
            Provider::Deterministic => {
                let stdin = if let Some(instruction) = spec.reference_instruction() {
                    #[cfg(feature = "test-support")]
                    {
                        match test_reference_delay_millis_for(spec.genome_id(), sandbox.worktree())
                        {
                            Some(delay_millis) => instruction
                                .frame_with_test_delay(spec.prompt().as_bytes(), delay_millis)?,
                            None => instruction.frame(spec.prompt().as_bytes())?,
                        }
                    }
                    #[cfg(not(feature = "test-support"))]
                    {
                        instruction.frame(spec.prompt().as_bytes())?
                    }
                } else {
                    spec.prompt().as_bytes().to_vec()
                };
                ProviderInvocation::deterministic(&self.executable, self.arguments.clone(), stdin)?
            }
            Provider::Codex => ProviderInvocation::codex(&self.executable, spec, sandbox)?,
            Provider::Claude => ProviderInvocation::claude(&self.executable, spec, sandbox)?,
        };
        let mut worker_command = self.isolation.command(&invocation, sandbox)?;
        worker_command.env_clear();
        let path = std::env::var("PATH").ok();
        if let Some(path_value) = &path {
            worker_command.env("PATH", path_value);
        }
        worker_command.env("HOME", sandbox.execution_dir());
        worker_command.env("TMPDIR", sandbox.execution_dir());
        for (key, value) in &self.extra_env {
            worker_command.env(key, value);
        }
        let Some(guardian) = &self.guardian_executable else {
            return Ok((worker_command, false, invocation.stdin().to_vec()));
        };
        let program = worker_command
            .get_program()
            .to_str()
            .ok_or(RuntimeError::InvalidSpec(
                "worker program path is not UTF-8",
            ))?
            .to_owned();
        let arguments = worker_command
            .get_args()
            .map(|argument| {
                argument
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(RuntimeError::InvalidSpec("worker argument is not UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input = invocation.stdin().to_vec();
        let config = GuardianLaunch {
            program,
            arguments,
            current_dir: sandbox.worktree().to_owned(),
            home: sandbox.execution_dir().to_owned(),
            temp: sandbox.execution_dir().to_owned(),
            path,
            input_bytes: input.len(),
        };
        let mut frame = serde_json::to_vec(&config)
            .map_err(|_| RuntimeError::InvalidSpec("guardian configuration is invalid"))?;
        frame.push(b'\n');
        frame.extend_from_slice(&input);
        frame.push(b'\n');
        Ok((Command::new(guardian), true, frame))
    }
}

/// Test-only, process-wide slot naming a single Genome id and a delay in
/// milliseconds. Lets a test make one specific, already-registered Genome's
/// reference worker genuinely slower (a real `sleep` inside the worker
/// process) without touching any other Genome or introducing timing races.
/// Compiled only under `test-support`, so it cannot exist in a release
/// build; a plain `Mutex`, not an env var, so setting it never requires
/// `unsafe` (this workspace forbids `unsafe` code).
#[cfg(feature = "test-support")]
static TEST_REFERENCE_DELAY: std::sync::Mutex<Option<(String, u64)>> = std::sync::Mutex::new(None);

/// Test-only: arranges for the reference worker to sleep `delay_millis`
/// (bounded, see [`crate::reference_instruction`]) whenever it next runs
/// `genome_id`. See [`TEST_REFERENCE_DELAY`].
#[cfg(feature = "test-support")]
pub fn set_test_reference_delay(genome_id: impl Into<String>, delay_millis: u64) {
    *TEST_REFERENCE_DELAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((genome_id.into(), delay_millis));
}

/// Test-only: clears any delay set by [`set_test_reference_delay`].
#[cfg(feature = "test-support")]
pub fn clear_test_reference_delay() {
    *TEST_REFERENCE_DELAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

/// Test-only delay added to every reference-worker run, on top of any
/// per-Genome delay. Latency-gated tests set it so per-trial scheduling noise
/// is small relative to the measured latency.
#[cfg(feature = "test-support")]
static TEST_REFERENCE_BASELINE_DELAY: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Test-only: makes every reference-worker run sleep `delay_millis` (bounded,
/// see [`crate::reference_instruction`]) in addition to any per-Genome delay.
#[cfg(feature = "test-support")]
pub fn set_test_reference_baseline_delay(delay_millis: u64) {
    TEST_REFERENCE_BASELINE_DELAY.store(delay_millis, std::sync::atomic::Ordering::SeqCst);
}

/// Test-only delays scoped to a directory: they apply only to runs whose
/// sandbox lives under that directory, so tests that each own a temporary
/// directory can inject delays in parallel without affecting one another.
#[cfg(feature = "test-support")]
#[derive(Default)]
struct ScopedTestDelays {
    /// (scope, per-run baseline milliseconds)
    baselines: Vec<(std::path::PathBuf, u64)>,
    /// (scope, genome id, milliseconds)
    genomes: Vec<(std::path::PathBuf, String, u64)>,
}

#[cfg(feature = "test-support")]
static SCOPED_TEST_DELAYS: std::sync::Mutex<Option<ScopedTestDelays>> = std::sync::Mutex::new(None);

#[cfg(feature = "test-support")]
fn canonical_scope(scope: &std::path::Path) -> std::path::PathBuf {
    scope.canonicalize().unwrap_or_else(|_| scope.to_path_buf())
}

#[cfg(feature = "test-support")]
fn with_scoped_delays<T>(action: impl FnOnce(&mut ScopedTestDelays) -> T) -> T {
    let mut guard = SCOPED_TEST_DELAYS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    action(guard.get_or_insert_with(ScopedTestDelays::default))
}

/// Test-only: every reference-worker run whose sandbox is under `scope`
/// sleeps `delay_millis` (bounded) in addition to any Genome delay.
#[cfg(feature = "test-support")]
pub fn set_test_reference_baseline_delay_in(scope: &std::path::Path, delay_millis: u64) {
    let scope = canonical_scope(scope);
    with_scoped_delays(|delays| {
        delays.baselines.retain(|(existing, _)| *existing != scope);
        delays.baselines.push((scope, delay_millis));
    });
}

/// Test-only: reference-worker runs of `genome_id` whose sandbox is under
/// `scope` sleep `delay_millis` (bounded) on top of any baseline.
#[cfg(feature = "test-support")]
pub fn set_test_reference_delay_in(
    scope: &std::path::Path,
    genome_id: impl Into<String>,
    delay_millis: u64,
) {
    let scope = canonical_scope(scope);
    let genome_id = genome_id.into();
    with_scoped_delays(|delays| {
        delays
            .genomes
            .retain(|(existing, genome, _)| !(*existing == scope && *genome == genome_id));
        delays.genomes.push((scope, genome_id, delay_millis));
    });
}

/// Test-only: clears the per-Genome delays under `scope`, keeping its baseline.
#[cfg(feature = "test-support")]
pub fn clear_test_reference_delays_in(scope: &std::path::Path) {
    let scope = canonical_scope(scope);
    with_scoped_delays(|delays| delays.genomes.retain(|(existing, _, _)| *existing != scope));
}

/// Test-only: removes every delay registered under `scope`.
#[cfg(feature = "test-support")]
pub fn forget_test_reference_scope(scope: &std::path::Path) {
    let scope = canonical_scope(scope);
    with_scoped_delays(|delays| {
        delays.baselines.retain(|(existing, _)| *existing != scope);
        delays.genomes.retain(|(existing, _, _)| *existing != scope);
    });
}

#[cfg(feature = "test-support")]
fn test_reference_delay_millis_for(genome_id: &str, sandbox_path: &std::path::Path) -> Option<u64> {
    let global_specific = TEST_REFERENCE_DELAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|(target, millis)| (target == genome_id).then_some(*millis))
        .unwrap_or(0);
    let sandbox_path = canonical_scope(sandbox_path);
    let scoped = with_scoped_delays(|delays| {
        let baseline: u64 = delays
            .baselines
            .iter()
            .filter(|(scope, _)| sandbox_path.starts_with(scope))
            .map(|(_, millis)| *millis)
            .sum();
        let genome: u64 = delays
            .genomes
            .iter()
            .filter(|(scope, genome, _)| sandbox_path.starts_with(scope) && genome == genome_id)
            .map(|(_, _, millis)| *millis)
            .sum();
        baseline.saturating_add(genome)
    });
    let total = TEST_REFERENCE_BASELINE_DELAY
        .load(std::sync::atomic::Ordering::SeqCst)
        .saturating_add(global_specific)
        .saturating_add(scoped);
    (total > 0).then_some(total)
}

impl RuntimeAdapter for SupervisedRuntime {
    fn provider(&self) -> Provider {
        self.provider
    }

    fn report_capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            resume: false,
            interrupt: true,
            snapshot: true,
            authority: match self.provider {
                // Codex and Claude Code must themselves reach a hosted model API,
                // so their ceiling allows network; the actual grant for one run
                // still comes only from that run's own `RunSpec` capabilities.
                Provider::Deterministic => CapabilitySet::new(true, false),
                Provider::Codex | Provider::Claude => CapabilitySet::new(true, true),
            },
        }
    }

    fn start(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        token: &CapabilityToken,
    ) -> Result<RunHandle, RuntimeError> {
        self.launch(spec, sandbox, token)
    }

    fn resume(
        &mut self,
        _spec: &RunSpec,
        _sandbox: &Sandbox,
        _token: &CapabilityToken,
        _checkpoint: &str,
    ) -> Result<RunHandle, RuntimeError> {
        Err(RuntimeError::Unsupported(
            "supervised helper does not support resume",
        ))
    }

    fn interrupt(&mut self, run_id: &str) -> Result<(), RuntimeError> {
        let run = self
            .runs
            .get(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        run.shared.interrupt.store(true, Ordering::Release);
        request_guardian_cancel(&run.shared);
        let mut observed = run.shared.observed.lock().expect("run state lock poisoned");
        while observed.status == RunStatus::Running {
            observed = run
                .shared
                .changed
                .wait(observed)
                .expect("run state lock poisoned");
        }
        Ok(())
    }

    fn snapshot(&mut self, run_id: &str) -> Result<RunSnapshot, RuntimeError> {
        let run = self
            .runs
            .get(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        let observed = *run.shared.observed.lock().expect("run state lock poisoned");
        Ok(RunSnapshot {
            run_id: run_id.to_owned(),
            status: observed.status,
            exit_code: observed.exit_code,
            completion_reason: observed.completion_reason,
            elapsed: if observed.status == RunStatus::Running {
                Duration::ZERO
            } else {
                observed.elapsed
            },
            stdout_path: run.stdout_path.clone(),
            stderr_path: run.stderr_path.clone(),
            capabilities: run.capabilities,
        })
    }

    fn drain_observations(
        &mut self,
        run_id: &str,
    ) -> Result<Vec<RuntimeObservation>, RuntimeError> {
        let run = self
            .runs
            .get_mut(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        if run.provider == Provider::Deterministic {
            return Ok(Vec::new());
        }
        let mut file = match File::open(&run.stdout_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        file.seek(SeekFrom::Start(run.observed_bytes))?;
        let mut chunk = Vec::new();
        file.read_to_end(&mut chunk)?;
        run.observed_bytes += chunk.len() as u64;
        let provider = run.provider;
        Ok(run.observation_cursor.feed(provider, &chunk))
    }
}

fn request_guardian_cancel(shared: &SharedRun) {
    send_guardian_control(shared, GuardianControl::Cancel);
}

fn close_guardian_control(shared: &SharedRun) {
    send_guardian_control(shared, GuardianControl::Close);
}

fn send_guardian_control(shared: &SharedRun, control: GuardianControl) {
    if let Some(sender) = shared.cancel.lock().expect("cancel lock poisoned").as_ref() {
        let _ignored = sender.try_send(control);
    }
}

/// Spawns `command`, retrying briefly when Linux reports the executable as
/// busy (ETXTBSY). That happens when another thread forked while a freshly
/// written executable (a pinned worker snapshot, a test's fake CLI) still had
/// a write handle open; the handle disappears once that child execs.
fn spawn_retrying_busy_executable(command: &mut Command) -> std::io::Result<std::process::Child> {
    let mut backoff = Duration::from_millis(5);
    for _ in 0..6 {
        match command.spawn() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                thread::sleep(backoff);
                backoff = backoff.saturating_mul(2);
            }
            result => return result,
        }
    }
    command.spawn()
}

fn spawn_stdin_writer(
    mut stdin: impl Write + Send + 'static,
    bytes: Vec<u8>,
    cancel: Option<Receiver<GuardianControl>>,
    shared: Arc<SharedRun>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if let Err(error) = stdin.write_all(&bytes) {
            shared.stdin_closed_early.store(
                error.kind() == std::io::ErrorKind::BrokenPipe,
                Ordering::Release,
            );
            shared.io_failed.store(true, Ordering::Release);
            return;
        }
        if let Some(cancel) = cancel
            && matches!(cancel.recv(), Ok(GuardianControl::Cancel))
            && stdin.write_all(b"cancel\n").is_err()
        {
            shared.io_failed.store(true, Ordering::Release);
        }
    })
}

pub(crate) fn execute_supervised_process(
    command: Command,
    stdin: Vec<u8>,
    stdout_path: &std::path::Path,
    stderr_path: &std::path::Path,
    wall: Duration,
    maximum_output_bytes: usize,
) -> Result<ProcessOutput, RuntimeError> {
    execute_supervised_process_inner(
        command,
        stdin,
        stdout_path,
        stderr_path,
        wall,
        maximum_output_bytes,
        false,
        None,
    )
}

pub(crate) fn execute_guarded_process(
    command: Command,
    stdin: Vec<u8>,
    stdout_path: &std::path::Path,
    stderr_path: &std::path::Path,
    wall: Duration,
    maximum_output_bytes: usize,
    cancel: Arc<AtomicBool>,
) -> Result<ProcessOutput, RuntimeError> {
    execute_supervised_process_inner(
        command,
        stdin,
        stdout_path,
        stderr_path,
        wall,
        maximum_output_bytes,
        true,
        Some(cancel),
    )
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
fn execute_supervised_process_inner(
    mut command: Command,
    stdin: Vec<u8>,
    stdout_path: &std::path::Path,
    stderr_path: &std::path::Path,
    wall: Duration,
    maximum_output_bytes: usize,
    guarded: bool,
    external_cancel: Option<Arc<AtomicBool>>,
) -> Result<ProcessOutput, RuntimeError> {
    let started = Instant::now();
    let deadline = started
        .checked_add(wall)
        .ok_or(RuntimeError::InvalidSpec("wall budget exceeds clock range"))?;
    let stdout_file = File::create(stdout_path)?;
    let stderr_file = File::create(stderr_path)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = spawn_retrying_busy_executable(&mut command)?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or(RuntimeError::InvalidSpec("child stdin was not piped"))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or(RuntimeError::InvalidSpec("child stdout was not piped"))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or(RuntimeError::InvalidSpec("child stderr was not piped"))?;
    let shared = Arc::new(SharedRun {
        observed: Mutex::new(ObservedRun {
            status: RunStatus::Running,
            exit_code: None,
            completion_reason: None,
            elapsed: Duration::ZERO,
        }),
        changed: Condvar::new(),
        interrupt: AtomicBool::new(false),
        output_exceeded: AtomicBool::new(false),
        io_failed: AtomicBool::new(false),
        stdin_closed_early: AtomicBool::new(false),
        guarded,
        cancel: Mutex::new(None),
    });
    let (cancel_sender, cancel_receiver) = if guarded {
        let (sender, receiver) = mpsc::sync_channel(1);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    if let Some(sender) = cancel_sender {
        *shared.cancel.lock().expect("cancel lock poisoned") = Some(sender);
    }
    let cancel_watcher = external_cancel.map(|external| {
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            loop {
                if shared
                    .observed
                    .lock()
                    .expect("run state lock poisoned")
                    .status
                    != RunStatus::Running
                {
                    return;
                }
                if external.load(Ordering::Acquire) {
                    shared.interrupt.store(true, Ordering::Release);
                    request_guardian_cancel(&shared);
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
        })
    });
    let stdin_writer = spawn_stdin_writer(child_stdin, stdin, cancel_receiver, Arc::clone(&shared));
    spawn_monitor(
        child,
        child_stdout,
        child_stderr,
        stdout_file,
        stderr_file,
        maximum_output_bytes,
        deadline,
        started,
        stdin_writer,
        Arc::clone(&shared),
    );
    let mut observed = shared.observed.lock().expect("run state lock poisoned");
    while observed.status == RunStatus::Running {
        observed = shared
            .changed
            .wait(observed)
            .expect("run state lock poisoned");
    }
    let completion_reason = observed.completion_reason.ok_or(RuntimeError::InvalidSpec(
        "worker completion reason is missing",
    ));
    let exit_code = observed.exit_code;
    let elapsed = observed.elapsed;
    drop(observed);
    if let Some(watcher) = cancel_watcher {
        let _ = watcher.join();
    }
    Ok(ProcessOutput {
        completion_reason: completion_reason?,
        exit_code,
        elapsed,
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_monitor(
    mut child: Child,
    stdout: impl Read + Send + 'static,
    stderr: impl Read + Send + 'static,
    stdout_file: File,
    stderr_file: File,
    maximum_output_bytes: usize,
    deadline: Instant,
    started: Instant,
    stdin_writer: thread::JoinHandle<()>,
    shared: Arc<SharedRun>,
) {
    thread::spawn(move || {
        let used = Arc::new(AtomicUsize::new(0));
        let stdout_reader = spawn_output_reader(
            stdout,
            stdout_file,
            Arc::clone(&used),
            maximum_output_bytes,
            Arc::clone(&shared),
        );
        let stderr_reader = spawn_output_reader(
            stderr,
            stderr_file,
            used,
            maximum_output_bytes,
            Arc::clone(&shared),
        );

        let outcome = loop {
            if shared.interrupt.load(Ordering::Acquire) {
                break stop_supervised_child(&mut child, &shared, StopReason::Interrupted);
            }
            if shared.output_exceeded.load(Ordering::Acquire) {
                break stop_supervised_child(&mut child, &shared, StopReason::OutputExceeded);
            }
            // A broken stdin pipe alone does not stop the child: its exit
            // status decides whether this was a provider failure or an
            // undelivered prompt (see the completion classification below).
            if shared.io_failed.load(Ordering::Acquire)
                && !shared.stdin_closed_early.load(Ordering::Acquire)
            {
                break stop_supervised_child(&mut child, &shared, StopReason::IoFailed);
            }
            if Instant::now() >= deadline {
                break stop_supervised_child(&mut child, &shared, StopReason::TimedOut);
            }
            match child.try_wait() {
                Ok(Some(status)) => break (Some(status), None),
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(_) => break terminate(&mut child, StopReason::IoFailed),
            }
        };

        close_guardian_control(&shared);
        if stdin_writer.join().is_err()
            || stdout_reader.join().is_err()
            || stderr_reader.join().is_err()
        {
            shared.io_failed.store(true, Ordering::Release);
        }
        let reason = outcome.1.or_else(|| {
            if shared.output_exceeded.load(Ordering::Acquire) {
                Some(StopReason::OutputExceeded)
            } else if shared.io_failed.load(Ordering::Acquire)
                && !(shared.stdin_closed_early.load(Ordering::Acquire)
                    && outcome.0.is_some_and(|status| !status.success()))
            {
                // A child that exits nonzero before reading its prompt is a
                // provider failure; one that exits successfully without
                // reading it is still an I/O failure, never a success.
                Some(StopReason::IoFailed)
            } else {
                None
            }
        });
        let observed = ObservedRun {
            status: reason.map_or_else(
                || outcome.0.map_or(RunStatus::Failed, status_from_exit),
                |reason| match reason {
                    StopReason::Interrupted => RunStatus::Interrupted,
                    StopReason::TimedOut => RunStatus::TimedOut,
                    StopReason::OutputExceeded | StopReason::IoFailed => RunStatus::Failed,
                },
            ),
            exit_code: outcome.0.and_then(|status| status.code()),
            completion_reason: reason.map_or_else(
                || {
                    outcome.0.map(|status| {
                        if status.success() {
                            CompletionReason::Success
                        } else {
                            CompletionReason::ProviderFailure
                        }
                    })
                },
                |reason| {
                    Some(match reason {
                        StopReason::Interrupted => CompletionReason::OperatorInterrupt,
                        StopReason::TimedOut => CompletionReason::WallBudgetExceeded,
                        StopReason::OutputExceeded => CompletionReason::OutputBudgetExceeded,
                        StopReason::IoFailed => CompletionReason::IoFailure,
                    })
                },
            ),
            elapsed: started.elapsed(),
        };
        *shared.observed.lock().expect("run state lock poisoned") = observed;
        shared.changed.notify_all();
    });
}

fn spawn_output_reader(
    mut source: impl Read + Send + 'static,
    mut destination: impl Write + Send + 'static,
    used: Arc<AtomicUsize>,
    maximum: usize,
    shared: Arc<SharedRun>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let count = match source.read(&mut buffer) {
                Ok(0) => return,
                Ok(count) => count,
                Err(_) => {
                    shared.io_failed.store(true, Ordering::Release);
                    return;
                }
            };
            let previous = used.fetch_add(count, Ordering::AcqRel);
            let allowed = maximum.saturating_sub(previous).min(count);
            if allowed != 0 && destination.write_all(&buffer[..allowed]).is_err() {
                shared.io_failed.store(true, Ordering::Release);
                return;
            }
            if allowed != count {
                shared.output_exceeded.store(true, Ordering::Release);
            }
        }
    })
}

fn stop_supervised_child(
    child: &mut Child,
    shared: &SharedRun,
    reason: StopReason,
) -> (Option<ExitStatus>, Option<StopReason>) {
    if !shared.guarded {
        return terminate(child, reason);
    }
    request_guardian_cancel(shared);
    match child.wait() {
        Ok(status) => (Some(status), Some(reason)),
        Err(_) => (None, Some(StopReason::IoFailed)),
    }
}

fn terminate(child: &mut Child, reason: StopReason) -> (Option<ExitStatus>, Option<StopReason>) {
    let _ignored = process_group_kill(child.id()).status();
    let _ignored = child.kill();
    let status = child.wait().ok();
    (status, Some(reason))
}

fn process_group_kill(pid: u32) -> Command {
    let process_group = format!("-{pid}");
    let mut command = Command::new("/bin/kill");
    command.args(["-KILL", "--", &process_group]);
    command
}

fn status_from_exit(status: ExitStatus) -> RunStatus {
    if status.success() {
        RunStatus::Succeeded
    } else {
        RunStatus::Failed
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io, process::Command, thread, time::Duration};

    use hephaestus_core::authority::CapabilitySet;
    use tempfile::{TempDir, tempdir};

    use super::*;
    use crate::{Budget, IsolationPolicy, RunSpec, SandboxManager};

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("injected read failure"))
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("injected write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct ErrorKindWriter(io::ErrorKind);

    impl Write for ErrorKindWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stdin_writer_records_a_broken_pipe_separately_from_other_failures() {
        let closed = shared_run();
        spawn_stdin_writer(
            ErrorKindWriter(io::ErrorKind::BrokenPipe),
            b"prompt".to_vec(),
            None,
            Arc::clone(&closed),
        )
        .join()
        .expect("join stdin writer");
        assert!(closed.io_failed.load(Ordering::Acquire));
        assert!(
            closed.stdin_closed_early.load(Ordering::Acquire),
            "a broken pipe is recorded so a nonzero exit can be judged by its status"
        );

        let failed = shared_run();
        spawn_stdin_writer(
            ErrorKindWriter(io::ErrorKind::PermissionDenied),
            b"prompt".to_vec(),
            None,
            Arc::clone(&failed),
        )
        .join()
        .expect("join stdin writer");
        assert!(failed.io_failed.load(Ordering::Acquire));
        assert!(!failed.stdin_closed_early.load(Ordering::Acquire));
    }

    #[test]
    fn output_capture_surfaces_reader_and_writer_failures() {
        let read_failure = shared_run();
        spawn_output_reader(
            FailingReader,
            Vec::new(),
            Arc::new(AtomicUsize::new(0)),
            10,
            Arc::clone(&read_failure),
        )
        .join()
        .expect("join failing reader");
        assert!(read_failure.io_failed.load(Ordering::Acquire));

        let write_failure = shared_run();
        spawn_output_reader(
            io::Cursor::new(b"captured output"),
            FailingWriter,
            Arc::new(AtomicUsize::new(0)),
            100,
            Arc::clone(&write_failure),
        )
        .join()
        .expect("join failing writer");
        assert!(write_failure.io_failed.load(Ordering::Acquire));

        let command = process_group_kill(123);
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert_eq!(arguments, ["-KILL", "--", "-123"]);
    }

    #[test]
    fn test_only_launch_path_exercises_stdin_environment_and_exit_status() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");

        let cat_spec = spec("unit-cat", repository.path(), Duration::from_secs(2), 1_000);
        let (cat_sandbox, cat_token) = manager.create(&cat_spec).expect("create cat sandbox");
        let mut cat = test_runtime("/bin/cat", []);
        cat.start(&cat_spec, &cat_sandbox, &cat_token)
            .expect("start cat");
        let cat_snapshot = wait_for_terminal(&mut cat, cat_spec.run_id());
        assert_eq!(cat_snapshot.status, RunStatus::Succeeded);
        assert_eq!(
            cat_snapshot.completion_reason,
            Some(CompletionReason::Success)
        );
        assert_eq!(
            fs::read_to_string(cat_snapshot.stdout_path).expect("read cat output"),
            cat_spec.prompt()
        );
        cat_sandbox.cleanup().expect("clean cat sandbox");

        let env_spec = spec(
            "unit-env",
            repository.path(),
            Duration::from_secs(2),
            10_000,
        );
        let (env_sandbox, env_token) = manager.create(&env_spec).expect("create env sandbox");
        let mut env = test_runtime("/usr/bin/env", []);
        env.start(&env_spec, &env_sandbox, &env_token)
            .expect("start env");
        let variables =
            fs::read_to_string(wait_for_terminal(&mut env, env_spec.run_id()).stdout_path)
                .expect("read environment");
        assert!(variables.lines().all(|line| {
            line.starts_with("HOME=") || line.starts_with("PATH=") || line.starts_with("TMPDIR=")
        }));
        env_sandbox.cleanup().expect("clean env sandbox");

        let failure_spec = spec(
            "unit-failure",
            repository.path(),
            Duration::from_secs(2),
            1_000,
        );
        let (failure_sandbox, failure_token) = manager
            .create(&failure_spec)
            .expect("create failure sandbox");
        let mut failure = test_runtime("/usr/bin/false", []);
        failure
            .start(&failure_spec, &failure_sandbox, &failure_token)
            .expect("start failure");
        let failed = wait_for_terminal(&mut failure, failure_spec.run_id());
        assert_eq!(failed.status, RunStatus::Failed);
        assert_eq!(
            failed.completion_reason,
            Some(CompletionReason::ProviderFailure)
        );
        failure_sandbox.cleanup().expect("clean failure sandbox");
    }

    #[test]
    fn production_launch_path_cannot_bypass_isolation_policy() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let run_spec = spec(
            "unit-unavailable",
            repository.path(),
            Duration::from_secs(2),
            1_000,
        );
        let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
        let marker = sandbox.execution_dir().join("process-started");
        let mut runtime = SupervisedRuntime::deterministic(
            IsolationPolicy::unavailable_for_testing(),
            "/usr/bin/touch",
            [marker.to_string_lossy().into_owned()],
        )
        .expect("create unavailable supervisor");

        assert!(matches!(
            runtime.start(&run_spec, &sandbox, &token),
            Err(RuntimeError::Unsupported(_))
        ));
        assert!(!marker.exists());
        sandbox.cleanup().expect("clean sandbox");
    }

    #[test]
    fn offline_supervisor_rejects_network_authority_before_launch() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let run_spec = RunSpec::new(
            "unit-network-denied",
            "genome",
            "world",
            repository.path(),
            "deterministic fixture",
            CapabilitySet::new(true, true),
            Budget::new(Duration::from_secs(2), 1_000, 0).expect("budget"),
        )
        .expect("run specification");
        let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
        let marker = sandbox.execution_dir().join("process-started");
        let mut runtime = test_runtime("/usr/bin/touch", [marker.to_string_lossy().into_owned()]);

        assert!(matches!(
            runtime.start(&run_spec, &sandbox, &token),
            Err(RuntimeError::CapabilityDenied)
        ));
        assert!(!marker.exists());
        sandbox.cleanup().expect("clean sandbox");
    }

    #[test]
    fn a_child_that_exits_nonzero_without_reading_its_prompt_is_a_provider_failure() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        // A 2 MiB prompt cannot fit in the pipe buffer, so the write always
        // meets a broken pipe after `false` exits.
        let prompt = "x".repeat(2 * 1024 * 1024);
        let run_spec = spec_with_prompt(
            "unit-stdin-nonzero",
            repository.path(),
            Duration::from_secs(2),
            1_000,
            prompt,
        );
        let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
        let mut runtime = test_runtime("/usr/bin/false", []);
        runtime
            .start(&run_spec, &sandbox, &token)
            .expect("start early-exit child");
        let snapshot = wait_for_terminal(&mut runtime, run_spec.run_id());
        assert_eq!(snapshot.status, RunStatus::Failed);
        assert_eq!(
            snapshot.completion_reason,
            Some(CompletionReason::ProviderFailure)
        );
        sandbox.cleanup().expect("clean sandbox");
    }

    #[test]
    fn incomplete_stdin_delivery_is_an_io_failure() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let prompt = "x".repeat(2 * 1024 * 1024);
        let run_spec = spec_with_prompt(
            "unit-stdin-failure",
            repository.path(),
            Duration::from_secs(2),
            1_000,
            prompt,
        );
        let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
        let mut runtime = test_runtime("/usr/bin/true", []);
        runtime
            .start(&run_spec, &sandbox, &token)
            .expect("start early-exit child");
        let snapshot = wait_for_terminal(&mut runtime, run_spec.run_id());
        assert_eq!(snapshot.status, RunStatus::Failed);
        assert_eq!(
            snapshot.completion_reason,
            Some(CompletionReason::IoFailure)
        );
        sandbox.cleanup().expect("clean sandbox");
    }

    #[test]
    fn test_only_launch_path_exercises_output_and_wall_budgets() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let output_spec = spec("unit-output", repository.path(), Duration::from_secs(2), 17);
        let (output_sandbox, output_token) =
            manager.create(&output_spec).expect("create output sandbox");
        let mut output = test_runtime("/usr/bin/yes", []);
        output
            .start(&output_spec, &output_sandbox, &output_token)
            .expect("start output");
        let exceeded = wait_for_terminal(&mut output, output_spec.run_id());
        assert_eq!(exceeded.status, RunStatus::Failed);
        assert_eq!(
            exceeded.completion_reason,
            Some(CompletionReason::OutputBudgetExceeded)
        );
        assert!(
            fs::metadata(exceeded.stdout_path)
                .expect("stdout metadata")
                .len()
                + fs::metadata(exceeded.stderr_path)
                    .expect("stderr metadata")
                    .len()
                <= 17
        );
        output_sandbox.cleanup().expect("clean output sandbox");

        let timeout_spec = spec(
            "unit-timeout",
            repository.path(),
            Duration::from_millis(20),
            1_000,
        );
        let (timeout_sandbox, timeout_token) = manager
            .create(&timeout_spec)
            .expect("create timeout sandbox");
        let mut timeout = test_runtime("/bin/sleep", ["2".to_owned()]);
        timeout
            .start(&timeout_spec, &timeout_sandbox, &timeout_token)
            .expect("start timeout");
        let timed_out = wait_for_terminal(&mut timeout, timeout_spec.run_id());
        assert_eq!(timed_out.status, RunStatus::TimedOut);
        assert_eq!(
            timed_out.completion_reason,
            Some(CompletionReason::WallBudgetExceeded)
        );
        timeout_sandbox.cleanup().expect("clean timeout sandbox");
    }

    #[test]
    fn test_only_launch_path_kills_descendant_process_group() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let run_spec = spec(
            "unit-interrupt",
            repository.path(),
            Duration::from_secs(10),
            1_000,
        );
        let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
        let script = sandbox.worktree().join("spawn-child");
        fs::write(
            &script,
            b"#!/bin/sh\n/bin/sleep 4 &\necho $! > \"$1\"\nwait\n",
        )
        .expect("write process fixture");
        let child_pid_path = sandbox.execution_dir().join("child.pid");
        let mut runtime = test_runtime(
            "/bin/sh",
            [
                script.display().to_string(),
                child_pid_path.display().to_string(),
            ],
        );
        runtime
            .start(&run_spec, &sandbox, &token)
            .expect("start process group");
        for _ in 0..500 {
            if child_pid_path.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let child_pid = fs::read_to_string(child_pid_path)
            .expect("read child PID")
            .trim()
            .to_owned();
        let interrupt_started = Instant::now();
        runtime
            .interrupt(run_spec.run_id())
            .expect("interrupt group");
        assert!(interrupt_started.elapsed() < Duration::from_secs(2));
        let snapshot = runtime
            .snapshot(run_spec.run_id())
            .expect("snapshot interrupt");
        assert_eq!(snapshot.status, RunStatus::Interrupted);
        assert_eq!(
            snapshot.completion_reason,
            Some(CompletionReason::OperatorInterrupt)
        );
        assert!(!process_is_alive(&child_pid));
        sandbox.cleanup().expect("clean sandbox");
    }

    /// Whether `pid` still names a live process. `kill -0` alone reports
    /// success for a zombie (an exited orphan PID 1 has not reaped yet); a
    /// zombie has already terminated, so it does not count as alive.
    fn process_is_alive(pid: &str) -> bool {
        let signalable = Command::new("/bin/kill")
            .args(["-0", pid])
            .output()
            .expect("probe child")
            .status
            .success();
        if !signalable {
            return false;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return true;
        };
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            != Some("Z")
    }

    #[test]
    fn dropping_active_supervisor_stops_its_process_group() {
        let repository = repository_fixture();
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let run_spec = spec(
            "unit-drop",
            repository.path(),
            Duration::from_secs(10),
            1_000,
        );
        let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
        let script = sandbox.worktree().join("spawn-child");
        fs::write(
            &script,
            b"#!/bin/sh\n/bin/sleep 4 &\necho $! > \"$1\"\nwait\n",
        )
        .expect("write process fixture");
        let child_pid_path = sandbox.execution_dir().join("child.pid");
        let mut runtime = test_runtime(
            "/bin/sh",
            [
                script.display().to_string(),
                child_pid_path.display().to_string(),
            ],
        );
        runtime
            .start(&run_spec, &sandbox, &token)
            .expect("start process group");
        for _ in 0..500 {
            if child_pid_path.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let child_pid = fs::read_to_string(child_pid_path)
            .expect("read child PID")
            .trim()
            .to_owned();
        let dropped_at = Instant::now();
        drop(runtime);
        assert!(dropped_at.elapsed() < Duration::from_secs(2));
        // The killed grandchild is reaped by init asynchronously, so allow a
        // short window before requiring that it no longer exists.
        let alive = || {
            Command::new("/bin/kill")
                .args(["-0", &child_pid])
                .output()
                .expect("probe child")
                .status
                .success()
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !alive(),
            "dropping the supervisor must stop its process group"
        );
        sandbox.cleanup().expect("clean sandbox");
    }

    fn test_runtime(
        executable: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = String>,
    ) -> SupervisedRuntime {
        SupervisedRuntime::deterministic(
            IsolationPolicy::unconfined_for_testing(),
            executable,
            arguments,
        )
        .expect("create test supervisor")
    }

    fn wait_for_terminal(runtime: &mut SupervisedRuntime, run_id: &str) -> RunSnapshot {
        for _ in 0..400 {
            let snapshot = runtime.snapshot(run_id).expect("snapshot run");
            if snapshot.status != RunStatus::Running {
                return snapshot;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("run did not become terminal");
    }

    fn spec(
        run_id: &str,
        repository: &std::path::Path,
        wall: Duration,
        maximum_output_bytes: usize,
    ) -> RunSpec {
        spec_with_prompt(
            run_id,
            repository,
            wall,
            maximum_output_bytes,
            "deterministic fixture".to_owned(),
        )
    }

    fn spec_with_prompt(
        run_id: &str,
        repository: &std::path::Path,
        wall: Duration,
        maximum_output_bytes: usize,
        prompt: String,
    ) -> RunSpec {
        RunSpec::new(
            run_id,
            "genome",
            "world",
            repository,
            prompt,
            CapabilitySet::new(true, false),
            Budget::new(wall, maximum_output_bytes, 0).expect("budget"),
        )
        .expect("run specification")
    }

    fn repository_fixture() -> TempDir {
        let directory = tempdir().expect("repository directory");
        run_git(directory.path(), &["init", "-q"]);
        fs::write(directory.path().join("fixture.txt"), b"fixture\n").expect("write fixture");
        run_git(directory.path(), &["add", "fixture.txt"]);
        run_git(
            directory.path(),
            &[
                "-c",
                "user.name=Hephaestus Tests",
                "-c",
                "user.email=hephaestus@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        );
        directory
    }

    fn run_git(repository: &std::path::Path, arguments: &[&str]) {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(repository)
                .args(arguments)
                .status()
                .expect("run Git fixture command")
                .success()
        );
    }

    fn shared_run() -> Arc<SharedRun> {
        Arc::new(SharedRun {
            observed: Mutex::new(ObservedRun {
                status: RunStatus::Running,
                exit_code: None,
                completion_reason: None,
                elapsed: Duration::ZERO,
            }),
            changed: Condvar::new(),
            interrupt: AtomicBool::new(false),
            output_exceeded: AtomicBool::new(false),
            io_failed: AtomicBool::new(false),
            stdin_closed_early: AtomicBool::new(false),
            guarded: false,
            cancel: Mutex::new(None),
        })
    }
}
