use std::{error::Error, fmt};

use hephaestus_experience::ExperienceError;
use hephaestus_ledger::LedgerError;
use hephaestus_runtime::RuntimeError;

/// Fail-closed evaluation and persistence errors.
#[derive(Debug)]
pub enum ArenaError {
    /// An identifier is empty, malformed, or non-canonical.
    InvalidId { field: &'static str, value: String },
    /// A task identifier occurs more than once.
    DuplicateTaskId(String),
    /// One runtime result is reused for more than one evaluation task.
    DuplicateRunEvent(String),
    /// A manifest has no tasks.
    EmptyManifest,
    /// A persisted task manifest uses an unsupported wire schema.
    UnsupportedManifestSchema(u16),
    /// Parsed manifest bytes do not equal the canonical wire encoding.
    NonCanonicalManifest,
    /// A manifest, evaluation, or submission exceeds the bounded task count.
    TooManyTasks,
    /// A task or submission contains text exceeding the evaluator boundary.
    TextTooLarge { field: &'static str, limit: usize },
    /// The visible and sealed manifests use the same identity.
    DuplicateManifestId(String),
    /// Parent and candidate resolve to the same immutable Genome.
    DuplicateGenomeId(String),
    /// Provenance differs between an input and the evaluator binding.
    BindingMismatch(&'static str),
    /// A required evaluator artifact is absent from the compiled World.
    MissingWorldArtifact(&'static str),
    /// World-bound evaluator evidence does not match the supplied canonical bytes.
    WorldArtifactMismatch(&'static str),
    /// The configured evaluator does not implement the scoring semantics used here.
    UnsupportedEvaluator,
    /// The isolated evaluator request or response violated its strict protocol.
    EvaluatorProtocol(&'static str),
    /// The isolated evaluator process failed or could not be trusted.
    EvaluatorExecution(String),
    /// The evaluator worker runtime failed closed.
    Runtime(RuntimeError),
    /// A manifest was supplied in the wrong visibility slot.
    VisibilityMismatch,
    /// A submission is missing or adds task identifiers.
    TaskSetMismatch { submission_id: String },
    /// A referenced canonical runtime result event does not exist.
    UnknownRunEvent(String),
    /// A requested canonical evaluation event does not exist.
    UnknownEvaluation(String),
    /// A runtime receipt failed its canonical envelope or schema validation.
    RunReceipt(ExperienceError),
    /// A trial ran under a different compiled World.
    RunWorldMismatch(String),
    /// One submission mixes trials from different immutable Genomes.
    MixedSubmissionGenome,
    /// Paired trials did not execute the same source revision.
    SourceRevisionMismatch(String),
    /// A runtime stdout artifact is not bounded UTF-8 evaluator input.
    InvalidRunOutput(String),
    /// Authenticated run metrics could not be aggregated without overflow.
    MetricOverflow(&'static str),
    /// An existing deterministic evaluation identity has different canonical contents.
    EvaluationConflict(String),
    /// A stored operator receipt is malformed or inconsistent with its ledger metadata.
    InvalidStoredReceipt(&'static str),
    /// Durable evidence storage failed.
    Ledger(LedgerError),
    /// Selection was requested under a World different from its verified evaluation.
    SelectionWorldMismatch,
    /// The Python statistics contract does not support the World's 10000-bps confidence endpoint.
    UnsupportedSelectionConfidence(u16),
    /// Bootstrap work exceeds the explicit deterministic CPU bound.
    BootstrapWorkExceeded,
    /// No durable selection event exists for the requested evaluation.
    UnknownSelection(String),
    /// A selection receipt conflicts with the recomputed trusted result.
    SelectionConflict(String),
    /// A selection event envelope or canonical payload is invalid.
    InvalidSelectionEvent,
    /// No durable invariant receipt exists for the requested evaluation.
    UnknownInvariantCheck(String),
    /// A deterministic invariant event conflicts with recomputed evidence.
    InvariantConflict(String),
    /// An invariant event envelope or canonical payload is invalid.
    InvalidInvariantEvent,
    /// Canonical JSON encoding failed.
    Serialization(serde_json::Error),
    /// Evaluator executable inspection failed.
    Io(std::io::Error),
}

impl fmt::Display for ArenaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for ArenaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Ledger(error) => Some(error),
            Self::RunReceipt(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Serialization(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<LedgerError> for ArenaError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}

impl From<serde_json::Error> for ArenaError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

impl From<ExperienceError> for ArenaError {
    fn from(error: ExperienceError) -> Self {
        Self::RunReceipt(error)
    }
}

impl From<RuntimeError> for ArenaError {
    fn from(error: RuntimeError) -> Self {
        Self::Runtime(error)
    }
}

impl From<std::io::Error> for ArenaError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
