use std::{collections::BTreeMap, path::Path};

use hephaestus_ledger::{
    ArtifactBackend, ArtifactId, ArtifactStore, EventInput, EventLedger, EventStore, StoredEvent,
};
use serde::Serialize;

use crate::{
    ExperienceError, ExperienceInput, ExperienceReceipt, Provenance, RedactionPolicy, TraceInput,
    TraceReceipt,
};

/// Durable trace retention ceilings enforced before canonical append.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionLimits {
    maximum_records_per_run: usize,
    maximum_record_bytes: usize,
}

impl RetentionLimits {
    /// Creates non-zero trace retention ceilings.
    ///
    /// # Errors
    ///
    /// Rejects a zero record count or byte ceiling.
    pub const fn new(
        maximum_records_per_run: usize,
        maximum_record_bytes: usize,
    ) -> Result<Self, ExperienceError> {
        if maximum_records_per_run == 0 || maximum_record_bytes == 0 {
            return Err(ExperienceError::InvalidInput(
                "retention limits must be non-zero",
            ));
        }
        Ok(Self {
            maximum_records_per_run,
            maximum_record_bytes,
        })
    }
}

/// Single-writer redaction and provenance boundary over the canonical ledger and CAS.
///
/// Holds its stores as boxed trait objects so any [`EventLedger`]/
/// [`ArtifactBackend`] pair can back a recorder, not only the SQLite/CAS
/// backends `open` builds for convenience.
pub struct EvidenceRecorder {
    events: Box<dyn EventLedger + Send>,
    artifacts: Box<dyn ArtifactBackend + Send + Sync>,
    redaction: RedactionPolicy,
    limits: RetentionLimits,
}

impl EvidenceRecorder {
    /// Opens durable evidence storage backed by SQLite and a filesystem CAS.
    ///
    /// # Errors
    ///
    /// Returns a ledger error for invalid or corrupt canonical storage.
    pub fn open(
        database: impl AsRef<Path>,
        artifact_root: impl Into<std::path::PathBuf>,
        redaction: RedactionPolicy,
        limits: RetentionLimits,
    ) -> Result<Self, ExperienceError> {
        Ok(Self {
            events: Box::new(EventStore::open(database)?),
            artifacts: Box::new(ArtifactStore::open(artifact_root)?),
            redaction,
            limits,
        })
    }

    /// Takes ownership of already-open canonical stores of any backend pair.
    #[must_use]
    pub fn from_stores(
        events: Box<dyn EventLedger + Send>,
        artifacts: Box<dyn ArtifactBackend + Send + Sync>,
        redaction: RedactionPolicy,
        limits: RetentionLimits,
    ) -> Self {
        Self {
            events,
            artifacts,
            redaction,
            limits,
        }
    }

    /// Returns the canonical stores to their single-writer composition root.
    #[must_use]
    pub fn into_stores(
        self,
    ) -> (
        Box<dyn EventLedger + Send>,
        Box<dyn ArtifactBackend + Send + Sync>,
    ) {
        (self.events, self.artifacts)
    }

    /// Redacts, bounds, content-addresses, and canonically appends one runtime trace.
    ///
    /// # Errors
    ///
    /// Fails before append when retention or byte ceilings are exceeded.
    pub fn record_trace(&mut self, input: TraceInput) -> Result<TraceReceipt, ExperienceError> {
        self.record_trace_reserving(input, 0)
    }

    pub(crate) fn record_trace_reserving(
        &mut self,
        input: TraceInput,
        reserved_after: usize,
    ) -> Result<TraceReceipt, ExperienceError> {
        let history = self.events.replay_verified()?;
        self.enforce_available(
            &history,
            input.provenance.run_id(),
            reserved_after.saturating_add(1),
        )?;
        let aggregate = format!("run:{}", input.provenance.run_id());
        let redacted = self.redaction.redact(&input.fields);
        let artifact = TraceArtifact {
            schema_version: 1,
            event_id: &input.event_id,
            provenance: &input.provenance,
            kind: input.kind,
            timestamp_millis: input.timestamp_millis,
            fields: &redacted.values,
        };
        let bytes = self.serialize_bounded(&artifact)?;
        let artifact_id = self.artifacts.put(&bytes)?;
        let receipt = TraceReceipt {
            schema_version: 1,
            event_id: input.event_id.clone(),
            provenance: input.provenance,
            kind: input.kind,
            artifact_id: artifact_id.as_str().to_owned(),
            redacted_fields: redacted.redacted,
        };
        self.append_receipt(
            &input.event_id,
            &aggregate,
            "trace.recorded",
            input.timestamp_millis,
            &receipt,
        )?;
        Ok(receipt)
    }

    pub(crate) fn ensure_capacity(
        &self,
        run_id: &str,
        required_records: usize,
    ) -> Result<(), ExperienceError> {
        let history = self.events.replay_verified()?;
        self.enforce_available(&history, run_id, required_records)
    }

