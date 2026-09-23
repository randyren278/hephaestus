use hephaestus_arena::SelectionReceipt;
pub use hephaestus_experience::RunCompletionReason;
pub use hephaestus_genome::{GenomeRecord, WorldRecord};
use serde::{Deserialize, Serialize};

/// The only local operator API version accepted by this release.
pub const API_VERSION: u16 = 1;

/// One authenticated request over the owner-only local socket.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRequest {
    /// Protocol version.
    pub version: u16,
    /// Caller-generated correlation identifier.
    pub request_id: String,
    /// Secret read from the owner-only daemon token file.
    pub token: String,
    /// Typed operator command.
    pub command: Command,
}

/// Operator actions implemented by the first control plane.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Inspect the current canonical projection.
    Status,
    /// Stop new evolution work.
    Freeze,
    /// Resume evolution through the external operator boundary.
    Unfreeze,
    /// Terminate all active work represented by canonical events.
    KillAll,
    /// Inspect one immutable Genome record.
    GenomeShow {
        /// Content-derived Genome identity.
        genome_id: String,
    },
    /// Read the verified reserved prompt of one registered Markdown Genome.
    GenomePrompt {
        /// Content-derived Genome identity.
        genome_id: String,
    },
    /// List every registered immutable Genome record.
    GenomeList,
    /// Compile one Genome source file under a registered World and register it.
    GenomeRegister {
        /// Absolute path to a JSON, YAML, or Markdown Genome source readable by the daemon.
        path: String,
        /// Content-derived registered World identity governing the Genome.
        world_id: String,
    },
    /// Inspect one immutable World record.
    WorldShow {
        /// Content-derived World identity.
        world_id: String,
    },
    /// List every registered immutable World record.
    WorldList,
    /// Compile one World source file and register it.
    WorldRegister {
        /// Absolute path to a JSON or YAML World source readable by the daemon.
        path: String,
    },
    /// Canonicalize one Arena task manifest and store it as an artifact.
    ManifestPut {
        /// Absolute path to a JSON manifest readable by the daemon.
        path: String,
    },
    /// Store one file in the content-addressed artifact store.
    ArtifactPut {
        /// Absolute path to a file readable by the daemon.
        path: String,
    },
    /// Publish the daemon's runtime-result verifier public key as an artifact.
    VerifierShow,
    /// Execute the offline deterministic reference runtime for one registered Genome.
    RunReference {
        /// Content-derived registered Genome identity.
        genome_id: String,
    },
    /// Execute one World-bound task with daemon-owned runtime provenance.
    RunEvaluation {
        /// Content-derived registered Genome identity.
        genome_id: String,
        /// Exact World task identity.
        task_id: String,
        /// Exact provider input committed by the runtime.
        input: String,
        /// Deterministic evaluation seed.
        seed: u64,
        /// Hard wall deadline in milliseconds.
        wall_millis: u64,
        /// Maximum combined output bytes.
        maximum_output_bytes: u64,
        /// Maximum provider spend in micro-US dollars.
        maximum_cost_microusd: u64,
    },
    /// Run one trusted parent-versus-candidate evaluation from daemon-owned inputs.
    EvaluatePair {
        /// Stable caller-selected evaluation identity.
        evaluation_id: String,
        /// Immutable registered parent Genome identity.
        parent_genome_id: String,
        /// Immutable registered candidate Genome identity.
        candidate_genome_id: String,
    },
    /// Select from one exact persisted Arena evaluation using its registered World's policy.
    ArenaSelect {
        /// Stable Arena evaluation identity.
        evaluation_id: String,
    },
    /// Verify and replay canonical history into a fresh projection.
    Replay,
    /// Stop the local daemon after acknowledging the audited request.
    DaemonStop,
}

