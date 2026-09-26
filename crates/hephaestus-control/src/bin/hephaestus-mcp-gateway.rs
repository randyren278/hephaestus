//! Model Context Protocol (MCP) gateway for Hephaestus.
//!
//! Implements the JSON-RPC 2.0 messages an MCP client needs over the stdio
//! transport (`initialize`, `notifications/initialized`, `tools/list`,
//! `tools/call`) per the MCP specification, revision 2025-06-18
//! (<https://modelcontextprotocol.io/specification/2025-06-18>). See
//! `docs/MCP_GATEWAY.md` for the exact tool set, capability policy format, and
//! ledgering behavior.
//!
//! Every tool call is routed through the daemon's ordinary authenticated
//! operator API as `Command::McpCall`, which the daemon ledgers
//! unconditionally (including a denial) and, only when the gateway's
//! capability policy allows it, dispatches through the exact same command
//! path any other operator client uses. This gateway never touches
//! canonical storage directly.

use std::{
    collections::BTreeSet,
    io::{self, BufRead, Write},
    path::PathBuf,
};

use clap::Parser;
use hephaestus_control::{Client, Command, McpDecision, ResponseData, data_dir_from_environment};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SERVER_NAME: &str = "hephaestus-mcp-gateway";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "hephaestus-mcp-gateway",
    about = "MCP gateway exposing Hephaestus capabilities as versioned tools over stdio"
)]
struct Arguments {
    /// Canonical daemon data directory this gateway routes calls through.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Path to this client's capability policy JSON file.
    #[arg(long)]
    policy: PathBuf,
}

/// One connected MCP client's declared capability policy: which tools it
/// may call at all, and which of those mutating tools it has additionally
/// been granted. Read-only tools need only appear in `allow`; a mutating
/// tool also needs to appear in `grants`. Unlisted tools are denied by
/// default.
#[derive(Deserialize)]
struct ClientPolicy {
    client_id: String,
    #[serde(default)]
    allow: BTreeSet<String>,
    #[serde(default)]
    grants: BTreeSet<String>,
}

struct Tool {
    name: &'static str,
    version: u16,
    mutating: bool,
    description: &'static str,
    input_schema: fn() -> Value,
    build: fn(&Value) -> Result<Command, String>,
}

fn string_arg(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("argument '{key}' is required and must be a string"))
}

fn u32_arg_or(arguments: &Value, key: &str, default: u32) -> Result<u32, String> {
    match arguments.get(key) {
        None => Ok(default),
        Some(value) => u32::try_from(
            value
                .as_u64()
                .ok_or_else(|| format!("argument '{key}' must be a non-negative integer"))?,
        )
        .map_err(|_| format!("argument '{key}' is out of range")),
    }
}

fn bool_arg_or(arguments: &Value, key: &str, default: bool) -> Result<bool, String> {
    match arguments.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("argument '{key}' must be a boolean")),
    }
}

fn empty_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn id_schema(field: &str) -> Value {
    json!({
        "type": "object",
        "properties": { field: { "type": "string" } },
        "required": [field],
        "additionalProperties": false
    })
}

