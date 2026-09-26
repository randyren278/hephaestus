# Evolution runs

`hephaestus evolve` runs unattended, budget-bounded evolution inside the daemon. It uses only the primitives an operator can already run by hand: paired Arena evaluation, selection, a Forge proposal, Forge assessment, reference-output invariants, and Champion promotion. It adds no new authority.

```sh
hephaestus evolve start <run-id> --world <world-id> --from <genome-id> \
  --generations <n> --budget <paired-evaluations> [--strategy <strategy-id>]
hephaestus evolve status <run-id>
hephaestus evolve cancel <run-id>
hephaestus evolve coding --budget <paired-evaluations>
```

`evolve coding` is a CLI convenience, not a different engine. It registers
the bundled `examples/gauntlet/coding` World and its parent (`identity`) and
candidate (`ascii_uppercase`) reference Genomes against the connected daemon
(idempotently, if already registered — the evaluator binary it publishes is
found next to the `hephaestus` executable, and its runtime verifier key
comes from that daemon's own `verifier` command), then runs `evolve start`
with `--generations 3` and the given `--budget` and polls `evolve status`
until the run finishes. The daemon must already be running and reachable at
`--data-dir`; the command unfreezes it if needed. Every Genome it registers
still uses the plain reference operations, so this proves the unattended
multi-generation *mechanism* against a Gauntlet-flavored World, not that the
optimizer can solve a real coding task — see
[examples/gauntlet/README.md](../examples/gauntlet/README.md).

`--from` is seeded as generation zero's Champion, or kept if it already is one. The World needs a second registered Genome, which the run uses as the fixed baseline for each generation's diagnostic evaluation. `--budget` caps paired Arena evaluations; each generation consumes exactly two.

## One generation

1. Evaluate the current Champion against the fixed baseline and record a selection.
2. Propose one Forge mutation of the Champion with a deterministic generated hypothesis.
3. Evaluate the child against the Champion, select, check reference-output invariants, and assess.
4. Promote the child only if the ordinary Champion policy admits it: a `metrics_passed` assessment whose parent is the current Champion, and an invariant receipt that satisfies the World contract. Otherwise record a rejected generation; the child stays registered and reconstructable.

## Guarantees

- The run is owned by the daemon's reconciliation loop and the single canonical writer. Each step is an idempotent internal call, so a restart mid-generation resumes from durable evidence.
- Freeze halts a run and the run never clears it. `evolve cancel` and `kill` stop it cooperatively.
- The run stops before exceeding its paired-evaluation budget and at its generation ceiling, recording why it finished.
- `evolution.started`, `evolution.generation`, `evolution.cancel_requested`, and `evolution.finished` are canonical ledger events that startup, replay, and projection refresh verify. A generation that claims a promotion must cite the Forge proposal, the assessment, and the matching evidence-bound Champion promotion. Otherwise history fails to verify.
- The optimizer cannot change evaluators, Laws, or the World.

## The mutation catalog and strategy-steered generations

Forge's supported one-step agent.prompt mutations are now a deterministic,
versioned catalog of all 16 reference-runtime operations
(`crates/hephaestus-runtime/src/mutation_catalog.rs`): `identity`/`ascii_uppercase`
plus the seven Gauntlet bad/fix pairs. Every ordered pair of distinct
operations is a representable edge, classified as `Fix` (a Gauntlet family's
bad operation to its own fix), `Regress` (fix back to bad), `Flip` (either
direction of the casing pair), or `CrossFamily` (any other pair). A proposal
targeting agent.prompt requires `MutationTarget::Harness` in the World's
`mutation_scope`, checked once, on the propose path only; replay of an
already-recorded `forge.proposed` event re-checks only that its recorded
`operation_before -> operation_after` edge is a representable catalog edge
(so every previously recordable receipt, the identity/uppercase flip, still
verifies byte-for-byte), plus the new optional `catalog_version` and
`mutation_kind` fields when present.

Failure-cluster analysis (`crates/hephaestus-arena/src/clusters.rs`) gained a
second algorithm, `failure-cluster-v2`: given the analyzed candidate's own
current reference operation, every cluster (any shape or completion
signature) suggests a Gauntlet family's fix when the candidate runs that
family's bad operation, the casing pair behaves exactly like the historical
`failure-cluster-v1` algorithm (`shape_case_mismatch` suggests the flip),
and a Gauntlet fix operation never suggests regressing back to its bad pair.
Replay dispatches on the algorithm already recorded for a given analysis, so
an existing `failure-cluster-v1` receipt still recomputes and verifies
exactly as before; every newly computed analysis is `failure-cluster-v2`.

An `EvolveStart` may now bind a registered Evolver strategy
(`--strategy <strategy-id>`, recorded in `evolution.started` as an optional
`strategy_id`, replay-checked against a registered `meta_strategy.registered`
event). With one bound, each generation runs `forge analyze` on the
diagnostic evaluation (the Champion is the analyzed candidate), orders the
resulting failure clusters by the strategy's `mutation_prioritization`
(`Fifo` keeps their stable signature order; `CostWeighted` sorts by
descending failure count), and, when `gene_selection ==
HighestTransferEffect`, prefers whichever cluster's suggested operation
matches the Gene Bank's highest-mean-positive-effect Gene whose
`operation_before` equals the Champion's operation. The chosen cluster's
suggestion becomes an analysis-bound Forge proposal, exactly like an
operator's `genome propose --analysis --cluster` call. If no cluster
suggests anything, a run without a strategy (or whose Champion runs the
casing pair) falls back to the historical flip; a strategy-bound run whose
Champion runs anything else instead finishes with the new
`EvolutionFinishReason::NoCandidateMutation`. This is what lets `evolve`
*discover* a Gauntlet mode's fix on its own — not just replay an
operator-supplied one — proven by a per-mode in-process test that seeds each
mode's bad Genome as Champion and confirms the strategy-driven run promotes
its paired fix (see [examples/gauntlet/README.md](../examples/gauntlet/README.md)).

`candidate_count` above `1` (proposing and ranking several candidates per
generation) is still not implemented: Forge proposes exactly one candidate
per generation regardless of a strategy's declared `candidate_count`, and
`TRIALS_PER_GENERATION` stays the fixed constant `2` rather than becoming a
per-generation minimum (see `TECH_DEBT.md` TD-17).

## What this proves today

With the deterministic reference runtime, a run can complete at least three unattended generations, promoting an improving child and rejecting the rest; in-process tests cover a three-generation run (both a hand-registered World and the bundled `evolve coding` fixture), freeze and resume, cancellation, budget exhaustion, and replay rejection of forged promotions. Without a bound strategy, the only mutation available is still the reference-operation flip, so improvement is bounded to that one change; with one, Forge can discover any of the 16 catalog operations' fixes when a failure cluster names them (see above). Statistically supported improvement on a sealed holdout needs a richer optimizer than either mode provides. The seven named Gauntlet failure modes are now real, deterministic reference-worker operation pairs, each provably rejecting a "bad" operation and passing its "fix" via direct Arena evaluation, **and** — new in this lane — through a strategy-driven `evolve` run discovering the fix on its own; see [examples/gauntlet/README.md](../examples/gauntlet/README.md) for exactly what is and is not expressible today.
