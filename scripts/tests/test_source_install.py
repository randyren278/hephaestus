"""Contracts for scripts/install.sh's Senate-only and full modes.

Runs the real installer against fake `cargo` and `npm` executables, so it
builds nothing and needs no network.
"""
from __future__ import annotations

import os
import pty
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "scripts/install.sh"
FULL = {
    "heph", "hephaestus", "hephaestusd", "hephaestus-reference-worker",
    "hephaestus-reference-evaluator", "hephaestus-process-guardian", "senate",
}

FAKE_CARGO = """#!/bin/sh
echo "$@" >> "$FAKE_LOG/cargo.log"
mkdir -p "$CARGO_TARGET_DIR/release"
previous=""
for argument in "$@"; do
    if [ "$previous" = --bin ]; then
        printf '#!/bin/sh\\n' > "$CARGO_TARGET_DIR/release/$argument"
        chmod +x "$CARGO_TARGET_DIR/release/$argument"
    fi
    previous="$argument"
done
"""

FAKE_NPM = """#!/bin/sh
echo "$@" >> "$FAKE_LOG/npm.log"
"""


class SourceInstallTests(unittest.TestCase):
    def setUp(self) -> None:
        self.work = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.work, ignore_errors=True)
        self.bin = self.work / "fake-bin"
        self.bin.mkdir()
        for name, body in (("cargo", FAKE_CARGO), ("npm", FAKE_NPM)):
            path = self.bin / name
            path.write_text(body)
            path.chmod(0o755)
        self.prefix = self.work / "prefix"
        self.env = {
            "HOME": str(self.work),
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "CARGO_TARGET_DIR": str(self.work / "target"),
            "FAKE_LOG": str(self.work),
        }

    def install(self, *flags: str, stdin: int | None = subprocess.DEVNULL) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["sh", str(INSTALLER), "--prefix", str(self.prefix), *flags],
            env=self.env, stdin=stdin, capture_output=True, text=True, timeout=30,
        )

    def installed(self) -> set[str]:
        return {path.name for path in (self.prefix / "bin").iterdir()}

    def log(self, name: str) -> str:
        path = self.work / name
        return path.read_text() if path.exists() else ""

    def test_senate_only_installs_just_the_senate_without_npm(self) -> None:
        (self.bin / "npm").unlink()
        result = self.install("--senate-only")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed(), {"senate"})
        self.assertIn("--bin senate", self.log("cargo.log"))
        self.assertNotIn("hephaestusd", self.log("cargo.log"))
        self.assertIn('Run: senate ask "your question" --size M', result.stderr)

    def test_full_installs_everything_including_the_senate(self) -> None:
        result = self.install("--full")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed(), FULL)
        self.assertIn("ci", self.log("npm.log"))
        self.assertIn("Run: heph", result.stderr)

    def test_non_interactive_default_is_the_full_install(self) -> None:
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("Just here for the Senate?", result.stderr)
        self.assertEqual(self.installed(), FULL)

    def test_conflicting_modes_are_rejected(self) -> None:
        result = self.install("--senate-only", "--full")
        self.assertEqual(result.returncode, 2)
        self.assertIn("choose one of --senate-only and --full", result.stderr)

    def test_interactive_run_asks_and_yes_means_senate_only(self) -> None:
        for answer, expected in (("y\n", {"senate"}), ("\n", FULL)):
            with self.subTest(answer=answer):
                if self.prefix.exists():
                    for link in (self.prefix / "bin").iterdir():
                        link.unlink()
                controller, terminal = pty.openpty()
                try:
                    os.write(controller, answer.encode())
                    result = self.install(stdin=terminal)
                finally:
                    os.close(controller)
                    os.close(terminal)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("Just here for the Senate?", result.stderr)
                self.assertEqual(self.installed(), expected)


if __name__ == "__main__":
    unittest.main()
