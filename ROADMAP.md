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

### 6. Complete observable tracing and experience provenance
**Why**: The Forge needs attributable failures, not opaque transcripts or private chain-of-thought.
**Done when**: Runs record lifecycle, tool calls/results, context metadata, memory retrieval IDs, subagent graph, file/test activity, denials, cost, latency, checkpoints, and completion reason; structured observations, hypotheses, evidence, contradictions, and experience records link back to source events.
**Risk**: Trace collection can leak secrets or become too expensive; redaction and bounded retention are required.
**Hook**: Provenance-aware experience supports useful retrieval without confusing model claims with experimental facts.
**Invariants**: Every trace record has run/Genome/World provenance; secret material is redacted before persistence; raw experience cannot become a Gene without Arena evidence.

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

**Champion slice**: An authenticated operator can seed a World's first Champion, promote an assessed child only when its `metrics_passed` assessment names the current Champion as parent and a verified invariant receipt for the same evaluation satisfies the World contract, and roll back to the previous Champion, which quarantines the replaced one. Each transition is one idempotent, hash-chained ledger event that replay recomputes from the history before it. Automatic failure clustering and automatic regression-triggered rollback remain future work.

### 9. TUI-first operator experience
**Why**: Operators need to understand live experiments, ancestry, authority denials, costs, and evidence before autonomy expands.
**Done when**: A fully interactive terminal application built with Ink shows home status, live Arena progress, runs, lineage DAG, Genome diff, evidence receipt, costs, denials, and freeze/kill/rollback controls; scripted terminal tests and a recorded real demo prove navigation and actions. While a real long-running job runs, the TUI remains responsive and surfaces live progress; a PTY test navigates to status, issues kill, and verifies the run's confirmed termination before its budget expires. Operators can install the supported Hephaestus tool from a clean user environment without a source checkout, launch it with its bundled fixture workspace, and author, validate, register, inspect, and test a Markdown-defined agent (Genome) against registered Worlds and fixture Gauntlets through the real daemon, Arena, and persistent receipt path. The reference runtime must consume and act on the Markdown agent's prompt content: an end-to-end fixture proves that prompt instructions affect task behavior and resulting evidence, rather than only being stored or hashed. TUI text and graphics remain readable and operable at common terminal sizes. All newly created product visuals, including interface graphics and artwork in later surfaces, use a pixel-art visual language aligned with the colosseum hero.
**Risk**: UI projections can become stale or imply false comparability.
**Hook**: A navigable evolutionary tree makes every improvement edge inspectable.
**Invariants**: The daemon remains the source of truth; UI controls receive deterministic acknowledgements; incompatible World scores are visually separated; Markdown agent definitions are an authoring format only and cannot bypass canonical compilation, authority checks, sealed-evaluator isolation, or ledgered registration; install and TUI flows never enable evolution without the operator's explicit unfreeze action.

### 10. Autonomous multi-generation evolution and the Gauntlet
**Why**: The product thesis is not proven until an imperfect G0 improves on unseen tasks without human harness edits.
**Done when**: `hephaestus evolve coding --budget <limit>` completes at least three unattended generations; the Gauntlet includes context loss, premature completion, schema drift, bad routing, duplicate subagents, poisoned memory, and hallucinated verification; a sealed holdout shows statistically supported improvement within enforced budget.
**Risk**: Provider cost or credentials may block hosted-model trials; a deterministic local reference runtime remains mandatory and any billable run requires explicit approval.
**Hook**: The benchmark measures improvement ability under budget, not static agent quality.
**Invariants**: The optimizer cannot alter evaluators or Laws; budgets are runtime-enforced; unseen-task evidence is sealed until experiment completion; no manual edits occur mid-run.

### 11. Population intelligence: Gene Bank, transfer, and speciation
**Why**: Proven adaptations should compound across lineages while preserving evidence about where they fail.
**Done when**: Successful mutations become Genes only after minimum evidence; transfer trials record positive and negative effects across at least three lineages; contradictions are explicit; specialist species are created only from persistent statistically significant domain advantage.
**Risk**: Overgeneralized Genes can spread harmful behavior; decorative species add complexity without value.
**Hook**: Experimental memory becomes reusable intelligence with domain-specific effect sizes.
**Invariants**: Every Gene links to origin evidence; negative transfer is retained; contradictions are never silently overwritten; species creation requires empirical specialization.

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
**Slice done**: `apps/hephaestus-web` is a local, read-only web console. It proxies a fixed allowlist of read-only daemon commands (`status`, `world_list`, `genome_list`, `genome_show`, `genome_prompt`, `champion_show`, `job_status`) over the same owner-only Unix socket protocol the TUI uses, renders Worlds, Genome lineage with Champion/standby/quarantined roles, a Genome prompt diff against its parent, and Champion transition history, and adds a per-launch session token, `Host`/`Origin` checks against DNS rebinding and CSRF, and a strict no-CDN CSP. It never forwards a mutating command. There is no MCP gateway and no authenticated remote worker execution yet; costs, drift, and full evidence/experiment views are also still open. See [apps/hephaestus-web/README.md](apps/hephaestus-web/README.md).

### 15. Public Gauntlet, production hardening, and self-dogfooding
**Why**: A public system needs adversarial proof, reproducible releases, and evidence that Hephaestus can safely work on itself.
**Done when**: Crash, corruption, hash-tamper, sandbox escape, evaluator leakage, budget bypass, outage, timeout, partial-promotion, duplicate-event, rollback, and supply-chain suites pass; releases are signed; benchmark Worlds reproduce publicly; a sandboxed Hephaestus engineering lineage proposes and validates a real improvement without self-merging.
**Risk**: Security testing may reveal architecture-level flaws; self-dogfooding increases blast radius if merge authority leaks.
**Hook**: The project becomes its own evidence-governed managed lineage.
**Invariants**: Protected CI and human-controlled merge remain mandatory; no candidate receives release or promotion credentials; corruption and tampering fail closed; release artifacts are reproducible and signed.

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

For every item: observe the behavior test fail first; implement the smallest vertical slice; add mutations for every new invariant; raise `--assert-min`; add production-critical modules to the 95% coverage gate; update docs in the same commit; push; and wait for hosted CI green before starting the next item.