/// Stable machine-readable local API response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiResponse {
    /// Protocol version emitted by the daemon.
    pub version: u16,
    /// Correlation identifier copied from the request when available.
    pub request_id: String,
    /// Successful response body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<ResponseData>,
    /// Stable failure body with no internal error disclosure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl ApiResponse {
    pub(crate) fn success(request_id: String, data: ResponseData) -> Self {
        Self {
            version: API_VERSION,
            request_id,
            data: Some(data),
            error: None,
        }
    }

    pub(crate) fn failure(
        request_id: impl Into<String>,
        code: ApiErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            version: API_VERSION,
            request_id: request_id.into(),
            data: None,
            error: Some(ApiError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Successful typed response data.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResponseData {
    /// Current desired and observed control state.
    Status {
        /// Whether evolution is frozen.
        frozen: bool,
        /// Number of canonically active runs.
        active_runs: usize,
        /// Number of canonical events after auditing this request.
        event_count: u64,
        /// Number of registered immutable Genomes.
        genome_count: usize,
    },
    /// Deterministic acknowledgement of a consequential operator action.
    Acknowledged {
        /// Freeze state after the action.
        frozen: bool,
        /// Runs terminated by this action.
        killed_runs: usize,
    },
    /// One registered immutable Genome.
    Genome {
        /// Canonical projection record.
        genome: GenomeRecord,
    },
    /// Exact UTF-8 body bytes of a registered Genome's reserved prompt.
    GenomePrompt {
        /// Content-derived Genome identity.
        genome_id: String,
        /// Prompt body, without Markdown frontmatter.
        prompt: String,
    },
    /// Every registered immutable Genome in canonical identity order.
    Genomes {
        /// Canonical projection records.
        genomes: Vec<GenomeRecord>,
    },
    /// One registered immutable World.
    World {
        /// Canonical projection record.
        world: WorldRecord,
    },
    /// Every registered immutable World in canonical identity order.
    Worlds {
        /// Canonical projection records.
        worlds: Vec<WorldRecord>,
    },
    /// One content-addressed artifact.
    Artifact {
        /// BLAKE3 artifact address.
        artifact_id: String,
        /// Stored size in bytes.
        bytes: u64,
    },
    /// The daemon's Ed25519 runtime-result verifier.
    Verifier {
        /// CAS address of the raw 32-byte public key, usable as `arena.runtime_verifier`.
        artifact_id: String,
        /// Hex-encoded public key.
        public_key_hex: String,
    },
    /// Terminal result from the offline deterministic reference runtime.
    Run {
        /// Stable run identity.
        run_id: String,
        /// Immutable Genome identity executed by the runtime.
        genome_id: String,
        /// Immutable registered World identity governing the run.
        world_id: String,
        /// Exact Git commit inventoried by the isolated runtime.
        source_revision: String,
        /// Exact runtime-owned completion reason.
        completion_reason: RunCompletionReason,
        /// Runtime-owned terminal latency.
        latency_millis: u64,
        /// Exact deterministic provider cost in micro-US dollars.
        actual_cost_microusd: u64,
        /// CAS address of the bounded inventory output.
        stdout_artifact_id: String,
        /// CAS address of the bounded diagnostic output.
        stderr_artifact_id: String,
        /// CAS addresses of the redacted trace artifacts.
        trace_artifact_ids: Vec<String>,
    },
    /// Visible aggregate result from a trusted paired evaluation.
    Evaluation {
        /// Visible summary and payload-free canonical event metadata.
        evaluation: EvaluationRecord,
    },
    /// Operator-only metrics and fail-closed promotion state for one selection.
    Selection {
        /// Aggregate measurements and payload-free canonical selection event metadata.
        selection: Box<SelectionRecord>,
    },
    /// Result of a fresh verified replay.
    Replay {
        /// Number of verified canonical events.
        event_count: u64,
        /// Freeze state reconstructed from history.
        frozen: bool,
        /// Active run count reconstructed from history.
        active_runs: usize,
        /// Stable BLAKE3 hash of the reconstructed projection.
        projection_hash: String,
    },
}

/// Candidate-safe visible aggregate and ledger metadata for one evaluation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationRecord {
    /// Stable evaluation identity.
    pub evaluation_id: String,
    /// Exact immutable World identity shared by both Genomes.
    pub world_id: String,
    /// Immutable parent Genome identity.
    pub parent_genome_id: String,
    /// Immutable candidate Genome identity.
    pub candidate_genome_id: String,
    /// Parent correct answers on candidate-visible tasks.
    pub parent_visible_correct: u32,
    /// Candidate correct answers on candidate-visible tasks.
    pub candidate_visible_correct: u32,
    /// Number of candidate-visible tasks.
    pub visible_total: u32,
    /// Payload-free canonical event metadata.
    pub event: EvaluationEventRecord,
}

/// Payload-free canonical ledger metadata for an evaluation receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic event identity.
    pub event_id: String,
    /// Deterministic evaluation aggregate identity.
    pub aggregate_id: String,
    /// Stable event type.
    pub event_type: String,
    /// Fixed trusted actor.
    pub actor: String,
    /// Caller-observed Unix timestamp in milliseconds.
    pub timestamp_millis: i64,
}

/// Operator-only full result of the trusted selection calculation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionRecord {
    /// Stable Arena evaluation identity.
    pub evaluation_id: String,
    /// Exact immutable registered World identity whose policy governed selection.
    pub world_id: String,
    /// Full deterministic receipt, including sealed-derived operator metrics and policy inputs.
    pub receipt: SelectionReceipt,
    /// Payload-free canonical event metadata binding the receipt artifact.
    pub event: SelectionEventRecord,
}

