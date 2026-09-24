"""Contracts for the relocatable macOS package and user-local installer."""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("package_macos", ROOT / "scripts/package_macos.py")
assert SPEC is not None and SPEC.loader is not None
PACKAGE_BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE_BUILDER)


def write_package(directory: Path, architecture: str = "arm64") -> Path:
    root = directory / f"hephaestus-v0.1.0-macos-{architecture}"
    package = root / "hephaestus"
    bin_dir = package / "bin"
    share = package / "share/hephaestus"
    bin_dir.mkdir(parents=True)
    share.mkdir(parents=True)
    for name in PACKAGE_BUILDER.BINARIES:
        binary = bin_dir / name
        binary.write_text("#!/bin/sh\nprintf '%s\\n' '" + name + "'\n", encoding="utf-8")
        binary.chmod(0o755)
    (bin_dir / "node").write_text("#!/bin/sh\necho v24.21.0\n", encoding="utf-8")
    (bin_dir / "node").chmod(0o755)
    (share / "package.json").write_text('{"version":"0.1.0"}\n', encoding="utf-8")
    (share / "architecture").write_text(architecture + "\n", encoding="utf-8")
    (share / "tui").mkdir()
    (share / "tui/main.mjs").write_text("process.exit(0)\n", encoding="utf-8")
    installer = root / "install.sh"
    shutil.copy2(ROOT / "scripts/package/install.sh", installer)
    installer.chmod(0o755)
    return root


class MacosPackageTests(unittest.TestCase):
    def test_node_runtime_is_pinned_for_both_macos_architectures(self) -> None:
        self.assertEqual(set(PACKAGE_BUILDER.NODE_SHA256), {"arm64", "x86_64"})
        self.assertEqual(set(PACKAGE_BUILDER.NODE_SHA256.values()), {
            "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057",
            "1462cb3b3046b815cf8ea436d3da450ec1a9f11dac7e5a46b0ada5305d7e8097",
        })
        self.assertEqual(PACKAGE_BUILDER.NODE_VERSION, "24.21.0")

    def test_version_comes_from_workspace_metadata(self) -> None:
        self.assertEqual(PACKAGE_BUILDER.workspace_version(ROOT), "0.1.0")

    def test_installer_survives_relocation_and_keeps_binaries_adjacent(self) -> None:
        with tempfile.TemporaryDirectory(prefix="hephaestus-install-contract-") as temporary:
            base = Path(temporary)
            package_root = write_package(base / "archive")
            fake_bin = base / "fake-bin"
            fake_bin.mkdir()
            fake_uname = fake_bin / "uname"
            fake_uname.write_text("#!/bin/sh\ncase \"$1\" in -s) echo Darwin ;; *) echo arm64 ;; esac\n", encoding="utf-8")
            fake_uname.chmod(0o755)
            home = base / "home"
            home.mkdir()
            prefix = home / ".local"
            env = os.environ.copy()
            env.update({"HOME": str(home), "PATH": f"{fake_bin}:/usr/bin:/bin"})
            subprocess.run([str(package_root / "install.sh"), "--prefix", str(prefix)], env=env, check=True, capture_output=True, text=True, timeout=20)
            moved = base / "moved-local"
            prefix.rename(moved)
            for name in PACKAGE_BUILDER.BINARIES:
                installed = moved / "bin" / name
                self.assertTrue(installed.is_symlink())
                self.assertEqual(installed.read_text(encoding="utf-8").splitlines()[-1], f"printf '%s\\n' '{name}'")
                self.assertTrue(os.access(installed, os.X_OK))
            node = moved / "share/hephaestus/current/bin/node"
            self.assertEqual(subprocess.check_output([str(node), "--version"], text=True).strip(), "v24.21.0")
            tui = moved / "share/hephaestus/current/share/hephaestus/tui/main.mjs"
            self.assertTrue(tui.is_file())

    def test_installer_rejects_a_mismatched_architecture_without_installing(self) -> None:
        with tempfile.TemporaryDirectory(prefix="hephaestus-install-arch-") as temporary:
            base = Path(temporary)
            package_root = write_package(base / "archive", architecture="x86_64")
            fake_bin = base / "fake-bin"
            fake_bin.mkdir()
            fake_uname = fake_bin / "uname"
            fake_uname.write_text("#!/bin/sh\ncase \"$1\" in -s) echo Darwin ;; *) echo arm64 ;; esac\n", encoding="utf-8")
            fake_uname.chmod(0o755)
            home = base / "home"
            home.mkdir()
            prefix = home / ".local"
            env = os.environ.copy()
            env.update({"HOME": str(home), "PATH": f"{fake_bin}:/usr/bin:/bin"})
            result = subprocess.run([str(package_root / "install.sh"), "--prefix", str(prefix)], env=env, capture_output=True, text=True, timeout=20)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match host", result.stderr)
            self.assertFalse(prefix.exists())


if __name__ == "__main__":
    unittest.main()
