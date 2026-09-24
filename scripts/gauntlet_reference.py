#!/usr/bin/env python3
"""Run the bounded offline Markdown reference-instruction Gauntlet fixture."""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


MAX_COMMAND_OUTPUT = 64 * 1024
MAX_EVALUATION_SECONDS = 180
MAX_REPORT_BYTES = 16 * 1024
MAX_DAEMON_TAIL_BYTES = 16 * 1024
PIPE_DRAIN_SECONDS = 1.0
PROCESS_EXIT_SECONDS = 2.0
FIXTURE_ROOT = Path(__file__).resolve().parents[1] / "examples" / "gauntlet-reference"
EXPECTED_OPS = ("identity", "ascii_uppercase")


class RunnerError(RuntimeError):
    """A bounded, user-actionable fixture failure."""


class _Capture:
    """Thread-safe bounded byte capture for a subprocess pipe."""

    def __init__(self, limit: int, *, keep_tail: bool = False) -> None:
        self.limit = limit
        self.keep_tail = keep_tail
        self.data = bytearray()
        self.overflow = threading.Event()
        self._lock = threading.Lock()

    def append(self, chunk: bytes) -> None:
        with self._lock:
            if self.keep_tail:
                self.data.extend(chunk)
                if len(self.data) > self.limit:
                    del self.data[:len(self.data) - self.limit]
                return
            available = self.limit - len(self.data)
            self.data.extend(chunk[:available])
            if len(chunk) > available:
                self.overflow.set()

    def text(self) -> str:
        with self._lock:
            return self.data.decode("utf-8", errors="replace")


@dataclass
class _OwnedDaemon:
    process: subprocess.Popen[bytes]
    output_tail: _Capture
    reader: threading.Thread


def _pump(stream: Any, capture: _Capture) -> None:
    try:
        while chunk := stream.read(8192):
            capture.append(chunk)
    except (OSError, ValueError):
        # Closing a pipe after the bounded drain deadline ends this reader.
        pass


