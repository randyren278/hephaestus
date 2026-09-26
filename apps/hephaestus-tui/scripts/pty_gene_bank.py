#!/usr/bin/env python3
"""Drive the Ink Gene Bank screens through a PTY against a live daemon.

Arguments: <app-dir> <data-dir> <gene-id> <recipient-genome-id>

Navigates to Gene Bank, opens the list, and asserts it shows the real
extracted Gene and its recorded transfer counts, then inspects the Gene's
detail screen and asserts it shows the real recipient transfer and its
outcome, sourced from the live daemon rather than placeholders.
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
    app_dir, data_dir, gene_id, recipient_genome_id = sys.argv[1:5]
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

    short_gene = gene_id.split(":")[-1][:12]
    short_recipient = recipient_genome_id.split(":")[-1][:12]

    try:
        if not until("Gene Bank", LAUNCH_TIMEOUT_SECONDS):
            raise RuntimeError("TUI did not render the Gene Bank action")
        stage = "select-gene-bank"
        # Walk the menu until the action is selected instead of counting a
        # fixed number of rows, so adding a menu entry above it cannot break this.
        for _ in range(20):
            start = mark()
            press(b"\x1b[B")
            if until(SELECTED + "Gene Bank", 0.5, start):
                break
        else:
            raise RuntimeError("TUI did not select the Gene Bank action")
        stage = "open-gene-list"
        start = mark()
        press(b"\r")
        if not until("GENE BANK", 5, start) or not until(short_gene, 5, start):
            raise RuntimeError("TUI did not list the extracted Gene")
        list_frame = latest_frame("GENE BANK")
        if "lineages" not in list_frame:
            raise RuntimeError("TUI Gene Bank list did not show a lineage count")

        stage = "open-gene-detail"
        start = mark()
        press(b"\r")
        if not until("GENE / " + short_gene, 5, start) or not until("TRANSFERS", 5, start):
            raise RuntimeError("TUI did not open the Gene detail screen")
        if not until(short_recipient, 5, start):
            raise RuntimeError("TUI Gene detail did not show the real recorded transfer")
        detail_frame = latest_frame("GENE / " + short_gene)

        stage = "back-to-list"
        start = mark()
        press(b"\x1b")
        if not until("GENE BANK", 5, start):
            raise RuntimeError("Esc did not return to the Gene Bank list")

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

        print("PTY 80x24: Gene Bank list and detail both showed real daemon data.")
        print("List evidence: " + " | ".join(line.strip(" │") for line in list_frame.splitlines() if short_gene in line)[:300])
        print("Detail evidence: " + " | ".join(line.strip(" │") for line in detail_frame.splitlines() if short_recipient in line)[:300])
        return 0
    except Exception as error:  # report concise failure evidence
        print(f"PTY Gene Bank failed during {stage}: {error}", file=sys.stderr)
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
