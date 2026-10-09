"""Upload a complete release privately, then publish it with explicit identity."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import tempfile
from pathlib import Path

from scripts.package_macos import BINARIES, RUST_TARGET, workspace_version


def gh(*arguments: str):
    result = subprocess.run(["gh", *arguments], capture_output=True, text=True, check=False)
    if result.returncode:
        raise RuntimeError(f"GitHub command failed: {result.stderr[:4096].strip()}")
    return json.loads(result.stdout) if arguments[0] == "api" and result.stdout.strip() else None


def expected_assets(version: str) -> set[str]:
    names = {
        f"hephaestus-v{version}-macos-{arch}.tar.gz{suffix}"
        for arch in RUST_TARGET for suffix in ("", ".sha256")
    }
    names.update(f"{binary}_bin_{target}.cdx.json" for binary in BINARIES for target in RUST_TARGET.values())
    return names | {f"{name}.sigstore.json" for name in names}


def check_release(release: dict, tag: str, commit: str, names: set[str]) -> None:
    if release.get("tag_name") != tag or source_marker(commit) not in release.get("body", ""):
        raise RuntimeError("existing release belongs to a different tag or source commit")
    assets = release.get("assets", [])
    actual = [asset["name"] for asset in assets]
    if len(actual) != len(set(actual)) or not set(actual) <= names:
        raise RuntimeError("existing release contains unexpected or duplicate assets")


def complete_assets(release: dict, files: dict[str, Path]) -> None:
    remote = {asset["name"]: asset for asset in release["assets"]}
    if set(remote) != set(files):
        raise RuntimeError("release upload is incomplete; draft remains private")
    for name, path in files.items():
        digest = "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()
        if (remote[name].get("state") != "uploaded" or remote[name].get("size") != path.stat().st_size
                or remote[name].get("digest") != digest):
            raise RuntimeError(f"uploaded release asset differs: {name}")


def source_marker(commit: str) -> str:
    return f"<!-- hephaestus-release-source:{commit} -->"


def verify_tag(repo: str, tag: str, commit: str) -> None:
    reference = gh("api", f"repos/{repo}/git/ref/tags/{tag}")
    for _ in range(8):
        if reference["object"]["type"] == "commit":
            break
        if reference["object"]["type"] != "tag":
            raise RuntimeError("release tag does not identify a commit")
        reference = gh("api", f"repos/{repo}/git/tags/{reference['object']['sha']}")
    if reference["object"]["type"] != "commit" or reference["object"]["sha"] != commit:
        raise RuntimeError("remote release tag differs from the approved source commit")


def find_release(repo: str, tag: str) -> dict | None:
    # Published-only lookup by tag misses private drafts. Enumerate all pages,
    # reject ambiguous ownership, then pin subsequent readbacks to the ID.
    pages = gh("api", f"repos/{repo}/releases?per_page=100", "--paginate", "--slurp")
    matches = [release for page in pages for release in page if release.get("tag_name") == tag]
    if len(matches) > 1:
        raise RuntimeError("multiple releases claim this tag; refusing to choose a draft")
    return matches[0] if matches else None


def publish(repo: str, tag: str, commit: str, directory: Path, notes: Path, prerelease: bool) -> None:
    version = workspace_version(Path(__file__).resolve().parents[1])
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo):
        raise RuntimeError("repository must be OWNER/REPO")
    if tag != f"v{version}" or not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise RuntimeError("release tag/version or source commit is invalid")
    files = {path.name: path for path in directory.iterdir()}
    names = expected_assets(version)
    if set(files) != names or any(not path.is_file() or path.is_symlink() for path in files.values()):
        raise RuntimeError("release assets must contain exactly both packages, checksums, inventories and signature bundles")
    if not notes.is_file() or not notes.read_text().strip():
        raise RuntimeError("release notes are missing")

    verify_tag(repo, tag, commit)

    release = find_release(repo, tag)
    if release is not None:
        check_release(release, tag, commit, names)
        if not release["draft"]:
            complete_assets(release, files)
            if release["prerelease"] != prerelease:
                raise RuntimeError("published release channel differs; refusing to change it")
            verify_tag(repo, tag, commit)
            print("Published release already matches the requested source and assets")
            return
    else:
        with tempfile.TemporaryDirectory(prefix="heph-release-notes-") as temporary:
            staged_notes = Path(temporary) / "notes.md"
            staged_notes.write_text(notes.read_text() + "\n" + source_marker(commit) + "\n")
            gh("release", "create", tag, "--repo", repo, "--verify-tag", "--target", commit,
               "--draft", "--title", f"Hephaestus {tag}", "--notes-file", str(staged_notes), "--generate-notes")
        release = find_release(repo, tag)
        if release is None:
            raise RuntimeError("created draft was not found")

    endpoint = f"repos/{repo}/releases/{release['id']}"

    release = gh("api", endpoint)
    check_release(release, tag, commit, names)
    if not release["draft"]:
        raise RuntimeError("release is no longer a private draft; refusing uploads")
    # Retrying can replace assets in this exact source-bound private draft.
    gh("release", "upload", tag, *[str(files[name]) for name in sorted(files)], "--repo", repo, "--clobber")
    release = gh("api", endpoint)
    check_release(release, tag, commit, names)
    if not release["draft"]:
        raise RuntimeError("release became public before upload verification")
    complete_assets(release, files)
    verify_tag(repo, tag, commit)
    gh("release", "edit", tag, "--repo", repo, "--draft=false",
       f"--prerelease={'true' if prerelease else 'false'}", f"--latest={'false' if prerelease else 'true'}")
    release = gh("api", endpoint)
    check_release(release, tag, commit, names)
    complete_assets(release, files)
    if release["draft"] or release["prerelease"] != prerelease:
        raise RuntimeError("release publication readback did not match")
    verify_tag(repo, tag, commit)
    print("Published release verified against the approved source and complete asset digests")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--assets", type=Path, required=True)
    parser.add_argument("--notes", type=Path, required=True)
    parser.add_argument("--prerelease", action="store_true")
    arguments = parser.parse_args()
    try:
        publish(arguments.repo, arguments.tag, arguments.commit, arguments.assets, arguments.notes, arguments.prerelease)
    except (RuntimeError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"Release publication refused: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
