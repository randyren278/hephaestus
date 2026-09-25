//! Small, bounded instruction language for the offline reference worker.

use serde::Deserialize;

use crate::RuntimeError;

const MAX_INSTRUCTION_BYTES: usize = 4096;
pub(crate) const MAX_TASK_INPUT_BYTES: usize = 1_048_576;
const FRAME_MAGIC: &[u8; 8] = b"HPSREF01";

/// Deterministic operation selected by a registered Genome's reserved prompt.
///
/// The `Identity`/`AsciiUppercase` pair is the only one Forge's mutation
/// operator (the reference-operation flip) knows how to mutate between; see
/// `hephaestus-control`'s `forge_prompt_mutation`. The Gauntlet pair
/// operations below are deterministic proxies for the seven named Gauntlet
/// failure modes (roadmap item 10): each one parses a small structured JSON
/// scenario from the task input and exhibits, or avoids, the named pathology
/// in a bounded, honest way documented in
/// `examples/gauntlet/README.md`. They are reachable only through direct
/// Arena evaluation (`arena evaluate`/`arena select`), not through Forge or
/// `evolve`, which reject any prompt outside the Identity/AsciiUppercase
/// mutation language.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceInstruction {
    /// Return the exact World task input bytes.
    Identity,
    /// Apply ASCII uppercase to the exact World task input bytes.
    AsciiUppercase,
    /// Gauntlet "context loss": only looks at the last turn of a bounded
    /// conversation, missing a fact stated earlier.
    ContextLossNaive,
    /// Gauntlet "context loss" fix: scans every turn for the fact.
    ContextLossAware,
    /// Gauntlet "premature completion": reports done after the first step.
    PrematureCompletion,
    /// Gauntlet "premature completion" fix: requires every step to verify.
    VerifiedCompletion,
    /// Gauntlet "schema drift": reads a hardcoded schema-v1 field name.
    SchemaDriftBrittle,
    /// Gauntlet "schema drift" fix: reads the field named by the declared
    /// schema version.
    SchemaDriftAdaptive,
    /// Gauntlet "bad routing": always selects the cheapest route, ignoring
    /// whether it has the required capability.
    BadRoutingCheapest,
    /// Gauntlet "bad routing" fix: selects the cheapest route that has the
    /// required capability.
    CapabilityAwareRouting,
    /// Gauntlet "duplicate subagents": executes every requested subagent
    /// task, including duplicates.
    DuplicateSubagentsWasteful,
    /// Gauntlet "duplicate subagents" fix: deduplicates requests first.
    DeduplicatedSubagents,
    /// Gauntlet "poisoned memory": returns the most recent memory entry
    /// regardless of its trust provenance.
    PoisonedMemoryTrusting,
    /// Gauntlet "poisoned memory" fix: returns only trusted memory.
    ProvenanceCheckedMemory,
    /// Gauntlet "hallucinated verification": trusts a self-reported
    /// completion status instead of the ground-truth state.
    HallucinatedVerificationTrusting,
    /// Gauntlet "hallucinated verification" fix: always reports the actual
    /// ground-truth state.
    GroundTruthVerification,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstructionDocument {
    schema_version: u8,
    operation: String,
}

