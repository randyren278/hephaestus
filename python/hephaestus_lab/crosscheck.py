"""Independent Python cross-check of Rust Arena selection and meta-evolution receipts.

This module never writes anything: it only reads a receipt JSON file the
Rust daemon/CLI already produced (an `hephaestus arena select --json` /
`hephaestus arena show --json` `SelectionReceipt`, or an `hephaestus meta
evaluate --json` / `hephaestus meta show --json` `MetaEvaluationPayload`,
in any of the shapes described below), independently recomputes every field
that the receipt's own recorded evidence makes recomputable, and reports any
disagreement. It has no canonical write authority and no opinion the Rust
receipt does not already assert; it exists only to catch the Rust
implementation (`crates/hephaestus-arena/src/selection.rs`,
`crates/hephaestus-control/src/meta_evolve.rs`) drifting from itself.

Accepted input shapes, unwrapped automatically:

* Selection: a bare `SelectionReceipt`; a `SelectionRecord`
  (`{"evaluation_id", "world_id", "receipt", "event"}`); or a full
  `ApiResponse` with `data.type == "selection"`.
* Meta-evolution: a bare `MetaEvaluationPayload`; a `MetaReceiptRecord`
  (`{"payload", "event"}`); or a full `ApiResponse` with
  `data.type == "meta_evaluation"`.

What is cross-checked for a selection receipt (see `crosscheck_selection`):

* The bootstrap interval (`estimate_bps`, `lower_bps`, `upper_bps`) is
  recomputed from the recorded outcome histogram
  (`correctness_regressions`/`_unchanged`/`_improvements`), `seed`,
  `resamples`, and `confidence_bps`, using the same canonical
  [-1, 0, 1]-ordered histogram expansion, `SplitMix64` generator, and
  floor-division fixed-point arithmetic as
  `hephaestus_arena::selection::bootstrap` (mirrored here through
  `hephaestus_lab.statistics.histogram_bootstrap`, itself already pinned to
  literal Rust reference vectors).
* `candidate_pareto_dominates` is recomputed by dispatching on the
  receipt's own `algorithm` field, exactly as Rust's `analyze` does:
  `histogram-bootstrap-v1` uses the strict no-latency-regression rule
  (`pareto_dominates_v1`); `histogram-bootstrap-pareto-tolerant-v2` tolerates
  a candidate latency increase up to
  `max(10% of parent latency, 50ms * paired task count)`
  (`pareto_dominates_v2`/`latency_tolerance_millis`).
* `metrics_eligible` is recomputed as
  `lower_bps > minimum_delta_bps and candidate_pareto_dominates and
  candidate_cost_microusd <= maximum_cost_microusd`.
* `schema_version` and `resamples` are checked against the constants this
  repository's Rust always emits (`1` and `10_000`).

What is **not** independently re-derived, and why:

* `invariant_gate_verified` and `promotion_eligible` are pinned to the
  constant `false` this repository's `selection.rs::analyze` always emits
  today (the invariant gate is not yet wired into selection promotion). A
  receipt that disagrees with `false` is reported as a mismatch against that
  known-current Rust behavior, not as evidence independently re-derived from
  first principles; if a later Rust revision starts deriving these from real
  invariant evidence, this module must be updated to match.
* `evaluation_id`, `evaluation_event_id`, `evaluation_event_hash`,
  `world_id`, `parent_genome_id`, `candidate_genome_id`, and
  `maximum_regressions` are recorded identity/context with no local evidence
  to recompute them from; they are checked only for presence.

What is cross-checked for a meta-evolution receipt (see `crosscheck_meta`):

* `quality_delta` and `cost_delta` (each an `{estimate,lower,upper}_x10000`
  bootstrap interval) are fully recomputed from the recorded per-lineage
  `strategy_a_promotions`/`strategy_b_promotions` and
  `strategy_a_trials_consumed`/`strategy_b_trials_consumed` fields, using the
  exact same paired-bootstrap arithmetic as
  `hephaestus_control::meta_evolve::paired_bootstrap` (a *different* numeric
  contract than the Arena selection bootstrap: it accepts as few as one
  delta and clamps confidence to 9999bps instead of rejecting 10000bps, so
  it is reimplemented here rather than reusing
  `hephaestus_lab.statistics.paired_bootstrap`).
* `schema_version`, `algorithm`, and `bootstrap_resamples` are checked
  against the constants this repository's Rust always emits (`1`,
  `"lineage-paired-histogram-bootstrap-v1"`, `10_000`).

What is **not**, and why:

* `descendant_cheaper_at_equal_quality` cannot be recomputed from this
  receipt alone. Rust derives it (`meta_evolve::descendant_verdict`) from
  the two compared strategies' registered `parent_strategy_id`, which lives
  in their separate `meta_strategy.registered` events/`MetaStrategyRecord`
  projections, not in the `MetaEvaluationPayload` itself. This module
  reports the recorded value as a note, not a pass/fail.
* `meta_run_id`, `strategy_a_id`, `strategy_b_id`, and every lineage's
  `world_id`/`from_genome_id`/run ids/champion genome ids are recorded
  identity/context, checked only for presence.

Exit codes (see `main`): 0 full agreement, 1 any recomputed field disagrees
with the recorded receipt, 2 malformed input (unparsable JSON, an
unrecognized shape, a missing required field, a non-numeric evidence field,
or an unrecognized `algorithm` identity).
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from typing import Any, Sequence

from hephaestus_lab.statistics import histogram_bootstrap

ALGORITHM_V1 = "histogram-bootstrap-v1"
ALGORITHM_V2 = "histogram-bootstrap-pareto-tolerant-v2"
SELECTION_ALGORITHMS = (ALGORITHM_V1, ALGORITHM_V2)
SELECTION_SCHEMA_VERSION = 1
RESAMPLES = 10_000

META_BOOTSTRAP_ALGORITHM = "lineage-paired-histogram-bootstrap-v1"
META_SCHEMA_VERSION = 1
META_RESAMPLES = 10_000

REQUIRED_SELECTION_FIELDS = (
    "schema_version",
    "algorithm",
    "resamples",
    "seed",
    "evaluation_id",
    "evaluation_event_id",
    "evaluation_event_hash",
    "world_id",
    "parent_genome_id",
    "candidate_genome_id",
    "maximum_cost_microusd",
    "minimum_delta_bps",
    "maximum_regressions",
    "confidence_bps",
    "correctness_regressions",
    "correctness_unchanged",
    "correctness_improvements",
    "estimate_bps",
    "lower_bps",
    "upper_bps",
    "parent_correctness_bps",
    "candidate_correctness_bps",
    "parent_reliability_bps",
    "candidate_reliability_bps",
    "parent_cost_microusd",
    "candidate_cost_microusd",
    "parent_latency_millis",
    "candidate_latency_millis",
    "candidate_pareto_dominates",
    "metrics_eligible",
    "invariant_gate_verified",
    "promotion_eligible",
)

REQUIRED_META_FIELDS = (
    "schema_version",
    "meta_run_id",
    "strategy_a_id",
    "strategy_b_id",
    "confidence_bps",
    "bootstrap_seed",
    "bootstrap_resamples",
    "algorithm",
    "lineages",
    "quality_delta",
    "cost_delta",
    "descendant_cheaper_at_equal_quality",
)

REQUIRED_LINEAGE_FIELDS = (
    "world_id",
    "from_genome_id",
    "strategy_a_run_id",
    "strategy_b_run_id",
    "strategy_a_champion_genome_id",
    "strategy_b_champion_genome_id",
    "strategy_a_promotions",
    "strategy_b_promotions",
    "strategy_a_trials_consumed",
    "strategy_b_trials_consumed",
)

REQUIRED_INTERVAL_FIELDS = ("estimate_x10000", "lower_x10000", "upper_x10000")


class MalformedReceipt(ValueError):
    """Raised for input that is not a recognizable Rust receipt."""


@dataclass(frozen=True)
class Mismatch:
    """One field where the recomputed value disagrees with the recorded one."""

    field: str
    expected: Any
    actual: Any

    def __str__(self) -> str:
        return f"{self.field}: recomputed={self.expected!r} recorded={self.actual!r}"


@dataclass(frozen=True)
class Report:
    """Full cross-check result: every disagreement, plus explanatory notes."""

    mismatches: Sequence[Mismatch]
    notes: Sequence[str]

    @property
    def ok(self) -> bool:
        return not self.mismatches


def _compare(mismatches: list[Mismatch], container: dict, field: str, expected: Any, *, prefix: str = "") -> None:
    actual = container[field]
    if actual != expected:
        mismatches.append(Mismatch(prefix + field, expected, actual))


def _require_fields(container: Any, fields: Sequence[str], context: str) -> dict:
    if not isinstance(container, dict):
        raise MalformedReceipt(f"{context} is not a JSON object")
    missing = [field for field in fields if field not in container]
    if missing:
        raise MalformedReceipt(f"{context} is missing field(s): {', '.join(missing)}")
    return container


def _as_int(container: dict, field: str, context: str) -> int:
    value = container[field]
    if isinstance(value, bool) or not isinstance(value, int):
        raise MalformedReceipt(f"{context}.{field} must be an integer, got {value!r}")
    return value


# --------------------------------------------------------------------------
# Selection receipts
# --------------------------------------------------------------------------


def _latency_tolerance_millis(parent_latency_millis: int, task_count: int) -> int:
    """Mirrors `hephaestus_arena::selection::latency_tolerance_millis`."""

    return max(parent_latency_millis // 10, task_count * 50)


def _pareto_dominates_v1(parent: tuple[int, int, int, int], candidate: tuple[int, int, int, int]) -> bool:
    """Mirrors `hephaestus_arena::selection::pareto_dominates_v1`."""

    no_worse = (
        candidate[0] >= parent[0]
        and candidate[1] >= parent[1]
        and candidate[2] <= parent[2]
        and candidate[3] <= parent[3]
    )
    strictly_better = (
        candidate[0] > parent[0]
        or candidate[1] > parent[1]
        or candidate[2] < parent[2]
        or candidate[3] < parent[3]
    )
    return no_worse and strictly_better


def _pareto_dominates_v2(
    parent: tuple[int, int, int, int],
    candidate: tuple[int, int, int, int],
    tolerance_millis: int,
) -> bool:
    """Mirrors `hephaestus_arena::selection::pareto_dominates_v2`."""

    within_tolerance = candidate[3] <= parent[3] + tolerance_millis
    effective_latency = min(candidate[3], parent[3]) if within_tolerance else candidate[3]
    effective_candidate = (candidate[0], candidate[1], candidate[2], effective_latency)
    return _pareto_dominates_v1(parent, effective_candidate)


def _unwrap_selection(raw: Any) -> Any:
    obj = raw
    if isinstance(obj, dict) and isinstance(obj.get("data"), dict):
        obj = obj["data"]
    if isinstance(obj, dict) and obj.get("type") == "selection" and isinstance(obj.get("selection"), dict):
        obj = obj["selection"]
    if isinstance(obj, dict) and isinstance(obj.get("receipt"), dict) and "algorithm" not in obj:
        obj = obj["receipt"]
    return obj


def crosscheck_selection(raw: Any) -> Report:
    """Independently recomputes and cross-checks one `SelectionReceipt`."""

    receipt = _require_fields(_unwrap_selection(raw), REQUIRED_SELECTION_FIELDS, "selection receipt")
    algorithm = receipt["algorithm"]
    if algorithm not in SELECTION_ALGORITHMS:
        raise MalformedReceipt(
            f"unrecognized selection algorithm {algorithm!r}; expected one of {SELECTION_ALGORITHMS}"
        )

    regressions = _as_int(receipt, "correctness_regressions", "selection receipt")
    unchanged = _as_int(receipt, "correctness_unchanged", "selection receipt")
    improvements = _as_int(receipt, "correctness_improvements", "selection receipt")
    seed = _as_int(receipt, "seed", "selection receipt")
    resamples = _as_int(receipt, "resamples", "selection receipt")
    confidence_bps = _as_int(receipt, "confidence_bps", "selection receipt")
    minimum_delta_bps = _as_int(receipt, "minimum_delta_bps", "selection receipt")
    maximum_cost_microusd = _as_int(receipt, "maximum_cost_microusd", "selection receipt")
    parent = (
        _as_int(receipt, "parent_correctness_bps", "selection receipt"),
        _as_int(receipt, "parent_reliability_bps", "selection receipt"),
        _as_int(receipt, "parent_cost_microusd", "selection receipt"),
        _as_int(receipt, "parent_latency_millis", "selection receipt"),
    )
    candidate = (
        _as_int(receipt, "candidate_correctness_bps", "selection receipt"),
        _as_int(receipt, "candidate_reliability_bps", "selection receipt"),
        _as_int(receipt, "candidate_cost_microusd", "selection receipt"),
        _as_int(receipt, "candidate_latency_millis", "selection receipt"),
    )

    mismatches: list[Mismatch] = []
    notes: list[str] = []

    try:
        interval = histogram_bootstrap(
            regressions,
            unchanged,
            improvements,
            seed=seed,
            resamples=resamples,
            confidence_bps=confidence_bps,
        )
    except ValueError as error:
        mismatches.append(Mismatch("confidence_bps/resamples/correctness_*", "a value Rust would accept", str(error)))
        interval = None

    if interval is not None:
        _compare(mismatches, receipt, "estimate_bps", interval.estimate_bps)
        _compare(mismatches, receipt, "lower_bps", interval.lower_bps)
        _compare(mismatches, receipt, "upper_bps", interval.upper_bps)
        recomputed_lower = interval.lower_bps
    else:
        recomputed_lower = _as_int(receipt, "lower_bps", "selection receipt")

    _compare(mismatches, receipt, "schema_version", SELECTION_SCHEMA_VERSION)
    _compare(mismatches, receipt, "resamples", RESAMPLES)

    task_count = regressions + unchanged + improvements
    if algorithm == ALGORITHM_V1:
        dominates = _pareto_dominates_v1(parent, candidate)
    else:
        tolerance = _latency_tolerance_millis(parent[3], task_count)
        dominates = _pareto_dominates_v2(parent, candidate, tolerance)
    _compare(mismatches, receipt, "candidate_pareto_dominates", dominates)

    metrics_eligible = (
        recomputed_lower > minimum_delta_bps and dominates and candidate[2] <= maximum_cost_microusd
    )
    _compare(mismatches, receipt, "metrics_eligible", metrics_eligible)

    notes.append(
        "invariant_gate_verified and promotion_eligible are pinned to the constant `false` "
        "this repository's selection.rs::analyze always emits today (the invariant gate is "
        "not yet wired into selection promotion); they are not independently re-derived from "
        "recorded evidence."
    )
    _compare(mismatches, receipt, "invariant_gate_verified", False)
    _compare(mismatches, receipt, "promotion_eligible", False)

    notes.append(
        "evaluation_id, evaluation_event_id, evaluation_event_hash, world_id, "
        "parent_genome_id, candidate_genome_id, and maximum_regressions are recorded "
        "identity/context with no local evidence to recompute them from; checked only for "
        "presence."
    )
    return Report(mismatches=mismatches, notes=notes)


# --------------------------------------------------------------------------
# Meta-evolution receipts
# --------------------------------------------------------------------------


class _MetaSplitMix64:
    """Standalone `SplitMix64`, kept independent of `hephaestus_lab.statistics`
    because the meta bootstrap's numeric contract (minimum sample size,
    confidence clamping) differs from the Arena selection bootstrap's."""

    __slots__ = ("_state",)

    def __init__(self, seed: int) -> None:
        self._state = seed & 0xFFFFFFFFFFFFFFFF

    def next(self) -> int:
        self._state = (self._state + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
        value = self._state
        value = ((value ^ (value >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
        value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
        return value ^ (value >> 31)


def _meta_bootstrap(deltas: Sequence[int], seed: int, resamples: int, confidence_bps: int) -> dict:
    """Mirrors `hephaestus_control::meta_evolve::paired_bootstrap` exactly,
    including its clamp-to-9999bps confidence rule (no rejection at
    10000bps) and its minimum sample size of one delta (not two)."""

    if not deltas:
        raise MalformedReceipt("meta bootstrap requires at least one lineage delta")
    if resamples < 1:
        raise MalformedReceipt("meta bootstrap requires at least one resample")
    count = len(deltas)
    estimate = sum(deltas) * 10_000 // count
    generator = _MetaSplitMix64(seed)
    distribution = []
    for _ in range(resamples):
        total = 0
        for _ in range(count):
            total += deltas[generator.next() % count]
        distribution.append(total * 10_000 // count)
    distribution.sort()
    confidence = min(confidence_bps, 9_999)
    tail = (10_000 - confidence) // 2
    lower_index = tail * (resamples - 1) // 10_000
    upper_index = (10_000 - tail) * (resamples - 1) // 10_000
    return {
        "estimate_x10000": estimate,
        "lower_x10000": distribution[lower_index],
        "upper_x10000": distribution[upper_index],
    }


def _unwrap_meta(raw: Any) -> Any:
    obj = raw
    if isinstance(obj, dict) and isinstance(obj.get("data"), dict):
        obj = obj["data"]
    if isinstance(obj, dict) and obj.get("type") == "meta_evaluation" and isinstance(obj.get("receipt"), dict):
        obj = obj["receipt"]
    if isinstance(obj, dict) and isinstance(obj.get("payload"), dict) and "meta_run_id" not in obj:
        obj = obj["payload"]
    return obj


def crosscheck_meta(raw: Any) -> Report:
    """Independently recomputes and cross-checks one `MetaEvaluationPayload`."""

    receipt = _require_fields(_unwrap_meta(raw), REQUIRED_META_FIELDS, "meta receipt")
    lineages = receipt["lineages"]
    if not isinstance(lineages, list) or len(lineages) < 2:
        raise MalformedReceipt("meta receipt lineages must be a list of at least two entries")
    for index, lineage in enumerate(lineages):
        _require_fields(lineage, REQUIRED_LINEAGE_FIELDS, f"meta receipt lineages[{index}]")
    if receipt["strategy_a_id"] == receipt["strategy_b_id"]:
        raise MalformedReceipt("strategy_a_id and strategy_b_id must be distinct")
    _require_fields(receipt["quality_delta"], REQUIRED_INTERVAL_FIELDS, "meta receipt quality_delta")
    _require_fields(receipt["cost_delta"], REQUIRED_INTERVAL_FIELDS, "meta receipt cost_delta")

    seed = _as_int(receipt, "bootstrap_seed", "meta receipt")
    resamples = _as_int(receipt, "bootstrap_resamples", "meta receipt")
    confidence_bps = _as_int(receipt, "confidence_bps", "meta receipt")

    quality_deltas = [
        _as_int(lineage, "strategy_b_promotions", "meta receipt lineage")
        - _as_int(lineage, "strategy_a_promotions", "meta receipt lineage")
        for lineage in lineages
    ]
    cost_deltas = [
        _as_int(lineage, "strategy_b_trials_consumed", "meta receipt lineage")
        - _as_int(lineage, "strategy_a_trials_consumed", "meta receipt lineage")
        for lineage in lineages
    ]

    mismatches: list[Mismatch] = []
    notes: list[str] = []

    quality = _meta_bootstrap(quality_deltas, seed, resamples, confidence_bps)
    cost = _meta_bootstrap(cost_deltas, seed, resamples, confidence_bps)

    recorded_quality = receipt["quality_delta"]
    recorded_cost = receipt["cost_delta"]
    for key in REQUIRED_INTERVAL_FIELDS:
        _compare(mismatches, recorded_quality, key, quality[key], prefix="quality_delta.")
        _compare(mismatches, recorded_cost, key, cost[key], prefix="cost_delta.")

    _compare(mismatches, receipt, "schema_version", META_SCHEMA_VERSION)
    _compare(mismatches, receipt, "algorithm", META_BOOTSTRAP_ALGORITHM)
    _compare(mismatches, receipt, "bootstrap_resamples", META_RESAMPLES)

    notes.append(
        "descendant_cheaper_at_equal_quality cannot be independently recomputed from this "
        "receipt: Rust derives it from the two compared strategies' registered "
        "parent_strategy_id, recorded only in their separate meta_strategy.registered "
        "events/MetaStrategyRecord projections, not in this MetaEvaluationPayload. Recorded "
        f"value: {receipt['descendant_cheaper_at_equal_quality']!r} (not cross-checked)."
    )
    notes.append(
        "meta_run_id, strategy_a_id, strategy_b_id, and every lineage's "
        "world_id/from_genome_id/run ids/champion genome ids are recorded identity/context; "
        "checked only for presence."
    )
    return Report(mismatches=mismatches, notes=notes)


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def _load_json(path: str) -> Any:
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m hephaestus_lab.crosscheck",
        description=(
            "Independently recompute a Rust-produced Arena selection or meta-evolution "
            "receipt from its own recorded evidence and report any disagreement."
        ),
    )
    parser.add_argument("receipt", help="Path to the Rust-produced receipt JSON file")
    parser.add_argument(
        "--kind",
        choices=("selection", "meta"),
        default="selection",
        help="Which receipt shape to cross-check (default: selection)",
    )
    arguments = parser.parse_args(argv)

    try:
        raw = _load_json(arguments.receipt)
    except (OSError, json.JSONDecodeError) as error:
        print(f"hephaestus-lab-crosscheck: malformed input: {error}", file=sys.stderr)
        return 2

    try:
        report = crosscheck_selection(raw) if arguments.kind == "selection" else crosscheck_meta(raw)
    except MalformedReceipt as error:
        print(f"hephaestus-lab-crosscheck: malformed input: {error}", file=sys.stderr)
        return 2

    for note in report.notes:
        print(f"note: {note}")

    if report.ok:
        print("OK: every recomputed field agrees with the recorded receipt")
        return 0

    for mismatch in report.mismatches:
        print(f"MISMATCH {mismatch}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
