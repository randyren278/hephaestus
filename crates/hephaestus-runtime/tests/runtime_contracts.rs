use std::{fs, process::Command, thread, time::Duration};

#[cfg(target_os = "macos")]
use std::{os::unix::fs::PermissionsExt, time::Instant};

use hephaestus_core::authority::CapabilitySet;
use hephaestus_runtime::{
    Budget, DeterministicRuntime, ExperimentContext, IsolationBackend, IsolationPolicy, Provider,
    ProviderInvocation, ReferenceInstruction, RunSnapshot, RunSpec, RunStatus, RuntimeAdapter,
    RuntimeError, SandboxManager, SupervisedRuntime,
};
use tempfile::tempdir;

#[test]
fn isolated_worktrees_bind_expiring_tokens_to_one_run() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let first_spec = spec("run-one", repository.path(), 1_000_000, false);
    let second_spec = spec("run-two", repository.path(), 1_000_000, false);
    let (first, first_token) = manager.create(&first_spec).expect("create first sandbox");
    let (second, second_token) = manager.create(&second_spec).expect("create second sandbox");

    assert_ne!(first.worktree(), second.worktree());
    assert!(
        first
            .authorize(&first_token, first_spec.capabilities())
            .is_ok()
    );
    assert!(matches!(
        first.authorize(&second_token, first_spec.capabilities()),
        Err(RuntimeError::CapabilityDenied)
    ));
    assert!(matches!(
        first.authorize(&first_token, CapabilitySet::new(true, true)),
        Err(RuntimeError::CapabilityDenied)
    ));

    let protected = tempdir().expect("protected directory");
    let protected_file = protected.path().join("canonical-secret");
    fs::write(&protected_file, b"must remain hidden").expect("write protected fixture");
    let policy = IsolationPolicy::detect(vec![protected.path().to_owned()]);
    let sibling_probe = ProviderInvocation::deterministic(
        "/bin/cat",
        [second
            .worktree()
            .join("fixture.txt")
            .to_string_lossy()
            .into_owned()],
        [],
    )
    .expect("build sibling probe");
    match policy.backend() {
        IsolationBackend::MacOsSeatbelt => {
            let own_probe = ProviderInvocation::deterministic(
                "/bin/cat",
                [first
                    .worktree()
                    .join("fixture.txt")
                    .to_string_lossy()
                    .into_owned()],
                [],
            )
            .expect("build own probe");
            assert!(
                policy
                    .command(&own_probe, &first)
                    .expect("build own sandbox command")
                    .status()
                    .expect("run own sandbox probe")
                    .success()
            );
            assert!(
                !policy
                    .command(&sibling_probe, &first)
                    .expect("build sibling sandbox command")
                    .status()
                    .expect("run sibling sandbox probe")
                    .success()
            );
            let protected_probe = ProviderInvocation::deterministic(
                "/bin/cat",
                [protected_file.to_string_lossy().into_owned()],
                [],
            )
            .expect("build protected probe");
            assert!(
                !policy
                    .command(&protected_probe, &first)
                    .expect("build protected sandbox command")
                    .status()
                    .expect("run protected sandbox probe")
                    .success()
            );
        }
        IsolationBackend::Unavailable => assert!(matches!(
            policy.command(&sibling_probe, &first),
            Err(RuntimeError::Unsupported(_))
        )),
        #[cfg(feature = "test-support")]
        IsolationBackend::TestOnlyUnconfined => {
            unreachable!("detected production policy cannot select the test-only backend")
        }
    }

    first.cleanup().expect("clean first sandbox");
    second.cleanup().expect("clean second sandbox");
}

