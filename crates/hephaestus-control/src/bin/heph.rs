//! `heph`: the convenience launcher for a fresh Hephaestus install.
//!
//! It starts the local daemon if one is not already serving the configured
//! data directory, waits (bounded) for it to become ready, and then opens the
//! real operator TUI exactly as `hephaestus tui` does — forcing the first-run
//! tour when this data directory has never completed one. `heph` holds no
//! authority of its own: every action it takes is one of the ordinary
//! `hephaestus` subcommands (spawning `hephaestusd`, running
//! `hephaestus init --fixture quickstart`, running `hephaestus tui`), and the
//! daemon it starts still boots frozen exactly as it does launched by hand.
//! An operator who prefers to drive each step themselves can do everything
//! `heph` does with the ordinary CLI.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode, Stdio},
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand};
use hephaestus_control::{Client, Command, data_dir_from_environment};
use serde::{Deserialize, Serialize};

/// Bounded wait for a freshly spawned daemon to start answering requests.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(20);
/// Poll interval while waiting for the daemon socket to come up.
const DAEMON_READY_POLL: Duration = Duration::from_millis(20);

#[derive(Parser)]
#[command(
    name = "heph",
    about = "Launch Hephaestus: start the daemon if needed, then open the operator tour",
    version
)]
struct Arguments {
    /// Canonical daemon data directory (defaults like `hephaestus`).
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Force the first-run tour to play even if it was already completed.
    #[arg(long)]
    tour: bool,
    /// Never start a daemon; fail if one is not already running.
    #[arg(long = "no-daemon")]
    no_daemon: bool,
    #[command(subcommand)]
    command: Option<HephCommand>,
}

#[derive(Subcommand)]
enum HephCommand {
    /// Stop the daemon serving this data directory.
    Stop,
}

/// Durable first-run marker shared with the TUI at `<data_dir>/tui/tour.json`.
#[derive(Debug, Default, Deserialize, Serialize)]
struct TourMarker {
    #[serde(default)]
    completed: bool,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let data_dir = match arguments
        .data_dir
        .clone()
        .map_or_else(data_dir_from_environment, Ok)
    {
        Ok(path) => path,
        Err(error) => {
            eprintln!("heph: {error}");
            return ExitCode::FAILURE;
        }
    };
    if matches!(arguments.command, Some(HephCommand::Stop)) {
        return run_stop(&data_dir);
    }
    run_launch(&data_dir, arguments.tour, arguments.no_daemon)
}

