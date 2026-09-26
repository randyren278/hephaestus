//! The model backend: the user's own subscription CLI (`claude -p` or
//! `codex exec`), run fully isolated from their hooks and settings.

use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// One prompt in, one reply out. Implementations must be safe to call from
/// several senator threads at once.
pub trait Backend: Sync {
    /// Sends `prompt` to the model and returns its final reply text.
    ///
    /// # Errors
    ///
    /// A human-readable reason when the call failed or timed out.
    fn complete(&self, prompt: &str) -> Result<String, String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum BackendKind {
    Claude,
    Codex,
}

impl BackendKind {
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// A subscription CLI invoked once per prompt, with the prompt on stdin.
#[derive(Clone, Debug)]
pub struct CliBackend {
    pub kind: BackendKind,
    pub program: PathBuf,
    pub timeout: Duration,
}

impl CliBackend {
    /// The fixed arguments for one isolated call. The nested CLI must not
    /// load the user's hooks, settings, MCP servers, skills, or rules, or a
    /// Senate running inside a hooked session would recurse. `--bare` is
    /// deliberately absent: it skips the keychain and breaks subscription
    /// auth.
    #[must_use]
    pub fn arguments(&self, last_message: &Path) -> Vec<String> {
        let fixed: &[&str] = match self.kind {
            BackendKind::Claude => &[
                "-p",
                "--setting-sources",
                "",
                "--strict-mcp-config",
                "--tools",
                "",
                "--disable-slash-commands",
                "--no-session-persistence",
            ],
            BackendKind::Codex => &[
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config",
                "--ignore-rules",
                "--sandbox",
                "read-only",
                "--color",
                "never",
                "--output-last-message",
            ],
        };
        let mut arguments: Vec<String> =
            fixed.iter().map(|&argument| argument.to_owned()).collect();
        if self.kind == BackendKind::Codex {
            arguments.push(last_message.display().to_string());
            arguments.push("-".to_owned());
        }
        arguments
    }
}

impl Backend for CliBackend {
    fn complete(&self, prompt: &str) -> Result<String, String> {
        // An empty scratch directory keeps project instructions (CLAUDE.md,
        // AGENTS.md) of whatever checkout the user ran `senate` from out of
        // the senators' context.
        let scratch = tempfile::tempdir().map_err(|error| format!("scratch directory: {error}"))?;
        let last_message = scratch.path().join("last-message.txt");
        let mut child = Command::new(&self.program)
            .args(self.arguments(&last_message))
            .current_dir(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", self.program.display()))?;
        let mut stdin = child.stdin.take().expect("stdin was piped");
        let prompt = prompt.to_owned();
        let writer = thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let stdout = drain(child.stdout.take().expect("stdout was piped"));
        let stderr = drain(child.stderr.take().expect("stderr was piped"));
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ignored = child.kill();
                let _ignored = child.wait();
                return Err(format!(
                    "{} timed out after {}s",
                    self.kind.program(),
                    self.timeout.as_secs()
                ));
            }
            thread::sleep(Duration::from_millis(20));
        };
        // A child that exits without reading its prompt closes the pipe
        // early; its exit status is the error worth reporting.
        let _ignored = writer.join();
        let stdout = stdout.join().unwrap_or_default();
        let stderr = stderr.join().unwrap_or_default();
        if !status.success() {
            let detail = stderr.trim();
            let detail = detail
                .char_indices()
                .nth(400)
                .map_or(detail, |(end, _)| &detail[..end]);
            return Err(format!(
                "{} exited with {status}: {detail}",
                self.kind.program()
            ));
        }
        let reply = match self.kind {
            BackendKind::Claude => stdout,
            BackendKind::Codex => fs::read_to_string(&last_message).unwrap_or(stdout),
        };
        let reply = reply.trim();
        if reply.is_empty() {
            return Err(format!("{} returned an empty reply", self.kind.program()));
        }
        Ok(reply.to_owned())
    }
}

fn drain(mut source: impl Read + Send + 'static) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ignored = source.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// The first executable named `program` on `PATH`.
#[must_use]
pub fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use super::{BackendKind, CliBackend};

    fn backend(kind: BackendKind) -> CliBackend {
        CliBackend {
            kind,
            program: kind.program().into(),
            timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn claude_calls_are_isolated_and_never_bare() {
        let arguments = backend(BackendKind::Claude).arguments(Path::new("unused"));
        for required in [
            "-p",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--no-session-persistence",
        ] {
            assert!(arguments.iter().any(|argument| argument == required));
        }
        for (flag, value) in [("--setting-sources", ""), ("--tools", "")] {
            let at = arguments.iter().position(|argument| argument == flag);
            assert_eq!(
                at.and_then(|at| arguments.get(at + 1)).map(String::as_str),
                Some(value)
            );
        }
        assert!(!arguments.iter().any(|argument| argument == "--bare"));
    }

    #[test]
    fn codex_calls_ignore_user_config_and_read_the_prompt_from_stdin() {
        let arguments = backend(BackendKind::Codex).arguments(Path::new("/tmp/last"));
        assert_eq!(arguments.first().map(String::as_str), Some("exec"));
        for required in ["--ignore-user-config", "--ignore-rules", "--ephemeral"] {
            assert!(arguments.iter().any(|argument| argument == required));
        }
        assert_eq!(arguments.last().map(String::as_str), Some("-"));
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair[0] == "--output-last-message" && pair[1] == "/tmp/last")
        );
    }
}