#[test]
fn run_specs_pin_the_resolved_revision_before_head_moves() {
    let repository = repository_fixture();
    let original_revision = git_stdout(repository.path(), &["rev-parse", "HEAD"]);
    let run_spec = spec("pinned-revision", repository.path(), 1_000_000, false);
    assert_eq!(run_spec.source_revision(), original_revision);

    fs::write(
        repository.path().join("fixture.txt"),
        b"changed after specification\n",
    )
    .expect("change fixture");
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
            "move head",
        ],
    );
    assert_ne!(
        git_stdout(repository.path(), &["rev-parse", "HEAD"]),
        original_revision
    );
    let moved_spec = spec("pinned-revision", repository.path(), 1_000_000, false);
    let historical_spec = RunSpec::new_at_revision(
        "historical-revision",
        "hephaestus:genome:test",
        "hephaestus:world:test",
        repository.path(),
        "HEAD~1",
        "inventory the isolated worktree",
        CapabilitySet::new(false, false),
        Budget::new(Duration::from_secs(5), 1_000_000, 0).expect("budget"),
    )
    .expect("resolve historical tree-ish");
    assert_eq!(historical_spec.source_revision(), original_revision);

    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let (sandbox, token) = manager.create(&run_spec).expect("create pinned sandbox");
    assert_eq!(
        fs::read(sandbox.worktree().join("fixture.txt")).expect("read pinned fixture"),
        b"deterministic fixture\n"
    );
    assert_eq!(
        git_stdout(sandbox.worktree(), &["rev-parse", "HEAD"]),
        original_revision
    );
    assert_eq!(sandbox.source_revision(), original_revision);
    assert!(matches!(
        DeterministicRuntime::default().start(&moved_spec, &sandbox, &token),
        Err(RuntimeError::InvalidSpec(_))
    ));
    assert!(matches!(
        ProviderInvocation::codex("codex", &moved_spec, &sandbox),
        Err(RuntimeError::InvalidSpec(_))
    ));
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn run_specs_reject_unresolvable_revisions_at_construction() {
    let repository = repository_fixture();
    let empty = RunSpec::new_at_revision(
        "empty-revision",
        "hephaestus:genome:test",
        "hephaestus:world:test",
        repository.path(),
        "   ",
        "inventory the isolated worktree",
        CapabilitySet::new(false, false),
        Budget::new(Duration::from_secs(5), 1_000_000, 0).expect("budget"),
    );
    assert!(matches!(empty, Err(RuntimeError::InvalidSpec(_))));
    let result = RunSpec::new_at_revision(
        "invalid-revision",
        "hephaestus:genome:test",
        "hephaestus:world:test",
        repository.path(),
        "refs/heads/does-not-exist",
        "inventory the isolated worktree",
        CapabilitySet::new(false, false),
        Budget::new(Duration::from_secs(5), 1_000_000, 0).expect("budget"),
    );
    assert!(matches!(result, Err(RuntimeError::Git(_))));
}

#[test]
fn experiment_run_ids_match_the_signed_result_length_contract() {
    let repository = repository_fixture();
    let prompt = "bounded experiment input";
    let experiment = ExperimentContext::new("task", prompt, 7, "environment-v1").unwrap();
    let build = |run_id: String| {
        RunSpec::new_for_experiment_at_revision(
            run_id,
            "hephaestus:genome:test",
            "hephaestus:world:test",
            repository.path(),
            "HEAD",
            prompt,
            CapabilitySet::new(false, false),
            Budget::new(Duration::from_secs(5), 1_000_000, 0).unwrap(),
            experiment.clone(),
        )
    };

    assert!(build("r".repeat(128)).is_ok());
    assert!(matches!(
        build("r".repeat(129)),
        Err(RuntimeError::InvalidSpec("run_id is not path safe"))
    ));
}

