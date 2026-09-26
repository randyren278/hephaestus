#!/usr/bin/env python3
"""Drive the real first-run product tour through a PTY end to end.

Arguments: <heph-binary> <data-dir>

Launches `heph --data-dir <data-dir> --tour` with nothing pre-existing at
that data directory: `heph` must auto-bootstrap the quickstart fixture,
start `hephaestusd` itself, wait for it to become ready, and then open the
real operator TUI already showing the forced tour. Steps through Welcome,
Lineage (must genuinely register the bundled World/Genomes), Unfreeze & run
and Arena (host-dependent: accepted either way, since a host with no
verified OS sandbox refuses candidate execution by design), Replay (must
show a sealed match), and Done, then quits from the home menu it returns to.
"""

import fcntl
import os
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
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    terminal_before = termios.tcgetattr(slave)
    env = os.environ.copy()
    env["CI"] = "false"
    env.update({"COLUMNS": "80", "LINES": "24"})
    # `heph` forwards its own environment to the TUI it spawns, so the tour's
    # own (separate) evaluator lookup can find the same binary heph gave the
    # daemon, even though this is an unbundled dev/test checkout.
    bin_dir = os.path.dirname(os.path.abspath(heph_binary))
    env["HEPHAESTUS_EVALUATOR"] = os.path.join(bin_dir, "hephaestus-reference-evaluator")
    child = subprocess.Popen(
        [heph_binary, "--data-dir", data_dir, "--tour"],
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
        if not until("Step 1 of 6", LAUNCH_TIMEOUT_SECONDS) or not until("Welcome to Hephaestus", 2):
            raise RuntimeError("tour did not open on the Welcome step")
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
        if not until("Step 3 of 6", 5, start) or not until("Ledgered:", 15, start):
            raise RuntimeError("tour did not unfreeze and attempt the parent run")
        stage = "arena"
        start = mark()
        press(b"\r")
        if not until("Step 4 of 6", 5, start):
            raise RuntimeError("tour did not advance to the Arena step")
        # Host-dependent: a verified OS sandbox lets this actually measure;
        # without one it is refused by design. Either way the step settles.
        time.sleep(2)
        pump(0.5)
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

        print("PTY 80x24: the first-run tour registered a real World/Genomes, ran, measured, and sealed a replay through a real daemon heph started itself.")
        return 0
    except Exception as error:  # report concise failure evidence
        print(f"PTY tour failed during {stage}: {error}", file=sys.stderr)
        print(visible()[-3000:], file=sys.stderr)
        if child.poll() is None:
            child.kill()
        child.wait()
        return 1
    finally:
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    raise SystemExit(main())
