use std::path::PathBuf;

use crate::{Provider, RunSpec, RuntimeError, Sandbox};

/// Fixed instruction telling Claude Code to read the task from stdin. Kept short
/// and constant so argv length never depends on the genome prompt size.
const CLAUDE_STDIN_INSTRUCTION: &str = "Complete the task specification piped in on standard input. Do not ask for confirmation; your last message is the final answer.";

/// Immutable, inspectable provider process contract. The prompt is supplied on stdin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderInvocation {
    provider: Provider,
    program: PathBuf,
    arguments: Vec<String>,
    stdin: Vec<u8>,
}

impl ProviderInvocation {
    /// Builds a deterministic non-shell helper invocation for isolation probes.
    ///
    /// # Errors
    ///
    /// Rejects an empty executable path.
    pub fn deterministic(
        executable: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = String>,
        stdin: impl AsRef<[u8]>,
    ) -> Result<Self, RuntimeError> {
        let executable = executable.into();
        validate_executable(&executable)?;
        Ok(Self {
            provider: Provider::Deterministic,
            program: executable,
            arguments: arguments.into_iter().collect(),
            stdin: stdin.as_ref().to_vec(),
        })
    }

    /// Builds the current non-interactive Codex CLI contract without invoking it.
    ///
    /// # Errors
    ///
    /// Rejects an empty executable path or a sandbox that cannot enforce the `RunSpec`.
    pub fn codex(
        executable: impl Into<PathBuf>,
        spec: &RunSpec,
        sandbox: &Sandbox,
    ) -> Result<Self, RuntimeError> {
        let executable = executable.into();
        validate_executable(&executable)?;
        validate_sandbox_authority(spec, sandbox)?;
        let network = spec.capabilities().allows_network();
        Ok(Self {
            provider: Provider::Codex,
            program: executable,
            arguments: vec![
                "exec".to_owned(),
                "--ignore-user-config".to_owned(),
                "--ephemeral".to_owned(),
                "--json".to_owned(),
                "--sandbox".to_owned(),
                if spec.capabilities().allows_workspace_write() {
                    "workspace-write".to_owned()
                } else {
                    "read-only".to_owned()
                },
                "--cd".to_owned(),
                sandbox.worktree().to_string_lossy().into_owned(),
                "--config".to_owned(),
                format!("sandbox_workspace_write.network_access={network}"),
                "-".to_owned(),
            ],
            stdin: spec.prompt().as_bytes().to_vec(),
        })
    }

    /// Builds the current non-interactive Claude Code CLI contract without invoking it.
    ///
    /// Flags follow the documented `claude -p` (print mode) contract: see
    /// `docs/RUNTIMES.md` for the exact sources. The genome prompt is delivered on
    /// stdin (the documented "pipe data through Claude" pattern) rather than as an
    /// argv value, so its size never depends on the platform's argument-length
    /// limit; a short fixed instruction on argv tells Claude to read it there.
    /// Isolation flags (`--setting-sources ""`, `--strict-mcp-config`,
    /// `--disable-slash-commands`) stop a nested Claude Code call from picking up
    /// this operator's hooks, MCP servers, skills, or commands. `--bare` is
    /// deliberately never used: on a subscription login it requires
    /// `ANTHROPIC_API_KEY` and fails closed with "Not logged in".
    ///
    /// # Errors
    ///
    /// Rejects an empty executable path or a sandbox that cannot enforce the `RunSpec`.
    pub fn claude(
        executable: impl Into<PathBuf>,
        spec: &RunSpec,
        sandbox: &Sandbox,
    ) -> Result<Self, RuntimeError> {
        let executable = executable.into();
        validate_executable(&executable)?;
        validate_sandbox_authority(spec, sandbox)?;
        Ok(Self {
            provider: Provider::Claude,
            program: executable,
            arguments: vec![
                "--print".to_owned(),
                CLAUDE_STDIN_INSTRUCTION.to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                "--verbose".to_owned(),
                "--permission-mode".to_owned(),
                "dontAsk".to_owned(),
                "--permission-prompts".to_owned(),
                "none".to_owned(),
                "--setting-sources".to_owned(),
                String::new(),
                "--strict-mcp-config".to_owned(),
                "--disable-slash-commands".to_owned(),
                "--tools".to_owned(),
                if spec.capabilities().allows_workspace_write() {
                    "Read,Edit,Write".to_owned()
                } else {
                    "Read".to_owned()
                },
            ],
            stdin: spec.prompt().as_bytes().to_vec(),
        })
    }

    /// Provider represented by this invocation.
    #[must_use]
    pub const fn provider(&self) -> Provider {
        self.provider
    }

    /// Absolute or PATH-resolved provider executable.
    #[must_use]
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }

    /// Exact non-secret provider arguments.
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Prompt bytes written to child stdin rather than exposed in the process list.
    #[must_use]
    pub fn stdin(&self) -> &[u8] {
        &self.stdin
    }
}

fn validate_executable(path: &std::path::Path) -> Result<(), RuntimeError> {
    if path.as_os_str().is_empty() {
        return Err(RuntimeError::InvalidSpec("provider executable is empty"));
    }
    Ok(())
}

fn validate_sandbox_authority(spec: &RunSpec, sandbox: &Sandbox) -> Result<(), RuntimeError> {
    if sandbox.capabilities() != spec.capabilities() {
        return Err(RuntimeError::CapabilityDenied);
    }
    sandbox.validate_spec_binding(spec)
}