def _kill_process_group(process: subprocess.Popen[Any]) -> None:
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def _finish_killed_process(process: subprocess.Popen[Any], readers: list[threading.Thread]) -> None:
    try:
        process.wait(timeout=PROCESS_EXIT_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        try:
            process.wait(timeout=PROCESS_EXIT_SECONDS)
        except subprocess.TimeoutExpired as error:
            raise RunnerError("owned subprocess did not exit after process-group termination") from error
    deadline = time.monotonic() + PIPE_DRAIN_SECONDS
    for reader in readers:
        reader.join(timeout=max(0.0, deadline - time.monotonic()))
    for stream in (process.stdout, process.stderr):
        if stream is not None:
            stream.close()
    for reader in readers:
        reader.join(timeout=0.1)


def _binary(value: str, label: str) -> Path:
    path = Path(value).expanduser().resolve(strict=True)
    if not path.is_file() or not os.access(path, os.X_OK):
        raise RunnerError(f"{label} must be an executable file: {path}")
    return path


def _scratch_root(requested: str | None) -> Path:
    if requested is None:
        # macOS AF_UNIX paths are short; avoid the deeply nested default temp root.
        root = Path(tempfile.mkdtemp(prefix="hg-ref-", dir="/tmp"))
    else:
        root = Path(requested).expanduser().absolute()
        if root.exists():
            raise RunnerError("--work-dir must name a new, nonexistent directory")
        root.mkdir(mode=0o700, parents=True)
    os.chmod(root, 0o700)
    return root


def _run(command: list[str], *, timeout: float = 20, cwd: Path | None = None) -> str:
    try:
        process = subprocess.Popen(
            command,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            bufsize=0,
            env={**os.environ, "LC_ALL": "C", "LANG": "C"},
        )
    except OSError as error:
        raise RunnerError(f"could not start command {Path(command[0]).name}") from error

    stdout = _Capture(MAX_COMMAND_OUTPUT)
    stderr = _Capture(MAX_COMMAND_OUTPUT)
    readers = [
        threading.Thread(target=_pump, args=(process.stdout, stdout), daemon=True),
        threading.Thread(target=_pump, args=(process.stderr, stderr), daemon=True),
    ]
    for reader in readers:
        reader.start()
    deadline = time.monotonic() + timeout
    failure: str | None = None
    try:
        while process.poll() is None:
            if stdout.overflow.is_set() or stderr.overflow.is_set():
                failure = f"command output exceeded {MAX_COMMAND_OUTPUT} bytes"
                break
            if time.monotonic() >= deadline:
                failure = f"command exceeded its {timeout:g}s deadline: {Path(command[0]).name}"
                break
            time.sleep(0.01)
        if failure is not None:
            _kill_process_group(process)
            _finish_killed_process(process, readers)
            raise RunnerError(failure)
        process.wait(timeout=PROCESS_EXIT_SECONDS)
        drain_deadline = time.monotonic() + PIPE_DRAIN_SECONDS
        for reader in readers:
            reader.join(timeout=max(0.0, drain_deadline - time.monotonic()))
        if any(reader.is_alive() for reader in readers):
            _kill_process_group(process)
            _finish_killed_process(process, readers)
            raise RunnerError(f"command left a descendant holding its output pipe: {Path(command[0]).name}")
        if stdout.overflow.is_set() or stderr.overflow.is_set():
            raise RunnerError(f"command output exceeded {MAX_COMMAND_OUTPUT} bytes")
        output = stdout.text()
        if process.returncode != 0:
            detail = stderr.text().strip().replace("\n", " ")[:500]
            raise RunnerError(f"{Path(command[0]).name} failed ({process.returncode}): {detail}")
        return output.strip()
    finally:
        for stream in (process.stdout, process.stderr):
            if stream is not None and not stream.closed:
                stream.close()
        for reader in readers:
            if reader.is_alive():
                reader.join(timeout=0.1)


def _cli(cli: Path, data_dir: Path, *arguments: str, timeout: float = 20) -> dict[str, Any]:
    output = _run(
        [str(cli), "--data-dir", str(data_dir), "--json", *arguments],
        timeout=timeout,
    )
    try:
        response = json.loads(output)
    except json.JSONDecodeError as error:
        raise RunnerError("CLI returned invalid JSON") from error
    if not isinstance(response, dict) or response.get("version") != 1:
        raise RunnerError("CLI response did not match API schema v1")
    if response.get("error") is not None:
        raise RunnerError("CLI returned an API error")
    data = response.get("data")
    if not isinstance(data, dict):
        raise RunnerError("CLI response did not contain typed data")
    return data


def _data(cli: Path, data_dir: Path, expected_type: str, *arguments: str,
          timeout: float = 20) -> dict[str, Any]:
    data = _cli(cli, data_dir, *arguments, timeout=timeout)
    if data.get("type") != expected_type:
        raise RunnerError(f"expected {expected_type} response from {arguments[0]}")
    return data


def _daemon_command(daemon: Path, worker: Path, evaluator: Path,
                    data_dir: Path, source_repo: Path) -> list[str]:
    return [
        str(daemon), "--data-dir", str(data_dir),
        "--source-repository", str(source_repo),
        "--evaluator-executable", str(evaluator),
        "--reference-worker-executable", str(worker),
    ]


def _start_daemon(command: list[str], cli: Path, data_dir: Path) -> _OwnedDaemon:
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, start_new_session=True, bufsize=0)
    output_tail = _Capture(MAX_DAEMON_TAIL_BYTES, keep_tail=True)
    reader = threading.Thread(target=_pump, args=(process.stdout, output_tail), daemon=True)
    reader.start()
    daemon = _OwnedDaemon(process, output_tail, reader)
    try:
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            if process.poll() is not None:
                tail = output_tail.text().strip()[-2000:]
                raise RunnerError(f"fixture-owned daemon exited during startup: {tail}")
            try:
                _data(cli, data_dir, "status", "status", timeout=2)
                return daemon
            except RunnerError:
                time.sleep(0.1)
        tail = output_tail.text().strip()[-2000:]
        raise RunnerError(f"fixture-owned daemon did not become ready within 12 seconds: {tail}")
    except RunnerError:
        _terminate_owned(daemon)
        raise


