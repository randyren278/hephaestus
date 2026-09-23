use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ExperienceError;

const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_FIELDS: usize = 64;
const MAX_FIELD_KEY_BYTES: usize = 128;

/// Immutable run, Genome, and World origin attached to every trace and lesson.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    #[serde(rename = "run_id")]
    pub(crate) run: String,
    #[serde(rename = "genome_id")]
    pub(crate) genome: String,
    #[serde(rename = "world_id")]
    pub(crate) world: String,
}

impl Provenance {
    /// Creates validated provenance.
    ///
    /// # Errors
    ///
    /// Rejects blank or excessively long identifiers.
    pub fn new(
        run_id: impl Into<String>,
        genome_id: impl Into<String>,
        world_id: impl Into<String>,
    ) -> Result<Self, ExperienceError> {
        let value = Self {
            run: run_id.into(),
            genome: genome_id.into(),
            world: world_id.into(),
        };
        value.validate()?;
        Ok(value)
    }

    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run
    }

    #[must_use]
    pub fn genome_id(&self) -> &str {
        &self.genome
    }

    #[must_use]
    pub fn world_id(&self) -> &str {
        &self.world
    }

    pub(crate) fn validate(&self) -> Result<(), ExperienceError> {
        for field in [&self.run, &self.genome, &self.world] {
            if field.trim().is_empty() || field.len() > MAX_IDENTIFIER_BYTES {
                return Err(ExperienceError::InvalidInput(
                    "provenance identifiers must be non-empty and bounded",
                ));
            }
        }
        Ok(())
    }
}

/// Observable runtime event classes; none require hidden chain-of-thought.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceKind {
    LifecycleStarted,
    LifecycleResumed,
    LifecycleCompleted,
    ToolCalled,
    ToolResult,
    ContextComposed,
    MemoryRetrieved,
    SubagentSpawned,
    FileRead,
    FileChanged,
    TestExecuted,
    CapabilityDenied,
    CostObserved,
    CheckpointCreated,
    Error,
    Retry,
    ModelResponse,
}

/// Caller-supplied structured trace before redaction and persistence.
pub struct TraceInput {
    pub(crate) event_id: String,
    pub(crate) provenance: Provenance,
    pub(crate) kind: TraceKind,
    pub(crate) timestamp_millis: i64,
    pub(crate) fields: BTreeMap<String, String>,
}

impl TraceInput {
    /// Returns the provenance supplied with this trace.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.provenance
    }

    /// Creates a bounded structured trace input.
    ///
    /// # Errors
    ///
    /// Rejects an invalid event ID, excessive field count, or invalid field key.
    pub fn new(
        event_id: impl Into<String>,
        provenance: Provenance,
        kind: TraceKind,
        timestamp_millis: i64,
        fields: BTreeMap<String, String>,
    ) -> Result<Self, ExperienceError> {
        let event_id = event_id.into();
        validate_identifier(&event_id, "trace event ID is invalid")?;
        validate_fields(&fields)?;
        Ok(Self {
            event_id,
            provenance,
            kind,
            timestamp_millis,
            fields,
        })
    }
}

/// Safe ledger receipt pointing to the full redacted CAS trace.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TraceReceipt {
    pub schema_version: u16,
    pub event_id: String,
    pub provenance: Provenance,
    pub kind: TraceKind,
    pub artifact_id: String,
    pub redacted_fields: usize,
}

/// Structured experience class kept distinct from experimentally proven Genes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceKind {
    Observation,
    Hypothesis,
    Evidence,
    Contradiction,
}

/// Caller-supplied provenance-backed experience before redaction and persistence.
pub struct ExperienceInput {
    pub(crate) experience_id: String,
    pub(crate) provenance: Provenance,
    pub(crate) kind: ExperienceKind,
    pub(crate) timestamp_millis: i64,
    pub(crate) source_event_ids: Vec<String>,
    pub(crate) evidence_artifact_ids: Vec<String>,
    pub(crate) confidence_bps: u16,
    pub(crate) fields: BTreeMap<String, String>,
}

