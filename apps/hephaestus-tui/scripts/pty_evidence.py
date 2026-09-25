#!/usr/bin/env python3
"""Drive the Ink Runs/Evidence/Costs/Denials screens through a PTY against a live daemon.

Arguments: <app-dir> <data-dir> <world-id> <genome-id>

Navigates Evidence & Costs -> Runs -> Evidence receipts -> Costs -> Denials,
asserting each screen shows real daemon-sourced data (not placeholders) and
that incompatible/registered Worlds are visually separated by a header row.
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
SELECTED = "› "


def main() -> int:
    app_dir, data_dir, world_id, genome_id = sys.argv[1:5]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    terminal_before = termios.tcgetattr(slave)
    env = os.environ.copy()
    env["CI"] = "false"
    env.update({"HEPHAESTUS_HOME": data_dir, "COLUMNS": "80", "LINES": "24"})
    child = subprocess.Popen(
        ["npm", "run", "start"], cwd=app_dir, env=env,
        stdin=slave, stdout=slave, stderr=slave, close_fds=True,
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

    def settle(quiet: float = 0.15, timeout: float = 2.5) -> None:
        deadline = time.monotonic() + timeout
        last_length = len(visible())
        quiet_since = time.monotonic()
        while time.monotonic() < deadline:
            pump(0.05)
            length = len(visible())
            if length != last_length:
                last_length = length
                quiet_since = time.monotonic()
            elif time.monotonic() - quiet_since >= quiet:
                return

    def press(keys: bytes) -> None:
        os.write(master, keys)
        settle()

    def latest_frame(marker: str) -> str:
        text = visible()
        idx = text.rfind(marker)
        return text[idx:] if idx != -1 else text

    short_genome = genome_id.split(":")[-1][:12]
    short_world = world_id.split(":")[-1][:12]

    try:
        if not until("Evidence & Costs", LAUNCH_TIMEOUT_SECONDS):
            raise RuntimeError("TUI did not render the Evidence & Costs action")
        stage = "select-evidence-menu"
        for _ in range(8):
            press(b"\x1b[B")
        if not until(SELECTED + "Evidence & Costs", 3):
            raise RuntimeError("TUI did not select the Evidence & Costs action")
        start = mark()
        press(b"\r")
        if not until("EVIDENCE & COSTS", 5, start):
            raise RuntimeError("TUI did not open the Evidence & Costs submenu")

        stage = "runs"
        start = mark()
        press(b"\r")
        if not until("RUNS", 5, start) or not until(short_world, 5, start):
            raise RuntimeError("TUI Runs screen did not show the World header")
        if not until(short_genome, 5, start):
            raise RuntimeError("TUI Runs screen did not show the real registered Genome")
        runs_frame = latest_frame("RUNS")
        start = mark()
        press(b"\x1b")
        if not until("EVIDENCE & COSTS", 5, start):
            raise RuntimeError("Esc did not return to the Evidence & Costs submenu from Runs")

        stage = "evidence"
        press(b"\x1b[B")
        start = mark()
        press(b"\r")
        if not until("EVIDENCE RECEIPTS", 5, start) or not until(short_world, 5, start):
            raise RuntimeError("TUI Evidence screen did not show the World header")
        evidence_frame = latest_frame("EVIDENCE RECEIPTS")
        if "bps" not in evidence_frame:
            raise RuntimeError("TUI Evidence screen did not show a selection estimate")
        start = mark()
        press(b"\x1b")
        if not until("EVIDENCE & COSTS", 5, start):
            raise RuntimeError("Esc did not return to the Evidence & Costs submenu from Evidence")

        stage = "costs"
        press(b"\x1b[B")
        start = mark()
        press(b"\r")
        if not until("COSTS", 5, start) or not until("total $", 5, start):
            raise RuntimeError("TUI Costs screen did not show a real dollar total")
        costs_frame = latest_frame("COSTS")
        start = mark()
        press(b"\x1b")
        if not until("EVIDENCE & COSTS", 5, start):
            raise RuntimeError("Esc did not return to the Evidence & Costs submenu from Costs")

        stage = "denials"
        press(b"\x1b[B")
        start = mark()
        press(b"\r")
        if not until("DENIALS", 5, start) or not until("request rejected", 5, start):
            raise RuntimeError("TUI Denials screen did not show the refused request")
        denials_frame = latest_frame("DENIALS")

        stage = "quit"
        press(b"\x1b")
        press(b"\x1b")
        press(b"q")
        deadline = time.monotonic() + 5
        while child.poll() is None and time.monotonic() < deadline:
            pump(0.1)
        if child.poll() is None:
            raise RuntimeError("TUI did not exit after q")
        if termios.tcgetattr(slave) != terminal_before:
            raise RuntimeError("TUI did not restore terminal settings on exit")

        print("PTY 80x24: Runs, Evidence receipts, Costs, and Denials all showed real daemon data.")
        print("Runs evidence: " + " | ".join(line.strip(" │") for line in runs_frame.splitlines() if short_genome in line)[:300])
        print("Evidence evidence: " + " | ".join(line.strip(" │") for line in evidence_frame.splitlines() if "bps" in line)[:300])
        print("Costs evidence: " + next((line.strip(" │") for line in costs_frame.splitlines() if "total $" in line), ""))
        print("Denials evidence: " + next((line.strip(" │") for line in denials_frame.splitlines() if "request rejected" in line), ""))
        return 0
    except Exception as error:  # report concise failure evidence
        print(f"PTY evidence failed during {stage}: {error}", file=sys.stderr)
        print(visible()[-2500:], file=sys.stderr)
        if child.poll() is None:
            child.kill()
        child.wait()
        return 1
    finally:
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    raise SystemExit(main())
