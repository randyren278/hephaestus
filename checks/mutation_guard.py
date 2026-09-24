#!/usr/bin/env python3
"""Prove the test suite actually guards the invariants it claims to guard.

A green suite is not evidence. An agent that writes both the code and the tests
can produce a suite that passes no matter what the code does. This closes that
hole: for each entry in the manifest, deliberately break one invariant in place,
run the suite, and require it to go RED. A mutation the suite does not catch
(SURVIVED) means that invariant is unguarded -- the test is theatre.

The mutated file is always restored, including on failure, and the restore is
verified byte-for-byte before the process exits.

Language-agnostic: the mutation is a textual find/replace and the suite is
whatever `--test-cmd` names.

    mutation_guard.py --manifest checks/mutations.yaml --assert-min 12
    mutation_guard.py --manifest checks/mutations.json --test-cmd "npm test"
"""
from __future__ import annotations

import argparse
import math
import os
import pathlib
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from manifest import ManifestError, load, mutations  # noqa: E402

# A mutation that removes a guard rail can leave a test waiting on something
# that will now never happen. Without a bound the suite hangs, the whole job is
# killed by the CI timeout, and the log shows no verdict at all -- so cap each
# run well above the honest suite runtime and report the hang as its own class.
DEFAULT_TIMEOUT_SECONDS = 120.0
DEFAULT_TEST_COMMAND = "python -m pytest -q -x"
PROCESS_GROUP_GRACE_SECONDS = 1.0


ANSI = re.compile(r"\x1b\[[0-9;]*[a-zA-Z]")


class ProcessCleanupError(RuntimeError):
    """The mutation suite's process tree could not be proven terminated."""


