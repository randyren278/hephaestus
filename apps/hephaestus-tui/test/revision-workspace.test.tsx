import test from 'node:test';
import assert from 'node:assert/strict';
import {chmod, link, lstat, mkdtemp, readFile, readdir, rm, symlink, utimes, writeFile} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {MAX_PROMPT_BYTES, RevisionWorkspace} from '../src/revision-workspace.js';

const source = {source_evaluation_id: 'comparison', selection_event_id: 'selection:comparison', world_id: 'world:abc', parent_genome_id: 'genome:abc'};
const before = '\uFEFFBefore\r\ncafé 🙂  ';
const after = '\uFEFFAfter\r\ncafé 🙂  ';

async function fixture(run: (workspace: RevisionWorkspace, root: string) => Promise<void>): Promise<void> {
	const root = await mkdtemp(join(tmpdir(), 'heph-revision-'));
	try {
		await chmod(root, 0o700);
		await run(new RevisionWorkspace(root), root);
	} finally {
		await rm(root, {recursive: true, force: true});
	}
}

test('a revision gets stable reserved-prefix-free IDs and private exact UTF-8 source copies', async () => fixture(async workspace => {
	const value = await workspace.create(source, before);
	assert.match(value.proposal_id, /^tui-rev-[a-f0-9-]{36}$/);
	assert.match(value.assessment_id, /^tui-assess-[a-f0-9-]{36}$/);
	assert.deepEqual(await workspace.load(value.proposal_id), value);
	assert.deepEqual(await workspace.list(), {revisions: [value], errors: [], truncated: false});
	assert.equal((await lstat(workspace.directory)).mode & 0o777, 0o700);
	assert.equal((await lstat(workspace.path(value.proposal_id, 'draft'))).mode & 0o777, 0o600);
	assert.equal((await lstat(workspace.path(value.proposal_id, 'before'))).mode & 0o777, 0o400);
	assert.equal((await lstat(workspace.path(value.proposal_id, 'cursor'))).mode & 0o777, 0o600);
	assert.deepEqual(await readFile(workspace.path(value.proposal_id, 'draft')), Buffer.from(before));
	assert.deepEqual(await readFile(workspace.path(value.proposal_id, 'before')), Buffer.from(before));
	await assert.rejects(workspace.review(value), /unchanged/);
}));

test('review preserves BOM, CRLF, Unicode and trailing spaces and fixes editor replacement permissions', async () => fixture(async workspace => {
	const value = await workspace.create(source, before);
	const path = workspace.path(value.proposal_id, 'draft');
	await rm(path);
	await writeFile(path, after, {mode: 0o644});
	const reviewed = await workspace.review(value);
	assert.deepEqual(reviewed.bytes, Buffer.from(after));
	assert.equal(reviewed.change.before_bytes, Buffer.byteLength(before));
	assert.equal(reviewed.change.after_bytes, Buffer.byteLength(after));
	assert.equal(reviewed.change.before_lines, 2);
	assert.equal(reviewed.change.after_lines, 2);
	assert.equal(reviewed.change.before_crlf, 1);
	assert.equal(reviewed.change.after_crlf, 1);
	assert.equal(reviewed.change.whitespace_only, false);
	assert.equal((await lstat(path)).mode & 0o777, 0o600);
}));

test('a reviewed snapshot is private, retryable across restart and independent of later draft edits', async () => fixture(async workspace => {
	let value = await workspace.create(source, before);
	await writeFile(workspace.path(value.proposal_id, 'draft'), after);
	value = {...value, hypothesis: 'Explicit instructions should improve the task.'};
	const reviewed = await workspace.review(value);
	value = await workspace.snapshot(value, reviewed.change.sha256);
	assert.equal(value.phase, 'record_unknown');
	const path = await workspace.retryPath(value);
	assert.equal((await lstat(path)).mode & 0o777, 0o400);
	assert.deepEqual(await readFile(path), Buffer.from(after));
	await writeFile(workspace.path(value.proposal_id, 'draft'), 'A later unrecorded edit');
	const restarted = new RevisionWorkspace(workspace.dataDir);
	assert.deepEqual(await restarted.load(value.proposal_id), value);
	assert.equal(await restarted.retryPath(value), path);
	assert.deepEqual(await readFile(path), Buffer.from(after));
	value = await restarted.save({...value, child_genome_id: 'genome:child', phase: 'recorded'});
	value = await restarted.newAttempt(value);
	assert.equal(value.phase, 'compare_unknown');
	assert.equal(value.evaluation_ids.length, 1);
	assert.match(value.evaluation_ids[0]!, /^tui-arena-[a-f0-9-]{36}$/);
	value = await restarted.save({...value, phase: 'completed'});
	const next = await restarted.newAttempt(value);
	assert.equal(next.evaluation_ids.length, 2);
	assert.notEqual(next.evaluation_ids[0], next.evaluation_ids[1]);
	assert.deepEqual(await restarted.load(value.proposal_id), next);
}));

