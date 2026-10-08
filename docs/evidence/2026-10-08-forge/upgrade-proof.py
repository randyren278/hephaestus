"""Offline, actual-binary catalog-v1 to prose-v2 history upgrade proof.

Run legacy then current against a fresh, short scratch root containing
legacy-target/debug built from commit 7bf55d0. Current binaries come from
this checkout's target/debug. Credentials and daemon logs stay private.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import sqlite3
import subprocess
import sys
import time

root = Path(sys.argv[1]).resolve()
mode = sys.argv[2]
assert mode in {"legacy", "current", "restart"}
data = root / "data"
repo = root / "source"
bins = root / "legacy-target/debug" if mode == "legacy" else Path.cwd() / "target/debug"


def invoke(*args, raw=False):
    command = [str(bins / "hephaestus"), "--data-dir", str(data)]
    if not raw:
        command.append("--json")
    result = subprocess.run(command + list(args), text=True, capture_output=True, timeout=60)
    if result.returncode:
        raise RuntimeError(result.stderr or result.stdout)
    if raw:
        return result.stdout
    response = json.loads(result.stdout)
    assert response.get("error") is None, response.get("error")
    return response["data"]


def put(path, manifest=False):
    args = ["arena", "manifest"] if manifest else ["artifact", "put"]
    return invoke(*args, str(path), raw=True).split()[0]


def proposal_bytes(proposal_id):
    with sqlite3.connect(f"file:{data}/events.sqlite3?mode=ro", uri=True) as connection:
        rows = connection.execute("SELECT payload FROM events WHERE event_id=?", (f"forge:{proposal_id}:proposed",)).fetchall()
    assert len(rows) == 1
    payload = rows[0][0]
    return {"payload": json.loads(payload), "payload_sha256": hashlib.sha256(payload).hexdigest()}


def binary_hashes():
    paths = {name: bins / name for name in ["hephaestus", "hephaestusd"]}
    paths.update({name: data / name for name in ["reference-evaluator", "reference-worker"]})
    return {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in paths.items()}


def write_json(name, value):
    path = root / name
    path.write_text(json.dumps(value, indent=2) + "\n")
    return path


if mode == "legacy":
    assert not data.exists() and not repo.exists(), "legacy mode needs a fresh scratch directory"
    repo.mkdir()
    (repo / "fixture.txt").write_text("Forge history upgrade fixture\n")
    for args in [["init", "-q"], ["config", "user.name", "Upgrade Fixture"], ["config", "user.email", "upgrade@example.invalid"], ["add", "."], ["commit", "-m", "fixture", "-q"]]:
        subprocess.run(["git", "-C", str(repo)] + args, check=True, capture_output=True)
    data.mkdir()
    for name in ["evaluator", "worker"]:
        shutil.copyfile(bins / f"hephaestus-reference-{name}", data / f"reference-{name}")
        (data / f"reference-{name}").chmod(0o700)
    (root / "fake-claude").write_text("#!/bin/sh\nexit 42\n")
    (root / "fake-claude").chmod(0o700)

environment = os.environ.copy()
environment["HEPHAESTUS_CLAUDE_EXECUTABLE"] = str(root / "fake-claude")
log = (root / f"{mode}-daemon.log").open("w")
process = subprocess.Popen([
    str(bins / "hephaestusd"), "--data-dir", str(data), "--source-repository", str(repo),
    "--evaluator-executable", str(data / "reference-evaluator"),
    "--reference-worker-executable", str(data / "reference-worker"),
], stdout=log, stderr=log, env=environment)
try:
    deadline = time.monotonic() + 15
    while True:
        assert process.poll() is None, f"{mode} daemon exited; inspect private log"
        try:
            with socket.socket(socket.AF_UNIX) as connection:
                connection.connect(str(data / "control.sock"))
            break
        except OSError:
            assert time.monotonic() < deadline
            time.sleep(.05)
    if mode == "legacy":
        ids = {}
        for visibility in ["visible", "sealed"]:
            path = write_json(f"{visibility}.json", {
                "schema_version": 1, "manifest_id": f"forge-upgrade-{visibility}", "visibility": visibility,
                "tasks": [{"task_id": f"{visibility}-task", "input": visibility, "expected_output": visibility.upper()}],
            })
            ids[visibility] = put(path, manifest=True)
        ids["evaluator"] = put(data / "reference-evaluator")
        ids["verifier"] = invoke("verifier", raw=True).split()[0]
        invariant_path = write_json("invariants.json", {"schema_version": 1, "algorithm": "reference-output-invariants-v1", "maximum_output_bytes": 4096, "forbidden_ascii_bytes": [0]})
        invariant_path.write_bytes(json.dumps(json.loads(invariant_path.read_text()), separators=(",", ":")).encode())
        ids["invariants"] = put(invariant_path)
        world = {
            "schema_version": 1, "name": "forge-history-upgrade",
            "laws": {"candidate_network": False, "candidate_evaluator_access": False, "maximum_cost_microusd": 0},
            "authority_ceiling": {"workspace_write": False, "network": False}, "mutation_scope": ["harness"],
            "promotion": {"minimum_delta_bps": 0, "maximum_regressions": 0, "confidence_bps": 9500},
            "objectives": ["correctness"],
            "evaluator_artifacts": {"arena.visible_manifest": ids["visible"], "arena.sealed_manifest": ids["sealed"], "arena.evaluator": ids["evaluator"], "arena.runtime_verifier": ids["verifier"], "arena.invariant_manifest": ids["invariants"]},
        }
        path = write_json("world.json", world)
        ids["world"] = invoke("world", "register", str(path))["world"]["world_id"]
        for role in ["parent", "candidate"]:
            path = root / f"reference-{role}.md"
            parents = [] if role == "parent" else [ids["parent"]]
            path.write_text("---\nschema_version: 1\n" + f"name: upgrade-{role}\nparents: {json.dumps(parents)}\n" + "model:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n")
            ids[role] = invoke("genome", "register", str(path), "--world", ids["world"])["genome"]["genome_id"]
        invoke("unfreeze")
        invoke("arena", "evaluate", "catalog-source", ids["parent"], ids["candidate"])
        selected = invoke("arena", "select", "catalog-source")["selection"]
        ids["selection"] = selected["event"]["event_id"]
        proposal = invoke("genome", "propose", "old-catalog", "--selection-event", ids["selection"], "--parent", ids["candidate"], "--hypothesis", "Explicit uppercase operation should improve correctness.")["proposal"]
        assert proposal["payload"]["schema_version"] == 1
        ids["catalog_child"] = proposal["payload"]["child"]["genome_id"]
        write_json("ids.json", ids)
        report = {"legacy_commit": "7bf55d0", "binary_sha256": binary_hashes(), "catalog_proposal": proposal, "catalog_bytes": proposal_bytes("old-catalog"), "replay": invoke("replay")}
    else:
        ids = json.loads((root / "ids.json").read_text())
        legacy = json.loads((root / "legacy-report.json").read_text())
        assert proposal_bytes("old-catalog") == legacy["catalog_bytes"], "schema-1 proposal bytes changed"
        for name in ["reference-evaluator", "reference-worker"]:
            assert binary_hashes()[name] == legacy["binary_sha256"][name], "pinned evaluator/worker changed"
        old = invoke("genome", "propose", "old-catalog", "--selection-event", ids["selection"], "--parent", ids["candidate"], "--hypothesis", "Explicit uppercase operation should improve correctness.")["proposal"]
        assert old == legacy["catalog_proposal"], "schema-1 retry changed the recorded envelope"
        replay_before = invoke("replay")
        if mode == "current":
            before = "Return the task unchanged.\r\ncafé "
            after = "Return the task in uppercase.\r\ncafé "
            script = '#!/bin/sh\ncase " $* " in *\' --model=sonnet \'*) ;; *) exit 41 ;; esac\ncat > "$TMPDIR/frame"\n'
            for role, instruction in [("before", before), ("after", after)]:
                for task in ["visible", "sealed"]:
                    frame_name = f"{role}-{task}.frame"
                    (repo / frame_name).write_bytes((f"HEPHAESTUS-PROVIDER-INPUT-V2\nFollow the agent instructions to complete the task. Section lengths count UTF-8 bytes.\nAGENT-INSTRUCTION {len(instruction.encode())}\n{instruction}\nTASK {len(task)}\n{task}\n").encode())
                    result = json.dumps({"type": "result", "subtype": "success", "result": task.upper() if role == "after" else task, "total_cost_usd": 0.0})
                    script += f'if cmp -s "$TMPDIR/frame" "{frame_name}"; then\nprintf \'%s\\n\' \'{result}\'\nexit 0\nfi\n'
            script += "exit 42\n"
            (root / "fake-claude").write_text(script)
            for args in [["add", "."], ["commit", "-m", "hosted frame fixtures", "-q"]]:
                subprocess.run(["git", "-C", str(repo)] + args, check=True, capture_output=True)
            for role in ["baseline", "hosted"]:
                path = root / f"{role}.md"
                parents = [] if role == "baseline" else [ids["baseline"]]
                header = "---\nschema_version: 1\n" + f"name: upgrade-{role}\nparents: {json.dumps(parents)}\n" + "model:\n  provider: claude\n  family: sonnet\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n"
                path.write_bytes((header + before).encode())
                ids[role] = invoke("genome", "register", str(path), "--world", ids["world"])["genome"]["genome_id"]
            invoke("arena", "evaluate", "prose-source", ids["baseline"], ids["hosted"])
            selected = invoke("arena", "select", "prose-source")["selection"]
            ids["prose_selection"] = selected["event"]["event_id"]
            path = root / "revision.txt"
            path.write_bytes(after.encode())
            revision_args = ["genome", "revise", "prose-upgrade", "--selection-event", ids["prose_selection"], "--parent", ids["hosted"], "--prompt-file", str(path), "--hypothesis", "Explicit uppercase prose should improve correctness."]
            revision = invoke(*revision_args)["revision"]
            assert revision["payload"]["schema_version"] == 2 and not revision["promotion_eligible"]
            assert invoke(*revision_args)["revision"] == revision
            ids["prose_child"] = revision["payload"]["child"]["genome_id"]
            prompt = invoke("genome", "prompt", ids["prose_child"])["prompt"]
            assert prompt == after, "exact prompt bytes changed"
            evaluation = invoke("arena", "evaluate", "prose-comparison", ids["hosted"], ids["prose_child"])
            selected = invoke("arena", "select", "prose-comparison")["selection"]
            assessment = invoke("genome", "assess", "prose-upgrade-assessment", "--proposal", "prose-upgrade", "--selection-event", selected["event"]["event_id"])["assessment"]
            assert assessment["payload"]["outcome"] == "metrics_passed"
            invariants = invoke("arena", "invariants", "prose-comparison")
            invoke("champion", "seed", "hosted-bootstrap", "--world", ids["world"], "--genome", ids["hosted"], "--reason", "Bootstrap hosted prompt")
            promotion = invoke("champion", "promote", "hosted-promotion", "--assessment", "prose-upgrade-assessment")
            write_json("ids.json", ids)
            report = {"legacy_commit": "7bf55d0", "binary_sha256": binary_hashes(), "catalog_bytes_after_upgrade": proposal_bytes("old-catalog"), "replay_before_revision": replay_before, "revision": revision, "revision_bytes": proposal_bytes("prose-upgrade"), "comparison": evaluation, "assessment": assessment, "invariants": invariants, "promotion": promotion, "mixed_replay": invoke("replay")}
        else:
            current = json.loads((root / "current-report.json").read_text())
            assert proposal_bytes("prose-upgrade") == current["revision_bytes"]
            prompt = invoke("genome", "prompt", ids["prose_child"])["prompt"]
            assert prompt == "Return the task in uppercase.\r\ncafé "
            champion = invoke("champion", "show", ids["world"])
            assert champion["champion"]["champion_genome_id"] == ids["prose_child"]
            report = {"binary_sha256": binary_hashes(), "catalog_bytes_after_restart": proposal_bytes("old-catalog"), "revision_bytes_after_restart": proposal_bytes("prose-upgrade"), "champion": champion, "replay": invoke("replay")}
    write_json(f"{mode}-report.json", report)
    print(json.dumps({"mode": mode, "report_saved": True}))
finally:
    if process.poll() is None:
        try:
            invoke("daemon", "stop")
        except Exception:
            process.terminate()
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    log.close()
