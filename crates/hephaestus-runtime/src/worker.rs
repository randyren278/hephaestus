use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use crate::{
    CompletionReason, IsolationPolicy, ProviderInvocation, RuntimeError,
    guardian::GuardianLaunch,
    supervisor::{execute_guarded_process, execute_supervised_process},
};

const MAX_WORKER_ID_BYTES: usize = 128;

/// Trust domain assigned to one isolated helper process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerDomain {
    /// Untrusted candidate execution.
    Candidate,
    /// Trusted evaluator execution isolated from candidates and canonical stores.
    Evaluator,
}

impl WorkerDomain {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Evaluator => "evaluator",
        }
    }
}

/// Hard limits applied by the trusted worker supervisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerLimits {
    wall: Duration,
    maximum_input_bytes: usize,
    maximum_output_bytes: usize,
}

impl WorkerLimits {
    /// Creates non-zero wall, input, and combined-output limits.
    ///
    /// # Errors
    ///
    /// Rejects any zero limit.
    pub const fn new(
        wall: Duration,
        maximum_input_bytes: usize,
        maximum_output_bytes: usize,
    ) -> Result<Self, RuntimeError> {
        if wall.is_zero() || maximum_input_bytes == 0 || maximum_output_bytes == 0 {
            return Err(RuntimeError::InvalidSpec("worker limits must be non-zero"));
        }
        Ok(Self {
            wall,
            maximum_input_bytes,
            maximum_output_bytes,
        })
    }
}

/// Fully captured terminal worker result; its private execution root is already removed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerOutput {
    /// Explicit trust domain used for execution.
    pub domain: WorkerDomain,
    /// Supervisor-owned terminal reason.
    pub completion_reason: CompletionReason,
    /// Child exit code when the operating system reported one.
    pub exit_code: Option<i32>,
    /// Monotonic elapsed wall time.
    pub elapsed: Duration,
    /// Bounded standard output.
    pub stdout: Vec<u8>,
    /// Bounded standard error.
    pub stderr: Vec<u8>,
}

/// Synchronous, offline isolated worker bound to one explicit trust domain.
pub struct IsolatedWorker {
    root: PathBuf,
    root_device: u64,
    root_inode: u64,
    isolation: IsolationPolicy,
    domain: WorkerDomain,
    executable: PathBuf,
    arguments: Vec<String>,
    limits: WorkerLimits,
}

impl IsolatedWorker {
    /// Opens a trusted worker root and fixes its filesystem identity.
    ///
    /// Protected paths are supplied through `isolation`; production execution
    /// fails closed when that policy has no verified backend.
    ///
    /// # Errors
    ///
    /// Rejects unsafe roots and empty executable paths, and propagates filesystem
    /// failures while preparing the private root.
    pub fn open(
        root: impl Into<PathBuf>,
        isolation: IsolationPolicy,
        domain: WorkerDomain,
        executable: impl Into<PathBuf>,
        arguments: impl IntoIterator<Item = String>,
        limits: WorkerLimits,
    ) -> Result<Self, RuntimeError> {
        let root = root.into();
        prepare_root(&root)?;
        let root = fs::canonicalize(root)?;
        let metadata = fs::symlink_metadata(&root)?;
        let executable = executable.into();
        ProviderInvocation::deterministic(&executable, [], [])?;
        Ok(Self {
            root,
            root_device: metadata.dev(),
            root_inode: metadata.ino(),
            isolation,
            domain,
            executable,
            arguments: arguments.into_iter().collect(),
            limits,
        })
    }

