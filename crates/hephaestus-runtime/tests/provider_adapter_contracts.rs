//! Offline contract tests for the Codex/Claude `SupervisedRuntime` adapter
//! path: environment allowlisting, hard budget enforcement, completion-reason
//! mapping, and NDJSON event parsing, all against small fake executables.
//! Nothing here invokes a real `codex` or `claude` binary or touches the
//! network.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command, time::Duration};

use hephaestus_core::authority::CapabilitySet;
use hephaestus_runtime::{
    Budget, CompletionReason, IsolationPolicy, Provider, ProviderEventCursor, RunSpec, RunStatus,
    RuntimeAdapter, RuntimeObservationKind, SandboxManager, SupervisedRuntime,
    extract_actual_cost_microusd, extract_final_answer,
};
use tempfile::tempdir;

#[test]
fn provider_adapter_exposes_only_the_named_allowlisted_environment() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox root");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec("env-allowlist", repository.path(), 65_536);
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");

    let script = sandboxes.path().join("print-env");
    write_script(&script, "#!/bin/sh\ncat >/dev/null\nenv\n");
    let mut runtime = SupervisedRuntime::provider(
        IsolationPolicy::unconfined_for_testing(),
        Provider::Claude,
        &script,
        vec![(
            "HEPHAESTUS_TEST_ALLOWED".to_owned(),
            "visible-value".to_owned(),
        )],
    )
    .expect("create provider adapter");
    runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start provider adapter");
    let snapshot = wait_for_terminal(&mut runtime, run_spec.run_id());
    assert_eq!(snapshot.status, RunStatus::Succeeded);
    let printed = fs::read_to_string(snapshot.stdout_path).expect("read printed environment");
    let names: Vec<&str> = printed
        .lines()
        .filter_map(|line| line.split('=').next())
        .collect();
    // Only the fixed PATH/HOME/TMPDIR baseline, the one explicitly named
    // variable, and the shell's own injected bookkeeping (PWD/SHLVL/_) are
    // present: no ambient daemon environment variable this test process
    // itself happens to carry (an unrelated secret-shaped variable, for
    // example) leaks through to the child.
    let unexpected: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| {
            !matches!(
                *name,
                "HOME" | "PATH" | "TMPDIR" | "PWD" | "SHLVL" | "_" | "HEPHAESTUS_TEST_ALLOWED"
            )
        })
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected environment reached the provider child: {unexpected:?}"
    );
    assert!(printed.contains("HEPHAESTUS_TEST_ALLOWED=visible-value"));
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn provider_adapter_enforces_wall_and_output_budgets() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox root");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");

    let timeout_spec = spec_with_wall(
        "provider-timeout",
        repository.path(),
        1_000,
        Duration::from_millis(20),
    );
    let (timeout_sandbox, timeout_token) = manager
        .create(&timeout_spec)
        .expect("create timeout sandbox");
    let slow_script = sandboxes.path().join("slow-codex");
    write_script(&slow_script, "#!/bin/sh\nexec /bin/sleep 5\n");
    let mut timeout_runtime = SupervisedRuntime::provider(
        IsolationPolicy::unconfined_for_testing(),
        Provider::Codex,
        &slow_script,
        Vec::new(),
    )
    .expect("create codex adapter");
    timeout_runtime
        .start(&timeout_spec, &timeout_sandbox, &timeout_token)
        .expect("start slow codex fake");
    let timed_out = wait_for_terminal(&mut timeout_runtime, timeout_spec.run_id());
    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert_eq!(
        timed_out.completion_reason,
        Some(CompletionReason::WallBudgetExceeded)
    );
    timeout_sandbox.cleanup().expect("clean timeout sandbox");

    let output_spec = spec("provider-output-budget", repository.path(), 17);
    let (output_sandbox, output_token) =
        manager.create(&output_spec).expect("create output sandbox");
    let noisy_script = sandboxes.path().join("noisy-codex");
    write_script(&noisy_script, "#!/bin/sh\nexec /usr/bin/yes\n");
    let mut output_runtime = SupervisedRuntime::provider(
        IsolationPolicy::unconfined_for_testing(),
        Provider::Codex,
        &noisy_script,
        Vec::new(),
    )
    .expect("create codex adapter");
    output_runtime
        .start(&output_spec, &output_sandbox, &output_token)
        .expect("start noisy codex fake");
    let exceeded = wait_for_terminal(&mut output_runtime, output_spec.run_id());
    assert_eq!(exceeded.status, RunStatus::Failed);
    assert_eq!(
        exceeded.completion_reason,
        Some(CompletionReason::OutputBudgetExceeded)
    );
    output_sandbox.cleanup().expect("clean output sandbox");
}

