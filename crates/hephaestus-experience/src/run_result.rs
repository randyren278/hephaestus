use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hephaestus_ledger::{ArtifactId, EventInput, StoredEvent};
use hephaestus_runtime::{CompletionReason, RunSpec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::ExperienceError;

/// Current schema for canonical runtime result receipts.
pub const RUN_RESULT_SCHEMA_VERSION: u16 = 2;
const RUN_RESULT_EVENT_TYPE: &str = "run.result_recorded";
const RUNTIME_ACTOR: &str = "runtime-plane";
const MAX_RUN_ID_BYTES: usize = 128;
const MAX_TRACE_ARTIFACTS: usize = 1_024;
const MAX_RUN_RESULT_PAYLOAD_BYTES: usize = 131_072;
const MAX_RUN_WALL_MILLIS: u64 = 86_400_000;
const MAX_RUN_LATENCY_MILLIS: u64 = MAX_RUN_WALL_MILLIS;
const MAX_RUN_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RUN_COST_MICROUSD: u64 = 1_000_000_000;
const SIGNED_ENVELOPE_SCHEMA_VERSION: u16 = 1;
const SIGNATURE_HEX_BYTES: usize = 128;
const SIGNING_DOMAIN: &[u8] = b"hephaestus-run-result-attestation-v1\0";
const KEY_ID_DOMAIN: &[u8] = b"hephaestus-runtime-producer-ed25519-v1\0";

/// Stable terminal reasons stored in canonical history and exposed by the API.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunCompletionReason {
    /// The reference runtime completed successfully.
    Success,
    /// The provider reported an ordinary failure.
    ProviderFailure,
    /// The operator interrupted execution.
    OperatorInterrupt,
    /// The wall-clock budget expired.
    WallBudgetExceeded,
    /// The output byte budget was exceeded.
    OutputBudgetExceeded,
    /// Provider input or output could not be delivered durably.
    IoFailure,
}

impl From<CompletionReason> for RunCompletionReason {
    fn from(reason: CompletionReason) -> Self {
        match reason {
            CompletionReason::Success => Self::Success,
            CompletionReason::ProviderFailure => Self::ProviderFailure,
            CompletionReason::OperatorInterrupt => Self::OperatorInterrupt,
            CompletionReason::WallBudgetExceeded => Self::WallBudgetExceeded,
            CompletionReason::OutputBudgetExceeded => Self::OutputBudgetExceeded,
            CompletionReason::IoFailure => Self::IoFailure,
        }
    }
}

/// Versioned, self-validating receipt for one deterministic reference run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunResultReceipt {
    /// Receipt schema version.
    pub schema_version: u16,
    /// Stable path-safe run identity.
    pub run_id: String,
    /// Immutable Genome identity executed by the runtime.
    pub genome_id: String,
    /// Immutable World identity governing the run.
    pub world_id: String,
    /// Exact Git object ID inventoried by the isolated runtime.
    pub source_revision: String,
    /// Stable task identity shared by paired trials.
    pub task_id: String,
    /// BLAKE3 commitment to the exact provider input bytes.
    pub input_commitment: String,
    /// Runtime-owned deterministic seed.
    pub seed: u64,
    /// Exact runtime environment identity.
    pub environment_id: String,
    /// Complete hard budget enforced for this run.
    pub budget: RunBudgetReceipt,
    /// Runtime-owned terminal reason.
    pub completion_reason: RunCompletionReason,
    /// Runtime-owned terminal latency.
    pub latency_millis: u64,
    /// Exact provider-reported cost in micro-US dollars. Zero for the
    /// deterministic reference worker and for a provider stream that reports
    /// none; bounded above by this receipt's own approved budget.
    pub actual_cost_microusd: u64,
    /// CAS address of bounded standard output.
    pub stdout_artifact_id: String,
    /// CAS address of bounded diagnostic output.
    pub stderr_artifact_id: String,
    /// CAS addresses of redacted trace artifacts.
    pub trace_artifact_ids: Vec<String>,
}

