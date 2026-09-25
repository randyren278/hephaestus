# Hephaestus Full-Reign Roadmap

This roadmap implements the complete master-plan sequence without presenting later-stage features as current behavior. Each item ends with real executable evidence, documentation, a raised mutation ratchet, a pushed commit, and a green hosted CI run.

## Build order

### 1. Constitution and executable domain contracts
**Why**: Every later ledger, API, and UI needs one stable vocabulary and explicit non-evolvable Laws.
**Done when**: The six L0 constitution documents exist; Rust types encode Agent, Genome, Mutation, Lineage, Gene, World, Law, Champion, Arena, Drift, and lifecycle states; schema fixtures validate; conflicting or unknown versions fail tests.
**Risk**: Freezing the wrong abstraction creates migration pressure; changes remain reversible until persisted schema v1 is released.
**Hook**: A typed constitution makes product language executable instead of aspirational.
**Invariants**: Unknown schema versions fail closed; released identifiers are immutable; a candidate cannot alter a Law.

### 2. Hash-linked event spine, SQLite ledger, and artifact CAS
**Why**: Replayable evidence and crash recovery are prerequisites for every trustworthy autonomous action.
**Done when**: SQLite/WAL appends ordered hash-linked events transactionally; BLAKE3-addressed artifacts verify on read; deleting all projections and replaying produces byte-identical state; kill/restart tests lose no accepted event and create no duplicate transition.
**Risk**: Transaction and filesystem ordering can corrupt the evidence chain; migrations must preserve all prior events.
**Hook**: Event sourcing plus a content-addressed store yields independently verifiable evolutionary history.
**Invariants**: Events are append-only and sequence-monotonic; hash-chain tampering is detected; artifact content must match its address; replay is deterministic.

### 3. Genome, World, and authority compilers
**Why**: Hephaestus cannot evaluate evolution until candidates and their immutable physics have reproducible identities.
**Done when**: Canonical YAML/JSON compiles into normalized content-addressed Genomes and versioned Worlds; parent ancestry and referenced artifacts resolve; authority ceilings and mutation scopes validate; incompatible Worlds cannot be compared. A documented Markdown authoring format for agent/Genome definitions compiles through the same validator into the same canonical Genome objects; malformed or ambiguous Markdown fails closed, and fixtures prove equivalent definitions produce identical identities.
**Risk**: Canonicalization bugs can assign two identities to equivalent content or reuse one identity for different content.
**Hook**: Deterministic compilation turns agent harnesses into Git-like immutable objects.
**Invariants**: Released Genome content never changes; children have equal or narrower authority; malformed references fail closed; changing Laws creates a new World.

### 4. Daemon, operator API, and deterministic CLI
**Why**: One local process must own canonical truth before runtimes or user interfaces can act on it.
**Done when**: `hephaestusd` owns storage and exposes a versioned local API; `hephaestus status`, `freeze`, `unfreeze`, `kill --all`, Genome inspection, and replay work end to end; process restart preserves state and freeze status. Long-running work is submitted as bounded jobs while a single canonical writer and reconciliation loop remain authoritative, so status and operator-control requests stay responsive during execution.
**Risk**: Split-brain daemon instances or unauthenticated local clients could bypass policy.
**Hook**: A reconciliation loop separates desired state from agent suggestions.
**Invariants**: Only one writer owns canonical state; evolution cannot clear freeze; kill terminates active work; API requests are authenticated and ledgered; asynchronous job execution cannot bypass reconciliation or create competing writers.

### 5. Sandboxed runtime adapter contract with Codex and Claude
**Why**: Provider-neutral, capability-scoped execution is the bridge from immutable specifications to observable agent runs.
**Done when**: The runtime trait starts, resumes, interrupts, snapshots, and reports capabilities; Codex and Claude adapters each complete a real local task; every run receives an isolated worktree, environment, capability token, execution directory, and enforced time/resource budget.
**Risk**: Real adapters may require installed CLIs or user credentials; those integrations pause rather than accepting secrets or paid usage without authorization.
**Hook**: The same RunSpec can drive heterogeneous coding agents without leaking provider details into Genome semantics.
**Invariants**: Siblings cannot inspect one another; candidates cannot access canonical storage or hidden evaluators; expired capabilities fail; hard limits terminate overruns.

