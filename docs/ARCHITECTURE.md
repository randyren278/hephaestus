# Architecture

**In short.** Hephaestus is a set of Rust libraries stacked in four layers. At the bottom are the rules and the permanent record. Above them sit the parts that seal agents and exams, run agents in sandboxes, record what happened, and compare agents fairly. Above those sits the daemon, the one program allowed to write the record. At the top are the consoles and command-line programs you use. The Senate debate tool stands on its own.

<p align="center">
  <img src="assets/crate-map.svg" width="100%" alt="How the codebase fits together, in four layers. Front doors: the consoles in apps/ and the programs you run, hephaestus, heph, the MCP gateway, and the remote worker. The engine: hephaestus-control, whose daemon hephaestusd is the only writer to the record. Capabilities: hephaestus-genome seals World and Genome files, hephaestus-runtime runs agents in locked sandboxes, hephaestus-experience records runs with secrets scrubbed, and hephaestus-arena runs fair paired comparisons. Foundation: hephaestus-ledger, the chained record and file store, and hephaestus-core, the Laws and shared vocabulary. Standalone: hephaestus-senate, the debate tool. Code only depends on its own layer or the layers below.">
</p>

The rest of this page is the precise reference, including the full system diagram.

The Rust workspace separates Laws and domain contracts, canonical evidence persistence, immutable Genome/World compilation, the local daemon boundary, capability-scoped runtimes, and the Experience Plane. New planes are added only with executable invariants.

<p align="center">
  <img src="assets/system-map.svg" width="100%" alt="The whole Hephaestus system in five parts. 1, you and the daemon: the operator CLI talks over an owner-only socket that checks your token and schema v1 to hephaestusd, the single writer; a candidate asking to unfreeze is always refused. 2, sealing Worlds and Genomes: source files, stored artifacts, and the vocabulary go through a fail-closed compiler into content-addressed Worlds and Genomes, and registration replay checks acyclic, same-World ancestry; child permissions must be equal or narrower. 3, running an agent: a pinned run spec gets a private worktree in a sandbox, one runner executes it (offline reference, Codex, or Claude), an evidence-required wrapper records redacted traces, and the daemon signs the run result. 4, measuring in the Arena: a paired scheduler runs parent and candidate processes, the Arena checks their signed results against the World's tasks and public key and asks the isolated judge, which returns only totals, to score them; it writes a verified evaluation event that yields a candidate-safe summary and operator-only evidence for deterministic selection. 5, the record: a declared lifecycle produces hash-linked events stored in SQLite and a BLAKE3 file store, and verified replay must match live state or the daemon won't start.">
</p>

## Trust boundary

The authority, domain, compiler, Genome, World, registry, event-store, artifact-store, runtime, isolation, control, Experience, Arena, evaluator-protocol, and isolated-evaluator modules named by `checks/checks.json` are production-critical. The manifest sets a per-module coverage floor (95%) and deliberate source mutations for implemented invariants. Mutation commands and timeouts resolve by longest file-prefix match, while an explicit CLI test command overrides every scoped command. Each suite runs in a fresh process group; timeout or interruption terminates and waits for descendants before byte-exact source restoration. The mutation ratchet may only increase.

## Repository map

- `crates/hephaestus-core/` — shared trust primitives.
- `crates/hephaestus-control/` — daemon, versioned local API, and operator CLI.
- `crates/hephaestus-arena/` — World-bound deterministic paired measurement, strict manifest rehydration, operator-safe task scheduling, authenticated terminal metrics, sealed receipts, and restart-safe event-bound selection evidence.
- `crates/hephaestus-experience/` — redacted traces, runtime evidence integration, and restart-safe trusted Experience rehydration with recursive provenance checks.
- `crates/hephaestus-genome/` — canonical Genome and World compilers, plus the trusted registration registry that replays `world.registered` / `genome.registered` events in ledger order and fails closed on missing Worlds, unregistered or cross-World parents, non-canonical payloads or artifacts, and conflicting metadata.
- `crates/hephaestus-ledger/` — canonical events and artifacts.
- `crates/hephaestus-runtime/` — capability-scoped worktrees and runtime adapters.
- `crates/hephaestus-senate/` — the standalone Senate debate CLI (`senate`); it depends on no other workspace crate. See [SENATE.md](SENATE.md).
- `apps/hephaestus-tui/` and `apps/hephaestus-web/` — the terminal and web operator consoles.
- `examples/quickstart/` — a World template, a parent Genome, a child Genome template, and visible/sealed task manifests.
- `scripts/quickstart.sh` — builds and drives the complete loop against real binaries.
- `checks/` — coverage, mutation, and documentation gates.
- `.github/workflows/ci.yml` — deterministic and adversarial CI jobs.
