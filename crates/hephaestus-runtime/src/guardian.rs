use std::{
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use rustix::{
    io::Errno,
    process::{
        Pid, Signal, WaitId, WaitidOptions, kill_process_group as send_group_signal, waitid,
    },
};
use serde::{Deserialize, Serialize};

use crate::RuntimeError;

const MAX_GUARDIAN_CONFIG_BYTES: usize = 65_536;
// Evaluator requests are bounded independently by WorkerLimits (currently
// 16 MiB). Keep one explicit guardian ceiling large enough to carry those
// requests without making the guardian an unbounded stdin relay.
const MAX_GUARDIAN_INPUT_BYTES: usize = 16 * 1_048_576;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GuardianLaunch {
    pub(crate) program: String,
    pub(crate) arguments: Vec<String>,
    pub(crate) current_dir: PathBuf,
    pub(crate) home: PathBuf,
    pub(crate) temp: PathBuf,
    pub(crate) path: Option<String>,
    pub(crate) input_bytes: usize,
}

/// Runs the small parent guardian protocol used by supervised workers.
///
/// The guardian is the worker's parent, owns a separate worker process group,
/// and kills that group when the daemon-held control pipe closes.
///
/// # Errors
///
/// Returns an error for an invalid launch frame, worker spawn failure, or
/// worker output/termination failure.
pub fn run_process_guardian() -> Result<(), RuntimeError> {
    let control = BufReader::new(File::open("/dev/stdin")?);
    run_process_guardian_with(control, std::env::current_exe()?)
}

fn run_process_guardian_with(
    control: BufReader<File>,
    anchor_executable: PathBuf,
) -> Result<(), RuntimeError> {
    let (config, input, control) = read_launch(control)?;

    let mut anchor_command = Command::new(anchor_executable);
    anchor_command
        .arg("--hold-worker-group")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut anchor = anchor_command.spawn()?;
    let (anchor_stdin, mut anchor_ready) = take_anchor_pipes(&mut anchor)?;
    let mut readiness = [0_u8; 1];
    if anchor_ready.read_exact(&mut readiness).is_err() || readiness != *b"R" {
        terminate_anchor(&mut anchor)?;
        return Err(RuntimeError::InvalidSpec("guardian anchor failed to start"));
    }
    let anchor_state = anchor_has_exited(anchor.id());
    ensure_anchor_state(&mut anchor, None, anchor_state)?;

    let mut command = Command::new(&config.program);
    command
        .args(&config.arguments)
        .current_dir(&config.current_dir)
        .env_clear()
        .env("HOME", &config.home)
        .env("TMPDIR", &config.temp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(
        i32::try_from(anchor.id())
            .map_err(|_| RuntimeError::InvalidSpec("guardian anchor id is invalid"))?,
    );
    if let Some(path) = &config.path {
        command.env("PATH", path);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            terminate_anchor(&mut anchor)?;
            return Err(error.into());
        }
    };
    let anchor_state = anchor_has_exited(anchor.id());
    ensure_anchor_state(&mut anchor, Some(&mut child), anchor_state)?;
    drop(anchor_ready);
    monitor_worker(child, anchor, anchor_stdin, control, input)
}

/// Holds an isolated worker process group open until its guardian signals it.
///
/// # Errors
///
/// Returns an error if the parent cannot establish the anchor handshake.
pub fn hold_process_group_anchor() -> Result<(), RuntimeError> {
    let mut stdout = io::stdout().lock();
    let mut stdin = io::stdin().lock();
    let process_group = process_pid(std::process::id())?;
    hold_anchor_with(&mut stdout, &mut stdin, process_group, |group| {
        send_group_signal(group, Signal::Kill)
    })
}

fn hold_anchor_with<W, R, F>(
    stdout: &mut W,
    stdin: &mut R,
    process_group: Pid,
    signal_group: F,
) -> Result<(), RuntimeError>
where
    W: Write,
    R: Read,
    F: FnOnce(Pid) -> Result<(), Errno>,
{
    stdout.write_all(b"R")?;
    stdout.flush()?;
    let mut byte = [0_u8; 1];
    let _ = stdin.read(&mut byte);
    match signal_group(process_group) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(io::Error::from(error).into()),
    }
}

