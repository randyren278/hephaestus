use std::{fmt::Write as _, path::PathBuf, process::Command};

use crate::{ProviderInvocation, RuntimeError, Sandbox};

/// External operating-system isolation backend selected without downgrade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IsolationBackend {
    /// macOS Seatbelt through the system `sandbox-exec` launcher.
    MacOsSeatbelt,
    /// No verified external sandbox exists on this host.
    Unavailable,
    /// Unit-test-only process launcher used to exercise supervisor mechanics.
    #[cfg(any(test, feature = "test-support"))]
    TestOnlyUnconfined,
}

/// Immutable external filesystem and network isolation policy.
pub struct IsolationPolicy {
    backend: IsolationBackend,
    protected_paths: Vec<PathBuf>,
}

impl IsolationPolicy {
    /// Detects a supported OS sandbox and records canonical paths workers cannot access.
    #[must_use]
    pub fn detect(protected_paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let backend = detected_backend(
            cfg!(target_os = "macos"),
            std::path::Path::new("/usr/bin/sandbox-exec").is_file(),
        );
        Self {
            backend,
            protected_paths: protected_paths.into_iter().collect(),
        }
    }

    /// Reports the selected backend.
    #[must_use]
    pub const fn backend(&self) -> IsolationBackend {
        self.backend
    }

    /// Builds an externally sandboxed provider process without starting it.
    ///
    /// # Errors
    ///
    /// Fails closed when no verified backend exists or a policy path is not UTF-8.
    pub fn command(
        &self,
        invocation: &ProviderInvocation,
        sandbox: &Sandbox,
    ) -> Result<Command, RuntimeError> {
        #[cfg(any(test, feature = "test-support"))]
        if self.backend == IsolationBackend::TestOnlyUnconfined {
            let mut command = Command::new(invocation.program());
            command.args(invocation.arguments());
            command.current_dir(sandbox.worktree());
            return Ok(command);
        }
        let mut command = Command::new(self.launcher()?);
        let profile = self.macos_profile(invocation, sandbox)?;
        command.args(["-p", &profile]);
        command.arg(invocation.program());
        command.args(invocation.arguments());
        command.current_dir(sandbox.worktree());
        Ok(command)
    }

    pub(crate) fn worker_command(
        &self,
        invocation: &ProviderInvocation,
        worker_root: &std::path::Path,
    ) -> Result<Command, RuntimeError> {
        #[cfg(any(test, feature = "test-support"))]
        if self.backend == IsolationBackend::TestOnlyUnconfined {
            let mut command = Command::new(invocation.program());
            command.args(invocation.arguments());
            command.current_dir(worker_root);
            return Ok(command);
        }
        let mut command = Command::new(self.launcher()?);
        let profile = build_macos_profile(
            invocation.program(),
            worker_root,
            &self.protected_paths,
            false,
            None,
        )?;
        command.args(["-p", &profile]);
        command.arg(invocation.program());
        command.args(invocation.arguments());
        command.current_dir(worker_root);
        Ok(command)
    }

    fn launcher(&self) -> Result<&'static str, RuntimeError> {
        match self.backend {
            IsolationBackend::MacOsSeatbelt => Ok("/usr/bin/sandbox-exec"),
            IsolationBackend::Unavailable => Err(RuntimeError::Unsupported(
                "no verified external sandbox backend",
            )),
            #[cfg(any(test, feature = "test-support"))]
            IsolationBackend::TestOnlyUnconfined => Err(RuntimeError::Unsupported(
                "test-only backend has no production launcher",
            )),
        }
    }

    /// Creates an unconfined process policy for contract tests only.
    ///
    /// This API does not exist unless the explicitly unsafe `test-support`
    /// feature is enabled; production callers must use [`Self::detect`].
    #[cfg(feature = "test-support")]
    #[must_use]
    pub const fn unconfined_for_testing() -> Self {
        Self {
            backend: IsolationBackend::TestOnlyUnconfined,
            protected_paths: Vec::new(),
        }
    }

    #[cfg(all(test, not(feature = "test-support")))]
    pub(crate) const fn unconfined_for_testing() -> Self {
        Self {
            backend: IsolationBackend::TestOnlyUnconfined,
            protected_paths: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) const fn unavailable_for_testing() -> Self {
        Self {
            backend: IsolationBackend::Unavailable,
            protected_paths: Vec::new(),
        }
    }

    fn macos_profile(
        &self,
        invocation: &ProviderInvocation,
        sandbox: &Sandbox,
    ) -> Result<String, RuntimeError> {
        build_macos_profile(
            invocation.program(),
            sandbox.run_root(),
            &self.protected_paths,
            sandbox.capabilities().allows_network(),
            (!sandbox.capabilities().allows_workspace_write()).then(|| sandbox.worktree()),
        )
    }
}

