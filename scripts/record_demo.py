#!/usr/bin/env python3
"""Record a deterministic terminal demo of Hephaestus as an asciicast v2 file.

Story recorded in one continuous 80x24 terminal session:

  1. Start the real daemon against a fresh, isolated data directory.
  2. Register a World and a parent Genome through the CLI, then seed it as
     the World's Champion.
  3. Launch the Ink TUI and, through it, author, register, and paired-Test
     a candidate Markdown Genome against the real daemon and Arena.
  4. Open Lineage and Champions to show the ancestry and roles.

Every command runs against real binaries (built with `cargo build --release
--workspace`) and the real daemon/Arena — nothing here is mocked. The only
concession to determinism is `$EDITOR=true` (a no-op) and pre-writing the
candidate Genome's Markdown source before the TUI opens it, so the "editor"
step is instant and reproducible instead of depending on an interactive
program the recorder cannot script.

Usage:
    python3 scripts/record_demo.py [output.cast]

Re-recording: just run this script again from a clean checkout with the
release binaries built (`cargo build --release --workspace` and
`npm ci` in apps/hephaestus-tui). It builds its own scratch HOME and data
directory under a temp path and cleans them up on exit.
"""
from __future__ import annotations

import fcntl
import json
import os
import pty
import re
import select
import shutil
import struct
import signal
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path

ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
ROOT = Path(__file__).resolve().parents[1]
COLUMNS, ROWS = 80, 24


def find_release_binaries() -> Path:
    for candidate in (
        ROOT / "target/release",
        ROOT / "target/package-build/aarch64-apple-darwin/release",
        ROOT / "target/package-build/x86_64-apple-darwin/release",
    ):
        if (candidate / "hephaestusd").exists():
            return candidate
    raise SystemExit(
        "no release binaries found; run `cargo build --release --workspace` first"
    )


def descendant_pids(root: int) -> list[int]:
    """Returns every live descendant of `root`, deepest first."""
    listing = subprocess.run(
        ["ps", "-A", "-o", "pid=,ppid="], capture_output=True, text=True, check=False
    ).stdout
    children: dict[int, list[int]] = {}
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) == 2:
            children.setdefault(int(fields[1]), []).append(int(fields[0]))
    ordered: list[int] = []
    stack = list(children.get(root, []))
    while stack:
        pid = stack.pop()
        ordered.append(pid)
        stack.extend(children.get(pid, []))
    return list(reversed(ordered))


