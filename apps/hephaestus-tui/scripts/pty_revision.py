#!/usr/bin/env python3
"""Drive the packaged revision flow against a running offline fixture daemon.

Arguments: app-dir data-dir source-evaluation exact-after-body original-source.
Checks editor input, explicit admission, TUI restart and canonical event counts.
"""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time

app, data, source_id, body, original = sys.argv[1:]
data = Path(data)
body = Path(body).resolve()
original = Path(original).resolve()
original_hash = hashlib.sha256(original.read_bytes()).hexdigest()
old_proposals = {path.name.split('.cursor-')[0] for path in (data / 'revision-drafts').glob('*.cursor-*.json')}
ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")


class Terminal:
    def __init__(self, home):
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        self.output = bytearray()
        editor = home / 'revision editor.sh'
        editor.write_text('#!/bin/sh\n[ "$1" = --wait ] && [ "$2" = "two words" ] || exit 41\n'
                          'stty -echo\nprintf "REVISION_EDITOR_START\\n"\nIFS= read -r line\n'
                          'printf \'%s\\n\' "$line" >> "$REVISION_INPUT_REPORT"\n'
                          'cat "$REVISION_BODY" > "$3"\nstty echo\nprintf "REVISION_EDITOR_END\\n"\n')
        env = os.environ.copy()
        env.update({'HOME': str(home), 'HEPHAESTUS_HOME': str(data), 'CI': 'false', 'COLUMNS': '80', 'LINES': '24',
                    'EDITOR': f"sh '{editor}' --wait 'two words'", 'REVISION_BODY': str(body), 'REVISION_INPUT_REPORT': str(home / 'input.txt')})
        env.pop('VISUAL', None)
        self.child = subprocess.Popen(['node', str(Path(app).resolve() / 'dist/main.mjs')], cwd=home, env=env,
                                      stdin=self.slave, stdout=self.slave, stderr=self.slave, close_fds=True)

    def text(self):
        return ANSI.sub(b'', bytes(self.output)).decode('utf-8', errors='replace')

    def pump(self, seconds=.05):
        if select.select([self.master], [], [], seconds)[0]:
            try:
                self.output.extend(os.read(self.master, 65536))
            except OSError:
                pass

    def until(self, needle, after=0, timeout=30):
        deadline = time.monotonic() + timeout
        while needle not in self.text()[after:]:
            assert self.child.poll() is None, 'Packaged TUI exited unexpectedly'
            assert time.monotonic() < deadline, f'Missing visible TUI text: {needle!r}; tail={self.text()[-2000:]}'
            self.pump()

    def press(self, keys):
        os.write(self.master, keys)
        deadline = time.monotonic() + .15
        while time.monotonic() < deadline:
            self.pump()

    def open_revision(self):
        self.until('Evidence & Costs')
        for _ in range(15):
            start = len(self.text())
            self.press(b'\x1b[B')
            if '› Evidence & Costs' in self.text()[start:]:
                break
        else:
            raise AssertionError('Evidence menu not selectable')
        start = len(self.text())
        self.press(b'\r'); self.until('Forge: revise a prompt', start)
        self.press(b'\x1b[B' * 4)
        start = len(self.text())
        self.press(b'\r'); self.until('FORGE / REVISE A PROMPT', start)
        self.until(source_id, start)

    def resume(self):
        self.open_revision()
        start = len(self.text())
        self.press(b'r'); self.until('RECOVERY DRAFTS', start)
        self.until('tui-rev-', start)
        self.until('Enter records or reads its selection', start)
        start = len(self.text())
        self.press(b'\r'); self.until('Recovery reconciled with canonical daemon evidence.', start)

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill(); self.child.wait(timeout=5)
        os.close(self.master); os.close(self.slave)


def cursor():
    proposals = {path.name.split('.cursor-')[0] for path in (data / 'revision-drafts').glob('*.cursor-*.json')} - old_proposals
    assert len(proposals) == 1, 'One editing action must create one recovery draft'
    paths = sorted((data / 'revision-drafts').glob(next(iter(proposals)) + '.cursor-*.json'))
    return json.loads(paths[-1].read_text())


