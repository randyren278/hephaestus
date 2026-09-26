# Status

What is built and what is not, reconciled with [AUDIT.md](../AUDIT.md) (the
detailed, evidence-cited feature audit). The reference runtime used
throughout this system is a deterministic, offline simulation; no run against
a hosted model is claimed as evidence of anything here.

## What it does

<table>
<tr>
<td width="34%" valign="top"><strong>Compiles Genomes and Worlds into immutable identities</strong></td>
<td valign="top">JSON or YAML in, canonical content-addressed objects out. Two equivalent sources get one identity. A child that claims more authority than its parent or its World is refused at compile time, with the reason.</td>
</tr>
<tr>
<td valign="top"><strong>Registers them through one trusted projection</strong></td>
<td valign="top"><code>hephaestus world register</code> and <code>hephaestus genome register</code> append <code>world.registered</code> / <code>genome.registered</code> events. Every startup replays them strictly: a Genome before its World, a parent under another World, a tampered payload, or a non-canonical artifact fails the boot rather than producing a broken state.</td>
</tr>
<tr>
<td valign="top"><strong>Runs a Genome in isolation</strong></td>
<td valign="top"><code>hephaestus run</code> materializes a private Git worktree at a pinned commit, executes the offline reference runtime under a deny-by-default Seatbelt profile with hard wall and output limits, records redacted lifecycle traces, and signs the terminal result.</td>
</tr>
<tr>
<td valign="top"><strong>Measures parent against child in a protected Arena</strong></td>
<td valign="top"><code>hephaestus arena evaluate</code> loads the World's visible and sealed task manifests itself, pins one revision, seed, environment, and budget for both trials, verifies the deployed evaluator's hash against the World before spending any work, and returns only visible aggregates. A crashed trial is recorded as unreliable and incorrect, with its cost and latency, not dropped.</td>
</tr>
<tr>
<td valign="top"><strong>Records deterministic measured selection</strong></td>
<td valign="top"><code>hephaestus arena select &lt;evaluation-id&gt;</code> recomputes a seeded bootstrap and correctness/reliability/cost/latency gates from verified Arena evidence, then stores an event-bound, hash-addressed receipt. Its invariant gate is explicitly unverified, so it never by itself authorizes promotion.</td>
</tr>
<tr>
<td valign="top"><strong>Proves its own state</strong></td>
<td valign="top"><code>hephaestus replay</code> reloads the ledger from disk, verifies its hash chain, registrations, recorded selections, and projected state, then compares the result with the live daemon. Mismatch is an error, not a warning.</td>
</tr>
<tr>
<td valign="top"><strong>Stays under your thumb</strong></td>
<td valign="top"><code>freeze</code>, <code>unfreeze</code>, and cancellation requests survive restarts. A job is reported stopped only after its guardian confirms process-group termination. The socket, token, database, and producer key are owner-only (0600) inside a 0700 directory.</td>
</tr>
</table>

## What is not built yet

- **Promotion and rollback exist, but only as explicit, policy-gated operator
  decisions.** `hephaestus champion seed/promote/rollback` gate a Champion
  transition on a passing Forge assessment plus a verified invariant receipt
  within the World's regression budget; a staged canary
  (`hephaestus canary start/advance/live-check`) automatically aborts on a
  regression mid-rollout or rolls back on a live regression after completion;
  and `hephaestus evolve` drives this same policy automatically across
  generations within one run. What is missing: drift observations do not yet
  auto-start a canary, and there is no mutation-discovery loop richer than the
  single reference-operation flip below.
- **No hosted-model runs.** The Codex and Claude Code invocation adapters are
  real, tested `RuntimeAdapter`s wired into `run`, `submit`, and
  `arena evaluate`, but only inert scripted stand-ins exercise them in CI and
  the test suite; only the deterministic offline reference runtime executes by
  default. A run against a live provider CLI needs an explicit
  `HEPHAESTUS_LIVE_PROVIDER_SMOKE=1` opt-in a user runs themselves (see
  [RUNTIMES.md](RUNTIMES.md)); nothing in CI spends quota or touches the
  network.
- **macOS only for execution.** Isolation is Seatbelt. On other hosts the
  daemon refuses to launch candidate processes rather than running them
  unsandboxed. Registration, replay, and inspection work everywhere.
- **The mutation engine is one flip.** Forge's only supported mutation is
  switching a Genome's reference operation between `identity` and
  `ascii_uppercase`; `evolve` and Gene transfer both inherit that ceiling, so
  neither can discover or apply a richer fix on its own. The Gauntlet's seven
  failure modes are real, deterministic proxies reachable only through direct
  Arena evaluation, not yet through `evolve` itself (see
  [examples/gauntlet/README.md](../examples/gauntlet/README.md)).
- **The Gene Bank tracks transfer, it does not yet feed back.**
  `hephaestus gene extract/transfer/record/show/list/speciate`
  ([GENE_BANK.md](GENE_BANK.md)) records a Gene's transfer trials and
  contradictions and can speciate a domain specialist, but a created species
  does not yet change how the daemon evolves.
- **Meta-evolution can measure two strategies, not act on most of their
  knobs.** `hephaestus meta strategy register/show/list` and
  `hephaestus meta evaluate/show/list` ([META_EVOLUTION.md](META_EVOLUTION.md))
  compare two Evolver strategies' generation and budget allocation with a
  bootstrap confidence interval, but because Forge only proposes the one
  mutation above, a strategy's other declared knobs are recorded but not yet
  actionable.
- **The MCP gateway and remote workers are a first slice.**
  `hephaestus-mcp-gateway` (11 read-only and 4 mutating tools, a per-client
  capability policy, every call ledgered) and `hephaestus-remote-worker`
  (scoped, expiring credentials; one job kind, a bounded direct reference run)
  are real and driven end to end against a live daemon (see
  [MCP_GATEWAY.md](MCP_GATEWAY.md), [REMOTE_WORKERS.md](REMOTE_WORKERS.md)). A
  second storage backend now exists for both storage traits &mdash;
  `FileEventLedger` (an append-only JSONL ledger) and
  `MemoryArtifactBackend` &mdash; proven against the same contract suite as
  the original SQLite/CAS pair (see [LEDGERS.md](LEDGERS.md)), but the
  daemon's own call sites are not yet migrated onto that trait boundary, and
  only one job kind can be leased remotely; leasing an Arena trial remains
  open.
- **The Ink TUI and the read-only web console are partial operator
  surfaces.** Both cover status, freeze/kill, Arena progress, a Lineage &
  Champions screen with operator-confirmed rollback, an Evidence & Costs view,
  a read-only Gene Bank screen, and a read-only Drift, Canary & Meta-eval
  screen; the TUI additionally has a Markdown agent authoring flow. Neither
  exposes an MCP-call or remote-worker-job activity view, because the daemon
  itself has no list command for either (only lookup by known ID). See
  [the TUI setup guide](../apps/hephaestus-tui/README.md) and
  [the web console](../apps/hephaestus-web/README.md).

For the full evidence trail behind every line above &mdash; commands run,
tests exercised, and what remains open in more detail &mdash; see
[AUDIT.md](../AUDIT.md).
