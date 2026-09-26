//! World-bound, operator-only reference-output invariant receipts.

use std::collections::BTreeMap;

use hephaestus_experience::{RunCompletionReason, RunResultReceipt, RunResultVerifier};
use hephaestus_genome::CompiledWorld;
use hephaestus_ledger::{ArtifactId, EventIndex, EventInput, StoredEvent};
use hephaestus_runtime::ExperimentContext;
use serde::{Deserialize, Serialize};

use crate::{
    ArenaError, ArtifactStore, EvaluationStores, OperatorReceipt, load_operator_evaluation,
    load_operator_evaluation_in, run_result_verifier, verify_operator_artifact,
};

const MANIFEST_KEY: &str = "arena.invariant_manifest";
const MANIFEST_ALGORITHM: &str = "reference-output-invariants-v1";
const RECEIPT_SCHEMA_VERSION: u16 = 1;
const EVENT_TYPE: &str = "invariants.recorded";
const EVENT_ACTOR: &str = "arena-plane";

/// Aggregate results for one invariant predicate across authenticated paired trials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvariantPredicateResult {
    /// Stable predicate name (`successful_terminal`, `maximum_output_bytes`, or `forbidden_ascii_byte`).
    pub predicate: String,
    /// Present only for a forbidden-byte predicate.
    pub forbidden_ascii_byte: Option<u8>,
    /// Parent trial outputs violating this predicate.
    pub parent_violations: u32,
    /// Candidate trial outputs violating this predicate.
    pub candidate_violations: u32,
    /// Paired tasks where the parent passed and the candidate violated this predicate.
    pub paired_regressions: u32,
}

/// Canonical aggregate receipt recomputed from authenticated paired trial evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvariantReceipt {
    /// Receipt schema version.
    pub schema_version: u16,
    /// Deterministic checker algorithm.
    pub algorithm: String,
    /// Stable source evaluation identity.
    pub evaluation_id: String,
    /// Exact source evaluation event identity and hash.
    pub evaluation_event_id: String,
    pub evaluation_event_hash: String,
    /// Immutable World identity bound to the checker manifest.
    pub world_id: String,
    /// Parent Genome identity in the source paired evaluation.
    pub parent_genome_id: String,
    /// Candidate Genome identity in the source paired evaluation.
    pub candidate_genome_id: String,
    /// World-bound canonical invariant manifest artifact.
    pub manifest_artifact_id: String,
    /// Authenticated parent submission evidence artifact.
    pub parent_submission_artifact_id: String,
    /// Authenticated candidate submission evidence artifact.
    pub candidate_submission_artifact_id: String,
    /// Number of paired task outputs evaluated.
    pub total_evaluated_trials: u32,
    /// Number of individual parent/candidate predicate checks.
    pub total_checks: u32,
    /// Predicate aggregates in manifest order.
    pub predicates: Vec<InvariantPredicateResult>,
    /// Sum of candidate violations across all predicates and trials.
    pub total_candidate_violations: u32,
    /// Sum of paired regressions across all predicates and trials.
    pub total_paired_regressions: u32,
    /// World policy ceiling for paired regressions.
    pub maximum_regressions: u32,
    /// Sum of paired regressions across all predicates is within the World policy.
    pub regressions_within_budget: bool,
    /// True exactly when candidate outputs have no invariant violations.
    pub candidate_contract_satisfied: bool,
}

/// Canonical metadata for one durable invariant receipt event.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InvariantEvent {
    /// Canonical global event sequence.
    pub sequence: u64,
    /// Deterministic event identity.
    pub event_id: String,
    /// Evaluation-bound invariant aggregate identity.
    pub aggregate_id: String,
    /// Durable event type.
    pub event_type: String,
    /// Fixed trusted writer identity.
    pub actor: String,
    /// Event-chain hash in canonical lowercase hex.
    pub event_hash: String,
    /// Content address of canonical `InvariantReceipt` bytes.
    pub receipt_artifact_id: String,
}

/// Operator-side trusted invariant result retaining evaluator store capability.
pub struct OperatorInvariantCheck {
    receipt: InvariantReceipt,
    event: InvariantEvent,
    stores: EvaluationStores,
}

