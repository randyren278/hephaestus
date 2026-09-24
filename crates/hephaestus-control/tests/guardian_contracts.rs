use std::{
    io::{Read, Write},
    os::unix::process::CommandExt as _,
    process::{Command, Stdio},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use hephaestus_runtime::{
    CompletionReason, IsolatedWorker, IsolationPolicy, WorkerDomain, WorkerLimits,
};
use tempfile::tempdir;

const PROCESS_GUARDIAN: &str = env!("CARGO_BIN_EXE_hephaestus-process-guardian");

fn guardian_frame(program: &str, arguments: &[&str], input: &[u8]) -> Vec<u8> {
    let mut frame = serde_json::to_vec(&serde_json::json!({
        "program": program,
        "arguments": arguments,
        "current_dir": "/tmp",
        "home": "/tmp",
        "temp": "/tmp",
        "path": null,
        "input_bytes": input.len(),
    }))
    .expect("encode guardian configuration");
    frame.push(b'\n');
    frame.extend_from_slice(input);
    frame.push(b'\n');
    frame
}

#[test]
fn production_guardian_runs_worker_and_reaps_its_anchor() {
    let mut guardian = Command::new(PROCESS_GUARDIAN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start production guardian");
    guardian
        .stdin
        .take()
        .expect("guardian stdin")
        .write_all(
            &[
                guardian_frame("/bin/cat", &[], "fixture-λ\n".as_bytes()).as_slice(),
                b"continue\n",
            ]
            .concat(),
        )
        .expect("send launch frame");

    let output = guardian.wait_with_output().expect("wait for guardian");

    assert!(
        output.status.success(),
        "status={:?}, stderr={}, stdout={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout),
    );
    assert_eq!(output.stdout, "fixture-λ\n".as_bytes());
}

#[test]
fn production_guardian_reports_worker_spawn_failure() {
    let mut guardian = Command::new(PROCESS_GUARDIAN)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start production guardian");
    guardian
        .stdin
        .take()
        .expect("guardian stdin")
        .write_all(&guardian_frame("/missing/guardian-worker", &[], b""))
        .expect("send launch frame");

    let output = guardian
        .wait_with_output()
        .expect("wait for failed guardian");

    assert!(!output.status.success());
}

#[test]
fn production_anchor_signals_its_isolated_group_after_handshake() {
    let mut anchor = Command::new(PROCESS_GUARDIAN)
        .arg("--hold-worker-group")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start production process-group anchor");
    let mut ready = [0_u8; 1];
    anchor
        .stdout
        .take()
        .expect("anchor readiness pipe")
        .read_exact(&mut ready)
        .expect("read anchor handshake");
    assert_eq!(ready, *b"R");
    drop(anchor.stdin.take());

    let status = anchor.wait().expect("wait for terminated anchor");

    assert!(!status.success(), "the anchor terminates its process group");
}

#[test]
fn production_guardian_reports_unsuccessful_worker_exit() {
    let mut guardian = Command::new(PROCESS_GUARDIAN)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start production guardian");
    guardian
        .stdin
        .take()
        .expect("guardian stdin")
        .write_all(
            &[
                guardian_frame("/usr/bin/false", &[], b"").as_slice(),
                b"continue\n",
            ]
            .concat(),
        )
        .expect("send launch frame");

    let output = guardian
        .wait_with_output()
        .expect("wait for unsuccessful worker");

    assert!(!output.status.success());
}

#[test]
fn production_guardian_cancels_worker_when_control_pipe_closes() {
    let mut guardian = Command::new(PROCESS_GUARDIAN)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start production guardian");
    guardian
        .stdin
        .take()
        .expect("guardian stdin")
        .write_all(&guardian_frame("/bin/sleep", &["30"], b""))
        .expect("send launch frame");

    let output = guardian
        .wait_with_output()
        .expect("wait for cancelled guardian");

    assert!(
        !output.status.success(),
        "cancelled worker must fail the request"
    );
}

#[test]
fn guarded_worker_captures_success_and_rejects_invalid_requests_before_launch() {
    let root = tempdir().expect("worker root");
    let worker = IsolatedWorker::open(
        root.path(),
        IsolationPolicy::unconfined_for_testing(),
        WorkerDomain::Evaluator,
        "/bin/cat",
        [],
        WorkerLimits::new(Duration::from_secs(2), 128, 128).expect("limits"),
    )
    .expect("open worker");

    let output = worker
        .execute_guarded(
            "guarded-e2e",
            "fixture-λ".as_bytes(),
            PROCESS_GUARDIAN,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("guarded worker output");

    assert_eq!(output.completion_reason, CompletionReason::Success);
    assert_eq!(output.stdout, "fixture-λ".as_bytes());
    assert!(matches!(
        worker.execute_guarded(
            "../escape",
            b"",
            PROCESS_GUARDIAN,
            Arc::new(AtomicBool::new(false))
        ),
        Err(hephaestus_runtime::RuntimeError::InvalidSpec(
            "worker id is not path-safe"
        ))
    ));
    assert!(matches!(
        worker.execute_guarded(
            "oversized",
            &[b'x'; 129],
            PROCESS_GUARDIAN,
            Arc::new(AtomicBool::new(false))
        ),
        Err(hephaestus_runtime::RuntimeError::InvalidSpec(
            "worker input exceeds its byte limit"
        ))
    ));
    assert!(
        worker
            .execute_guarded(
                "missing-guardian",
                b"",
                "/missing/guardian-executable",
                Arc::new(AtomicBool::new(false))
            )
            .is_err()
    );
    assert!(
        root.path()
            .read_dir()
            .expect("read worker root")
            .next()
            .is_none()
    );
}
