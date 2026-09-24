"""Contracts for the relocatable macOS package and user-local installer."""

from __future__ import annotations

import importlib.util
import os
import platform
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("package_macos", ROOT / "scripts/package_macos.py")
assert SPEC is not None and SPEC.loader is not None
PACKAGE_BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE_BUILDER)


def write_package(directory: Path, architecture: str = "arm64", version: str = "0.1.0") -> Path:
    root = directory / f"hephaestus-v{version}-macos-{architecture}"
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
    (share / "package.json").write_text(f'{{"version":"{version}"}}\n', encoding="utf-8")
    (share / "architecture").write_text(architecture + "\n", encoding="utf-8")
    (share / "tui").mkdir()
    (share / "tui/main.mjs").write_text("process.exit(0)\n", encoding="utf-8")
    installer = root / "install.sh"
    shutil.copy2(ROOT / "scripts/package/install.sh", installer)
    installer.chmod(0o755)
    return root


class MacosPackageTests(unittest.TestCase):
    @staticmethod
    def add_platform_tools(fake_bin: Path) -> None:
        fake_uname = fake_bin / "uname"
        fake_uname.write_text(
            "#!/bin/sh\ncase \"$1\" in -s) echo Darwin ;; *) echo arm64 ;; esac\n",
            encoding="utf-8",
        )
        fake_uname.chmod(0o755)
        # CI runs this contract on Linux. Translate BSD mv's -h no-follow flag
        # to GNU mv's -T so the test exercises replacement instead of traversal.
        move_args = '-fh "$@"' if platform.system() == "Darwin" else '-fT "$@"'
        fake_mv = fake_bin / "mv"
        fake_mv.write_text(
            f'#!/bin/sh\nif [ "$1" = "-fh" ]; then shift; exec /bin/mv {move_args}; fi\nexec /bin/mv "$@"\n',
            encoding="utf-8",
        )
        fake_mv.chmod(0o755)

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
            self.add_platform_tools(fake_bin)
            home = base / "home"
            home.mkdir()
            prefix = home / ".local"
            release_root = prefix / "share/hephaestus"
            attacker_directory = release_root / "attacker"
            attacker_directory.mkdir(parents=True)
            (attacker_directory / "owner.txt").write_text("preserve me", encoding="utf-8")
            (release_root / "current").symlink_to("attacker")
            env = os.environ.copy()
            env.update({"HOME": str(home), "PATH": f"{fake_bin}:/usr/bin:/bin"})
            staging_record = base / "staging-path"
            installer_wrapper = base / "run-installer.sh"
            installer_wrapper.write_text(
                "#!/bin/sh\nset -eu\n"
                f"release=$(basename {shlex.quote(str(package_root))})\n"
                f"staging={shlex.quote(str(release_root / 'releases'))}/.install-$release-$$\n"
                "mkdir -p \"$staging\"\n"
                "printf '%s' 'preserve me' > \"$staging/owner.txt\"\n"
                f"printf '%s' \"$staging\" > {shlex.quote(str(staging_record))}\n"
                f"exec {shlex.quote(str(package_root / 'install.sh'))} --prefix {shlex.quote(str(prefix))}\n",
                encoding="utf-8",
            )
            installer_wrapper.chmod(0o755)
            subprocess.run([str(installer_wrapper)], env=env, check=True, capture_output=True, text=True, timeout=20)
            current = release_root / "current"
            self.assertEqual(os.readlink(current), "releases/hephaestus-v0.1.0-macos-arm64")
            self.assertEqual((attacker_directory / "owner.txt").read_text(encoding="utf-8"), "preserve me")
            stale_staging = Path(staging_record.read_text(encoding="utf-8"))
            self.assertEqual((stale_staging / "owner.txt").read_text(encoding="utf-8"), "preserve me")

            next_package = write_package(base / "archive", version="0.1.1")
            subprocess.run([str(next_package / "install.sh"), "--prefix", str(prefix)], env=env, check=True, capture_output=True, text=True, timeout=20)
            self.assertEqual(os.readlink(current), "releases/hephaestus-v0.1.1-macos-arm64")
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
            self.add_platform_tools(fake_bin)
            home = base / "home"
            home.mkdir()
            prefix = home / ".local"
            env = os.environ.copy()
            env.update({"HOME": str(home), "PATH": f"{fake_bin}:/usr/bin:/bin"})
            result = subprocess.run([str(package_root / "install.sh"), "--prefix", str(prefix)], env=env, capture_output=True, text=True, timeout=20)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match host", result.stderr)
            self.assertFalse(prefix.exists())

    def test_installer_refuses_to_replace_a_broken_release_symlink(self) -> None:
        with tempfile.TemporaryDirectory(prefix="hephaestus-install-link-") as temporary:
            base = Path(temporary)
            package_root = write_package(base / "archive")
            fake_bin = base / "fake-bin"
            fake_bin.mkdir()
            self.add_platform_tools(fake_bin)
            home = base / "home"
            home.mkdir()
            prefix = home / ".local"
            release = prefix / "share/hephaestus/releases/hephaestus-v0.1.0-macos-arm64"
            release.parent.mkdir(parents=True)
            release.symlink_to("missing-release")
            env = os.environ.copy()
            env.update({"HOME": str(home), "PATH": f"{fake_bin}:/usr/bin:/bin"})

            result = subprocess.run(
                [str(package_root / "install.sh"), "--prefix", str(prefix)],
                env=env,
                capture_output=True,
                text=True,
                timeout=20,
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(release.is_symlink())
            self.assertEqual(os.readlink(release), "missing-release")


if __name__ == "__main__":
    unittest.main()
