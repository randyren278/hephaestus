"""Contracts for the cheap mutation source-anchor preflight."""

import json
import pathlib
import sys
import tempfile
import unittest

CHECKS = pathlib.Path(__file__).resolve().parents[2] / "checks"
sys.path.insert(0, str(CHECKS))

from target_gate import stale_targets  # noqa: E402


class TargetGateTests(unittest.TestCase):
    def test_missing_and_ambiguous_anchors_are_reported(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "source.rs").write_text("once twice twice")
            manifest = root / "checks.json"
            manifest.write_text(
                json.dumps(
                    {
                        "mutations": [
                            {
                                "id": "unique",
                                "file": "source.rs",
                                "invariant": "unique anchor",
                                "find": "once",
                                "replace": "other",
                            },
                            {
                                "id": "duplicate",
                                "file": "source.rs",
                                "invariant": "ambiguous anchor",
                                "find": "twice",
                                "replace": "other",
                            },
                            {
                                "id": "missing",
                                "file": "missing.rs",
                                "invariant": "missing source",
                                "find": "anchor",
                                "replace": "other",
                            },
                        ]
                    }
                )
            )
            self.assertEqual(
                stale_targets(manifest, root),
                [
                    "duplicate: source.rs matched 2 times",
                    "missing: missing.rs matched 0 times",
                ],
            )


if __name__ == "__main__":
    unittest.main()