const fn detected_backend(is_macos: bool, seatbelt_exists: bool) -> IsolationBackend {
    if is_macos && seatbelt_exists {
        IsolationBackend::MacOsSeatbelt
    } else {
        IsolationBackend::Unavailable
    }
}

fn build_macos_profile(
    executable: &std::path::Path,
    run_root: &std::path::Path,
    protected_paths: &[PathBuf],
    network: bool,
    read_only_worktree: Option<&std::path::Path>,
) -> Result<String, RuntimeError> {
    let run_root = quote_path(run_root)?;
    let executable = quote_path(executable)?;
    let mut profile = format!(
        "(version 1)\n(deny default)\n(import \"system.sb\")\n(allow process*)\n(allow sysctl-read)\n(allow mach-lookup)\n(allow file-read-metadata)\n(allow file-read* (subpath \"/System\") (subpath \"/usr\") (subpath \"/bin\") (literal {executable}) (subpath {run_root}))\n(allow file-write* (subpath {run_root}) (literal \"/dev/null\"))\n"
    );
    for path in protected_paths {
        writeln!(
            &mut profile,
            "(deny file-read* file-write* (subpath {}))",
            quote_path(path)?
        )
        .expect("writing to a String cannot fail");
    }
    // Without workspace-write authority the worktree is read-only at the OS
    // layer. Hosted CLIs cannot nest their own sandbox inside this one, so
    // this profile is the only place that authority is enforced for them.
    if let Some(worktree) = read_only_worktree {
        writeln!(
            &mut profile,
            "(deny file-write* (subpath {}))",
            quote_path(worktree)?
        )
        .expect("writing to a String cannot fail");
    }
    if !network {
        profile.push_str("(deny network*)\n");
    }
    // `(deny default)` also denies networking, so omitting the explicit deny
    // above is not enough: a network-capable run needs sockets and outbound
    // connections allowed, or hosted providers fail at DNS resolution.
    if network {
        profile.push_str("(allow network*)\n(allow system-socket)\n");
    }
    Ok(profile)
}

