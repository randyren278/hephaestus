//! Small, bounded instruction language for the offline reference worker.

use serde::Deserialize;

use crate::RuntimeError;

const MAX_INSTRUCTION_BYTES: usize = 4096;
pub(crate) const MAX_TASK_INPUT_BYTES: usize = 1_048_576;
const FRAME_MAGIC: &[u8; 8] = b"HPSREF01";

/// Deterministic operation selected by a registered Genome's reserved prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceInstruction {
    /// Return the exact World task input bytes.
    Identity,
    /// Apply ASCII uppercase to the exact World task input bytes.
    AsciiUppercase,
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
            _ => Err(RuntimeError::InvalidSpec(
                "reference instruction operation is unsupported",
            )),
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
        });
        frame.extend_from_slice(&length.to_be_bytes());
        frame.extend_from_slice(input);
        Ok(frame)
    }
}

/// Executes a validated reference worker frame. The worker has no filesystem or network behavior.
///
/// # Errors
///
/// Rejects truncated, oversized, trailing, or unknown-version frames.
pub fn execute_reference_worker_request(frame: &[u8]) -> Result<Vec<u8>, RuntimeError> {
    const HEADER: usize = 13;
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
        _ => Err(RuntimeError::InvalidSpec(
            "reference worker operation is unsupported",
        )),
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
}