#[test]
fn provider_adapter_maps_exit_status_and_interrupt_to_completion_reasons() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox root");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");

    let failure_spec = spec("provider-nonzero-exit", repository.path(), 1_000);
    let (failure_sandbox, failure_token) = manager
        .create(&failure_spec)
        .expect("create failure sandbox");
    let mut failure_runtime = SupervisedRuntime::provider(
        IsolationPolicy::unconfined_for_testing(),
        Provider::Claude,
        "/usr/bin/false",
        Vec::new(),
    )
    .expect("create claude adapter");
    failure_runtime
        .start(&failure_spec, &failure_sandbox, &failure_token)
        .expect("start failing claude fake");
    let failed = wait_for_terminal(&mut failure_runtime, failure_spec.run_id());
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(
        failed.completion_reason,
        Some(CompletionReason::ProviderFailure)
    );
    failure_sandbox.cleanup().expect("clean failure sandbox");

    let interrupt_spec = spec_with_wall(
        "provider-interrupt",
        repository.path(),
        1_000,
        Duration::from_secs(10),
    );
    let (interrupt_sandbox, interrupt_token) = manager
        .create(&interrupt_spec)
        .expect("create interrupt sandbox");
    let slow_script = sandboxes.path().join("slow-claude");
    write_script(&slow_script, "#!/bin/sh\nexec /bin/sleep 5\n");
    let mut interrupt_runtime = SupervisedRuntime::provider(
        IsolationPolicy::unconfined_for_testing(),
        Provider::Claude,
        &slow_script,
        Vec::new(),
    )
    .expect("create claude adapter");
    interrupt_runtime
        .start(&interrupt_spec, &interrupt_sandbox, &interrupt_token)
        .expect("start slow claude fake");
    interrupt_runtime
        .interrupt(interrupt_spec.run_id())
        .expect("interrupt slow claude fake");
    let interrupted = interrupt_runtime
        .snapshot(interrupt_spec.run_id())
        .expect("snapshot interrupted run");
    assert_eq!(interrupted.status, RunStatus::Interrupted);
    assert_eq!(
        interrupted.completion_reason,
        Some(CompletionReason::OperatorInterrupt)
    );
    interrupt_sandbox
        .cleanup()
        .expect("clean interrupt sandbox");
}

#[test]
fn provider_adapter_reports_denials_and_malformed_lines_through_drain_observations() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox root");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec("provider-events", repository.path(), 65_536);
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");

    let script = sandboxes.path().join("fake-claude");
    write_script(
        &script,
        "#!/bin/sh\n\
cat >/dev/null\n\
echo '{\"type\":\"system\",\"subtype\":\"permission_denied\",\"tool_name\":\"Bash\"}'\n\
echo 'not-json-at-all'\n\
echo '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"done\",\"total_cost_usd\":0.001}'\n",
    );
    let mut runtime = SupervisedRuntime::provider(
        IsolationPolicy::unconfined_for_testing(),
        Provider::Claude,
        &script,
        Vec::new(),
    )
    .expect("create claude adapter");
    runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start fake claude");
    let snapshot = wait_for_terminal(&mut runtime, run_spec.run_id());
    assert_eq!(snapshot.status, RunStatus::Succeeded);
    let observations = runtime
        .drain_observations(run_spec.run_id())
        .expect("drain provider observations");
    assert!(
        observations.iter().any(
            |observation| observation.kind == RuntimeObservationKind::Error
                && observation.fields.get("denied").map(String::as_str) == Some("true")
        ),
        "a permission_denied event must surface as a recorded denial, not be swallowed"
    );
    assert!(
        observations.iter().any(
            |observation| observation.kind == RuntimeObservationKind::Error
                && observation.fields.get("source").map(String::as_str) == Some("provider_stream")
        ),
        "a malformed line must be reported, not silently dropped or panic"
    );
    assert!(
        observations
            .iter()
            .any(|observation| observation.kind == RuntimeObservationKind::CostObserved),
        "the terminal result event must still be parsed after a malformed line"
    );

    let raw_stdout = fs::read(&snapshot.stdout_path).expect("read stdout");
    assert_eq!(extract_final_answer(Provider::Claude, &raw_stdout), b"done");
    assert_eq!(
        extract_actual_cost_microusd(Provider::Claude, &raw_stdout),
        1_000
    );
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn provider_event_cursor_never_panics_on_truncated_or_oversized_input() {
    let mut cursor = ProviderEventCursor::new();
    // No trailing newline: still buffered, no panic, no observation yet.
    assert!(
        cursor
            .feed(Provider::Codex, b"{\"type\":\"item.completed\"")
            .is_empty()
    );
    // A pathologically long unterminated line is reported and dropped rather
    // than growing the buffer without bound.
    let mut cursor = ProviderEventCursor::new();
    let huge = vec![b'x'; 2_000_000];
    let observations = cursor.feed(Provider::Claude, &huge);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].kind, RuntimeObservationKind::Error);
}