/// Stable full run-budget tuple recorded with canonical results.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunBudgetReceipt {
    /// Hard wall deadline in milliseconds.
    pub wall_millis: u64,
    /// Maximum persisted stdout plus stderr bytes.
    pub maximum_output_bytes: u64,
    /// Maximum provider spend in micro-US dollars.
    pub maximum_cost_microusd: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedRunResultEnvelope {
    schema_version: u16,
    producer_key_id: String,
    claims: RunResultReceipt,
    signature: String,
}

/// Runtime-result producer authority held only by the trusted daemon composition root.
pub struct RunResultSigner(SigningKey);

impl RunResultSigner {
    /// Creates a deterministic signer from a secret 32-byte Ed25519 seed.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }

    /// Returns the non-secret verifier paired with this signer.
    #[must_use]
    pub fn verifier(&self) -> RunResultVerifier {
        RunResultVerifier::from_verifying_key(self.0.verifying_key())
    }

    /// Signs claims and creates their complete canonical ledger event.
    ///
    /// # Errors
    ///
    /// Rejects invalid claims or canonical JSON serialization failures.
    pub fn issue(
        &self,
        claims: RunResultReceipt,
        timestamp_millis: i64,
    ) -> Result<EventInput, ExperienceError> {
        claims.validate()?;
        let event_id = claims.event_id();
        let aggregate_id = claims.aggregate_id();
        let verifier = self.verifier();
        let message = signing_preimage(
            verifier.key_id(),
            &event_id,
            &aggregate_id,
            timestamp_millis,
            &claims,
        );
        let signature = self.0.sign(&message);
        let payload = serde_json::to_vec(&SignedRunResultEnvelope {
            schema_version: SIGNED_ENVELOPE_SCHEMA_VERSION,
            producer_key_id: verifier.key_id().to_owned(),
            claims,
            signature: encode_hex(&signature.to_bytes()),
        })?;
        Ok(EventInput::new(
            event_id,
            aggregate_id,
            RUN_RESULT_EVENT_TYPE,
            RUNTIME_ACTOR,
            timestamp_millis,
            payload,
        ))
    }
}

/// Public-key trust anchor used to authenticate canonical runtime-result events.
///
/// Remembers each event it has already authenticated, keyed by a digest of
/// every event field it checks, so a daemon that re-derives its projection
/// after each append does not re-verify every earlier Ed25519 signature.
#[derive(Clone, Debug)]
pub struct RunResultVerifier {
    key: VerifyingKey,
    key_id: String,
    verified: Arc<Mutex<HashMap<[u8; 32], RunResultReceipt>>>,
}

impl PartialEq for RunResultVerifier {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.key_id == other.key_id
    }
}

impl Eq for RunResultVerifier {}

impl RunResultVerifier {
    /// Constructs a verifier from canonical Ed25519 public-key bytes.
    ///
    /// # Errors
    ///
    /// Rejects bytes that are not a valid compressed Edwards point.
    pub fn from_public_key_bytes(bytes: [u8; 32]) -> Result<Self, ExperienceError> {
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| ExperienceError::InvalidInput("runtime producer public key is invalid"))?;
        Ok(Self::from_verifying_key(key))
    }

    fn from_verifying_key(key: VerifyingKey) -> Self {
        let key_id = producer_key_id(&key.to_bytes());
        Self {
            key,
            key_id,
            verified: Arc::default(),
        }
    }

    /// Returns the exact non-secret Ed25519 public key.
    #[must_use]
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    /// Returns the domain-separated identity of the anchored producer key.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Parses and authenticates one canonical runtime-result event.
    ///
    /// # Errors
    ///
    /// Rejects oversized or malformed payloads, a different producer key, invalid claims,
    /// non-canonical event metadata, and invalid Ed25519 signatures.
    pub fn verify_event(&self, event: &StoredEvent) -> Result<RunResultReceipt, ExperienceError> {
        let digest = verified_event_digest(event);
        if let Some(receipt) = self
            .verified
            .lock()
            .map_err(|_| ExperienceError::InvalidInput("run result verifier cache is poisoned"))?
            .get(&digest)
        {
            return Ok(receipt.clone());
        }
        let receipt = self.verify_event_uncached(event)?;
        self.verified
            .lock()
            .map_err(|_| ExperienceError::InvalidInput("run result verifier cache is poisoned"))?
            .insert(digest, receipt.clone());
        Ok(receipt)
    }

    fn verify_event_uncached(
        &self,
        event: &StoredEvent,
    ) -> Result<RunResultReceipt, ExperienceError> {
        if event.payload.len() > MAX_RUN_RESULT_PAYLOAD_BYTES {
            return Err(ExperienceError::RecordTooLarge {
                actual: event.payload.len(),
                maximum: MAX_RUN_RESULT_PAYLOAD_BYTES,
            });
        }
        let envelope: SignedRunResultEnvelope = serde_json::from_slice(&event.payload)?;
        if envelope.schema_version != SIGNED_ENVELOPE_SCHEMA_VERSION
            || envelope.producer_key_id != self.key_id
        {
            return Err(ExperienceError::InvalidInput(
                "run result producer attestation is not trusted",
            ));
        }
        envelope.claims.validate()?;
        if event.event_type != RUN_RESULT_EVENT_TYPE
            || event.actor != RUNTIME_ACTOR
            || event.event_id != envelope.claims.event_id()
            || event.aggregate_id != envelope.claims.aggregate_id()
        {
            return Err(ExperienceError::InvalidInput(
                "run result crossed its provenance boundary",
            ));
        }
        let signature_bytes = decode_signature(&envelope.signature)?;
        let signature = Signature::from_bytes(&signature_bytes);
        let message = signing_preimage(
            &envelope.producer_key_id,
            &event.event_id,
            &event.aggregate_id,
            event.timestamp_millis,
            &envelope.claims,
        );
        self.key
            .verify_strict(&message, &signature)
            .map_err(|_| ExperienceError::InvalidInput("run result signature is invalid"))?;
        Ok(envelope.claims)
    }
}