def _terminate_owned(daemon: _OwnedDaemon | None) -> None:
    if daemon is None:
        return
    process = daemon.process
    _kill_process_group(process)
    _finish_killed_process(process, [daemon.reader])


def _write(path: Path, contents: str) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.write_text(contents, encoding="utf-8")
    os.chmod(path, 0o600)


def _identity_child(source: str, *, name: str, parent_id: str) -> str:
    source = re.sub(r"(?m)^name: .*?$", f"name: {name}", source, count=1)
    source = re.sub(r"(?m)^parents: \[\]$", f'parents: ["{parent_id}"]', source, count=1)
    if f"name: {name}" not in source or f'parents: ["{parent_id}"]' not in source:
        raise RunnerError("could not derive Markdown identity child from fixture")
    return source


def _expected_outcome(name: str, selection: dict[str, Any]) -> dict[str, Any]:
    receipt = selection.get("receipt")
    event = selection.get("event")
    if not isinstance(receipt, dict) or not isinstance(event, dict):
        raise RunnerError("Arena selection did not include its durable receipt and event")
    if (event.get("event_type") != "selection.recorded" or not event.get("event_hash")
            or not event.get("receipt_artifact_id") or not event.get("event_id")):
        raise RunnerError("selection event metadata is missing its receipt or canonical event hash")
    if receipt.get("invariant_gate_verified") is not False or receipt.get("promotion_eligible") is not False:
        raise RunnerError("fixture selection unexpectedly claims invariant verification or promotion")
    expectations = {
        "improvement": (0, 3, 3, 0, 0),
        "regression": (3, 0, 0, 3, 0),
        "tie": (0, 0, 0, 0, 3),
    }
    parent_score, candidate_score, improvements, regressions, unchanged = expectations[name]
    if (receipt.get("parent_correctness_bps"), receipt.get("candidate_correctness_bps"),
            receipt.get("correctness_improvements"), receipt.get("correctness_regressions"),
            receipt.get("correctness_unchanged")) != (
                parent_score * 10_000 // 3, candidate_score * 10_000 // 3,
                improvements, regressions, unchanged):
        raise RunnerError(f"{name} selection did not match its expected measured outcome")
    return {
        "selection_event_id": event["event_id"],
        "selection_event_hash": event["event_hash"],
        "receipt_artifact_id": event.get("receipt_artifact_id"),
        "correctness_improvements": improvements,
        "correctness_regressions": regressions,
        "correctness_unchanged": unchanged,
        "parent_correctness_bps": receipt["parent_correctness_bps"],
        "candidate_correctness_bps": receipt["candidate_correctness_bps"],
        "metrics_eligible": receipt["metrics_eligible"],
        "promotion_eligible": False,
    }


def _setup_scratch(root: Path) -> tuple[Path, Path, Path]:
    repo = root / "candidate-repo"
    data = root / "daemon-data"
    external = root / "operator-fixtures"
    repo.mkdir(mode=0o700)
    data.mkdir(mode=0o700)
    external.mkdir(mode=0o700)
    _write(repo / "README.md", "Scratch source repository for isolated reference runs.\n")
    _run(["git", "init", "-q", str(repo)], timeout=10)
    _run(["git", "-C", str(repo), "add", "README.md"], timeout=10)
    _run(["git", "-C", str(repo), "-c", "user.name=Gauntlet Fixture",
          "-c", "user.email=gauntlet-fixture@invalid.example", "commit", "-qm", "fixture base"], timeout=10)
    _write(external / "visible.json", (FIXTURE_ROOT / "visible.json").read_text(encoding="utf-8"))
    # The sealed manifest is operator-owned and kept outside the candidate Git repo.
    sealed = {
        "schema_version": 1,
        "manifest_id": "reference-gauntlet-sealed-v1",
        "visibility": "sealed",
        "tasks": [{
            "task_id": "multiline-sealed",
            "input": "mixé 123\nlower sealed line",
            "expected_output": "MIXé 123\nLOWER SEALED LINE",
        }],
    }
    _write(external / "sealed.json", json.dumps(sealed, ensure_ascii=False, indent=2) + "\n")
    if repo in (external / "sealed.json").parents or repo in (external / "visible.json").parents:
        raise RunnerError("task manifests must remain outside the candidate repository")
    return repo, data, external


