#!/usr/bin/env python3
"""Verify packaged TUI editor ownership, failure recovery and literal arguments.

Arguments: <app-dir> <data-dir> <world-name>. Requires a running scratch daemon.
All authored files and fake editors stay under a private temporary HOME.
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
import tempfile
import termios
import time

ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")


def check(app_dir, data_dir, world_name, home, succeeds):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    pristine = termios.tcgetattr(slave)
    editor = home / "fake editor.sh"
    editor.write_text(
        '#!/bin/sh\n[ "$1" = --wait ] || exit 41\n'
        '[ "$2" = "two words" ] || exit 42\n'
        '[ "$3" = "$EXPECTED_AGENT_PATH" ] && [ -f "$3" ] || exit 43\n'
        'stty -echo\nprintf "EDITOR_START\\n"\nIFS= read -r line\n'
        'printf \'%s\\n\' "$line" > "$EDITOR_INPUT_REPORT"\n'
        'sleep 2\nprintf "EDITOR_END\\n"\nstty echo\n'
        + ("exit 0\n" if succeeds else "exit 23\n")
    )
    env = os.environ.copy()
    env.update({"HEPHAESTUS_HOME": data_dir, "HOME": str(home), "CI": "false", "COLUMNS": "80", "LINES": "24"})
    env.pop("VISUAL", None)
    env["EDITOR"] = f"sh '{editor}' --wait 'two words'"
    slug = re.sub(r"[^a-z0-9]+", "-", world_name.lower()).strip("-") or "agent"
    env["EXPECTED_AGENT_PATH"] = str(home / ".hephaestus/agents" / f"{slug}.md")
    env["EDITOR_INPUT_REPORT"] = str(home / "editor-input.txt")
    child = subprocess.Popen(
        ["node", str(Path(app_dir) / "dist/main.mjs")], cwd=home, env=env,
        stdin=slave, stdout=slave, stderr=slave, close_fds=True,
    )
    output = bytearray()

    def text():
        return ANSI.sub(b"", bytes(output)).decode("utf-8", errors="replace")

    def pump(seconds=.05):
        ready, _, _ = select.select([master], [], [], seconds)
        if ready:
            try:
                output.extend(os.read(master, 65536))
            except OSError:
                pass

    def until(needle, after=0, timeout=10):
        deadline = time.monotonic() + timeout
        while needle not in text()[after:]:
            assert child.poll() is None, "TUI exited unexpectedly"
            assert time.monotonic() < deadline, f"TUI did not show {needle!r}"
            pump()

    def press(keys):
        os.write(master, keys)
        deadline = time.monotonic() + .25
        while time.monotonic() < deadline:
            pump()

    try:
        until("Author Markdown agent")
        for _ in range(20):
            start = len(text())
            press(b"\x1b[B")
            if "› Author Markdown agent" in text()[start:]:
                break
        else:
            raise AssertionError("Author action was not selectable")
        start = len(text())
        press(b"\r")
        until("WORLDS", start)
        until(world_name, start)
        start = len(text())
        press(b"\r")
        until("AUTHOR MARKDOWN AGENT / " + world_name, start)
        raw = termios.tcgetattr(slave)
        start = len(text())
        # Two simultaneous Enter presses must still launch exactly one editor.
        press(b"\r\r")
        until("EDITOR_START", start)
        input_line = "q should reach only the editor: café --wait"
        press(input_line.encode() + b"\r")
        until("EDITOR_END", start)
        assert (home / "editor-input.txt").read_text() == input_line + "\n", "Editor lost or changed keyboard input"
        until("REGISTER / " + world_name if succeeds else "Editor failed (status 23)", start)
        assert termios.tcgetattr(slave) == raw, "Ink raw mode was not restored"
        segment = bytes(output)
        begin = segment.index(b"EDITOR_START\r\n") + len(b"EDITOR_START\r\n")
        end = segment.index(b"EDITOR_END\r\n", begin)
        assert segment[begin:end] == b"", "Ink wrote into the editor's terminal"
        assert segment.count(b"EDITOR_START\r\n") == 1, "Editor launched twice"
        if not succeeds:
            assert "REGISTER / " + world_name not in text()[start:], "Failed editor advanced to registration"
            assert "Source kept; Enter retries" in text()[start:]
        sources = list((home / ".hephaestus/agents").glob("*.md"))
        assert len(sources) == 1 and sources[0].is_file(), "Authored source was not preserved"
        press(b"\x1b")
        if not succeeds:
            press(b"\x1b")
        press(b"q")
        child.wait(timeout=10)
        assert child.returncode == 0, "TUI exit failed"
        assert termios.tcgetattr(slave) == pristine, "Terminal settings were not restored on exit"
        return {"editor_succeeded": succeeds, "exact_arguments": True, "editor_received_exact_keyboard_input": True, "quiet_editor_interval_seconds": 2,
                "single_launch": True, "raw_mode_restored": True, "source_preserved": True,
                "registration_step_entered": succeeds, "terminal_restored_on_exit": True}
    except Exception:
        print(text()[-3000:], file=sys.stderr)
        raise
    finally:
        if child.poll() is None:
            child.kill()
        child.wait()
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    app, data, world = sys.argv[1:4]
    with tempfile.TemporaryDirectory(prefix="heph-editor-") as scratch:
        root = Path(scratch)
        results = []
        for succeeds in (True, False):
            home = root / ("success" if succeeds else "failure")
            home.mkdir(mode=0o700)
            results.append(check(str(Path(app).resolve()), str(Path(data).resolve()), world, home, succeeds))
        print(json.dumps({"packaged_tui_80x24": results}, indent=2))