/// Digest of every event field `verify_event_uncached` reads. It is computed
/// from the event's own bytes, never from its stored chain hash, so an event
/// whose payload or metadata was edited misses the cache and is re-verified.
fn verified_event_digest(event: &StoredEvent) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"hephaestus-run-result-verified-v1\0");
    for field in [
        event.event_type.as_bytes(),
        event.actor.as_bytes(),
        event.event_id.as_bytes(),
        event.aggregate_id.as_bytes(),
        event.payload.as_slice(),
    ] {
        hasher.update(&(field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    hasher.update(&event.timestamp_millis.to_be_bytes());
    *hasher.finalize().as_bytes()
}

impl RunResultReceipt {
    /// Builds a canonical receipt by copying immutable context from its run specification.
    ///
    /// # Errors
    ///
    /// Rejects out-of-range runtime observations or malformed artifact identities.
    #[allow(clippy::too_many_arguments)]
    pub fn from_run_spec(
        spec: &RunSpec,
        completion_reason: RunCompletionReason,
        latency_millis: u64,
        actual_cost_microusd: u64,
        stdout_artifact_id: impl Into<String>,
        stderr_artifact_id: impl Into<String>,
        trace_artifact_ids: Vec<String>,
    ) -> Result<Self, ExperienceError> {
        let wall_millis = u64::try_from(spec.budget().wall().as_millis()).map_err(|_| {
            ExperienceError::InvalidInput("run result wall budget exceeds schema range")
        })?;
        let maximum_output_bytes = spec.budget().maximum_output_bytes() as u64;
        let receipt = Self {
            schema_version: RUN_RESULT_SCHEMA_VERSION,
            run_id: spec.run_id().to_owned(),
            genome_id: spec.genome_id().to_owned(),
            world_id: spec.world_id().to_owned(),
            source_revision: spec.source_revision().to_owned(),
            task_id: spec.experiment().task_id().to_owned(),
            input_commitment: spec.experiment().input_commitment().to_owned(),
            seed: spec.experiment().seed(),
            environment_id: spec.experiment().environment_id().to_owned(),
            budget: RunBudgetReceipt {
                wall_millis,
                maximum_output_bytes,
                maximum_cost_microusd: spec.budget().maximum_cost_microusd(),
            },
            completion_reason,
            latency_millis,
            actual_cost_microusd,
            stdout_artifact_id: stdout_artifact_id.into(),
            stderr_artifact_id: stderr_artifact_id.into(),
            trace_artifact_ids,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    /// Parses and authenticates a receipt directly from one canonical stored event.
    ///
    /// # Errors
    ///
    /// Rejects unauthenticated or malformed payloads and non-canonical event metadata.
    pub fn parse_from_event(
        event: &StoredEvent,
        verifier: &RunResultVerifier,
    ) -> Result<Self, ExperienceError> {
        verifier.verify_event(event)
    }

    /// Canonical event identifier derived from the run identity.
    #[must_use]
    pub fn event_id(&self) -> String {
        format!("result:{}", self.run_id)
    }

    /// Canonical run aggregate identifier.
    #[must_use]
    pub fn aggregate_id(&self) -> String {
        format!("run:{}", self.run_id)
    }

    fn validate(&self) -> Result<(), ExperienceError> {
        if self.schema_version != RUN_RESULT_SCHEMA_VERSION {
            return Err(ExperienceError::InvalidInput(
                "unsupported run result schema version",
            ));
        }
        validate_run_id(&self.run_id)?;
        validate_content_id(&self.genome_id, "genome")?;
        validate_content_id(&self.world_id, "world")?;
        validate_source_revision(&self.source_revision)?;
        validate_bounded_id(&self.task_id, "run result task_id is invalid")?;
        validate_artifact_id(&self.input_commitment)?;
        validate_bounded_id(&self.environment_id, "run result environment_id is invalid")?;
        self.budget.validate()?;
        if self.actual_cost_microusd > self.budget.maximum_cost_microusd {
            return Err(ExperienceError::InvalidInput(
                "run result cost exceeds its own approved budget",
            ));
        }
        if self.latency_millis > MAX_RUN_LATENCY_MILLIS {
            return Err(ExperienceError::InvalidInput(
                "run result latency exceeds the canonical runtime bound",
            ));
        }
        if !matches!(
            self.completion_reason,
            RunCompletionReason::Success
                | RunCompletionReason::ProviderFailure
                | RunCompletionReason::OperatorInterrupt
                | RunCompletionReason::OutputBudgetExceeded
                | RunCompletionReason::WallBudgetExceeded
                | RunCompletionReason::IoFailure
        ) {
            return Err(ExperienceError::InvalidInput(
                "run result has an unsupported completion reason",
            ));
        }
        if self.trace_artifact_ids.len() > MAX_TRACE_ARTIFACTS {
            return Err(ExperienceError::InvalidInput(
                "run result contains too many trace artifacts",
            ));
        }
        validate_artifact_id(&self.stdout_artifact_id)?;
        validate_artifact_id(&self.stderr_artifact_id)?;
        for artifact_id in &self.trace_artifact_ids {
            validate_artifact_id(artifact_id)?;
        }
        Ok(())
    }
}

impl RunBudgetReceipt {
    fn validate(self) -> Result<(), ExperienceError> {
        if self.wall_millis == 0 || self.wall_millis > MAX_RUN_WALL_MILLIS {
            return Err(ExperienceError::InvalidInput(
                "run result wall budget is invalid",
            ));
        }
        if self.maximum_output_bytes == 0 || self.maximum_output_bytes > MAX_RUN_OUTPUT_BYTES {
            return Err(ExperienceError::InvalidInput(
                "run result output budget is invalid",
            ));
        }
        if self.maximum_cost_microusd > MAX_RUN_COST_MICROUSD {
            return Err(ExperienceError::InvalidInput(
                "run result cost budget is invalid",
            ));
        }
        Ok(())
    }
}

fn validate_run_id(run_id: &str) -> Result<(), ExperienceError> {
    if run_id.is_empty()
        || run_id.len() > MAX_RUN_ID_BYTES
        || !run_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ExperienceError::InvalidInput(
            "run result run_id is not path safe",
        ));
    }
    Ok(())
}

fn validate_content_id(value: &str, namespace: &'static str) -> Result<(), ExperienceError> {
    let prefix = format!("hephaestus:{namespace}:");
    let hash = value
        .strip_prefix(&prefix)
        .ok_or(ExperienceError::InvalidInput(
            "run result content identity is malformed",
        ))?;
    validate_artifact_id(hash)
}

fn validate_bounded_id(value: &str, error: &'static str) -> Result<(), ExperienceError> {
    if value.is_empty()
        || value.len() > MAX_RUN_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
    {
        return Err(ExperienceError::InvalidInput(error));
    }
    Ok(())
}

fn validate_source_revision(revision: &str) -> Result<(), ExperienceError> {
    if !matches!(revision.len(), 40 | 64)
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ExperienceError::InvalidInput(
            "run result source revision is not a canonical object ID",
        ));
    }
    Ok(())
}

