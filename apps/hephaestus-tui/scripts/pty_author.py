#!/usr/bin/env python3
"""Drive the Ink Markdown-agent authoring flow through a PTY against a live daemon.

Arguments: <app-dir> <data-dir> <world-name>

Picks the World, accepts the default Markdown Genome source path (creating
it from the starter template), hands the terminal to `$EDITOR` (the caller
sets `EDITOR` to a no-op so this stays deterministic), registers the result
through the real daemon compiler, then runs a paired Test against the
existing parent/Champion via `evaluate_pair` and waits for a live visible
score from the reused Arena progress panel.
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
    app_dir, data_dir, world_name = sys.argv[1:4]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    terminal_before = termios.tcgetattr(slave)
    env = os.environ.copy()
    env["CI"] = "false"
    env.update({"HEPHAESTUS_HOME": data_dir, "COLUMNS": "80", "LINES": "24"})
    env.setdefault("EDITOR", "true")
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

    def settle(quiet: float = 0.15, timeout: float = 3.0) -> None:
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

    try:
        if not until("Author Markdown agent", LAUNCH_TIMEOUT_SECONDS):
            raise RuntimeError("TUI did not render the Author Markdown agent action")
        stage = "select-author"
        for _ in range(9):
            press(b"\x1b[B")
        if not until(SELECTED + "Author Markdown agent", 3):
            raise RuntimeError("TUI did not select the Author Markdown agent action")
        stage = "open-author-world"
        start = mark()
        press(b"\r")
        if not until("WORLDS", 5, start) or not until(world_name, 5, start):
            raise RuntimeError("TUI did not list the registered World to author under")
        stage = "pick-world"
        start = mark()
        press(b"\r")
        if not until("AUTHOR MARKDOWN AGENT / " + world_name, 5, start):
            raise RuntimeError("TUI did not open the Markdown source path prompt")
        stage = "accept-path-and-edit"
        # Ink's own raw-mode setup already differs from the pristine
        # pre-launch termios captured in `terminal_before`, so the
        # regression to catch here is whether `openEditor()` restores
        # Ink's raw-mode settings, not the pristine terminal; snapshot
        # them immediately before handing the terminal to `$EDITOR`.
        ink_raw_mode = termios.tcgetattr(slave)
        start = mark()
        press(b"\r")
        if not until("REGISTER / " + world_name, 10, start):
            raise RuntimeError("TUI did not return from the editor hand-off to the register screen")
        if termios.tcgetattr(slave) != ink_raw_mode:
            raise RuntimeError("TUI did not restore its raw-mode terminal settings after the $EDITOR hand-off")
        stage = "register"
        start = mark()
        press(b"\r")
        if not until("Registered ", 10, start) or not until("Press T to test", 5, start):
            raise RuntimeError("TUI did not confirm daemon registration of the Markdown Genome")
        registered_frame = latest_frame("REGISTER / " + world_name)
        stage = "open-test-parent"
        start = mark()
        press(b"t")
        if not until("LINEAGE / " + world_name, 5, start):
            raise RuntimeError("TUI did not open parent selection for the paired Test")
        stage = "start-test"
        start = mark()
        press(b"\r")
        if not until("ARENA /", 10, start):
            raise RuntimeError("TUI did not start the paired Arena evaluation")
        stage = "await-score"
        if not until("Visible score", 30, start):
            raise RuntimeError("TUI did not display a live visible score for the paired Test")
        score_frame = latest_frame("ARENA /")

        stage = "quit"
        press(b"\x1b")
        press(b"q")
        deadline = time.monotonic() + 5
        while child.poll() is None and time.monotonic() < deadline:
            pump(0.1)
        if child.poll() is None:
            raise RuntimeError("TUI did not exit after q")
        if termios.tcgetattr(slave) != terminal_before:
            raise RuntimeError("TUI did not restore terminal settings on exit")

        print("PTY 80x24: authored, registered, and tested a Markdown agent end to end through the real daemon.")
        print("Registration evidence: " + next((line.strip(" │") for line in registered_frame.splitlines() if "Registered" in line or "Genome" in line), ""))
        print("Test evidence: " + next((line.strip(" │") for line in score_frame.splitlines() if "Visible score" in line), ""))
        return 0
    except Exception as error:  # report concise failure evidence
        print(f"PTY author failed during {stage}: {error}", file=sys.stderr)
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
