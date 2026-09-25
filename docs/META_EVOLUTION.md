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
  "gene_selection": "none"
}
```

Its identity is the BLAKE3 hash of its canonical JSON, exactly like a
compiled Genome; registering it twice with identical content is idempotent
and appends no second event. `generation_count` and `experiment_allocation`
are passed straight through as `evolve start`'s `--generations` and
`--budget` for every lineage the strategy evaluates. `mutation_prioritization`,
`candidate_count`, and `gene_selection` are recorded for a richer Forge to
read later; see "What this proves today" below for what they do now.

**Enforced by construction, not by a runtime check:** `EvolverStrategyConfig`
has no field that names or touches a meta-evaluator, a Law, a receipt, or a
World's budget ceiling. A strategy cannot grant itself authority beyond the
generation ceiling and paired-trial budget any operator could already pass to
`evolve start` by hand.

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

## What this proves today

The only mutation the Forge can propose is the deterministic
reference-operation flip described in [docs/EVOLUTION.md](EVOLUTION.md).
`mutation_prioritization`, `candidate_count` above `1`, and `gene_selection`
are recorded on every strategy but do not yet change what Forge proposes or
how many candidates it considers, so **two strategies that only differ in
those fields cannot show a real efficiency difference today** — the
in-process test suite covers a two-strategy, two-lineage meta-evaluation
whose declared `generation_count`/`experiment_allocation` are equal and whose
bootstrap intervals correctly center on zero, not a case where one strategy
demonstrably out-performs the other. Meta-evaluations that actually compare
efficiency need `generation_count`/`experiment_allocation` to differ between
strategies (which the engine already supports and measures) or a richer
Forge that reads the other fields.