test('snapshot refuses a draft changed since review and detects later snapshot tampering', async () => fixture(async workspace => {
	const value = {...await workspace.create(source, before), hypothesis: 'Changing the instruction should improve the task.'};
	const draft = workspace.path(value.proposal_id, 'draft');
	await writeFile(draft, after);
	const reviewed = await workspace.review(value);
	await writeFile(draft, 'Changed after review');
	await assert.rejects(workspace.snapshot(value, reviewed.change.sha256), /changed after review/);
	await writeFile(draft, after);
	const recorded = await workspace.snapshot(value, reviewed.change.sha256);
	await assert.rejects(workspace.snapshot(recorded, reviewed.change.sha256), /Snapshot already recorded/);
	const path = await workspace.retryPath(recorded);
	await chmod(path, 0o600);
	await writeFile(path, 'Tampered');
	await assert.rejects(workspace.retryPath(recorded), /snapshot changed/);
}));

test('invalid prompt bodies reject without recording, and the exact maximum size is accepted', async () => fixture(async workspace => {
	const value = await workspace.create(source, before);
	const path = workspace.path(value.proposal_id, 'draft');
	for (const [bytes, error] of [[Buffer.from(' \r\n\t'), /blank/], [Buffer.from([0xff]), /valid UTF-8/], [Buffer.alloc(MAX_PROMPT_BYTES + 1, 65), /byte limit/]] as const) {
		await writeFile(path, bytes);
		await assert.rejects(workspace.review(value), error);
	}
	await writeFile(path, Buffer.alloc(MAX_PROMPT_BYTES, 65));
	assert.equal((await workspace.review(value)).bytes.length, MAX_PROMPT_BYTES);
	assert.equal((await workspace.load(value.proposal_id)).phase, 'editing');
}));

test('workspace and files reject symlinks, and reads verify the original prompt digest', async () => fixture(async (workspace, root) => {
	await symlink(root, workspace.directory);
	await assert.rejects(workspace.create(source, before), /owner-only directory/);
	await rm(workspace.directory);
	const value = await workspace.create(source, before);
	const draft = workspace.path(value.proposal_id, 'draft');
	await rm(draft);
	await symlink(workspace.path(value.proposal_id, 'before'), draft);
	await assert.rejects(workspace.review(value), {code: 'ELOOP'});
	await rm(draft);
	await writeFile(draft, after, {mode: 0o600});
	const original = workspace.path(value.proposal_id, 'before');
	await chmod(original, 0o600);
	await writeFile(original, 'Changed original');
	await assert.rejects(workspace.review(value), /Original prompt copy changed/);
}));

test('unsafe directories, traversing IDs and malformed recovery cursors reject', async () => fixture(async (workspace, root) => {
	await chmod(root, 0o755);
	await assert.rejects(workspace.create(source, before), /owner-only directory/);
	await chmod(root, 0o700);
	assert.throws(() => workspace.path('../outside', 'draft'), /Invalid revision workspace identity/);
	const value = await workspace.create(source, before);
	await assert.rejects(workspace.save({...value, hypothesis: '🙂'.repeat(129)}), /Invalid revision recovery cursor/);
	await assert.rejects(workspace.save({...value, evaluation_ids: ['evolve-unsafe']}), /Invalid revision recovery cursor/);
	const path = workspace.path(value.proposal_id, 'cursor');
	await writeFile(path, '{');
	await assert.rejects(workspace.load(value.proposal_id), /unreadable/);
	await writeFile(path, JSON.stringify({...value, proposal_id: value.assessment_id}));
	await assert.rejects(workspace.load(value.proposal_id), /invalid/);
	assert.equal((await readdir(workspace.directory)).some(name => name.endsWith('.tmp')), false);
}));

test('review calls out whitespace-only changes without normalizing the recorded bytes', async () => fixture(async workspace => {
	const value = await workspace.create(source, 'Return café.\n');
	await writeFile(workspace.path(value.proposal_id, 'draft'), '  Return café.\r\n');
	const reviewed = await workspace.review(value);
	assert.equal(reviewed.change.whitespace_only, true);
	assert.deepEqual(reviewed.bytes, Buffer.from('  Return café.\r\n'));
}));

