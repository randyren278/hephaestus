#!/usr/bin/env python3
"""Drive the real Ink UI through a PTY against a live Hephaestus daemon."""

import os
import pty
import fcntl
import re
import select
import struct
import tempfile
import shutil
import subprocess
import sys
import termios
import time

# A cold `tsx` start on a fresh CI runner can take well over ten seconds.
LAUNCH_TIMEOUT_SECONDS = 45


def main() -> int:
    app_dir, data_dir, job_id = sys.argv[1:4]
    launcher = sys.argv[4:]
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
    terminal_before = termios.tcgetattr(slave)
    env = os.environ.copy()
    env["COLUMNS"] = "80"
    env["LINES"] = "24"
    if launcher:
        binary, caller_cwd, relative_data_dir, fallback_dir = launcher
        env["HEPHAESTUS_HOME"] = fallback_dir
        command = [binary, "--data-dir", relative_data_dir, "tui"]
        cwd = caller_cwd
    else:
        env["HEPHAESTUS_HOME"] = data_dir
        command = ["npm", "run", "start"]
        cwd = app_dir
    child = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        close_fds=True,
    )
    output = bytearray()
    stage = "launch"

    def visible_output() -> bytes:
        return re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(output))

    def until(needle: bytes, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in visible_output():
                return True
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    output.extend(os.read(master, 8192))
                except OSError:
                    break
        return needle in visible_output()

    def wait_while_draining(process: subprocess.Popen[bytes], stream_fd: int, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if process.poll() is not None:
                return True
            ready, _, _ = select.select([stream_fd], [], [], 0.1)
            if ready:
                try:
                    output.extend(os.read(stream_fd, 8192))
                except OSError:
                    break
        return process.poll() is not None

    try:
        if not until(b"Cancel job by ID", LAUNCH_TIMEOUT_SECONDS):
            raise RuntimeError("TUI did not render the cancel action")
        if launcher and not until(b"Active runs  1", 3):
            raise RuntimeError("TUI launcher did not read status from the selected relative data directory")
        stage = "navigate"
        for _ in range(5):
            os.write(master, b"\x1b[B")
            time.sleep(0.12)
        if not until(b"\xe2\x80\xba Cancel job by ID", 2):
            raise RuntimeError("TUI did not select the cancel action")
        stage = "open-job-prompt"
        os.write(master, b"\r")
        if not until(b"Job ID:", 3):
            raise RuntimeError("TUI did not open the job ID prompt")
        stage = "enter-job-id"
        for char in job_id:
            os.write(master, char.encode("ascii"))
            time.sleep(0.03)
        os.write(master, b"\r")
        if not until(b"Cancel job " + job_id.encode("ascii"), 3):
            raise RuntimeError("TUI did not show the explicit cancellation confirmation")
        stage = "confirm-cancel"
        os.write(master, b"y")
        confirmation = b"Daemon confirmed " + job_id.encode("ascii") + b" terminal: interrupted"
        if not until(confirmation, 12):
            raise RuntimeError("TUI did not display daemon-confirmed job termination")
        os.write(master, b"q")
        if not wait_while_draining(child, master, 5):
            raise RuntimeError("TUI did not exit after q while its PTY output was drained")
        if termios.tcgetattr(slave) != terminal_before:
            raise RuntimeError("TUI did not restore terminal settings on exit")
        disconnected_dir = tempfile.mkdtemp(prefix="hephaestus-tui-offline-")
        os.chmod(disconnected_dir, 0o700)
        with open(os.path.join(disconnected_dir, "operator.token"), "w", encoding="ascii") as token_file:
            token_file.write("a" * 64)
        os.chmod(os.path.join(disconnected_dir, "operator.token"), 0o600)
        offline_master, offline_slave = pty.openpty()
        fcntl.ioctl(offline_slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        offline_before = termios.tcgetattr(offline_slave)
        offline_env = env.copy()
        offline_env["HEPHAESTUS_HOME"] = disconnected_dir
        offline_child = subprocess.Popen(
            ["npm", "run", "start"], cwd=app_dir, env=offline_env,
            stdin=offline_slave, stdout=offline_slave, stderr=offline_slave, close_fds=True,
        )
        offline_output = bytearray()
        try:
            deadline = time.monotonic() + LAUNCH_TIMEOUT_SECONDS
            while time.monotonic() < deadline and b"STALE" not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", offline_output):
                ready, _, _ = select.select([offline_master], [], [], 0.1)
                if ready:
                    try:
                        offline_output.extend(os.read(offline_master, 8192))
                    except OSError:
                        break
            if b"STALE" not in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", offline_output):
                raise RuntimeError("TUI did not mark an unavailable daemon as stale")
            os.write(offline_master, b"q")
            offline_deadline = time.monotonic() + 4
            while time.monotonic() < offline_deadline and offline_child.poll() is None:
                ready, _, _ = select.select([offline_master], [], [], 0.1)
                if ready:
                    try:
                        offline_output.extend(os.read(offline_master, 8192))
                    except OSError:
                        break
            if offline_child.poll() is None:
                raise RuntimeError("disconnected TUI did not exit after q while PTY output was drained")
            if termios.tcgetattr(offline_slave) != offline_before:
                raise RuntimeError("TUI did not restore terminal settings after daemon disconnect")
        finally:
            if offline_child.poll() is None:
                offline_child.kill()
                offline_child.wait()
            os.close(offline_master)
            os.close(offline_slave)
            shutil.rmtree(disconnected_dir)

        kill_master, kill_slave = pty.openpty()
        fcntl.ioctl(kill_slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        kill_before = termios.tcgetattr(kill_slave)
        kill_child = subprocess.Popen(
            command, cwd=cwd, env=env, stdin=kill_slave, stdout=kill_slave, stderr=kill_slave, close_fds=True,
        )
        kill_output = bytearray()

        def kill_until(needle: bytes, timeout: float) -> bool:
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if needle in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(kill_output)):
                    return True
                ready, _, _ = select.select([kill_master], [], [], 0.1)
                if ready:
                    try:
                        kill_output.extend(os.read(kill_master, 8192))
                    except OSError:
                        break
            return needle in re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(kill_output))

        try:
            if not kill_until(b"Kill all active work", LAUNCH_TIMEOUT_SECONDS):
                raise RuntimeError("fresh kill-all PTY did not render the action list")
            for _ in range(3):
                os.write(kill_master, b"\x1b[B")
                time.sleep(0.15)
            if not kill_until(b"\xe2\x80\xba Kill all active work", 2):
                raise RuntimeError("fresh kill-all PTY did not select the action")
            os.write(kill_master, b"\r")
            if not kill_until(b"Cancel ALL active work", 3):
                raise RuntimeError("fresh kill-all PTY did not show explicit confirmation")
            os.write(kill_master, b"y")
            if not kill_until(b"Kill-all recorded", 4):
                raise RuntimeError("fresh kill-all PTY did not report the request")
            clean_kill_output = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", bytes(kill_output))
            if b"terminal states unavailable" not in re.sub(rb"\s+", b" ", clean_kill_output):
                raise RuntimeError("kill-all pending wording missing from fresh PTY: " + re.sub(rb"\s+", b" ", clean_kill_output)[-900:].decode("utf-8", errors="replace"))
            os.write(kill_master, b"q")
            if not wait_while_draining(kill_child, kill_master, 5):
                raise RuntimeError("kill-all PTY did not exit after q while output was drained")
            if termios.tcgetattr(kill_slave) != kill_before:
                raise RuntimeError("kill-all PTY did not restore terminal settings")
        finally:
            if kill_child.poll() is None:
                kill_child.kill()
                kill_child.wait()
            os.close(kill_master)
            os.close(kill_slave)

        print("PTY 80x24: job cancellation was daemon-confirmed; separate session confirmed kill-all request wording without claiming terminal completion.")
        print("PTY screen evidence: " + confirmation.decode())
        print("Disconnect evidence: unavailable daemon is marked STALE; terminal settings restore on exit.")
        if launcher:
            print("Launcher evidence: relative data directory selected the intended daemon; terminal settings restored.")
        return 0
    except Exception as error:  # test helper reports concise failure evidence
        print(f"PTY smoke failed during {stage}: {error}", file=sys.stderr)
        print(visible_output()[-1200:].decode("utf-8", errors="replace"), file=sys.stderr)
        print(output.decode("utf-8", errors="replace")[-5000:], file=sys.stderr)
        if child.poll() is None:
            child.kill()
        child.wait()
        return 1
    finally:
        os.close(master)
        os.close(slave)


if __name__ == "__main__":
    raise SystemExit(main())
