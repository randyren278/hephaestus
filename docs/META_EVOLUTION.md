# Recursive evolution of the Evolver

`hephaestus meta` compares two Evolver *strategies* — versioned,
content-addressed configurations of the knobs that steer `hephaestus evolve`'s
own admission policy — by running the existing, unmodified evolve engine once
per strategy over each of a set of held-out base lineages, then recording a
replay-verified receipt with a bootstrap confidence interval over the
per-lineage Champion-quality and experiment-cost deltas.

```sh
hephaestus meta strategy register <strategy.json>
hephaestus meta strategy show <strategy-id>
hephaestus meta strategy list

hephaestus meta evaluate <meta-run-id> \
  --strategy-a <strategy-id> --strategy-b <strategy-id> \
  --lineage-world <world-id> --lineage-genome <genome-id> \
  --lineage-world <world-id> --lineage-genome <genome-id> \
  --lineages <n> [--confidence-bps 9500] [--seed 0]
hephaestus meta show <meta-run-id>
hephaestus meta list
```

## Evolver strategy Genomes

A strategy is a small JSON document:

```json
{
  "schema_version": 1,
  "name": "fifo-baseline",
  "mutation_prioritization": "fifo",
  "generation_count": 3,
  "experiment_allocation": 6,
  "candidate_count": 1,
  "gene_selection": "none",
  "parent_strategy_id": "hephaestus:meta-strategy:<hash-of-fifo-baseline>"
}
```

Its identity is the BLAKE3 hash of its canonical JSON, exactly like a
compiled Genome; registering it twice with identical content is idempotent
and appends no second event. `generation_count` and `experiment_allocation`
are passed straight through as `evolve start`'s `--generations` and
`--budget` for every lineage the strategy evaluates. `mutation_prioritization`,
`candidate_count`, and `gene_selection` are recorded for a richer Forge to
read later; see "What this proves today" below for what they do now.

`parent_strategy_id` is optional and, when present, declares this strategy a
descendant of an already-registered strategy Genome. It is part of the
strategy's own content identity (two strategies with identical knobs but a
different declared parent, or none, register as distinct strategies), and
registration rejects a `parent_strategy_id` that does not resolve to an
already-registered strategy or that names the strategy itself. Every replay
re-checks that lineage edge against the same history that preceded it,
exactly like the rest of a strategy's fields.

**Enforced by construction, not by a runtime check:** `EvolverStrategyConfig`
has no field that names or touches a meta-evaluator, a Law, a receipt, or a
World's budget ceiling. A strategy cannot grant itself authority beyond the
generation ceiling and paired-trial budget any operator could already pass to
`evolve start` by hand. Declaring a parent grants no additional authority
either; it is bookkeeping consumed only by the receipt's descendant verdict
below.

## One meta-evaluation

For each held-out lineage (an already-registered World plus the Genome
installed as its generation-zero Champion — exactly what `evolve start
--from` takes):

1. Drive strategy A's evolve run to completion via the ordinary evolve engine
   (`EvolveStart` plus the same reconciliation step the daemon's `serve` loop
   calls every tick), using the strategy's `generation_count` and
   `experiment_allocation` as `--generations`/`--budget`.
2. Cooperatively roll the lineage's World Champion back to its starting
   Genome (one promotion at a time, exactly like `champion rollback`), so
   strategy B starts from the identical condition strategy A did.
3. Drive strategy B's evolve run to completion the same way, then roll the
   Champion back once more.