test('corrupt cursors and foreign JSON files do not hide healthy recoveries, which list newest first', async () => fixture(async workspace => {
	const first = await workspace.create(source, before);
	const second = await workspace.create(source, before);
	await writeFile(join(workspace.directory, 'foreign.json'), '{');
	const broken = await workspace.create(source, before);
	await writeFile(workspace.path(broken.proposal_id, 'cursor'), '{');
	const updated = await workspace.save({...first, hypothesis: 'A later edit'});
	await utimes(workspace.path(updated.proposal_id, 'cursor', updated.cursor_revision), 3, 3);
	await utimes(workspace.path(second.proposal_id, 'cursor'), 2, 2);
	const listed = await workspace.list();
	assert.deepEqual(listed.revisions, [updated, second]);
	assert.equal(listed.errors.length, 1);
	assert.equal(listed.errors[0]!.proposal_id, broken.proposal_id);
	assert.match(listed.errors[0]!.message, /unreadable/);
}));

test('stale and concurrent cursor updates cannot overwrite saved IDs or bindings', async () => fixture(async workspace => {
	const original = await workspace.create(source, before);
	const results = await Promise.allSettled([
		workspace.save({...original, hypothesis: 'First edit'}),
		new RevisionWorkspace(workspace.dataDir).save({...original, hypothesis: 'Second edit'}),
	]);
	assert.equal(results.filter(result => result.status === 'fulfilled').length, 1);
	assert.equal(results.filter(result => result.status === 'rejected').length, 1);
	const latest = await workspace.load(original.proposal_id);
	assert.equal(latest.cursor_revision, 1);
	await assert.rejects(workspace.save(original), /cursor changed/);
	await assert.rejects(workspace.save({...latest, parent_genome_id: 'genome:other'}), /source binding cannot change/);
	await assert.rejects(workspace.newAttempt(latest), /Finish or reconcile/);
}));

test('phases require their bindings and prevent losing comparison IDs or rolling back a recorded snapshot', async () => fixture(async workspace => {
	let value = {...await workspace.create(source, before), hypothesis: 'Uppercase should improve correctness.'};
	await writeFile(workspace.path(value.proposal_id, 'draft'), after);
	value = await workspace.snapshot(value, (await workspace.review(value)).change.sha256);
	await assert.rejects(workspace.save({...value, snapshot_sha256: null}), /Invalid revision recovery cursor/);
	await assert.rejects(workspace.save({...value, phase: 'editing', snapshot_sha256: null}), /phase transition/);
	await assert.rejects(workspace.save({...value, hypothesis: 'Changed after send'}), /Recorded hypothesis cannot change/);
	value = await workspace.save({...value, phase: 'recorded', child_genome_id: 'genome:child'});
	const recorded = value;
	value = await workspace.newAttempt(value);
	await assert.rejects(workspace.save(recorded), /cursor changed/);
	await assert.rejects(workspace.save({...value, evaluation_ids: []}), /Invalid revision recovery cursor/);
	await assert.rejects(workspace.save({...value, evaluation_ids: [`tui-arena-00000000-0000-0000-0000-000000000000`]}), /comparison identities/);
	await assert.rejects(workspace.newAttempt(value), /Finish or reconcile/);
}));

test('hard-linked drafts, FIFOs and symlinked retry snapshots reject without changing other files', async () => fixture(async (workspace, root) => {
	const value = {...await workspace.create(source, before), hypothesis: 'Change the task instruction.'};
	const draft = workspace.path(value.proposal_id, 'draft');
	const other = join(root, 'other.txt');
	await writeFile(other, after, {mode: 0o644});
	await rm(draft);
	await link(other, draft);
	await assert.rejects(workspace.review(value), /must not be hard-linked/);
	assert.equal((await lstat(other)).mode & 0o777, 0o644);
	await rm(draft);
	assert.equal(spawnSync('mkfifo', [draft]).status, 0);
	await assert.rejects(workspace.review(value), /owned regular file/);
	await rm(draft);
	await writeFile(draft, after, {mode: 0o600});
	const recorded = await workspace.snapshot(value, (await workspace.review(value)).change.sha256);
	const path = await workspace.retryPath(recorded);
	await rm(path);
	await symlink(other, path);
	await assert.rejects(workspace.retryPath(recorded), {code: 'ELOOP'});
}));

test('a revision permits 100 distinct attempts and then reports the exact limit', async () => fixture(async workspace => {
	let value = {...await workspace.create(source, before), hypothesis: 'Change task instruction.'};
	await writeFile(workspace.path(value.proposal_id, 'draft'), after);
	value = await workspace.snapshot(value, (await workspace.review(value)).change.sha256);
	value = await workspace.save({...value, phase: 'recorded', child_genome_id: 'genome:child'});
	for (let i = 0; i < 100; i++) {
		value = await workspace.newAttempt(value);
		value = await workspace.save({...value, phase: 'completed'});
	}
	assert.equal(new Set(value.evaluation_ids).size, 100);
	await assert.rejects(workspace.newAttempt(value), /100 comparison attempt limit/);
	assert.deepEqual(await workspace.load(value.proposal_id), value);
}));