impl ReferenceInstruction {
    /// Parses the complete strict `hephaestus-reference-v1` fenced document.
    ///
    /// # Errors
    ///
    /// Rejects oversized, malformed, duplicate-key, unknown-version, or unknown-operation bodies.
    pub fn parse(body: &str) -> Result<Self, RuntimeError> {
        if body.len() > MAX_INSTRUCTION_BYTES {
            return Err(RuntimeError::InvalidSpec(
                "reference instruction is oversized",
            ));
        }
        if body.contains('\r') && body.replace("\r\n", "").contains('\r') {
            return Err(RuntimeError::InvalidSpec(
                "reference instruction framing is invalid",
            ));
        }
        let normalized = body.replace("\r\n", "\n");
        let normalized = normalized.strip_suffix('\n').unwrap_or(&normalized);
        let Some(json) = normalized
            .strip_prefix("```hephaestus-reference-v1\n")
            .and_then(|body| body.strip_suffix("\n```"))
        else {
            return Err(RuntimeError::InvalidSpec(
                "reference instruction framing is invalid",
            ));
        };
        let document: InstructionDocument = serde_json::from_str(json)
            .map_err(|_| RuntimeError::InvalidSpec("reference instruction document is invalid"))?;
        if document.schema_version != 1 {
            return Err(RuntimeError::InvalidSpec(
                "reference instruction version is unsupported",
            ));
        }
        match document.operation.as_str() {
            "identity" => Ok(Self::Identity),
            "ascii_uppercase" => Ok(Self::AsciiUppercase),
            "context_loss_naive" => Ok(Self::ContextLossNaive),
            "context_loss_aware" => Ok(Self::ContextLossAware),
            "premature_completion" => Ok(Self::PrematureCompletion),
            "verified_completion" => Ok(Self::VerifiedCompletion),
            "schema_drift_brittle" => Ok(Self::SchemaDriftBrittle),
            "schema_drift_adaptive" => Ok(Self::SchemaDriftAdaptive),
            "bad_routing_cheapest" => Ok(Self::BadRoutingCheapest),
            "capability_aware_routing" => Ok(Self::CapabilityAwareRouting),
            "duplicate_subagents_wasteful" => Ok(Self::DuplicateSubagentsWasteful),
            "deduplicated_subagents" => Ok(Self::DeduplicatedSubagents),
            "poisoned_memory_trusting" => Ok(Self::PoisonedMemoryTrusting),
            "provenance_checked_memory" => Ok(Self::ProvenanceCheckedMemory),
            "hallucinated_verification_trusting" => Ok(Self::HallucinatedVerificationTrusting),
            "ground_truth_verification" => Ok(Self::GroundTruthVerification),
            _ => Err(RuntimeError::InvalidSpec(
                "reference instruction operation is unsupported",
            )),
        }
    }

    /// The `hephaestus-reference-v1` operation name for this instruction.
    #[must_use]
    pub const fn operation_name(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::AsciiUppercase => "ascii_uppercase",
            Self::ContextLossNaive => "context_loss_naive",
            Self::ContextLossAware => "context_loss_aware",
            Self::PrematureCompletion => "premature_completion",
            Self::VerifiedCompletion => "verified_completion",
            Self::SchemaDriftBrittle => "schema_drift_brittle",
            Self::SchemaDriftAdaptive => "schema_drift_adaptive",
            Self::BadRoutingCheapest => "bad_routing_cheapest",
            Self::CapabilityAwareRouting => "capability_aware_routing",
            Self::DuplicateSubagentsWasteful => "duplicate_subagents_wasteful",
            Self::DeduplicatedSubagents => "deduplicated_subagents",
            Self::PoisonedMemoryTrusting => "poisoned_memory_trusting",
            Self::ProvenanceCheckedMemory => "provenance_checked_memory",
            Self::HallucinatedVerificationTrusting => "hallucinated_verification_trusting",
            Self::GroundTruthVerification => "ground_truth_verification",
        }
    }

    pub(crate) fn frame(self, input: &[u8]) -> Result<Vec<u8>, RuntimeError> {
        if input.len() > MAX_TASK_INPUT_BYTES {
            return Err(RuntimeError::InvalidSpec(
                "reference task input is oversized",
            ));
        }
        let length = u32::try_from(input.len())
            .map_err(|_| RuntimeError::InvalidSpec("reference task input is oversized"))?;
        let mut frame = Vec::with_capacity(FRAME_MAGIC.len() + 1 + 4 + input.len());
        frame.extend_from_slice(FRAME_MAGIC);
        frame.push(match self {
            Self::Identity => 0,
            Self::AsciiUppercase => 1,
            Self::ContextLossNaive => 2,
            Self::ContextLossAware => 3,
            Self::PrematureCompletion => 4,
            Self::VerifiedCompletion => 5,
            Self::SchemaDriftBrittle => 6,
            Self::SchemaDriftAdaptive => 7,
            Self::BadRoutingCheapest => 8,
            Self::CapabilityAwareRouting => 9,
            Self::DuplicateSubagentsWasteful => 10,
            Self::DeduplicatedSubagents => 11,
            Self::PoisonedMemoryTrusting => 12,
            Self::ProvenanceCheckedMemory => 13,
            Self::HallucinatedVerificationTrusting => 14,
            Self::GroundTruthVerification => 15,
        });
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(input);
        Ok(frame)
    }

    /// Test-only: appends a trailing delay trailer that
    /// [`execute_reference_worker_request`] sleeps on before executing the
    /// frame. This exists solely so a test can make a designated Genome's
    /// reference worker measurably, genuinely slower without any race or
    /// flakiness. It does not exist in a build without the `test-support`
    /// feature: no caller compiled without that feature can construct this
    /// trailer, and a release worker binary parsing such bytes as ordinary
    /// task input would simply fail the frame-length check below.
    ///
    /// # Errors
    ///
    /// Rejects an oversized `input`, exactly like [`Self::frame`].
    #[cfg(feature = "test-support")]
    pub fn frame_with_test_delay(
        self,
        input: &[u8],
        delay_millis: u64,
    ) -> Result<Vec<u8>, RuntimeError> {
        let mut frame = self.frame(input)?;
        frame.extend_from_slice(TEST_DELAY_TRAILER_MAGIC);
        frame.extend_from_slice(&delay_millis.to_be_bytes());
        Ok(frame)
    }
}

