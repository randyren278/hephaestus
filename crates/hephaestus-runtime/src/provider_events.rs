//! Maps Codex and Claude Code NDJSON event streams onto the provider-neutral
//! [`RuntimeObservation`] contract and extracts the final-answer text and
//! reported cost used by signed run results.
//!
//! Exact event names and fields are cited in `docs/RUNTIMES.md`. Anything the
//! stream does not expose (for example Codex's per-run USD cost) is left at
//! its neutral default rather than guessed.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{Provider, RuntimeObservation, RuntimeObservationKind};

const MAX_LINE_BYTES: usize = 1_048_576;

/// Incremental line-buffered cursor over one provider's growing NDJSON stdout.
///
/// Feed it the bytes captured by the supervisor since the previous call; it
/// keeps any trailing partial line for the next feed and returns the
/// observations parsed from every complete line seen so far.
#[derive(Default)]
pub struct ProviderEventCursor {
    partial: Vec<u8>,
}

impl ProviderEventCursor {
    /// Creates an empty cursor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses every complete line newly available in `chunk`.
    ///
    /// A line longer than the bounded maximum, or one that is not valid JSON,
    /// is reported as a single bounded `Error` observation instead of being
    /// silently dropped or causing a panic.
    pub fn feed(&mut self, provider: Provider, chunk: &[u8]) -> Vec<RuntimeObservation> {
        self.partial.extend_from_slice(chunk);
        let mut observations = Vec::new();
        loop {
            let Some(newline) = self.partial.iter().position(|byte| *byte == b'\n') else {
                if self.partial.len() > MAX_LINE_BYTES {
                    observations.push(malformed_line_observation("line exceeds bound"));
                    self.partial.clear();
                }
                break;
            };
            let line: Vec<u8> = self.partial.drain(..=newline).collect();
            let line = &line[..line.len() - 1];
            if line.trim_ascii().is_empty() {
                continue;
            }
            if line.len() > MAX_LINE_BYTES {
                observations.push(malformed_line_observation("line exceeds bound"));
                continue;
            }
            match serde_json::from_slice::<Value>(line) {
                Ok(value) => observations.extend(parse_event(provider, &value)),
                Err(_) => observations.push(malformed_line_observation("line is not valid JSON")),
            }
        }
        observations
    }
}

fn malformed_line_observation(reason: &'static str) -> RuntimeObservation {
    RuntimeObservation::new(
        RuntimeObservationKind::Error,
        BTreeMap::from([
            ("source".to_owned(), "provider_stream".to_owned()),
            ("reason".to_owned(), reason.to_owned()),
        ]),
    )
}