fn quote_path(path: &std::path::Path) -> Result<String, RuntimeError> {
    let canonical = path.canonicalize()?;
    let value = canonical
        .to_str()
        .ok_or(RuntimeError::InvalidSpec("sandbox path is not UTF-8"))?;
    if value.chars().any(char::is_control) {
        return Err(RuntimeError::InvalidSpec(
            "sandbox path contains control characters",
        ));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command, time::Duration};

    use hephaestus_core::authority::CapabilitySet;
    use tempfile::tempdir;

    use super::*;
    use crate::{Budget, RunSpec, SandboxManager};

    #[test]
    fn seatbelt_profile_is_deny_by_default_and_capability_exact() {
        let directory = tempdir().expect("policy directory");
        let run = directory.path().join("run");
        let protected = directory.path().join("canonical");
        fs::create_dir(&run).expect("create run root");
        fs::create_dir(&protected).expect("create protected root");
        let executable = std::path::Path::new("/bin/cat");
        let offline = build_macos_profile(
            executable,
            &run,
            std::slice::from_ref(&protected),
            false,
            None,
        )
        .expect("build offline profile");
        assert!(offline.contains("(deny default)"));
        assert!(offline.contains("(deny network*)"));
        assert!(offline.contains(&quote_path(&protected).expect("quote protected path")));
        let online =
            build_macos_profile(executable, &run, &[], true, None).expect("build online profile");
        assert!(!online.contains("(deny network*)"));
        assert!(online.contains("(allow network*)"));
        assert!(!offline.contains("(allow network*)"));
        let worktree = run.join("worktree");
        fs::create_dir(&worktree).expect("create worktree");
        let read_only = build_macos_profile(executable, &run, &[], true, Some(&worktree))
            .expect("build read-only profile");
        let worktree_deny = format!(
            "(deny file-write* (subpath {}))",
            quote_path(&worktree).expect("quote worktree")
        );
        assert!(read_only.contains(&worktree_deny));
        assert!(
            read_only.find(&worktree_deny)
                > read_only.find(&format!(
                    "(allow file-write* (subpath {})",
                    quote_path(&run).expect("quote run root")
                )),
            "the worktree deny must follow the run-root allow so it takes precedence"
        );
        assert!(!online.contains(&worktree_deny));
    }

    #[test]
    fn seatbelt_profile_rejects_control_characters_in_paths() {
        let directory = tempdir().expect("policy directory");
        let executable = std::path::Path::new("/bin/cat");
        let invalid = directory.path().join("line\nbreak");
        fs::create_dir(&invalid).expect("create invalid path fixture");
        assert!(matches!(
            build_macos_profile(executable, &invalid, &[], false, None),
            Err(RuntimeError::InvalidSpec(_))
        ));
    }

    #[test]
    fn unavailable_backend_fails_closed_before_process_construction() {
        let policy = IsolationPolicy {
            backend: IsolationBackend::Unavailable,
            protected_paths: Vec::new(),
        };
        assert!(matches!(
            policy.launcher(),
            Err(RuntimeError::Unsupported(_))
        ));
        assert_eq!(
            detected_backend(true, true),
            IsolationBackend::MacOsSeatbelt
        );
        assert_eq!(detected_backend(true, false), IsolationBackend::Unavailable);
        assert_eq!(detected_backend(false, true), IsolationBackend::Unavailable);
        assert!(matches!(
            IsolationPolicy::unconfined_for_testing().launcher(),
            Err(RuntimeError::Unsupported(
                "test-only backend has no production launcher"
            ))
        ));
    }

    #[test]
    fn seatbelt_command_construction_is_testable_on_every_host() {
        let repository = tempdir().expect("repository directory");
        run_git(repository.path(), &["init", "-q"]);
        fs::write(repository.path().join("fixture.txt"), b"fixture\n")
            .expect("write repository fixture");
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
        let root = tempdir().expect("sandbox root");
        let manager = SandboxManager::open(root.path(), Duration::from_secs(30))
            .expect("open sandbox manager");
        let spec = RunSpec::new(
            "policy-construction",
            "genome",
            "world",
            repository.path(),
            "test the policy constructor",
            CapabilitySet::new(true, false),
            Budget::new(Duration::from_secs(5), 1_000, 0).expect("budget"),
        )
        .expect("run specification");
        let (sandbox, _token) = manager.create(&spec).expect("create sandbox");
        let invocation =
            ProviderInvocation::deterministic("/bin/cat", [], []).expect("provider invocation");
        let policy = IsolationPolicy {
            backend: IsolationBackend::MacOsSeatbelt,
            protected_paths: vec![repository.path().to_owned()],
        };

        let command = policy
            .command(&invocation, &sandbox)
            .expect("construct Seatbelt command");
        assert_eq!(command.get_program(), "/usr/bin/sandbox-exec");
        assert_eq!(command.get_current_dir(), Some(sandbox.worktree()));
        assert!(command.get_args().any(|argument| argument == "-p"));
        let worker = policy
            .worker_command(&invocation, sandbox.execution_dir())
            .expect("construct worker Seatbelt command");
        assert_eq!(worker.get_program(), "/usr/bin/sandbox-exec");
        assert_eq!(worker.get_current_dir(), Some(sandbox.execution_dir()));
        assert!(worker.get_args().any(|argument| argument == "-p"));
        sandbox.cleanup().expect("clean sandbox");
    }

    fn run_git(repository: &std::path::Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .status()
            .expect("run Git fixture command");
        assert!(status.success());
    }
}