def run_fixture(args: argparse.Namespace) -> dict[str, Any]:
    if sys.platform != "darwin":
        raise RunnerError("candidate execution requires macOS Seatbelt isolation")
    daemon = _binary(args.daemon_bin, "--daemon-bin")
    cli = _binary(args.cli_bin, "--cli-bin")
    evaluator = _binary(args.evaluator_bin, "--evaluator-bin")
    worker = _binary(args.worker_bin, "--worker-bin")
    if not 1 <= args.timeout_seconds <= MAX_EVALUATION_SECONDS:
        raise RunnerError(f"--timeout-seconds must be between 1 and {MAX_EVALUATION_SECONDS}")
    root = _scratch_root(args.work_dir)
    repo, data_dir, external = _setup_scratch(root)
    daemon_command = _daemon_command(daemon, worker, evaluator, data_dir, repo)
    process: _OwnedDaemon | None = None
    report: dict[str, Any] = {
        "schema_version": 1,
        "scope": "offline-reference-instructions",
        "operations": list(EXPECTED_OPS),
        "comparisons": [],
        "scratch_dir": str(root),
        "daemon_restarted": False,
        "replay_verified": False,
        "signed_runtime_receipts_verified_by_replay": False,
    }
    try:
        process = _start_daemon(daemon_command, cli, data_dir)
        visible = _data(cli, data_dir, "artifact", "arena", "manifest", str(external / "visible.json"))["artifact_id"]
        sealed = _data(cli, data_dir, "artifact", "arena", "manifest", str(external / "sealed.json"))["artifact_id"]
        evaluator_id = _data(cli, data_dir, "artifact", "artifact", "put", str(evaluator))["artifact_id"]
        verifier_id = _data(cli, data_dir, "verifier", "verifier")["artifact_id"]
        world_text = (FIXTURE_ROOT / "world.template.json").read_text(encoding="utf-8")
        for token, value in (("__VISIBLE_MANIFEST__", visible), ("__SEALED_MANIFEST__", sealed),
                             ("__EVALUATOR__", evaluator_id), ("__VERIFIER__", verifier_id)):
            world_text = world_text.replace(token, value)
        world_path = root / "operator-fixtures" / "world.json"
        _write(world_path, world_text)
        world_id = _data(cli, data_dir, "world", "world", "register", str(world_path))["world"]["world_id"]

        identity_source = (FIXTURE_ROOT / "agents" / "identity.md").read_text(encoding="utf-8")
        uppercase_template = (FIXTURE_ROOT / "agents" / "uppercase.md").read_text(encoding="utf-8")
        identity_path = root / "operator-fixtures" / "identity.md"
        _write(identity_path, identity_source)
        identity_id = _data(cli, data_dir, "genome", "genome", "register", str(identity_path),
                            "--world", world_id)["genome"]["genome_id"]
        uppercase_path = root / "operator-fixtures" / "uppercase.md"
        _write(uppercase_path, uppercase_template.replace("__PARENT_ID__", identity_id))
        uppercase_id = _data(cli, data_dir, "genome", "genome", "register", str(uppercase_path),
                             "--world", world_id)["genome"]["genome_id"]
        regression_path = root / "operator-fixtures" / "identity-regression.md"
        _write(regression_path, _identity_child(identity_source,
                                                name="gauntlet-identity-regression",
                                                parent_id=uppercase_id))
        regression_id = _data(cli, data_dir, "genome", "genome", "register", str(regression_path),
                              "--world", world_id)["genome"]["genome_id"]
        tie_path = root / "operator-fixtures" / "identity-tie.md"
        _write(tie_path, _identity_child(identity_source,
                                         name="gauntlet-identity-tie",
                                         parent_id=identity_id))
        tie_id = _data(cli, data_dir, "genome", "genome", "register", str(tie_path),
                       "--world", world_id)["genome"]["genome_id"]
        status = _data(cli, data_dir, "acknowledged", "unfreeze")
        if status.get("frozen") is not False:
            raise RunnerError("isolated daemon did not acknowledge unfreeze")

        pairs = [
            ("improvement", identity_id, uppercase_id),
            ("regression", uppercase_id, regression_id),
            ("tie", identity_id, tie_id),
        ]
        persisted: dict[str, dict[str, Any]] = {}
        for label, parent_id, candidate_id in pairs:
            evaluation_id = f"reference-gauntlet-{label}-v1"
            evaluation = _data(cli, data_dir, "evaluation", "arena", "evaluate",
                               evaluation_id, parent_id, candidate_id,
                               timeout=args.timeout_seconds)
            record = evaluation.get("evaluation")
            if not isinstance(record, dict) or record.get("event", {}).get("event_type") != "evaluation.recorded":
                raise RunnerError(f"{label} evaluation did not return durable Arena event metadata")
            if (record.get("parent_genome_id"), record.get("candidate_genome_id"), record.get("visible_total")) != (
                    parent_id, candidate_id, 2):
                raise RunnerError(f"{label} evaluation was not bound to the expected pair and visible task count")
            selection_data = _data(cli, data_dir, "selection", "arena", "select", evaluation_id)
            selection = selection_data.get("selection")
            if not isinstance(selection, dict):
                raise RunnerError(f"{label} selection did not return a durable receipt")
            metrics = _expected_outcome(label, selection)
            report["comparisons"].append({
                "name": label,
                "evaluation_id": evaluation_id,
                "world_id": world_id,
                "parent_genome_id": parent_id,
                "candidate_genome_id": candidate_id,
                "evaluation_event_id": record["event"]["event_id"],
                "evaluation_aggregate_id": record["event"]["aggregate_id"],
                "visible_parent_correct": record["parent_visible_correct"],
                "visible_candidate_correct": record["candidate_visible_correct"],
                **metrics,
            })
            persisted[evaluation_id] = selection

        before = _data(cli, data_dir, "replay", "replay")
        report["replay_verified"] = True
        _terminate_owned(process)
        process = None
        process = _start_daemon(daemon_command, cli, data_dir)
        report["daemon_restarted"] = True
        after = _data(cli, data_dir, "replay", "replay")
        report["signed_runtime_receipts_verified_by_replay"] = True
        if (before.get("frozen"), before.get("active_runs")) != (
                after.get("frozen"), after.get("active_runs")):
            raise RunnerError("replayed daemon state changed across restart")
        for evaluation_id, original in persisted.items():
            replayed = _data(cli, data_dir, "selection", "arena", "select", evaluation_id).get("selection")
            if replayed != original:
                raise RunnerError(f"persisted {evaluation_id} selection changed after daemon restart")
        report["replay_projection_hash_after_restart"] = after["projection_hash"]
        report["evaluation_event_count_before_restart"] = before["event_count"]
        if len(report["comparisons"]) != 3:
            raise RunnerError("fixture did not complete exactly three comparisons")
        encoded = json.dumps(report, ensure_ascii=False, separators=(",", ":"))
        if len(encoded.encode("utf-8")) > MAX_REPORT_BYTES:
            raise RunnerError("fixture report exceeded its size bound")
        return report
    finally:
        _terminate_owned(process)


def _arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon-bin", required=True, help="absolute path to hephaestusd")
    parser.add_argument("--cli-bin", required=True, help="absolute path to hephaestus")
    parser.add_argument("--evaluator-bin", required=True,
                        help="absolute path to hephaestus-reference-evaluator")
    parser.add_argument("--worker-bin", required=True,
                        help="absolute path to hephaestus-reference-worker")
    parser.add_argument("--work-dir", help="new scratch path to create; contents are retained")
    parser.add_argument("--timeout-seconds", type=int, default=90,
                        help=f"per-evaluation command deadline (1..{MAX_EVALUATION_SECONDS})")
    return parser.parse_args()


def main() -> int:
    previous_umask = os.umask(0o077)
    try:
        report = run_fixture(_arguments())
    except (RunnerError, OSError) as error:
        print(json.dumps({"schema_version": 1, "status": "error", "message": str(error)},
                         ensure_ascii=False, separators=(",", ":")), file=sys.stderr)
        return 1
    finally:
        os.umask(previous_umask)
    print(json.dumps({"schema_version": 1, "status": "complete", **report},
                     ensure_ascii=False, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