impl OperatorInvariantCheck {
    /// Canonical aggregate result without task identities or raw outputs.
    #[must_use]
    pub const fn receipt(&self) -> &InvariantReceipt {
        &self.receipt
    }

    /// Canonical event metadata binding the receipt into the ledger.
    #[must_use]
    pub const fn event(&self) -> &InvariantEvent {
        &self.event
    }

    /// Returns the evaluator-owned stores for the next trusted operation.
    #[must_use]
    pub fn into_stores(self) -> EvaluationStores {
        self.stores
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct InvariantManifest {
    schema_version: u16,
    algorithm: String,
    maximum_output_bytes: usize,
    forbidden_ascii_bytes: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct InvariantEventPayload {
    schema_version: u16,
    evaluation_id: String,
    world_id: String,
    receipt_artifact_id: String,
}

pub(crate) struct VerifiedTrialOutput {
    pub(crate) completion_reason: RunCompletionReason,
    pub(crate) stdout: Vec<u8>,
}

/// Checks authenticated outputs and durably records one deterministic receipt.
/// An exact retry recomputes and returns the prior event and receipt.
///
/// # Errors
///
/// Rejects an unknown evaluation, absent or malformed World manifest, unverified
/// run evidence, a World mismatch, or a conflicting deterministic retry.
pub fn check_reference_output_invariants(
    stores: EvaluationStores,
    evaluation_id: &str,
    world: &CompiledWorld,
    timestamp_millis: i64,
) -> Result<OperatorInvariantCheck, ArenaError> {
    check(stores, evaluation_id, world, timestamp_millis, true)
}

/// Recomputes an existing invariant receipt without appending an event.
///
/// # Errors
///
/// Fails when the receipt is absent, noncanonical, tampered, or inconsistent
/// with authenticated source evaluation evidence.
pub fn load_reference_output_invariants(
    stores: EvaluationStores,
    evaluation_id: &str,
    world: &CompiledWorld,
) -> Result<OperatorInvariantCheck, ArenaError> {
    check(stores, evaluation_id, world, 0, false)
}

/// Verifies a supplied durable invariant event against canonical history and recomputed evidence.
///
/// # Errors
///
/// Rejects a malformed event envelope, a noncanonical hash-chain snapshot, or
/// receipt content that does not recompute from the signed paired trial evidence.
pub fn verify_reference_output_invariant_event(
    stores: EvaluationStores,
    event: &StoredEvent,
    world: &CompiledWorld,
) -> Result<OperatorInvariantCheck, ArenaError> {
    let (evaluation_id, world_id) = invariant_event_references(event)?;
    if world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch(MANIFEST_KEY));
    }
    let check = load_reference_output_invariants(stores, &evaluation_id, world)?;
    let canonical = check
        .stores
        .events
        .replay_verified()?
        .into_iter()
        .find(|stored| stored.event_id == event.event_id)
        .ok_or(ArenaError::InvalidInvariantEvent)?;
    if canonical != *event {
        return Err(ArenaError::InvalidInvariantEvent);
    }
    Ok(check)
}

/// Read-only, history-borrowing counterpart to [`OperatorInvariantCheck`],
/// with no evaluator-store capability of its own. Produced by
/// [`verify_reference_output_invariant_event_in`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvariantView {
    receipt: InvariantReceipt,
    event: InvariantEvent,
}

impl InvariantView {
    /// Canonical aggregate result without task identities or raw outputs.
    #[must_use]
    pub const fn receipt(&self) -> &InvariantReceipt {
        &self.receipt
    }

    /// Canonical event metadata binding the receipt into the ledger.
    #[must_use]
    pub const fn event(&self) -> &InvariantEvent {
        &self.event
    }
}