fn read_launch<R: BufRead + Read>(
    mut control: R,
) -> Result<(GuardianLaunch, Vec<u8>, R), RuntimeError> {
    let mut line = Vec::new();
    (&mut control)
        .take(u64::try_from(MAX_GUARDIAN_CONFIG_BYTES + 1).unwrap_or(u64::MAX))
        .read_until(b'\n', &mut line)?;
    if line.len() > MAX_GUARDIAN_CONFIG_BYTES || !line.ends_with(b"\n") {
        return Err(RuntimeError::InvalidSpec(
            "guardian configuration is invalid",
        ));
    }
    let config: GuardianLaunch = serde_json::from_slice(&line[..line.len() - 1])
        .map_err(|_| RuntimeError::InvalidSpec("guardian configuration is invalid"))?;
    if config.program.is_empty()
        || config.arguments.len() > 128
        || config.input_bytes > MAX_GUARDIAN_INPUT_BYTES
    {
        return Err(RuntimeError::InvalidSpec("guardian launch exceeds limits"));
    }
    let mut input = vec![0_u8; config.input_bytes];
    control.read_exact(&mut input)?;
    let mut separator = [0_u8; 1];
    control.read_exact(&mut separator)?;
    if separator != *b"\n" {
        return Err(RuntimeError::InvalidSpec("guardian framing is invalid"));
    }

    Ok((config, input, control))
}

fn take_anchor_pipes(
    anchor: &mut std::process::Child,
) -> Result<(std::process::ChildStdin, std::process::ChildStdout), RuntimeError> {
    let Some(anchor_stdin) = anchor.stdin.take() else {
        terminate_anchor(anchor)?;
        return Err(RuntimeError::InvalidSpec(
            "guardian anchor stdin unavailable",
        ));
    };
    let Some(anchor_ready) = anchor.stdout.take() else {
        terminate_anchor(anchor)?;
        return Err(RuntimeError::InvalidSpec(
            "guardian anchor readiness unavailable",
        ));
    };
    Ok((anchor_stdin, anchor_ready))
}

fn ensure_anchor_state(
    anchor: &mut std::process::Child,
    worker: Option<&mut std::process::Child>,
    state: Result<bool, RuntimeError>,
) -> Result<(), RuntimeError> {
    match state {
        Ok(false) => Ok(()),
        Ok(true) => {
            terminate_anchor_and_worker(anchor, worker)?;
            Err(RuntimeError::InvalidSpec("guardian anchor exited early"))
        }
        Err(error) => {
            terminate_anchor_and_worker(anchor, worker)?;
            Err(error)
        }
    }
}

fn terminate_anchor_and_worker(
    anchor: &mut std::process::Child,
    worker: Option<&mut std::process::Child>,
) -> Result<(), RuntimeError> {
    if let Some(worker) = worker {
        terminate_and_reap(worker, anchor)
    } else {
        terminate_anchor(anchor)
    }
}

fn take_worker_pipes(
    worker: &mut std::process::Child,
    anchor: &mut std::process::Child,
) -> Result<
    (
        std::process::ChildStdin,
        std::process::ChildStdout,
        std::process::ChildStderr,
    ),
    RuntimeError,
> {
    let stdin = worker.stdin.take();
    let stdout = worker.stdout.take();
    let stderr = worker.stderr.take();
    let (Some(stdin), Some(stdout), Some(stderr)) = (stdin, stdout, stderr) else {
        terminate_and_reap(worker, anchor)?;
        return Err(RuntimeError::InvalidSpec(
            "guardian worker pipes unavailable",
        ));
    };
    Ok((stdin, stdout, stderr))
}

fn observe_anchor_state(
    worker: &mut std::process::Child,
    anchor: &mut std::process::Child,
    state: Result<bool, RuntimeError>,
    cancellation_sent: bool,
) -> Result<(), RuntimeError> {
    match state {
        Ok(true) if !cancellation_sent => {
            terminate_and_reap(worker, anchor)?;
            Err(RuntimeError::InvalidSpec("guardian anchor exited early"))
        }
        Ok(_) => Ok(()),
        Err(error) => {
            terminate_and_reap(worker, anchor)?;
            Err(error)
        }
    }
}

fn fail_after_group_signal(
    worker: &mut std::process::Child,
    anchor: &mut std::process::Child,
    error: RuntimeError,
) -> Result<(), RuntimeError> {
    match terminate_and_reap(worker, anchor) {
        Ok(()) => Err(error),
        Err(containment) => Err(containment),
    }
}