fn validate_artifact_id(value: &str) -> Result<(), ExperienceError> {
    ArtifactId::parse(value.to_owned())?;
    Ok(())
}

fn producer_key_id(public_key: &[u8; 32]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(KEY_ID_DOMAIN);
    hasher.update(public_key);
    hasher.finalize().to_hex().to_string()
}

fn signing_preimage(
    producer_key_id: &str,
    event_id: &str,
    aggregate_id: &str,
    timestamp_millis: i64,
    claims: &RunResultReceipt,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SIGNING_DOMAIN);
    bytes.extend_from_slice(&SIGNED_ENVELOPE_SCHEMA_VERSION.to_be_bytes());
    push_bytes(&mut bytes, producer_key_id.as_bytes());
    push_bytes(&mut bytes, event_id.as_bytes());
    push_bytes(&mut bytes, aggregate_id.as_bytes());
    push_bytes(&mut bytes, RUN_RESULT_EVENT_TYPE.as_bytes());
    push_bytes(&mut bytes, RUNTIME_ACTOR.as_bytes());
    bytes.extend_from_slice(&timestamp_millis.to_be_bytes());
    bytes.extend_from_slice(&claims.schema_version.to_be_bytes());
    for value in [
        &claims.run_id,
        &claims.genome_id,
        &claims.world_id,
        &claims.source_revision,
        &claims.task_id,
        &claims.input_commitment,
    ] {
        push_bytes(&mut bytes, value.as_bytes());
    }
    bytes.extend_from_slice(&claims.seed.to_be_bytes());
    push_bytes(&mut bytes, claims.environment_id.as_bytes());
    bytes.extend_from_slice(&claims.budget.wall_millis.to_be_bytes());
    bytes.extend_from_slice(&claims.budget.maximum_output_bytes.to_be_bytes());
    bytes.extend_from_slice(&claims.budget.maximum_cost_microusd.to_be_bytes());
    bytes.push(completion_reason_tag(claims.completion_reason));
    bytes.extend_from_slice(&claims.latency_millis.to_be_bytes());
    bytes.extend_from_slice(&claims.actual_cost_microusd.to_be_bytes());
    push_bytes(&mut bytes, claims.stdout_artifact_id.as_bytes());
    push_bytes(&mut bytes, claims.stderr_artifact_id.as_bytes());
    let trace_count = u64::try_from(claims.trace_artifact_ids.len())
        .expect("trace artifact count always fits u64");
    bytes.extend_from_slice(&trace_count.to_be_bytes());
    for artifact_id in &claims.trace_artifact_ids {
        push_bytes(&mut bytes, artifact_id.as_bytes());
    }
    bytes
}

fn completion_reason_tag(reason: RunCompletionReason) -> u8 {
    match reason {
        RunCompletionReason::Success => 0,
        RunCompletionReason::ProviderFailure => 1,
        RunCompletionReason::OperatorInterrupt => 2,
        RunCompletionReason::WallBudgetExceeded => 3,
        RunCompletionReason::OutputBudgetExceeded => 4,
        RunCompletionReason::IoFailure => 5,
    }
}

fn push_bytes(target: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("field length always fits u64");
    target.extend_from_slice(&length.to_be_bytes());
    target.extend_from_slice(value);
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_signature(value: &str) -> Result<[u8; 64], ExperienceError> {
    if value.len() != SIGNATURE_HEX_BYTES {
        return Err(ExperienceError::InvalidInput(
            "run result signature is malformed",
        ));
    }
    let mut decoded = [0_u8; 64];
    for (destination, pair) in decoded.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *destination = (decode_nibble(pair[0])? << 4) | decode_nibble(pair[1])?;
    }
    Ok(decoded)
}

fn decode_nibble(value: u8) -> Result<u8, ExperienceError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(ExperienceError::InvalidInput(
            "run result signature is malformed",
        )),
    }
}