fn parse_event(provider: Provider, value: &Value) -> Vec<RuntimeObservation> {
    match provider {
        Provider::Codex => parse_codex_event(value),
        Provider::Claude => parse_claude_event(value),
        Provider::Deterministic => Vec::new(),
    }
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

// --- Codex `codex exec --json` events -------------------------------------
// Event names and fields: `thread.started`, `turn.started`, `turn.completed`
// (with `usage`), `turn.failed` (with `error`), `item.started`/`item.completed`
// (with `item.type` in `agent_message`, `command_execution`, `file_change`,
// `mcp_tool_call`). See docs/RUNTIMES.md for sources.
fn parse_codex_event(value: &Value) -> Vec<RuntimeObservation> {
    let Some(event_type) = value.get("type").and_then(Value::as_str) else {
        return vec![malformed_line_observation("codex event has no type")];
    };
    match event_type {
        "item.completed" | "item.started" => {
            let Some(item) = value.get("item") else {
                return Vec::new();
            };
            let item_type = item.get("item_type").or_else(|| item.get("type"));
            let item_type = item_type.and_then(Value::as_str).unwrap_or("unknown");
            let mut fields = BTreeMap::from([
                ("provider".to_owned(), "codex".to_owned()),
                ("item_type".to_owned(), item_type.to_owned()),
                ("event".to_owned(), event_type.to_owned()),
            ]);
            if let Some(text) = text_field(item, "text") {
                fields.insert("text".to_owned(), text);
            }
            if let Some(command) = text_field(item, "command") {
                fields.insert("command".to_owned(), command);
            }
            let kind = match item_type {
                "command_execution" => {
                    if event_type == "item.completed" {
                        RuntimeObservationKind::ToolResult
                    } else {
                        RuntimeObservationKind::ToolCalled
                    }
                }
                "mcp_tool_call" => {
                    if event_type == "item.completed" {
                        RuntimeObservationKind::ToolResult
                    } else {
                        RuntimeObservationKind::ToolCalled
                    }
                }
                "file_change" => RuntimeObservationKind::FileChanged,
                "agent_message" => RuntimeObservationKind::ModelResponse,
                _ => RuntimeObservationKind::ModelResponse,
            };
            vec![RuntimeObservation::new(kind, fields)]
        }
        "turn.completed" => {
            let mut fields = BTreeMap::from([("provider".to_owned(), "codex".to_owned())]);
            if let Some(usage) = value.get("usage") {
                for key in [
                    "input_tokens",
                    "cached_input_tokens",
                    "output_tokens",
                    "reasoning_output_tokens",
                ] {
                    if let Some(count) = usage.get(key) {
                        fields.insert(key.to_owned(), count.to_string());
                    }
                }
            }
            // Codex does not report a per-run USD figure in its event stream, so
            // the run result's actual cost stays zero; only token counts are kept.
            fields.insert("actual_cost_microusd".to_owned(), "0".to_owned());
            vec![RuntimeObservation::new(
                RuntimeObservationKind::CostObserved,
                fields,
            )]
        }
        "turn.failed" => {
            let mut fields = BTreeMap::from([("provider".to_owned(), "codex".to_owned())]);
            if let Some(error) = value.get("error") {
                fields.insert("error".to_owned(), error.to_string());
            }
            vec![RuntimeObservation::new(
                RuntimeObservationKind::Error,
                fields,
            )]
        }
        _ => Vec::new(),
    }
}

// --- Claude Code `claude -p --output-format stream-json` events -----------
// Event/message shapes: `assistant`/`user` messages (with `content` blocks of
// type `tool_use`/`tool_result`, and `parent_tool_use_id` for subagents),
// `system` messages (`subtype: "init"`, `subtype: "permission_denied"`), and a
// terminal `result` message carrying `total_cost_usd`, `result`, and
// `session_id`. See docs/RUNTIMES.md for sources.
fn parse_claude_event(value: &Value) -> Vec<RuntimeObservation> {
    let Some(message_type) = value.get("type").and_then(Value::as_str) else {
        return vec![malformed_line_observation("claude event has no type")];
    };
    match message_type {
        "system" => parse_claude_system_event(value),
        "assistant" | "user" => parse_claude_conversation_event(value),
        "result" => parse_claude_result_event(value),
        _ => Vec::new(),
    }
}

fn parse_claude_system_event(value: &Value) -> Vec<RuntimeObservation> {
    let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
    match subtype {
        "init" => {
            let mut fields = BTreeMap::from([("provider".to_owned(), "claude".to_owned())]);
            if let Some(session_id) = text_field(value, "session_id") {
                fields.insert("session_id".to_owned(), session_id);
            }
            vec![RuntimeObservation::new(
                RuntimeObservationKind::ContextComposed,
                fields,
            )]
        }
        "permission_denied" => {
            let mut fields = BTreeMap::from([
                ("provider".to_owned(), "claude".to_owned()),
                ("denied".to_owned(), "true".to_owned()),
            ]);
            if let Some(tool) = text_field(value, "tool_name") {
                fields.insert("tool_name".to_owned(), tool);
            }
            vec![RuntimeObservation::new(
                RuntimeObservationKind::Error,
                fields,
            )]
        }
        _ => Vec::new(),
    }
}

fn parse_claude_conversation_event(value: &Value) -> Vec<RuntimeObservation> {
    let mut observations = Vec::new();
    let parent_tool_use_id = value.get("parent_tool_use_id").and_then(Value::as_str);
    if let Some(parent) = parent_tool_use_id {
        observations.push(RuntimeObservation::new(
            RuntimeObservationKind::SubagentSpawned,
            BTreeMap::from([
                ("provider".to_owned(), "claude".to_owned()),
                ("parent_tool_use_id".to_owned(), parent.to_owned()),
            ]),
        ));
    }
    let Some(content) = value.pointer("/message/content").and_then(Value::as_array) else {
        return observations;
    };
    for block in content {
        let Some(block_type) = block.get("type").and_then(Value::as_str) else {
            continue;
        };
        match block_type {
            "tool_use" => {
                let mut fields = BTreeMap::from([("provider".to_owned(), "claude".to_owned())]);
                if let Some(name) = text_field(block, "name") {
                    fields.insert("tool_name".to_owned(), name);
                }
                if let Some(input) = block.get("input") {
                    fields.insert("tool_input".to_owned(), input.to_string());
                }
                observations.push(RuntimeObservation::new(
                    RuntimeObservationKind::ToolCalled,
                    fields,
                ));
            }
            "tool_result" => {
                let mut fields = BTreeMap::from([("provider".to_owned(), "claude".to_owned())]);
                if let Some(text) = block.get("content").map(ToString::to_string) {
                    fields.insert("tool_result".to_owned(), text);
                }
                if let Some(is_error) = block.get("is_error") {
                    fields.insert("is_error".to_owned(), is_error.to_string());
                }
                observations.push(RuntimeObservation::new(
                    RuntimeObservationKind::ToolResult,
                    fields,
                ));
            }
            "text" if parent_tool_use_id.is_none() => {
                if let Some(text) = text_field(block, "text") {
                    observations.push(RuntimeObservation::new(
                        RuntimeObservationKind::ModelResponse,
                        BTreeMap::from([
                            ("provider".to_owned(), "claude".to_owned()),
                            ("text".to_owned(), text),
                        ]),
                    ));
                }
            }
            _ => {}
        }
    }
    observations
}

fn parse_claude_result_event(value: &Value) -> Vec<RuntimeObservation> {
    let mut fields = BTreeMap::from([("provider".to_owned(), "claude".to_owned())]);
    let cost_microusd = value
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .map_or(0, usd_to_microusd);
    fields.insert("actual_cost_microusd".to_owned(), cost_microusd.to_string());
    if let Some(subtype) = text_field(value, "subtype") {
        fields.insert("subtype".to_owned(), subtype);
    }
    vec![RuntimeObservation::new(
        RuntimeObservationKind::CostObserved,
        fields,
    )]
}

fn usd_to_microusd(dollars: f64) -> u64 {
    if !dollars.is_finite() || dollars <= 0.0 {
        return 0;
    }
    let scaled = dollars * 1_000_000.0;
    if scaled >= u64::MAX as f64 {
        u64::MAX
    } else {
        scaled.round() as u64
    }
}

/// Extracts the final-answer text a signed run result should record as
/// stdout: Codex's last `agent_message` item, or Claude's terminal `result`
/// message's `result` field. Returns an empty string when the stream never
/// reports one (for example a provider crash before any output).
#[must_use]
pub fn extract_final_answer(provider: Provider, ndjson: &[u8]) -> Vec<u8> {
    let mut answer = String::new();
    for line in ndjson.split(|byte| *byte == b'\n') {
        if line.trim_ascii().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        match provider {
            Provider::Codex => {
                if value.get("type").and_then(Value::as_str) == Some("item.completed") {
                    let item = value.get("item");
                    let item_type = item
                        .and_then(|item| item.get("item_type").or_else(|| item.get("type")))
                        .and_then(Value::as_str);
                    if item_type == Some("agent_message")
                        && let Some(text) = item.and_then(|item| text_field(item, "text"))
                    {
                        answer = text;
                    }
                }
            }
            Provider::Claude => {
                if value.get("type").and_then(Value::as_str) == Some("result")
                    && let Some(text) = text_field(&value, "result")
                {
                    answer = text;
                }
            }
            Provider::Deterministic => {}
        }
    }
    answer.into_bytes()
}

/// Extracts the total reported cost in micro-US-dollars from a completed
/// provider stream. Codex does not report a dollar figure, so its result is
/// always zero.
#[must_use]
pub fn extract_actual_cost_microusd(provider: Provider, ndjson: &[u8]) -> u64 {
    if provider != Provider::Claude {
        return 0;
    }
    let mut cost = 0;
    for line in ndjson.split(|byte| *byte == b'\n') {
        if line.trim_ascii().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("result")
            && let Some(dollars) = value.get("total_cost_usd").and_then(Value::as_f64)
        {
            cost = usd_to_microusd(dollars);
        }
    }
    cost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_buffers_partial_lines_and_flags_malformed_ones() {
        let mut cursor = ProviderEventCursor::new();
        let first = cursor.feed(Provider::Claude, br#"{"type":"system","subtype":"init""#);
        assert!(first.is_empty());
        let second = cursor.feed(Provider::Claude, b"}\nnot json\n");
        assert_eq!(second.len(), 2);
        assert_eq!(second[0].kind, RuntimeObservationKind::ContextComposed);
        assert_eq!(second[1].kind, RuntimeObservationKind::Error);
    }

    #[test]
    fn claude_tool_use_and_result_map_to_expected_kinds() {
        let mut cursor = ProviderEventCursor::new();
        let line = br#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","name":"Read","input":{"path":"a"}}]}}
"#;
        let observations = cursor.feed(Provider::Claude, line);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].kind, RuntimeObservationKind::ToolCalled);
        assert_eq!(observations[0].fields["tool_name"], "Read");
    }

    #[test]
    fn claude_subagent_message_is_reported() {
        let mut cursor = ProviderEventCursor::new();
        let line = br#"{"type":"user","parent_tool_use_id":"tool-1","message":{"content":[]}}
"#;
        let observations = cursor.feed(Provider::Claude, line);
        assert_eq!(observations.len(), 1);
        assert_eq!(
            observations[0].kind,
            RuntimeObservationKind::SubagentSpawned
        );
        assert_eq!(observations[0].fields["parent_tool_use_id"], "tool-1");
    }

    #[test]
    fn claude_final_answer_and_cost_come_from_the_result_event() {
        let ndjson = b"{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"42\",\"total_cost_usd\":0.0031}\n";
        assert_eq!(extract_final_answer(Provider::Claude, ndjson), b"42");
        assert_eq!(extract_actual_cost_microusd(Provider::Claude, ndjson), 3100);
    }

    #[test]
    fn codex_final_answer_comes_from_the_last_agent_message_and_cost_is_zero() {
        let ndjson = b"{\"type\":\"item.completed\",\"item\":{\"item_type\":\"agent_message\",\"text\":\"done\"}}\n{\"type\":\"turn.completed\",\"usage\":{\"output_tokens\":5}}\n";
        assert_eq!(extract_final_answer(Provider::Codex, ndjson), b"done");
        assert_eq!(extract_actual_cost_microusd(Provider::Codex, ndjson), 0);
        let mut cursor = ProviderEventCursor::new();
        let observations = cursor.feed(Provider::Codex, ndjson);
        assert!(
            observations
                .iter()
                .any(|observation| observation.kind == RuntimeObservationKind::CostObserved)
        );
    }

    #[test]
    fn codex_permission_and_command_failure_events_map_to_tool_and_error_kinds() {
        let mut cursor = ProviderEventCursor::new();
        let line = b"{\"type\":\"item.started\",\"item\":{\"item_type\":\"command_execution\",\"command\":\"ls\"}}\n{\"type\":\"turn.failed\",\"error\":{\"message\":\"boom\"}}\n";
        let observations = cursor.feed(Provider::Codex, line);
        assert_eq!(observations[0].kind, RuntimeObservationKind::ToolCalled);
        assert_eq!(observations[1].kind, RuntimeObservationKind::Error);
    }

    #[test]
    fn empty_stream_yields_empty_answer_and_zero_cost() {
        assert_eq!(
            extract_final_answer(Provider::Claude, b""),
            Vec::<u8>::new()
        );
        assert_eq!(extract_actual_cost_microusd(Provider::Claude, b""), 0);
    }
}
