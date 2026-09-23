import unittest

from hephaestus_lab.statistics import (
    ComparisonPolicy,
    FitnessVector,
    PairedOutcome,
    analyze_selection,
    histogram_bootstrap,
    paired_bootstrap,
    pareto_dominates,
)


def outcome(parent: bool, candidate: bool, *, regression: bool = False) -> PairedOutcome:
    return PairedOutcome(
        parent_correct=parent,
        candidate_correct=candidate,
        parent_reliable=True,
        candidate_reliable=True,
        parent_cost_microusd=10,
        candidate_cost_microusd=9,
        parent_latency_millis=20,
        candidate_latency_millis=19,
        invariant_regression=regression,
    )


class StatisticsTests(unittest.TestCase):
    def test_bootstrap_is_seeded_and_reproducible(self) -> None:
        values = [1, 1, 1, 0, 0, 1, 1, 0]
        first = paired_bootstrap(values, seed=42, resamples=1_000)
        second = paired_bootstrap(values, seed=42, resamples=1_000)
        self.assertEqual(first, second)
        self.assertEqual(first.estimate_bps, 6_250)
        self.assertEqual((first.lower_bps, first.upper_bps), (2_500, 10_000))
        short_run = paired_bootstrap(values, seed=42, resamples=100)
        self.assertEqual((short_run.lower_bps, short_run.upper_bps), (2_500, 8_750))

    def test_histogram_bootstrap_matches_canonical_rust_vectors_and_floor_rounding(self) -> None:
        negative = histogram_bootstrap(2, 1, 0, seed=42)
        self.assertEqual(
            (negative.estimate_bps, negative.lower_bps, negative.upper_bps),
            (-6_667, -10_000, 0),
        )
        mixed = histogram_bootstrap(1, 2, 2, seed=7)
        self.assertEqual(
            (mixed.estimate_bps, mixed.lower_bps, mixed.upper_bps),
            (2_000, -4_000, 8_000),
        )
        seeds = {0: 8_333, 7: 8_888, 42: 8_888, 123_456_789: 8_333}
        for seed, upper in seeds.items():
            interval = histogram_bootstrap(1, 5, 12, seed=seed)
            self.assertEqual(
                (interval.estimate_bps, interval.lower_bps, interval.upper_bps),
                (6_111, 3_333, upper),
            )

    def test_histogram_bootstrap_rejects_unsupported_endpoint_and_excessive_work(self) -> None:
        with self.assertRaises(ValueError):
            histogram_bootstrap(0, 1, 1, seed=1, confidence_bps=10_000)
        with self.assertRaises(ValueError):
            histogram_bootstrap(1_001, 1_000, 1_000, seed=1)
        with self.assertRaises(ValueError):
            histogram_bootstrap(1 << 100, 0, 0, seed=1)

    def test_pareto_keeps_dimensions_separate(self) -> None:
        parent = FitnessVector(8_000, 9_000, 100, 100)
        better = FitnessVector(8_500, 9_000, 90, 100)
        tradeoff = FitnessVector(8_500, 9_000, 110, 100)
        self.assertTrue(pareto_dominates(better, parent))
        self.assertFalse(pareto_dominates(tradeoff, parent))
        self.assertFalse(pareto_dominates(parent, parent))

    def test_selection_requires_effect_confidence_regression_and_pareto_gates(self) -> None:
        outcomes = [outcome(False, True) for _ in range(8)]
        policy = ComparisonPolicy(5_000, 0, 9_500)
        accepted = analyze_selection(outcomes, policy, seed=7, resamples=1_000)
        self.assertTrue(accepted.promotion_eligible)
        regressed = list(outcomes)
        regressed[0] = outcome(False, True, regression=True)
        denied = analyze_selection(regressed, policy, seed=7, resamples=1_000)
        self.assertFalse(denied.promotion_eligible)
        tied = analyze_selection(
            [outcome(True, True) for _ in range(8)], policy, seed=7, resamples=1_000
        )
        self.assertEqual(tied.correctness.estimate_bps, 0)
        self.assertFalse(tied.promotion_eligible)

    def test_invalid_statistics_inputs_fail_closed(self) -> None:
        with self.assertRaises(ValueError):
            paired_bootstrap([1], seed=1)
        with self.assertRaises(ValueError):
            paired_bootstrap([1, 0], seed=1, resamples=99)
        with self.assertRaises(ValueError):
            ComparisonPolicy(1, -1, 9_500)
        with self.assertRaises(ValueError):
            PairedOutcome(True, True, True, True, -1, 0, 0, 0)


if __name__ == "__main__":
    unittest.main()
