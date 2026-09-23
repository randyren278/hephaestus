"""Deterministic paired statistics with no canonical write authority."""

from dataclasses import dataclass
from typing import Sequence

MAX_BOOTSTRAP_DRAWS = 20_000_000


@dataclass(frozen=True)
class PairedOutcome:
    """One parent/candidate outcome under identical experimental inputs."""

    parent_correct: bool
    candidate_correct: bool
    parent_reliable: bool
    candidate_reliable: bool
    parent_cost_microusd: int
    candidate_cost_microusd: int
    parent_latency_millis: int
    candidate_latency_millis: int
    invariant_regression: bool = False

    def __post_init__(self) -> None:
        for field in (
            "parent_cost_microusd",
            "candidate_cost_microusd",
            "parent_latency_millis",
            "candidate_latency_millis",
        ):
            if getattr(self, field) < 0:
                raise ValueError(f"{field} must be non-negative")


@dataclass(frozen=True)
class BootstrapInterval:
    """A deterministic percentile interval in basis points."""

    estimate_bps: int
    lower_bps: int
    upper_bps: int
    confidence_bps: int
    resamples: int
    seed: int


@dataclass(frozen=True)
class FitnessVector:
    """Separate objective dimensions; no magical aggregate score."""

    correctness_bps: int
    reliability_bps: int
    cost_microusd: int
    latency_millis: int


@dataclass(frozen=True)
class ComparisonPolicy:
    """World-derived selection gates."""

    minimum_delta_bps: int
    maximum_regressions: int
    confidence_bps: int

    def __post_init__(self) -> None:
        if self.maximum_regressions < 0:
            raise ValueError("maximum_regressions must be non-negative")
        if not 1 <= self.confidence_bps <= 10_000:
            raise ValueError("confidence_bps must be between 1 and 10000")


@dataclass(frozen=True)
class SelectionAnalysis:
    """Read-only analysis result for Rust to validate and persist."""

    correctness: BootstrapInterval
    parent: FitnessVector
    candidate: FitnessVector
    regressions: int
    candidate_pareto_dominates: bool
    promotion_eligible: bool


class _SplitMix64:
    """Tiny version-independent deterministic generator."""

    def __init__(self, seed: int) -> None:
        self._state = seed & 0xFFFFFFFFFFFFFFFF

    def next(self) -> int:
        self._state = (self._state + 0x9E3779B97F4A7C15) & 0xFFFFFFFFFFFFFFFF
        value = self._state
        value = ((value ^ (value >> 30)) * 0xBF58476D1CE4E5B9) & 0xFFFFFFFFFFFFFFFF
        value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & 0xFFFFFFFFFFFFFFFF
        return value ^ (value >> 31)


def _mean_bps(values: Sequence[int]) -> int:
    return sum(values) * 10_000 // len(values)