/// History-borrowing counterpart to [`verify_reference_output_invariant_event`]:
/// verifies against a caller-supplied history index and artifact store
/// instead of opening fresh evaluator-owned stores and replaying the ledger
/// again. See [`crate::load_operator_evaluation_in`] for the trust
/// requirement on `index` and why callers verifying many events should build
/// it once.
///
/// # Errors
///
/// Rejects a malformed event envelope, a noncanonical hash-chain snapshot, or
/// receipt content that does not recompute from the signed paired trial evidence.
pub fn verify_reference_output_invariant_event_in(
    index: &EventIndex<'_>,
    artifacts: &ArtifactStore,
    event: &StoredEvent,
    world: &CompiledWorld,
) -> Result<InvariantView, ArenaError> {
    let (evaluation_id, world_id) = invariant_event_references(event)?;
    if world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch(MANIFEST_KEY));
    }
    let operator = load_operator_evaluation_in(index, artifacts, &evaluation_id)?;
    if operator.operator_receipt.world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch(MANIFEST_KEY));
    }
    let evaluation_event = index
        .get(&operator.recorded.event.event_id)
        .ok_or_else(|| ArenaError::UnknownEvaluation(evaluation_id.clone()))?;
    if super::encode_hash(evaluation_event.hash) != super::encode_hash(operator.event_hash) {
        return Err(ArenaError::InvalidStoredReceipt(
            "invariant evaluation event",
        ));
    }
    let (manifest, manifest_artifact_id) = load_invariant_manifest(world, artifacts)?;
    let receipt = compute_receipt(
        &operator.operator_receipt,
        artifacts,
        index,
        evaluation_event,
        world,
        &manifest,
        &manifest_artifact_id,
    )?;
    let event_id = invariant_event_id(&evaluation_id);
    let stored = index
        .get(&event_id)
        .ok_or_else(|| ArenaError::UnknownInvariantCheck(evaluation_id.clone()))?;
    if stored != event {
        return Err(ArenaError::InvalidInvariantEvent);
    }
    if stored.sequence <= evaluation_event.sequence {
        return Err(ArenaError::InvariantConflict(evaluation_id));
    }
    let rehydrated = rehydrate_event(artifacts, stored, &receipt)?;
    Ok(InvariantView {
        receipt,
        event: invariant_event(stored, &rehydrated.receipt_artifact_id),
    })
}

fn check(
    stores: EvaluationStores,
    evaluation_id: &str,
    world: &CompiledWorld,
    timestamp_millis: i64,
    append_missing: bool,
) -> Result<OperatorInvariantCheck, ArenaError> {
    let mut operator = load_operator_evaluation(stores, evaluation_id)?;
    if operator.operator_receipt.world_id != world.id() {
        return Err(ArenaError::WorldArtifactMismatch(MANIFEST_KEY));
    }
    let history = operator.stores.events.replay_verified()?;
    let evaluation_event = history
        .iter()
        .find(|event| event.event_id == operator.recorded.event.event_id)
        .ok_or_else(|| ArenaError::UnknownEvaluation(evaluation_id.to_owned()))?;
    if super::encode_hash(evaluation_event.hash) != super::encode_hash(operator.event_hash) {
        return Err(ArenaError::InvalidStoredReceipt(
            "invariant evaluation event",
        ));
    }
    let (manifest, manifest_artifact_id) =
        load_invariant_manifest(world, &operator.stores.artifacts)?;
    let index = EventIndex::build(&history);
    let receipt = compute_receipt(
        &operator.operator_receipt,
        &operator.stores.artifacts,
        &index,
        evaluation_event,
        world,
        &manifest,
        &manifest_artifact_id,
    )?;
    let event_id = invariant_event_id(evaluation_id);
    if let Some(event) = history.iter().find(|event| event.event_id == event_id) {
        let rehydrated = rehydrate_event(&operator.stores.artifacts, event, &receipt)?;
        if event.sequence <= evaluation_event.sequence {
            return Err(ArenaError::InvariantConflict(evaluation_id.to_owned()));
        }
        return Ok(OperatorInvariantCheck {
            receipt,
            event: invariant_event(event, &rehydrated.receipt_artifact_id),
            stores: operator.into_stores(),
        });
    }
    if !append_missing {
        return Err(ArenaError::UnknownInvariantCheck(evaluation_id.to_owned()));
    }
    let receipt_bytes = serde_json::to_vec(&receipt)?;
    let receipt_artifact_id = operator.stores.artifacts.put(&receipt_bytes)?;
    let payload = InvariantEventPayload {
        schema_version: RECEIPT_SCHEMA_VERSION,
        evaluation_id: evaluation_id.to_owned(),
        world_id: world.id().to_owned(),
        receipt_artifact_id: receipt_artifact_id.as_str().to_owned(),
    };
    let event = operator.stores.events.append(EventInput::new(
        event_id,
        invariant_aggregate_id(evaluation_id),
        EVENT_TYPE,
        EVENT_ACTOR,
        timestamp_millis,
        serde_json::to_vec(&payload)?,
    ))?;
    Ok(OperatorInvariantCheck {
        receipt,
        event: invariant_event(&event, receipt_artifact_id.as_str()),
        stores: operator.into_stores(),
    })
}