#[test]
fn sandbox_rejects_specs_for_another_run_or_repository_at_the_same_revision() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let original = spec("binding-run", repository.path(), 1_000_000, false);
    let (sandbox, token) = manager.create(&original).expect("create sandbox");
    let other_run = RunSpec::new_at_revision(
        "other-run",
        original.genome_id(),
        original.world_id(),
        original.source_repository(),
        original.source_revision(),
        original.prompt(),
        original.capabilities(),
        original.budget(),
    )
    .expect("other run spec");
    assert!(matches!(
        DeterministicRuntime::default().start(&other_run, &sandbox, &token),
        Err(RuntimeError::InvalidSpec(_))
    ));

    let alias_root = tempdir().expect("repository alias directory");
    let repository_alias = alias_root.path().join("repository-alias");
    std::os::unix::fs::symlink(repository.path(), &repository_alias).expect("repository symlink");
    let other_repository = RunSpec::new_at_revision(
        original.run_id(),
        original.genome_id(),
        original.world_id(),
        &repository_alias,
        original.source_revision(),
        original.prompt(),
        original.capabilities(),
        original.budget(),
    )
    .expect("aliased repository spec");
    assert!(matches!(
        DeterministicRuntime::default().start(&other_repository, &sandbox, &token),
        Err(RuntimeError::InvalidSpec(_))
    ));
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn deterministic_runtime_starts_resumes_interrupts_and_snapshots_real_worktrees() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec("reference", repository.path(), 1_000_000, false);
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
    let mut runtime = DeterministicRuntime::default();

    let codex =
        ProviderInvocation::codex("codex", &run_spec, &sandbox).expect("build Codex invocation");
    assert_eq!(codex.provider(), Provider::Codex);
    assert!(
        codex
            .arguments()
            .iter()
            .any(|argument| argument == "--ephemeral")
    );
    assert!(
        codex
            .arguments()
            .iter()
            .any(|argument| argument == "workspace-write")
    );
    assert!(
        !codex
            .arguments()
            .iter()
            .any(|argument| argument == run_spec.prompt())
    );
    assert_eq!(codex.stdin(), run_spec.prompt().as_bytes());
    let claude =
        ProviderInvocation::claude("claude", &run_spec, &sandbox).expect("build Claude invocation");
    assert_eq!(claude.provider(), Provider::Claude);
    assert!(
        claude
            .arguments()
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "dontAsk"])
    );

    let handle = runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start reference runtime");
    assert_eq!(handle.run_id, "reference");
    let snapshot = runtime.snapshot("reference").expect("snapshot run");
    assert_eq!(snapshot.status, RunStatus::Succeeded);
    let output: serde_json::Value = serde_json::from_slice(
        &fs::read(&snapshot.stdout_path).expect("read deterministic output"),
    )
    .expect("decode deterministic output");
    assert_eq!(output["schema_version"], 1);
    assert_eq!(output["files"][0]["path"], "fixture.txt");

    let other_spec = spec("other-token", repository.path(), 1_000_000, false);
    let (other_sandbox, other_token) = manager.create(&other_spec).expect("create other sandbox");
    assert!(matches!(
        runtime.resume(&run_spec, &sandbox, &other_token, "rejected-checkpoint"),
        Err(RuntimeError::CapabilityDenied)
    ));
    assert_eq!(
        runtime
            .snapshot("reference")
            .expect("original run survives failed resume")
            .status,
        RunStatus::Succeeded
    );
    other_sandbox.cleanup().expect("clean other sandbox");

    runtime
        .resume(&run_spec, &sandbox, &token, "checkpoint-1")
        .expect("resume reference runtime");
    let resumed = runtime.snapshot("reference").expect("snapshot resumed run");
    let resumed_output: serde_json::Value =
        serde_json::from_slice(&fs::read(&resumed.stdout_path).expect("read resumed output"))
            .expect("decode resumed output");
    assert_eq!(resumed_output["checkpoint"], "checkpoint-1");

    runtime
        .resume(&run_spec, &sandbox, &token, "checkpoint-2")
        .expect("resume for interrupt");
    assert!(matches!(
        runtime.resume(&run_spec, &sandbox, &token, "checkpoint-while-running"),
        Err(RuntimeError::InvalidSpec(_))
    ));
    runtime.interrupt("reference").expect("interrupt run");
    assert_eq!(
        runtime
            .snapshot("reference")
            .expect("snapshot interrupt")
            .status,
        RunStatus::Interrupted
    );
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
#[cfg(target_os = "macos")]
#[allow(clippy::too_many_lines)]
fn supervisor_streams_stdin_and_reports_bounded_success_artifacts() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec_with_budget(
        "supervised-cat",
        repository.path(),
        Duration::from_secs(2),
        1_000,
    );
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
    let mut runtime = SupervisedRuntime::deterministic(isolation_policy(), "/bin/cat", [])
        .expect("create supervised runtime");
    assert_eq!(runtime.provider(), Provider::Deterministic);
    assert!(SupervisedRuntime::deterministic(isolation_policy(), "", []).is_err());

    let handle = runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start supervised process");
    assert_eq!(handle.provider, Provider::Deterministic);
    assert!(!runtime.report_capabilities().resume);
    let snapshot = wait_for_terminal(&mut runtime, "supervised-cat");
    assert_eq!(snapshot.status, RunStatus::Succeeded);
    assert_eq!(snapshot.exit_code, Some(0));
    assert_eq!(
        snapshot.completion_reason,
        Some(hephaestus_runtime::CompletionReason::Success)
    );
    assert!(!snapshot.elapsed.is_zero());
    assert_eq!(
        fs::read_to_string(snapshot.stdout_path).expect("read supervised stdout"),
        run_spec.prompt()
    );
    assert!(
        fs::read(snapshot.stderr_path)
            .expect("read supervised stderr")
            .is_empty()
    );
    assert!(matches!(
        runtime.resume(&run_spec, &sandbox, &token, "checkpoint"),
        Err(RuntimeError::Unsupported(_))
    ));
    assert!(runtime.start(&run_spec, &sandbox, &token).is_err());
    sandbox.cleanup().expect("clean sandbox");

    let failure_spec = spec_with_budget(
        "supervised-failure",
        repository.path(),
        Duration::from_secs(2),
        1_000,
    );
    let (failure_sandbox, failure_token) = manager
        .create(&failure_spec)
        .expect("create failure sandbox");
    let mut failure_runtime =
        SupervisedRuntime::deterministic(isolation_policy(), "/usr/bin/false", [])
            .expect("create failing runtime");
    failure_runtime
        .start(&failure_spec, &failure_sandbox, &failure_token)
        .expect("start failing process");
    let failure = wait_for_terminal(&mut failure_runtime, "supervised-failure");
    assert_eq!(failure.status, RunStatus::Failed);
    assert_ne!(failure.exit_code, Some(0));
    assert_eq!(
        failure.completion_reason,
        Some(hephaestus_runtime::CompletionReason::ProviderFailure)
    );
    failure_sandbox.cleanup().expect("clean failure sandbox");

    let environment_spec = spec_with_budget(
        "supervised-environment",
        repository.path(),
        Duration::from_secs(2),
        10_000,
    );
    let (environment_sandbox, environment_token) = manager
        .create(&environment_spec)
        .expect("create environment sandbox");
    let mut environment_runtime =
        SupervisedRuntime::deterministic(isolation_policy(), "/usr/bin/env", [])
            .expect("create environment runtime");
    environment_runtime
        .start(&environment_spec, &environment_sandbox, &environment_token)
        .expect("start environment process");
    let environment = wait_for_terminal(&mut environment_runtime, "supervised-environment");
    let variables = fs::read_to_string(environment.stdout_path).expect("read child environment");
    assert!(variables.lines().all(|line| {
        line.starts_with("HOME=") || line.starts_with("PATH=") || line.starts_with("TMPDIR=")
    }));
    let execution_dir = environment_sandbox.execution_dir().to_string_lossy();
    assert!(variables.contains(&format!("HOME={execution_dir}")));
    assert!(variables.contains(&format!("TMPDIR={execution_dir}")));
    environment_sandbox
        .cleanup()
        .expect("clean environment sandbox");

    let network_spec = spec("supervised-network", repository.path(), 1_000, true);
    let (network_sandbox, network_token) = manager
        .create(&network_spec)
        .expect("create network sandbox");
    assert!(matches!(
        failure_runtime.start(&network_spec, &network_sandbox, &network_token),
        Err(RuntimeError::CapabilityDenied)
    ));
    network_sandbox.cleanup().expect("clean network sandbox");
}

