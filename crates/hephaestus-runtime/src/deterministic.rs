use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use hephaestus_core::authority::CapabilitySet;
use serde::Serialize;

use crate::{
    AdapterCapabilities, CapabilityToken, CompletionReason, Provider, RunHandle, RunSnapshot,
    RunSpec, RunStatus, RuntimeAdapter, RuntimeError, RuntimeObservation, RuntimeObservationKind,
    Sandbox,
};

/// Offline reference runtime that inventories the isolated worktree deterministically.
#[derive(Default)]
pub struct DeterministicRuntime {
    runs: BTreeMap<String, ReferenceRun>,
}

struct ReferenceRun {
    worktree: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    capabilities: CapabilitySet,
    maximum_output_bytes: usize,
    status: RunStatus,
    checkpoint: Option<String>,
    genome_id: String,
    world_id: String,
    source_revision: String,
    prompt_hash: String,
    started: Instant,
    deadline: Instant,
    elapsed: Duration,
    observations: Vec<RuntimeObservation>,
}

impl RuntimeAdapter for DeterministicRuntime {
    fn provider(&self) -> Provider {
        Provider::Deterministic
    }

    fn report_capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            resume: true,
            interrupt: true,
            snapshot: true,
            authority: CapabilitySet::new(true, false),
        }
    }

    fn start(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        token: &CapabilityToken,
    ) -> Result<RunHandle, RuntimeError> {
        if self.runs.contains_key(spec.run_id()) {
            return Err(RuntimeError::InvalidSpec("run already exists"));
        }
        let run = reference_run(self, spec, sandbox, token, None)?;
        self.runs.insert(spec.run_id().to_owned(), run);
        Ok(RunHandle {
            run_id: spec.run_id().to_owned(),
            provider: Provider::Deterministic,
        })
    }

    fn resume(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        token: &CapabilityToken,
        checkpoint: &str,
    ) -> Result<RunHandle, RuntimeError> {
        if checkpoint.trim().is_empty() {
            return Err(RuntimeError::InvalidSpec("checkpoint is required"));
        }
        let existing = self
            .runs
            .get(spec.run_id())
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        if existing.status == RunStatus::Running {
            return Err(RuntimeError::InvalidSpec("running run cannot resume"));
        }
        let run = reference_run(self, spec, sandbox, token, Some(checkpoint))?;
        self.runs.insert(spec.run_id().to_owned(), run);
        Ok(RunHandle {
            run_id: spec.run_id().to_owned(),
            provider: Provider::Deterministic,
        })
    }

    fn interrupt(&mut self, run_id: &str) -> Result<(), RuntimeError> {
        let run = self
            .runs
            .get_mut(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        run.status = RunStatus::Interrupted;
        run.elapsed = run.started.elapsed();
        fs::write(&run.stderr_path, b"interrupted by supervisor\n")?;
        Ok(())
    }

    fn snapshot(&mut self, run_id: &str) -> Result<RunSnapshot, RuntimeError> {
        let run = self
            .runs
            .get_mut(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        if run.status == RunStatus::Running {
            if let Some((output, files)) = inventory(run)? {
                run.observations.extend(files.iter().map(|file| {
                    RuntimeObservation::new(
                        RuntimeObservationKind::FileRead,
                        BTreeMap::from([
                            ("path".to_owned(), file.path.clone()),
                            ("bytes".to_owned(), file.bytes.to_string()),
                            ("blake3".to_owned(), file.blake3.clone()),
                        ]),
                    )
                }));
                run.observations.push(RuntimeObservation::new(
                    RuntimeObservationKind::ModelResponse,
                    BTreeMap::from([
                        ("output_bytes".to_owned(), output.len().to_string()),
                        (
                            "output_hash".to_owned(),
                            blake3::hash(&output).to_hex().to_string(),
                        ),
                    ]),
                ));
                persist_terminal_output(run, &output)?;
            } else {
                mark_wall_budget_exceeded(run)?;
            }
            run.elapsed = run.started.elapsed();
        }
        Ok(RunSnapshot {
            run_id: run_id.to_owned(),
            status: run.status,
            exit_code: match run.status {
                RunStatus::Succeeded => Some(0),
                RunStatus::Failed => Some(1),
                RunStatus::Running | RunStatus::Interrupted | RunStatus::TimedOut => None,
            },
            completion_reason: match run.status {
                RunStatus::Running => None,
                RunStatus::Succeeded => Some(CompletionReason::Success),
                RunStatus::Failed => Some(CompletionReason::OutputBudgetExceeded),
                RunStatus::Interrupted => Some(CompletionReason::OperatorInterrupt),
                RunStatus::TimedOut => Some(CompletionReason::WallBudgetExceeded),
            },
            elapsed: if run.status == RunStatus::Running {
                run.started.elapsed()
            } else {
                run.elapsed
            },
            stdout_path: run.stdout_path.clone(),
            stderr_path: run.stderr_path.clone(),
            capabilities: run.capabilities,
        })
    }

    fn drain_observations(
        &mut self,
        run_id: &str,
    ) -> Result<Vec<RuntimeObservation>, RuntimeError> {
        let run = self
            .runs
            .get_mut(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        Ok(std::mem::take(&mut run.observations))
    }
}

fn reference_run(
    runtime: &DeterministicRuntime,
    spec: &RunSpec,
    sandbox: &Sandbox,
    token: &CapabilityToken,
    checkpoint: Option<&str>,
) -> Result<ReferenceRun, RuntimeError> {
    if spec.agent_instruction().is_some() || spec.provider_model().is_some() {
        return Err(RuntimeError::InvalidSpec(
            "deterministic runtime cannot deliver provider configuration",
        ));
    }
    sandbox.authorize_spec(token, spec)?;
    runtime
        .report_capabilities()
        .authority
        .derive_child(spec.capabilities())
        .map_err(|_| RuntimeError::CapabilityDenied)?;
    let started = Instant::now();
    let deadline = started
        .checked_add(spec.budget().wall())
        .ok_or(RuntimeError::InvalidSpec("wall budget exceeds clock range"))?;
    Ok(ReferenceRun {
        worktree: sandbox.worktree().to_owned(),
        stdout_path: sandbox.execution_dir().join("stdout.json"),
        stderr_path: sandbox.execution_dir().join("stderr.txt"),
        capabilities: spec.capabilities(),
        maximum_output_bytes: spec.budget().maximum_output_bytes(),
        status: RunStatus::Running,
        checkpoint: checkpoint.map(str::to_owned),
        genome_id: spec.genome_id().to_owned(),
        world_id: spec.world_id().to_owned(),
        source_revision: spec.source_revision().to_owned(),
        prompt_hash: blake3::hash(spec.prompt().as_bytes()).to_hex().to_string(),
        started,
        deadline,
        elapsed: Duration::ZERO,
        observations: vec![RuntimeObservation::new(
            RuntimeObservationKind::ContextComposed,
            BTreeMap::from([
                ("prompt_bytes".to_owned(), spec.prompt().len().to_string()),
                (
                    "prompt_hash".to_owned(),
                    blake3::hash(spec.prompt().as_bytes()).to_hex().to_string(),
                ),
            ]),
        )],
    })
}

#[derive(Serialize)]
struct Inventory<'a> {
    schema_version: u16,
    genome_id: &'a str,
    world_id: &'a str,
    source_revision: &'a str,
    prompt_hash: &'a str,
    checkpoint: Option<&'a str>,
    files: Vec<FileRecord>,
}

#[derive(Clone, Serialize)]
struct FileRecord {
    path: String,
    bytes: u64,
    blake3: String,
}

type InventoryResult = (Vec<u8>, Vec<FileRecord>);

fn inventory(run: &ReferenceRun) -> Result<Option<InventoryResult>, RuntimeError> {
    if deadline_reached(run.deadline) {
        return Ok(None);
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&run.worktree)
        .args(["ls-files", "-z"])
        .output();
    if deadline_reached(run.deadline) {
        return Ok(None);
    }
    let output = output?;
    if !output.status.success() {
        return Err(RuntimeError::Git(
            "tracked-file inventory failed".to_owned(),
        ));
    }
    let mut files = Vec::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        if deadline_reached(run.deadline) {
            return Ok(None);
        }
        let relative = std::str::from_utf8(raw)
            .map_err(|_| RuntimeError::InvalidSpec("repository path is not UTF-8"))?;
        let path = safe_tracked_path(&run.worktree, relative)?;
        let metadata = fs::symlink_metadata(&path);
        if deadline_reached(run.deadline) {
            return Ok(None);
        }
        let metadata = metadata?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        let bytes = fs::read(path);
        if deadline_reached(run.deadline) {
            return Ok(None);
        }
        let bytes = bytes?;
        files.push(FileRecord {
            path: relative.to_owned(),
            bytes: metadata.len(),
            blake3: blake3::hash(&bytes).to_hex().to_string(),
        });
    }
    let output = serde_json::to_vec(&Inventory {
        schema_version: 1,
        genome_id: &run.genome_id,
        world_id: &run.world_id,
        source_revision: &run.source_revision,
        prompt_hash: &run.prompt_hash,
        checkpoint: run.checkpoint.as_deref(),
        files: files.clone(),
    })
    .map_err(|_| RuntimeError::InvalidSpec("inventory serialization failed"))?;
    if deadline_reached(run.deadline) {
        return Ok(None);
    }
    Ok(Some((output, files)))
}

