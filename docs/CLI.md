# CLI Reference

Everything the `hephaestus` operator binary can do: the manual daily-use
walkthrough, and the full command table. For the one-command scripted
version, see the Quickstart in the [root README](../README.md#quickstart).

## Daily use

For a relocatable, user-local macOS package and its fixture initializer, see
[macOS installation](MACOS_INSTALL.md).

Start the daemon once, from the repository you want candidates to work in:

```bash
cargo build --release --workspace
target/release/hephaestusd --source-repository . \
  --evaluator-executable target/release/hephaestus-reference-evaluator
```

It defaults to `HEPHAESTUS_HOME`, then `~/.hephaestus`. Every `hephaestus`
command below accepts `--data-dir` and `--json`.

**Build a World.** A World is Laws plus the evaluator artifacts that enforce
them. Put the artifacts in the store first, then reference them by address:

```bash
hephaestus arena manifest tasks/visible.json      # canonicalizes and stores → artifact id
hephaestus arena manifest tasks/sealed.json
hephaestus artifact put target/release/hephaestus-reference-evaluator
hephaestus verifier                               # this daemon's Ed25519 producer key
hephaestus world register world.json              # → hephaestus:world:<blake3>
```

The World source names those four addresses under `evaluator_artifacts`
(`arena.visible_manifest`, `arena.sealed_manifest`, `arena.evaluator`,
`arena.runtime_verifier`). A World that anchors somebody else's verifier key is
rejected on the spot. Full schema in [WORLDS.md](WORLDS.md); a working
template in [examples/quickstart/world.template.json](../examples/quickstart/world.template.json).

**Register Genomes.** A Genome is compiled *under* a World, and its parents must
already be registered under that same World:

```bash
hephaestus genome register parent.md --world hephaestus:world:<id>
hephaestus genome register child.md  --world hephaestus:world:<id>   # child.md lists the parent id
hephaestus genome list
```

Registration is idempotent: the same source always yields the same identity and
never a conflict. Schema in [GENOMES.md](GENOMES.md).

**Run and measure.**

```bash
hephaestus unfreeze
hephaestus run hephaestus:genome:<parent>
hephaestus arena evaluate eval-001 hephaestus:genome:<parent> hephaestus:genome:<child>
hephaestus arena select eval-001
hephaestus arena invariants eval-001
hephaestus replay
```

Retrying `arena evaluate` with the same evaluation id reuses the same signed
run events and receipt; a conflicting retry fails closed.

## Command reference

The daemon supports one active direct run or paired Arena evaluation at a time.
Arena trials and protected scoring run under the same supervised async job, so
status, freeze, and cancellation requests remain responsive. Cancellation
records a request; the job becomes terminal only after supervised process
termination. The admitted overall wall budget bounds the complete Arena job.

| Command | What it does |
|---|---|
| `hephaestus status` | Freeze state, active runs, event count, registered Genomes |
| `hephaestus freeze` / `unfreeze` | Halt or resume evolution; ledgered, restart-safe |
| `hephaestus kill --all` | Request cancellation of the active async job |
| `hephaestus init [--fixture <name>] <path>` | Copy a bundled example fixture (default `quickstart`) into a new local directory |
| `hephaestus arena manifest <file>` | Canonicalize a task manifest into the artifact store |
| `hephaestus artifact put <file>` | Store any file by BLAKE3 address |
| `hephaestus verifier` | Publish this daemon's runtime-result public key as an artifact |
| `hephaestus world register <file>` | Compile and register a World (`.json`, `.yaml`, `.yml`) |
| `hephaestus world list` / `show <id>` | Inspect registered Worlds |
| `hephaestus genome register <file> --world <id>` | Compile and register a Genome under a World |
| `hephaestus genome list` / `show <id>` | Inspect registered Genomes |
| `hephaestus genome prompt <id>` | Print the verified reserved prompt body for a Markdown Genome |
| `hephaestus genome propose <id> --selection-event <event> --parent <genome> --hypothesis <text>` | Propose one evidence-bound prompt mutation; never promotes |
| `hephaestus genome assess <id> --proposal <proposal> --selection-event <event>` | Record measured evidence for a proposed child; never promotes |
| `hephaestus run <genome>` | One isolated reference run with signed evidence |
| `hephaestus evaluate <genome> --task-id <id> --input <text>` | Execute one Genome against a single World-bound evaluation task |
| `hephaestus submit <job-id> <genome>` | Submit a bounded async direct reference run |
| `hephaestus job status <job-id>` | Inspect durable state and last recorded trace progress |
| `hephaestus job kill <job-id>` | Request cancellation; confirm termination with `job status` |
| `hephaestus tui` | Open the source-checkout Ink operator console (Node.js 22+ and `npm ci` required) |
| `hephaestus arena evaluate <id> <parent> <child>` | Protected paired evaluation |
| `hephaestus arena select <id>` | Deterministic measured decision from trusted evaluation history |
| `hephaestus arena invariants <id>` | Record aggregate reference-output invariant evidence; never promotes |
| `hephaestus evolve start <id> --world <w> --from <g> --generations <n> --budget <b>` | Unattended, budget-bounded evolution through the ordinary Forge and Champion policy ([EVOLUTION.md](EVOLUTION.md)) |
| `hephaestus evolve status <id>` / `cancel <id>` | Inspect or cooperatively stop an evolution run |
| `hephaestus evolve coding --budget <n>` | Convenience: registers the bundled Gauntlet "coding" World/Genomes and drives a 3-generation evolve run to completion |
| `hephaestus meta strategy register <file>` | Register a versioned, content-addressed Evolver strategy Genome ([META_EVOLUTION.md](META_EVOLUTION.md)) |
| `hephaestus meta strategy show <id>` / `list` | Inspect one or every registered Evolver strategy |
| `hephaestus meta evaluate <id> --strategy-a <g> --strategy-b <g> --lineage-world <w> --lineage-genome <g> ... --lineages <n>` | Run a paired meta-evaluation of two Evolver strategies over held-out base lineages using the existing evolve engine; records a replay-verified receipt with a bootstrap confidence interval |
| `hephaestus meta show <id>` / `list` | Inspect one meta-evaluation receipt, or list recent receipts |
| `hephaestus forge analyze <id> --evaluation <evaluation>` | Record deterministic failure clusters with hypotheses and suggested minimal mutations; never promotes |
| `hephaestus champion seed <id> --world <world> --genome <genome> --reason <text>` | Bootstrap a World's first Champion by operator authority |
| `hephaestus champion promote <id> --assessment <assessment>` | Promote an assessed child whose metrics and invariant evidence pass World policy |
| `hephaestus champion rollback <id> --world <world> --reason <text>` | Restore the previous Champion and quarantine the current one |
| `hephaestus champion show <world>` | Current Champion, standby predecessors, quarantined Genomes, and transition history |
| `hephaestus drift record <id> --world <w> --kind latency\|cost\|correctness\|workload --evidence <evaluation>` | Record verified drift evidence against the current Champion; never replaces it ([CANARY.md](CANARY.md)) |
| `hephaestus drift show <id>` | Inspect one recorded drift observation |
| `hephaestus canary start <id> --world <w> --candidate <genome> --assessment <assessment>` | Start a staged canary bound to a shadow-evaluated candidate; the prior Champion stays Champion |
| `hephaestus canary advance <id> --evidence <evaluation>` | Advance 5% -> 25% -> 50% -> 100% on healthy evidence; automatically aborts on a regression |
| `hephaestus canary live-check <id> --evidence <evaluation>` | Check a completed canary's Champion against the previous one; a regression automatically rolls back through the existing Champion policy |
| `hephaestus canary show <id>` | Inspect one canary's stage and transition history |
| `hephaestus gene extract <id> --promotion <transition>` | Extract a Gene from a promoted, evidence-bound Champion transition ([GENE_BANK.md](GENE_BANK.md)) |
| `hephaestus gene transfer <id> --gene <gene> --to <genome>` | Apply a Gene's mutation to another lineage's Genome through the ordinary compiler |
| `hephaestus gene record <id> --evaluation <evaluation>` | Record a transfer trial's measured effect as positive, neutral, or negative |
| `hephaestus gene show <id>` / `list` | Inspect one Gene's transfer trials, contradictions, and species, or list every Gene |
| `hephaestus gene speciate <id> --gene <gene> --domain <world>` | Create a specialist species from persistent, statistically significant domain advantage |
| `hephaestus replay` | Verify history and compare it with live state |
| `hephaestus runs --limit <n>` | Recent direct runs and jobs, newest first, bounded (default 20, max 200) |
| `hephaestus evaluations --limit <n>` | Recent Arena evaluations with selection, invariant, and Forge evidence references, newest first |
| `hephaestus denials --limit <n>` | Recent refused operator requests and recorded runtime denials, newest first |
| `hephaestus daemon stop` | Audited graceful stop |
| `hephaestus worker credential-mint <worker-id> --ttl-seconds <n>` | Mint a scoped, expiring remote worker credential |
| `hephaestus worker credential-revoke <credential-id>` | Revoke a remote worker credential; fails closed on future use |
| `hephaestus worker submit <job-id> <genome-id>` | Admit one bounded direct reference run for remote-worker execution |
| `hephaestus worker status <job-id>` | Inspect one remote-worker job |

Operator behavior in detail: [CONTROL_PLANE.md](CONTROL_PLANE.md).
Remote workers: [REMOTE_WORKERS.md](REMOTE_WORKERS.md). The MCP
gateway (`hephaestus-mcp-gateway`) is a separate stdio binary, not a CLI
subcommand: [MCP_GATEWAY.md](MCP_GATEWAY.md).