fn limit_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 200 } },
        "additionalProperties": false
    })
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "status",
        version: 1,
        mutating: false,
        description: "Inspect the current canonical daemon projection.",
        input_schema: empty_schema,
        build: |_| Ok(Command::Status),
    },
    Tool {
        name: "world_list",
        version: 1,
        mutating: false,
        description: "List every registered immutable World.",
        input_schema: empty_schema,
        build: |_| Ok(Command::WorldList),
    },
    Tool {
        name: "world_show",
        version: 1,
        mutating: false,
        description: "Inspect one registered World by identity.",
        input_schema: || id_schema("world_id"),
        build: |arguments| {
            Ok(Command::WorldShow {
                world_id: string_arg(arguments, "world_id")?,
            })
        },
    },
    Tool {
        name: "genome_list",
        version: 1,
        mutating: false,
        description: "List every registered immutable Genome.",
        input_schema: empty_schema,
        build: |_| Ok(Command::GenomeList),
    },
    Tool {
        name: "genome_show",
        version: 1,
        mutating: false,
        description: "Inspect one registered Genome by identity.",
        input_schema: || id_schema("genome_id"),
        build: |arguments| {
            Ok(Command::GenomeShow {
                genome_id: string_arg(arguments, "genome_id")?,
            })
        },
    },
    Tool {
        name: "champion_show",
        version: 1,
        mutating: false,
        description: "Reconstruct the Champion projection and transition history of one World.",
        input_schema: || id_schema("world_id"),
        build: |arguments| {
            Ok(Command::ChampionShow {
                world_id: string_arg(arguments, "world_id")?,
            })
        },
    },
    Tool {
        name: "gene_list",
        version: 1,
        mutating: false,
        description: "List every extracted Gene with its aggregate transfer counts.",
        input_schema: empty_schema,
        build: |_| Ok(Command::GeneList),
    },
    Tool {
        name: "gene_show",
        version: 1,
        mutating: false,
        description: "Inspect one Gene, its transfer trials, and any contradiction or species.",
        input_schema: || id_schema("gene_id"),
        build: |arguments| {
            Ok(Command::GeneShow {
                gene_id: string_arg(arguments, "gene_id")?,
            })
        },
    },
    Tool {
        name: "run_list",
        version: 1,
        mutating: false,
        description: "List recent direct runs and jobs, newest first.",
        input_schema: limit_schema,
        build: |arguments| {
            Ok(Command::RunList {
                limit: u32_arg_or(arguments, "limit", 20)?,
            })
        },
    },
    Tool {
        name: "evaluation_list",
        version: 1,
        mutating: false,
        description: "List recent Arena evaluations, newest first.",
        input_schema: limit_schema,
        build: |arguments| {
            Ok(Command::EvaluationList {
                limit: u32_arg_or(arguments, "limit", 20)?,
            })
        },
    },
    Tool {
        name: "denial_list",
        version: 1,
        mutating: false,
        description: "List recent refused operator requests and recorded runtime denials.",
        input_schema: limit_schema,
        build: |arguments| {
            Ok(Command::DenialList {
                limit: u32_arg_or(arguments, "limit", 20)?,
            })
        },
    },
    Tool {
        name: "arena_evaluate",
        version: 1,
        mutating: true,
        description: "Run one trusted parent-versus-candidate Arena evaluation.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "evaluation_id": { "type": "string" },
                    "parent_genome_id": { "type": "string" },
                    "candidate_genome_id": { "type": "string" },
                    "remote": {
                        "type": "boolean",
                        "description": "Lease each reference-role trial to a remote worker instead of running it locally. Defaults to false."
                    }
                },
                "required": ["evaluation_id", "parent_genome_id", "candidate_genome_id"],
                "additionalProperties": false
            })
        },
        build: |arguments| {
            Ok(Command::EvaluatePair {
                evaluation_id: string_arg(arguments, "evaluation_id")?,
                parent_genome_id: string_arg(arguments, "parent_genome_id")?,
                candidate_genome_id: string_arg(arguments, "candidate_genome_id")?,
                remote: bool_arg_or(arguments, "remote", false)?,
            })
        },
    },
    Tool {
        name: "arena_select",
        version: 1,
        mutating: true,
        description: "Select from one exact persisted Arena evaluation using its World's policy.",
        input_schema: || id_schema("evaluation_id"),
        build: |arguments| {
            Ok(Command::ArenaSelect {
                evaluation_id: string_arg(arguments, "evaluation_id")?,
            })
        },
    },
    Tool {
        name: "genome_propose",
        version: 1,
        mutating: true,
        description: "Propose one compiler-validated prompt mutation from a trusted selection.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "proposal_id": { "type": "string" },
                    "selection_event_id": { "type": "string" },
                    "parent_genome_id": { "type": "string" },
                    "hypothesis": { "type": "string" }
                },
                "required": ["proposal_id", "selection_event_id", "parent_genome_id", "hypothesis"],
                "additionalProperties": false
            })
        },
        build: |arguments| {
            Ok(Command::GenomePropose {
                proposal_id: string_arg(arguments, "proposal_id")?,
                selection_event_id: string_arg(arguments, "selection_event_id")?,
                parent_genome_id: string_arg(arguments, "parent_genome_id")?,
                hypothesis: Some(string_arg(arguments, "hypothesis")?),
                analysis_id: None,
                cluster_index: None,
            })
        },
    },
    Tool {
        name: "genome_assess",
        version: 1,
        mutating: true,
        description: "Assess one proposed child against a verified Arena selection receipt.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "assessment_id": { "type": "string" },
                    "proposal_id": { "type": "string" },
                    "selection_event_id": { "type": "string" }
                },
                "required": ["assessment_id", "proposal_id", "selection_event_id"],
                "additionalProperties": false
            })
        },
        build: |arguments| {
            Ok(Command::GenomeAssess {
                assessment_id: string_arg(arguments, "assessment_id")?,
                proposal_id: string_arg(arguments, "proposal_id")?,
                selection_event_id: string_arg(arguments, "selection_event_id")?,
            })
        },
    },
];