fn wait_for_anchor(
    worker: &mut std::process::Child,
    anchor: &mut std::process::Child,
) -> Result<(), RuntimeError> {
    let result = anchor.wait();
    wait_for_anchor_result(worker, anchor, result)
}

fn wait_for_anchor_result(
    worker: &mut std::process::Child,
    anchor: &mut std::process::Child,
    result: io::Result<std::process::ExitStatus>,
) -> Result<(), RuntimeError> {
    match result {
        Ok(_) => Ok(()),
        Err(error) => {
            terminate_and_reap(worker, anchor)?;
            Err(error.into())
        }
    }
}

fn fail_after_wait_error(
    worker: &mut std::process::Child,
    anchor: &mut std::process::Child,
    error: RuntimeError,
) -> Result<(), RuntimeError> {
    terminate_and_reap(worker, anchor)?;
    Err(error)
}

fn monitor_worker(
    mut child: std::process::Child,
    mut anchor: std::process::Child,
    anchor_stdin: std::process::ChildStdin,
    mut control: BufReader<File>,
    input: Vec<u8>,
) -> Result<(), RuntimeError> {
    let (mut child_stdin, mut child_stdout, mut child_stderr) =
        take_worker_pipes(&mut child, &mut anchor)?;
    let (cancel, cancelled) = mpsc::channel();
    thread::spawn(move || {
        let mut line = Vec::new();
        let cancelled = match control.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => true,
            Ok(_) => line == b"cancel\n",
        };
        let _ignored = cancel.send(cancelled);
    });
    let input_writer = thread::spawn(move || child_stdin.write_all(&input));
    let stdout = thread::spawn(move || io::copy(&mut child_stdout, &mut io::stdout().lock()));
    let stderr = thread::spawn(move || io::copy(&mut child_stderr, &mut io::stderr().lock()));

    let mut cancellation_sent = false;
    let status = loop {
        if !cancellation_sent
            && matches!(
                cancelled.try_recv(),
                Ok(true) | Err(mpsc::TryRecvError::Disconnected)
            )
        {
            if let Err(error) = kill_process_group(anchor.id()) {
                return fail_after_group_signal(&mut child, &mut anchor, error);
            }
            cancellation_sent = true;
        }
        let anchor_state = anchor_has_exited(anchor.id());
        observe_anchor_state(&mut child, &mut anchor, anchor_state, cancellation_sent)?;
        match child.try_wait() {
            Ok(Some(status)) => {
                if !cancellation_sent {
                    if let Err(error) = kill_process_group(anchor.id()) {
                        return fail_after_group_signal(&mut child, &mut anchor, error);
                    }
                }
                drop(anchor_stdin);
                wait_for_anchor(&mut child, &mut anchor)?;
                break status;
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                return fail_after_wait_error(&mut child, &mut anchor, error.into());
            }
        }
    };
    stdout
        .join()
        .map_err(|_| RuntimeError::InvalidSpec("guardian stdout reader failed"))??;
    stderr
        .join()
        .map_err(|_| RuntimeError::InvalidSpec("guardian stderr reader failed"))??;
    let input_result = input_writer
        .join()
        .map_err(|_| RuntimeError::InvalidSpec("guardian stdin writer failed"))?;
    if !status.success() || input_result.is_err() {
        return Err(RuntimeError::InvalidSpec(
            "guardian worker exited unsuccessfully",
        ));
    }
    Ok(())
}

fn process_pid(pid: u32) -> Result<Pid, RuntimeError> {
    let pid = i32::try_from(pid)
        .map_err(|_| RuntimeError::InvalidSpec("guardian process id is invalid"))?;
    Pid::from_raw(pid).ok_or(RuntimeError::InvalidSpec("guardian process id is invalid"))
}

fn anchor_has_exited(pid: u32) -> Result<bool, RuntimeError> {
    let status = waitid(
        WaitId::Pid(process_pid(pid)?),
        WaitidOptions::EXITED | WaitidOptions::NOHANG | WaitidOptions::NOWAIT,
    )
    .map_err(io::Error::from)?;
    Ok(status.is_some())
}

