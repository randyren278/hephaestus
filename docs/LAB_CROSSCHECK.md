# Lab cross-check

`python/hephaestus_lab/crosscheck.py` is a second, independent implementation
that reads a Rust-produced receipt and recomputes what it can from the
receipt's own recorded evidence, to catch the Rust implementation drifting
from itself. It has no ledger or CAS dependency, no canonical write
authority, and cannot promote anything; it can only agree or disagree with a
receipt Rust already produced.

## Selection receipts

Ground truth: `crates/hephaestus-arena/src/selection.rs` (`analyze`,
`bootstrap`, `SplitMix64`, the `SelectionReceipt` schema, and the two
algorithm identities `histogram-bootstrap-v1` /
`histogram-bootstrap-pareto-tolerant-v2`), reached from
`hephaestus arena select`/`arena show` and the protocol's `SelectionRecord`
in `crates/hephaestus-control/src/protocol.rs`.

```
python3 -m hephaestus_lab.crosscheck <receipt.json> --kind selection
```

The receipt already carries every input its own verdict was computed from
(the outcome histogram, seed, resamples, confidence, and both fitness
vectors), so nothing beyond the receipt file is needed. From those recorded
fields the cross-check independently recomputes:

- the bootstrap interval (`estimate_bps`, `lower_bps`, `upper_bps`), using
  the same canonical `[-1, 0, 1]` histogram expansion, seeded `SplitMix64`
  stream, and floor-division fixed-point arithmetic as Rust;
- `candidate_pareto_dominates`, dispatching on the receipt's own `algorithm`
  field exactly as Rust's `analyze` does: `histogram-bootstrap-v1`'s strict
  no-latency-regression rule, or
  `histogram-bootstrap-pareto-tolerant-v2`'s tolerant rule (a candidate
  latency increase up to
  `max(10% of parent latency, 50ms * paired task count)` still counts as
  tied, not a regression);
- `metrics_eligible` (the confidence/effect-size, Pareto, and World
  cost-ceiling gates together).

Two fields are checked only against this codebase's known-current Rust
behavior, not independently re-derived: `invariant_gate_verified` and
`promotion_eligible` are pinned to the constant `false` that
`crates/hephaestus-arena/src/selection.rs`'s `analyze` always emits today,
because the invariant gate is not yet wired into selection promotion. A
handful of identity/context fields (`evaluation_id`, `evaluation_event_id`,
`evaluation_event_hash`, `world_id`, `parent_genome_id`,
`candidate_genome_id`, `maximum_regressions`) are checked only for presence:
there is no local evidence to recompute them from.

## Meta-evolution receipts

Ground truth: `crates/hephaestus-control/src/meta_evolve.rs`'s
`paired_bootstrap` (a second, independently implemented bootstrap, in the
same histogram-resampling style as the Arena selection receipt but with a
different numeric contract: as few as one lineage delta, and confidence
clamped to 9999bps instead of rejected at 10000bps), and the
`MetaEvaluationPayload` schema in
`crates/hephaestus-control/src/protocol.rs`.

```
python3 -m hephaestus_lab.crosscheck <receipt.json> --kind meta
```

Each lineage in the receipt already records both strategies'
`promotions`/`trials_consumed`, so the cross-check rebuilds the exact
per-lineage `quality_delta`/`cost_delta` inputs and fully recomputes
`quality_delta` and `cost_delta` (each an `estimate`/`lower`/`upper`
`_x10000` bootstrap interval).

One field is recorded but **not** cross-checked, and the tool says so on
every run: `descendant_cheaper_at_equal_quality`. Rust derives it
(`descendant_verdict`) from the two compared strategies' registered
`parent_strategy_id`, which lives only in their separate
`meta_strategy.registered` events, not in the `MetaEvaluationPayload` this
tool reads. Verifying it would require also passing in both strategies'
registered configs, which this tool does not currently accept. As with
selection receipts, per-lineage identity fields (world, genome, run, and
Champion ids) are checked only for presence.

## Exit codes

`0` every recomputed field agrees with the recorded receipt; `1` at least
one recomputed field disagrees (each is printed by name); `2` malformed
input (unparsable JSON, an unrecognized receipt shape, a missing required
field, a non-numeric evidence field, or an unrecognized `algorithm`
identity).

## Fixtures

`python/tests/fixtures/` holds golden receipts generated *from* Rust, not
hand-written: `crates/hephaestus-arena/src/selection.rs`'s `fixtures` test
module and `crates/hephaestus-control/src/meta_evolve.rs`'s
`fixture_meta_evaluation_receipt_matches_the_checked_in_python_fixture` test
build known evidence, call the real `analyze`/`paired_bootstrap`, and
byte-compare the serialized result against the checked-in file. Running
either test with `HEPHAESTUS_WRITE_FIXTURES=1` set (re)writes the fixture
instead of asserting; the fixture is pinned from the Rust side, and Rust
drifting from itself fails `cargo test`, not just the Python cross-check.

The six selection fixtures cover: a clearly eligible candidate
(`python/tests/fixtures/selection_eligible_v1.json` and
`python/tests/fixtures/selection_eligible_v2.json`, `metrics_eligible: true`
under both algorithms), a clear regression that fails both the effect-size
and Pareto gates (`python/tests/fixtures/selection_regression_v1.json` and
`python/tests/fixtures/selection_regression_v2.json`,
`metrics_eligible: false` under both), and one case built to make the two
algorithm identities disagree on the very same recorded evidence
(`python/tests/fixtures/selection_latency_disagreement_v1.json` and
`python/tests/fixtures/selection_latency_disagreement_v2.json`): a candidate
exactly at the `histogram-bootstrap-pareto-tolerant-v2` latency-tolerance
boundary, strictly cheaper, and unchanged on correctness/reliability — the
`v1` fixture sees the raw latency increase and reports
`candidate_pareto_dominates: false`; the `v2` fixture tolerates it and
reports `true`. `python/tests/fixtures/meta_evaluation_fixture.json` covers
a three-lineage meta-evaluation with mixed per-lineage quality/cost deltas.

`python/tests/test_crosscheck.py` loads every fixture and asserts the
cross-check passes, then mutates copies (a `lower_bps` off by one, a
flipped `metrics_eligible`/`candidate_pareto_dominates`, an off-by-one
`quality_delta.lower_x10000`, a missing field, an unrecognized `algorithm`,
unparsable JSON) and asserts each is caught with the exit code and field
name the mismatch document above promises.