**Slice done**: `SupervisedRuntime::provider`/`provider_guarded` (`crates/hephaestus-runtime`) are real `RuntimeAdapter`s for Codex and Claude Code, with invocation flags pinned to the current documented non-interactive CLI contracts (docs/RUNTIMES.md cites sources), running under the same isolated worktree/environment/capability-token/budget machinery as the reference worker. A Genome's `model.provider` selects the adapter for the daemon's `run` and synchronous `RunEvaluation` paths end to end, proven by a daemon-level test against fake `codex`/`claude` binaries. Adapter-specific `resume` is not implemented (fails closed, `RuntimeError::Unsupported`, matching the reference worker's own precedent); `submit` and `arena evaluate` still only accept the reference worker because their canonical job/trial projections hard-code its digest-pinned contract — see docs/RUNTIMES.md "Hosted drivers" for exactly what remains. No real provider CLI or credential has been invoked; docs/RUNTIMES.md documents the opt-in, explicitly-marked live smoke test a human runs separately.

### 6. Complete observable tracing and experience provenance
**Why**: The Forge needs attributable failures, not opaque transcripts or private chain-of-thought.
**Done when**: Runs record lifecycle, tool calls/results, context metadata, memory retrieval IDs, subagent graph, file/test activity, denials, cost, latency, checkpoints, and completion reason; structured observations, hypotheses, evidence, contradictions, and experience records link back to source events.
**Risk**: Trace collection can leak secrets or become too expensive; redaction and bounded retention are required.
**Hook**: Provenance-aware experience supports useful retrieval without confusing model claims with experimental facts.
**Invariants**: Every trace record has run/Genome/World provenance; secret material is redacted before persistence; raw experience cannot become a Gene without Arena evidence.

**Slice done (provider portion)**: Codex/Claude adapters map their NDJSON event stream onto the existing `RuntimeObservation`/`TraceKind` vocabulary (tool calls/results, subagent linkage, cost, denials) through `ProviderEventCursor` (`crates/hephaestus-runtime/src/provider_events.rs`) and the existing `RecordedRuntime`/`EvidenceRecorder` pipeline, unchanged from the reference worker's. A provider run's extracted final-answer stdout and raw stderr are redacted with the same `RedactionPolicy` used for structured trace fields (`RedactionPolicy::redact_text`) before either reaches the content-addressed artifact store; a Codex/Claude-specific `RunResultReceipt.actual_cost_microusd` is now bounded by the receipt's own approved budget rather than hard-coded to zero. Full subagent transcript reconstruction is not implemented (only linkage via `parent_tool_use_id`); Codex reports no per-run USD cost, so its `actual_cost_microusd` is always `0` with token counts recorded separately.

### 7. Scientific Arena, sealed evaluators, statistics, and evidence receipts
**Why**: Trustworthy measurement must exist before automated mutation.
**Done when**: Visible and sealed tasks run in separated evaluator contexts; parent/candidate paired trials reproduce by World, seed, environment, and budget; the Python lab computes bootstrap intervals and Pareto comparisons; every verdict emits a hash-addressed receipt with correctness, reliability, cost, latency, regressions, and confidence.
**Risk**: Evaluator leakage or invalid statistical assumptions can manufacture progress.
**Hook**: Protected paired evaluation makes improvement falsifiable and cost-aware.
**Invariants**: Candidates cannot inspect evaluator source, expected output, or sealed scores; incompatible Worlds never share a progress line; claims without receipts cannot support promotion.

**Latency-tolerance note**: The Pareto comparison's latency dimension originally required the candidate's raw measured wall-clock latency to be no worse than the parent's, which made selection noise-driven on trivial reference tasks where latency is a few milliseconds of scheduling jitter. It now tolerates latency within `max(10% of parent latency, 50ms * paired task count)` before counting it as a regression (algorithm identity `histogram-bootstrap-pareto-tolerant-v2`), while a receipt recorded under the retired strict comparison (`histogram-bootstrap-v1`) still verifies under its own rule on replay.

### 8. Forge, selection, lineage, promotion, and rollback
**Why**: This is the minimum complete evolutionary loop: observable failure becomes one causal mutation and an evidence-backed descendant.
**Done when**: Failure clusters produce explicit hypotheses and minimal mutations; descendants form an ancestry DAG; selection uses paired evidence and regression floors; deterministic policy promotes a winner or rejects all; rollback reconstructs the previous Champion after injected live regression.
**Risk**: Multi-change mutations weaken causal attribution; race conditions could double-promote or lose the last known-good Champion.
**Hook**: Git-like ancestry and Kubernetes-like reconciliation applied to agent intelligence.
**Invariants**: Models may recommend but never execute promotion; one transition has one idempotency key; losers are archived, not deleted; rollback always retains a reconstructable Champion.

**Implemented slice**: An authenticated operator can propose one supported reference-prompt operation flip from a verified selection event. The child is compiled and registered with explicit hypothesis and durable idempotent lineage evidence. A second child/parent Arena evaluation can produce an evidence-only assessment; automatic failure clustering, trusted invariant authorization, promotion, and rollback remain future work.

**Assessment slice**: A proposal can now be assessed only after a new paired Arena evaluation of its exact parent and child has a verified selection receipt. The idempotent assessment records whether the measured gates passed; it leaves invariant verification and promotion eligibility false and does not declare a winner or alter lineage.

**Invariant evidence slice**: An authenticated operator can record replay-verified aggregate reference-output invariant evidence for an exact paired evaluation. The response excludes task identities and raw outputs. This evidence is separate from selection and Forge assessment, leaves their flags unchanged, and does not authorize promotion.

**Failure-cluster slice**: `forge analyze` records a deterministic, replay-verified `forge.clustered` receipt that groups a candidate's failed trials by observable signature, never reading sealed task content, and each cluster names an explicit hypothesis plus its single supported minimal mutation when one exists. `genome propose --analysis --cluster` turns a cluster into a bound Forge proposal. Only the reference-operation flip is a supported mutation today.

**Champion slice**: An authenticated operator can seed a World's first Champion, promote an assessed child only when its `metrics_passed` assessment names the current Champion as parent and a verified invariant receipt for the same evaluation satisfies the World contract, and roll back to the previous Champion, which quarantines the replaced one. Each transition is one idempotent, hash-chained ledger event that replay recomputes from the history before it. Automatic failure clustering and automatic regression-triggered rollback remain future work.

### 9. TUI-first operator experience
**Why**: Operators need to understand live experiments, ancestry, authority denials, costs, and evidence before autonomy expands.
**Done when**: A fully interactive terminal application built with Ink shows home status, live Arena progress, runs, lineage DAG, Genome diff, evidence receipt, costs, denials, and freeze/kill/rollback controls; scripted terminal tests and a recorded real demo prove navigation and actions. While a real long-running job runs, the TUI remains responsive and surfaces live progress; a PTY test navigates to status, issues kill, and verifies the run's confirmed termination before its budget expires. Operators can install the supported Hephaestus tool from a clean user environment without a source checkout, launch it with its bundled fixture workspace, and author, validate, register, inspect, and test a Markdown-defined agent (Genome) against registered Worlds and fixture Gauntlets through the real daemon, Arena, and persistent receipt path. The reference runtime must consume and act on the Markdown agent's prompt content: an end-to-end fixture proves that prompt instructions affect task behavior and resulting evidence, rather than only being stored or hashed. TUI text and graphics remain readable and operable at common terminal sizes. All newly created product visuals, including interface graphics and artwork in later surfaces, use a pixel-art visual language aligned with the colosseum hero.
**Risk**: UI projections can become stale or imply false comparability.
**Hook**: A navigable evolutionary tree makes every improvement edge inspectable.
**Invariants**: The daemon remains the source of truth; UI controls receive deterministic acknowledgements; incompatible World scores are visually separated; Markdown agent definitions are an authoring format only and cannot bypass canonical compilation, authority checks, sealed-evaluator isolation, or ledgered registration; install and TUI flows never enable evolution without the operator's explicit unfreeze action.

**Screens and authoring slice**: The TUI now has, alongside home status/freeze/kill and Arena progress by known evaluation ID, an Evidence & Costs submenu (Runs, Evidence receipts, Costs, Denials, all World-grouped and read-only from the existing `run_list`/`evaluation_list`/`denial_list` projections) and a Markdown agent authoring flow (World pick, a default workspace path under `~/.hephaestus/agents`, a `$EDITOR`/`$VISUAL` hand-off with verified raw-mode terminal restoration, registration through the real `genome_register` compiler with sanitized rejection reasons, and a paired Test via `evaluate_pair` reusing `ArenaProgressPanel` for live progress and the visible score). An opt-in PTY test (`tui_evidence_screens_and_markdown_authoring_flow_through_a_pty`) builds real World/Genome/run/evaluation/denial state through the CLI, then drives both flows end to end against a real daemon in an 80×24 pseudo-terminal, run 3/3 locally. Live-job responsiveness (navigate to status, kill, confirmed termination before budget) is proven by the pre-existing kill-flow PTY test.

**Packaging, Gene Bank screen, and demo slice**: `scripts/package_macos_acceptance.sh` was re-run in this lane end to end against a freshly built package archive in a temporary, isolated `$HOME` with no host Node/npm on `PATH`: it installs the relocatable package, initializes the bundled quickstart fixture (a World template, Markdown parent/candidate Genomes, and visible/sealed task manifests — the fixture Gauntlet this milestone ships), registers the World and Genomes with the installed CLI, runs the offline reference runtime, measures the expected Arena improvement, computes the fail-closed selection receipt, replays the ledger, and opens the packaged, bundled-Node-runtime TUI, all against the real daemon; it passed. The TUI gained a read-only **Gene Bank** screen (`gene_list`/`gene_show`): a list of extracted Genes with their lineage/transfer tally and contradiction flag, and a detail view with origin, evidence trial count, every transfer trial's outcome and effect estimate, any contradiction, and speciation. There are intentionally no drift, canary, MCP gateway, or remote-worker screens: the control protocol (`crates/hephaestus-control/src/server.rs`) and `AUDIT.md` confirm none of those exist yet in the daemon, so a TUI projection of them would be fictional; those screens land with roadmap items 12 and 14. New pixel-art SVG assets (`scripts/pixel_art/generate.py`, checked in under `docs/assets/pixel/`) extend the colosseum hero's palette and block-glyph style into a TUI banner, Champion/Gene/canary icons, and the web console header. A deterministic recorded demo (`scripts/record_demo.py`) drives the real daemon, CLI, and TUI end to end — daemon start, World/Genome registration and Champion seeding, TUI Lineage/Champion view, and TUI-driven authoring/registration/paired-Test of a candidate Genome — into an asciicast v2 file under `docs/demo/`. Still open: richer runtimes that read the Markdown prompt as free-form instructions rather than only the deterministic reference operation.

### 10. Autonomous multi-generation evolution and the Gauntlet
**Why**: The product thesis is not proven until an imperfect G0 improves on unseen tasks without human harness edits.
**Done when**: `hephaestus evolve coding --budget <limit>` completes at least three unattended generations; the Gauntlet includes context loss, premature completion, schema drift, bad routing, duplicate subagents, poisoned memory, and hallucinated verification; a sealed holdout shows statistically supported improvement within enforced budget.
**Risk**: Provider cost or credentials may block hosted-model trials; a deterministic local reference runtime remains mandatory and any billable run requires explicit approval.
**Hook**: The benchmark measures improvement ability under budget, not static agent quality.
**Invariants**: The optimizer cannot alter evaluators or Laws; budgets are runtime-enforced; unseen-task evidence is sealed until experiment completion; no manual edits occur mid-run.

**Evolve slice**: `hephaestus evolve start|status|cancel` runs durable, replay-verified, budget-bounded evolution in the daemon using only existing Arena, Forge, invariant, and Champion primitives; it respects freeze, never alters evaluators or Laws, and a generation's promotion claim must cite matching evidence. In-process tests run three unattended generations with the deterministic reference runtime. A sealed-holdout fixture World (examples/gauntlet) shows the zero-regression policy refusing a candidate that improves only on visible tasks. The seven named Gauntlet failure modes need richer runtimes and are not yet expressible, and statistically supported improvement on a sealed holdout is not yet demonstrated.

### 11. Population intelligence: Gene Bank, transfer, and speciation
**Why**: Proven adaptations should compound across lineages while preserving evidence about where they fail.
**Done when**: Successful mutations become Genes only after minimum evidence; transfer trials record positive and negative effects across at least three lineages; contradictions are explicit; specialist species are created only from persistent statistically significant domain advantage.
**Risk**: Overgeneralized Genes can spread harmful behavior; decorative species add complexity without value.
**Hook**: Experimental memory becomes reusable intelligence with domain-specific effect sizes.
**Invariants**: Every Gene links to origin evidence; negative transfer is retained; contradictions are never silently overwritten; species creation requires empirical specialization.
**Slice done**: `hephaestus gene extract|transfer|record|show|list|speciate` runs canonical, idempotent, replay-verified `gene.extracted` / `gene.transfer_applied` / `gene.transfer_recorded` / `gene.contradiction` / `gene.species_created` events through the real daemon and CLI. Extraction refuses a promotion below a deterministic minimum measured-trial threshold or one that isn't a Champion `Promoted` transition; transfer applies the Gene's exact mutation to another lineage's Genome through the ordinary compiler and refuses a recipient that doesn't currently carry the Gene's origin operation; recording classifies a verified paired evaluation's selection receipt as positive, neutral, or negative from its confidence bounds, and negative transfer is never dropped. A contradiction (positive in one lineage, negative in another) is recorded automatically, once, and never overwritten. Speciation is refused unless a domain has zero recorded negatives and at least three distinct positive lineages with a mean effect above a documented threshold. See [docs/GENE_BANK.md](docs/GENE_BANK.md). The only supported mutation is still the reference-operation flip; domain is exactly "the recipient's registered World," with no richer taxonomy; and a created species does not yet feed back into Forge proposals or Arena scoring.

### 12. Drift detection, self-healing branches, and canary control
**Why**: A living agent system must distinguish poor agents from changed environments and adapt without risking the current Champion.
**Done when**: Injected tool-schema, latency, cost, and workload shifts create drift records; adaptation branches evaluate in shadow; staged 5/25/50/100% canaries promote on healthy evidence and automatically roll back on injected regression while the prior Champion remains active.
**Risk**: Noisy signals can cause adaptation storms or unsafe promotions.
**Hook**: Evolution becomes a controlled self-healing mechanism rather than blind retry.
**Invariants**: Drift never directly replaces a Champion; canary thresholds are deterministic; any material regression rolls back; adaptation respects freeze and budget ceilings.

### 13. Recursive evolution of the Evolver
**Why**: Hephaestus should eventually improve how efficiently it discovers improvements, while leaving the Laws immutable.
**Done when**: Evolver strategies are versioned Genomes evaluated in a meta-World; a descendant reaches equal or better Champions with statistically lower experiment cost or fewer trials; the base evaluator and Laws remain outside its mutation scope.
**Risk**: Meta-optimization can overfit the benchmark or obscure causal responsibility.
**Hook**: Evidence-backed improvement of the improvement process itself.
**Invariants**: Evolvers cannot alter meta-evaluators, Laws, receipts, or budgets; comparisons use held-out lineages; efficiency claims include uncertainty.

### 14. Web console, MCP gateway, and remote workers
**Why**: Once the local TUI proves the projections, operators and agents need mediated remote access and scalable execution.
**Done when**: The web console renders lineage, evidence, experiments, Genes, drift, costs, Worlds, and authority history from daemon APIs; an MCP gateway enforces tool capabilities and ledgers calls; authenticated remote workers execute isolated jobs against swappable storage backends without changing domain semantics.
**Risk**: Network exposure, multi-writer state, and distributed failure substantially expand the threat model.
**Hook**: Local-first domain boundaries scale to distributed execution without replacing the trustworthy core.
**Invariants**: Browsers and agents never access canonical storage directly; remote identities are scoped and expiring; duplicate delivery is idempotent; tool schemas and calls are versioned.
**Slice done**: `apps/hephaestus-web` is a local, read-only web console. It proxies a fixed allowlist of read-only daemon commands (`status`, `world_list`, `genome_list`, `genome_show`, `genome_prompt`, `champion_show`, `job_status`, `gene_list`, `gene_show`, `run_list`, `evaluation_list`, `denial_list`) over the same owner-only Unix socket protocol the TUI uses, renders Worlds, Genome lineage with Champion/standby/quarantined roles, a Genome prompt diff against its parent, Champion transition history, extracted Genes, and run/evaluation costs and authority/denial history, and adds a per-launch session token, `Host`/`Origin` checks against DNS rebinding and CSRF, and a strict no-CDN CSP. It never forwards a mutating command. See [apps/hephaestus-web/README.md](apps/hephaestus-web/README.md).
**MCP gateway**: `hephaestus-mcp-gateway` (`crates/hephaestus-control/src/bin/hephaestus-mcp-gateway.rs`) implements the MCP JSON-RPC 2.0 stdio transport (`initialize`, tools/list, tools/call) against the specification's 2025-06-18 revision, exposing 11 read-only tools and 4 mutating tools (`arena_evaluate`, `arena_select`, `genome_propose`, `genome_assess`) at schema version 1. Every call is routed through the daemon's ordinary authenticated operator API as a new `Command::McpCall`, per-client capability policy (`allow` plus `grants` for mutating tools) is enforced by the gateway before it ever contacts the daemon, and every call — allowed or denied — is ledgered as an mcp.call event, replay-verified like every other canonical event; denials also appear in `denials`. See [docs/MCP_GATEWAY.md](docs/MCP_GATEWAY.md).
**Remote workers**: `hephaestus worker credential-mint`/`credential-revoke` mint and revoke scoped, expiring credentials (ledgered, never storing the raw secret); `hephaestus worker submit`/`status` admit and inspect one bounded direct reference run for remote pickup. `hephaestus-remote-worker` authenticates over a dedicated owner-only worker.sock, leases a job, executes it with the exact same isolated sandboxed transform the local reference worker uses (`execute_reference_worker_request`), and returns raw output for the daemon to sign with its own Ed25519 key and record as an ordinary `run.result_recorded` event — indistinguishable from a local run. Duplicate delivery is idempotent (a job with an existing signed result is acknowledged without reprocessing); an expired or revoked credential fails closed before any lease or result is processed. `hephaestus-ledger::{EventLedger, ArtifactBackend}` introduce the storage-backend trait boundary; the existing SQLite/CAS backend is proven against a backend-agnostic contract test suite. See [docs/REMOTE_WORKERS.md](docs/REMOTE_WORKERS.md).
**Not in this slice**: leasing one Arena trial (only a direct reference run); a second storage backend implementation; migrating `ControlPlane`'s internal call sites onto the new storage traits; drift/canary state in the web console (roadmap item 12 is not part of this lane's base); full evidence/experiment views; daemon-level end-to-end tests and a full mutation-guard run for the new modules (see AUDIT.md).