Each lineage records both runs' final Champion, promoted-generation count
(the quality proxy), and paired-trials consumed (the cost). Across every
lineage, a deterministic histogram bootstrap (`SplitMix64`-seeded, 10,000
resamples, independently implemented in the same style as the Arena
selection receipt's bootstrap) produces a confidence interval over the
paired quality delta and the paired cost delta (strategy B minus strategy A).
The whole computation is recorded as one `meta_evolution.evaluated` event; a
retry with the same `meta-run-id` is idempotent and resumes from whichever
per-lineage evolve runs already finished.

## Guarantees

- Held-out comparison: a meta-evaluation always uses lineages the caller
  names explicitly, never a lineage a strategy or the meta-evaluation itself
  selects; at least two are required so the bootstrap has something to
  resample over.
- No lasting side effect: every lineage's World Champion is restored to its
  starting Genome before the receipt is recorded, whether or not either
  strategy promoted something.
- Replay-verified: `meta_strategy.registered` and `meta_evolution.evaluated`
  events are recomputed and cross-referenced from the history that preceded
  them on every replay, exactly like Champion transitions and evolution
  events. A recorded receipt's bootstrap interval is recomputed from its own
  referenced evolve runs and must match exactly, or replay fails closed.
- No new authority: a meta-evaluation only ever calls `EvolveStart` and
  Champion rollback, both of which an operator could already call directly;
  it adds no new evaluator, no new Law, and no new budget primitive.
- Uncertainty is always reported: both the quality delta and the cost delta
  are bootstrap confidence intervals, never a bare point estimate.

## Descendant verdict

When a meta-evaluation compares two strategies where one declares the other
as its `parent_strategy_id`, the receipt records one more field:
`descendant_cheaper_at_equal_quality`. It reorients `quality_delta` and
`cost_delta` (which are always strategy-B-minus-A) to
descendant-minus-ancestor and is:

- `Some(true)` when the descendant's bootstrapped quality delta has a lower
  bound `>= 0` (equal-or-better Champion quality) **and** its bootstrapped
  cost delta has an upper bound `< 0` (statistically lower experiment cost);
- `Some(false)` when a lineage relationship is declared but that bar is not
  met;
- `None` when neither strategy is the other's declared parent, so no lineage
  claim applies.

Replay recomputes this verdict from the recorded strategy configs and the
receipt's own `quality_delta`/`cost_delta`, exactly like every other derived
field on the receipt.

## What this proves today

The only mutation the Forge can propose is the deterministic
reference-operation flip described in [docs/EVOLUTION.md](EVOLUTION.md).
`mutation_prioritization`, `candidate_count` above `1`, and `gene_selection`
are recorded on every strategy but do not yet change what Forge proposes or
how many candidates it considers, so **two strategies that only differ in
those fields cannot show a real efficiency difference today**.
`generation_count`/`experiment_allocation` are the one knob pair that is
already actionable, and the in-process test suite now covers both ends of
that:

- a two-strategy, two-lineage meta-evaluation whose declared
  `generation_count`/`experiment_allocation` are equal and whose bootstrap
  intervals correctly center on zero (no difference to find, and none
  found); and
- `meta_evaluate_shows_a_descendant_strategy_reaching_equal_champions_at_lower_cost`,
  a three-lineage meta-evaluation of a declared parent/descendant pair over
  fresh held-out lineages (whose parent Genome runs the wrong reference
  operation against uppercase-expecting tasks, so the reference-operation-flip
  mutation both fixes it and reaches its ceiling in generation zero). It
  asserts that the ancestor spends a full multi-generation budget, the
  descendant spends exactly the one generation that ever promotes, both reach
  an equally already-optimal Champion on every lineage (one promotion each —
  the two final Genome identities differ only because each strategy's own
  evolve run content-addresses its proposals by that run's own run ID), and
  the receipt's `descendant_cheaper_at_equal_quality` comes back `Some(true)`
  with the cost-delta interval's upper bound below zero — see the known
  limitation below for what has and has not actually been observed to
  complete on this machine.

This would not be evidence of a smarter search policy even once it is
observed to complete — with only the reference-operation flip available, "run
fewer generations once no further improvement is possible" is the only
actionable strategy difference today — but it targets the exact claim
roadmap item 13 asks for: a descendant strategy reaching an equal-or-better
Champion at a statistically lower experiment cost, over held-out lineages,
through the unmodified evolve engine. `descendant_verdict` itself is
replay-verified by unit test and `verify_meta_evolution_history` recomputes
it from the recorded strategies and lineage outcomes on every replay,
independent of whether the slow end-to-end scenario above is enabled.

**Known limitation:** the three-lineage descendant test above is currently
marked `#[ignore]`. It drives ~9 real evolve generations (18 paired Arena
evaluations) through the reference worker/evaluator subprocesses, and every
daemon step re-verifies the entire ledger from scratch
(`refresh_projection` reopens `EvaluationStores` and re-hashes every artifact
inside every `verify_*_history` call), so per-step cost grows with ledger
history length. On the machine this was authored on, that made the test spin
at the CPU for 5+ minutes per attempt instead of completing in a reasonable
time; a fix to that per-step replay cost is in progress on
the full-reign/2026-09-23 lane. Before that ignore was added, earlier runs of the
same scenario did reach `strategy_a_promotions == 1` and
`strategy_b_promotions == 1` on every lineage with the ancestor spending its
full generation budget and the descendant spending exactly its one promoting
generation, confirming the scenario design itself is sound; only wall-clock
cost under the current replay performance blocks running it routinely.
Re-enable the test once the replay performance fix lands.

A genuinely smarter search policy (prioritizing which failure cluster to
mutate first, trying more than one candidate per generation, or consulting
the Gene Bank) needs a richer Forge that reads `mutation_prioritization`,
`candidate_count`, and `gene_selection`, which
does not exist yet.
