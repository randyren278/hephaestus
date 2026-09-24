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
    let (config, input, control) = read_launch(control)?;

    let anchor_executable = std::env::current_exe()?;
    let mut anchor_command = Command::new(anchor_executable);
    anchor_command
        .arg("--hold-worker-group")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut anchor = anchor_command.spawn()?;
    let Some(anchor_stdin) = anchor.stdin.take() else {
        terminate_anchor(&mut anchor)?;
        return Err(RuntimeError::InvalidSpec(
            "guardian anchor stdin unavailable",
        ));
    };
    let Some(mut anchor_ready) = anchor.stdout.take() else {
        terminate_anchor(&mut anchor)?;
        return Err(RuntimeError::InvalidSpec(
            "guardian anchor readiness unavailable",
        ));
    };
    let mut readiness = [0_u8; 1];
    if anchor_ready.read_exact(&mut readiness).is_err() || readiness != *b"R" {
        terminate_anchor(&mut anchor)?;
        return Err(RuntimeError::InvalidSpec("guardian anchor failed to start"));
    }
    match anchor_has_exited(anchor.id()) {
        Ok(true) => {
            terminate_anchor(&mut anchor)?;
            return Err(RuntimeError::InvalidSpec("guardian anchor exited early"));
        }
        Ok(false) => {}
        Err(error) => {
            terminate_anchor(&mut anchor)?;
            return Err(error);
        }
    }

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
    let confirmation = match anchor_has_exited(anchor.id()) {
        Ok(confirmation) => confirmation,
        Err(error) => {
            terminate_and_reap(&mut child, &mut anchor)?;
            return Err(error);
        }
    };
    if confirmation {
        terminate_and_reap(&mut child, &mut anchor)?;
        return Err(RuntimeError::InvalidSpec("guardian anchor exited early"));
    }
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
    stdout.write_all(b"R")?;
    stdout.flush()?;
    let mut stdin = io::stdin().lock();
    let mut byte = [0_u8; 1];
    let _ = stdin.read(&mut byte);
    let process_group = process_pid(std::process::id())?;
    match send_group_signal(process_group, Signal::Kill) {
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

fn monitor_worker(
    mut child: std::process::Child,
    mut anchor: std::process::Child,
    anchor_stdin: std::process::ChildStdin,
    mut control: BufReader<File>,
    input: Vec<u8>,
) -> Result<(), RuntimeError> {
    let child_stdin = child.stdin.take();
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();
    let (Some(mut child_stdin), Some(mut child_stdout), Some(mut child_stderr)) =
        (child_stdin, child_stdout, child_stderr)
    else {
        terminate_and_reap(&mut child, &mut anchor)?;
        return Err(RuntimeError::InvalidSpec(
            "guardian worker pipes unavailable",
        ));
    };
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
                return match terminate_and_reap(&mut child, &mut anchor) {
                    Ok(()) => Err(error),
                    Err(containment) => Err(containment),
                };
            }
            cancellation_sent = true;
        }
        match anchor_has_exited(anchor.id()) {
            Ok(true) if !cancellation_sent => {
                terminate_and_reap(&mut child, &mut anchor)?;
                return Err(RuntimeError::InvalidSpec("guardian anchor exited early"));
            }
            Ok(_) => {}
            Err(error) => {
                terminate_and_reap(&mut child, &mut anchor)?;
                return Err(error);
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !cancellation_sent {
                    if let Err(error) = kill_process_group(anchor.id()) {
                        return match terminate_and_reap(&mut child, &mut anchor) {
                            Ok(()) => Err(error),
                            Err(containment) => Err(containment),
                        };
                    }
                }
                drop(anchor_stdin);
                if let Err(error) = anchor.wait() {
                    terminate_and_reap(&mut child, &mut anchor)?;
                    return Err(error.into());
                }
                break status;
            }
            Ok(None) => thread::sleep(Duration::from_millis(5)),
            Err(error) => {
                terminate_and_reap(&mut child, &mut anchor)?;
                return Err(error.into());
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
    let pid = process_pid(pid)?;
    match send_group_signal(pid, Signal::Kill) {
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
        io::Cursor,
        os::unix::process::CommandExt as _,
        process::{Command, Stdio},
        thread,
        time::Duration,
    };

    use super::{
        GuardianLaunch, MAX_GUARDIAN_INPUT_BYTES, anchor_has_exited, kill_process_group,
        read_launch, terminate_anchor,
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
}
