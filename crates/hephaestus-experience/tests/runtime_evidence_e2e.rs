use std::{
    collections::BTreeMap,
    fs,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use hephaestus_experience::{
    EvidenceRecorder, Provenance, RecordedRuntime, RedactionPolicy, RetentionLimits, TraceInput,
    TraceKind, TraceReceipt,
};
use hephaestus_runtime::{
    AdapterCapabilities, Budget, CapabilityToken, CompletionReason, DeterministicRuntime, Provider,
    RunHandle, RunSnapshot, RunSpec, RunStatus, RuntimeAdapter, RuntimeError, RuntimeObservation,
    RuntimeObservationKind, Sandbox, SandboxManager,
};
use tempfile::{TempDir, tempdir};

#[test]
fn real_offline_run_emits_provider_observations_and_truthful_terminal_evidence() {
    let fixture = Fixture::new("observed-run", false);
    let evidence = tempdir().expect("evidence directory");
    let mut runtime = RecordedRuntime::new(
        DeterministicRuntime::default(),
        recorder(&evidence, 32, 16_384),
    )
    .expect("create recorded runtime");
    assert_eq!(runtime.provider(), Provider::Deterministic);
    assert!(runtime.report_capabilities().snapshot);
    assert_eq!(runtime.inner().provider(), Provider::Deterministic);

    runtime
        .start(&fixture.spec, &fixture.sandbox, &fixture.token)
        .expect("start observed run");
    let terminal = runtime
        .snapshot(fixture.spec.run_id())
        .expect("snapshot observed run");
    assert_eq!(terminal.status, RunStatus::Succeeded);
    assert_eq!(terminal.completion_reason, Some(CompletionReason::Success));
    let initial_receipts = receipts(&runtime);
    assert_eq!(
        kinds(&initial_receipts),
        [
            TraceKind::LifecycleStarted,
            TraceKind::ContextComposed,
            TraceKind::FileRead,
            TraceKind::ModelResponse,
            TraceKind::LifecycleCompleted,
        ]
    );
    assert_artifacts_are_safe(&runtime, &initial_receipts, fixture.spec.prompt());
    assert!(artifact(&runtime, &initial_receipts[0]).contains(fixture.spec.source_revision()));
    let completion = artifact(&runtime, &initial_receipts[4]);
    assert!(completion.contains("\"completion_reason\":\"success\""));
    assert!(completion.contains("\"actual_cost_microusd\":\"0\""));
    assert!(completion.contains("latency_millis"));

    runtime
        .snapshot(fixture.spec.run_id())
        .expect("repeat terminal snapshot");
    assert_eq!(receipts(&runtime).len(), 5);
    assert_eq!(
        runtime
            .drain_observations(fixture.spec.run_id())
            .expect("wrapper observation drain"),
        []
    );

    runtime
        .resume(
            &fixture.spec,
            &fixture.sandbox,
            &fixture.token,
            "checkpoint-secret",
        )
        .expect("resume observed run");
    runtime
        .snapshot(fixture.spec.run_id())
        .expect("snapshot resumed run");
    let resumed = receipts(&runtime);
    assert_eq!(resumed[5].kind, TraceKind::LifecycleResumed);
    assert!(!artifact(&runtime, &resumed[5]).contains("checkpoint-secret"));
    assert!(artifact(&runtime, &resumed[5]).contains("checkpoint_used_hash"));
    assert!(artifact(&runtime, &resumed[5]).contains(fixture.spec.source_revision()));
    fixture.cleanup();
}

#[test]
fn running_polls_preserve_the_reserved_terminal_slot() {
    let fixture = Fixture::new("polling-run", false);
    let evidence = tempdir().expect("evidence directory");
    let (inner, control) = ScriptedRuntime::new(Provider::Codex);
    let mut runtime =
        RecordedRuntime::new(inner, recorder(&evidence, 2, 16_384)).expect("recorded runtime");
    runtime
        .start(&fixture.spec, &fixture.sandbox, &fixture.token)
        .expect("start polling run");
    for _ in 0..20 {
        assert_eq!(
            runtime
                .snapshot(fixture.spec.run_id())
                .expect("poll running state")
                .status,
            RunStatus::Running
        );
    }
    assert_eq!(receipts(&runtime).len(), 1);
    control.terminal(RunStatus::Succeeded, CompletionReason::Success, Some(0));
    runtime
        .snapshot(fixture.spec.run_id())
        .expect("persist terminal state");
    assert_eq!(
        kinds(&receipts(&runtime)),
        [TraceKind::LifecycleStarted, TraceKind::LifecycleCompleted]
    );
    fixture.cleanup();
}

#[test]
fn output_budget_reason_comes_from_runtime_snapshot() {
    let fixture = Fixture::new_with_output("tiny-output", false, 1);
    let evidence = tempdir().expect("evidence directory");
    let mut runtime = RecordedRuntime::new(
        DeterministicRuntime::default(),
        recorder(&evidence, 16, 16_384),
    )
    .expect("create recorded runtime");
    runtime
        .start(&fixture.spec, &fixture.sandbox, &fixture.token)
        .expect("start tiny run");
    let snapshot = runtime
        .snapshot(fixture.spec.run_id())
        .expect("snapshot tiny run");
    assert_eq!(
        snapshot.completion_reason,
        Some(CompletionReason::OutputBudgetExceeded)
    );
    let receipt = receipts(&runtime).pop().expect("completion receipt");
    assert_eq!(receipt.kind, TraceKind::LifecycleCompleted);
    assert!(artifact(&runtime, &receipt).contains("output_budget_exceeded"));
    fixture.cleanup();
}

#[test]
fn adapter_failures_are_recorded_and_denials_never_become_active() {
    let denied = Fixture::new("denied-run", true);
    let denied_evidence = tempdir().expect("denied evidence");
    let mut denied_runtime = RecordedRuntime::new(
        DeterministicRuntime::default(),
        recorder(&denied_evidence, 8, 16_384),
    )
    .expect("create denied runtime");
    assert!(matches!(
        denied_runtime.start(&denied.spec, &denied.sandbox, &denied.token),
        Err(RuntimeError::CapabilityDenied)
    ));
    assert_eq!(
        receipts(&denied_runtime)[0].kind,
        TraceKind::CapabilityDenied
    );
    assert!(matches!(
        denied_runtime.snapshot(denied.spec.run_id()),
        Err(RuntimeError::InvalidSpec(_))
    ));
    denied.cleanup();

    let failed = Fixture::new("failed-start", false);
    let failed_evidence = tempdir().expect("failed evidence");
    let (inner, control) = ScriptedRuntime::new(Provider::Claude);
    control.fail(FailurePoint::Start);
    let mut failed_runtime = RecordedRuntime::new(inner, recorder(&failed_evidence, 8, 16_384))
        .expect("create failed runtime");
    assert!(matches!(
        failed_runtime.start(&failed.spec, &failed.sandbox, &failed.token),
        Err(RuntimeError::Unsupported(_))
    ));
    assert_eq!(receipts(&failed_runtime)[0].kind, TraceKind::Error);
    failed.cleanup();
}

#[test]
fn failed_containment_remains_addressable_until_termination_is_confirmed() {
    let interrupt_fixture = Fixture::new("interrupt-evidence-failure", false);
    let interrupt_evidence = tempdir().expect("interrupt evidence");
    let (interrupt_inner, interrupt_control) = ScriptedRuntime::new(Provider::Claude);
    let mut interrupt_runtime =
        RecordedRuntime::new(interrupt_inner, recorder(&interrupt_evidence, 2, 16_384))
            .expect("interrupt runtime");
    interrupt_runtime
        .start(
            &interrupt_fixture.spec,
            &interrupt_fixture.sandbox,
            &interrupt_fixture.token,
        )
        .expect("start interrupt runtime");
    interrupt_control.fail(FailurePoint::Interrupt);
    assert!(matches!(
        interrupt_runtime.interrupt(interrupt_fixture.spec.run_id()),
        Err(RuntimeError::ContainmentFailed { .. })
    ));
    interrupt_control.fail(FailurePoint::None);
    interrupt_runtime
        .interrupt(interrupt_fixture.spec.run_id())
        .expect("retry failed interrupt evidence containment");
    assert_eq!(receipts(&interrupt_runtime).len(), 2);
    interrupt_fixture.cleanup();
}

#[test]
fn unpersisted_observations_cannot_be_lost_before_terminal_evidence() {
    let fixture = Fixture::new("pending-observations", false);
    let evidence = tempdir().expect("evidence directory");
    let (inner, control) = ScriptedRuntime::new(Provider::Claude);
    let mut runtime =
        RecordedRuntime::new(inner, recorder(&evidence, 4, 16_384)).expect("recorded runtime");
    runtime
        .start(&fixture.spec, &fixture.sandbox, &fixture.token)
        .expect("start pending-observation run");
    for sequence in 1..=3 {
        control.observe(RuntimeObservation::new(
            RuntimeObservationKind::Retry,
            BTreeMap::from([("sequence".to_owned(), sequence.to_string())]),
        ));
    }
    control.fail(FailurePoint::Interrupt);
    assert!(matches!(
        runtime.snapshot(fixture.spec.run_id()),
        Err(RuntimeError::ContainmentFailed { .. })
    ));
    assert_eq!(receipts(&runtime).len(), 3);

    control.fail(FailurePoint::None);
    control.terminal(RunStatus::Succeeded, CompletionReason::Success, Some(0));
    assert!(matches!(
        runtime.snapshot(fixture.spec.run_id()),
        Err(RuntimeError::Evidence(_))
    ));
    assert_eq!(receipts(&runtime).len(), 3);
    fixture.cleanup();
}

#[test]
fn interrupt_records_natural_completion_race_and_terminal_state_is_closed() {
    let fixture = Fixture::new("interrupt-race", false);
    let evidence = tempdir().expect("evidence directory");
    let (inner, control) = ScriptedRuntime::new(Provider::Claude);
    control.interrupt_as(RunStatus::Succeeded, CompletionReason::Success);
    let mut runtime =
        RecordedRuntime::new(inner, recorder(&evidence, 8, 16_384)).expect("recorded runtime");
    runtime
        .start(&fixture.spec, &fixture.sandbox, &fixture.token)
        .expect("start race run");
    control.observe(RuntimeObservation::new(
        RuntimeObservationKind::Retry,
        BTreeMap::new(),
    ));
    assert_eq!(
        RuntimeAdapter::drain_observations(&mut runtime, fixture.spec.run_id())
            .expect("guarded observation drain"),
        []
    );
    runtime
        .interrupt(fixture.spec.run_id())
        .expect("interrupt race run");
    let completion = receipts(&runtime).pop().expect("completion receipt");
    assert!(artifact(&runtime, &completion).contains("\"completion_reason\":\"success\""));
    assert!(artifact(&runtime, &completion).contains("\"latency_millis\":\"7\""));
    control.observe(RuntimeObservation::new(
        RuntimeObservationKind::Retry,
        BTreeMap::new(),
    ));
    runtime
        .snapshot(fixture.spec.run_id())
        .expect("terminal snapshot is idempotent");
    assert_eq!(
        kinds(&receipts(&runtime)),
        [
            TraceKind::LifecycleStarted,
            TraceKind::Retry,
            TraceKind::LifecycleCompleted,
        ]
    );
    fixture.cleanup();
}

#[test]
fn terminal_state_is_not_closed_when_canonical_append_fails() {
    let fixture = Fixture::new("terminal-append-failure", false);
    let evidence = tempdir().expect("evidence directory");
    let database = evidence.path().join("events.sqlite3");
    let artifacts = evidence.path().join("artifacts");
    let (inner, control) = ScriptedRuntime::new(Provider::Claude);
    let mut runtime =
        RecordedRuntime::new(inner, recorder(&evidence, 16, 16_384)).expect("recorded runtime");
    runtime
        .start(&fixture.spec, &fixture.sandbox, &fixture.token)
        .expect("start append-race run");

    let mut concurrent = EvidenceRecorder::open(
        database,
        artifacts,
        RedactionPolicy::new([]),
        RetentionLimits::new(16, 16_384).expect("retention limits"),
    )
    .expect("open concurrent writer");
    concurrent
        .record_trace(
            TraceInput::new(
                "concurrent-event",
                Provenance::new(fixture.spec.run_id(), "genome-1", "world-1").expect("provenance"),
                TraceKind::Retry,
                1,
                BTreeMap::new(),
            )
            .expect("trace input"),
        )
        .expect("advance canonical head");
    control.terminal(RunStatus::Succeeded, CompletionReason::Success, Some(0));
    assert!(runtime.snapshot(fixture.spec.run_id()).is_err());
    assert!(runtime.snapshot(fixture.spec.run_id()).is_err());
    fixture.cleanup();
}

#[test]
fn runtime_operation_failures_remain_evidence_backed() {
    let fixture = Fixture::new("operation-failures", false);
    let evidence = tempdir().expect("evidence directory");
    let (inner, control) = ScriptedRuntime::new(Provider::Claude);
    let mut runtime =
        RecordedRuntime::new(inner, recorder(&evidence, 16, 16_384)).expect("recorded runtime");
    runtime
        .start(&fixture.spec, &fixture.sandbox, &fixture.token)
        .expect("start scripted runtime");

    assert!(matches!(
        runtime.resume(
            &fixture.spec,
            &fixture.sandbox,
            &fixture.token,
            "active-checkpoint",
        ),
        Err(RuntimeError::InvalidSpec(_))
    ));

    control.fail(FailurePoint::Interrupt);
    assert!(runtime.interrupt(fixture.spec.run_id()).is_err());
    control.fail(FailurePoint::None);
    control.terminal(RunStatus::Succeeded, CompletionReason::Success, Some(0));
    runtime
        .snapshot(fixture.spec.run_id())
        .expect("finish scripted runtime before resume failure");
    fs::write(
        fixture.repository.path().join("fixture.txt"),
        b"a different immutable revision\n",
    )
    .expect("change repository revision");
    run_git(fixture.repository.path(), &["add", "fixture.txt"]);
    run_git(
        fixture.repository.path(),
        &["commit", "--quiet", "-m", "different revision"],
    );
    let mismatched_revision_spec = RunSpec::new(
        fixture.spec.run_id(),
        fixture.spec.genome_id(),
        fixture.spec.world_id(),
        fixture.spec.source_repository(),
        fixture.spec.prompt(),
        fixture.spec.capabilities(),
        fixture.spec.budget(),
    )
    .expect("mismatched revision spec");
    assert!(matches!(
        runtime.resume(
            &mismatched_revision_spec,
            &fixture.sandbox,
            &fixture.token,
            "mismatched-revision-checkpoint",
        ),
        Err(RuntimeError::InvalidSpec(_))
    ));
    let mismatched_spec = RunSpec::new_at_revision(
        fixture.spec.run_id(),
        "different-genome",
        "world-1",
        fixture.spec.source_repository(),
        fixture.spec.source_revision(),
        fixture.spec.prompt(),
        fixture.spec.capabilities(),
        fixture.spec.budget(),
    )
    .expect("mismatched resume spec");
    assert!(matches!(
        runtime.resume(
            &mismatched_spec,
            &fixture.sandbox,
            &fixture.token,
            "mismatched-checkpoint",
        ),
        Err(RuntimeError::InvalidSpec(_))
    ));
    control.fail(FailurePoint::Resume);
    assert!(
        runtime
            .resume(
                &fixture.spec,
                &fixture.sandbox,
                &fixture.token,
                "checkpoint",
            )
            .is_err()
    );
    control.fail(FailurePoint::Snapshot);
    assert!(runtime.snapshot(fixture.spec.run_id()).is_err());
    assert_eq!(
        kinds(&receipts(&runtime)),
        [
            TraceKind::LifecycleStarted,
            TraceKind::Error,
            TraceKind::LifecycleCompleted,
            TraceKind::Error,
            TraceKind::Error,
        ]
    );
    fixture.cleanup();
}

#[test]
#[allow(clippy::too_many_lines)]
fn evidence_and_adapter_protocol_failures_fail_closed() {
    let drain_fixture = Fixture::new("drain-failure", false);
    let drain_evidence = tempdir().expect("drain evidence");
    let (drain_inner, drain_control) = ScriptedRuntime::new(Provider::Claude);
    drain_control.fail(FailurePoint::Drain);
    let mut drain_runtime = RecordedRuntime::new(drain_inner, recorder(&drain_evidence, 8, 16_384))
        .expect("drain runtime");
    assert!(matches!(
        drain_runtime.start(
            &drain_fixture.spec,
            &drain_fixture.sandbox,
            &drain_fixture.token
        ),
        Err(RuntimeError::Unsupported(_))
    ));
    assert!(drain_runtime.snapshot(drain_fixture.spec.run_id()).is_err());
    drain_fixture.cleanup();

    let resume_fixture = Fixture::new("resume-drain-failure", false);
    let resume_evidence = tempdir().expect("resume evidence");
    let (resume_inner, resume_control) = ScriptedRuntime::new(Provider::Claude);
    let mut resume_runtime =
        RecordedRuntime::new(resume_inner, recorder(&resume_evidence, 8, 16_384))
            .expect("resume runtime");
    resume_runtime
        .start(
            &resume_fixture.spec,
            &resume_fixture.sandbox,
            &resume_fixture.token,
        )
        .expect("start resume runtime");
    resume_control.terminal(RunStatus::Succeeded, CompletionReason::Success, Some(0));
    resume_runtime
        .snapshot(resume_fixture.spec.run_id())
        .expect("finish run before resume");
    resume_control.fail(FailurePoint::Drain);
    assert!(
        resume_runtime
            .resume(
                &resume_fixture.spec,
                &resume_fixture.sandbox,
                &resume_fixture.token,
                "checkpoint",
            )
            .is_err()
    );
    resume_control.fail(FailurePoint::None);
    assert!(
        resume_runtime
            .snapshot(resume_fixture.spec.run_id())
            .is_err()
    );
    resume_fixture.cleanup();

    let capacity_fixture = Fixture::new("evidence-failure", false);
    let capacity_evidence = tempdir().expect("capacity evidence");
    let (capacity_inner, _) = ScriptedRuntime::new(Provider::Claude);
    let mut capacity_runtime =
        RecordedRuntime::new(capacity_inner, recorder(&capacity_evidence, 8, 1))
            .expect("capacity runtime");
    assert!(matches!(
        capacity_runtime.start(
            &capacity_fixture.spec,
            &capacity_fixture.sandbox,
            &capacity_fixture.token
        ),
        Err(RuntimeError::Evidence(_))
    ));
    assert!(
        capacity_runtime
            .snapshot(capacity_fixture.spec.run_id())
            .is_err()
    );
    capacity_fixture.cleanup();

    let snapshot_fixture = Fixture::new("snapshot-evidence-failure", false);
    let snapshot_evidence = tempdir().expect("snapshot evidence");
    let (snapshot_inner, snapshot_control) = ScriptedRuntime::new(Provider::Claude);
    let mut snapshot_runtime =
        RecordedRuntime::new(snapshot_inner, recorder(&snapshot_evidence, 2, 16_384))
            .expect("snapshot runtime");
    snapshot_runtime
        .start(
            &snapshot_fixture.spec,
            &snapshot_fixture.sandbox,
            &snapshot_fixture.token,
        )
        .expect("start snapshot runtime");
    snapshot_control.fail(FailurePoint::Snapshot);
    assert!(matches!(
        snapshot_runtime.snapshot(snapshot_fixture.spec.run_id()),
        Err(RuntimeError::Evidence(_))
    ));
    snapshot_control.fail(FailurePoint::None);
    assert!(
        snapshot_runtime
            .snapshot(snapshot_fixture.spec.run_id())
            .is_err()
    );
    snapshot_fixture.cleanup();

    let protocol_fixture = Fixture::new("protocol-failure", false);
    let protocol_evidence = tempdir().expect("protocol evidence");
    let (protocol_inner, protocol_control) = ScriptedRuntime::new(Provider::Claude);
    let mut protocol_runtime =
        RecordedRuntime::new(protocol_inner, recorder(&protocol_evidence, 8, 16_384))
            .expect("protocol runtime");
    protocol_runtime
        .start(
            &protocol_fixture.spec,
            &protocol_fixture.sandbox,
            &protocol_fixture.token,
        )
        .expect("start protocol runtime");
    protocol_control.interrupt_as(RunStatus::Running, CompletionReason::OperatorInterrupt);
    assert!(matches!(
        protocol_runtime.interrupt(protocol_fixture.spec.run_id()),
        Err(RuntimeError::Evidence(_))
    ));
    protocol_control.status_without_reason(RunStatus::Succeeded);
    assert!(matches!(
        protocol_runtime.snapshot(protocol_fixture.spec.run_id()),
        Err(RuntimeError::Evidence(_))
    ));
    protocol_fixture.cleanup();
}

fn receipts<R>(runtime: &RecordedRuntime<R>) -> Vec<TraceReceipt> {
    runtime
        .evidence()
        .replay_verified()
        .expect("replay evidence")
        .iter()
        .map(|event| serde_json::from_slice(&event.payload).expect("decode trace receipt"))
        .collect()
}

fn kinds(receipts: &[TraceReceipt]) -> Vec<TraceKind> {
    receipts.iter().map(|receipt| receipt.kind).collect()
}

fn artifact<R>(runtime: &RecordedRuntime<R>, receipt: &TraceReceipt) -> String {
    String::from_utf8(
        runtime
            .evidence()
            .artifact(&receipt.artifact_id)
            .expect("read trace artifact"),
    )
    .expect("UTF-8 trace artifact")
}

fn assert_artifacts_are_safe(
    runtime: &RecordedRuntime<DeterministicRuntime>,
    receipts: &[TraceReceipt],
    prompt: &str,
) {
    assert!(receipts.iter().all(|receipt| {
        receipt.provenance.run_id() == "observed-run"
            && receipt.provenance.genome_id() == "genome-1"
            && receipt.provenance.world_id() == "world-1"
            && !artifact(runtime, receipt).contains(prompt)
    }));
}

fn recorder(directory: &TempDir, maximum_records: usize, maximum_bytes: usize) -> EvidenceRecorder {
    EvidenceRecorder::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("artifacts"),
        RedactionPolicy::new(["known-secret".to_owned()]),
        RetentionLimits::new(maximum_records, maximum_bytes).expect("retention limits"),
    )
    .expect("open evidence recorder")
}

