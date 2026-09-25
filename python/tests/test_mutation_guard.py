from __future__ import annotations

import contextlib
import io
import json
import os
import pathlib
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock


CHECKS = pathlib.Path(__file__).resolve().parents[2] / "checks"
sys.path.insert(0, str(CHECKS))

from manifest import ManifestError, load, mutations  # noqa: E402
from mutation_guard import (  # noqa: E402
    apply_mutation,
    main,
    mutation_command,
    mutation_timeout,
    shard_entries,
    ProcessCleanupError,
    _process_group_members,
    _signal_process_group,
)


class MutationShardingTests(unittest.TestCase):
    def test_control_mutations_are_partitioned_once_across_five_shards(self) -> None:
        manifest_path = CHECKS / "checks.json"
        control = [
            entry
            for entry in mutations(load(manifest_path), manifest_path)
            if entry["file"].startswith("crates/hephaestus-control/")
        ]
        expected_ids = [entry["id"] for entry in control]
        self.assertEqual(len(expected_ids), 78)
        self.assertEqual(len(set(expected_ids)), len(expected_ids))

        shards = [shard_entries(control, index, 5) for index in range(5)]
        sharded_ids = [entry["id"] for shard in shards for entry in shard]

        self.assertEqual([len(shard) for shard in shards], [16, 16, 16, 15, 15])
        self.assertCountEqual(sharded_ids, expected_ids)
        self.assertEqual(len(sharded_ids), len(set(sharded_ids)))
        self.assertEqual(shards, [shard_entries(control, index, 5) for index in range(5)])

    def test_unsharded_selection_preserves_entries_and_invalid_shards_fail(self) -> None:
        entries = [{"id": "one"}, {"id": "two"}]
        self.assertIs(shard_entries(entries, None, None), entries)

        for shard_index, shard_count in (
            (None, 4),
            (0, None),
            (-1, 4),
            (4, 4),
            (0, 0),
        ):
            with self.subTest(shard_index=shard_index, shard_count=shard_count):
                with self.assertRaises(ManifestError):
                    shard_entries(entries, shard_index, shard_count)


