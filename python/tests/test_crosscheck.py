import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from hephaestus_lab.crosscheck import (
    MalformedReceipt,
    crosscheck_meta,
    crosscheck_selection,
)

THIS_FILE = Path(__file__).resolve()
TESTS_DIR = THIS_FILE.parent
PYTHON_DIR = TESTS_DIR.parent
FIXTURES_DIR = TESTS_DIR / "fixtures"

SELECTION_FIXTURES = sorted(FIXTURES_DIR.glob("selection_*.json"))
META_FIXTURES = sorted(FIXTURES_DIR.glob("meta_*.json"))


def load(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as handle:
        return json.load(handle)


def run_cli(path: Path, kind: str) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    env["PYTHONPATH"] = str(PYTHON_DIR)
    return subprocess.run(
        [sys.executable, "-m", "hephaestus_lab.crosscheck", str(path), "--kind", kind],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )


def write_mutated(base: dict, path: Path) -> None:
    with path.open("w", encoding="utf-8") as handle:
        json.dump(base, handle)


class FixtureDiscoveryTests(unittest.TestCase):
    def test_at_least_six_selection_fixtures_and_one_meta_fixture_are_checked_in(self) -> None:
        self.assertGreaterEqual(len(SELECTION_FIXTURES), 6)
        self.assertGreaterEqual(len(META_FIXTURES), 1)


class SelectionFixtureAgreementTests(unittest.TestCase):
    def test_every_checked_in_selection_fixture_cross_checks_clean(self) -> None:
        for path in SELECTION_FIXTURES:
            with self.subTest(fixture=path.name):
                report = crosscheck_selection(load(path))
                self.assertTrue(report.ok, msg=[str(m) for m in report.mismatches])

    def test_every_checked_in_selection_fixture_exits_zero_over_the_cli(self) -> None:
        for path in SELECTION_FIXTURES:
            with self.subTest(fixture=path.name):
                result = run_cli(path, "selection")
                self.assertEqual(result.returncode, 0, msg=result.stdout + result.stderr)
                self.assertIn("OK", result.stdout)

    def test_eligible_fixtures_report_metrics_eligible_true(self) -> None:
        for name in ("selection_eligible_v1.json", "selection_eligible_v2.json"):
            receipt = load(FIXTURES_DIR / name)
            self.assertTrue(receipt["metrics_eligible"])
            self.assertTrue(receipt["candidate_pareto_dominates"])

    def test_regression_fixtures_report_metrics_eligible_false(self) -> None:
        for name in ("selection_regression_v1.json", "selection_regression_v2.json"):
            receipt = load(FIXTURES_DIR / name)
            self.assertFalse(receipt["metrics_eligible"])
            self.assertFalse(receipt["candidate_pareto_dominates"])

    def test_v1_and_v2_disagree_on_the_same_recorded_latency_evidence(self) -> None:
        v1 = load(FIXTURES_DIR / "selection_latency_disagreement_v1.json")
        v2 = load(FIXTURES_DIR / "selection_latency_disagreement_v2.json")
        # Same recorded evidence except the algorithm identity itself.
        shared = (
            "correctness_regressions",
            "correctness_unchanged",
            "correctness_improvements",
            "seed",
            "parent_correctness_bps",
            "candidate_correctness_bps",
            "parent_reliability_bps",
            "candidate_reliability_bps",
            "parent_cost_microusd",
            "candidate_cost_microusd",
            "parent_latency_millis",
            "candidate_latency_millis",
        )
        for field in shared:
            self.assertEqual(v1[field], v2[field], msg=field)
        self.assertNotEqual(v1["algorithm"], v2["algorithm"])
        self.assertFalse(v1["candidate_pareto_dominates"])
        self.assertFalse(v1["metrics_eligible"])
        self.assertTrue(v2["candidate_pareto_dominates"])
        self.assertTrue(v2["metrics_eligible"])
        # And our independent cross-check reproduces both verdicts.
        self.assertTrue(crosscheck_selection(v1).ok)
        self.assertTrue(crosscheck_selection(v2).ok)


class MetaFixtureAgreementTests(unittest.TestCase):
    def test_every_checked_in_meta_fixture_cross_checks_clean(self) -> None:
        for path in META_FIXTURES:
            with self.subTest(fixture=path.name):
                report = crosscheck_meta(load(path))
                self.assertTrue(report.ok, msg=[str(m) for m in report.mismatches])

    def test_every_checked_in_meta_fixture_exits_zero_over_the_cli(self) -> None:
        for path in META_FIXTURES:
            with self.subTest(fixture=path.name):
                result = run_cli(path, "meta")
                self.assertEqual(result.returncode, 0, msg=result.stdout + result.stderr)
                self.assertIn("OK", result.stdout)

    def test_descendant_verdict_is_reported_as_not_cross_checked(self) -> None:
        report = crosscheck_meta(load(FIXTURES_DIR / "meta_evaluation_fixture.json"))
        self.assertTrue(any("descendant_cheaper_at_equal_quality" in note for note in report.notes))


class MutationDetectionTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self._tempdir.cleanup)
        self.tmp_path = Path(self._tempdir.name)

    def test_selection_lower_bps_off_by_one_is_detected_by_field_name_and_exit_code(self) -> None:
        receipt = load(FIXTURES_DIR / "selection_eligible_v2.json")
        receipt["lower_bps"] -= 1
        mutated = self.tmp_path / "mutated.json"
        write_mutated(receipt, mutated)

        report = crosscheck_selection(receipt)
        self.assertFalse(report.ok)
        self.assertTrue(any(mismatch.field == "lower_bps" for mismatch in report.mismatches))

        result = run_cli(mutated, "selection")
        self.assertEqual(result.returncode, 1, msg=result.stdout + result.stderr)
        self.assertIn("lower_bps", result.stdout)

    def test_selection_flipped_eligibility_is_detected_by_field_name_and_exit_code(self) -> None:
        receipt = load(FIXTURES_DIR / "selection_eligible_v2.json")
        self.assertTrue(receipt["metrics_eligible"])
        receipt["metrics_eligible"] = False
        mutated = self.tmp_path / "mutated.json"
        write_mutated(receipt, mutated)

        report = crosscheck_selection(receipt)
        self.assertFalse(report.ok)
        self.assertTrue(any(mismatch.field == "metrics_eligible" for mismatch in report.mismatches))

        result = run_cli(mutated, "selection")
        self.assertEqual(result.returncode, 1, msg=result.stdout + result.stderr)
        self.assertIn("metrics_eligible", result.stdout)

    def test_selection_flipped_pareto_dominance_is_detected(self) -> None:
        receipt = load(FIXTURES_DIR / "selection_latency_disagreement_v1.json")
        self.assertFalse(receipt["candidate_pareto_dominates"])
        receipt["candidate_pareto_dominates"] = True
        mutated = self.tmp_path / "mutated.json"
        write_mutated(receipt, mutated)

        result = run_cli(mutated, "selection")
        self.assertEqual(result.returncode, 1, msg=result.stdout + result.stderr)
        self.assertIn("candidate_pareto_dominates", result.stdout)

    def test_selection_invalid_algorithm_is_malformed(self) -> None:
        receipt = load(FIXTURES_DIR / "selection_eligible_v2.json")
        receipt["algorithm"] = "histogram-bootstrap-v3-does-not-exist"
        mutated = self.tmp_path / "mutated.json"
        write_mutated(receipt, mutated)

        with self.assertRaises(MalformedReceipt):
            crosscheck_selection(receipt)

        result = run_cli(mutated, "selection")
        self.assertEqual(result.returncode, 2, msg=result.stdout + result.stderr)

    def test_selection_missing_field_is_malformed(self) -> None:
        receipt = load(FIXTURES_DIR / "selection_eligible_v2.json")
        del receipt["lower_bps"]
        mutated = self.tmp_path / "mutated.json"
        write_mutated(receipt, mutated)

        result = run_cli(mutated, "selection")
        self.assertEqual(result.returncode, 2, msg=result.stdout + result.stderr)

    def test_selection_not_json_is_malformed(self) -> None:
        mutated = self.tmp_path / "not_json.json"
        mutated.write_text("{not json", encoding="utf-8")
        result = run_cli(mutated, "selection")
        self.assertEqual(result.returncode, 2, msg=result.stdout + result.stderr)

    def test_meta_quality_delta_off_by_one_is_detected_by_field_name_and_exit_code(self) -> None:
        receipt = load(FIXTURES_DIR / "meta_evaluation_fixture.json")
        receipt["quality_delta"]["lower_x10000"] -= 1
        mutated = self.tmp_path / "mutated.json"
        write_mutated(receipt, mutated)

        report = crosscheck_meta(receipt)
        self.assertFalse(report.ok)
        self.assertTrue(any(mismatch.field == "quality_delta.lower_x10000" for mismatch in report.mismatches))

        result = run_cli(mutated, "meta")
        self.assertEqual(result.returncode, 1, msg=result.stdout + result.stderr)
        self.assertIn("quality_delta.lower_x10000", result.stdout)

    def test_meta_single_lineage_is_malformed(self) -> None:
        receipt = load(FIXTURES_DIR / "meta_evaluation_fixture.json")
        receipt["lineages"] = receipt["lineages"][:1]
        mutated = self.tmp_path / "mutated.json"
        write_mutated(receipt, mutated)

        result = run_cli(mutated, "meta")
        self.assertEqual(result.returncode, 2, msg=result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