fn run_stop(data_dir: &Path) -> ExitCode {
    match Client::new(data_dir.to_path_buf()).request(Command::DaemonStop) {
        Ok(response) if response.error.is_none() => {
            println!("heph: daemon stopped");
            let _ignored = fs::remove_file(data_dir.join("heph.pid"));
            ExitCode::SUCCESS
        }
        Ok(response) => {
            let error = response.error.expect("checked above");
            eprintln!("heph: {:?}: {}", error.code, error.message);
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("heph: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_launch(data_dir: &Path, force_tour: bool, no_daemon: bool) -> ExitCode {
    if !daemon_reachable(data_dir) {
        if no_daemon {
            eprintln!(
                "heph: no daemon is running for {}; omit --no-daemon to start one automatically",
                data_dir.display()
            );
            return ExitCode::FAILURE;
        }
        if let Err(error) = start_daemon(data_dir) {
            eprintln!("heph: {error}");
            return ExitCode::FAILURE;
        }
        let client = Client::new(data_dir.to_path_buf());
        if !wait_until_ready(DAEMON_READY_TIMEOUT, || {
            client.request(Command::Status).is_ok()
        }) {
            eprintln!(
                "heph: hephaestusd did not become ready within {DAEMON_READY_TIMEOUT:?}; see {}",
                data_dir.join("logs/hephaestusd.log").display()
            );
            return ExitCode::FAILURE;
        }
    }
    let show_tour = force_tour || !tour_marker_completed(data_dir);
    run_tui(data_dir, show_tour)
}

fn daemon_reachable(data_dir: &Path) -> bool {
    Client::new(data_dir.to_path_buf())
        .request(Command::Status)
        .is_ok()
}

/// Polls `probe` until it reports readiness or `timeout` elapses. Kept
/// generic over the probe so the bounded-wait behavior is unit-testable
/// without a real daemon socket.
fn wait_until_ready(timeout: Duration, mut probe: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if probe() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(DAEMON_READY_POLL);
    }
}

fn tour_marker_path(data_dir: &Path) -> PathBuf {
    data_dir.join("tui").join("tour.json")
}

fn tour_marker_completed(data_dir: &Path) -> bool {
    fs::read_to_string(tour_marker_path(data_dir))
        .ok()
        .and_then(|text| serde_json::from_str::<TourMarker>(&text).ok())
        .is_some_and(|marker| marker.completed)
}

fn current_exe_dir() -> Result<PathBuf, String> {
    let exe = env::current_exe()
        .map_err(|error| format!("could not resolve heph's own executable path: {error}"))?;
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "heph executable has no parent directory".to_owned())
}

fn sibling_binary(exe_dir: &Path, name: &str) -> PathBuf {
    exe_dir.join(format!("{name}{}", env::consts::EXE_SUFFIX))
}

fn start_daemon(data_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(data_dir)
        .map_err(|error| format!("could not create data directory: {error}"))?;
    let log_dir = data_dir.join("logs");
    fs::create_dir_all(&log_dir)
        .map_err(|error| format!("could not create log directory: {error}"))?;
    let exe_dir = current_exe_dir()?;
    let hephaestusd = sibling_binary(&exe_dir, "hephaestusd");
    if !hephaestusd.is_file() {
        return Err(format!(
            "hephaestusd was not found beside heph (expected {})",
            hephaestusd.display()
        ));
    }
    let hephaestus_cli = sibling_binary(&exe_dir, "hephaestus");
    let source_repository = resolve_source_repository(data_dir, &hephaestus_cli)?;
    let evaluator = sibling_binary(&exe_dir, "hephaestus-reference-evaluator");

    let log_path = log_dir.join("hephaestusd.log");
    let log_out = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("could not open daemon log {}: {error}", log_path.display()))?;
    let log_err = log_out
        .try_clone()
        .map_err(|error| format!("could not duplicate daemon log handle: {error}"))?;

    let mut command = ProcessCommand::new(&hephaestusd);
    command
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--source-repository")
        .arg(&source_repository);
    if evaluator.is_file() {
        command.arg("--evaluator-executable").arg(&evaluator);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_out))
        .stderr(Stdio::from(log_err));
    // Detach hephaestusd into its own process group so it outlives `heph`
    // (and is not signaled along with it by the shell's job control); this is
    // the strongest detachment available without `unsafe` code, which this
    // workspace forbids.
    detach_process_group(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("could not start hephaestusd: {error}"))?;
    let _ignored = fs::write(data_dir.join("heph.pid"), child.id().to_string());
    Ok(())
}

#[cfg(unix)]
fn detach_process_group(command: &mut ProcessCommand) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn detach_process_group(_command: &mut ProcessCommand) {}

/// Resolves the Git repository the daemon should inventory: an explicit
/// override, a fixture this data directory already bootstrapped, or (failing
/// both) a freshly installed quickstart fixture — through the exact same
/// code path as `hephaestus init --fixture quickstart`.
fn resolve_source_repository(data_dir: &Path, hephaestus_cli: &Path) -> Result<PathBuf, String> {
    if let Some(configured) = env::var_os("HEPHAESTUS_SOURCE_REPOSITORY") {
        return Ok(PathBuf::from(configured));
    }
    let quickstart_repository = data_dir.join("quickstart").join("repository");
    if quickstart_repository.join(".git").is_dir() {
        return Ok(quickstart_repository);
    }
    if !hephaestus_cli.is_file() {
        return Err(format!(
            "hephaestus was not found beside heph (expected {}); cannot bootstrap the quickstart fixture",
            hephaestus_cli.display()
        ));
    }
    let quickstart_dir = data_dir.join("quickstart");
    let status = ProcessCommand::new(hephaestus_cli)
        .arg("init")
        .arg("--fixture")
        .arg("quickstart")
        .arg(&quickstart_dir)
        .status()
        .map_err(|error| format!("could not run `hephaestus init`: {error}"))?;
    if !status.success() {
        return Err("`hephaestus init --fixture quickstart` failed".to_owned());
    }
    Ok(quickstart_dir.join("repository"))
}