class MutationTimeoutTests(unittest.TestCase):
    def test_longest_matching_prefix_wins(self) -> None:
        entry = {"file": "crates/hephaestus-control/src/server.rs"}
        data = {
            "mutation_timeout_seconds": {
                "crates/": 180,
                "crates/hephaestus-control/": 240,
            }
        }

        self.assertEqual(mutation_timeout(entry, data, 120), 240)
        self.assertEqual(
            mutation_timeout({"file": "python/statistics.py"}, data, 120),
            120,
        )

    def test_timeout_must_be_positive_finite_number(self) -> None:
        entry = {"file": "crates/hephaestus-control/src/server.rs"}
        for invalid in (True, "240", 0, -1, float("inf"), float("nan")):
            with self.subTest(invalid=invalid), self.assertRaises(ManifestError):
                mutation_timeout(
                    {**entry},
                    {"mutation_timeout_seconds": {"crates/": invalid}},
                    120,
                )

    def test_cli_command_override_disables_scoped_manifest_command(self) -> None:
        entry = {"file": "crates/hephaestus-control/src/server.rs"}
        data = {
            "mutation_test_commands": {
                "crates/hephaestus-control/": "cargo test -p hephaestus-control"
            }
        }
        override = ["python3", "override.py"]

        self.assertEqual(mutation_command(entry, data, override), [
            "cargo", "test", "-p", "hephaestus-control"
        ])
        self.assertEqual(
            mutation_command(entry, data, override, allow_scoped=False),
            override,
        )

    def test_cli_override_and_effective_settings_are_used_and_logged(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "source.txt"
            target.write_text("guard = true\n")
            manifest = root / "checks.json"
            manifest.write_text(
                json.dumps(
                    {
                        "test_command": "/usr/bin/true",
                        "mutation_test_commands": {"source": "/usr/bin/true"},
                        "timeout_seconds": 9,
                        "mutations": [
                            {
                                "id": "cli-override",
                                "file": "source.txt",
                                "invariant": "the CLI command wins",
                                "find": "true",
                                "replace": "false",
                            }
                        ],
                    }
                )
            )
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                result = main(
                    [
                        "--manifest",
                        str(manifest),
                        "--root",
                        str(root),
                        "--test-cmd",
                        "/usr/bin/false",
                        "--timeout",
                        "3",
                        "--skip-baseline",
                        "--assert-min",
                        "1",
                    ]
                )

            self.assertEqual(result, 0)
            self.assertEqual(target.read_text(), "guard = true\n")
            self.assertIn("suite:   /usr/bin/false (CLI override)", output.getvalue())
            self.assertIn("command=/usr/bin/false timeout=3s", output.getvalue())
            self.assertNotIn("command=/usr/bin/true", output.getvalue())


class MutationProcessLifecycleTests(unittest.TestCase):
    def test_group_signal_does_not_depend_on_process_enumeration(self) -> None:
        with (
            mock.patch("mutation_guard.os.killpg") as kill_group,
            mock.patch(
                "mutation_guard.subprocess.run",
                side_effect=AssertionError("ps must not run"),
            ),
        ):
            _signal_process_group(123, signal.SIGTERM)
        kill_group.assert_called_once_with(123, signal.SIGTERM)

    def test_process_enumeration_failures_are_unknown_not_empty(self) -> None:
        failures = [
            OSError("ps unavailable"),
            subprocess.TimeoutExpired(["ps"], 1),
        ]
        for failure in failures:
            with (
                self.subTest(failure=failure),
                mock.patch("mutation_guard.subprocess.run", side_effect=failure),
                self.assertRaises(ProcessCleanupError),
            ):
                _process_group_members(123)

        failed = subprocess.CompletedProcess(["ps"], 2, "", "failed")
        with (
            mock.patch("mutation_guard.subprocess.run", return_value=failed),
            self.assertRaises(ProcessCleanupError),
        ):
            _process_group_members(123)

        truncated = subprocess.CompletedProcess(["ps"], 0, "456 123\n", "")
        with (
            mock.patch("mutation_guard.subprocess.run", return_value=truncated),
            self.assertRaises(ProcessCleanupError),
        ):
            _process_group_members(123)

    def test_fallback_revalidates_pid_group_before_signaling(self) -> None:
        with (
            mock.patch("mutation_guard.os.killpg", side_effect=PermissionError),
            mock.patch("mutation_guard._process_group_members", return_value={456}),
            mock.patch("mutation_guard.os.getpgid", return_value=999),
            mock.patch("mutation_guard.os.kill") as kill_process,
        ):
            _signal_process_group(123, signal.SIGKILL)
        kill_process.assert_not_called()

    def test_timeout_kills_grandchild_before_restoring_source(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "source.txt"
            target.write_text("guard = true\n")
            pid_file = root / "grandchild.pid"
            status, _detail = apply_mutation(
                {
                    "id": "timeout-tree",
                    "file": "source.txt",
                    "find": "true",
                    "replace": "false",
                },
                root,
                self.process_tree_command(pid_file),
                0.5,
            )

            self.assertEqual(status, "timeout")
            self.assertEqual(target.read_text(), "guard = true\n")
            self.assert_process_gone(self.read_pid(pid_file))

    def test_timeout_kills_silent_grandchild_after_leader_pipes_close(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "source.txt"
            target.write_text("guard = true\n")
            pid_file = root / "silent-grandchild.pid"
            status, _detail = apply_mutation(
                {
                    "id": "silent-timeout-tree",
                    "file": "source.txt",
                    "find": "true",
                    "replace": "false",
                },
                root,
                self.process_tree_command(pid_file, silent=True),
                0.5,
            )

            self.assertEqual(status, "timeout")
            self.assertEqual(target.read_text(), "guard = true\n")
            self.assert_process_gone(self.read_pid(pid_file))

    def test_escaped_pipe_holder_cannot_block_source_restoration(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "source.txt"
            target.write_text("guard = true\n")
            pid_file = root / "escaped-grandchild.pid"
            started = time.monotonic()
            try:
                status, _detail = apply_mutation(
                    {
                        "id": "escaped-timeout-tree",
                        "file": "source.txt",
                        "find": "true",
                        "replace": "false",
                    },
                    root,
                    self.process_tree_command(pid_file, escaped=True),
                    0.5,
                )
                self.assertEqual(status, "timeout")
                self.assertLess(time.monotonic() - started, 4)
                self.assertEqual(target.read_text(), "guard = true\n")
            finally:
                if pid_file.exists():
                    pid = self.read_pid(pid_file)
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    self.assert_process_gone(pid)

    def test_interrupt_kills_grandchild_before_restoring_source(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            target = root / "source.txt"
            target.write_text("guard = true\n")
            pid_file = root / "grandchild.pid"

            def interrupt_after_spawn() -> None:
                deadline = time.monotonic() + 5
                while not pid_file.exists():
                    if time.monotonic() >= deadline:
                        return
                    time.sleep(0.01)
                os.kill(os.getpid(), signal.SIGINT)

            interrupter = threading.Thread(target=interrupt_after_spawn)
            interrupter.start()
            with self.assertRaises(KeyboardInterrupt):
                apply_mutation(
                    {
                        "id": "interrupt-tree",
                        "file": "source.txt",
                        "find": "true",
                        "replace": "false",
                    },
                    root,
                    self.process_tree_command(pid_file),
                    5,
                )
            interrupter.join()

            self.assertEqual(target.read_text(), "guard = true\n")
            self.assert_process_gone(self.read_pid(pid_file))

    @staticmethod
    def process_tree_command(
        pid_file: pathlib.Path,
        *,
        silent: bool = False,
        escaped: bool = False,
    ) -> list[str]:
        grandchild = (
            "import os,pathlib,signal,time;"
            "signal.signal(signal.SIGTERM, signal.SIG_IGN);"
            f"pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid()));"
            "time.sleep(60)"
        )
        options = []
        if silent:
            options.extend(["stdout=subprocess.DEVNULL", "stderr=subprocess.DEVNULL"])
        if escaped:
            options.append("start_new_session=True")
        keyword_arguments = f",{','.join(options)}" if options else ""
        parent = (
            "import subprocess,sys,time;"
            f"subprocess.Popen([sys.executable,'-c',{grandchild!r}]"
            f"{keyword_arguments});"
            "time.sleep(60)"
        )
        return [sys.executable, "-c", parent]

    @staticmethod
    def read_pid(pid_file: pathlib.Path) -> int:
        return int(pid_file.read_text())

    def assert_process_gone(self, pid: int) -> None:
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.01)
        self.fail(f"grandchild process {pid} is still alive")


if __name__ == "__main__":
    unittest.main()
