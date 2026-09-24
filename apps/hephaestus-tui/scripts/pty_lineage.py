#!/usr/bin/env python3
"""Drive the Ink lineage screens through a PTY against a live daemon.

Arguments: <app-dir> <data-dir> <world-name> <champion-name> <reason>

Navigates to Lineage and Champions, opens the World, moves to the Champion,
inspects its prompt diff, then rolls the Champion back through the reason
prompt and explicit confirmation.
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
SELECTED = "› "


def main() -> int:
    app_dir, data_dir, world_name, champion_name, reason = sys.argv[1:6]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
    terminal_before = termios.tcgetattr(slave)
    env = os.environ.copy()
    env.update({"HEPHAESTUS_HOME": data_dir, "COLUMNS": "100", "LINES": "30"})
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

    def settle(quiet: float = 0.15, timeout: float = 2.0) -> None:
        """Block until the PTY has gone quiet for `quiet` seconds.

        If a keystroke is written while the app is still busy (e.g. mid
        network round-trip to the daemon), the kernel's tty input queue can
        hold it until the app's next stdin read, which may then also pick up
        a subsequent keystroke in the same read() and hand Ink a combined
        chunk such as "j\\r" instead of a lone Return byte. Ink's key parser
        only recognizes Return on an isolated "\\r", so a coalesced chunk is
        treated as literal text and the keypress is silently lost. Waiting
        for a quiet window after each write gives the app a chance to fully
        drain and render before the next key is sent.
        """
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
        """Return the most recent complete redraw containing `marker`.

        The PTY buffer is cumulative and never cleared, so after several
        redraws it can hold multiple overlapping frames back to back.
        Slicing from a byte offset captured before a redraw finished (as
        `mark()` does) misses the frame that is already on screen. Instead,
        find the last occurrence of a screen-identifying marker and read
        from there, which always reflects the current frame regardless of
        how many redraws happened earlier.
        """
        text = visible()
        idx = text.rfind(marker)
        return text[idx:] if idx != -1 else text

    def selected_line(marker: str) -> str:
        lines = [line for line in latest_frame(marker).splitlines() if SELECTED in line]
        return lines[-1] if lines else ""

    try:
        if not until("Lineage and Champions", 10):
            raise RuntimeError("TUI did not render the lineage action")
        stage = "select-lineage"
        for _ in range(7):
            press(b"\x1b[B")
        if not until(SELECTED + "Lineage and Champions", 3):
            raise RuntimeError("TUI did not select the lineage action")
        stage = "open-worlds"
        start = mark()
        press(b"\r")
        if not until("WORLDS", 5, start) or not until(world_name, 5, start):
            raise RuntimeError("TUI did not list the registered World")
        stage = "open-lineage"
        start = mark()
        press(b"\r")
        if not until("LINEAGE / " + world_name, 5, start) or not until("CHAMPION", 5, start):
            raise RuntimeError("TUI did not show the World lineage with its Champion")
        stage = "find-champion"
        lineage_marker = "LINEAGE / " + world_name
        for _ in range(8):
            pump(0.2)
            line = selected_line(lineage_marker)
            if champion_name in line and "CHAMPION" in line:
                break
            press(b"j")
        else:
            raise RuntimeError("TUI selection never reached the Champion row")
        stage = "inspect-genome"
        start = mark()
        press(b"\r")
        if not until("GENOME / " + champion_name, 5, start) or not until("vs parent", 5, start):
            raise RuntimeError("TUI did not open the Champion Genome detail")
        if not until("+ ", 5, start) or not until("- ", 5, start):
            raise RuntimeError("TUI did not render the prompt diff against the parent")
        diff = latest_frame("GENOME / " + champion_name)
        stage = "back-to-lineage"
        start = mark()
        press(b"\x1b")
        if not until("B roll back Champion", 5, start):
            raise RuntimeError("TUI did not return to the lineage view")
        stage = "rollback-reason"
        start = mark()
        press(b"b")
        if not until("Rollback reason:", 5, start):
            raise RuntimeError("TUI did not open the rollback reason prompt")
        for char in reason:
            os.write(master, char.encode("utf-8"))
            time.sleep(0.02)
        start = mark()
        press(b"\r")
        if not until("Press Y to request", 5, start):
            raise RuntimeError("TUI did not ask for explicit rollback confirmation")
        stage = "confirm-rollback"
        start = mark()
        press(b"y")
        if not until("Rolled back", 10, start):
            raise RuntimeError("TUI did not report the daemon rollback acknowledgement")
        if not until("quarantined", 5, start):
            raise RuntimeError("TUI did not refresh the lineage after rollback")
        acknowledgement = next(line.strip(" │") for line in latest_frame("Rolled back").splitlines() if "Rolled back" in line)
        stage = "quit"
        press(b"q")
        deadline = time.monotonic() + 5
        while child.poll() is None and time.monotonic() < deadline:
            pump(0.1)
        if child.poll() is None:
            raise RuntimeError("TUI did not exit after q")
        if termios.tcgetattr(slave) != terminal_before:
            raise RuntimeError("TUI did not restore terminal settings on exit")
        print("PTY 100x30: lineage navigation reached the Champion and its prompt diff.")
        print("Diff evidence: " + " | ".join(line.strip(" │") for line in diff.splitlines() if line.strip(" │").startswith(("+ ", "- ")))[:400])
        print("Rollback evidence: " + acknowledgement)
        return 0
    except Exception as error:  # report concise failure evidence
        print(f"PTY lineage failed during {stage}: {error}", file=sys.stderr)
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
