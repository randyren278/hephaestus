#!/usr/bin/env python3
"""Exercise the installed browser launcher under the package acceptance PATH.

Uses only Python's standard library. Session URLs stay in a private temporary
file; acceptance output never includes session or operator tokens.
"""

from __future__ import annotations

import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager


def cli_data(cli: str, data: Path, *arguments: str) -> dict:
    result = subprocess.run(
        [cli, "--data-dir", str(data), "--json", *arguments],
        capture_output=True, text=True, check=True, timeout=30,
    )
    response = json.loads(result.stdout)
    assert response.get("error") is None, "daemon returned an error"
    return response["data"]


@contextmanager
def console(heph: str, data: Path, home: Path, *, no_daemon: bool):
    env = dict(
        os.environ, HEPHAESTUS_HOME=str(home / "wrong-data-directory"),
        NODE_OPTIONS="--hephaestus-invalid-host-option", NODE_PATH=str(home / "unrelated-node-modules"),
    )
    env.pop("HEPHAESTUS_WEB_PORT", None)
    arguments = [heph, "web", "--data-dir", str(data)]
    if no_daemon:
        arguments.append("--no-daemon")
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as output:
        child = subprocess.Popen(
            arguments, cwd=home, env=env, stdin=subprocess.DEVNULL,
            stdout=output, stderr=output, start_new_session=True,
        )
        try:
            deadline = time.monotonic() + 30
            match = None
            while time.monotonic() < deadline:
                output.seek(0)
                text = output.read()
                match = re.search(r"http://127\.0\.0\.1:(\d+)/#token=([0-9a-f]{64})", text)
                if match:
                    break
                if child.poll() is not None:
                    raise RuntimeError("installed browser launcher exited before binding")
                time.sleep(0.05)
            if not match:
                raise RuntimeError("installed browser launcher did not bind within 30 seconds")
            assert "Just here for the Senate?" not in text, "web launch asked a terminal question"
            yield int(match[1]), match[2]
        finally:
            # Match terminal Ctrl+C: stop launcher and Node in their private
            # foreground group. The daemon started by heph has its own group.
            try:
                os.killpg(child.pid, signal.SIGINT)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)


def request(port: int, path: str, *, body: dict | None = None, headers: dict | None = None):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        connection.request(
            "GET" if body is None else "POST", path,
            body=None if body is None else json.dumps(body),
            headers=headers or {},
        )
        response = connection.getresponse()
        return response.status, dict(response.getheaders()), response.read()
    finally:
        connection.close()


def main() -> int:
    heph, cli, data_string, home_string, evaluation_id = sys.argv[1:6]
    data, home = Path(data_string), Path(home_string)
    before = cli_data(cli, data, "status")
    expected = cli_data(cli, data, "evaluations", "--limit", "50")
    assert any(entry["evaluation"]["evaluation_id"] == evaluation_id for entry in expected["evaluations"])
    bundle = Path(heph).resolve().parent.parent / "share/hephaestus/web/web"
    asset_hashes = {}
    with console(heph, data, home, no_daemon=True) as (port, token):
        for route, asset in (
            ("/", "index.html"), ("/app.js", "app.js"),
            ("/styles.css", "styles.css"), ("/web-header-crest.svg", "web-header-crest.svg"),
        ):
            status, headers, content = request(port, route)
            assert status == 200 and content == (bundle / asset).read_bytes(), f"packaged asset mismatch: {asset}"
            assert "default-src 'self'" in headers["Content-Security-Policy"]
            asset_hashes[asset] = hashlib.sha256(content).hexdigest()
        status, _, _ = request(port, "/api/command", body={"command": "status"})
        assert status == 401, "API accepted a missing session token"
        headers = {"Content-Type": "application/json", "x-hephaestus-web-token": token}
        for command in ("freeze", "unfreeze", "kill_all", "job_kill", "daemon_stop", "champion_rollback"):
            status, _, _ = request(port, "/api/command", body={"command": command}, headers=headers)
            assert status == 400, f"browser accepted a mutating command: {command}"
        for extra, expected_status in (({"Host": "untrusted.invalid"}, 421), ({"Origin": "https://untrusted.invalid"}, 403)):
            status, _, _ = request(port, "/api/command", body={"command": "status"}, headers=dict(headers, **extra))
            assert status == expected_status, "browser accepted an untrusted origin or host"
        status, _, payload = request(port, "/api/command", body={"command": "evaluation_list", "limit": 50}, headers=headers)
        response = json.loads(payload)
        assert status == 200 and response.get("error") is None
        assert response["data"] == expected, "browser comparisons differ from CLI evidence"
    after = cli_data(cli, data, "status")
    # Authenticated reads are themselves audited canonical events. Compare
    # operational state and immutable evidence, rather than the audit count.
    for key in ("frozen", "active_runs", "genome_count"):
        assert after[key] == before[key], f"read-only browser or Ctrl+C changed {key}"
    assert cli_data(cli, data, "evaluations", "--limit", "50") == expected
    try:
        request(port, "/")
    except OSError:
        pass
    else:
        raise RuntimeError("browser still serves after foreground Ctrl+C")

    missing = home / "web-no-daemon"
    result = subprocess.run(
        [heph, "web", "--data-dir", str(missing), "--no-daemon"],
        capture_output=True, text=True, timeout=30,
    )
    assert result.returncode != 0 and "no daemon is running" in result.stderr
    assert not missing.exists(), "--no-daemon created a data directory"

    broken_data = home / "web-broken-bundle"
    asset = bundle / "app.js"
    hidden = bundle / "app.js.acceptance-backup"
    assert not hidden.exists()
    asset.rename(hidden)
    try:
        result = subprocess.run(
            [heph, "web", "--data-dir", str(broken_data)],
            capture_output=True, text=True, timeout=30,
        )
        assert result.returncode != 0 and "asset is missing" in result.stderr
        assert not broken_data.exists(), "broken package started a daemon"
    finally:
        hidden.rename(asset)

    fresh = home / "web-bootstrap"
    try:
        with console(heph, fresh, home, no_daemon=False) as (port, token):
            status, _, payload = request(
                port, "/api/command", body={"command": "status"},
                headers={"x-hephaestus-web-token": token},
            )
            response = json.loads(payload)
            assert status == 200 and response.get("error") is None
            assert response["data"]["frozen"] is True and response["data"]["active_runs"] == 0
            assert cli_data(cli, fresh, "evaluations")["evaluations"] == []
            assert (fresh / "quickstart/repository/.git/HEAD").is_file()
        assert cli_data(cli, fresh, "status")["frozen"] is True, "Ctrl+C stopped or unfroze the daemon"
    finally:
        if (fresh / "heph.pid").is_file():
            subprocess.run([heph, "--data-dir", str(fresh), "stop"], capture_output=True, timeout=30, check=True)
    print(json.dumps({
        "status": "passed", "bundled_assets_sha256": asset_hashes,
        "comparisons_match_cli": True, "read_only_state_unchanged": True,
        "no_daemon_has_no_side_effects": True, "fresh_bootstrap_frozen": True,
        "broken_bundle_has_no_side_effects": True,
        "ctrl_c_preserves_daemon": True,
        "host_node_options_ignored": True,
    }, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