fn compute_receipt(
    receipt: &OperatorReceipt,
    artifacts: &ArtifactStore,
    index: &EventIndex<'_>,
    evaluation_event: &StoredEvent,
    world: &CompiledWorld,
    manifest: &InvariantManifest,
    manifest_artifact_id: &str,
) -> Result<InvariantReceipt, ArenaError> {
    let visible_bytes = verify_operator_artifact(artifacts, &receipt.visible_manifest_artifact_id)?;
    let sealed_bytes = verify_operator_artifact(artifacts, &receipt.sealed_manifest_artifact_id)?;
    let visible =
        super::TrustedManifest::from_canonical_bytes(&visible_bytes, super::Visibility::Visible)?;
    let sealed =
        super::TrustedManifest::from_canonical_bytes(&sealed_bytes, super::Visibility::Sealed)?;
    let mut task_inputs = BTreeMap::new();
    for task in visible.tasks.iter().chain(&sealed.tasks) {
        if task_inputs
            .insert(task.task_id.clone(), task.input.clone())
            .is_some()
        {
            return Err(ArenaError::InvalidStoredReceipt("invariant task set"));
        }
    }
    let parent = verified_submission_outputs(
        artifacts,
        index,
        receipt,
        &receipt.parent_submission_artifact_id,
        &receipt.parent_genome_id,
        &task_inputs,
        world,
        evaluation_event,
    )?;
    let candidate = verified_submission_outputs(
        artifacts,
        index,
        receipt,
        &receipt.candidate_submission_artifact_id,
        &receipt.candidate_genome_id,
        &task_inputs,
        world,
        evaluation_event,
    )?;
    let mut predicates = Vec::with_capacity(2 + manifest.forbidden_ascii_bytes.len());
    predicates.push(aggregate_predicate(
        "successful_terminal",
        None,
        &parent,
        &candidate,
        |trial| trial.completion_reason == RunCompletionReason::Success,
    )?);
    predicates.push(aggregate_predicate(
        "maximum_output_bytes",
        None,
        &parent,
        &candidate,
        |trial| trial.stdout.len() <= manifest.maximum_output_bytes,
    )?);
    for byte in &manifest.forbidden_ascii_bytes {
        predicates.push(aggregate_predicate(
            "forbidden_ascii_byte",
            Some(*byte),
            &parent,
            &candidate,
            |trial| !trial.stdout.contains(byte),
        )?);
    }
    let total_evaluated_trials =
        u32::try_from(task_inputs.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let predicate_count = u32::try_from(predicates.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let total_checks = total_evaluated_trials
        .checked_mul(predicate_count)
        .and_then(|count| count.checked_mul(2))
        .ok_or(ArenaError::MetricOverflow("invariant checks"))?;
    let total_candidate_violations = predicates.iter().try_fold(0_u32, |total, predicate| {
        total
            .checked_add(predicate.candidate_violations)
            .ok_or(ArenaError::MetricOverflow("candidate invariant violations"))
    })?;
    let total_paired_regressions = predicates.iter().try_fold(0_u32, |total, predicate| {
        total
            .checked_add(predicate.paired_regressions)
            .ok_or(ArenaError::MetricOverflow("paired invariant regressions"))
    })?;
    let maximum_regressions = world.evaluation_policy().maximum_regressions();
    Ok(InvariantReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        algorithm: MANIFEST_ALGORITHM.to_owned(),
        evaluation_id: receipt.evaluation_id.clone(),
        evaluation_event_id: evaluation_event.event_id.clone(),
        evaluation_event_hash: super::encode_hash(evaluation_event.hash),
        world_id: receipt.world_id.clone(),
        parent_genome_id: receipt.parent_genome_id.clone(),
        candidate_genome_id: receipt.candidate_genome_id.clone(),
        manifest_artifact_id: manifest_artifact_id.to_owned(),
        parent_submission_artifact_id: receipt.parent_submission_artifact_id.clone(),
        candidate_submission_artifact_id: receipt.candidate_submission_artifact_id.clone(),
        total_evaluated_trials,
        total_checks,
        predicates,
        total_candidate_violations,
        total_paired_regressions,
        maximum_regressions,
        regressions_within_budget: total_paired_regressions <= maximum_regressions,
        candidate_contract_satisfied: total_candidate_violations == 0,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn verified_submission_outputs(
    artifacts: &ArtifactStore,
    index: &EventIndex<'_>,
    operator_receipt: &OperatorReceipt,
    submission_artifact_id: &str,
    genome_id: &str,
    task_inputs: &BTreeMap<String, String>,
    world: &CompiledWorld,
    evaluation_event: &StoredEvent,
) -> Result<BTreeMap<String, VerifiedTrialOutput>, ArenaError> {
    let bytes = verify_operator_artifact(artifacts, submission_artifact_id)?;
    let submission: super::SubmissionEvidence = serde_json::from_slice(&bytes)?;
    if submission.schema_version != 1
        || serde_json::to_vec(&submission)? != bytes
        || submission.genome_id != genome_id
        || submission.trials.keys().ne(task_inputs.keys())
    {
        return Err(ArenaError::InvalidStoredReceipt(
            "invariant submission evidence",
        ));
    }
    let verifier = run_result_verifier(world, artifacts)?;
    submission
        .trials
        .iter()
        .map(|(task_id, trial)| {
            let event = index
                .get(trial.run_result_event_id.as_str())
                .ok_or_else(|| ArenaError::UnknownRunEvent(trial.run_result_event_id.clone()))?;
            if super::encode_hash(event.hash) != trial.run_result_event_hash
                || event.sequence >= evaluation_event.sequence
            {
                return Err(ArenaError::InvalidStoredReceipt(
                    "invariant run event binding",
                ));
            }
            let run = verify_signed_run_result(event, &verifier)?;
            let experiment = ExperimentContext::new(
                task_id,
                &task_inputs[task_id],
                operator_receipt.seed,
                &operator_receipt.environment_id,
            )?;
            if run.event_id() != trial.run_result_event_id
                || run.world_id != world.id()
                || run.genome_id != genome_id
                || run.task_id != *task_id
                || run.input_commitment != experiment.input_commitment()
                || run.seed != operator_receipt.seed
                || run.environment_id != operator_receipt.environment_id
                || run.budget != operator_receipt.budget
                || run.completion_reason != trial.completion_reason
                || run.latency_millis != trial.latency_millis
                || run.actual_cost_microusd != trial.actual_cost_microusd
                || run.stdout_artifact_id != trial.stdout_artifact_id
                || run.stderr_artifact_id != trial.stderr_artifact_id
                || run.trace_artifact_ids != trial.trace_artifact_ids
            {
                return Err(ArenaError::InvalidStoredReceipt(
                    "invariant signed run binding",
                ));
            }
            let stdout = verify_operator_artifact(artifacts, &run.stdout_artifact_id)?;
            Ok((
                task_id.clone(),
                VerifiedTrialOutput {
                    completion_reason: run.completion_reason,
                    stdout,
                },
            ))
        })
        .collect()
}

fn verify_signed_run_result(
    event: &StoredEvent,
    verifier: &RunResultVerifier,
) -> Result<RunResultReceipt, ArenaError> {
    Ok(RunResultReceipt::parse_from_event(event, verifier)?)
}

fn aggregate_predicate(
    predicate: &str,
    forbidden_ascii_byte: Option<u8>,
    parent: &BTreeMap<String, VerifiedTrialOutput>,
    candidate: &BTreeMap<String, VerifiedTrialOutput>,
    passes: impl Fn(&VerifiedTrialOutput) -> bool,
) -> Result<InvariantPredicateResult, ArenaError> {
    let mut result = InvariantPredicateResult {
        predicate: predicate.to_owned(),
        forbidden_ascii_byte,
        parent_violations: 0,
        candidate_violations: 0,
        paired_regressions: 0,
    };
    for (task_id, parent_trial) in parent {
        let candidate_trial = candidate
            .get(task_id)
            .ok_or(ArenaError::InvalidStoredReceipt("invariant paired task"))?;
        let parent_violates = !passes(parent_trial);
        let candidate_violates = !passes(candidate_trial);
        result.parent_violations = result
            .parent_violations
            .checked_add(u32::from(parent_violates))
            .ok_or(ArenaError::MetricOverflow("parent invariant violations"))?;
        result.candidate_violations = result
            .candidate_violations
            .checked_add(u32::from(candidate_violates))
            .ok_or(ArenaError::MetricOverflow("candidate invariant violations"))?;
        result.paired_regressions = result
            .paired_regressions
            .checked_add(u32::from(!parent_violates && candidate_violates))
            .ok_or(ArenaError::MetricOverflow("paired invariant regressions"))?;
    }
    Ok(result)
}

fn load_invariant_manifest(
    world: &CompiledWorld,
    artifacts: &super::ArtifactStore,
) -> Result<(InvariantManifest, String), ArenaError> {
    let artifact_id = world
        .evaluator_artifact(MANIFEST_KEY)
        .ok_or(ArenaError::MissingWorldArtifact(MANIFEST_KEY))?;
    let bytes = verify_operator_artifact(artifacts, artifact_id)?;
    let manifest: InvariantManifest = serde_json::from_slice(&bytes)
        .map_err(|_| ArenaError::InvalidStoredReceipt("invariant manifest"))?;
    if serde_json::to_vec(&manifest)
        .map_err(|_| ArenaError::InvalidStoredReceipt("invariant manifest"))?
        != bytes
        || manifest.schema_version != RECEIPT_SCHEMA_VERSION
        || manifest.algorithm != MANIFEST_ALGORITHM
        || !(1..=4_096).contains(&manifest.maximum_output_bytes)
        || manifest
            .forbidden_ascii_bytes
            .iter()
            .any(|byte| !byte.is_ascii())
        || manifest
            .forbidden_ascii_bytes
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(ArenaError::InvalidStoredReceipt("invariant manifest"));
    }
    Ok((manifest, artifact_id.to_owned()))
}

/// Reads invariant-event routing identities only after strict envelope validation.
/// The returned evaluation and World identities remain untrusted until verification.
///
/// # Errors
///
/// Returns [`ArenaError::InvalidInvariantEvent`] for malformed, noncanonical,
/// or incorrectly identified event envelopes.
pub fn invariant_event_references(event: &StoredEvent) -> Result<(String, String), ArenaError> {
    let payload: InvariantEventPayload =
        serde_json::from_slice(&event.payload).map_err(|_| ArenaError::InvalidInvariantEvent)?;
    if serde_json::to_vec(&payload).map_err(|_| ArenaError::InvalidInvariantEvent)? != event.payload
        || payload.schema_version != RECEIPT_SCHEMA_VERSION
        || event.event_type != EVENT_TYPE
        || event.actor != EVENT_ACTOR
        || event.event_id != invariant_event_id(&payload.evaluation_id)
        || event.aggregate_id != invariant_aggregate_id(&payload.evaluation_id)
        || ArtifactId::parse(payload.receipt_artifact_id.clone()).is_err()
    {
        return Err(ArenaError::InvalidInvariantEvent);
    }
    Ok((payload.evaluation_id, payload.world_id))
}

fn rehydrate_event(
    artifacts: &super::ArtifactStore,
    event: &StoredEvent,
    expected: &InvariantReceipt,
) -> Result<InvariantEventPayload, ArenaError> {
    let (evaluation_id, world_id) = invariant_event_references(event)?;
    let payload: InvariantEventPayload =
        serde_json::from_slice(&event.payload).map_err(|_| ArenaError::InvalidInvariantEvent)?;
    if evaluation_id != expected.evaluation_id || world_id != expected.world_id {
        return Err(ArenaError::InvariantConflict(
            expected.evaluation_id.clone(),
        ));
    }
    let bytes = verify_operator_artifact(artifacts, &payload.receipt_artifact_id)?;
    let receipt: InvariantReceipt = serde_json::from_slice(&bytes)
        .map_err(|_| ArenaError::InvalidStoredReceipt("invariant receipt"))?;
    if serde_json::to_vec(&receipt)
        .map_err(|_| ArenaError::InvalidStoredReceipt("invariant receipt"))?
        != bytes
        || receipt != *expected
    {
        return Err(ArenaError::InvariantConflict(
            expected.evaluation_id.clone(),
        ));
    }
    Ok(payload)
}

fn invariant_event(event: &StoredEvent, receipt_artifact_id: &str) -> InvariantEvent {
    InvariantEvent {
        sequence: event.sequence,
        event_id: event.event_id.clone(),
        aggregate_id: event.aggregate_id.clone(),
        event_type: event.event_type.clone(),
        actor: event.actor.clone(),
        event_hash: super::encode_hash(event.hash),
        receipt_artifact_id: receipt_artifact_id.to_owned(),
    }
}

fn invariant_event_id(evaluation_id: &str) -> String {
    format!("arena:invariants:{evaluation_id}:checked")
}

fn invariant_aggregate_id(evaluation_id: &str) -> String {
    format!("arena:invariants:{evaluation_id}")
}

#[cfg(test)]
mod tests {
    use hephaestus_experience::{RunBudgetReceipt, RunResultSigner};
    use hephaestus_ledger::{ArtifactId, EventStore};
    use tempfile::tempdir;

    use super::*;

    fn claims() -> RunResultReceipt {
        RunResultReceipt {
            schema_version: 2,
            run_id: "invariant-signature-test".to_owned(),
            genome_id: format!("hephaestus:genome:{}", "1".repeat(64)),
            world_id: format!("hephaestus:world:{}", "2".repeat(64)),
            source_revision: "3".repeat(40),
            task_id: "task-a".to_owned(),
            input_commitment: ArtifactId::for_bytes(b"input").as_str().to_owned(),
            seed: 42,
            environment_id: "test-environment".to_owned(),
            budget: RunBudgetReceipt {
                wall_millis: 10_000,
                maximum_output_bytes: 1024,
                maximum_cost_microusd: 100,
            },
            completion_reason: RunCompletionReason::Success,
            latency_millis: 1,
            actual_cost_microusd: 0,
            stdout_artifact_id: ArtifactId::for_bytes(b"output").as_str().to_owned(),
            stderr_artifact_id: ArtifactId::for_bytes(b"").as_str().to_owned(),
            trace_artifact_ids: Vec::new(),
        }
    }

    #[test]
    fn invariant_run_verification_rejects_forged_signature_on_valid_ledger_chain() {
        let directory = tempdir().unwrap();
        let mut events = EventStore::open(directory.path().join("events.sqlite3")).unwrap();
        let trusted = RunResultSigner::from_seed([7; 32]);
        let forged = RunResultSigner::from_seed([8; 32]);
        let event = events
            .append(forged.issue(claims(), 1_788_000_123_500).unwrap())
            .unwrap();

        assert!(events.replay_verified().is_ok());
        assert!(matches!(
            verify_signed_run_result(&event, &trusted.verifier()),
            Err(ArenaError::RunReceipt(_))
        ));
        assert_eq!(
            verify_signed_run_result(&event, &forged.verifier())
                .unwrap()
                .event_id(),
            event.event_id
        );
    }
}