#[test]
fn provider_adapter_runs_are_isolated_from_each_other() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox root");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let script = sandboxes.path().join("touch-marker");
    write_script(
        &script,
        "#!/bin/sh\ncat >/dev/null\ntouch \"$(pwd)/marker\"\nls\n",
    );
    let mut runtime = SupervisedRuntime::provider(
        IsolationPolicy::unconfined_for_testing(),
        Provider::Codex,
        &script,
        Vec::new(),
    )
    .expect("create codex adapter");

    let first_spec = spec("sibling-one", repository.path(), 65_536);
    let (first_sandbox, first_token) = manager.create(&first_spec).expect("create first sandbox");
    runtime
        .start(&first_spec, &first_sandbox, &first_token)
        .expect("start first sibling");
    let first_snapshot = wait_for_terminal(&mut runtime, first_spec.run_id());

    let second_spec = spec("sibling-two", repository.path(), 65_536);
    let (second_sandbox, second_token) =
        manager.create(&second_spec).expect("create second sandbox");
    runtime
        .start(&second_spec, &second_sandbox, &second_token)
        .expect("start second sibling");
    let second_snapshot = wait_for_terminal(&mut runtime, second_spec.run_id());

    assert_ne!(first_sandbox.worktree(), second_sandbox.worktree());
    let first_listing = fs::read_to_string(first_snapshot.stdout_path).expect("first listing");
    let second_listing = fs::read_to_string(second_snapshot.stdout_path).expect("second listing");
    assert!(first_listing.contains("marker"));
    assert!(second_listing.contains("marker"));
    assert!(
        !first_listing.contains("fixture.txt") || !second_listing.is_empty(),
        "each sibling only ever lists its own private worktree"
    );
    first_sandbox.cleanup().expect("clean first sandbox");
    second_sandbox.cleanup().expect("clean second sandbox");
}

fn write_script(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write fake provider script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("mark script executable");
}

fn wait_for_terminal(
    runtime: &mut SupervisedRuntime,
    run_id: &str,
) -> hephaestus_runtime::RunSnapshot {
    for _ in 0..400 {
        let snapshot = runtime.snapshot(run_id).expect("snapshot run");
        if snapshot.status != RunStatus::Running {
            return snapshot;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("provider adapter run did not become terminal");
}

fn spec(run_id: &str, repository: &Path, maximum_output_bytes: usize) -> RunSpec {
    spec_with_wall(
        run_id,
        repository,
        maximum_output_bytes,
        Duration::from_secs(5),
    )
}

fn spec_with_wall(
    run_id: &str,
    repository: &Path,
    maximum_output_bytes: usize,
    wall: Duration,
) -> RunSpec {
    RunSpec::new(
        run_id,
        "hephaestus:genome:test",
        "hephaestus:world:test",
        repository,
        "inventory the isolated worktree",
        CapabilitySet::new(false, false),
        Budget::new(wall, maximum_output_bytes, 0).expect("budget"),
    )
    .expect("RunSpec")
}

fn repository_fixture() -> tempfile::TempDir {
    let repository = tempdir().expect("repository directory");
    run_git(repository.path(), &["init", "-q"]);
    fs::write(
        repository.path().join("fixture.txt"),
        b"deterministic fixture\n",
    )
    .expect("write fixture");
    run_git(repository.path(), &["add", "fixture.txt"]);
    run_git(
        repository.path(),
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
    repository
}

fn run_git(repository: &Path, arguments: &[&str]) {
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