    /// Executes one bounded request and removes all worker files before returning.
    ///
    /// # Errors
    ///
    /// Rejects malformed identities, oversized input, changed roots, unavailable
    /// isolation, process-launch failures, and incomplete cleanup.
    pub fn execute(&self, worker_id: &str, input: &[u8]) -> Result<WorkerOutput, RuntimeError> {
        validate_worker_id(worker_id)?;
        if input.len() > self.limits.maximum_input_bytes {
            return Err(RuntimeError::InvalidSpec(
                "worker input exceeds its byte limit",
            ));
        }
        self.validate_root()?;
        let run_root = self
            .root
            .join(format!("{}-{worker_id}", self.domain.prefix()));
        fs::create_dir(&run_root)?;
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700))?;
        let mut cleanup = WorkerRoot::new(run_root);
        let stdout_path = cleanup.path().join("stdout");
        let stderr_path = cleanup.path().join("stderr");
        let invocation =
            ProviderInvocation::deterministic(&self.executable, self.arguments.clone(), input)?;
        let mut command = self.isolation.worker_command(&invocation, cleanup.path())?;
        command.env_clear();
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        command.env("HOME", cleanup.path());
        command.env("TMPDIR", cleanup.path());
        let process = execute_supervised_process(
            command,
            invocation.stdin().to_vec(),
            &stdout_path,
            &stderr_path,
            self.limits.wall,
            self.limits.maximum_output_bytes,
        )?;
        let stdout = fs::read(stdout_path)?;
        let stderr = fs::read(stderr_path)?;
        cleanup.cleanup()?;
        Ok(WorkerOutput {
            domain: self.domain,
            completion_reason: process.completion_reason,
            exit_code: process.exit_code,
            elapsed: process.elapsed,
            stdout,
            stderr,
        })
    }

    /// Executes one bounded request under the daemon-liveness process guardian.
    ///
    /// This path is intended for work whose daemon-owned caller can be cancelled
    /// independently. The guardian owns the worker process group and terminates it
    /// if the daemon closes its control pipe, even when the caller process crashes.
    ///
    /// # Errors
    ///
    /// Rejects invalid identities, oversized input, changed roots, unavailable
    /// isolation, worker failures, and incomplete output cleanup.
    pub fn execute_guarded(
        &self,
        worker_id: &str,
        input: &[u8],
        guardian_executable: impl AsRef<Path>,
        cancel: Arc<AtomicBool>,
    ) -> Result<WorkerOutput, RuntimeError> {
        validate_worker_id(worker_id)?;
        if input.len() > self.limits.maximum_input_bytes {
            return Err(RuntimeError::InvalidSpec(
                "worker input exceeds its byte limit",
            ));
        }
        self.validate_root()?;
        let run_root = self
            .root
            .join(format!("{}-{worker_id}", self.domain.prefix()));
        fs::create_dir(&run_root)?;
        fs::set_permissions(&run_root, fs::Permissions::from_mode(0o700))?;
        let mut cleanup = WorkerRoot::new(run_root);
        let stdout_path = cleanup.path().join("stdout");
        let stderr_path = cleanup.path().join("stderr");
        let invocation =
            ProviderInvocation::deterministic(&self.executable, self.arguments.clone(), input)?;
        let mut command = self.isolation.worker_command(&invocation, cleanup.path())?;
        command.env_clear();
        let path = std::env::var("PATH").ok();
        if let Some(path) = &path {
            command.env("PATH", path);
        }
        command.env("HOME", cleanup.path());
        command.env("TMPDIR", cleanup.path());
        let program = command
            .get_program()
            .to_str()
            .ok_or(RuntimeError::InvalidSpec(
                "worker program path is not UTF-8",
            ))?
            .to_owned();
        let arguments = command
            .get_args()
            .map(|argument| {
                argument
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(RuntimeError::InvalidSpec("worker argument is not UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let config = GuardianLaunch {
            program,
            arguments,
            current_dir: command
                .get_current_dir()
                .unwrap_or_else(|| cleanup.path())
                .to_owned(),
            home: cleanup.path().to_owned(),
            temp: cleanup.path().to_owned(),
            path,
            input_bytes: input.len(),
        };
        let mut frame = serde_json::to_vec(&config)
            .map_err(|_| RuntimeError::InvalidSpec("guardian configuration is invalid"))?;
        frame.push(b'\n');
        frame.extend_from_slice(input);
        frame.push(b'\n');
        let process = execute_guarded_process(
            Command::new(guardian_executable.as_ref()),
            frame,
            &stdout_path,
            &stderr_path,
            self.limits.wall,
            self.limits.maximum_output_bytes,
            cancel,
        )?;
        let stdout = fs::read(stdout_path)?;
        let stderr = fs::read(stderr_path)?;
        cleanup.cleanup()?;
        Ok(WorkerOutput {
            domain: self.domain,
            completion_reason: process.completion_reason,
            exit_code: process.exit_code,
            elapsed: process.elapsed,
            stdout,
            stderr,
        })
    }

    fn validate_root(&self) -> Result<(), RuntimeError> {
        let metadata = fs::symlink_metadata(&self.root)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.dev() != self.root_device
            || metadata.ino() != self.root_inode
        {
            return Err(RuntimeError::InvalidSpec("worker root identity changed"));
        }
        Ok(())
    }
}

fn prepare_root(root: &Path) -> Result<(), RuntimeError> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(RuntimeError::InvalidSpec("worker root is unsafe"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(root)?,
        Err(error) => return Err(error.into()),
    }
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn validate_worker_id(worker_id: &str) -> Result<(), RuntimeError> {
    if worker_id.is_empty()
        || worker_id.len() > MAX_WORKER_ID_BYTES
        || !worker_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(RuntimeError::InvalidSpec("worker id is not path-safe"));
    }
    Ok(())
}

struct WorkerRoot {
    path: Option<PathBuf>,
}

impl WorkerRoot {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().expect("worker root is armed")
    }

    fn cleanup(&mut self) -> Result<(), RuntimeError> {
        let path = self.path.take().expect("worker root is armed");
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for WorkerRoot {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn unavailable_policy_fails_before_spawn_and_cleans_root() {
        let root = tempdir().expect("worker root");
        let marker = root.path().join("process-started");
        let worker = IsolatedWorker::open(
            root.path(),
            IsolationPolicy::unavailable_for_testing(),
            WorkerDomain::Candidate,
            "/usr/bin/touch",
            [marker.to_string_lossy().into_owned()],
            WorkerLimits::new(Duration::from_secs(1), 1, 1).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            worker.execute("unavailable", b""),
            Err(RuntimeError::Unsupported(_))
        ));
        assert!(!marker.exists());
        assert!(root.path().read_dir().unwrap().next().is_none());
    }
}