fn kill_process_group(pid: u32) -> Result<(), RuntimeError> {
    signal_process_group(pid, |group| send_group_signal(group, Signal::Kill))
}

fn signal_process_group<F>(pid: u32, signal_group: F) -> Result<(), RuntimeError>
where
    F: FnOnce(Pid) -> Result<(), Errno>,
{
    let pid = process_pid(pid)?;
    match signal_group(pid) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(io::Error::from(error).into()),
    }
}

fn terminate_anchor(anchor: &mut std::process::Child) -> Result<(), RuntimeError> {
    let _ignored = anchor.kill();
    anchor.wait()?;
    Ok(())
}

fn terminate_and_reap(
    child: &mut std::process::Child,
    anchor: &mut std::process::Child,
) -> Result<(), RuntimeError> {
    let group_termination = kill_process_group(anchor.id());
    terminate_and_reap_with(child, anchor, group_termination)
}

fn terminate_and_reap_with(
    child: &mut std::process::Child,
    anchor: &mut std::process::Child,
    group_termination: Result<(), RuntimeError>,
) -> Result<(), RuntimeError> {
    let _ignored = child.kill();
    let _ignored = child.wait();
    let _ignored = anchor.kill();
    let _ignored = anchor.wait();
    group_termination.map_err(|error| RuntimeError::ContainmentFailed {
        evidence: "guardian process group termination failed".to_owned(),
        interrupt: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::{BufReader, Cursor, Error as IoError, Seek as _, Write as _},
        os::fd::OwnedFd,
        os::unix::{fs::PermissionsExt as _, net::UnixStream, process::CommandExt as _},
        path::PathBuf,
        process::{Command, Stdio},
        thread,
        time::Duration,
    };

    use rustix::io::Errno;

    use super::{
        GuardianLaunch, MAX_GUARDIAN_INPUT_BYTES, RuntimeError, anchor_has_exited,
        ensure_anchor_state, fail_after_group_signal, fail_after_wait_error, hold_anchor_with,
        kill_process_group, observe_anchor_state, process_pid, read_launch,
        run_process_guardian_with, signal_process_group, take_anchor_pipes, take_worker_pipes,
        terminate_anchor, terminate_and_reap, terminate_and_reap_with, wait_for_anchor_result,
    };

    fn launch(input_bytes: usize) -> GuardianLaunch {
        GuardianLaunch {
            program: "/bin/cat".to_owned(),
            arguments: Vec::new(),
            current_dir: "/tmp".into(),
            home: "/tmp".into(),
            temp: "/tmp".into(),
            path: None,
            input_bytes,
        }
    }

    fn frame(config: &GuardianLaunch, input: &[u8], separator: u8) -> Vec<u8> {
        let mut bytes = serde_json::to_vec(config).expect("encode config");
        bytes.push(b'\n');
        bytes.extend_from_slice(input);
        bytes.push(separator);
        bytes
    }

    fn control_frame(program: &str, arguments: &[&str], input: &[u8]) -> BufReader<File> {
        let config = GuardianLaunch {
            program: program.to_owned(),
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            current_dir: "/tmp".into(),
            home: "/tmp".into(),
            temp: "/tmp".into(),
            path: None,
            input_bytes: input.len(),
        };
        let mut bytes = frame(&config, input, b'\n');
        bytes.extend_from_slice(b"continue\n");
        let mut file = tempfile::tempfile().expect("create control frame");
        file.write_all(&bytes).expect("write control frame");
        file.rewind().expect("rewind control frame");
        BufReader::new(file)
    }

    fn open_control_frame(
        program: &str,
        arguments: &[&str],
        input: &[u8],
    ) -> (BufReader<File>, UnixStream) {
        let config = GuardianLaunch {
            program: program.to_owned(),
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
            current_dir: "/tmp".into(),
            home: "/tmp".into(),
            temp: "/tmp".into(),
            path: None,
            input_bytes: input.len(),
        };
        let bytes = frame(&config, input, b'\n');
        let (reader, mut writer) = UnixStream::pair().expect("create control pipe");
        writer.write_all(&bytes).expect("write control frame");
        (BufReader::new(File::from(OwnedFd::from(reader))), writer)
    }

    fn anchor_script(directory: &std::path::Path, script: &str) -> PathBuf {
        let path = directory.join("anchor.sh");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).expect("write anchor script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("make anchor executable");
        path
    }

    fn group_anchor() -> std::process::Child {
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start test anchor")
    }

    fn grouped_worker(anchor: &std::process::Child) -> std::process::Child {
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(i32::try_from(anchor.id()).expect("anchor pid fits pgid"))
            .spawn()
            .expect("start grouped worker")
    }

    #[test]
    fn launch_parser_rejects_unterminated_invalid_oversized_and_misframed_inputs() {
        assert!(read_launch(Cursor::new(b"{}".to_vec())).is_err());
        assert!(read_launch(Cursor::new(b"not-json\n".to_vec())).is_err());
        let mut oversized = launch(0);
        oversized.program.clear();
        assert!(read_launch(Cursor::new(frame(&oversized, b"", b'\n'))).is_err());
        let too_much_input = launch(MAX_GUARDIAN_INPUT_BYTES + 1);
        assert!(read_launch(Cursor::new(frame(&too_much_input, b"", b'\n'))).is_err());
        let bad_separator = launch(1);
        assert!(read_launch(Cursor::new(frame(&bad_separator, b"x", b'!'))).is_err());
        let valid = launch(3);
        assert_eq!(
            read_launch(Cursor::new(frame(&valid, b"abc", b'\n')))
                .expect("valid frame")
                .1,
            b"abc"
        );
    }

    #[test]
    fn termination_helper_kills_and_reaps_an_isolated_group() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start isolated child");
        terminate_anchor(&mut child).expect("terminate isolated group");
        assert!(child.try_wait().expect("poll reaped child").is_some());
    }

    #[test]
    fn process_group_anchor_keeps_finished_workers_signalable() {
        let mut anchor = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start process-group anchor");
        let mut worker = Command::new("/usr/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(i32::try_from(anchor.id()).expect("anchor pid fits pgid"))
            .spawn()
            .expect("start isolated worker");
        assert!(worker.wait().expect("reap isolated worker").success());
        kill_process_group(anchor.id()).expect("signal anchored process group");
        assert!(!anchor.wait().expect("reap process-group anchor").success());
    }

    #[test]
    fn guardian_kills_descendants_when_worker_leader_exits() {
        let directory = tempfile::tempdir().expect("worker directory");
        let descendant_ready = directory.path().join("descendant-ready");
        let descendant_marker = directory.path().join("descendant-survived");
        let descendant_ready = descendant_ready.display().to_string();
        let descendant_marker = descendant_marker.display().to_string();
        let worker_script = concat!(
            "(printf ready > \"$1\"; /bin/sleep 0.5; printf leaked > \"$2\") & ",
            "attempts=0; ",
            "while [ ! -f \"$1\" ] && [ \"$attempts\" -lt 200 ]; do ",
            "/bin/sleep 0.01; attempts=$((attempts + 1)); done; ",
            "[ -f \"$1\" ] || exit 7; exit 0"
        );
        let (control, _keep_control_open) = open_control_frame(
            "/bin/sh",
            &[
                "-c",
                worker_script,
                "guardian-worker",
                descendant_ready.as_str(),
                descendant_marker.as_str(),
            ],
            b"",
        );
        let result = run_process_guardian_with(
            control,
            anchor_script(directory.path(), "printf R; exec /bin/cat"),
        );

        assert!(result.is_ok(), "worker guardian failed: {result:?}");
        assert!(directory.path().join("descendant-ready").is_file());
        // The leader exits only after its descendant has started and entered a
        // delayed write. This grace period lets a leaked descendant finish.
        thread::sleep(Duration::from_millis(650));
        assert!(
            !directory.path().join("descendant-survived").exists(),
            "worker descendant survived its leader and escaped process-group containment"
        );
    }

    #[test]
    fn anchor_exit_observation_keeps_the_pid_pinned_until_wait() {
        let mut anchor = Command::new("/usr/bin/true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start short-lived anchor");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !anchor_has_exited(anchor.id()).expect("observe anchor with WNOWAIT") {
            assert!(std::time::Instant::now() < deadline, "anchor did not exit");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(anchor.wait().expect("reap observed anchor").success());
    }

    #[test]
    fn terminate_and_reap_kills_worker_group_and_reaps_both_children() {
        let mut anchor = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("start process-group anchor");
        let mut worker = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(i32::try_from(anchor.id()).expect("anchor pid fits pgid"))
            .spawn()
            .expect("start grouped worker");

        terminate_and_reap(&mut worker, &mut anchor).expect("terminate group and reap children");

        assert!(worker.try_wait().expect("poll reaped worker").is_some());
        assert!(anchor.try_wait().expect("poll reaped anchor").is_some());
    }

    #[test]
    fn missing_anchor_pipes_terminate_and_reap_the_anchor() {
        let mut no_stdin = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start anchor without stdin");
        let error = take_anchor_pipes(&mut no_stdin).expect_err("missing anchor stdin");
        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("guardian anchor stdin unavailable")
        ));
        assert!(no_stdin.try_wait().expect("reaped anchor").is_some());

        let mut no_stdout = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start anchor without readiness pipe");
        let error = take_anchor_pipes(&mut no_stdout).expect_err("missing anchor stdout");
        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("guardian anchor readiness unavailable")
        ));
        assert!(no_stdout.try_wait().expect("reaped anchor").is_some());
    }

    #[test]
    fn missing_worker_pipes_terminate_and_reap_both_processes() {
        let mut anchor = group_anchor();
        let mut worker = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(i32::try_from(anchor.id()).expect("anchor pid fits pgid"))
            .spawn()
            .expect("start worker without pipes");

        let error = take_worker_pipes(&mut worker, &mut anchor).expect_err("missing worker pipes");

        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("guardian worker pipes unavailable")
        ));
        assert!(worker.try_wait().expect("reaped worker").is_some());
        assert!(anchor.try_wait().expect("reaped anchor").is_some());
    }

    #[test]
    fn anchor_state_failures_clean_up_anchor_and_worker() {
        let mut anchor = group_anchor();
        let error = ensure_anchor_state(
            &mut anchor,
            None,
            Err(RuntimeError::InvalidSpec(
                "injected anchor observation failure",
            )),
        )
        .expect_err("propagate anchor observation error");
        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("injected anchor observation failure")
        ));
        assert!(anchor.try_wait().expect("reaped anchor").is_some());

        let mut anchor = group_anchor();
        let mut worker = grouped_worker(&anchor);
        let error = ensure_anchor_state(&mut anchor, Some(&mut worker), Ok(true))
            .expect_err("reject exited anchor");
        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("guardian anchor exited early")
        ));
        assert!(worker.try_wait().expect("reaped worker").is_some());
        assert!(anchor.try_wait().expect("reaped anchor").is_some());
    }

    #[test]
    fn monitor_anchor_observation_error_cleans_up_worker_group() {
        let mut anchor = group_anchor();
        let mut worker = grouped_worker(&anchor);

        let error = observe_anchor_state(
            &mut worker,
            &mut anchor,
            Err(RuntimeError::InvalidSpec(
                "injected monitor observation failure",
            )),
            false,
        )
        .expect_err("propagate monitor observation error");

        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("injected monitor observation failure")
        ));
        assert!(worker.try_wait().expect("reaped worker").is_some());
        assert!(anchor.try_wait().expect("reaped anchor").is_some());
    }

    #[test]
    fn process_group_signal_failure_is_reported_after_containment_cleanup() {
        assert!(matches!(
            signal_process_group(std::process::id(), |_| Err(Errno::PERM)),
            Err(RuntimeError::Io(_))
        ));

        let mut anchor = group_anchor();
        let mut worker = grouped_worker(&anchor);
        let error = fail_after_group_signal(
            &mut worker,
            &mut anchor,
            RuntimeError::Io(IoError::other("injected group signal failure")),
        )
        .expect_err("propagate signal error after cleanup");
        assert!(matches!(error, RuntimeError::Io(_)));
        assert!(worker.try_wait().expect("reaped worker").is_some());
        assert!(anchor.try_wait().expect("reaped anchor").is_some());
    }

    #[test]
    fn wait_failures_reap_worker_and_anchor_and_report_containment_errors() {
        let mut anchor = group_anchor();
        let mut worker = grouped_worker(&anchor);
        let error = wait_for_anchor_result(
            &mut worker,
            &mut anchor,
            Err(IoError::other("injected anchor wait failure")),
        )
        .expect_err("propagate anchor wait failure");
        assert!(matches!(error, RuntimeError::Io(_)));
        assert!(worker.try_wait().expect("reaped worker").is_some());
        assert!(anchor.try_wait().expect("reaped anchor").is_some());

        let mut anchor = group_anchor();
        let mut worker = grouped_worker(&anchor);
        let error = fail_after_wait_error(
            &mut worker,
            &mut anchor,
            RuntimeError::Io(IoError::other("injected worker wait failure")),
        )
        .expect_err("propagate worker wait failure");
        assert!(matches!(error, RuntimeError::Io(_)));
        assert!(worker.try_wait().expect("reaped worker").is_some());
        assert!(anchor.try_wait().expect("reaped anchor").is_some());

        let mut anchor = group_anchor();
        let mut worker = grouped_worker(&anchor);
        let error = terminate_and_reap_with(
            &mut worker,
            &mut anchor,
            Err(RuntimeError::Io(IoError::other(
                "injected containment signal failure",
            ))),
        )
        .expect_err("report containment failure after reaping");
        assert!(matches!(error, RuntimeError::ContainmentFailed { .. }));
        assert!(worker.try_wait().expect("reaped worker").is_some());
        assert!(anchor.try_wait().expect("reaped anchor").is_some());
    }

    #[test]
    fn guardian_reaps_anchor_that_fails_the_readiness_handshake() {
        let directory = tempfile::tempdir().expect("anchor directory");
        let error = run_process_guardian_with(
            control_frame("/bin/cat", &[], b""),
            anchor_script(directory.path(), "exit 1"),
        )
        .expect_err("failed anchor handshake");

        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("guardian anchor failed to start")
        ));
    }

    #[test]
    fn guardian_reaps_anchor_when_worker_spawn_fails() {
        let directory = tempfile::tempdir().expect("anchor directory");
        let error = run_process_guardian_with(
            control_frame("/missing/worker", &[], b""),
            anchor_script(directory.path(), "printf R; exec /bin/sleep 30"),
        )
        .expect_err("missing worker executable");

        assert!(matches!(error, RuntimeError::Io(_)));
    }

    #[test]
    fn guardian_rejects_worker_that_closes_stdin_before_receiving_the_frame() {
        let directory = tempfile::tempdir().expect("anchor directory");
        let error = run_process_guardian_with(
            control_frame("/usr/bin/true", &[], &vec![b'x'; 1_048_576]),
            anchor_script(directory.path(), "printf R; exec /bin/sleep 30"),
        )
        .expect_err("worker did not receive its full request");

        assert!(matches!(
            error,
            RuntimeError::InvalidSpec("guardian worker exited unsuccessfully")
        ));
    }

    #[test]
    fn guardian_terminates_worker_when_anchor_exits_after_startup() {
        let directory = tempfile::tempdir().expect("anchor directory");
        let (control, _keep_control_open) = open_control_frame("/bin/sleep", &["30"], b"");
        let error = run_process_guardian_with(
            control,
            anchor_script(directory.path(), "printf R; /bin/sleep 0.1"),
        )
        .expect_err("anchor exited before its worker");

        assert!(
            matches!(
                error,
                RuntimeError::InvalidSpec("guardian anchor exited early")
            ),
            "unexpected guardian error: {error:?}"
        );
    }

    #[test]
    fn anchor_handshake_signals_only_after_readiness_and_control_eof() {
        let process_group = process_pid(std::process::id()).expect("current pid");
        let mut stdout = Vec::new();
        let mut stdin = Cursor::new(Vec::<u8>::new());
        let mut signaled = None;

        hold_anchor_with(&mut stdout, &mut stdin, process_group, |group| {
            signaled = Some(group);
            Ok(())
        })
        .expect("complete anchor handshake");

        assert_eq!(stdout, b"R");
        assert_eq!(signaled, Some(process_group));
    }

    #[test]
    fn anchor_handshake_propagates_group_signal_failure() {
        let process_group = process_pid(std::process::id()).expect("current pid");
        let mut stdout = Vec::new();
        let mut stdin = Cursor::new(Vec::<u8>::new());

        let error = hold_anchor_with(&mut stdout, &mut stdin, process_group, |_| Err(Errno::PERM))
            .expect_err("group signal failure");

        assert!(matches!(error, RuntimeError::Io(_)));
        assert_eq!(stdout, b"R");
    }
}
