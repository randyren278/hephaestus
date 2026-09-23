use std::{path::PathBuf, process::Command, time::Duration};

use hephaestus_core::authority::CapabilitySet;

use crate::RuntimeError;
use crate::{ReferenceInstruction, reference_instruction::MAX_TASK_INPUT_BYTES};

const MAX_CONTEXT_ID_BYTES: usize = 128;
const MAX_RUN_ID_BYTES: usize = 128;
const REFERENCE_ENVIRONMENT_ID: &str = "reference-runtime-v1";

/// Runtime-owned experiment coordinates that make paired trials comparable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExperimentContext {
    task_id: String,
    input_commitment: String,
    seed: u64,
    environment_id: String,
}

impl ExperimentContext {
    /// Creates a bounded context and commits to the exact task input bytes.
    ///
    /// # Errors
    ///
    /// Rejects malformed task or environment identities.
    pub fn new(
        task_id: impl Into<String>,
        input: impl AsRef<[u8]>,
        seed: u64,
        environment_id: impl Into<String>,
    ) -> Result<Self, RuntimeError> {
        let task_id = task_id.into();
        let environment_id = environment_id.into();
        validate_context_id(&task_id, "task_id is invalid")?;
        validate_context_id(&environment_id, "environment_id is invalid")?;
        Ok(Self {
            task_id,
            input_commitment: blake3::hash(input.as_ref()).to_hex().to_string(),
            seed,
            environment_id,
        })
    }

    /// Stable task identity shared by paired trials.
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// BLAKE3 commitment to the exact provider input bytes.
    #[must_use]
    pub fn input_commitment(&self) -> &str {
        &self.input_commitment
    }

    /// Runtime-owned deterministic seed.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Exact execution environment identity.
    #[must_use]
    pub fn environment_id(&self) -> &str {
        &self.environment_id
    }
}

/// Hard per-run resource budget enforced by the supervisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Budget {
    wall: Duration,
    maximum_output_bytes: usize,
    maximum_cost_microusd: u64,
}

impl Budget {
    /// Creates a validated non-zero execution budget.
    ///
    /// # Errors
    ///
    /// Rejects zero wall time or output capacity.
    pub const fn new(
        wall: Duration,
        maximum_output_bytes: usize,
        maximum_cost_microusd: u64,
    ) -> Result<Self, RuntimeError> {
        if wall.is_zero() {
            return Err(RuntimeError::InvalidSpec("wall budget must be non-zero"));
        }
        if maximum_output_bytes == 0 {
            return Err(RuntimeError::InvalidSpec("output budget must be non-zero"));
        }
        Ok(Self {
            wall,
            maximum_output_bytes,
            maximum_cost_microusd,
        })
    }

    /// Maximum wall-clock duration.
    #[must_use]
    pub const fn wall(self) -> Duration {
        self.wall
    }

    /// Maximum persisted stdout plus stderr bytes.
    #[must_use]
    pub const fn maximum_output_bytes(self) -> usize {
        self.maximum_output_bytes
    }

    /// Maximum provider spend in micro-US dollars.
    #[must_use]
    pub const fn maximum_cost_microusd(self) -> u64 {
        self.maximum_cost_microusd
    }
}

/// Provider-neutral immutable request to execute one Genome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunSpec {
    run_id: String,
    genome_id: String,
    world_id: String,
    source_repository: PathBuf,
    source_revision: String,
    prompt: String,
    capabilities: CapabilitySet,
    budget: Budget,
    experiment: ExperimentContext,
    reference_instruction: Option<ReferenceInstruction>,
}

impl RunSpec {
    /// Creates a validated provider-neutral run request.
    ///
    /// # Errors
    ///
    /// Rejects unsafe run identifiers, blank immutable identities or prompts,
    /// and source paths that are not directories.
    pub fn new(
        run_id: impl Into<String>,
        genome_id: impl Into<String>,
        world_id: impl Into<String>,
        source_repository: impl Into<PathBuf>,
        prompt: impl Into<String>,
        capabilities: CapabilitySet,
        budget: Budget,
    ) -> Result<Self, RuntimeError> {
        let run_id = run_id.into();
        let prompt = prompt.into();
        let experiment =
            ExperimentContext::new(&run_id, prompt.as_bytes(), 0, REFERENCE_ENVIRONMENT_ID)?;
        Self::new_for_experiment_at_revision(
            run_id,
            genome_id,
            world_id,
            source_repository,
            "HEAD",
            prompt,
            capabilities,
            budget,
            experiment,
        )
    }

    /// Creates a run request pinned to the commit resolved from `source_revision`.
    ///
    /// The revision is resolved during construction, so later branch or `HEAD`
    /// movement cannot change the source paired with this request.
    ///
    /// # Errors
    ///
    /// In addition to [`Self::new`] validation, rejects revisions that Git cannot
    /// resolve to an immutable commit in the source repository.
    #[allow(clippy::too_many_arguments)]
    pub fn new_at_revision(
        run_id: impl Into<String>,
        genome_id: impl Into<String>,
        world_id: impl Into<String>,
        source_repository: impl Into<PathBuf>,
        source_revision: impl AsRef<str>,
        prompt: impl Into<String>,
        capabilities: CapabilitySet,
        budget: Budget,
    ) -> Result<Self, RuntimeError> {
        let run_id = run_id.into();
        let prompt = prompt.into();
        let experiment =
            ExperimentContext::new(&run_id, prompt.as_bytes(), 0, REFERENCE_ENVIRONMENT_ID)?;
        Self::new_for_experiment_at_revision(
            run_id,
            genome_id,
            world_id,
            source_repository,
            source_revision,
            prompt,
            capabilities,
            budget,
            experiment,
        )
    }