class Recorder:
    """Drives one PTY child and records every byte it writes as asciicast v2."""

    def __init__(self) -> None:
        self.events: list[tuple[float, str, str]] = []
        self.start = time.monotonic()
        self.output = bytearray()
        self.master = -1
        self.slave = -1
        self.child: subprocess.Popen | None = None

    def spawn(self, argv: list[str], *, cwd: str, env: dict[str, str]) -> None:
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
        self.child = subprocess.Popen(
            argv, cwd=cwd, env=env,
            stdin=self.slave, stdout=self.slave, stderr=self.slave, close_fds=True,
        )

    def visible(self) -> str:
        return ANSI.sub(b"", bytes(self.output)).decode("utf-8", errors="replace")

    def pump(self, timeout: float) -> None:
        ready, _, _ = select.select([self.master], [], [], timeout)
        if ready:
            try:
                chunk = os.read(self.master, 65536)
            except OSError:
                return
            if chunk:
                self.output.extend(chunk)
                self.events.append((time.monotonic() - self.start, "o", chunk.decode("utf-8", errors="replace")))

    def until(self, needle: str, timeout: float, after: int = 0) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.visible()[after:]:
                return True
            self.pump(0.1)
        return needle in self.visible()[after:]

    def mark(self) -> int:
        self.pump(0.05)
        return len(self.visible())

    def settle(self, quiet: float = 0.15, timeout: float = 3.0) -> None:
        deadline = time.monotonic() + timeout
        last_length = len(self.visible())
        quiet_since = time.monotonic()
        while time.monotonic() < deadline:
            self.pump(0.05)
            length = len(self.visible())
            if length != last_length:
                last_length = length
                quiet_since = time.monotonic()
            elif time.monotonic() - quiet_since >= quiet:
                return

    def press(self, keys: bytes, *, pause: float = 0.0) -> None:
        if pause:
            time.sleep(pause)
        os.write(self.master, keys)
        self.settle()

    def type_line(self, text: str) -> None:
        """Feeds a shell command a character at a time so the recording shows realistic typing, then presses Enter."""
        for char in text:
            os.write(self.master, char.encode())
            time.sleep(0.012)
        self.press(b"\r")

    def write_cast(self, path: Path) -> None:
        header = {
            "version": 2, "width": COLUMNS, "height": ROWS,
            "timestamp": int(time.time()), "env": {"TERM": "xterm-256color", "SHELL": "/bin/bash"},
            "title": "Hephaestus: daemon start, World/Genome registration, TUI authoring, paired evaluation, and lineage",
        }
        with path.open("w", encoding="utf-8") as handle:
            handle.write(json.dumps(header) + "\n")
            for elapsed, kind, data in self.events:
                handle.write(json.dumps([round(elapsed, 6), kind, data]) + "\n")

    def close(self) -> None:
        if self.child and self.child.poll() is None:
            # The shell backgrounds the daemon and runs the TUI as its own
            # jobs; killing only the shell would orphan them, so stop every
            # descendant while the process tree is still intact.
            for pid in descendant_pids(self.child.pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            self.child.kill()
            self.child.wait()
        for fd in (self.master, self.slave):
            try:
                os.close(fd)
            except OSError:
                pass


def main() -> int:
    out_path = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "docs/demo/hephaestus-tui-demo.cast"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    binaries = find_release_binaries()

    home = Path(tempfile.mkdtemp(prefix="hephaestus-demo-home-"))
    data_dir = home / "data"
    data_dir.mkdir(parents=True)
    data_dir.chmod(0o700)
    daemon_pid = None
    recorder = Recorder()
    try:
        env = os.environ.copy()
        env.update({
            "HOME": str(home), "PATH": f"{binaries}:{os.environ.get('PATH', '/usr/bin:/bin')}",
            "COLUMNS": str(COLUMNS), "LINES": str(ROWS), "TERM": "xterm-256color",
            "EDITOR": "true", "PS1": "demo$ ",
        })
        recorder.spawn(["/bin/bash", "--noprofile", "--norc"], cwd=str(home), env=env)
        recorder.until("demo$", 5)

        examples = ROOT / "examples/quickstart"

        def run_shown(command: str) -> None:
            start = recorder.mark()
            recorder.type_line(command)
            if not recorder.until("demo$", 20, start):
                raise RuntimeError(f"shell prompt did not return after: {command}")

        run_shown(
            f'"{binaries}/hephaestusd" --data-dir "{data_dir}" --source-repository "{ROOT}" '
            f'--evaluator-executable "{binaries}/hephaestus-reference-evaluator" > "{home}/daemon.log" 2>&1 &'
        )
        run_shown("sleep 1")

        cli = f'"{binaries}/hephaestus" --data-dir "{data_dir}"'
        run_shown(f"{cli} status")

        run_shown(f'VISIBLE=$({cli} arena manifest "{examples}/tasks/visible.json" | awk \'{{print $1}}\')')
        run_shown(f'SEALED=$({cli} arena manifest "{examples}/tasks/sealed.json" | awk \'{{print $1}}\')')
        run_shown(f'EVALUATOR=$({cli} artifact put "{binaries}/hephaestus-reference-evaluator" | awk \'{{print $1}}\')')
        run_shown(f'VERIFIER=$({cli} verifier | awk \'{{print $1}}\')')
        run_shown(
            f'sed -e "s/__VISIBLE_MANIFEST__/$VISIBLE/" -e "s/__SEALED_MANIFEST__/$SEALED/" '
            f'-e "s/__EVALUATOR__/$EVALUATOR/" -e "s/__VERIFIER__/$VERIFIER/" '
            f'"{examples}/world.template.json" > "{home}/world.json"'
        )
        run_shown(f'WORLD=$({cli} world register "{home}/world.json" | awk \'{{print $1}}\')')
        run_shown(f"echo World=$WORLD")
        run_shown(f'PARENT=$({cli} genome register "{examples}/agent.md" --world "$WORLD" | awk \'{{print $1}}\')')
        marker = recorder.mark()
        run_shown(f"echo Parent=$PARENT")
        # Match only the printed value, not the terminal's echo of the typed
        # `echo Parent=$PARENT` command itself (which also contains the
        # literal substring "Parent=$PARENT").
        parent_match = re.search(r"Parent=(hephaestus:genome:\S+)", recorder.visible()[marker:])
        if not parent_match:
            raise RuntimeError("could not read the registered parent Genome ID back from the terminal")
        parent_id = parent_match.group(1)
        run_shown(f"{cli} unfreeze")
        run_shown(f'{cli} champion seed demo-seed --world "$WORLD" --genome "$PARENT" --reason "Demo Champion seed"')

        # Pre-write the candidate Markdown source at the TUI's default authoring
        # path so the "author" step is instant and reproducible: this is the
        # deterministic stand-in for a human editing in $EDITOR. Written
        # directly (not through the recorded shell) to avoid quoting the
        # Genome ID through printf/YAML in the terminal.
        agents_dir = home / ".hephaestus/agents"
        agents_dir.mkdir(parents=True, exist_ok=True)
        candidate_source = (
            "---\nschema_version: 1\nname: demo-candidate\n"
            f'parents: ["{parent_id}"]\n'
            "model:\n  provider: deterministic\n  family: reference\n"
            "authority:\n  workspace_write: false\n  network: false\nartifacts: {}\n"
            "---\n```hephaestus-reference-v1\n"
            '{"schema_version":1,"operation":"ascii_uppercase"}\n```\n'
        )
        (agents_dir / "quickstart-world.md").write_text(candidate_source, encoding="utf-8")
        run_shown('echo "Prepared candidate Markdown Genome for the TUI to register"')

        run_shown(f'export HEPHAESTUS_HOME="{data_dir}"')
        run_shown(f'cd "{ROOT / "apps/hephaestus-tui"}"')
        start = recorder.mark()
        recorder.type_line("npm start")

        # --- Drive the TUI: Lineage and Champions first, to show the seeded Champion. ---
        if not recorder.until("Lineage and Champions", 45, start):
            raise RuntimeError("TUI did not render the home menu")
        for _ in range(7):
            recorder.press(b"\x1b[B", pause=0.12)
        if not recorder.until("› Lineage and Champions", 3):
            raise RuntimeError("TUI did not select Lineage and Champions")
        start = recorder.mark()
        recorder.press(b"\r", pause=0.3)
        if not recorder.until("WORLDS", 5, start):
            raise RuntimeError("TUI did not list Worlds")
        start = recorder.mark()
        recorder.press(b"\r", pause=0.3)
        if not recorder.until("CHAMPION", 5, start):
            raise RuntimeError("TUI did not show the seeded Champion")
        time.sleep(1.0)
        recorder.press(b"\x1b", pause=0.2)  # back to Worlds
        recorder.press(b"\x1b", pause=0.2)  # back to home

        # --- Author, register, and Test the candidate Markdown agent. ---
        if not recorder.until("Author Markdown agent", 5):
            raise RuntimeError("TUI did not return to the home menu")
        for _ in range(9):
            recorder.press(b"\x1b[B", pause=0.12)
        if not recorder.until("› Author Markdown agent", 3):
            raise RuntimeError("TUI did not select Author Markdown agent")
        start = recorder.mark()
        recorder.press(b"\r", pause=0.3)
        if not recorder.until("WORLDS", 5, start):
            raise RuntimeError("TUI did not list Worlds for authoring")
        start = recorder.mark()
        recorder.press(b"\r", pause=0.3)
        if not recorder.until("AUTHOR MARKDOWN AGENT", 5, start):
            raise RuntimeError("TUI did not open the authoring path prompt")
        start = recorder.mark()
        recorder.press(b"\r", pause=0.5)  # accept default path; $EDITOR=true returns instantly
        if not recorder.until("REGISTER /", 10, start):
            raise RuntimeError("TUI did not return to the register screen")
        start = recorder.mark()
        recorder.press(b"\r", pause=0.5)
        if not recorder.until("Registered ", 10, start):
            raise RuntimeError("TUI did not confirm registration")
        time.sleep(1.0)
        start = recorder.mark()
        recorder.press(b"t", pause=0.3)
        if not recorder.until("LINEAGE /", 5, start):
            raise RuntimeError("TUI did not open parent selection for the paired Test")
        start = recorder.mark()
        recorder.press(b"\r", pause=0.3)
        if not recorder.until("ARENA /", 10, start):
            raise RuntimeError("TUI did not start the paired Arena evaluation")
        if not recorder.until("Visible score", 30, start):
            raise RuntimeError("TUI did not report a live visible score")
        time.sleep(1.5)

        recorder.press(b"\x1b", pause=0.3)
        recorder.press(b"q", pause=0.3)
        deadline = time.monotonic() + 5
        while recorder.child.poll() is None and time.monotonic() < deadline:
            recorder.pump(0.1)

        run_shown(f"{cli} daemon stop")
        recorder.press(b"exit\r")
        time.sleep(0.3)
        recorder.pump(0.2)

        recorder.write_cast(out_path)
        print(f"wrote {out_path} ({len(recorder.events)} output events, "
              f"{recorder.events[-1][0]:.1f}s recorded)")
        return 0
    except Exception as error:
        print(f"demo recording failed: {error}", file=sys.stderr)
        print(recorder.visible()[-3000:], file=sys.stderr)
        return 1
    finally:
        recorder.close()
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