with tempfile.TemporaryDirectory(prefix='heph-revision-pty-') as private:
    home = Path(private)
    terminal = Terminal(home)
    try:
        terminal.open_revision()
        terminal.press(b'i'); terminal.until('Evaluation ID:')
        start = len(terminal.text())
        terminal.press(source_id.encode() + b'\r'); terminal.until('REVISION PARENT', start)
        terminal.until('Model: claude / sonnet', start)
        terminal.until('Each trial: 300s', start)
        start = len(terminal.text())
        terminal.press(b'e'); terminal.until('REVISION_EDITOR_START', start)
        terminal.press('q café --wait stays in the editor'.encode() + b'\r')
        terminal.until('REVISION_EDITOR_END', start); terminal.until('Hypothesis (1–512 UTF-8 bytes):', start)
        assert (home / 'input.txt').read_text() == 'q café --wait stays in the editor\n'
        terminal.press(b'Explicit uppercase instructions should improve correctness.\r')
        terminal.until('REVIEW PROMPT CHANGE')
        start = len(terminal.text())
        terminal.press(b'y'); terminal.until('Prompt revision recorded.', start)
        value = cursor()
        assert value['phase'] == 'recorded' and value['evaluation_ids'] == []
        snapshot = next((data / 'revision-drafts').glob(value['proposal_id'] + '.record-*.txt'))
        assert snapshot.read_bytes() == body.read_bytes(), 'Recorded snapshot must retain exact CRLF and Unicode'
        assert snapshot.stat().st_mode & 0o777 == 0o400
    finally:
        terminal.close()

    terminal = Terminal(home)
    try:
        terminal.resume()
        terminal.until('Total reported limit:')
        start = len(terminal.text())
        terminal.press(b'y'); terminal.press(b'y')
        terminal.until('RUNNING', start)
        value = cursor()
        assert value['phase'] == 'running' and len(value['evaluation_ids']) == 1
    finally:
        terminal.close()

    terminal = Terminal(home)
    try:
        start = len(terminal.text())
        terminal.resume()
        # Match the recovered phase, not the earlier COMPLETED COMPARISONS
        # heading. Assessment is admitted only after polling finishes.
        terminal.until('COMPLETED:', start, timeout=30)
        start = len(terminal.text())
        terminal.press(b'a'); terminal.until('Metrics assessment verified.', start)
        terminal.until('METRICS PASSED', start)
        terminal.until('Invariant verification and Champion promotion are separate actions.', start)
        value = cursor()
        assert value['phase'] == 'assessed' and len(value['evaluation_ids']) == 1
        assert (home / 'input.txt').read_text().count('\n') == 1, 'Restart must not reopen the editor'
    finally:
        terminal.close()

with sqlite3.connect(f'file:{data}/events.sqlite3?mode=ro', uri=True) as connection:
    events = [(kind, json.loads(payload)) for kind, payload in connection.execute('SELECT event_type,payload FROM events')]
    proposals = [payload for kind, payload in events if kind == 'forge.proposed' and payload.get('proposal_id') == value['proposal_id']]
    assessments = [payload for kind, payload in events if kind == 'forge.assessed' and payload.get('assessment_id') == value['assessment_id']]
    comparisons = [payload for kind, payload in events if kind == 'evaluation.recorded' and payload.get('evaluation_id') == value['evaluation_ids'][0]]
assert len(proposals) == len(assessments) == len(comparisons) == 1, f'Expected one canonical proposal/comparison/assessment: {len(proposals)}/{len(comparisons)}/{len(assessments)}; event kinds={sorted({kind for kind, _ in events})}'
assert hashlib.sha256(original.read_bytes()).hexdigest() == original_hash, 'Original Markdown source changed'
assert assessments[0]['promotion_eligible'] is False and assessments[0]['invariant_gate_verified'] is False
print(json.dumps({'packaged_terminal': '80x24', 'editor_input_verified': True, 'exact_snapshot_verified': True,
                  'source_unchanged': True, 'record_restart_verified': True, 'running_restart_verified': True,
                  'proposal_id': value['proposal_id'], 'evaluation_id': value['evaluation_ids'][0], 'assessment_id': value['assessment_id'],
                  'canonical_proposals': len(proposals), 'canonical_comparisons': len(comparisons), 'canonical_assessments': len(assessments),
                  'promotion_eligible': False}))
