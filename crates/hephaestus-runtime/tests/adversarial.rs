//! Adversarial sandbox-escape probes for roadmap item 15 (Public Gauntlet,
//! production hardening, and self-dogfooding).
//!
//! These exercise the same production `IsolatedWorker` path as
//! `worker_contracts.rs`'s `worker_policy_denies_protected_paths` (which
//! already proves a candidate cannot read a sealed evaluator file placed
//! under an explicitly protected path — that is the "evaluator leakage" and
//! "reading evaluator files" category from docs/ADVERSARIAL.md and is
//! referenced there rather than duplicated here) but attack two properties
//! that test does not cover: writing to the host filesystem outside the
//! worker's own root, and opening a real network socket. Both are denied by
//! `IsolationPolicy::worker_command`'s default-deny Seatbelt profile without
//! needing an explicit protected-path entry.
//!
//! Live-sandbox only: macOS Seatbelt is the only isolation backend this
//! repository has (see `docs/THREAT_MODEL.md`); on any other host
//! `IsolationPolicy::detect` reports `Unavailable` and these attacks would
//! never reach a real sandbox, so the tests are skipped rather than
//! asserting something meaningless.

use std::time::Duration;

use hephaestus_runtime::{
    CompletionReason, IsolatedWorker, IsolationPolicy, WorkerDomain, WorkerLimits,
};
use tempfile::tempdir;

#[test]
#[cfg(target_os = "macos")]
fn sandbox_escape_write_outside_worker_root_is_denied() {
    let root = tempdir().expect("worker root");
    let victim_dir = tempdir().expect("victim directory outside the worker root");
    let victim = victim_dir.path().join("escaped-write");

    let worker = IsolatedWorker::open(
        root.path(),
        IsolationPolicy::detect([]),
        WorkerDomain::Candidate,
        "/usr/bin/touch",
        [victim.to_string_lossy().into_owned()],
        WorkerLimits::new(Duration::from_secs(3), 1_000, 1_000).unwrap(),
    )
    .unwrap();

    let output = worker.execute("write-escape-probe", b"").unwrap();

    assert_eq!(output.completion_reason, CompletionReason::ProviderFailure);
    assert!(
        !victim.exists(),
        "a candidate worker wrote a file outside its own worker root"
    );
    assert!(root.path().read_dir().unwrap().next().is_none());
}

#[test]
#[cfg(target_os = "macos")]
fn sandbox_escape_network_connection_is_denied() {
    let root = tempdir().expect("worker root");

    // Workers never receive network authority regardless of the caller's
    // `IsolationPolicy` (see `IsolationPolicy::worker_command`, which always
    // builds its profile with `network = false`). A real TCP connect attempt
    // to a public address must fail closed at the sandbox boundary, not at
    // the network layer, so this holds even offline or air-gapped.
    let worker = IsolatedWorker::open(
        root.path(),
        IsolationPolicy::detect([]),
        WorkerDomain::Candidate,
        "/usr/bin/nc",
        [
            "-G".to_owned(),
            "2".to_owned(),
            "-w".to_owned(),
            "2".to_owned(),
            "-z".to_owned(),
            "1.1.1.1".to_owned(),
            "80".to_owned(),
        ],
        WorkerLimits::new(Duration::from_secs(5), 1_000, 1_000).unwrap(),
    )
    .unwrap();

    let output = worker.execute("network-escape-probe", b"").unwrap();

    assert_eq!(output.completion_reason, CompletionReason::ProviderFailure);
    assert!(root.path().read_dir().unwrap().next().is_none());
}

