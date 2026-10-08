//! Strict wire protocol for the trusted World-policy evaluator worker.

use std::collections::BTreeSet;

use hephaestus_genome::OutputScoring;
use hephaestus_ledger::ArtifactId;
use serde::{Deserialize, Serialize};

use crate::ArenaError;

/// Maximum canonical evaluator request size accepted across the process boundary.
pub const MAX_EVALUATOR_REQUEST_BYTES: usize = 16 * 1024 * 1024;
/// Maximum canonical evaluator response size accepted across the process boundary.
pub const MAX_EVALUATOR_RESPONSE_BYTES: usize = 64 * 1024;

/// One evaluator-only output-comparison trial.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorTrial {
    /// Stable task identity.
    pub task_id: String,
    /// Trusted expected output, never returned by the worker.
    pub expected_output: String,
    /// Immutable parent output.
    pub parent_output: String,
    /// Immutable candidate output.
    pub candidate_output: String,
    /// Whether the parent completed successfully.
    pub parent_reliable: bool,
    /// Whether the candidate completed successfully.
    pub candidate_reliable: bool,
}

/// Evaluator-only request passed over bounded stdin.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorRequest {
    /// Wire schema version.
    pub schema_version: u16,
    /// Stable evaluation identity used as a freshness and replay binding.
    pub evaluation_id: String,
    /// World-bound evaluator artifact identity.
    pub evaluator_id: String,
    /// World-bound comparison policy. Schema 1 omits the exact default;
    /// schema 2 requires an explicit non-default policy.
    #[serde(default, skip_serializing_if = "OutputScoring::is_exact")]
    pub output_scoring: OutputScoring,
    /// Candidate-visible trials.
    pub visible: Vec<EvaluatorTrial>,
    /// Evaluator-only trials.
    pub sealed: Vec<EvaluatorTrial>,
}

/// Aggregate scores returned by the evaluator worker.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorScores {
    /// Parent correct answers on visible tasks.
    pub parent_visible_correct: u32,
    /// Candidate correct answers on visible tasks.
    pub candidate_visible_correct: u32,
    /// Parent correct answers on sealed tasks.
    pub parent_sealed_correct: u32,
    /// Candidate correct answers on sealed tasks.
    pub candidate_sealed_correct: u32,
    /// Tasks the parent passed and candidate failed.
    pub regressions: u32,
    /// Tasks the parent failed and candidate passed.
    pub improvements: u32,
    /// Number of visible tasks.
    pub visible_total: u32,
    /// Number of sealed tasks.
    pub sealed_total: u32,
}

/// Candidate-safe evaluator response. It contains no task-level or expected data.
#[doc(hidden)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorResponse {
    /// Wire schema version.
    pub schema_version: u16,
    /// Content address of the exact canonical request bytes consumed by the worker.
    pub request_artifact_id: String,
    /// Aggregate World-policy scores.
    pub scores: EvaluatorScores,
}

/// Parses, validates, and evaluates one canonical worker request.
///
/// # Errors
///
/// Rejects oversized, malformed, non-canonical, duplicated, or unsupported requests.
#[doc(hidden)]
pub fn evaluate_request(bytes: &[u8]) -> Result<Vec<u8>, ArenaError> {
    if bytes.len() > MAX_EVALUATOR_REQUEST_BYTES {
        return Err(ArenaError::EvaluatorProtocol("request exceeds byte limit"));
    }
    let request: EvaluatorRequest = serde_json::from_slice(bytes)?;
    validate_request(&request)?;
    let canonical = serde_json::to_vec(&request)?;
    if canonical != bytes {
        return Err(ArenaError::EvaluatorProtocol(
            "request is not canonical JSON",
        ));
    }
    let scores = score(&request)?;
    let response = serde_json::to_vec(&EvaluatorResponse {
        schema_version: 1,
        request_artifact_id: ArtifactId::for_bytes(bytes).as_str().to_owned(),
        scores,
    })?;
    if response.len() > MAX_EVALUATOR_RESPONSE_BYTES {
        return Err(ArenaError::EvaluatorProtocol("response exceeds byte limit"));
    }
    Ok(response)
}

fn validate_request(request: &EvaluatorRequest) -> Result<(), ArenaError> {
    if !matches!(
        (request.schema_version, request.output_scoring.is_exact()),
        (1, true) | (2, false)
    ) {
        return Err(ArenaError::EvaluatorProtocol("unsupported request schema"));
    }
    super::validate_id("evaluation_id", &request.evaluation_id)?;
    ArtifactId::parse(request.evaluator_id.clone())?;
    if request.visible.is_empty() || request.sealed.is_empty() {
        return Err(ArenaError::EvaluatorProtocol("task sets must be non-empty"));
    }
    if request.visible.len().saturating_add(request.sealed.len()) > super::MAX_TASKS {
        return Err(ArenaError::TooManyTasks);
    }
    let mut task_ids = BTreeSet::new();
    for trial in request.visible.iter().chain(&request.sealed) {
        super::validate_id("task_id", &trial.task_id)?;
        for (field, value) in [
            ("task.expected_output", &trial.expected_output),
            ("submission.output", &trial.parent_output),
            ("submission.output", &trial.candidate_output),
        ] {
            super::validate_text(field, value)?;
        }
        crate::scoring::validate_expected(request.output_scoring, &trial.expected_output)?;
        if !task_ids.insert(&trial.task_id) {
            return Err(ArenaError::DuplicateTaskId(trial.task_id.clone()));
        }
    }
    Ok(())
}