#[test]
fn supervisor_fails_closed_without_a_verified_isolation_backend() {
    if isolation_policy().backend() != IsolationBackend::Unavailable {
        return;
    }
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec("unavailable-isolation", repository.path(), 1_000, false);
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
    let marker = sandbox.execution_dir().join("process-started");
    let mut runtime = SupervisedRuntime::deterministic(
        isolation_policy(),
        "/usr/bin/touch",
        [marker.to_string_lossy().into_owned()],
    )
    .expect("create supervised runtime");

    assert!(matches!(
        runtime.start(&run_spec, &sandbox, &token),
        Err(RuntimeError::Unsupported(_))
    ));
    assert!(!marker.exists());
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn supervisor_rejects_unrepresentable_deadline_before_spawn() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec_with_budget(
        "supervised-excessive-wall",
        repository.path(),
        Duration::MAX,
        1_000,
    );
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
    let mut runtime = SupervisedRuntime::deterministic(isolation_policy(), "/bin/cat", [])
        .expect("create supervised runtime");
    assert!(matches!(
        runtime.start(&run_spec, &sandbox, &token),
        Err(RuntimeError::InvalidSpec(_))
    ));
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
#[cfg(target_os = "macos")]
fn supervisor_enforces_combined_output_and_wall_budgets() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");

    let output_spec = spec_with_budget(
        "supervised-output",
        repository.path(),
        Duration::from_secs(2),
        17,
    );
    let (output_sandbox, output_token) =
        manager.create(&output_spec).expect("create output sandbox");
    let mut output_runtime =
        SupervisedRuntime::deterministic(isolation_policy(), "/usr/bin/yes", [])
            .expect("create output runtime");
    output_runtime
        .start(&output_spec, &output_sandbox, &output_token)
        .expect("start output process");
    let output = wait_for_terminal(&mut output_runtime, "supervised-output");
    assert_eq!(output.status, RunStatus::Failed);
    assert_eq!(
        output.completion_reason,
        Some(hephaestus_runtime::CompletionReason::OutputBudgetExceeded)
    );
    let persisted = fs::metadata(&output.stdout_path)
        .expect("stdout metadata")
        .len()
        + fs::metadata(&output.stderr_path)
            .expect("stderr metadata")
            .len();
    assert!(persisted <= 17);
    output_sandbox.cleanup().expect("clean output sandbox");

    let timeout_spec = spec_with_budget(
        "supervised-timeout",
        repository.path(),
        Duration::from_millis(20),
        1_000,
    );
    let (timeout_sandbox, timeout_token) = manager
        .create(&timeout_spec)
        .expect("create timeout sandbox");
    let mut timeout_runtime =
        SupervisedRuntime::deterministic(isolation_policy(), "/bin/sleep", ["2".to_owned()])
            .expect("create timeout runtime");
    timeout_runtime
        .start(&timeout_spec, &timeout_sandbox, &timeout_token)
        .expect("start timeout process");
    let timeout = wait_for_terminal(&mut timeout_runtime, "supervised-timeout");
    assert_eq!(timeout.status, RunStatus::TimedOut);
    assert_eq!(
        timeout.completion_reason,
        Some(hephaestus_runtime::CompletionReason::WallBudgetExceeded)
    );
    timeout_sandbox.cleanup().expect("clean timeout sandbox");
}

