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
import time
from pathlib import Path
from typing import Any


MAX_COMMAND_OUTPUT = 64 * 1024
MAX_EVALUATION_SECONDS = 180
MAX_REPORT_BYTES = 16 * 1024
FIXTURE_ROOT = Path(__file__).resolve().parents[1] / "examples" / "gauntlet-reference"
EXPECTED_OPS = ("identity", "ascii_uppercase")


class RunnerError(RuntimeError):
    """A bounded, user-actionable fixture failure."""


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


def _run(command: list[str], *, timeout: int = 20, cwd: Path | None = None) -> str:
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
            env={**os.environ, "LC_ALL": "C", "LANG": "C"},
        )
    except subprocess.TimeoutExpired as error:
        raise RunnerError(f"command exceeded its {timeout}s deadline: {Path(command[0]).name}") from error
    output = completed.stdout
    if (len(output.encode("utf-8")) > MAX_COMMAND_OUTPUT
            or len(completed.stderr.encode("utf-8")) > MAX_COMMAND_OUTPUT):
        raise RunnerError(f"command output exceeded {MAX_COMMAND_OUTPUT} bytes")
    if completed.returncode != 0:
        detail = completed.stderr.strip().replace("\n", " ")[:500]
        raise RunnerError(f"{Path(command[0]).name} failed ({completed.returncode}): {detail}")
    return output.strip()


def _cli(cli: Path, data_dir: Path, *arguments: str, timeout: int = 20) -> dict[str, Any]:
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
          timeout: int = 20) -> dict[str, Any]:
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


def _start_daemon(command: list[str], cli: Path, data_dir: Path) -> subprocess.Popen[str]:
    log_path = data_dir / "daemon.log"
    log = log_path.open("ab", buffering=0)
    process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                               start_new_session=True, text=True)
    log.close()
    try:
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise RunnerError(f"fixture-owned daemon exited during startup; inspect {log_path}")
            try:
                _data(cli, data_dir, "status", "status", timeout=2)
                return process
            except RunnerError:
                time.sleep(0.1)
        raise RunnerError(f"fixture-owned daemon did not become ready within 12 seconds; inspect {log_path}")
    except RunnerError:
        _terminate_owned(process)
        raise


def _terminate_owned(process: subprocess.Popen[str] | None) -> None:
    if process is None or process.poll() is not None:
        return
    os.killpg(process.pid, signal.SIGKILL)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired as error:
        raise RunnerError("fixture-owned daemon did not terminate") from error


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
    process: subprocess.Popen[str] | None = None
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
