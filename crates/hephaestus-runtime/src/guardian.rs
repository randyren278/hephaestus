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

use serde::{Deserialize, Serialize};

use crate::RuntimeError;

const MAX_GUARDIAN_CONFIG_BYTES: usize = 65_536;
const MAX_GUARDIAN_INPUT_BYTES: usize = 1_100_000;

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

    let mut command = Command::new(&config.program);
    command
        .args(&config.arguments)
        .current_dir(&config.current_dir)
        .env_clear()
        .env("HOME", &config.home)
        .env("TMPDIR", &config.temp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(path) = &config.path {
        command.env("PATH", path);
    }
    let child = command.spawn()?;
    monitor_worker(child, control, input)
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
    mut control: BufReader<File>,
    input: Vec<u8>,
) -> Result<(), RuntimeError> {
    let child_stdin = child.stdin.take();
    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();
    let (Some(mut child_stdin), Some(mut child_stdout), Some(mut child_stderr)) =
        (child_stdin, child_stdout, child_stderr)
    else {
        terminate_and_reap(&mut child);
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
            kill_process_group(child.id());
            cancellation_sent = true;
        }
        let Ok(observed_status) = child.try_wait() else {
            terminate_and_reap(&mut child);
            return Err(RuntimeError::InvalidSpec(
                "guardian worker status unavailable",
            ));
        };
        if let Some(status) = observed_status {
            // A leader may exit while descendants keep inherited output pipes
            // open. Contain its whole process group before joining relays.
            kill_process_group(child.id());
            break status;
        }
        thread::sleep(Duration::from_millis(5));
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

fn kill_process_group(pid: u32) {
    let process_group = format!("-{pid}");
    let _ignored = Command::new("/bin/kill")
        .args(["-KILL", "--", &process_group])
        .status();
}

fn terminate_and_reap(child: &mut std::process::Child) {
    kill_process_group(child.id());
    let _ignored = child.kill();
    let _ignored = child.wait();
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        os::unix::process::CommandExt as _,
        process::{Command, Stdio},
    };

    use super::{GuardianLaunch, MAX_GUARDIAN_INPUT_BYTES, read_launch, terminate_and_reap};

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
        terminate_and_reap(&mut child);
        assert!(child.try_wait().expect("poll reaped child").is_some());
    }
}