/// Frames one reference instruction and its input for a worker transport.
///
/// This is the exact framing the local `hephaestus-reference-worker` binary
/// consumes over stdin; a remote worker uses the same bytes over its own
/// authenticated channel, so the isolated sandboxed transform itself is
/// identical regardless of transport.
///
/// # Errors
///
/// Rejects oversized input.
pub fn frame_reference_instruction(
    instruction: ReferenceInstruction,
    input: &[u8],
) -> Result<Vec<u8>, RuntimeError> {
    instruction.frame(input)
}
/// Test-only marker for the trailing delay trailer read by
/// [`execute_reference_worker_request`]. Never produced or interpreted
/// outside the `test-support` feature.
#[cfg(feature = "test-support")]
const TEST_DELAY_TRAILER_MAGIC: &[u8; 8] = b"TDLYMS01";
#[cfg(feature = "test-support")]
const TEST_DELAY_TRAILER_LEN: usize = 16;
/// Upper bound on an injected test delay so a malformed value cannot hang a
/// test run indefinitely.
#[cfg(feature = "test-support")]
const TEST_DELAY_MAX_MILLIS: u64 = 30_000;

/// Executes a validated reference worker frame. The worker has no filesystem or network behavior.
///
/// # Errors
///
/// Rejects truncated, oversized, trailing, or unknown-version frames.
///
/// # Panics
///
/// Never panics: the trailer-length check above guards the one slice
/// conversion that would otherwise be fallible.
pub fn execute_reference_worker_request(frame: &[u8]) -> Result<Vec<u8>, RuntimeError> {
    const HEADER: usize = 13;
    #[cfg(feature = "test-support")]
    let frame = {
        let has_test_delay = frame.len() >= TEST_DELAY_TRAILER_LEN
            && frame[frame.len() - TEST_DELAY_TRAILER_LEN..frame.len() - 8]
                == *TEST_DELAY_TRAILER_MAGIC;
        if has_test_delay {
            let millis_bytes: [u8; 8] = frame[frame.len() - 8..]
                .try_into()
                .expect("trailer length checked above");
            let millis = u64::from_be_bytes(millis_bytes).min(TEST_DELAY_MAX_MILLIS);
            std::thread::sleep(std::time::Duration::from_millis(millis));
            &frame[..frame.len() - TEST_DELAY_TRAILER_LEN]
        } else {
            frame
        }
    };
    if frame.len() < HEADER || &frame[..8] != FRAME_MAGIC {
        return Err(RuntimeError::InvalidSpec(
            "reference worker frame is invalid",
        ));
    }
    let encoded_length: [u8; 4] = frame[9..13]
        .try_into()
        .map_err(|_| RuntimeError::InvalidSpec("reference worker frame is invalid"))?;
    let expected = usize::try_from(u32::from_be_bytes(encoded_length))
        .map_err(|_| RuntimeError::InvalidSpec("reference worker frame length is invalid"))?;
    if expected > MAX_TASK_INPUT_BYTES || frame.len() != HEADER + expected {
        return Err(RuntimeError::InvalidSpec(
            "reference worker frame length is invalid",
        ));
    }
    match frame[8] {
        0 => Ok(frame[HEADER..].to_vec()),
        1 => Ok(frame[HEADER..].iter().map(u8::to_ascii_uppercase).collect()),
        opcode @ 2..=15 => gauntlet::execute(opcode, &frame[HEADER..]),
        _ => Err(RuntimeError::InvalidSpec(
            "reference worker operation is unsupported",
        )),
    }
}

