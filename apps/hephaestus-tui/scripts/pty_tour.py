#!/usr/bin/env python3
"""Drive the real first-run product tour through a PTY end to end.

Arguments: <heph-binary> <empty-data-dir> [--require-measured] [--packaged] [--first-launch]

Launches `heph --data-dir <data-dir> --tour` with nothing pre-existing at
that data directory: `heph` must auto-bootstrap the quickstart fixture,
start `hephaestusd` itself, wait for it to become ready, and then open the
real operator TUI already showing the forced tour. Steps through Welcome,
Lineage (must genuinely register the bundled World/Genomes), Unfreeze & run
and Arena (--require-measured requires real scores and a Selection; otherwise
a host without a verified OS sandbox may refuse execution), Replay (must
show a sealed match), and Done, then quits from the home menu it returns to.
"""

import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import termios
import time

ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
LAUNCH_TIMEOUT_SECONDS = 45


def main() -> int:
    heph_binary, data_dir = sys.argv[1:3]
    require_measured = '--require-measured' in sys.argv[3:]
    packaged = '--packaged' in sys.argv[3:]
    first_launch = '--first-launch' in sys.argv[3:]
    existing = Path(data_dir)
    if existing.exists() and (not existing.is_dir() or any(existing.iterdir())):
        print('tour acceptance requires an empty data directory', file=sys.stderr)
        return 2
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    terminal_before = termios.tcgetattr(slave)
    env = os.environ.copy()
    env["CI"] = "false"
    env.pop('HEPHAESTUS_SOURCE_REPOSITORY', None)
    env.update({"COLUMNS": "80", "LINES": "24"})
    # `heph` forwards its own environment to the TUI it spawns, so the tour's
    # own (separate) evaluator lookup can find the same binary heph gave the
    # daemon, even though this is an unbundled dev/test checkout.
    bin_dir = os.path.dirname(os.path.abspath(heph_binary))
    if packaged:
        env.pop('HEPHAESTUS_EVALUATOR', None)
    else:
        env["HEPHAESTUS_EVALUATOR"] = os.path.join(bin_dir, "hephaestus-reference-evaluator")
    arguments = [heph_binary, '--data-dir', data_dir]
    if not first_launch:
        arguments.append('--tour')
    child = subprocess.Popen(
        arguments,
        env=env, stdin=slave, stdout=slave, stderr=slave, close_fds=True,
    )
    output = bytearray()
    stage = "launch"

    def visible() -> str:
        return ANSI.sub(b"", bytes(output)).decode("utf-8", errors="replace")

    def pump(timeout: float) -> None:
        ready, _, _ = select.select([master], [], [], timeout)
        if ready:
            try:
                output.extend(os.read(master, 65536))
            except OSError:
                pass

    def until(needle: str, timeout: float, after: int = 0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in visible()[after:]:
                return True
            pump(0.1)
        return needle in visible()[after:]

    def mark() -> int:
        pump(0.2)
        return len(visible())

    def press(keys: bytes, quiet: float = 0.2) -> None:
        os.write(master, keys)
        pump(quiet)

    try:
        if first_launch:
            if not until('Just here for the Senate?', LAUNCH_TIMEOUT_SECONDS):
                raise RuntimeError('first-launch choice did not appear')
            press(b'\r')
        if not until("Step 1 of 6", LAUNCH_TIMEOUT_SECONDS) or not until("Welcome to Hephaestus", 2):
            raise RuntimeError("tour did not open on the Welcome step")
        if not until('Every claim below is backed by a receipt', 5):
            raise RuntimeError('tour Welcome step did not finish loading')
        time.sleep(0.2)
        stage = "lineage"
        start = mark()
        press(b"\r")
        if not until("Step 2 of 6", 5, start):
            raise RuntimeError("tour did not advance to the Lineage step")
        if not until("Registered World", 30, start):
            raise RuntimeError("tour did not register the bundled quickstart World and Genomes")
        stage = "unfreeze_run"
        start = mark()
        press(b"\r")
        if not until("Step 3 of 6", 5, start) or not until("Ledgered:", 25, start):
            raise RuntimeError("tour did not unfreeze and attempt the parent run")
        if require_measured and not until('· succeeded', 15, start):
            raise RuntimeError('tour did not complete its reference run')
        stage = "arena"
        start = mark()
        press(b"\r")
        if not until("Step 4 of 6", 5, start):
            raise RuntimeError("tour did not advance to the Arena step")
        deadline = time.monotonic() + 70
        while time.monotonic() < deadline:
            text = visible()[start:]
            if 'Visible score:' in text or 'Arena refused:' in text or 'Something went wrong' in text:
                break
            pump(0.1)
        if 'Visible score:' in visible()[start:]:
            if not until('Visible score: parent 0/1 → candidate 1/1', 5, start):
                raise RuntimeError('tour visible score did not finish rendering')
            if not until('Correctness delta 10000 bps', 5, start) or not until('Eligible for promotion: no', 5, start):
                raise RuntimeError('tour selection did not finish rendering')
        measured = 'Visible score: parent 0/1 → candidate 1/1' in visible()[start:]
        selected = 'Correctness delta 10000 bps' in visible()[start:] and 'Eligible for promotion: no' in visible()[start:]
        if require_measured and not (measured and selected):
            raise RuntimeError('tour did not show the measured score and verified metrics selection')
        if not measured and 'Arena refused:' not in visible()[start:]:
            raise RuntimeError('tour Arena step did not settle')
        stage = "replay"
        start = mark()
        press(b"\r")
        if not until("Step 5 of 6", 5, start) or not until("sealed —", 15, start):
            raise RuntimeError("tour did not prove a sealed replay match")
        stage = "done"
        start = mark()
        press(b"\r")
        if not until("Step 6 of 6", 5, start) or not until("how to see this tour again", 3, start):
            raise RuntimeError("tour did not reach the Done step")
        stage = "finish-to-home"
        start = mark()
        press(b"\r")
        if not until("OPERATOR ACTIONS", 5, start):
            raise RuntimeError("tour completion did not return to the home menu")
        stage = "quit"
        press(b"q")
        deadline = time.monotonic() + 10
        while child.poll() is None and time.monotonic() < deadline:
            pump(0.1)
        if child.poll() is None:
            raise RuntimeError("heph did not exit after q")
        if child.returncode != 0:
            raise RuntimeError(f"heph exited with status {child.returncode}")
        if termios.tcgetattr(slave) != terminal_before:
            raise RuntimeError("TUI did not restore terminal settings on exit")

        print(json.dumps({'packaged_terminal': '80x24', 'empty_data_bootstrap': True,
                          'first_launch_prompt': first_launch, 'packaged_evaluator_lookup': packaged,
                          'registered_fixture': True, 'arena_measured': measured,
                          'metrics_selection_verified': selected, 'replay_verified': True,
                          'completed_six_steps': True, 'terminal_restored': True}))
        return 0
    except Exception as error:  # report concise failure evidence
        print(f"PTY tour failed during {stage}: {error}", file=sys.stderr)
        print(visible()[-3000:], file=sys.stderr)
        if child.poll() is None:
            child.kill()
        child.wait()
        return 1
    finally:
        subprocess.run([heph_binary, '--data-dir', data_dir, 'stop'], env=env,
                       capture_output=True, timeout=15, check=False)
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    raise SystemExit(main())