    /// Records an unverified structured lesson linked to canonical source events.
    ///
    /// # Errors
    ///
    /// Fails when a source event or evidence artifact is missing/corrupt, or when
    /// the redacted artifact exceeds the configured byte ceiling.
    pub fn record_experience(
        &mut self,
        input: ExperienceInput,
    ) -> Result<ExperienceReceipt, ExperienceError> {
        let history = self.events.replay_verified()?;
        self.enforce_available(&history, input.provenance.run_id(), 1)?;
        let known_events: BTreeMap<_, _> = history
            .iter()
            .map(|event| (&event.event_id, event))
            .collect();
        for source in &input.source_event_ids {
            let event = known_events
                .get(source)
                .ok_or_else(|| ExperienceError::UnknownSourceEvent(source.clone()))?;
            if source_provenance(event)? != input.provenance {
                return Err(ExperienceError::InvalidInput(
                    "source event provenance does not match experience",
                ));
            }
        }
        for evidence in &input.evidence_artifact_ids {
            let id = ArtifactId::parse(evidence)?;
            self.artifacts.get(&id)?;
        }
        let redacted = self.redaction.redact(&input.fields);
        let artifact = ExperienceArtifact {
            schema_version: 1,
            experience_id: &input.experience_id,
            provenance: &input.provenance,
            kind: input.kind,
            timestamp_millis: input.timestamp_millis,
            source_event_ids: &input.source_event_ids,
            evidence_artifact_ids: &input.evidence_artifact_ids,
            confidence_bps: input.confidence_bps,
            fields: &redacted.values,
            status: "unverified",
        };
        let bytes = self.serialize_bounded(&artifact)?;
        let artifact_id = self.artifacts.put(&bytes)?;
        let receipt = ExperienceReceipt {
            schema_version: 1,
            experience_id: input.experience_id.clone(),
            provenance: input.provenance,
            kind: input.kind,
            source_event_ids: input.source_event_ids,
            evidence_artifact_ids: input.evidence_artifact_ids,
            confidence_bps: input.confidence_bps,
            artifact_id: artifact_id.as_str().to_owned(),
            status: "unverified".to_owned(),
            redacted_fields: redacted.redacted,
        };
        self.append_receipt(
            &input.experience_id,
            &format!("experience:{}", input.experience_id),
            "experience.recorded",
            input.timestamp_millis,
            &receipt,
        )?;
        Ok(receipt)
    }

    /// Reads and verifies a receipt's redacted CAS artifact.
    ///
    /// # Errors
    ///
    /// Rejects malformed addresses and detects substituted artifact bytes.
    pub fn artifact(&self, artifact_id: &str) -> Result<Vec<u8>, ExperienceError> {
        Ok(self.artifacts.get(&ArtifactId::parse(artifact_id)?)?)
    }

    /// Returns verified canonical evidence history.
    ///
    /// # Errors
    ///
    /// Fails at the first ledger integrity violation.
    pub fn replay_verified(&self) -> Result<Vec<hephaestus_ledger::StoredEvent>, ExperienceError> {
        Ok(self.events.replay_verified()?)
    }

    fn serialize_bounded(&self, value: &impl Serialize) -> Result<Vec<u8>, ExperienceError> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > self.limits.maximum_record_bytes {
            return Err(ExperienceError::RecordTooLarge {
                actual: bytes.len(),
                maximum: self.limits.maximum_record_bytes,
            });
        }
        Ok(bytes)
    }

    fn enforce_available(
        &self,
        history: &[StoredEvent],
        run_id: &str,
        required_records: usize,
    ) -> Result<(), ExperienceError> {
        let count = history
            .iter()
            .filter_map(|event| source_provenance(event).ok())
            .filter(|provenance| provenance.run_id() == run_id)
            .count();
        if required_records == 0
            || count
                .checked_add(required_records)
                .is_none_or(|required| required > self.limits.maximum_records_per_run)
        {
            return Err(ExperienceError::RetentionExceeded {
                maximum: self.limits.maximum_records_per_run,
            });
        }
        Ok(())
    }

    fn append_receipt(
        &mut self,
        event_id: &str,
        aggregate: &str,
        event_type: &str,
        timestamp_millis: i64,
        receipt: &impl Serialize,
    ) -> Result<(), ExperienceError> {
        let payload = serde_json::to_vec(receipt)?;
        self.events.append(EventInput::new(
            event_id,
            aggregate,
            event_type,
            "experience-plane",
            timestamp_millis,
            payload,
        ))?;
        Ok(())
    }
}

fn source_provenance(event: &StoredEvent) -> Result<Provenance, ExperienceError> {
    match event.event_type.as_str() {
        "trace.recorded" => Ok(serde_json::from_slice::<TraceReceipt>(&event.payload)?.provenance),
        "experience.recorded" => {
            Ok(serde_json::from_slice::<ExperienceReceipt>(&event.payload)?.provenance)
        }
        _ => Err(ExperienceError::InvalidInput(
            "experience source is not a trace or experience record",
        )),
    }
}

#[derive(Serialize)]
struct TraceArtifact<'a> {
    schema_version: u16,
    event_id: &'a str,
    provenance: &'a crate::Provenance,
    kind: crate::TraceKind,
    timestamp_millis: i64,
    fields: &'a std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ExperienceArtifact<'a> {
    schema_version: u16,
    experience_id: &'a str,
    provenance: &'a crate::Provenance,
    kind: crate::ExperienceKind,
    timestamp_millis: i64,
    source_event_ids: &'a [String],
    evidence_artifact_ids: &'a [String],
    confidence_bps: u16,
    fields: &'a std::collections::BTreeMap<String, String>,
    status: &'static str,
}
