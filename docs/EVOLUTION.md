# Evolution runs

`hephaestus evolve` runs unattended, budget-bounded evolution inside the daemon. It uses only the primitives an operator can already run by hand: paired Arena evaluation, selection, a Forge proposal, Forge assessment, reference-output invariants, and Champion promotion. It adds no new authority.

```sh
hephaestus evolve start <run-id> --world <world-id> --from <genome-id> \
  --generations <n> --budget <paired-evaluations>
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

## What this proves today

With the deterministic reference runtime, a run can complete at least three unattended generations, promoting an improving child and rejecting the rest; in-process tests cover a three-generation run (both a hand-registered World and the bundled `evolve coding` fixture), freeze and resume, cancellation, budget exhaustion, and replay rejection of forged promotions. The only mutation available is the reference-operation flip, so improvement is bounded to that one change. Statistically supported improvement on a sealed holdout needs a richer optimizer. The seven named Gauntlet failure modes are now real, deterministic reference-worker operation pairs, each provably rejecting a "bad" operation and passing its "fix" via direct Arena evaluation — but not yet through `evolve` itself, since Forge's mutation operator only knows the identity/ascii_uppercase flip; see [examples/gauntlet/README.md](../examples/gauntlet/README.md) for exactly what is and is not expressible today.
