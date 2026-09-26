use std::collections::{BTreeMap, BTreeSet};

use hephaestus_ledger::{ArtifactBackend, ArtifactId, EventLedger, StoredEvent};
use serde::{Deserialize, Serialize};

use crate::{
    ExperienceError, ExperienceKind, ExperienceReceipt, Provenance, TraceReceipt,
    TrustedExperience,
    record::{validate_fields, validate_identifier},
};

const EXPERIENCE_EVENT_TYPE: &str = "experience.recorded";
const TRACE_EVENT_TYPE: &str = "trace.recorded";
const EXPERIENCE_ACTOR: &str = "experience-plane";
const STATUS_UNVERIFIED: &str = "unverified";
const MAX_REHYDRATED_EXPERIENCES: usize = 10_000;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredExperienceArtifact {
    schema_version: u16,
    experience_id: String,
    provenance: Provenance,
    kind: ExperienceKind,
    timestamp_millis: i64,
    source_event_ids: Vec<String>,
    evidence_artifact_ids: Vec<String>,
    confidence_bps: u16,
    fields: BTreeMap<String, String>,
    status: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTraceArtifact {
    schema_version: u16,
    event_id: String,
    provenance: Provenance,
    kind: crate::TraceKind,
    timestamp_millis: i64,
    fields: BTreeMap<String, String>,
}

struct ValidatedExperience {
    receipt: ExperienceReceipt,
    timestamp_millis: i64,
    fields: BTreeMap<String, String>,
    event_hash: String,
}

/// Rehydrates one store-consistent experience from verified canonical stores.
///
/// # Errors
///
/// Rejects malformed identities, missing events or artifacts, non-canonical
/// bytes, inconsistent receipt metadata, invalid provenance, and excessive or
/// invalid source graphs.
pub fn rehydrate_experience(
    events: &dyn EventLedger,
    artifacts: &dyn ArtifactBackend,
    experience_id: &str,
) -> Result<TrustedExperience, ExperienceError> {
    validate_identifier(experience_id, "experience ID is invalid")?;
    let history = events.replay_verified()?;
    let by_id = history
        .iter()
        .map(|event| (event.event_id.as_str(), event))
        .collect::<BTreeMap<_, _>>();
    let event = by_id
        .get(experience_id)
        .copied()
        .ok_or_else(|| ExperienceError::UnknownExperience(experience_id.to_owned()))?;
    rehydrate_graph(event, &by_id, artifacts)
}

fn rehydrate_graph(
    target: &StoredEvent,
    history: &BTreeMap<&str, &StoredEvent>,
    artifacts: &dyn ArtifactBackend,
) -> Result<TrustedExperience, ExperienceError> {
    let mut pending = vec![target];
    let mut experiences = BTreeMap::<String, ValidatedExperience>::new();
    let mut traces = BTreeMap::<String, Provenance>::new();

    while let Some(event) = pending.pop() {
        if experiences.contains_key(&event.event_id) {
            continue;
        }
        if experiences.len() >= MAX_REHYDRATED_EXPERIENCES {
            return Err(ExperienceError::InvalidStoredExperience(
                "experience source graph exceeds limit",
            ));
        }
        let validated = validate_experience_event(event, artifacts)?;
        for source_id in &validated.receipt.source_event_ids {
            let source = history
                .get(source_id.as_str())
                .copied()
                .ok_or_else(|| ExperienceError::UnknownSourceEvent(source_id.clone()))?;
            if source.sequence >= event.sequence {
                return Err(ExperienceError::InvalidStoredExperience(
                    "source does not precede experience",
                ));
            }
            match source.event_type.as_str() {
                TRACE_EVENT_TYPE => {
                    if !traces.contains_key(source_id) {
                        traces.insert(source_id.clone(), validate_trace_source(source, artifacts)?);
                    }
                }
                EXPERIENCE_EVENT_TYPE => pending.push(source),
                _ => {
                    return Err(ExperienceError::InvalidStoredExperience(
                        "source is not experience evidence",
                    ));
                }
            }
        }
        experiences.insert(event.event_id.clone(), validated);
    }

    for experience in experiences.values() {
        for source_id in &experience.receipt.source_event_ids {
            let source_provenance = traces.get(source_id).or_else(|| {
                experiences
                    .get(source_id)
                    .map(|source| &source.receipt.provenance)
            });
            let source_provenance = source_provenance.ok_or(
                ExperienceError::InvalidStoredExperience("source validation incomplete"),
            )?;
            if source_provenance != &experience.receipt.provenance {
                return Err(ExperienceError::InvalidStoredExperience(
                    "source provenance mismatch",
                ));
            }
        }
    }

    let validated =
        experiences
            .remove(&target.event_id)
            .ok_or(ExperienceError::InvalidStoredExperience(
                "target experience was not validated",
            ))?;
    Ok(TrustedExperience {
        receipt: validated.receipt,
        timestamp_millis: validated.timestamp_millis,
        fields: validated.fields,
        event_hash: validated.event_hash,
    })
}

fn validate_experience_event(
    event: &StoredEvent,
    artifacts: &dyn ArtifactBackend,
) -> Result<ValidatedExperience, ExperienceError> {
    let receipt: ExperienceReceipt = serde_json::from_slice(&event.payload)?;
    if serde_json::to_vec(&receipt)? != event.payload {
        return Err(ExperienceError::InvalidStoredExperience(
            "receipt is not canonical JSON",
        ));
    }
    validate_experience_metadata(event, &receipt)?;
    let bytes = verified_artifact(artifacts, &receipt.artifact_id)?;
    let artifact: StoredExperienceArtifact = canonical_decode(&bytes)?;
    validate_experience_artifact(event, &receipt, &artifact)?;

    for artifact_id in &receipt.evidence_artifact_ids {
        verified_artifact(artifacts, artifact_id)?;
    }

    Ok(ValidatedExperience {
        receipt,
        timestamp_millis: event.timestamp_millis,
        fields: artifact.fields,
        event_hash: encode_hash(event.hash),
    })
}

fn validate_experience_metadata(
    event: &StoredEvent,
    receipt: &ExperienceReceipt,
) -> Result<(), ExperienceError> {
    validate_identifier(&receipt.experience_id, "experience ID is invalid")?;
    receipt.provenance.validate()?;
    if receipt.schema_version != 1
        || receipt.status != STATUS_UNVERIFIED
        || receipt.source_event_ids.is_empty()
        || receipt
            .source_event_ids
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != receipt.source_event_ids.len()
        || !(1..=10_000).contains(&receipt.confidence_bps)
        || (receipt.kind == ExperienceKind::Contradiction && receipt.source_event_ids.len() < 2)
        || event.event_id != receipt.experience_id
        || event.aggregate_id != format!("experience:{}", receipt.experience_id)
        || event.event_type != EXPERIENCE_EVENT_TYPE
        || event.actor != EXPERIENCE_ACTOR
    {
        return Err(ExperienceError::InvalidStoredExperience(
            "event receipt metadata",
        ));
    }
    for source in &receipt.source_event_ids {
        validate_identifier(source, "source event ID is invalid")?;
    }
    ArtifactId::parse(receipt.artifact_id.clone())?;
    Ok(())
}

fn validate_experience_artifact(
    event: &StoredEvent,
    receipt: &ExperienceReceipt,
    artifact: &StoredExperienceArtifact,
) -> Result<(), ExperienceError> {
    artifact.provenance.validate()?;
    validate_fields(&artifact.fields)?;
    if artifact.schema_version != 1
        || artifact.experience_id != receipt.experience_id
        || artifact.provenance != receipt.provenance
        || artifact.kind != receipt.kind
        || artifact.timestamp_millis != event.timestamp_millis
        || artifact.source_event_ids != receipt.source_event_ids
        || artifact.evidence_artifact_ids != receipt.evidence_artifact_ids
        || artifact.confidence_bps != receipt.confidence_bps
        || artifact.status != receipt.status
        || receipt.redacted_fields > artifact.fields.len()
    {
        return Err(ExperienceError::InvalidStoredExperience(
            "experience artifact mismatch",
        ));
    }
    Ok(())
}

fn validate_trace_source(
    event: &StoredEvent,
    artifacts: &dyn ArtifactBackend,
) -> Result<Provenance, ExperienceError> {
    let receipt: TraceReceipt = serde_json::from_slice(&event.payload)?;
    if serde_json::to_vec(&receipt)? != event.payload {
        return Err(ExperienceError::InvalidStoredExperience(
            "source receipt is not canonical JSON",
        ));
    }
    receipt.provenance.validate()?;
    if receipt.schema_version != 1
        || event.event_id != receipt.event_id
        || event.aggregate_id != format!("run:{}", receipt.provenance.run_id())
        || event.actor != EXPERIENCE_ACTOR
    {
        return Err(ExperienceError::InvalidStoredExperience(
            "trace source metadata",
        ));
    }
    let bytes = verified_artifact(artifacts, &receipt.artifact_id)?;
    let artifact: StoredTraceArtifact = canonical_decode(&bytes)?;
    validate_fields(&artifact.fields)?;
    if artifact.schema_version != 1
        || artifact.event_id != receipt.event_id
        || artifact.provenance != receipt.provenance
        || artifact.kind != receipt.kind
        || artifact.timestamp_millis != event.timestamp_millis
        || receipt.redacted_fields > artifact.fields.len()
    {
        return Err(ExperienceError::InvalidStoredExperience(
            "trace source artifact mismatch",
        ));
    }
    Ok(receipt.provenance)
}

fn verified_artifact(
    artifacts: &dyn ArtifactBackend,
    artifact_id: &str,
) -> Result<Vec<u8>, ExperienceError> {
    Ok(artifacts.get(&ArtifactId::parse(artifact_id.to_owned())?)?)
}

fn canonical_decode<T>(bytes: &[u8]) -> Result<T, ExperienceError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let value: T = serde_json::from_slice(bytes)?;
    if serde_json::to_vec(&value)? != bytes {
        return Err(ExperienceError::InvalidStoredExperience(
            "artifact is not canonical JSON",
        ));
    }
    Ok(value)
}

fn encode_hash(hash: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in hash {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