    /// Creates a run with explicit runtime-owned experiment coordinates.
    ///
    /// # Errors
    ///
    /// Applies the same validation as [`Self::new`] and requires the context's
    /// input commitment to match the exact prompt bytes.
    #[allow(clippy::too_many_arguments)]
    pub fn new_for_experiment(
        run_id: impl Into<String>,
        genome_id: impl Into<String>,
        world_id: impl Into<String>,
        source_repository: impl Into<PathBuf>,
        prompt: impl Into<String>,
        capabilities: CapabilitySet,
        budget: Budget,
        experiment: ExperimentContext,
    ) -> Result<Self, RuntimeError> {
        Self::new_for_experiment_at_revision(
            run_id,
            genome_id,
            world_id,
            source_repository,
            "HEAD",
            prompt,
            capabilities,
            budget,
            experiment,
        )
    }

    /// Creates a revision-pinned run with explicit experiment coordinates.
    ///
    /// # Errors
    ///
    /// Rejects invalid run fields, revisions, or a context committed to input
    /// bytes different from the provider prompt.
    #[allow(clippy::too_many_arguments)]
    pub fn new_for_experiment_at_revision(
        run_id: impl Into<String>,
        genome_id: impl Into<String>,
        world_id: impl Into<String>,
        source_repository: impl Into<PathBuf>,
        source_revision: impl AsRef<str>,
        prompt: impl Into<String>,
        capabilities: CapabilitySet,
        budget: Budget,
        experiment: ExperimentContext,
    ) -> Result<Self, RuntimeError> {
        let run_id = run_id.into();
        if run_id.is_empty()
            || run_id.len() > MAX_RUN_ID_BYTES
            || !run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(RuntimeError::InvalidSpec("run_id is not path safe"));
        }
        let genome_id = genome_id.into();
        let world_id = world_id.into();
        let prompt = prompt.into();
        if genome_id.trim().is_empty() || world_id.trim().is_empty() || prompt.trim().is_empty() {
            return Err(RuntimeError::InvalidSpec(
                "Genome, World, and prompt are required",
            ));
        }
        if experiment.input_commitment != blake3::hash(prompt.as_bytes()).to_hex().as_str() {
            return Err(RuntimeError::InvalidSpec(
                "experiment input commitment does not match prompt",
            ));
        }
        let source_repository = source_repository.into();
        if !source_repository.is_dir() {
            return Err(RuntimeError::InvalidSpec(
                "source repository is not a directory",
            ));
        }
        let source_revision = resolve_commit(&source_repository, source_revision.as_ref())?;
        Ok(Self {
            run_id,
            genome_id,
            world_id,
            source_repository,
            source_revision,
            prompt,
            capabilities,
            budget,
            experiment,
            reference_instruction: None,
        })
    }

    /// Stable run identity.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Immutable Genome identity.
    #[must_use]
    pub fn genome_id(&self) -> &str {
        &self.genome_id
    }

    /// Immutable World identity.
    #[must_use]
    pub fn world_id(&self) -> &str {
        &self.world_id
    }

    /// Git repository used to create an isolated worktree.
    #[must_use]
    pub fn source_repository(&self) -> &std::path::Path {
        &self.source_repository
    }

    /// Exact Git commit paired with this run.
    #[must_use]
    pub fn source_revision(&self) -> &str {
        &self.source_revision
    }

    /// Provider-neutral task prompt.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Explicit worker capabilities.
    #[must_use]
    pub const fn capabilities(&self) -> CapabilitySet {
        self.capabilities
    }

    /// Hard run budget.
    #[must_use]
    pub const fn budget(&self) -> Budget {
        self.budget
    }

    /// Runtime-owned task, input, seed, and environment coordinates.
    #[must_use]
    pub const fn experiment(&self) -> &ExperimentContext {
        &self.experiment
    }

    /// Reference-worker instruction resolved from the registered Genome CAS.
    #[must_use]
    pub const fn reference_instruction(&self) -> Option<ReferenceInstruction> {
        self.reference_instruction
    }

    /// Adds a separate reference-worker instruction without changing task input.
    /// # Errors
    ///
    /// Rejects task input larger than the bounded reference worker frame.
    pub fn with_reference_instruction(
        mut self,
        instruction: ReferenceInstruction,
    ) -> Result<Self, RuntimeError> {
        if self.prompt.len() > MAX_TASK_INPUT_BYTES {
            return Err(RuntimeError::InvalidSpec(
                "reference task input is oversized",
            ));
        }
        self.reference_instruction = Some(instruction);
        Ok(self)
    }
}

fn validate_context_id(value: &str, error: &'static str) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.len() > MAX_CONTEXT_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        return Err(RuntimeError::InvalidSpec(error));
    }
    Ok(())
}

fn resolve_commit(repository: &std::path::Path, revision: &str) -> Result<String, RuntimeError> {
    if revision.trim().is_empty() {
        return Err(RuntimeError::InvalidSpec("source revision is required"));
    }
    let commit_expression = format!("{revision}^{{commit}}");
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(commit_expression)
        .output()?;
    if !output.status.success() {
        return Err(RuntimeError::Git(
            "source revision is not a commit".to_owned(),
        ));
    }
    let commit = std::str::from_utf8(&output.stdout)
        .map_err(|_| RuntimeError::Git("resolved source revision is not UTF-8".to_owned()))?
        .trim();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RuntimeError::Git(
            "resolved source revision is not an object ID".to_owned(),
        ));
    }
    Ok(commit.to_ascii_lowercase())
}