def _process_group_members(group_id: int) -> set[int]:
    """Return live, non-zombie members of one POSIX process group."""
    try:
        result = subprocess.run(
            ["ps", "-axo", "pid=,pgid=,stat="],
            check=False,
            capture_output=True,
            text=True,
            timeout=PROCESS_GROUP_GRACE_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ProcessCleanupError("could not inspect mutation process group") from error
    if result.returncode != 0:
        raise ProcessCleanupError(
            f"process-group inspection failed with exit {result.returncode}"
        )
    members = set()
    for line in result.stdout.splitlines():
        fields = line.split()
        if len(fields) < 3:
            if fields:
                raise ProcessCleanupError("process-group inspection was malformed")
            continue
        pid, pgid, state = fields[:3]
        try:
            parsed_pid = int(pid)
            parsed_pgid = int(pgid)
        except ValueError as error:
            raise ProcessCleanupError("process-group inspection was malformed") from error
        if parsed_pgid == group_id and not state.startswith("Z"):
            members.add(parsed_pid)
    return members


def _signal_process_group(group_id: int, sig: signal.Signals) -> None:
    """Signal a group, falling back to its observed members on host quirks."""
    try:
        os.killpg(group_id, sig)
        return
    except ProcessLookupError:
        return
    except PermissionError:
        members = _process_group_members(group_id)
    for pid in members:
        try:
            if os.getpgid(pid) != group_id:
                continue
            os.kill(pid, sig)
        except (ProcessLookupError, PermissionError):
            pass


def _stop_process_group(process: subprocess.Popen) -> None:
    """Terminate a mutation suite and every descendant before source restoration."""
    group_id = process.pid
    _signal_process_group(group_id, signal.SIGTERM)
    try:
        process.communicate(timeout=PROCESS_GROUP_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        pass
    # Always escalate the whole group. The leader and its captured pipes may be
    # gone while a silent TERM-ignoring descendant remains in the same group.
    _signal_process_group(group_id, signal.SIGKILL)
    try:
        process.communicate(timeout=PROCESS_GROUP_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        # A descendant can escape the group while retaining a captured writer.
        # Closing our readers prevents cleanup from blocking source restoration.
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                stream.close()
    try:
        process.wait(timeout=PROCESS_GROUP_GRACE_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=PROCESS_GROUP_GRACE_SECONDS)

    deadline = time.monotonic() + PROCESS_GROUP_GRACE_SECONDS
    while True:
        if not _process_group_members(group_id):
            return
        if time.monotonic() >= deadline:
            raise RuntimeError("mutation process group did not terminate after SIGKILL")
        time.sleep(0.01)


def _run(command: list[str], root: pathlib.Path, timeout: float):
    # Text mutations can preserve a source file's size and second-resolution
    # timestamp. Keep Python from accepting a stale local .pyc after restoration.
    with tempfile.TemporaryDirectory(prefix="hephaestus-mutation-pycache-") as cache:
        environment = os.environ.copy()
        environment["PYTHONPYCACHEPREFIX"] = cache
        process = subprocess.Popen(
            command,
            cwd=root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
            start_new_session=True,
        )
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except BaseException:
            _stop_process_group(process)
            raise
        return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def _tail(output: str, lines: int = 15, width: int = 200) -> str:
    """Readable tail of a suite's output.

    Test runners emit colour codes and progress lines that are megabytes of
    escape sequences on one line. Unfiltered, they bury the actual failure the
    operator needs to read.
    """
    kept = []
    for raw in output.strip().splitlines():
        line = ANSI.sub("", raw).rstrip()
        if not line.strip() or not line.strip(" ·.-_✓✔✗✕×"):
            continue
        kept.append(line if len(line) <= width else line[:width] + " ...")
    return "\n".join(f"    {line}" for line in kept[-lines:]) or "    (no output)"


def apply_mutation(entry: dict, root: pathlib.Path, command: list[str],
                   timeout: float) -> tuple[str, str]:
    """Apply one mutation, run the suite, restore the file.

    Returns (status, detail) where status is one of:
      "killed"   -- find matched once, mutation applied, suite went red (good).
      "survived" -- find matched once, mutation applied, suite stayed green
                    (the invariant is unguarded by any test).
      "timeout"  -- the suite neither passed nor failed within the timeout. A
                    test is waiting on something the mutation prevents. That is
                    a non-deterministic suite defect, not a kill: bound the wait
                    so the test fails fast.
      "stale"    -- find did not match exactly once. The manifest text no longer
                    matches the source (usually because a legitimate change
                    edited the surrounding lines). That is a manifest
                    maintenance problem, not evidence the invariant is
                    unguarded, and must never be conflated with "survived".
    """
    target = root / entry["file"]
    if not target.is_file():
        return "stale", (
            f"{entry['file']} does not exist -- the manifest points at a file that "
            f"was moved or deleted; update or remove this entry")

    original = target.read_text()
    count = original.count(entry["find"])
    if count != 1:
        return "stale", (
            f"'find' matched {count} times in {entry['file']} (need exactly 1) -- "
            f"manifest entry no longer matches the current source; update find/replace")

    mutated = original.replace(entry["find"], entry["replace"], 1)
    try:
        target.write_text(mutated)
        try:
            result = _run(command, root, timeout)
        except subprocess.TimeoutExpired:
            return "timeout", (
                f"suite did not finish within {timeout:.0f}s -- a test is blocking on a "
                f"condition this mutation prevents; give that wait a bound so it fails "
                f"fast instead of hanging")
        if result.returncode == 0:
            return "survived", "suite exited 0 (stayed green) -- SURVIVED"
        return "killed", f"suite exited {result.returncode} -- KILLED"
    finally:
        target.write_text(original)
        if target.read_text() != original:
            raise ManifestError(
                f"failed to restore {entry['file']} to its original content after "
                f"mutation {entry['id']!r}; on-disk content does not match the "
                f"pre-mutation snapshot")


def verify_baseline(root: pathlib.Path, command: list[str], timeout: float) -> str | None:
    """Require the unmutated suite to be green.

    Without this, a suite that is already red 'kills' every mutation and the
    whole gate reports PASS while proving nothing. This is the single cheapest
    way to stop the guard from being gamed.
    """
    try:
        result = _run(command, root, timeout)
    except subprocess.TimeoutExpired:
        return f"the unmutated suite did not finish within {timeout:.0f}s"
    if result.returncode != 0:
        return ("the unmutated suite is already failing (exit "
                f"{result.returncode}); every mutation would 'kill' trivially and the "
                "gate would prove nothing. Fix the suite first.\n"
                + _tail(result.stdout or result.stderr or ""))
    return None


def mutation_command(
    entry: dict,
    data: dict,
    default: list[str],
    *,
    allow_scoped: bool = True,
) -> list[str]:
    """Select the narrowest declared suite that owns the mutated source file."""
    if not allow_scoped:
        return default
    configured = data.get("mutation_test_commands", {})
    matches = [
        (prefix, command)
        for prefix, command in configured.items()
        if entry["file"].startswith(prefix)
    ]
    if not matches:
        return default
    _prefix, command = max(matches, key=lambda item: len(item[0]))
    return shlex.split(command)


def mutation_timeout(entry: dict, data: dict, default: float) -> float:
    """Select the narrowest declared timeout that owns the mutated source file."""
    configured = data.get("mutation_timeout_seconds", {})
    matches = [
        (prefix, timeout)
        for prefix, timeout in configured.items()
        if entry["file"].startswith(prefix)
    ]
    timeout = default if not matches else max(matches, key=lambda item: len(item[0]))[1]
    if isinstance(timeout, bool) or not isinstance(timeout, (int, float)):
        raise ManifestError("mutation timeouts must be numbers")
    timeout = float(timeout)
    if not math.isfinite(timeout) or timeout <= 0:
        raise ManifestError("mutation timeouts must be positive and finite")
    return timeout


def shard_entries(
    entries: list[dict], shard_index: int | None, shard_count: int | None
) -> list[dict]:
    """Select one deterministic round-robin partition of the filtered entries."""
    if shard_index is None and shard_count is None:
        return entries
    if shard_index is None or shard_count is None:
        raise ManifestError("mutation shard index and count must be provided together")
    if (
        isinstance(shard_index, bool)
        or not isinstance(shard_index, int)
        or isinstance(shard_count, bool)
        or not isinstance(shard_count, int)
    ):
        raise ManifestError("mutation shard index and count must be integers")
    if shard_count < 1 or shard_index < 0 or shard_index >= shard_count:
        raise ManifestError("mutation shard index must be within a positive shard count")
    return entries[shard_index::shard_count]


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--root", type=pathlib.Path, default=None,
                        help="repo root the suite runs in (default: manifest's repo)")
    parser.add_argument("--test-cmd", default=None,
                        help=f"suite command (default: manifest 'test_command', "
                             f"else {DEFAULT_TEST_COMMAND!r})")
    parser.add_argument("--timeout", type=float, default=None,
                        help=f"per-run seconds (default: manifest 'timeout_seconds', "
                             f"else {DEFAULT_TIMEOUT_SECONDS:.0f})")
    parser.add_argument("--assert-min", type=int, default=0,
                        help="fail unless at least this many mutations ran. This is the "
                             "ratchet: raise it as invariants are added, never lower it")
    parser.add_argument("--skip-baseline", action="store_true",
                        help="do not verify the unmutated suite is green first. Only for "
                             "CI where a prior job already proved it")
    parser.add_argument("--only", action="append", default=[],
                        help="run only this mutation ID; repeat for a focused local rerun")
    parser.add_argument("--file-prefix", default=None,
                        help="run only mutations whose source path starts with this prefix")
    parser.add_argument("--shard-index", type=int, default=None,
                        help="zero-based index of a deterministic mutation shard")
    parser.add_argument("--shard-count", type=int, default=None,
                        help="number of deterministic mutation shards")
    args = parser.parse_args(argv)

    try:
        data = load(args.manifest)
        entries = mutations(data, args.manifest)
        if args.only:
            requested = set(args.only)
            known = {entry["id"] for entry in entries}
            unknown = requested - known
            if unknown:
                raise ManifestError(
                    "unknown mutation ID(s): " + ", ".join(sorted(unknown)))
            entries = [entry for entry in entries if entry["id"] in requested]
        if args.file_prefix:
            entries = [
                entry for entry in entries
                if entry["file"].startswith(args.file_prefix)
            ]
        entries = shard_entries(entries, args.shard_index, args.shard_count)
    except ManifestError as error:
        print(f"MANIFEST ERROR: {error}", file=sys.stderr)
        return 1

    root = (args.root or args.manifest.resolve().parent).resolve()
    while not (root / ".git").exists() and root != root.parent and args.root is None:
        root = root.parent
    command = shlex.split(args.test_cmd or data.get("test_command") or DEFAULT_TEST_COMMAND)
    timeout = args.timeout or data.get("timeout_seconds") or DEFAULT_TIMEOUT_SECONDS
    try:
        entry_timeouts = {
            entry["id"]: (
                args.timeout
                if args.timeout is not None
                else mutation_timeout(entry, data, timeout)
            )
            for entry in entries
        }
        entry_commands = {
            entry["id"]: mutation_command(
                entry,
                data,
                command,
                allow_scoped=args.test_cmd is None,
            )
            for entry in entries
        }
    except ManifestError as error:
        print(f"MANIFEST ERROR: {error}", file=sys.stderr)
        return 1

    print(f"root:    {root}")
    print(f"suite:   {' '.join(command)}"
          f" ({'CLI override' if args.test_cmd is not None else 'manifest default'})")
    selected_timeouts = ", ".join(
        f"{value:.0f}s" for value in sorted(set(entry_timeouts.values())))
    print(f"timeout: {timeout:.0f}s default; selected mutation timeout(s): "
          f"{selected_timeouts}")
    if args.shard_count is not None:
        print(f"shard:   {args.shard_index + 1}/{args.shard_count} "
              f"({len(entries)} mutations)")
    print()

    if not args.skip_baseline:
        problem = verify_baseline(root, command, timeout)
        if problem:
            print(f"FAIL: {problem}", file=sys.stderr)
            return 1
        print("baseline: unmutated suite is green\n", flush=True)

    rows = []
    survived = stale = timed_out = 0
    for entry in entries:
        effective_command = entry_commands[entry["id"]]
        effective_timeout = entry_timeouts[entry["id"]]
        print(
            f"RUN       {entry['id']:32s} command={shlex.join(effective_command)} "
            f"timeout={effective_timeout:.0f}s",
            flush=True,
        )
        try:
            status, detail = apply_mutation(
                entry, root, effective_command, effective_timeout)
        except ManifestError as error:
            print(f"MANIFEST ERROR on {entry['id']}: {error}", file=sys.stderr, flush=True)
            return 1
        if status == "survived":
            survived += 1
        elif status == "stale":
            stale += 1
        elif status == "timeout":
            timed_out += 1
        rows.append((entry["id"], entry["file"], status.upper(), detail, entry["invariant"]))
        # Flush per mutation: CI captures stdout through a pipe, so without this
        # a run that dies part way through shows no verdicts at all.
        print(f"{status.upper():9s} {entry['id']:32s} {entry['file']} "
              f"(timeout {effective_timeout:.0f}s)", flush=True)

    print()
    print(f"{'id':32s} {'file':28s} verdict")
    print("-" * 76)
    for mutation_id, file, verdict, detail, invariant in rows:
        print(f"{mutation_id:32s} {file:28s} {verdict}")
        if verdict != "KILLED":
            print(f"  invariant: {invariant}")
            print(f"  {detail}")

    total = len(rows)
    killed = total - survived - stale - timed_out
    print()
    print(f"{total} mutations run, {killed} killed, {survived} survived, "
          f"{stale} stale, {timed_out} timed out", flush=True)

    if total < args.assert_min:
        print(f"FAIL: only {total} mutations ran, expected at least {args.assert_min} -- "
              f"the ratchet was lowered or entries were removed", file=sys.stderr)
        return 1
    if stale:
        print(f"FAIL: {stale} manifest entry(ies) no longer match the current source -- "
              f"update find/replace. This is NOT evidence of an unguarded invariant",
              file=sys.stderr)
        return 1
    if timed_out:
        print(f"FAIL: {timed_out} mutation(s) hung the suite instead of failing it -- "
              f"bound the blocking wait so the invariant is proven by a fast failure",
              file=sys.stderr)
        return 1
    if survived:
        print(f"FAIL: {survived} mutation(s) survived -- the invariant is unguarded by any "
              f"test. Write the missing test, do not delete the mutation", file=sys.stderr)
        return 1
    print("PASS: all mutations killed")
    return 0


if __name__ == "__main__":  # pragma: no cover - CLI wrapper
    raise SystemExit(main())
