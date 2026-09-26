# MCP Gateway

`hephaestus-mcp-gateway` exposes a fixed, versioned set of Hephaestus
capabilities to a Model Context Protocol (MCP) client (an agent) over the
stdio transport. It never touches canonical storage: every tool call is
routed through the exact same authenticated operator API
(`crates/hephaestus-control`, control.sock) the CLI and TUI use, wrapped in
one new operator command, `Command::McpCall`, so a tool call and an
equivalent CLI invocation are dispatched through the identical code path.

## Protocol

Implements the JSON-RPC 2.0 messages defined by the MCP specification,
revision **2025-06-18** (<https://modelcontextprotocol.io/specification/2025-06-18>):

- `initialize` — negotiates protocol version `2025-06-18` and declares the
  `tools` capability.
- notifications/initialized — accepted, produces no response (per spec,
  notifications never receive one).
- tools/list — returns the fixed tool set below, each with its JSON Schema
  `inputSchema` and a `(v<N>)` version suffix in its description.
- tools/call — invokes one tool by name with JSON arguments; returns
  `{"content": [{"type": "text", "text": ...}], "isError": bool}` per the
  spec's unstructured tool result format.

Messages are newline-delimited JSON on stdin/stdout, matching the stdio
transport's framing.

## Tool set (schema version 1 for every tool)

Read-only (always eligible for a client's `allow` list):

| Tool | Daemon command |
|---|---|
| `status` | `Status` |
| `world_list` | `WorldList` |
| `world_show` | `WorldShow` |
| `genome_list` | `GenomeList` |
| `genome_show` | `GenomeShow` |
| `champion_show` | `ChampionShow` |
| `gene_list` | `GeneList` |
| `gene_show` | `GeneShow` |
| `run_list` | `RunList` |
| `evaluation_list` | `EvaluationList` |
| `denial_list` | `DenialList` |

Mutating (need an explicit grant in addition to `allow`):

| Tool | Daemon command |
|---|---|
| `arena_evaluate` | `EvaluatePair` |
| `arena_select` | `ArenaSelect` |
| `genome_propose` | `GenomePropose` (operator-hypothesis path only) |
| `genome_assess` | `GenomeAssess` |

Each tool call is versioned (`tool_version`); a future breaking change adds
a new tool name or a new `tool_version`, never a silent behavior change
under the same version.

## Capability policy

Each connected client (an agent) is bound to exactly one JSON policy file,
passed with `--policy`:

```json
{
  "client_id": "my-agent",
  "allow": ["status", "world_list", "genome_list", "arena_evaluate"],
  "grants": ["arena_evaluate"]
}
```

- `allow` is the read-only-by-default allowlist: a tool absent from `allow`
  is refused regardless of anything else.
- `grants` additionally authorizes a *mutating* tool. A mutating tool present
  in `allow` but absent from `grants` is still refused — the operator must
  name it in both places to grant write access.
- An unrecognized tool name is refused.

This decision is made by the gateway process itself, before it ever
contacts the daemon, because the daemon has no notion of "MCP client" —
only the gateway does. The daemon is still the sole enforcement point for
every domain invariant a tool call reaches (frozen state, World law,
authority ceilings, and so on); the capability policy only decides whether
the gateway is willing to *attempt* the call.

## Ledgering (including denials)

Every tool call — allowed or denied — is sent to the daemon as one
`Command::McpCall { client_id, tool, tool_version, decision }` request:

- `decision: Denied { reason }` — the daemon appends one mcp.call audit
  event (via the same `append_audit` path every operator command uses) and
  returns `ResponseData::McpDenied` without dispatching anything further.
  Nothing is ever executed for a denied call.
- `decision: Allowed { command }` — the daemon appends the same mcp.call
  audit event, *then* dispatches `command` through the ordinary `execute`
  path recursively, so the wrapped command's own natural event (for
  example control.status, or a mutating command's own domain event) is
  appended immediately afterward. A nested `McpCall` inside `Allowed` is
  rejected (`ExecuteError::Invalid`) to prevent recursion tricks.

Because every authenticated request is already ledgered before dispatch
(`docs/CONTROL_PLANE.md`), this means an MCP tool call always produces at
least one canonical event, and a successful mutating call produces two:
the mcp.call audit record and the wrapped command's own event. `denials`
lists refused mcp.call events (`DenialKind::McpCallDenied`) alongside the
denial kinds the CLI and TUI already show, including the client's
`client_id`.

Both event types are replayed and hash-verified like every other canonical
event; mcp.call carries no additional domain-projection semantics beyond
the audit trail itself.

## Running it

```sh
cargo run -p hephaestus-control --bin hephaestus-mcp-gateway -- \
  --data-dir "$HEPHAESTUS_HOME" \
  --policy /path/to/policy.json
```

Point an MCP-capable client at this binary over stdio. It reads the
daemon's operator.token from the data directory exactly as the CLI does
(`crates/hephaestus-control/src/client.rs`), so it must run as the same
owner as the daemon.

## Testing

Besides the in-process ledgering tests in `crates/hephaestus-control/src/server_tests.rs`, `crates/hephaestus-control/tests/control_plane_e2e.rs`'s `mcp_gateway_speaks_json_rpc_framing_over_stdio_against_a_real_daemon` spawns the real `hephaestus-mcp-gateway` binary against a real daemon and drives its stdio JSON-RPC framing end to end.

## Not in this slice

- No `resources` or `prompts` MCP capabilities — only `tools`.
- No tools/list_changed notification (the tool set is fixed per binary
  version).
- Tool schemas are hand-written JSON Schema literals in
  `crates/hephaestus-control/src/bin/hephaestus-mcp-gateway.rs`, not generated from `Command`.
