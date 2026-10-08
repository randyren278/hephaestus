# Status

Hephaestus runs isolated agent comparisons and records evidence you can
replay. It is a working local evaluation system with an offline reference
runtime and opt-in Codex and Claude CLI adapters. Its autonomous improvement
proofs use deterministic fixtures; they do not prove that it improves a
customer’s live model agent.

## What works

- Compile and register JSON, YAML and Markdown agents and Worlds as immutable
  content identities. Authority can only narrow through ancestry.
- Run, submit and compare reference or hosted-provider agents through the
  same supervised process and evidence pipeline. Hosted runs require the
  operator’s own CLI login, explicit authentication setup, network capability
  and World cost budget. See [Runtimes](RUNTIMES.md).
- Compare parent and candidate against visible and sealed tasks using a
  protected evaluator with World-pinned exact, ASCII-trimmed or strict JSON
  scoring, then record seeded selection receipts with
  correctness, reliability, cost and latency gates.
- Verify independent invariants, record Forge assessments, and make
  policy-gated Champion promotions and rollbacks. A selection receipt by
  itself does not authorize promotion.
- Record an operator-authored Codex or Claude prompt revision through
  `genome revise` or the guided terminal Forge flow. The proposal binds its
  hypothesis, source Selection and exact before/after prompts, while preserving
  the model, World, authority and other artifacts. Compare the child separately
  and record its assessment; a revision does not authorize promotion. See
  [hosted prompt revisions](GENOMES.md#hosted-prompt-revisions).
- Evolve within a budget using a versioned catalog of 16 reference operations
  and failure-cluster analysis. Strategies can control mutation priority,
  candidate count and Gene selection. See [Evolution](EVOLUTION.md).
- Drive staged canaries and automatically adapt to recorded drift when a
  World explicitly enables the corresponding Law. Freeze pauses advancement;
  durable state resumes after a restart. See [Canary](CANARY.md).
- Extract and transfer Genes, record contradictions, and measure Evolver
  strategies across held-out lineages. These proofs use reference agents.
- Replay canonical history, refuse tampering, and persist freeze and
  cancellation requests. A job is terminal only after supervised termination.
- Use the Ink terminal console and its six-step tour, or the local read-only
  web console. The browser shows comparison scores, uncertainty, cost,
  latency, independent evidence gates and downloadable summaries. See
  [terminal console](../apps/hephaestus-tui/README.md) and
  [web console](../apps/hephaestus-web/README.md).
- Use the MCP gateway with scoped capabilities and remote workers for direct
  reference runs and reference-role Arena trials. The control plane supports
  the storage trait boundary with SQLite/CAS and a JSONL/in-memory backend.

## Limits that matter before using your own agent

- **Execution needs macOS.** The verified isolation backend is Seatbelt;
  other hosts support registration, inspection and replay but refuse
  candidate execution.
- **Scoring needs a declared answer contract.** The bundled evaluator supports
  exact, ASCII-trimmed and strict JSON output comparison, pinned to the World.
  It does not grade equivalent free-form prose. See [Worlds](WORLDS.md#output-scoring).
  A rebuilt evaluator digest requires a newly registered World.
- **Automatic evolution uses reference operations.** The operation catalog
  covers the seven Gauntlet failure families plus casing. Hosted prose revisions
  are written by the operator; unattended hosted prompt generation and
  autonomous hosted-model improvement are not established. See
  [Evolution](EVOLUTION.md) and [Genomes](GENOMES.md).
- **Real comparisons do not establish improvement or customer value.** Two
  authorized Codex comparisons on the public fictional support-triage pack
  completed 96 calls on 2026-10-08. Both tied at 24/24, and the evidence-bound
  revision's Forge assessment was `metrics_rejected`. Reports, signed receipts
  and restart/replay were checked. Attributable USD cost and independent
  private/customer acceptance remain unavailable. Repository tests use local
  fake provider CLIs and deterministic agents. See the
  [first comparison](evidence/2026-10-08-live-codex/README.md) and
  [Forge follow-up](evidence/2026-10-08-live-forge/README.md).
- **The web console is read-only.** Comparison reports summarize evidence;
  they are not signed receipts and historical transitions do not establish
  the current Champion. Use the operator CLI/TUI for actions and replay for
  canonical verification.
- **Remote Arena execution is sequential.** Reference-role trials are leased
  one at a time; parallel remote evaluation is not an operator feature.

[Product readiness](PRODUCT_READINESS.md) tracks the customer workflow still
needed before recommending this as a polished paid product. [AUDIT.md](../AUDIT.md)
contains the detailed implementation and historical test evidence; historical
results must be rechecked against the current revision.
