#!/usr/bin/env python3
"""Prepare the installed support-triage pilot with provider-launch markers.

Python is used only by this acceptance harness. The installed user workflow
uses native CLIs, bundled Node and host Git, with no host Python or npm.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

from web_acceptance import cli_data, console


def main() -> int:
    heph, cli, home_string = sys.argv[1:4]
    home = Path(home_string) / "pilot-acceptance"
    home.mkdir(mode=0o700)
    os.environ["HOME"] = str(home)
    hooks = home / "hooks"
    hooks.mkdir()
    hook_marker = home / "git-hook-was-launched"
    hook = hooks / "pre-commit"
    hook.write_text(f"#!/bin/sh\ntouch {shlex.quote(str(hook_marker))}\nexit 91\n")
    hook.chmod(0o700)
    template = home / "git-template"
    template.mkdir()
    (template / "template-marker").write_text("must not copy")
    foreign = home / "foreign"
    foreign.mkdir()
    (foreign / "owner.txt").write_text("preserve me")
    os.environ.update({"GIT_DIR": str(foreign / ".git"), "GIT_WORK_TREE": str(foreign), "GIT_INDEX_FILE": str(home / "foreign-index")})
    os.environ.pop("HEPHAESTUS_CODEX_AUTH_FILE", None)
    os.environ["HEPHAESTUS_PROVIDER_ENV_ALLOWLIST"] = ""
    (home / ".gitconfig").write_text(f"[commit]\n gpgsign = true\n[gpg]\n program = /nonexistent-fixture-signer\n[core]\n hooksPath = {hooks}\n[init]\n templateDir = {template}\n")
    pack = home / "support-triage"
    result = subprocess.run([cli, "--json", "init", "--fixture", "support-triage", str(pack)], capture_output=True, text=True, check=True)
    assert json.loads(result.stdout)["repository"] == str(pack / "repository")
    assert not hook_marker.exists() and not (pack / "repository/.git/template-marker").exists()
    assert (foreign / "owner.txt").read_text() == "preserve me" and not (foreign / ".git").exists()
    assert not (home / "foreign-index").exists()
    for variable in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
        os.environ.pop(variable)
    original = {str(path.relative_to(pack)): hashlib.sha256(path.read_bytes()).hexdigest() for path in pack.rglob("*") if path.is_file() and "repository" not in path.relative_to(pack).parts}
    assert "provider-setup.md" in original
    tracked = subprocess.check_output(["git", "-C", str(pack / "repository"), "ls-files"], text=True)
    assert tracked == "README.md\n", "fixture source contains task labels or prompt templates"
    marker = home / "provider-was-launched"
    stub = home / "provider-stub"
    stub.write_text(f"#!/bin/sh\ntouch {shlex.quote(str(marker))}\nexit 91\n")
    stub.chmod(0o700)
    os.environ.update({
        "HEPHAESTUS_SOURCE_REPOSITORY": str(pack / "repository"),
        "HEPHAESTUS_CODEX_EXECUTABLE": str(stub),
        "HEPHAESTUS_CLAUDE_EXECUTABLE": str(stub),
    })
    data = pack / "data"

    def prepare(provider: str, model: str = "offline-profile-model", cost: str = "250000"):
        return subprocess.run([
            cli, "--data-dir", str(data), "--json", "pilot", "prepare", str(pack),
            "--provider", provider, "--model", model, "--cost-microusd", cost,
        ], capture_output=True, text=True, timeout=30)

    def prepared(provider: str):
        result = prepare(provider)
        assert result.returncode == 0, "native pilot preparation failed"
        return json.loads(result.stdout)

    try:
        with console(heph, data, home, no_daemon=False):
            before_invalid = cli_data(cli, data, "genome", "list")
            before_worlds = cli_data(cli, data, "world", "list")
            before_setups = set(data.glob("pilot-prepare-*"))
            before_status = cli_data(cli, data, "status")
            for cost in ("0", "1000000001", "18446744073709551615"):
                assert prepare("codex", cost=cost).returncode != 0
            after_status = cli_data(cli, data, "status")
            assert after_status["event_count"] == before_status["event_count"] + 1, "invalid cost reached daemon commands"
            assert cli_data(cli, data, "genome", "list") == before_invalid
            assert cli_data(cli, data, "world", "list") == before_worlds
            assert set(data.glob("pilot-prepare-*")) == before_setups
            first = prepared("codex")
            second = prepared("codex")
            for field in ("world_id", "parent_genome_id", "candidate_genome_id", "baseline_profile", "candidate_profile"):
                assert first[field] == second[field], f"preparation retry changed {field}"
            assert first["frozen"] is True and first["provider_work_started"] is False
            for role in ("baseline_profile", "candidate_profile"):
                profile = first[role]
                assert profile["provider"] == "codex" and profile["family"] == "offline-profile-model"
                assert profile["world"]["world_id"] == first["world_id"]
                assert profile["workspace_write"] is False and profile["network"] is True
                assert profile["visible_tasks"] == 12 and profile["sealed_tasks"] == 12
                assert profile["output_scoring"] == "json_canonical"
                assert profile["reported_cost_limit_microusd"] == "250000"
                assert profile["paired_total_wall_millis"] == 14_410_000
            setup = Path(first["setup_directory"])
            assert setup.stat().st_mode & 0o777 == 0o700
            assert all((setup / name).is_file() for name in ("world.json", "baseline.md", "candidate.md"))
            evaluator = Path(cli).resolve().with_name("hephaestus-reference-evaluator")
            artifact = cli_data(cli, data, "artifact", "put", str(evaluator))["artifact_id"]
            assert json.loads((setup / "world.json").read_text())["evaluator_artifacts"]["arena.evaluator"] == artifact
            claude = prepared("claude")
            assert claude["candidate_profile"]["provider"] == "claude"
            assert not marker.exists(), "preparation launched a provider"
            assert cli_data(cli, data, "status")["frozen"] is True
            assert cli_data(cli, data, "evaluations")["evaluations"] == []
            subprocess.run([cli, "--data-dir", str(data), "unfreeze"], capture_output=True, check=True)
            before = cli_data(cli, data, "genome", "list")
            before_worlds = cli_data(cli, data, "world", "list")
            before_setups = set(data.glob("pilot-prepare-*"))
            before_status = cli_data(cli, data, "status")
            rejected = prepare("claude", "different-model")
            assert rejected.returncode != 0 and "frozen daemon" in rejected.stderr
            after_status = cli_data(cli, data, "status")
            assert after_status["event_count"] == before_status["event_count"] + 2, "unfrozen preparation did more than status preflight"
            assert cli_data(cli, data, "genome", "list") == before
            assert cli_data(cli, data, "world", "list") == before_worlds
            assert set(data.glob("pilot-prepare-*")) == before_setups
            subprocess.run([cli, "--data-dir", str(data), "freeze"], capture_output=True, check=True)
            cli_data(cli, data, "replay")
        subprocess.run([heph, "--data-dir", str(data), "stop"], capture_output=True, check=True)
        with console(heph, data, home, no_daemon=False):
            restarted = prepared("codex")
            assert restarted["candidate_profile"] == first["candidate_profile"]
            assert restarted["parent_genome_id"] == first["parent_genome_id"]
            cli_data(cli, data, "replay")
        for relative, digest in original.items():
            assert hashlib.sha256((pack / relative).read_bytes()).hexdigest() == digest, f"modified original: {relative}"
        assert not marker.exists()
    finally:
        if (data / "heph.pid").is_file():
            original_failure = sys.exc_info()[0] is not None
            try:
                subprocess.run([heph, "--data-dir", str(data), "stop"], capture_output=True, check=True, timeout=30)
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                if not original_failure:
                    raise
                print("owned pilot daemon cleanup also failed; original failure preserved", file=sys.stderr)
    print(json.dumps({
        "status": "passed", "fixture": "support-triage", "source_contains_no_cases": True,
        "provider_launches": 0, "frozen_preparation": True, "restart_replay_verified": True,
        "retry_profiles_identical": True, "unfrozen_preparation_rejected": True,
        "source_templates_preserved": True, "setup_mode": "0700", "global_git_signing_override_verified": True, "git_hooks_templates_and_location_env_ignored": True, "invalid_cost_rejected_before_registration": True,
        "evaluator_binding_matches_install": True,
        "scope": "offline placeholder model and marker stubs; no auth or model availability established",
    }, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