### 15. Public Gauntlet, production hardening, and self-dogfooding
**Why**: A public system needs adversarial proof, reproducible releases, and evidence that Hephaestus can safely work on itself.
**Done when**: Crash, corruption, hash-tamper, sandbox escape, evaluator leakage, budget bypass, outage, timeout, partial-promotion, duplicate-event, rollback, and supply-chain suites pass; releases are signed; benchmark Worlds reproduce publicly; a sandboxed Hephaestus engineering lineage proposes and validates a real improvement without self-merging.
**Risk**: Security testing may reveal architecture-level flaws; self-dogfooding increases blast radius if merge authority leaks.
**Hook**: The project becomes its own evidence-governed managed lineage.
**Invariants**: Protected CI and human-controlled merge remain mandatory; no candidate receives release or promotion credentials; corruption and tampering fail closed; release artifacts are reproducible and signed.
**Slice done**: `deny.toml` plus a CI `supply-chain` job run `cargo deny check` (license allowlist, RustSec advisories, crates.io-only sources, duplicate versions as warnings) on every push; `npm audit --audit-level=high` runs for both Node apps; every third-party GitHub Action is pinned to a commit SHA; workflow permissions are least-privilege by default. `.github/workflows/release.yml` builds signed macOS `arm64`/`x86_64` archives on `v*` tags or manual dispatch: SHA-256 checksums, a CycloneDX SBOM per binary, Sigstore keyless signatures plus GitHub build-provenance attestations, and a same-machine reproducibility job that builds the archive twice and fails if the bytes differ (`docs/RELEASES.md` states plainly what that does and does not prove — it is not an independent third-party rebuild). `docs/ADVERSARIAL.md` maps crash, corruption, hash-tamper, sandbox-escape, evaluator-leakage, budget-bypass, timeout, partial-promotion, and duplicate-event coverage to existing tests, and adds three new ones (`crates/hephaestus-runtime/tests/adversarial.rs`) for gaps: writing outside a worker's sandbox root, opening a real network connection from a worker, and — the self-dogfooding credential-containment proof — that no merge/release credential (`GIT_*`, `GH_TOKEN`, `SSH_AUTH_SOCK`, package-registry and signing tokens) is reachable from a candidate sandbox. `scripts/self_dogfood.sh` and `examples/self_dogfood/` are a documented, runnable procedure where a sandboxed lineage proposes and Arena-validates a change to a fixture "Hephaestus source file," producing a local branch a human must review and merge — never a push or an automatic merge (`docs/SELF_DOGFOODING.md`). Not yet done: an independent (differently provisioned) reproducibility rebuild, a macOS CI runner for the sandbox-escape test set (they currently only run on a developer's Mac), and self-dogfooding against this repository's own tracked files rather than a disposable fixture copy.