impl ExperienceInput {
    /// Creates a bounded, explicitly sourced experience input.
    ///
    /// # Errors
    ///
    /// Rejects invalid IDs, empty provenance, confidence outside 1..=10,000,
    /// unsourced records, or contradictions with fewer than two sources.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        experience_id: impl Into<String>,
        provenance: Provenance,
        kind: ExperienceKind,
        timestamp_millis: i64,
        source_event_ids: Vec<String>,
        evidence_artifact_ids: Vec<String>,
        confidence_bps: u16,
        fields: BTreeMap<String, String>,
    ) -> Result<Self, ExperienceError> {
        let experience_id = experience_id.into();
        validate_identifier(&experience_id, "experience ID is invalid")?;
        if source_event_ids.is_empty() {
            return Err(ExperienceError::InvalidInput(
                "experience requires source events",
            ));
        }
        if source_event_ids.iter().collect::<BTreeSet<_>>().len() != source_event_ids.len() {
            return Err(ExperienceError::InvalidInput(
                "experience source events must be distinct",
            ));
        }
        if kind == ExperienceKind::Contradiction && source_event_ids.len() < 2 {
            return Err(ExperienceError::InvalidInput(
                "contradiction requires at least two distinct source events",
            ));
        }
        if !(1..=10_000).contains(&confidence_bps) {
            return Err(ExperienceError::InvalidInput(
                "experience confidence is invalid",
            ));
        }
        for source in &source_event_ids {
            validate_identifier(source, "source event ID is invalid")?;
        }
        validate_fields(&fields)?;
        Ok(Self {
            experience_id,
            provenance,
            kind,
            timestamp_millis,
            source_event_ids,
            evidence_artifact_ids,
            confidence_bps,
            fields,
        })
    }
}

/// Safe ledger receipt pointing to a redacted, explicitly unverified experience artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExperienceReceipt {
    pub schema_version: u16,
    pub experience_id: String,
    pub provenance: Provenance,
    pub kind: ExperienceKind,
    pub source_event_ids: Vec<String>,
    pub evidence_artifact_ids: Vec<String>,
    pub confidence_bps: u16,
    pub artifact_id: String,
    pub status: String,
    pub redacted_fields: usize,
}

/// Store-consistent Experience evidence for trusted operator-side consumers.
///
/// Instances can only be minted by [`crate::rehydrate_experience`] from a
/// verified event ledger and content-addressed artifacts. The type deliberately
/// has no public constructor and does not implement `Deserialize`. Rehydration
/// verifies consistency and the recording convention; it does not authenticate
/// the producer or prove that a raw-store writer redacted every field.
///
/// ```compile_fail
/// use hephaestus_experience::TrustedExperience;
///
/// let _ = TrustedExperience {};
/// ```
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TrustedExperience {
    pub(crate) receipt: ExperienceReceipt,
    pub(crate) timestamp_millis: i64,
    pub(crate) fields: BTreeMap<String, String>,
    pub(crate) event_hash: String,
}

impl TrustedExperience {
    #[must_use]
    pub fn experience_id(&self) -> &str {
        &self.receipt.experience_id
    }

    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        &self.receipt.provenance
    }

    #[must_use]
    pub const fn kind(&self) -> ExperienceKind {
        self.receipt.kind
    }

    #[must_use]
    pub const fn confidence_bps(&self) -> u16 {
        self.receipt.confidence_bps
    }

    #[must_use]
    pub const fn timestamp_millis(&self) -> i64 {
        self.timestamp_millis
    }

    #[must_use]
    pub fn source_event_ids(&self) -> &[String] {
        &self.receipt.source_event_ids
    }

    #[must_use]
    pub fn evidence_artifact_ids(&self) -> &[String] {
        &self.receipt.evidence_artifact_ids
    }

    #[must_use]
    pub fn status(&self) -> &str {
        &self.receipt.status
    }

    #[must_use]
    pub const fn fields(&self) -> &BTreeMap<String, String> {
        &self.fields
    }

    #[must_use]
    pub fn event_hash(&self) -> &str {
        &self.event_hash
    }
}

pub(crate) fn validate_identifier(value: &str, error: &'static str) -> Result<(), ExperienceError> {
    if value.trim().is_empty() || value.len() > MAX_IDENTIFIER_BYTES {
        return Err(ExperienceError::InvalidInput(error));
    }
    Ok(())
}

pub(crate) fn validate_fields(fields: &BTreeMap<String, String>) -> Result<(), ExperienceError> {
    if fields.len() > MAX_FIELDS {
        return Err(ExperienceError::InvalidInput("too many structured fields"));
    }
    if fields
        .keys()
        .any(|key| key.trim().is_empty() || key.len() > MAX_FIELD_KEY_BYTES)
    {
        return Err(ExperienceError::InvalidInput("field key is invalid"));
    }
    Ok(())
}