fn persist_terminal_output(run: &mut ReferenceRun, output: &[u8]) -> Result<(), RuntimeError> {
    if deadline_reached(run.deadline) {
        return mark_wall_budget_exceeded(run);
    }
    if output.len() > run.maximum_output_bytes {
        let stdout = fs::write(&run.stdout_path, []);
        if deadline_reached(run.deadline) {
            return mark_wall_budget_exceeded(run);
        }
        stdout?;
        let stderr = fs::write(&run.stderr_path, b"output budget exceeded\n");
        if deadline_reached(run.deadline) {
            return mark_wall_budget_exceeded(run);
        }
        stderr?;
        run.status = RunStatus::Failed;
    } else {
        let stdout = fs::write(&run.stdout_path, output);
        if deadline_reached(run.deadline) {
            return mark_wall_budget_exceeded(run);
        }
        stdout?;
        let stderr = fs::write(&run.stderr_path, []);
        if deadline_reached(run.deadline) {
            return mark_wall_budget_exceeded(run);
        }
        stderr?;
        run.status = RunStatus::Succeeded;
    }
    Ok(())
}

fn mark_wall_budget_exceeded(run: &mut ReferenceRun) -> Result<(), RuntimeError> {
    run.status = RunStatus::TimedOut;
    run.elapsed = run.started.elapsed();
    fs::write(&run.stdout_path, [])?;
    fs::write(&run.stderr_path, b"wall budget exceeded\n")?;
    Ok(())
}