### 16. V1 continuous-operation proof
**Why**: The master plan defines operational credibility as sustained behavior, not a demo assembled for one run.
**Done when**: A monitored 30-day run produces multiple independently verified lineage improvements, survives deliberate drift and process crashes, preserves an unbroken evidence chain, and reproduces every Champion from its Genome and receipts; the final report includes raw machine-readable evidence.
**Risk**: This requires elapsed time, operating budget, and possibly provider credentials outside ordinary implementation authority.
**Hook**: A durability trial turns the north-star story into auditable operational history.
**Invariants**: No missing ledger intervals; every Champion is reproducible; all spend is attributable and capped; operator freeze/kill remains effective throughout.

## Master Build Ledger coverage

| Master-plan ledger | Roadmap item |
|---|---:|
| L0 | 1 |
| L1 | Completed in baseline `a48c4a5` |
| L2 | 2 |
| L3, L7 | 3 |
| L4, L5 | 5 |
| L6 | 6 |
| L8, L9, L11, L14 | 7 |
| L10, L12 | 8 |
| L13 | 9 |
| L15, L24 | 10 |
| L16–L18 | 11 |
| L19–L21 | 12 |
| L22 | 13 |
| L23 and V0.3 distributed surfaces | 14 |
| L25, L26 | 15 |
| V1.0 operational definition | 16 |

## Build discipline

For every item: observe the behavior test fail first; implement the smallest vertical slice; add mutations for every new invariant; raise `--assert-min`; add production-critical modules to the coverage gate (95% target; temporarily 80%, see TECH_DEBT.md); update docs in the same commit; push; and wait for hosted CI green before starting the next item.
