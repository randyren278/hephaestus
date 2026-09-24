#!/usr/bin/env python3
"""Open the installed TUI on a PTY and close it with q."""

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


def main() -> int:
    binary, data_dir, home, path = sys.argv[1:5]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    terminal_before = termios.tcgetattr(slave)
    env = os.environ.copy()
    env.update({"HOME": home, "HEPHAESTUS_HOME": data_dir, "PATH": path, "COLUMNS": "80", "LINES": "24"})
    child = subprocess.Popen(
        [binary, "--data-dir", data_dir, "tui"],
        cwd=home,
        env=env,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        close_fds=True,
    )
    output = bytearray()

    def visible() -> bytes:
        return ANSI.sub(b"", bytes(output))

    try:
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline and b"Kill all active work" not in visible():
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    output.extend(os.read(master, 8192))
                except OSError:
                    break
            if child.poll() is not None:
                break
        if b"Kill all active work" not in visible():
            raise RuntimeError("installed TUI did not render with its bundled Node runtime")
        os.write(master, b"q")
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline and child.poll() is None:
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    output.extend(os.read(master, 8192))
                except OSError:
                    break
        if child.poll() is None:
            raise RuntimeError("installed TUI did not exit after q")
        if child.returncode != 0:
            raise RuntimeError(f"installed TUI exited with status {child.returncode}")
        if termios.tcgetattr(slave) != terminal_before:
            raise RuntimeError("installed TUI did not restore terminal settings")
        print("packaged TUI rendered under the isolated PATH and exited cleanly on q")
        return 0
    except Exception as error:
        print(f"packaged TUI acceptance failed: {error}", file=sys.stderr)
        print(visible()[-1200:].decode("utf-8", errors="replace"), file=sys.stderr)
        if child.poll() is None:
            child.kill()
        child.wait()
        return 1
    finally:
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    raise SystemExit(main())
