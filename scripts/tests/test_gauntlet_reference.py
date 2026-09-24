"""Focused offline contract tests for the reference-instruction fixture."""

import importlib.util
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "gauntlet_reference.py"
SPEC = importlib.util.spec_from_file_location("gauntlet_reference", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
gauntlet = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gauntlet
SPEC.loader.exec_module(gauntlet)


class GauntletReferenceTests(unittest.TestCase):
    def test_visible_fixture_covers_multiline_and_non_ascii_ascii_mapping(self):
        manifest = json.loads((gauntlet.FIXTURE_ROOT / "visible.json").read_text(encoding="utf-8"))
        tasks = manifest["tasks"]
        self.assertEqual(len(tasks), 2)
        self.assertIn("\n", tasks[0]["input"])
        self.assertIn("\n", tasks[0]["expected_output"])
        self.assertEqual(tasks[1]["input"], "café — mañana\nüber λ")
        self.assertEqual(tasks[1]["expected_output"], "CAFé — MAñANA\nüBER λ")

    def test_identity_child_preserves_reference_operation_and_changes_lineage(self):
        source = (gauntlet.FIXTURE_ROOT / "agents" / "identity.md").read_text(encoding="utf-8")
        child = gauntlet._identity_child(source, name="child", parent_id="hephaestus:genome:parent")
        self.assertIn("name: child", child)
        self.assertIn('parents: ["hephaestus:genome:parent"]', child)
        self.assertIn('"operation":"identity"', child)

    def test_setup_places_owner_only_task_manifests_outside_candidate_repository(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "fixture"
            root.mkdir(mode=0o700)
            repo, data, external = gauntlet._setup_scratch(root)
            self.assertEqual(repo.stat().st_mode & 0o777, 0o700)
            self.assertEqual(data.stat().st_mode & 0o777, 0o700)
            self.assertEqual(external.stat().st_mode & 0o777, 0o700)
            self.assertNotIn(repo, (external / "visible.json").parents)
            self.assertNotIn(repo, (external / "sealed.json").parents)
            self.assertEqual((external / "visible.json").stat().st_mode & 0o777, 0o600)
            self.assertEqual((external / "sealed.json").stat().st_mode & 0o777, 0o600)
            sealed = json.loads((external / "sealed.json").read_text(encoding="utf-8"))
            self.assertEqual(sealed["visibility"], "sealed")
            self.assertIn("\n", sealed["tasks"][0]["input"])
            self.assertIn("é", sealed["tasks"][0]["input"])

    def test_measured_comparison_contract_requires_unpromoted_durable_receipt(self):
        event = {
            "event_type": "selection.recorded",
            "event_id": "event-1",
            "event_hash": "hash-1",
            "receipt_artifact_id": "blake3:receipt",
        }
        receipt = {
            "invariant_gate_verified": False,
            "promotion_eligible": False,
            "correctness_improvements": 3,
            "correctness_regressions": 0,
            "correctness_unchanged": 0,
            "parent_correctness_bps": 0,
            "candidate_correctness_bps": 10_000,
            "metrics_eligible": True,
        }
        metrics = gauntlet._expected_outcome("improvement", {"event": event, "receipt": receipt})
        self.assertEqual(metrics["correctness_improvements"], 3)
        self.assertEqual(metrics["receipt_artifact_id"], "blake3:receipt")
        receipt["promotion_eligible"] = True
        with self.assertRaises(gauntlet.RunnerError):
            gauntlet._expected_outcome("improvement", {"event": event, "receipt": receipt})

    def test_scratch_path_must_be_new(self):
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaises(gauntlet.RunnerError):
                gauntlet._scratch_root(temporary)

    def test_run_terminates_process_group_when_output_exceeds_capture_bound(self):
        command = [sys.executable, "-c", "import os; [os.write(1, b'x' * 8192) for _ in range(100)]"]
        with self.assertRaisesRegex(gauntlet.RunnerError, "output exceeded"):
            gauntlet._run(command, timeout=3)

    def test_daemon_diagnostic_capture_keeps_only_a_bounded_tail(self):
        capture = gauntlet._Capture(16, keep_tail=True)
        capture.append(b"abcdefghijk")
        capture.append(b"12345678")
        self.assertEqual(capture.text(), "defghijk12345678")
        self.assertLessEqual(len(capture.data), 16)

    def test_exited_leader_with_pipe_holding_child_is_killed_before_reap(self):
        with tempfile.TemporaryDirectory() as temporary:
            marker = Path(temporary) / "late-child-write"
            child = "import pathlib,sys,time; time.sleep(.8); pathlib.Path(sys.argv[1]).write_text('late')"
            parent = "import subprocess,sys; subprocess.Popen([sys.executable,'-c',sys.argv[1],sys.argv[2]])"
            observed = []
            original_kill_group = gauntlet._kill_process_group

            def verify_anchor(process):
                status = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                observed.append(status is not None and status.si_pid == process.pid)
                original_kill_group(process)

            with patch.object(gauntlet, "PIPE_DRAIN_SECONDS", 0.1):
                with patch.object(gauntlet, "_kill_process_group", side_effect=verify_anchor):
                    with self.assertRaisesRegex(gauntlet.RunnerError, "descendant holding"):
                        gauntlet._run([sys.executable, "-c", parent, child, str(marker)], timeout=2)
            self.assertEqual(observed, [True], "PGID was signaled after releasing the leader anchor")
            time.sleep(0.9)
            self.assertFalse(marker.exists(), "pipe-holding descendant survived after leader exit")

    def test_timeout_terminates_descendants_in_the_owned_process_group(self):
        with tempfile.TemporaryDirectory() as temporary:
            marker = Path(temporary) / "late-child-write"
            child = "import pathlib,sys,time; time.sleep(.6); pathlib.Path(sys.argv[1]).write_text('late')"
            parent = "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',sys.argv[1],sys.argv[2]]); time.sleep(5)"
            with self.assertRaisesRegex(gauntlet.RunnerError, "deadline"):
                gauntlet._run([sys.executable, "-c", parent, child, str(marker)], timeout=0.15)
            time.sleep(0.7)
            self.assertFalse(marker.exists(), "timed-out descendant survived its owned process group")

    def test_waitid_support_is_checked_before_spawning(self):
        with patch.object(gauntlet.os, "waitid", None), patch.object(gauntlet.subprocess, "Popen") as spawn:
            with self.assertRaisesRegex(gauntlet.RunnerError, r"waitid\(WNOWAIT\)"):
                gauntlet._run([sys.executable, "-c", "pass"])
            spawn.assert_not_called()

    def test_run_reaps_direct_process_after_group_signal_failure(self):
        spawned = []
        real_popen = gauntlet.subprocess.Popen

        def capture_process(*args, **kwargs):
            process = real_popen(*args, **kwargs)
            spawned.append(process)
            return process

        with patch.object(gauntlet.subprocess, "Popen", side_effect=capture_process):
            with patch.object(gauntlet, "_kill_process_group", side_effect=PermissionError("simulated EPERM")):
                with self.assertRaisesRegex(gauntlet.RunnerError, "signal failed.*killed and reaped"):
                    gauntlet._run([sys.executable, "-c", "import time; time.sleep(5)"], timeout=0.1)
        self.assertEqual(len(spawned), 1)
        self.assertIsNotNone(spawned[0].returncode)
        self.assertTrue(spawned[0].stdout.closed)
        self.assertTrue(spawned[0].stderr.closed)

    def test_fixture_daemon_cleanup_reaps_and_drains_after_signal_failure(self):
        process = gauntlet.subprocess.Popen(
            [sys.executable, "-c", "import time; time.sleep(5)"],
            stdin=gauntlet.subprocess.DEVNULL,
            stdout=gauntlet.subprocess.PIPE,
            stderr=gauntlet.subprocess.STDOUT,
            start_new_session=True,
            bufsize=0,
        )
        tail = gauntlet._Capture(gauntlet.MAX_DAEMON_TAIL_BYTES, keep_tail=True)
        reader = threading.Thread(target=gauntlet._pump, args=(process.stdout, tail), daemon=True)
        reader.start()
        owned = gauntlet._OwnedDaemon(process, tail, reader)
        with patch.object(gauntlet, "_kill_process_group", side_effect=PermissionError("simulated EPERM")):
            with self.assertRaisesRegex(gauntlet.RunnerError, "signal failed.*killed and reaped"):
                gauntlet._terminate_owned(owned)
        self.assertIsNotNone(process.returncode)
        self.assertTrue(process.stdout.closed)
        self.assertFalse(reader.is_alive())


if __name__ == "__main__":
    unittest.main()