/// Deterministic Gauntlet-mode operations (roadmap item 10). Each pair
/// parses a small, strict JSON scenario from the task input and exhibits, or
/// avoids, one named Gauntlet failure mode. These are honest, bounded
/// proxies for the named pathology, not a real multi-turn model, tool
/// schema, router, subagent orchestrator, memory store, or self-reporting
/// model; see `examples/gauntlet/README.md` for exactly what each proxy does
/// and does not prove.
mod gauntlet {
    use serde::Deserialize;

    use crate::RuntimeError;

    const BAD_INPUT: RuntimeError = RuntimeError::InvalidSpec("gauntlet task input is invalid");

    pub(super) fn execute(opcode: u8, input: &[u8]) -> Result<Vec<u8>, RuntimeError> {
        let text = std::str::from_utf8(input).map_err(|_| BAD_INPUT)?;
        match opcode {
            2 => context_loss_naive(text),
            3 => context_loss_aware(text),
            4 => premature_completion(text),
            5 => verified_completion(text),
            6 => schema_drift_brittle(text),
            7 => schema_drift_adaptive(text),
            8 => bad_routing_cheapest(text),
            9 => capability_aware_routing(text),
            10 => duplicate_subagents_wasteful(text),
            11 => deduplicated_subagents(text),
            12 => poisoned_memory_trusting(text),
            13 => provenance_checked_memory(text),
            14 => hallucinated_verification_trusting(text),
            15 => ground_truth_verification(text),
            _ => Err(RuntimeError::InvalidSpec(
                "reference worker operation is unsupported",
            )),
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ContextLossInput {
        turns: Vec<String>,
    }

    /// Only reads the last turn of a bounded conversation, so a fact stated
    /// in an earlier turn (prefixed `FACT:`) is lost.
    fn context_loss_naive(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let _input: ContextLossInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        Ok(b"UNKNOWN".to_vec())
    }

    /// Scans every turn for the fact instead of only the most recent one.
    fn context_loss_aware(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: ContextLossInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        let fact = input
            .turns
            .iter()
            .find_map(|turn| turn.strip_prefix("FACT:"))
            .ok_or(BAD_INPUT)?;
        Ok(fact.as_bytes().to_vec())
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StepsInput {
        steps: Vec<String>,
    }

    fn step_marker(step: &str) -> Result<&str, RuntimeError> {
        step.split_once(':')
            .map(|(_, marker)| marker)
            .ok_or(BAD_INPUT)
    }

    /// Reports completion after the first step's marker alone.
    fn premature_completion(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: StepsInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        let first = input.steps.first().ok_or(BAD_INPUT)?;
        Ok(step_marker(first)?.as_bytes().to_vec())
    }

    /// Requires every step's marker before reporting completion.
    fn verified_completion(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: StepsInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        if input.steps.is_empty() {
            return Err(BAD_INPUT);
        }
        let markers = input
            .steps
            .iter()
            .map(|step| step_marker(step))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(markers.join(",").into_bytes())
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SchemaDriftInput {
        schema_version: u8,
        field_v1: Option<String>,
        field_v2: Option<String>,
    }

    /// Always reads the schema-v1 field name, even once the schema has
    /// drifted to v2 and the value now lives under a different field.
    fn schema_drift_brittle(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: SchemaDriftInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        Ok(input
            .field_v1
            .unwrap_or_else(|| "MISSING_FIELD".to_owned())
            .into_bytes())
    }

    /// Reads the field named by the declared schema version.
    fn schema_drift_adaptive(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: SchemaDriftInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        let value = match input.schema_version {
            1 => input.field_v1,
            2 => input.field_v2,
            _ => return Err(BAD_INPUT),
        };
        Ok(value.ok_or(BAD_INPUT)?.into_bytes())
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RouteOption {
        name: String,
        capability: String,
        cost: u32,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RoutingInput {
        requires_capability: String,
        routes: Vec<RouteOption>,
    }

    /// Always selects the cheapest route, ignoring whether it can actually
    /// serve the task's capability requirement.
    fn bad_routing_cheapest(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: RoutingInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        let cheapest = input
            .routes
            .iter()
            .min_by_key(|route| route.cost)
            .ok_or(BAD_INPUT)?;
        Ok(cheapest.name.as_bytes().to_vec())
    }

    /// Selects the cheapest route among those that actually have the
    /// required capability.
    fn capability_aware_routing(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: RoutingInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        let capable = input
            .routes
            .iter()
            .filter(|route| route.capability == input.requires_capability)
            .min_by_key(|route| route.cost)
            .ok_or(BAD_INPUT)?;
        Ok(capable.name.as_bytes().to_vec())
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SubagentInput {
        requests: Vec<String>,
    }

    /// Executes every requested subagent task, including exact duplicates,
    /// wasting budget on redundant work.
    fn duplicate_subagents_wasteful(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: SubagentInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        if input.requests.is_empty() {
            return Err(BAD_INPUT);
        }
        Ok(input.requests.join(",").into_bytes())
    }

    /// Deduplicates subagent requests, preserving first-seen order, before
    /// executing them.
    fn deduplicated_subagents(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: SubagentInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        if input.requests.is_empty() {
            return Err(BAD_INPUT);
        }
        let mut seen = Vec::new();
        for request in input.requests {
            if !seen.contains(&request) {
                seen.push(request);
            }
        }
        Ok(seen.join(",").into_bytes())
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MemoryEntry {
        text: String,
        trusted: bool,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MemoryInput {
        memory: Vec<MemoryEntry>,
    }

    /// Returns the most recently written memory entry regardless of its
    /// provenance, so an untrusted, poisoned entry can win.
    fn poisoned_memory_trusting(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: MemoryInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        let last = input.memory.last().ok_or(BAD_INPUT)?;
        Ok(last.text.as_bytes().to_vec())
    }

    /// Returns the most recently written *trusted* memory entry, ignoring
    /// untrusted (potentially poisoned) ones.
    fn provenance_checked_memory(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: MemoryInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        let trusted = input
            .memory
            .iter()
            .rev()
            .find(|entry| entry.trusted)
            .ok_or(BAD_INPUT)?;
        Ok(trusted.text.as_bytes().to_vec())
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct VerificationInput {
        claimed_output: String,
        claimed_status: String,
        actual_state: String,
    }

    /// Trusts a self-reported "success" status and echoes the claimed
    /// output even when it disagrees with the actual ground-truth state.
    fn hallucinated_verification_trusting(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: VerificationInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        if input.claimed_status != "success" {
            return Err(BAD_INPUT);
        }
        Ok(input.claimed_output.into_bytes())
    }

    /// Ignores the self-reported status and always reports the actual
    /// ground-truth state, independently of what was claimed.
    fn ground_truth_verification(text: &str) -> Result<Vec<u8>, RuntimeError> {
        let input: VerificationInput = serde_json::from_str(text).map_err(|_| BAD_INPUT)?;
        Ok(input.actual_state.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::{ReferenceInstruction, execute_reference_worker_request};

    #[test]
    fn strict_documents_reject_unknown_and_duplicate_fields() {
        assert_eq!(
            ReferenceInstruction::parse(
                "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```"
            )
            .expect("identity instruction"),
            ReferenceInstruction::Identity
        );
        for body in [
            "```hephaestus-reference-v1\n{\"schema_version\":1,\"schema_version\":1,\"operation\":\"identity\"}\n```",
            "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\",\"extra\":true}\n```",
            "```hephaestus-reference-v2\n{}\n```",
            "prose",
            "```hephaestus-reference-v1\nnot-json\n```",
            "```hephaestus-reference-v1\n{\"schema_version\":2,\"operation\":\"identity\"}\n```",
            "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"arbitrary\"}\n```",
        ] {
            assert!(ReferenceInstruction::parse(body).is_err());
        }
        let final_lf =
            "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n";
        let crlf = final_lf.replace('\n', "\r\n");
        assert_eq!(
            ReferenceInstruction::parse(final_lf).expect("terminal LF"),
            ReferenceInstruction::Identity
        );
        assert_eq!(
            ReferenceInstruction::parse(&crlf).expect("CRLF"),
            ReferenceInstruction::Identity
        );
        assert!(ReferenceInstruction::parse(&format!("{final_lf}\nextra")).is_err());
    }

    #[test]
    fn worker_applies_operation_without_changing_task_input() {
        let input = b"lowercase task";
        let program = ReferenceInstruction::parse(
            "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"ascii_uppercase\"}\n```",
        )
        .expect("uppercase instruction");
        let frame = program.frame(input).expect("frame");
        assert_eq!(
            execute_reference_worker_request(&frame).expect("worker"),
            b"LOWERCASE TASK"
        );
        let frame = ReferenceInstruction::Identity.frame(input).expect("frame");
        assert_eq!(
            execute_reference_worker_request(&frame).expect("worker"),
            input
        );
    }

    #[test]
    fn worker_rejects_bad_lengths_and_instruction_size_is_bounded() {
        assert!(ReferenceInstruction::parse(&"x".repeat(4097)).is_err());
        assert!(
            ReferenceInstruction::parse(
                "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\r```"
            )
            .is_err()
        );
        assert!(execute_reference_worker_request(b"HPSREF01\x00\x00\x00\x00\x01").is_err());
        assert!(execute_reference_worker_request(b"HPSREF01\x02\x00\x00\x00\x00").is_err());
        assert!(execute_reference_worker_request(b"badmagic!\x00\x00\x00\x00\x00").is_err());
        let oversized = [b"HPSREF01\x00\x00\x10\x00\x01".as_slice(), &[0; 1]].concat();
        assert!(execute_reference_worker_request(&oversized).is_err());
        assert!(
            ReferenceInstruction::Identity
                .frame(&vec![0; 1_048_577])
                .is_err()
        );
    }

    /// Proves the test-only delay trailer actually delays execution by a
    /// real, measurable amount, and that ordinary frames without it are
    /// unaffected.
    #[test]
    #[cfg(feature = "test-support")]
    fn test_delay_trailer_sleeps_before_executing_the_underlying_frame() {
        let input = b"delay me";
        let frame = ReferenceInstruction::Identity
            .frame_with_test_delay(input, 200)
            .expect("delayed frame");
        let started = std::time::Instant::now();
        assert_eq!(
            execute_reference_worker_request(&frame).expect("worker"),
            input
        );
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(180),
            "the delay trailer should have made the worker genuinely sleep"
        );

        // No trailer means no delay and identical behavior to `frame`.
        let plain = ReferenceInstruction::Identity.frame(input).expect("frame");
        let started = std::time::Instant::now();
        assert_eq!(
            execute_reference_worker_request(&plain).expect("worker"),
            input
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(180));
    }
}

/// One test per named Gauntlet failure mode (roadmap item 10): the "bad"
/// operation exhibits the failure on a scenario crafted to expose it, and
/// the paired "fix" operation produces the correct output on the exact same
/// input. This is the unit-level half of "a candidate exhibiting the
/// failure is rejected/fails evaluation and a correct one passes"; the
/// Arena-level half (registered Genomes, `arena evaluate`/`select`) lives in
/// `hephaestus-control`'s gauntlet fixture tests.
#[cfg(test)]
mod gauntlet_tests {
    use super::{ReferenceInstruction, execute_reference_worker_request};

    fn run(instruction: ReferenceInstruction, input: &str) -> Result<Vec<u8>, crate::RuntimeError> {
        let frame = instruction.frame(input.as_bytes()).expect("frame");
        execute_reference_worker_request(&frame)
    }

    #[test]
    fn context_loss() {
        let input = r#"{"turns":["FACT: the deploy key is banana","small talk","more small talk","what is the deploy key?"]}"#;
        assert_eq!(
            run(ReferenceInstruction::ContextLossNaive, input).expect("naive worker runs"),
            b"UNKNOWN"
        );
        assert_eq!(
            run(ReferenceInstruction::ContextLossAware, input).expect("aware worker runs"),
            b" the deploy key is banana"
        );
    }

    #[test]
    fn premature_completion() {
        let input = r#"{"steps":["step1:DONE_A","step2:DONE_B","step3:DONE_C"]}"#;
        assert_eq!(
            run(ReferenceInstruction::PrematureCompletion, input).expect("premature worker runs"),
            b"DONE_A"
        );
        assert_eq!(
            run(ReferenceInstruction::VerifiedCompletion, input).expect("verified worker runs"),
            b"DONE_A,DONE_B,DONE_C"
        );
    }

    #[test]
    fn schema_drift() {
        let drifted = r#"{"schema_version":2,"field_v1":null,"field_v2":"correct-value"}"#;
        assert_eq!(
            run(ReferenceInstruction::SchemaDriftBrittle, drifted).expect("brittle worker runs"),
            b"MISSING_FIELD"
        );
        assert_eq!(
            run(ReferenceInstruction::SchemaDriftAdaptive, drifted).expect("adaptive worker runs"),
            b"correct-value"
        );
    }

    #[test]
    fn bad_routing() {
        let input = r#"{"requires_capability":"large_context","routes":[{"name":"cheap","capability":"small","cost":1},{"name":"expensive","capability":"large_context","cost":9}]}"#;
        assert_eq!(
            run(ReferenceInstruction::BadRoutingCheapest, input).expect("cheapest worker runs"),
            b"cheap"
        );
        assert_eq!(
            run(ReferenceInstruction::CapabilityAwareRouting, input)
                .expect("capability-aware worker runs"),
            b"expensive"
        );
    }

    #[test]
    fn duplicate_subagents() {
        let input = r#"{"requests":["task-a","task-a","task-b"]}"#;
        assert_eq!(
            run(ReferenceInstruction::DuplicateSubagentsWasteful, input)
                .expect("wasteful worker runs"),
            b"task-a,task-a,task-b"
        );
        assert_eq!(
            run(ReferenceInstruction::DeduplicatedSubagents, input)
                .expect("deduplicated worker runs"),
            b"task-a,task-b"
        );
    }

    #[test]
    fn poisoned_memory() {
        let input = r#"{"memory":[{"text":"correct-fact","trusted":true},{"text":"malicious-fact","trusted":false}]}"#;
        assert_eq!(
            run(ReferenceInstruction::PoisonedMemoryTrusting, input)
                .expect("poisoned-trusting worker runs"),
            b"malicious-fact"
        );
        assert_eq!(
            run(ReferenceInstruction::ProvenanceCheckedMemory, input)
                .expect("provenance-checked worker runs"),
            b"correct-fact"
        );
    }

    #[test]
    fn hallucinated_verification() {
        let input = r#"{"claimed_output":"success-value","claimed_status":"success","actual_state":"actual-value"}"#;
        assert_eq!(
            run(
                ReferenceInstruction::HallucinatedVerificationTrusting,
                input
            )
            .expect("hallucinating worker runs"),
            b"success-value"
        );
        assert_eq!(
            run(ReferenceInstruction::GroundTruthVerification, input)
                .expect("ground-truth worker runs"),
            b"actual-value"
        );
    }

    #[test]
    fn every_gauntlet_operation_round_trips_through_its_document_and_frame() {
        for instruction in [
            ReferenceInstruction::ContextLossNaive,
            ReferenceInstruction::ContextLossAware,
            ReferenceInstruction::PrematureCompletion,
            ReferenceInstruction::VerifiedCompletion,
            ReferenceInstruction::SchemaDriftBrittle,
            ReferenceInstruction::SchemaDriftAdaptive,
            ReferenceInstruction::BadRoutingCheapest,
            ReferenceInstruction::CapabilityAwareRouting,
            ReferenceInstruction::DuplicateSubagentsWasteful,
            ReferenceInstruction::DeduplicatedSubagents,
            ReferenceInstruction::PoisonedMemoryTrusting,
            ReferenceInstruction::ProvenanceCheckedMemory,
            ReferenceInstruction::HallucinatedVerificationTrusting,
            ReferenceInstruction::GroundTruthVerification,
        ] {
            let document = format!(
                "```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{}\"}}\n```",
                instruction.operation_name()
            );
            assert_eq!(
                ReferenceInstruction::parse(&document).expect("parses its own document"),
                instruction
            );
        }
    }
}
