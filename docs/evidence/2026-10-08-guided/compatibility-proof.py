"""Reopen the previously verified catalog-v1/prose-v2 scratch ledger.

Usage: python3 compatibility-proof.py <old-forge-proof-root> <current-bin-dir>
Only reads existing domain receipts; ordinary operator audits still append.
Original reports remain unchanged. Credentials and daemon logs stay private.
"""
import hashlib
import json
import os
from pathlib import Path
import socket
import sqlite3
import subprocess
import sys
import time

root = Path(sys.argv[1]).resolve()
bins = Path(sys.argv[2]).resolve()
data = root / 'data'
repo = root / 'source'
log = (root / 'guided-compatibility-daemon.log').open('w')
env = os.environ.copy()
env['HEPHAESTUS_CLAUDE_EXECUTABLE'] = str(root / 'fake-claude')
process = subprocess.Popen([
    str(bins / 'hephaestusd'), '--data-dir', str(data), '--source-repository', str(repo),
    '--evaluator-executable', str(data / 'reference-evaluator'),
    '--reference-worker-executable', str(data / 'reference-worker'),
], stdout=log, stderr=log, env=env)


def invoke(*args):
    output = subprocess.run([str(bins / 'hephaestus'), '--data-dir', str(data), '--json', *args],
                            capture_output=True, text=True, timeout=30)
    response = json.loads(output.stdout)
    assert output.returncode == 0 and response.get('error') is None, response.get('error')
    return response['data']


try:
    deadline = time.monotonic() + 15
    while True:
        assert process.poll() is None, 'scratch daemon stopped during verified startup'
        try:
            with socket.socket(socket.AF_UNIX) as connection:
                connection.connect(str(data / 'control.sock'))
            break
        except OSError:
            assert time.monotonic() < deadline
            time.sleep(.05)
    ids = json.loads((root / 'ids.json').read_text())
    catalog = invoke('genome', 'proposal', 'old-catalog')['proposal']
    prose = invoke('genome', 'proposal', 'prose-upgrade')['revision']
    legacy = json.loads((root / 'legacy-report.json').read_text())
    current = json.loads((root / 'current-report.json').read_text())
    assert catalog == legacy['catalog_proposal']
    assert prose == current['revision']
    assert invoke('genome', 'prompt', ids['prose_child'])['prompt'] == 'Return the task in uppercase.\r\ncafé '
    champion = invoke('champion', 'show', ids['world'])['champion']
    assert champion['champion_genome_id'] == ids['prose_child']
    old_job = invoke('job', 'status', 'prose-source')
    # The CLI resolves a succeeded Arena job to its terminal evaluation record.
    assert old_job['type'] == 'evaluation'
    assert old_job['evaluation']['evaluation_id'] == 'prose-source'
    # Read only the public admission metadata from this private offline fixture.
    with sqlite3.connect(f'file:{data}/events.sqlite3?mode=ro', uri=True) as connection:
        candidates = [json.loads(payload) for (payload,) in connection.execute(
            "SELECT payload FROM events WHERE event_type LIKE 'arena.job.%'")]
    admissions = [value for value in candidates if value.get('evaluation_id') == 'prose-source'
                  and 'trial_budget' in value]
    assert admissions, 'historical admission was not located'
    assert all(value['trial_budget']['wall_millis'] == 10_000 for value in admissions)
    assert any(value['state'] == 'succeeded' for value in admissions)
    report = {'catalog_schema1_preserved': True, 'prose_schema2_preserved': True,
              'exact_prompt_preserved': True, 'champion_preserved': True,
              'historical_terminal_preserved': True, 'historical_trial_wall_millis': 10_000,
              'binary_sha256': {name: hashlib.sha256((bins / name).read_bytes()).hexdigest()
                                for name in ['hephaestus', 'hephaestusd']}, 'replay': invoke('replay')}
    (root / 'guided-compatibility.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({'historical_reopen_verified': True, 'historical_trial_wall_millis': 10_000}))
finally:
    if process.poll() is None:
        try:
            invoke('daemon', 'stop')
        except Exception:
            process.terminate()
        process.wait(timeout=15)
    log.close()