def paired_bootstrap(
    deltas: Sequence[int],
    *,
    seed: int,
    resamples: int = 10_000,
    confidence_bps: int = 9_500,
) -> BootstrapInterval:
    """Bootstrap a paired mean delta using deterministic integer arithmetic."""

    if len(deltas) < 2:
        raise ValueError("paired bootstrap requires at least two outcomes")
    if resamples < 100:
        raise ValueError("resamples must be at least 100")
    if len(deltas) * resamples > MAX_BOOTSTRAP_DRAWS:
        raise ValueError("bootstrap work exceeds the 20000000-draw limit")
    if not 1 <= confidence_bps <= 9_999:
        raise ValueError("confidence_bps must be between 1 and 9999")
    generator = _SplitMix64(seed)
    count = len(deltas)
    distribution = []
    for _ in range(resamples):
        total = 0
        for _ in range(count):
            total += deltas[generator.next() % count]
        distribution.append(total * 10_000 // count)
    distribution.sort()
    tail_bps = (10_000 - confidence_bps) // 2
    lower_index = tail_bps * (resamples - 1) // 10_000
    upper_index = (10_000 - tail_bps) * (resamples - 1) // 10_000
    return BootstrapInterval(
        estimate_bps=_mean_bps(deltas),
        lower_bps=distribution[lower_index],
        upper_bps=distribution[upper_index],
        confidence_bps=confidence_bps,
        resamples=resamples,
        seed=seed,
    )


def histogram_bootstrap(
    regressions: int,
    unchanged: int,
    improvements: int,
    *,
    seed: int,
    resamples: int = 10_000,
    confidence_bps: int = 9_500,
) -> BootstrapInterval:
    """Bootstrap an aggregate Arena histogram in canonical [-1, 0, 1] order.

    Rust selection uses the same histogram expansion and integer floor rules.
    Confidence 10000 remains unsupported by this Python analysis contract.
    """

    counts = (regressions, unchanged, improvements)
    if any(not isinstance(count, int) or count < 0 for count in counts):
        raise ValueError("histogram counts must be non-negative integers")
    count = sum(counts)
    if count < 2:
        raise ValueError("paired bootstrap requires at least two outcomes")
    if resamples < 100:
        raise ValueError("resamples must be at least 100")
    if not 1 <= confidence_bps <= 9_999:
        raise ValueError("confidence_bps must be between 1 and 9999")
    if count * resamples > MAX_BOOTSTRAP_DRAWS:
        raise ValueError("bootstrap work exceeds the 20000000-draw limit")
    deltas = [-1] * regressions + [0] * unchanged + [1] * improvements
    return paired_bootstrap(
        deltas,
        seed=seed,
        resamples=resamples,
        confidence_bps=confidence_bps,
    )


def pareto_dominates(candidate: FitnessVector, parent: FitnessVector) -> bool:
    """Return true only for no-worse dimensions and one strict improvement."""

    no_worse = (
        candidate.correctness_bps >= parent.correctness_bps
        and candidate.reliability_bps >= parent.reliability_bps
        and candidate.cost_microusd <= parent.cost_microusd
        and candidate.latency_millis <= parent.latency_millis
    )
    strictly_better = (
        candidate.correctness_bps > parent.correctness_bps
        or candidate.reliability_bps > parent.reliability_bps
        or candidate.cost_microusd < parent.cost_microusd
        or candidate.latency_millis < parent.latency_millis
    )
    return no_worse and strictly_better


def _fitness(outcomes: Sequence[PairedOutcome], candidate: bool) -> FitnessVector:
    prefix = "candidate" if candidate else "parent"
    count = len(outcomes)
    return FitnessVector(
        correctness_bps=sum(getattr(item, f"{prefix}_correct") for item in outcomes)
        * 10_000
        // count,
        reliability_bps=sum(getattr(item, f"{prefix}_reliable") for item in outcomes)
        * 10_000
        // count,
        cost_microusd=sum(getattr(item, f"{prefix}_cost_microusd") for item in outcomes),
        latency_millis=sum(getattr(item, f"{prefix}_latency_millis") for item in outcomes),
    )


def analyze_selection(
    outcomes: Sequence[PairedOutcome],
    policy: ComparisonPolicy,
    *,
    seed: int,
    resamples: int = 10_000,
) -> SelectionAnalysis:
    """Apply confidence, regression, effect-size, and Pareto gates."""

    deltas = [int(item.candidate_correct) - int(item.parent_correct) for item in outcomes]
    interval = paired_bootstrap(
        deltas,
        seed=seed,
        resamples=resamples,
        confidence_bps=policy.confidence_bps,
    )
    parent = _fitness(outcomes, False)
    candidate = _fitness(outcomes, True)
    regressions = sum(item.invariant_regression for item in outcomes)
    dominates = pareto_dominates(candidate, parent)
    eligible = (
        regressions <= policy.maximum_regressions
        and interval.lower_bps > policy.minimum_delta_bps
        and dominates
    )
    return SelectionAnalysis(
        correctness=interval,
        parent=parent,
        candidate=candidate,
        regressions=regressions,
        candidate_pareto_dominates=dominates,
        promotion_eligible=eligible,
    )
