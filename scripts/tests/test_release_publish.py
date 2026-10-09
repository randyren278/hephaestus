"""Exercise publication and recovery against a local, stateful GitHub CLI fixture."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
from scripts.package_macos import workspace_version

VERSION = workspace_version(ROOT)
TAG = f"v{VERSION}"
COMMIT = "b" * 40
BINARIES = ("heph", "hephaestus", "hephaestusd", "hephaestus-reference-worker",
            "hephaestus-reference-evaluator", "hephaestus-process-guardian", "senate")

FAKE_GH = r'''
import hashlib, json, os, sys
from pathlib import Path
store = Path(os.environ["RELEASE_FIXTURE"])
state = json.loads(store.read_text())
args = sys.argv[1:]
state["calls"].append(args)
def finish(value=None, error=None):
    store.write_text(json.dumps(state))
    if error:
        print(error, file=sys.stderr)
        raise SystemExit(1)
    if value is not None:
        print(json.dumps(value))
    raise SystemExit(0)
if args[0] == "api":
    path = args[1]
    if path == "repos/owner/project/git/ref/tags/" + state["tag"]:
        state["tag_reads"] = state.get("tag_reads", 0) + 1
        if state.get("move_tag_after") and state["tag_reads"] > state["move_tag_after"]:
            state["tag_commit"] = "c" * 40
        if state.get("lightweight"):
            finish({"object": {"type": "commit", "sha": state["tag_commit"]}})
        finish({"object": {"type": "tag", "sha": "a" * 40}})
    if path == "repos/owner/project/git/tags/" + "a" * 40:
        finish({"object": {"type": "commit", "sha": state["tag_commit"]}})
    if path == "repos/owner/project/releases?per_page=100":
        if state.get("api_outage"):
            finish(error="gh: service unavailable (HTTP 503)")
        releases = [state["release"]] if state["release"] else []
        if state.get("duplicate"):
            releases.append(dict(state["release"], id=2))
        finish([[], releases])  # Draft can occur on a later page.
    if path == "repos/owner/project/releases/1":
        finish(state["release"])
    finish(error="unexpected API endpoint")
if args[:1] != ["release"] or "--repo" not in args or args[args.index("--repo") + 1] != "owner/project":
    finish(error="explicit repository is required")
if args[1] == "create":
    if state["release"] is not None or "--draft" not in args or "--verify-tag" not in args:
        finish(error="creation must be private and use an existing tag")
    state["release"] = {"tag_name": args[2], "id": 1, "target_commitish": "main",
                        "body": Path(args[args.index("--notes-file") + 1]).read_text(),
                        "draft": True, "prerelease": False, "assets": []}
    print("https://github.com/owner/project/releases/tag/" + state["tag"])
    finish()
if args[1] == "upload":
    release = state["release"]
    if not release["draft"] or "--clobber" not in args:
        finish(error="cannot replace public assets")
    assets = {asset["name"]: asset for asset in release["assets"]}
    for filename in args[3:args.index("--repo")]:
        path = Path(filename)
        data = path.read_bytes()
        assets[path.name] = {"name": path.name, "state": "uploaded", "size": len(data), "digest": "sha256:" + hashlib.sha256(data).hexdigest()}
        release["assets"] = list(assets.values())
        if state.get("fail_upload"):
            finish(error="simulated interrupted upload")
    if state.get("corrupt_upload"):
        release["assets"][0]["digest"] = "sha256:" + "0" * 64
    if state.get("pending_upload"):
        release["assets"][0]["state"] = "starter"
    finish()
if args[1] == "edit":
    if "--draft=false" not in args:
        finish(error="unexpected edit")
    state["release"]["draft"] = False
    state["release"]["prerelease"] = "--prerelease=true" in args
    state["release"]["latest"] = "--latest=true" in args
    print("https://github.com/owner/project/releases/tag/" + state["tag"])
    finish()
finish(error="unexpected command")
'''


class ReleasePublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="heph-release-contract-")
        self.addCleanup(self.temporary.cleanup)
        self.work = Path(self.temporary.name)
        tools = self.work / "bin"
        tools.mkdir()
        executable = tools / "gh"
        executable.write_text(f"#!{sys.executable}\n" + FAKE_GH)
        executable.chmod(0o755)
        self.assets = self.work / "assets"
        self.assets.mkdir()
        unsigned = {f"hephaestus-v{VERSION}-macos-{arch}.tar.gz{suffix}"
                    for arch in ("arm64", "x86_64") for suffix in ("", ".sha256")}
        unsigned.update(f"{binary}_bin_{target}.cdx.json" for binary in BINARIES
                        for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"))
        for name in unsigned | {f"{name}.sigstore.json" for name in unsigned}:
            (self.assets / name).write_text(f"fictional asset {name}\n")
        self.notes = self.work / "notes.md"
        self.notes.write_text("Working local tool; fresh-user acceptance remains unverified.\n")
        self.state_file = self.work / "server.json"
        self.write_state({"tag": TAG, "tag_commit": COMMIT, "release": None, "calls": []})
        self.environment = {"PATH": f"{tools}{os.pathsep}{os.defpath}", "HOME": str(self.work),
                            "RELEASE_FIXTURE": str(self.state_file)}

    def write_state(self, state: dict) -> None:
        self.state_file.write_text(json.dumps(state))

    def state(self) -> dict:
        return json.loads(self.state_file.read_text())

    def run_publisher(self) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, "-m", "scripts.publish_release", "--repo", "owner/project",
                               "--tag", TAG, "--commit", COMMIT, "--assets", str(self.assets),
                               "--notes", str(self.notes), "--prerelease"], cwd=ROOT, env=self.environment,
                              capture_output=True, text=True, timeout=30)

    def test_complete_private_upload_becomes_a_non_latest_prerelease(self) -> None:
        result = self.run_publisher()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        release = self.state()["release"]
        self.assertFalse(release["draft"])
        self.assertTrue(release["prerelease"])
        self.assertFalse(release["latest"])
        self.assertEqual(len(release["assets"]), 36)
        self.assertEqual(release["target_commitish"], "main")
        self.assertIn(COMMIT, release["body"])

    def test_interrupted_upload_remains_private_and_retry_completes_it(self) -> None:
        state = self.state()
        state["fail_upload"] = True
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        state = self.state()
        self.assertTrue(state["release"]["draft"])
        self.assertEqual(len(state["release"]["assets"]), 1)
        self.assertFalse(any(call[:2] == ["release", "edit"] for call in state["calls"]))
        state["fail_upload"] = False
        self.write_state(state)
        result = self.run_publisher()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.state()["release"]["draft"])
        self.assertEqual(len(self.state()["release"]["assets"]), 36)
        self.assertEqual(sum(call[:2] == ["release", "create"] for call in self.state()["calls"]), 1)

    def test_corrupted_remote_asset_never_becomes_public(self) -> None:
        state = self.state()
        state["corrupt_upload"] = True
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertTrue(self.state()["release"]["draft"])

    def test_remote_tag_must_resolve_to_the_approved_commit(self) -> None:
        state = self.state()
        state["tag_commit"] = "c" * 40
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertIsNone(self.state()["release"])

    def test_incomplete_local_assets_do_not_create_a_release(self) -> None:
        next(self.assets.glob("*.sigstore.json")).unlink()
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertEqual(self.state()["calls"], [])

    def test_api_failure_is_not_treated_as_an_absent_release(self) -> None:
        state = self.state()
        state["api_outage"] = True
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertIsNone(self.state()["release"])

    def test_successful_publication_retry_does_not_modify_public_assets(self) -> None:
        self.assertEqual(self.run_publisher().returncode, 0)
        state = self.state()
        state["calls"] = []
        self.write_state(state)
        self.assertEqual(self.run_publisher().returncode, 0)
        self.assertTrue(all(call[0] == "api" for call in self.state()["calls"]))

    def test_existing_draft_for_another_commit_is_preserved(self) -> None:
        state = self.state()
        state["release"] = {"id": 1, "tag_name": TAG, "target_commitish": "main",
                            "body": "<!-- hephaestus-release-source:" + "c" * 40 + " -->",
                            "draft": True, "prerelease": False, "assets": []}
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertEqual(self.state()["release"], state["release"])
        self.assertTrue(all(call[0] == "api" for call in self.state()["calls"]))

    def test_lightweight_tag_is_supported(self) -> None:
        state = self.state()
        state["lightweight"] = True
        self.write_state(state)
        result = self.run_publisher()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_tag_move_before_publication_keeps_draft_private(self) -> None:
        state = self.state()
        state["move_tag_after"] = 1
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertTrue(self.state()["release"]["draft"])

    def test_ambiguous_drafts_are_preserved(self) -> None:
        state = self.state()
        state["fail_upload"] = True
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        state = self.state()
        original = state["release"]
        state.update(duplicate=True, calls=[])
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertEqual(self.state()["release"], original)
        self.assertTrue(all(call[0] == "api" for call in self.state()["calls"]))

    def test_public_retry_revalidates_tag_and_channel(self) -> None:
        self.assertEqual(self.run_publisher().returncode, 0)
        state = self.state()
        state["release"]["prerelease"] = False
        state["calls"] = []
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertTrue(all(call[0] == "api" for call in self.state()["calls"]))

    def test_symlink_assets_are_refused_without_api_calls(self) -> None:
        asset = next(self.assets.iterdir())
        data = self.work / "original"
        asset.rename(data)
        asset.symlink_to(data)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertEqual(self.state()["calls"], [])

    def test_pending_remote_upload_stays_private(self) -> None:
        state = self.state()
        state["pending_upload"] = True
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertTrue(self.state()["release"]["draft"])

    def test_public_retry_refuses_a_tag_moved_after_inspection(self) -> None:
        self.assertEqual(self.run_publisher().returncode, 0)
        state = self.state()
        state.update(tag_reads=0, move_tag_after=1, calls=[])
        self.write_state(state)
        self.assertNotEqual(self.run_publisher().returncode, 0)
        self.assertTrue(all(call[0] == "api" for call in self.state()["calls"]))


if __name__ == "__main__":
    unittest.main()
