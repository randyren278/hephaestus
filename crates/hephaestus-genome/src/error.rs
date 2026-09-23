use std::{error::Error, fmt};

use hephaestus_core::domain::MutationTarget;

/// Fail-closed Genome and World compilation errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompileError {
    /// Source exceeded the deterministic parser budget.
    InputTooLarge {
        /// Actual UTF-8 byte length.
        bytes: usize,
        /// Maximum accepted UTF-8 byte length.
        maximum: usize,
    },
    /// Typed JSON or YAML deserialization failed.
    Parse(String),
    /// Markdown source did not contain exactly one supported YAML frontmatter block.
    InvalidMarkdownFrontmatter,
    /// Markdown prompt body or stored prompt artifact was blank.
    EmptyAgentPrompt,
    /// A Markdown Genome attempted to set the compiler-reserved prompt artifact.
    ReservedAgentPromptArtifact,
    /// The reserved prompt artifact was not valid UTF-8 text.
    InvalidAgentPromptArtifact,
    /// The artifact store could not persist the exact Markdown prompt body.
    ArtifactStore,
    /// The document schema version is not supported.
    UnsupportedSchemaVersion(u16),
    /// A required stable name was empty.
    EmptyField(&'static str),
    /// A declared parent Genome was unavailable.
    UnresolvedParent(String),
    /// A parent lookup key did not match the compiled parent's content identity.
    ParentIdentityMismatch {
        /// Identity declared by the child.
        declared: String,
        /// Identity recomputed and stored on the resolved parent.
        actual: String,
    },
    /// A referenced artifact was unavailable.
    UnresolvedArtifact(String),
    /// An artifact address was not canonical.
    InvalidArtifactId(String),
    /// Artifact bytes or storage could not be verified.
    ArtifactIntegrity(String),
    /// Requested authority exceeded the World or a parent.
    AuthorityEscalation,
    /// A World permitted direct candidate evaluator access.
    CandidateEvaluatorAccess,
    /// A World exposed a protected mutation surface.
    ProtectedMutationTarget(MutationTarget),
    /// Promotion confidence was outside 1..=10,000 basis points.
    InvalidConfidence(u16),
    /// A required objective list was empty.
    EmptyObjectives,
    /// Two results came from different World identities.
    IncompatibleWorlds {
        /// First World identity.
        left: String,
        /// Second World identity.
        right: String,
    },
    /// Canonical JSON serialization failed unexpectedly.
    Canonicalization(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for CompileError {}