struct Fixture {
    repository: TempDir,
    _sandboxes: TempDir,
    spec: RunSpec,
    sandbox: Sandbox,
    token: CapabilityToken,
}

impl Fixture {
    fn new(run_id: &str, network: bool) -> Self {
        Self::new_with_output(run_id, network, 1_000_000)
    }

    fn new_with_output(run_id: &str, network: bool, output_bytes: usize) -> Self {
        let repository = repository_fixture();
        let sandboxes = tempdir().expect("sandbox directory");
        let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let spec = RunSpec::new(
            run_id,
            "genome-1",
            "world-1",
            repository.path(),
            "inventory the repository without exposing this prompt",
            hephaestus_core::authority::CapabilitySet::new(true, network),
            Budget::new(Duration::from_secs(2), output_bytes, 0).expect("budget"),
        )
        .expect("run spec");
        let (sandbox, token) = manager.create(&spec).expect("create sandbox");
        Self {
            repository,
            _sandboxes: sandboxes,
            spec,
            sandbox,
            token,
        }
    }

    fn cleanup(self) {
        self.sandbox.cleanup().expect("clean sandbox");
    }
}

fn repository_fixture() -> TempDir {
    let directory = tempdir().expect("repository directory");
    run_git(directory.path(), &["init", "--quiet"]);
    run_git(
        directory.path(),
        &["config", "user.email", "tests@hephaestus.invalid"],
    );
    run_git(
        directory.path(),
        &["config", "user.name", "Hephaestus Tests"],
    );
    fs::write(
        directory.path().join("fixture.txt"),
        b"deterministic fixture\n",
    )
    .expect("write repository fixture");
    run_git(directory.path(), &["add", "fixture.txt"]);
    run_git(directory.path(), &["commit", "--quiet", "-m", "fixture"]);
    directory
}