fn score(request: &EvaluatorRequest) -> Result<EvaluatorScores, ArenaError> {
    let visible_total =
        u32::try_from(request.visible.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let sealed_total = u32::try_from(request.sealed.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let mut scores = EvaluatorScores {
        parent_visible_correct: 0,
        candidate_visible_correct: 0,
        parent_sealed_correct: 0,
        candidate_sealed_correct: 0,
        regressions: 0,
        improvements: 0,
        visible_total,
        sealed_total,
    };
    for (visible, trials) in [
        (true, request.visible.as_slice()),
        (false, request.sealed.as_slice()),
    ] {
        for trial in trials {
            let parent_correct = trial.parent_reliable
                && crate::scoring::outputs_match(
                    request.output_scoring,
                    &trial.parent_output,
                    &trial.expected_output,
                );
            let candidate_correct = trial.candidate_reliable
                && crate::scoring::outputs_match(
                    request.output_scoring,
                    &trial.candidate_output,
                    &trial.expected_output,
                );
            if visible {
                scores.parent_visible_correct += u32::from(parent_correct);
                scores.candidate_visible_correct += u32::from(candidate_correct);
            } else {
                scores.parent_sealed_correct += u32::from(parent_correct);
                scores.candidate_sealed_correct += u32::from(candidate_correct);
            }
            scores.regressions += u32::from(parent_correct && !candidate_correct);
            scores.improvements += u32::from(!parent_correct && candidate_correct);
        }
    }
    Ok(scores)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> EvaluatorRequest {
        EvaluatorRequest {
            schema_version: 1,
            output_scoring: OutputScoring::Exact,
            evaluation_id: "evaluation-1".to_owned(),
            evaluator_id: ArtifactId::for_bytes(b"worker").as_str().to_owned(),
            visible: vec![EvaluatorTrial {
                task_id: "visible".to_owned(),
                expected_output: "yes".to_owned(),
                parent_output: "no".to_owned(),
                candidate_output: "yes".to_owned(),
                parent_reliable: true,
                candidate_reliable: true,
            }],
            sealed: vec![EvaluatorTrial {
                task_id: "sealed".to_owned(),
                expected_output: "secret".to_owned(),
                parent_output: "secret".to_owned(),
                candidate_output: "wrong".to_owned(),
                parent_reliable: true,
                candidate_reliable: true,
            }],
        }
    }

    #[test]
    fn response_is_bound_and_contains_only_aggregate_scores() {
        let bytes = serde_json::to_vec(&request()).unwrap();
        let response = evaluate_request(&bytes).unwrap();
        let parsed: EvaluatorResponse = serde_json::from_slice(&response).unwrap();
        assert_eq!(
            parsed.request_artifact_id,
            ArtifactId::for_bytes(&bytes).as_str()
        );
        assert_eq!(parsed.scores.candidate_visible_correct, 1);
        assert_eq!(parsed.scores.parent_sealed_correct, 1);
        let text = String::from_utf8(response).unwrap();
        for forbidden in ["secret", "\"yes\"", "\"no\"", "task_id", "expected_output"] {
            assert!(!text.contains(forbidden));
        }
    }

    #[test]
    fn unreliable_matching_output_is_never_correct() {
        let mut request = request();
        request.visible[0].parent_output = "yes".to_owned();
        request.visible[0].candidate_reliable = false;
        let response = evaluate_request(&serde_json::to_vec(&request).unwrap()).unwrap();
        let parsed: EvaluatorResponse = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed.scores.parent_visible_correct, 1);
        assert_eq!(parsed.scores.candidate_visible_correct, 0);
        assert_eq!(parsed.scores.regressions, 2);
    }

    #[test]
    fn versioned_scoring_keeps_historical_wire_bytes_and_binds_the_policy() {
        let exact = request();
        let exact_bytes = serde_json::to_vec(&exact).unwrap();
        let historical = format!(
            r#"{{"schema_version":1,"evaluation_id":"evaluation-1","evaluator_id":"{}","visible":[{{"task_id":"visible","expected_output":"yes","parent_output":"no","candidate_output":"yes","parent_reliable":true,"candidate_reliable":true}}],"sealed":[{{"task_id":"sealed","expected_output":"secret","parent_output":"secret","candidate_output":"wrong","parent_reliable":true,"candidate_reliable":true}}]}}"#,
            exact.evaluator_id
        );
        assert_eq!(exact_bytes, historical.as_bytes());
        let explicit_exact = historical.replacen(
            ",\"visible\":",
            ",\"output_scoring\":\"exact\",\"visible\":",
            1,
        );
        assert!(matches!(
            evaluate_request(explicit_exact.as_bytes()),
            Err(ArenaError::EvaluatorProtocol(
                "request is not canonical JSON"
            ))
        ));
        for policy in [OutputScoring::Trimmed, OutputScoring::JsonCanonical] {
            let mut versioned = exact.clone();
            versioned.schema_version = 2;
            versioned.output_scoring = policy;
            let mode = serde_json::to_string(&policy).unwrap();
            let golden = historical
                .replacen("\"schema_version\":1", "\"schema_version\":2", 1)
                .replacen(
                    ",\"visible\":",
                    &format!(",\"output_scoring\":{mode},\"visible\":"),
                    1,
                );
            assert_eq!(serde_json::to_vec(&versioned).unwrap(), golden.as_bytes());
        }
        let mut request = exact;
        request.visible[0].candidate_output = " yes\n".to_owned();
        let parse = |request: &EvaluatorRequest| -> EvaluatorResponse {
            serde_json::from_slice(
                &evaluate_request(&serde_json::to_vec(request).unwrap()).unwrap(),
            )
            .unwrap()
        };
        let exact_response = parse(&request);
        assert_eq!(exact_response.scores.candidate_visible_correct, 0);
        request.schema_version = 2;
        request.output_scoring = OutputScoring::Trimmed;
        let normalized = parse(&request);
        assert_eq!(normalized.scores.candidate_visible_correct, 1);
        assert_ne!(
            exact_response.request_artifact_id,
            normalized.request_artifact_id
        );
        request.schema_version = 1;
        assert!(evaluate_request(&serde_json::to_vec(&request).unwrap()).is_err());
    }

    #[test]
    fn json_scoring_rejects_invalid_expectations_and_counts_only_reliable_semantic_matches() {
        let mut request = request();
        request.schema_version = 2;
        request.output_scoring = OutputScoring::JsonCanonical;
        for trial in request.visible.iter_mut().chain(&mut request.sealed) {
            trial.expected_output = r#"{"a":1,"b":2}"#.to_owned();
            trial.parent_output = r#"{"b":2.0,"a":1e0}"#.to_owned();
            trial.candidate_output = r#"{"a":1,"a":1,"b":2}"#.to_owned();
        }
        request.sealed[0].candidate_output = "{\"b\":2,\"a\":1}\n".to_owned();
        request.sealed[0].parent_reliable = false;
        let response: EvaluatorResponse = serde_json::from_slice(
            &evaluate_request(&serde_json::to_vec(&request).unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(response.scores.parent_visible_correct, 1);
        assert_eq!(response.scores.candidate_visible_correct, 0);
        assert_eq!(response.scores.parent_sealed_correct, 0);
        assert_eq!(response.scores.candidate_sealed_correct, 1);
        assert_eq!(response.scores.regressions, 1);
        assert_eq!(response.scores.improvements, 1);
        request.sealed[0].expected_output = r#"{"secret":1,"secret":2}"#.to_owned();
        let error = evaluate_request(&serde_json::to_vec(&request).unwrap()).unwrap_err();
        assert!(matches!(
            error,
            ArenaError::EvaluatorProtocol("expected output is not supported strict JSON")
        ));
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn protocol_rejects_noncanonical_unknown_duplicate_and_unsupported_requests() {
        let canonical = serde_json::to_vec(&request()).unwrap();
        let mut spaced = canonical.clone();
        spaced.push(b' ');
        assert!(matches!(
            evaluate_request(&spaced),
            Err(ArenaError::EvaluatorProtocol(
                "request is not canonical JSON"
            ))
        ));

        let mut unknown = serde_json::to_value(request()).unwrap();
        unknown["unexpected"] = serde_json::json!(true);
        assert!(evaluate_request(&serde_json::to_vec(&unknown).unwrap()).is_err());

        let mut duplicate = request();
        duplicate.sealed[0].task_id = duplicate.visible[0].task_id.clone();
        assert!(matches!(
            evaluate_request(&serde_json::to_vec(&duplicate).unwrap()),
            Err(ArenaError::DuplicateTaskId(_))
        ));

        let mut unsupported = request();
        unsupported.schema_version = 2;
        assert!(matches!(
            evaluate_request(&serde_json::to_vec(&unsupported).unwrap()),
            Err(ArenaError::EvaluatorProtocol("unsupported request schema"))
        ));
    }
}
