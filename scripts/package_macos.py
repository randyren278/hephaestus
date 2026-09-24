#!/usr/bin/env python3
"""Build a relocatable, pinned-runtime macOS distribution."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request


NODE_VERSION = "24.21.0"
NODE_SHA256 = {
    "arm64": "bed7eea5325e1108f32ce5228ddd6a5f0f08a499ee42aa7442aea583702f6057",
    "x86_64": "1462cb3b3046b815cf8ea436d3da450ec1a9f11dac7e5a46b0ada5305d7e8097",
}
RUST_TARGET = {"arm64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"}
NODE_ARCH = {"arm64": "arm64", "x86_64": "x64"}
BINARIES = (
    "hephaestus",
    "hephaestusd",
    "hephaestus-reference-worker",
    "hephaestus-reference-evaluator",
    "hephaestus-process-guardian",
)


def run(arguments: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> None:
    print("+", " ".join(arguments), flush=True)
    subprocess.run(arguments, cwd=cwd, env=env, check=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def workspace_version(root: Path) -> str:
    text = (root / "Cargo.toml").read_text(encoding="utf-8")
    workspace = text.split("[workspace.package]", 1)[1].split("[", 1)[0]
    match = re.search(r'^version\s*=\s*"([^"\n]+)"', workspace, re.MULTILINE)
    if not match:
        raise RuntimeError("workspace.package.version is missing")
    return match.group(1)


def node_archive(cache: Path, arch: str) -> Path:
    filename = f"node-v{NODE_VERSION}-darwin-{NODE_ARCH[arch]}.tar.gz"
    archive = cache / filename
    cache.mkdir(parents=True, exist_ok=True)
    if not archive.exists() or sha256(archive) != NODE_SHA256[arch]:
        url = f"https://nodejs.org/dist/v{NODE_VERSION}/{filename}"
        temporary = archive.with_suffix(archive.suffix + ".partial")
        print(f"Downloading pinned Node.js {NODE_VERSION} ({arch}) from {url}", flush=True)
        with urllib.request.urlopen(url, timeout=60) as response, temporary.open("wb") as output:
            shutil.copyfileobj(response, output)
        if sha256(temporary) != NODE_SHA256[arch]:
            temporary.unlink(missing_ok=True)
            raise RuntimeError(f"Node.js archive checksum mismatch for {filename}")
        temporary.replace(archive)
    return archive


def extract_node(archive: Path, destination: Path, license_destination: Path, arch: str) -> None:
    expected_root = f"node-v{NODE_VERSION}-darwin-{NODE_ARCH[arch]}"
    with tarfile.open(archive, "r:gz") as source:
        for member_name, output in (
            (f"{expected_root}/bin/node", destination / "node"),
            (f"{expected_root}/LICENSE", license_destination),
        ):
            member = source.getmember(member_name)
            if not member.isfile():
                raise RuntimeError(f"unexpected Node.js archive entry: {member_name}")
            stream = source.extractfile(member)
            if stream is None:
                raise RuntimeError(f"Node.js archive entry is unreadable: {member_name}")
            output.parent.mkdir(parents=True, exist_ok=True)
            with stream, output.open("wb") as target:
                shutil.copyfileobj(stream, target)
            output.chmod(0o755 if output.name == "node" else 0o644)


def verify_architecture(path: Path, expected: str) -> None:
    output = subprocess.run(
        ["/usr/bin/lipo", "-archs", str(path)],
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout.split()
    if output != [expected]:
        raise RuntimeError(f"{path.name} has architectures {output}, expected only {expected}")


def collect_tui_notices(root: Path, destination: Path) -> None:
    lockfile = json.loads((root / "apps/hephaestus-tui/package-lock.json").read_text())
    modules = root / "apps/hephaestus-tui/node_modules"
    rows: list[str] = ["Bundled Hephaestus operator TUI dependency notices", ""]
    packages: list[tuple[str, str, str, Path]] = []
    for relative, metadata in lockfile["packages"].items():
        if not relative.startswith("node_modules/"):
            continue
        package_dir = modules / relative.removeprefix("node_modules/")
        package_json = package_dir / "package.json"
        if not package_json.is_file():
            if metadata.get("optional"):
                continue
            raise RuntimeError(f"locked TUI dependency is missing: {relative}")
        manifest = json.loads(package_json.read_text(encoding="utf-8"))
        declared = manifest.get("license", metadata.get("license", "unspecified"))
        packages.append((manifest.get("name", relative), manifest.get("version", "unknown"), str(declared), package_dir))
    for name, version, license_name, package_dir in sorted(packages):
        rows.append(f"{name}@{version} — license: {license_name}")
        license_files = sorted(
            file
            for file in package_dir.iterdir()
            if file.is_file()
            and ("license" in file.name.lower() or "notice" in file.name.lower() or file.name.lower() == "copying")
        )
        for license_file in license_files:
            rows.extend([f"--- {name}/{license_file.name} ---", license_file.read_text(encoding="utf-8", errors="replace"), ""])
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text("\n".join(rows), encoding="utf-8")


def copy_tree(source: Path, destination: Path) -> None:
    shutil.copytree(source, destination, symlinks=False)


def build(args: argparse.Namespace) -> Path:
    root = Path(__file__).resolve().parents[1]
    if platform.system() != "Darwin":
        raise RuntimeError("macOS package creation must run on macOS")
    host_arch = platform.machine()
    default_arch = "arm64" if host_arch in {"arm64", "aarch64"} else "x86_64"
    arch = args.arch or default_arch
    if arch not in NODE_SHA256:
        raise RuntimeError(f"unsupported macOS architecture: {arch}")
    rust_target = RUST_TARGET[arch]
    binary_dir = Path(args.binary_dir).resolve() if args.binary_dir else None
    target_dir = Path(args.target_dir).resolve() if args.target_dir else root / "target/package-build"

    app = root / "apps/hephaestus-tui"
    run(["npm", "ci", "--no-audit", "--no-fund"], cwd=app)
    run(["npm", "run", "build:package"], cwd=app)

    if not args.skip_cargo_build:
        target_dir.mkdir(parents=True, exist_ok=True)
        build_env = os.environ.copy()
        build_env["CARGO_TARGET_DIR"] = str(target_dir)
        run(
            ["cargo", "build", "--locked", "--release", "--workspace", "--target", rust_target],
            cwd=root,
            env=build_env,
        )
        binary_dir = target_dir / rust_target / "release"
    if binary_dir is None:
        raise RuntimeError("--binary-dir is required with --skip-cargo-build")
    for name in BINARIES:
        path = binary_dir / name
        if not path.is_file():
            raise RuntimeError(f"required Rust executable is missing: {path}")
        verify_architecture(path, "arm64" if arch == "arm64" else "x86_64")

    version = workspace_version(root)
    package_name = f"hephaestus-v{version}-macos-{arch}"
    output_dir = Path(args.output_dir).resolve() if args.output_dir else root / "target/packages"
    output_dir.mkdir(parents=True, exist_ok=True)
    archive_path = output_dir / f"{package_name}.tar.gz"
    node_cache = Path(args.node_cache).resolve() if args.node_cache else root / "target/node-runtime-cache"
    node = node_archive(node_cache, arch)

    with tempfile.TemporaryDirectory(prefix="hephaestus-package-") as temporary:
        stage_root = Path(temporary) / package_name
        package = stage_root / "hephaestus"
        (package / "bin").mkdir(parents=True)
        share = package / "share/hephaestus"
        (share / "tui").mkdir(parents=True)
        for name in BINARIES:
            shutil.copy2(binary_dir / name, package / "bin" / name)
            (package / "bin" / name).chmod(0o755)
        extract_node(node, package / "bin", share / "licenses/node/LICENSE", arch)
        verify_architecture(package / "bin/node", "arm64" if arch == "arm64" else "x86_64")
        bundle = app / "dist/main.mjs"
        bundled_code = app / "dist/main.bundle.mjs"
        if not bundle.is_file():
            raise RuntimeError("TUI bundle was not produced")
        shutil.copy2(bundle, share / "tui/main.mjs")
        if not bundled_code.is_file():
            raise RuntimeError("TUI JavaScript payload was not produced")
        shutil.copy2(bundled_code, share / "tui/main.bundle.mjs")
        copy_tree(root / "examples/quickstart", share / "fixtures/quickstart")
        collect_tui_notices(root, share / "licenses/THIRD_PARTY_NOTICES.txt")
        metadata = {
            "version": version,
            "platform": "macos",
            "architecture": arch,
            "rust_target": rust_target,
            "node_version": NODE_VERSION,
            "node_archive_sha256": NODE_SHA256[arch],
        }
        (share / "package.json").write_text(json.dumps(metadata, indent=2) + "\n")
        (share / "architecture").write_text(arch + "\n")
        shutil.copy2(root / "scripts/package/install.sh", stage_root / "install.sh")
        (stage_root / "install.sh").chmod(0o755)
        os.chmod(package, 0o755)
        with tarfile.open(archive_path, "w:gz", compresslevel=9) as archive:
            archive.add(stage_root, arcname=package_name, recursive=True)
    print(f"Created {archive_path} ({archive_path.stat().st_size:,} bytes)")
    return archive_path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arch", choices=sorted(NODE_SHA256), help="target macOS architecture (default: host)")
    parser.add_argument("--output-dir", help="directory for the package archive")
    parser.add_argument("--target-dir", help="separate Cargo target directory")
    parser.add_argument("--node-cache", help="directory for verified Node.js archives")
    parser.add_argument("--binary-dir", help="release binaries to package instead of building them")
    parser.add_argument("--skip-cargo-build", action="store_true", help="require --binary-dir and package existing binaries")
    args = parser.parse_args()
    try:
        build(args)
    except (OSError, RuntimeError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"package-macos: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