fn deadline_reached(deadline: Instant) -> bool {
    Instant::now() >= deadline
}

fn safe_tracked_path(root: &Path, relative: &str) -> Result<PathBuf, RuntimeError> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(RuntimeError::InvalidSpec("tracked path escapes worktree"));
    }
    Ok(root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_tracked_path_joins_ordinary_relative_paths() {
        let root = Path::new("/worktree");
        assert_eq!(
            safe_tracked_path(root, "src/lib.rs").expect("ordinary path"),
            root.join("src/lib.rs")
        );
    }

    #[test]
    fn safe_tracked_path_rejects_absolute_paths() {
        let root = Path::new("/worktree");
        assert!(matches!(
            safe_tracked_path(root, "/etc/passwd"),
            Err(RuntimeError::InvalidSpec(message)) if message == "tracked path escapes worktree"
        ));
    }

    #[test]
    fn safe_tracked_path_rejects_parent_directory_escapes() {
        let root = Path::new("/worktree");
        assert!(matches!(
            safe_tracked_path(root, "../secret"),
            Err(RuntimeError::InvalidSpec(message)) if message == "tracked path escapes worktree"
        ));
    }

    #[test]
    fn deadline_reached_reports_past_and_future_instants() {
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("Instant subtraction fits");
        assert!(deadline_reached(past));
        let future = Instant::now() + Duration::from_secs(60);
        assert!(!deadline_reached(future));
    }
}