/// Credential-shaped environment variable names a merge/push/release step
/// could need, used by
/// `candidate_sandbox_never_receives_merge_or_release_credentials` below.
#[cfg(feature = "test-support")]
const CREDENTIAL_POISON: &[(&str, &str)] = &[
    ("GIT_ASKPASS", "/bin/false"),
    ("GIT_SSH_COMMAND", "ssh -i /secret/deploy_key"),
    ("GITHUB_TOKEN", "ghp_should_never_leak"),
    ("GH_TOKEN", "ghp_should_never_leak_either"),
    ("SSH_AUTH_SOCK", "/tmp/should-not-be-forwarded.sock"),
    ("NPM_TOKEN", "npm_should_never_leak"),
    ("CARGO_REGISTRY_TOKEN", "crates_io_should_never_leak"),
    ("COSIGN_PASSWORD", "should_never_leak"),
    ("SIGSTORE_ID_TOKEN", "should_never_leak"),
];
#[cfg(feature = "test-support")]
const INNER_MODE_MARKER: &str = "HEPHAESTUS_ADVERSARIAL_CREDENTIAL_PROBE_INNER";
#[cfg(feature = "test-support")]
const OUTPUT_MARKER: &str = "HEPHAESTUS-CREDENTIAL-PROBE-OUTPUT:";

/// Roadmap item 15's self-dogfooding slice (`docs/SELF_DOGFOODING.md`) needs
/// to prove that no merge or release credential is reachable from a
/// candidate sandbox. `IsolatedWorker::execute` calls `Command::env_clear()`
/// and then sets only `PATH`, `HOME`, and `TMPDIR` (see
/// `crates/hephaestus-runtime/src/worker.rs`).
///
/// The workspace forbids `unsafe_code`, and mutating this test binary's own
/// process environment (`std::env::set_var`) requires `unsafe` since Rust
/// 2024, so this test proves the claim without it: it re-executes itself as
/// a child process whose *ambient* environment is poisoned with every
/// credential shape a merge/push/release step could need (`Command::env` on
/// a fresh child is ordinary, safe API), asks that child to run just this
/// test in "inner" mode, and the inner run spawns a real `IsolatedWorker`
/// running `/usr/bin/env` to print exactly what the sandboxed process
/// observes. The outer run then asserts none of the poison crossed into the
/// worker's own printed environment. This does not require a live OS
/// sandbox, so it runs on every platform this crate's tests already run on.
#[test]
#[cfg(feature = "test-support")]
fn candidate_sandbox_never_receives_merge_or_release_credentials() {
    if std::env::var_os(INNER_MODE_MARKER).is_some() {
        let root = tempdir().expect("worker root");
        let worker = IsolatedWorker::open(
            root.path(),
            IsolationPolicy::unconfined_for_testing(),
            WorkerDomain::Candidate,
            "/usr/bin/env",
            [],
            WorkerLimits::new(Duration::from_secs(3), 1_000, 4_096).unwrap(),
        )
        .unwrap();
        let output = worker.execute("credential-leak-probe", b"").unwrap();
        assert_eq!(output.completion_reason, CompletionReason::Success);
        let observed = String::from_utf8(output.stdout).expect("worker environment is UTF-8");
        println!("{OUTPUT_MARKER}{observed}{OUTPUT_MARKER}");
        return;
    }

    let exe = std::env::current_exe().expect("this test binary's own path");
    let mut command = std::process::Command::new(exe);
    command.args([
        "candidate_sandbox_never_receives_merge_or_release_credentials",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ]);
    command.env(INNER_MODE_MARKER, "1");
    for (key, value) in CREDENTIAL_POISON {
        command.env(key, value);
    }
    let result = command.output().expect("relaunch this test binary");
    assert!(
        result.status.success(),
        "inner credential probe failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    let observed = stdout
        .split_once(OUTPUT_MARKER)
        .and_then(|(_, rest)| rest.split_once(OUTPUT_MARKER))
        .map(|(marked, _)| marked)
        .expect("inner probe printed its output marker");

    for (key, value) in CREDENTIAL_POISON {
        assert!(
            !observed.contains(key) && !observed.contains(value),
            "candidate sandbox observed a credential-shaped environment variable: {key}"
        );
    }
}