fn find_tool(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|tool| tool.name == name)
}

/// The gateway's own capability-policy decision, made before ever
/// contacting the daemon: `Some(reason)` refuses the call outright (a
/// denial the daemon still ledgers unconditionally); `None` allows it,
/// pending only argument validation while building the wrapped command.
fn policy_denial(policy: &ClientPolicy, tool: &Tool) -> Option<String> {
    if !policy.allow.contains(tool.name) {
        return Some(format!(
            "client '{}' is not allowed to call '{}'",
            policy.client_id, tool.name
        ));
    }
    if tool.mutating && !policy.grants.contains(tool.name) {
        return Some(format!(
            "client '{}' has not been granted the mutating tool '{}'",
            policy.client_id, tool.name
        ));
    }
    None
}

#[derive(Deserialize)]
struct RpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code: i64,
    message: String,
}

fn main() -> io::Result<()> {
    let arguments = Arguments::parse();
    let data_dir = arguments
        .data_dir
        .unwrap_or_else(|| data_dir_from_environment().unwrap_or_default());
    let policy_bytes = std::fs::read(&arguments.policy)?;
    let policy: ClientPolicy = match serde_json::from_slice(&policy_bytes) {
        Ok(policy) => policy,
        Err(error) => {
            eprintln!("hephaestus-mcp-gateway: invalid capability policy: {error}");
            std::process::exit(1);
        }
    };
    let client = Client::new(data_dir);

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<RpcRequest>(trimmed) else {
            // Per JSON-RPC 2.0, a line that cannot even be parsed into a
            // request still gets an error response (id is unknowable, so it
            // is `null`), rather than being silently dropped; this keeps
            // the gateway a well-behaved JSON-RPC peer for a client that
            // mis-frames one message.
            let envelope = RpcResponse {
                jsonrpc: "2.0",
                id: Value::Null,
                result: None,
                error: Some(RpcError {
                    code: -32700,
                    message: "parse error".to_owned(),
                }),
            };
            writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
            stdout.flush()?;
            continue;
        };
        let _ = request.jsonrpc;
        if request.method == "notifications/initialized" || request.id.is_none() {
            // Notifications never receive a response.
            continue;
        }
        let id = request.id.clone().unwrap_or(Value::Null);
        let response = handle_request(&client, &policy, &request.method, &request.params);
        let envelope = match response {
            Ok(result) => RpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(result),
                error: None,
            },
            Err((code, message)) => RpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(RpcError { code, message }),
            },
        };
        writeln!(stdout, "{}", serde_json::to_string(&envelope)?)?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle_request(
    client: &Client,
    policy: &ClientPolicy,
    method: &str,
    params: &Value,
) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let tools: Vec<Value> = TOOLS
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": format!("{} (v{})", tool.description, tool.version),
                        "inputSchema": (tool.input_schema)(),
                    })
                })
                .collect();
            Ok(json!({ "tools": tools }))
        }
        "tools/call" => tools_call(client, policy, params),
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn tools_call(
    client: &Client,
    policy: &ClientPolicy,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| (-32602, "params.name is required".to_owned()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let Some(tool) = find_tool(name) else {
        return Err((-32602, format!("unknown tool: {name}")));
    };

    let decision = if let Some(reason) = policy_denial(policy, tool) {
        McpDecision::Denied { reason }
    } else {
        match (tool.build)(&arguments) {
            Ok(command) => McpDecision::Allowed {
                command: Box::new(command),
            },
            Err(message) => return Ok(tool_error_result(&message)),
        }
    };

    let mcp_call = Command::McpCall {
        client_id: policy.client_id.clone(),
        tool: tool.name.to_owned(),
        tool_version: tool.version,
        decision,
    };
    let response = client
        .request(mcp_call)
        .map_err(|error| (-32000, format!("daemon request failed: {error}")))?;
    if let Some(error) = response.error {
        return Ok(tool_error_result(&error.message));
    }
    let Some(data) = response.data else {
        return Ok(tool_error_result("daemon returned an empty response"));
    };
    match data {
        ResponseData::McpDenied { reason } => Ok(tool_error_result(&reason)),
        other => {
            let text = serde_json::to_string(&other)
                .map_err(|error| (-32000, format!("response could not be serialized: {error}")))?;
            Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": false }))
        }
    }
}

fn tool_error_result(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}