#[test]
#[cfg(target_os = "macos")]
fn supervisor_interrupt_waits_for_process_group_termination() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec_with_budget(
        "supervised-interrupt",
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
    .expect("write child process fixture");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
        .expect("make child process fixture executable");
    let child_pid_path = sandbox.execution_dir().join("child.pid");
    let mut runtime = SupervisedRuntime::deterministic(
        isolation_policy(),
        &script,
        [child_pid_path.display().to_string()],
    )
    .expect("create interrupt runtime");
    runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start interrupt process");
    for _ in 0..500 {
        if child_pid_path.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !child_pid_path.is_file() {
        let snapshot = runtime
            .snapshot("supervised-interrupt")
            .expect("snapshot failed child launch");
        panic!(
            "child PID was not written; status={:?}, stderr={}",
            snapshot.status,
            fs::read_to_string(snapshot.stderr_path).expect("read child stderr")
        );
    }
    let child_pid = fs::read_to_string(&child_pid_path)
        .expect("read child PID")
        .trim()
        .to_owned();
    let interrupt_started = Instant::now();
    runtime
        .interrupt("supervised-interrupt")
        .expect("interrupt process");
    assert!(interrupt_started.elapsed() < Duration::from_secs(2));
    let interrupted = runtime
        .snapshot("supervised-interrupt")
        .expect("snapshot interrupt");
    assert_eq!(interrupted.status, RunStatus::Interrupted);
    assert_eq!(
        interrupted.completion_reason,
        Some(hephaestus_runtime::CompletionReason::OperatorInterrupt)
    );
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", &child_pid])
            .output()
            .expect("probe child process")
            .status
            .success()
    );
    assert!(runtime.interrupt("missing").is_err());
    assert!(runtime.snapshot("missing").is_err());
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn deterministic_runtime_reports_wall_overrun_instead_of_success() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec_with_budget(
        "deterministic-wall-overrun",
        repository.path(),
        Duration::from_nanos(1),
        1_000_000,
    );
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
    let mut runtime = DeterministicRuntime::default();

    runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start deterministic runtime");
    let snapshot = runtime
        .snapshot(run_spec.run_id())
        .expect("snapshot wall overrun");

    assert_eq!(snapshot.status, RunStatus::TimedOut);
    assert_eq!(
        snapshot.completion_reason,
        Some(hephaestus_runtime::CompletionReason::WallBudgetExceeded)
    );
    assert!(fs::read(&snapshot.stdout_path).unwrap().is_empty());
    assert_eq!(
        fs::read(&snapshot.stderr_path).unwrap(),
        b"wall budget exceeded\n"
    );
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn deterministic_runtime_reports_inventory_git_failures() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec("missing-worktree", repository.path(), 1_000_000, false);
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
    let worktree = sandbox.worktree().to_owned();
    let moved_worktree = worktree.with_extension("temporarily-moved");
    let mut runtime = DeterministicRuntime::default();

    runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start deterministic runtime");
    fs::rename(&worktree, &moved_worktree).expect("temporarily move worktree");
    let result = runtime.snapshot(run_spec.run_id());
    fs::rename(&moved_worktree, &worktree).expect("restore worktree");

    assert!(matches!(
        result,
        Err(RuntimeError::Git(message)) if message == "tracked-file inventory failed"
    ));
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn deterministic_runtime_skips_tracked_symlinks() {
    let repository = repository_fixture();
    std::os::unix::fs::symlink("fixture.txt", repository.path().join("tracked-link"))
        .expect("create tracked symlink");
    run_git(repository.path(), &["add", "tracked-link"]);
    run_git(
        repository.path(),
        &[
            "-c",
            "user.name=Hephaestus Tests",
            "-c",
            "user.email=hephaestus@example.invalid",
            "commit",
            "-qm",
            "tracked symlink",
        ],
    );
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let run_spec = spec("tracked-symlink", repository.path(), 1_000_000, false);
    let (sandbox, token) = manager.create(&run_spec).expect("create sandbox");
    let mut runtime = DeterministicRuntime::default();

    runtime
        .start(&run_spec, &sandbox, &token)
        .expect("start deterministic runtime");
    let snapshot = runtime
        .snapshot(run_spec.run_id())
        .expect("snapshot deterministic runtime");
    let output: serde_json::Value = serde_json::from_slice(
        &fs::read(&snapshot.stdout_path).expect("read deterministic output"),
    )
    .expect("decode deterministic output");

    assert_eq!(output["files"].as_array().unwrap().len(), 1);
    assert_eq!(output["files"][0]["path"], "fixture.txt");
    sandbox.cleanup().expect("clean sandbox");
}

#[test]
fn runtime_fails_closed_for_expiry_network_and_output_budget() {
    let repository = repository_fixture();
    let sandboxes = tempdir().expect("sandbox directory");
    let manager = SandboxManager::open(sandboxes.path(), Duration::from_secs(30))
        .expect("open sandbox manager");
    let network_spec = spec("network", repository.path(), 1_000_000, true);
    let (network_sandbox, network_token) = manager
        .create(&network_spec)
        .expect("create network sandbox");
    let network_invocation = ProviderInvocation::codex("codex", &network_spec, &network_sandbox)
        .expect("build network invocation");
    assert!(
        network_invocation
            .arguments()
            .iter()
            .any(|argument| argument == "sandbox_workspace_write.network_access=true")
    );
    let mut runtime = DeterministicRuntime::default();
    assert!(matches!(
        runtime.start(&network_spec, &network_sandbox, &network_token),
        Err(RuntimeError::CapabilityDenied)
    ));
    network_sandbox.cleanup().expect("clean network sandbox");

    let tiny_spec = spec("tiny", repository.path(), 1, false);
    let (tiny_sandbox, tiny_token) = manager.create(&tiny_spec).expect("create tiny sandbox");
    runtime
        .start(&tiny_spec, &tiny_sandbox, &tiny_token)
        .expect("start tiny run");
    assert_eq!(
        runtime.snapshot("tiny").expect("snapshot tiny run").status,
        RunStatus::Failed
    );
    tiny_sandbox.cleanup().expect("clean tiny sandbox");

    let expiring_root = tempdir().expect("expiring sandbox directory");
    let expiring = SandboxManager::open(expiring_root.path(), Duration::from_millis(1))
        .expect("open expiring manager");
    let expired_spec = spec("expired", repository.path(), 1_000_000, false);
    let (expired_sandbox, expired_token) = expiring
        .create(&expired_spec)
        .expect("create expiring sandbox");
    thread::sleep(Duration::from_millis(5));
    assert!(matches!(
        runtime.start(&expired_spec, &expired_sandbox, &expired_token),
        Err(RuntimeError::CapabilityDenied)
    ));
    expired_sandbox.cleanup().expect("clean expired sandbox");
}

#[test]
fn budgets_and_run_specs_reject_invalid_inputs() {
    assert!(Budget::new(Duration::ZERO, 1, 0).is_err());
    assert!(Budget::new(Duration::from_secs(1), 0, 0).is_err());
    let budget = Budget::new(Duration::from_secs(2), 10, 7).expect("valid budget");
    assert_eq!(budget.wall(), Duration::from_secs(2));
    assert_eq!(budget.maximum_cost_microusd(), 7);

    let repository = repository_fixture();
    let experiment = ExperimentContext::new("task-1", b"prompt", 42, "linux-arm64-v1")
        .expect("experiment context");
    let contextual = RunSpec::new_for_experiment(
        "paired-run",
        "genome",
        "world",
        repository.path(),
        "prompt",
        CapabilitySet::new(false, false),
        budget,
        experiment.clone(),
    )
    .expect("contextual spec");
    assert_eq!(contextual.experiment(), &experiment);
    assert_eq!(contextual.experiment().task_id(), "task-1");
    assert_eq!(contextual.experiment().seed(), 42);
    assert_eq!(contextual.experiment().environment_id(), "linux-arm64-v1");
    assert_eq!(
        contextual.experiment().input_commitment(),
        blake3::hash(b"prompt").to_hex().as_str()
    );
    assert!(
        RunSpec::new_for_experiment(
            "mismatched-input",
            "genome",
            "world",
            repository.path(),
            "different prompt",
            CapabilitySet::new(false, false),
            budget,
            experiment.clone(),
        )
        .is_err()
    );
    assert!(ExperimentContext::new("bad task", b"prompt", 0, "env").is_err());
    assert!(ExperimentContext::new("task", b"prompt", 0, "bad environment").is_err());
    assert!(matches!(
        RunSpec::new(
            "../escape",
            "genome",
            "world",
            repository.path(),
            "prompt",
            CapabilitySet::new(false, false),
            budget
        ),
        Err(RuntimeError::InvalidSpec(_))
    ));
    assert!(matches!(
        RunSpec::new_for_experiment(
            "../escape",
            "genome",
            "world",
            repository.path(),
            "prompt",
            CapabilitySet::new(false, false),
            budget,
            experiment,
        ),
        Err(RuntimeError::InvalidSpec(_))
    ));
    assert!(matches!(
        RunSpec::new(
            "valid",
            " ",
            "world",
            repository.path(),
            "prompt",
            CapabilitySet::new(false, false),
            budget
        ),
        Err(RuntimeError::InvalidSpec(_))
    ));
    assert!(matches!(
        RunSpec::new(
            "valid",
            "genome",
            "world",
            repository.path().join("missing"),
            "prompt",
            CapabilitySet::new(false, false),
            budget
        ),
        Err(RuntimeError::InvalidSpec(_))
    ));
}

#[test]
fn reference_instruction_keeps_world_input_and_experiment_unchanged() {
    let repository = repository_fixture();
    let budget = Budget::new(Duration::from_secs(2), 10, 7).expect("valid budget");
    let experiment = ExperimentContext::new("task-1", b"prompt", 42, "linux-arm64-v1")
        .expect("experiment context");
    let contextual = RunSpec::new_for_experiment(
        "paired-run",
        "genome",
        "world",
        repository.path(),
        "prompt",
        CapabilitySet::new(false, false),
        budget,
        experiment,
    )
    .expect("contextual spec");
    let instructed = contextual
        .clone()
        .with_reference_instruction(ReferenceInstruction::AsciiUppercase)
        .expect("in-range reference input");
    assert_eq!(instructed.prompt(), contextual.prompt());
    assert_eq!(instructed.experiment(), contextual.experiment());
    assert_eq!(
        instructed.reference_instruction(),
        Some(ReferenceInstruction::AsciiUppercase)
    );
}

#[test]
fn reference_instruction_rejects_oversized_task_input() {
    let budget = Budget::new(Duration::from_secs(2), 10, 7).expect("valid budget");
    let repository = repository_fixture();
    let oversized_prompt = "x".repeat(1_048_577);
    let oversized_context = ExperimentContext::new(
        "large-task",
        oversized_prompt.as_bytes(),
        42,
        "linux-arm64-v1",
    )
    .expect("large task context");
    let oversized_spec = RunSpec::new_for_experiment(
        "oversized-reference-run",
        "genome",
        "world",
        repository.path(),
        oversized_prompt,
        CapabilitySet::new(false, false),
        budget,
        oversized_context,
    )
    .expect("ordinary spec accepts bounded-by-daemon large task");
    assert!(
        oversized_spec
            .with_reference_instruction(ReferenceInstruction::Identity)
            .is_err()
    );
}

#[test]
fn provider_and_sandbox_setup_reject_invalid_inputs() {
    let budget = Budget::new(Duration::from_secs(2), 10, 7).expect("valid budget");
    let repository = repository_fixture();
    let root = tempdir().expect("sandbox root");
    assert!(SandboxManager::open(root.path(), Duration::ZERO).is_err());
    let unsafe_root = root.path().join("file");
    fs::write(&unsafe_root, b"not a directory").expect("write unsafe root");
    assert!(SandboxManager::open(&unsafe_root, Duration::from_secs(1)).is_err());
    let manager = SandboxManager::open(root.path().join("valid"), Duration::from_secs(30))
        .expect("open valid manager");
    let read_only = RunSpec::new(
        "read-only",
        "genome",
        "world",
        repository.path(),
        "prompt",
        CapabilitySet::new(false, false),
        budget,
    )
    .expect("read-only spec");
    let (sandbox, token) = manager
        .create(&read_only)
        .expect("create read-only sandbox");
    let codex = ProviderInvocation::codex("codex", &read_only, &sandbox).expect("Codex command");
    assert_eq!(codex.program(), std::path::Path::new("codex"));
    assert!(
        codex
            .arguments()
            .iter()
            .any(|argument| argument == "read-only")
    );
    let claude =
        ProviderInvocation::claude("claude", &read_only, &sandbox).expect("Claude command");
    assert!(claude.arguments().iter().any(|argument| argument == "Read"));
    assert!(ProviderInvocation::codex("", &read_only, &sandbox).is_err());

    let mismatched = spec("mismatch", repository.path(), 1000, true);
    assert!(matches!(
        ProviderInvocation::claude("claude", &mismatched, &sandbox),
        Err(RuntimeError::CapabilityDenied)
    ));
    let mut runtime = DeterministicRuntime::default();
    assert_eq!(runtime.provider(), Provider::Deterministic);
    runtime
        .start(&read_only, &sandbox, &token)
        .expect("start read-only run");
    assert!(runtime.start(&read_only, &sandbox, &token).is_err());
    assert!(runtime.resume(&read_only, &sandbox, &token, " ").is_err());
    assert!(runtime.interrupt("missing").is_err());
    assert!(runtime.snapshot("missing").is_err());
    sandbox.cleanup().expect("clean read-only sandbox");

    let invalid_repository = tempdir().expect("invalid repository");
    let invalid_spec = RunSpec::new(
        "not-git",
        "genome",
        "world",
        invalid_repository.path(),
        "prompt",
        CapabilitySet::new(false, false),
        budget,
    );
    assert!(matches!(invalid_spec, Err(RuntimeError::Git(_))));

    let vanished_repository = repository_fixture();
    let vanished_spec = spec(
        "vanished-before-create",
        vanished_repository.path(),
        1000,
        false,
    );
    let vanished_path = vanished_repository.keep();
    fs::remove_dir_all(vanished_path).expect("remove repository before sandbox creation");
    assert!(matches!(
        manager.create(&vanished_spec),
        Err(RuntimeError::RollbackFailed {
            operation,
            git_failed: true,
            filesystem_failed: false,
        }) if matches!(*operation, RuntimeError::Git(_))
    ));
    assert!(!root.path().join("valid/vanished-before-create").exists());

    let removed_repository = repository_fixture();
    let removed_spec = spec("cleanup-failure", removed_repository.path(), 1000, false);
    let (orphaned, _token) = manager
        .create(&removed_spec)
        .expect("create cleanup-failure sandbox");
    let removed_path = removed_repository.keep();
    fs::remove_dir_all(removed_path).expect("remove source repository");
    let orphaned_root = root.path().join("valid/cleanup-failure");
    assert!(matches!(
        orphaned.cleanup(),
        Err(RuntimeError::CleanupFailed {
            git_failed: true,
            filesystem_failed: false,
        })
    ));
    assert!(!orphaned_root.exists());
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

fn git_stdout(repository: &std::path::Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("execute git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn spec(
    run_id: &str,
    repository: &std::path::Path,
    maximum_output_bytes: usize,
    network: bool,
) -> RunSpec {
    RunSpec::new(
        run_id,
        "hephaestus:genome:test",
        "hephaestus:world:test",
        repository,
        "inventory the isolated worktree",
        CapabilitySet::new(true, network),
        Budget::new(Duration::from_secs(5), maximum_output_bytes, 0).expect("budget"),
    )
    .expect("RunSpec")
}

fn spec_with_budget(
    run_id: &str,
    repository: &std::path::Path,
    wall: Duration,
    maximum_output_bytes: usize,
) -> RunSpec {
    RunSpec::new(
        run_id,
        "hephaestus:genome:test",
        "hephaestus:world:test",
        repository,
        "supervised prompt",
        CapabilitySet::new(true, false),
        Budget::new(wall, maximum_output_bytes, 0).expect("budget"),
    )
    .expect("RunSpec")
}

// Only the macOS Seatbelt tests above call this helper; on other hosts it
// would otherwise trip `dead_code` under `-D warnings`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn wait_for_terminal(runtime: &mut SupervisedRuntime, run_id: &str) -> RunSnapshot {
    for _ in 0..400 {
        let snapshot = runtime.snapshot(run_id).expect("snapshot supervised run");
        if snapshot.status != RunStatus::Running {
            return snapshot;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("supervised run did not terminate");
}

fn isolation_policy() -> IsolationPolicy {
    IsolationPolicy::detect(Vec::new())
}

fn run_git(repository: &std::path::Path, arguments: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()
        .expect("run git");
    assert!(status.success());
}