/// Payload-free canonical ledger metadata for a selection receipt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectionEventRecord {
    /// Canonical global ledger sequence.
    pub sequence: u64,
    /// Deterministic event identity.
    pub event_id: String,
    /// Stable evaluation selection aggregate identity.
    pub aggregate_id: String,
    /// Event type.
    pub event_type: String,
    /// Trusted actor.
    pub actor: String,
    /// Canonical event-chain hash.
    pub event_hash: String,
    /// BLAKE3 address of the canonical selection receipt.
    pub receipt_artifact_id: String,
}

/// Safe local API failure body.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApiError {
    /// Stable error category.
    pub code: ApiErrorCode,
    /// Human-readable message without internal storage detail.
    pub message: String,
}

/// Stable fail-closed local API categories.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    /// The request used an unsupported protocol version.
    UnsupportedVersion,
    /// Authentication failed.
    Unauthorized,
    /// A required identifier was empty or absent.
    InvalidRequest,
    /// The requested canonical record does not exist.
    NotFound,
    /// Canonical persistence or projection verification failed.
    Internal,
}

#[cfg(test)]
mod tests {
    use super::{
        API_VERSION, ApiRequest, ApiResponse, Command, EvaluationEventRecord, EvaluationRecord,
        ResponseData,
    };

    #[test]
    fn paired_evaluation_command_has_a_stable_wire_shape() {
        let request = ApiRequest {
            version: API_VERSION,
            request_id: "request-1".to_owned(),
            token: "secret".to_owned(),
            command: Command::EvaluatePair {
                evaluation_id: "evaluation-1".to_owned(),
                parent_genome_id: "parent-1".to_owned(),
                candidate_genome_id: "candidate-1".to_owned(),
            },
        };

        let encoded = serde_json::to_value(&request).expect("request serializes");
        assert_eq!(
            encoded["command"],
            serde_json::json!({
                "command": "evaluate_pair",
                "evaluation_id": "evaluation-1",
                "parent_genome_id": "parent-1",
                "candidate_genome_id": "candidate-1"
            })
        );
        assert_eq!(
            serde_json::from_value::<ApiRequest>(encoded).expect("request deserializes"),
            request
        );
    }

    #[test]
    fn evaluation_response_contains_only_visible_aggregates_and_event_metadata() {
        let response = ApiResponse::success(
            "request-1".to_owned(),
            ResponseData::Evaluation {
                evaluation: EvaluationRecord {
                    evaluation_id: "evaluation-1".to_owned(),
                    world_id: "world-1".to_owned(),
                    parent_genome_id: "parent-1".to_owned(),
                    candidate_genome_id: "candidate-1".to_owned(),
                    parent_visible_correct: 2,
                    candidate_visible_correct: 3,
                    visible_total: 4,
                    event: EvaluationEventRecord {
                        sequence: 9,
                        event_id: "evaluation:evaluation-1:recorded".to_owned(),
                        aggregate_id: "evaluation:evaluation-1".to_owned(),
                        event_type: "evaluation.recorded".to_owned(),
                        actor: "arena-plane".to_owned(),
                        timestamp_millis: 1_234,
                    },
                },
            },
        );

        let encoded = serde_json::to_value(&response).expect("response serializes");
        assert_eq!(encoded["data"]["type"], "evaluation");
        assert_eq!(encoded["data"]["evaluation"]["visible_total"], 4);
        assert_eq!(encoded["data"]["evaluation"]["event"]["sequence"], 9);
        let text = serde_json::to_string(&encoded).expect("JSON value serializes");
        assert!(!text.contains("sealed"));
        assert!(!text.contains("expected_output"));
        assert!(!text.contains("artifact_id"));
        assert_eq!(
            serde_json::from_value::<ApiResponse>(encoded).expect("response deserializes"),
            response
        );
    }

    #[test]
    fn evaluation_response_rejects_unknown_evidence_fields() {
        let value = serde_json::json!({
            "version": API_VERSION,
            "request_id": "request-1",
            "data": {
                "type": "evaluation",
                "evaluation": {
                    "evaluation_id": "evaluation-1",
                    "world_id": "world-1",
                    "parent_genome_id": "parent-1",
                    "candidate_genome_id": "candidate-1",
                    "parent_visible_correct": 2,
                    "candidate_visible_correct": 3,
                    "visible_total": 4,
                    "sealed_total": 5,
                    "event": {
                        "sequence": 9,
                        "event_id": "evaluation:evaluation-1:recorded",
                        "aggregate_id": "evaluation:evaluation-1",
                        "event_type": "evaluation.recorded",
                        "actor": "arena-plane",
                        "timestamp_millis": 1234
                    }
                }
            },
            "error": null
        });

        assert!(serde_json::from_value::<ApiResponse>(value).is_err());
    }
}