fn run_git(directory: &std::path::Path, arguments: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .expect("run git fixture command")
            .success(),
        "git fixture command failed: {arguments:?}"
    );
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FailurePoint {
    None,
    Start,
    Resume,
    Interrupt,
    Snapshot,
    Drain,
}

struct ScriptedState {
    status: RunStatus,
    reason: Option<CompletionReason>,
    exit_code: Option<i32>,
    failure: FailurePoint,
    interrupt_terminal: (RunStatus, CompletionReason),
    observations: Vec<RuntimeObservation>,
}

#[derive(Clone)]
struct ScriptedControl(Arc<Mutex<ScriptedState>>);

impl ScriptedControl {
    fn fail(&self, failure: FailurePoint) {
        self.0.lock().expect("scripted state").failure = failure;
    }

    fn terminal(&self, status: RunStatus, reason: CompletionReason, exit_code: Option<i32>) {
        let mut state = self.0.lock().expect("scripted state");
        state.status = status;
        state.reason = Some(reason);
        state.exit_code = exit_code;
    }

    fn interrupt_as(&self, status: RunStatus, reason: CompletionReason) {
        self.0.lock().expect("scripted state").interrupt_terminal = (status, reason);
    }

    fn status_without_reason(&self, status: RunStatus) {
        let mut state = self.0.lock().expect("scripted state");
        state.status = status;
        state.reason = None;
    }