fn run_tui(data_dir: &Path, show_tour: bool) -> ExitCode {
    let exe_dir = match current_exe_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("heph: {error}");
            return ExitCode::FAILURE;
        }
    };
    let hephaestus_cli = sibling_binary(&exe_dir, "hephaestus");
    if !hephaestus_cli.is_file() {
        eprintln!(
            "heph: hephaestus was not found beside heph (expected {})",
            hephaestus_cli.display()
        );
        return ExitCode::FAILURE;
    }
    let mut command = ProcessCommand::new(&hephaestus_cli);
    command.arg("--data-dir").arg(data_dir).arg("tui");
    if show_tour {
        command.arg("--tour");
    }
    match command.status() {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(error) => {
            eprintln!("heph: could not start the operator TUI: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use clap::Parser;
    use tempfile::tempdir;

    use super::{
        Arguments, HephCommand, tour_marker_completed, tour_marker_path, wait_until_ready,
    };

    #[test]
    fn default_invocation_starts_the_daemon_and_shows_no_forced_tour() {
        let arguments = Arguments::try_parse_from(["heph"]).expect("heph parses with no flags");
        assert!(!arguments.tour);
        assert!(!arguments.no_daemon);
        assert!(arguments.command.is_none());
    }

    #[test]
    fn tour_and_no_daemon_flags_parse() {
        let arguments =
            Arguments::try_parse_from(["heph", "--tour", "--no-daemon"]).expect("flags parse");
        assert!(arguments.tour);
        assert!(arguments.no_daemon);
    }

    #[test]
    fn stop_subcommand_parses() {
        let arguments = Arguments::try_parse_from(["heph", "stop"]).expect("stop parses");
        assert!(matches!(arguments.command, Some(HephCommand::Stop)));
    }

    #[test]
    fn data_dir_flag_parses_as_a_path() {
        let arguments = Arguments::try_parse_from(["heph", "--data-dir", "/tmp/example"])
            .expect("data-dir parses");
        assert_eq!(
            arguments.data_dir.as_deref(),
            Some(std::path::Path::new("/tmp/example"))
        );
    }

    #[test]
    fn tour_marker_is_incomplete_when_the_file_is_absent() {
        let directory = tempdir().expect("temporary directory");
        assert!(!tour_marker_completed(directory.path()));
    }

    #[test]
    fn tour_marker_is_incomplete_when_the_file_is_malformed() {
        let directory = tempdir().expect("temporary directory");
        std::fs::create_dir_all(tour_marker_path(directory.path()).parent().unwrap())
            .expect("create tui directory");
        std::fs::write(tour_marker_path(directory.path()), b"not json").expect("write marker");
        assert!(!tour_marker_completed(directory.path()));
    }

    #[test]
    fn tour_marker_is_complete_once_the_tui_writes_it() {
        let directory = tempdir().expect("temporary directory");
        let marker_path = tour_marker_path(directory.path());
        std::fs::create_dir_all(marker_path.parent().unwrap()).expect("create tui directory");
        std::fs::write(&marker_path, br#"{"completed":true}"#).expect("write marker");
        assert!(tour_marker_completed(directory.path()));
    }

    #[test]
    fn wait_until_ready_succeeds_once_the_probe_reports_ready() {
        let mut attempts = 0;
        let ready = wait_until_ready(std::time::Duration::from_secs(1), || {
            attempts += 1;
            attempts >= 3
        });
        assert!(ready);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn wait_until_ready_times_out_when_the_probe_never_succeeds() {
        let started = Instant::now();
        let timeout = std::time::Duration::from_millis(80);
        let ready = wait_until_ready(timeout, || false);
        assert!(!ready);
        assert!(started.elapsed() >= timeout);
    }
}