test('leftover unpublished snapshots do not lock a draft to bytes that were never sent', async () => fixture(async workspace => {
	let value = {...await workspace.create(source, before), hypothesis: 'Change task instruction.'};
	const draft = workspace.path(value.proposal_id, 'draft');
	await writeFile(draft, after);
	const first = await workspace.review(value);
	const leftover = workspace.snapshotPath(value.proposal_id, first.change.sha256);
	await writeFile(leftover, first.bytes, {mode: 0o400});
	// Simulate a crash after snapshot publication but before the cursor update.
	await writeFile(draft, 'A different reviewed instruction.\r\ncafé ');
	const second = await workspace.review(value);
	assert.notEqual(second.change.sha256, first.change.sha256);
	value = await workspace.snapshot(value, second.change.sha256);
	assert.equal(value.snapshot_sha256, second.change.sha256);
	assert.deepEqual(await readFile(await workspace.retryPath(value)), second.bytes);
	assert.deepEqual(await readFile(leftover), first.bytes);
}));

test('definite rejections have explicit recovery states and controls in source IDs reject', async () => fixture(async workspace => {
	await assert.rejects(workspace.create({...source, parent_genome_id: 'genome:\u009bunsafe'}, before), /Invalid revision source/);
	let value = {...await workspace.create(source, before), hypothesis: 'Change task instruction.'};
	await writeFile(workspace.path(value.proposal_id, 'draft'), after);
	value = await workspace.snapshot(value, (await workspace.review(value)).change.sha256);
	const rejected = await workspace.save({...value, phase: 'record_rejected'});
	assert.equal((await workspace.load(value.proposal_id)).phase, 'record_rejected');
	await assert.rejects(workspace.newAttempt(rejected), /Finish or reconcile/);
	value = {...await workspace.create(source, before), hypothesis: 'A separate revision.'};
	await writeFile(workspace.path(value.proposal_id, 'draft'), after);
	value = await workspace.snapshot(value, (await workspace.review(value)).change.sha256);
	value = await workspace.save({...value, phase: 'recorded', child_genome_id: 'genome:child'});
	value = await workspace.newAttempt(value);
	value = await workspace.save({...value, phase: 'compare_rejected'});
	const firstId = value.evaluation_ids[0];
	value = await workspace.newAttempt(value);
	assert.equal(value.evaluation_ids.length, 2);
	assert.equal(value.evaluation_ids[0], firstId);
	assert.notEqual(value.evaluation_ids[1], firstId);
}));

test('an unpublished matching snapshot resumes while a corrupt copy refuses to overwrite', async () => fixture(async workspace => {
	const value = {...await workspace.create(source, before), hypothesis: 'Change task instruction.'};
	const draft = workspace.path(value.proposal_id, 'draft');
	await writeFile(draft, after);
	const reviewed = await workspace.review(value);
	const path = workspace.snapshotPath(value.proposal_id, reviewed.change.sha256);
	await writeFile(path, 'A corrupt fixed-name copy', {mode: 0o400});
	await assert.rejects(workspace.snapshot(value, reviewed.change.sha256), /snapshot differs/);
	assert.equal((await workspace.load(value.proposal_id)).phase, 'editing');
	assert.equal(await readFile(path, 'utf8'), 'A corrupt fixed-name copy');
	await rm(path);
	await writeFile(path, reviewed.bytes, {mode: 0o400});
	const resumed = await workspace.snapshot(value, reviewed.change.sha256);
	assert.equal(resumed.phase, 'record_unknown');
	assert.equal(await workspace.retryPath(resumed), path);
}));

test('failed or interrupted running comparisons preserve their IDs and can retry without assessment', async () => fixture(async workspace => {
	let value = {...await workspace.create(source, before), hypothesis: 'Change task instruction.'};
	await writeFile(workspace.path(value.proposal_id, 'draft'), after);
	value = await workspace.snapshot(value, (await workspace.review(value)).change.sha256);
	value = await workspace.save({...value, phase: 'recorded', child_genome_id: 'genome:child'});
	value = await workspace.newAttempt(value);
	value = await workspace.save({...value, phase: 'running'});
	value = await workspace.save({...value, phase: 'compare_failed'});
	await assert.rejects(workspace.save({...value, phase: 'assessment_unknown'}), /phase transition/);
	const firstId = value.evaluation_ids[0];
	value = await workspace.newAttempt(value);
	assert.equal(value.evaluation_ids[0], firstId);
	assert.notEqual(value.evaluation_ids[1], firstId);
	// A terminal failure found immediately on resume need not pass through running.
	value = await workspace.save({...value, phase: 'compare_failed'});
	assert.equal((await workspace.load(value.proposal_id)).phase, 'compare_failed');
}));