    fn observe(&self, observation: RuntimeObservation) {
        self.0
            .lock()
            .expect("scripted state")
            .observations
            .push(observation);
    }
}

struct ScriptedRuntime {
    provider: Provider,
    state: Arc<Mutex<ScriptedState>>,
    stdout_path: std::path::PathBuf,
    stderr_path: std::path::PathBuf,
}

impl ScriptedRuntime {
    fn new(provider: Provider) -> (Self, ScriptedControl) {
        let state = Arc::new(Mutex::new(ScriptedState {
            status: RunStatus::Running,
            reason: None,
            exit_code: None,
            failure: FailurePoint::None,
            interrupt_terminal: (RunStatus::Interrupted, CompletionReason::OperatorInterrupt),
            observations: Vec::new(),
        }));
        (
            Self {
                provider,
                state: Arc::clone(&state),
                stdout_path: std::path::PathBuf::new(),
                stderr_path: std::path::PathBuf::new(),
            },
            ScriptedControl(state),
        )
    }

    fn fails(&self, point: FailurePoint) -> bool {
        self.state.lock().expect("scripted state").failure == point
    }
}

impl RuntimeAdapter for ScriptedRuntime {
    fn provider(&self) -> Provider {
        self.provider
    }

    fn report_capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            resume: true,
            interrupt: true,
            snapshot: true,
            authority: hephaestus_core::authority::CapabilitySet::new(true, false),
        }
    }

    fn start(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        _token: &CapabilityToken,
    ) -> Result<RunHandle, RuntimeError> {
        if self.fails(FailurePoint::Start) {
            return Err(RuntimeError::Unsupported("injected start failure"));
        }
        self.stdout_path = sandbox.execution_dir().join("stdout.log");
        self.stderr_path = sandbox.execution_dir().join("stderr.log");
        Ok(RunHandle {
            run_id: spec.run_id().to_owned(),
            provider: self.provider,
        })
    }

    fn resume(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        token: &CapabilityToken,
        _checkpoint: &str,
    ) -> Result<RunHandle, RuntimeError> {
        if self.fails(FailurePoint::Resume) {
            return Err(RuntimeError::Unsupported("injected resume failure"));
        }
        self.start(spec, sandbox, token)
    }

    fn interrupt(&mut self, _run_id: &str) -> Result<(), RuntimeError> {
        if self.fails(FailurePoint::Interrupt) {
            return Err(RuntimeError::Unsupported("injected interrupt failure"));
        }
        let mut state = self.state.lock().expect("scripted state");
        let terminal = state.interrupt_terminal;
        state.status = terminal.0;
        state.reason = Some(terminal.1);
        Ok(())
    }

    fn snapshot(&mut self, run_id: &str) -> Result<RunSnapshot, RuntimeError> {
        if self.fails(FailurePoint::Snapshot) {
            return Err(RuntimeError::Unsupported("injected snapshot failure"));
        }
        let state = self.state.lock().expect("scripted state");
        Ok(RunSnapshot {
            run_id: run_id.to_owned(),
            status: state.status,
            exit_code: state.exit_code,
            completion_reason: state.reason,
            elapsed: Duration::from_millis(7),
            stdout_path: self.stdout_path.clone(),
            stderr_path: self.stderr_path.clone(),
            capabilities: hephaestus_core::authority::CapabilitySet::new(true, false),
        })
    }

    fn drain_observations(
        &mut self,
        _run_id: &str,
    ) -> Result<Vec<RuntimeObservation>, RuntimeError> {
        if self.fails(FailurePoint::Drain) {
            return Err(RuntimeError::Unsupported("injected drain failure"));
        }
        Ok(std::mem::take(
            &mut self.state.lock().expect("scripted state").observations,
        ))
    }
}
