import {createHash, randomUUID} from 'node:crypto';
import {constants, promises as fs} from 'node:fs';
import {join, resolve} from 'node:path';

export const MAX_PROMPT_BYTES = 1024 * 1024;
const MAX_CURSOR_BYTES = 16 * 1024;
const PHASES = ['editing', 'record_unknown', 'record_rejected', 'recorded', 'compare_unknown', 'compare_rejected', 'compare_failed', 'running', 'completed', 'assessment_unknown', 'assessed'] as const;
export type RevisionPhase = typeof PHASES[number];

/** A local recovery cursor; only daemon responses establish recorded evidence. */
export type RevisionCursor = {
	schema_version: 1;
	cursor_revision: number;
	proposal_id: string;
	assessment_id: string;
	source_evaluation_id: string;
	selection_event_id: string;
	world_id: string;
	parent_genome_id: string;
	before_sha256: string;
	hypothesis: string;
	phase: RevisionPhase;
	snapshot_sha256: string | null;
	child_genome_id: string | null;
	evaluation_ids: string[];
};

export type RevisionListing = {revisions: RevisionCursor[]; errors: {proposal_id: string; message: string}[]; truncated: boolean};

export type PromptChange = {
	before_bytes: number; after_bytes: number;
	before_lines: number; after_lines: number;
	before_crlf: number; after_crlf: number;
	whitespace_only: boolean; sha256: string;
};

function sha256(bytes: Uint8Array): string {
	return createHash('sha256').update(bytes).digest('hex');
}

function localId(value: unknown, kind?: 'rev' | 'assess' | 'arena'): value is string {
	return typeof value === 'string' && /^tui-(rev|assess|arena)-[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(value)
		&& (kind === undefined || value.startsWith(`tui-${kind}-`));
}

function identity(value: unknown): value is string {
	return typeof value === 'string' && value.length > 0 && value.length <= 512 && !/[\u0000-\u0020\u007f-\u009f]/u.test(value);
}

function digest(value: unknown): value is string {
	return typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
}

function cursor(value: unknown): value is RevisionCursor {
	if (value === null || typeof value !== 'object' || Array.isArray(value)) return false;
	const v = value as Record<string, unknown>;
	const valid = v['schema_version'] === 1 && typeof v['cursor_revision'] === 'number'
		&& Number.isSafeInteger(v['cursor_revision']) && v['cursor_revision'] >= 0 && v['cursor_revision'] < 999999
		&& localId(v['proposal_id'], 'rev') && localId(v['assessment_id'], 'assess')
		&& identity(v['source_evaluation_id']) && identity(v['selection_event_id'])
		&& identity(v['world_id']) && identity(v['parent_genome_id']) && digest(v['before_sha256'])
		&& typeof v['hypothesis'] === 'string' && Buffer.byteLength(v['hypothesis']) <= 512
		&& !/[\u0000-\u001f\u007f-\u009f]/u.test(v['hypothesis'])
		&& PHASES.includes(v['phase'] as RevisionPhase)
		&& (v['snapshot_sha256'] === null || digest(v['snapshot_sha256']))
		&& (v['child_genome_id'] === null || identity(v['child_genome_id']))
		&& Array.isArray(v['evaluation_ids']) && v['evaluation_ids'].length <= 100
		&& v['evaluation_ids'].every(id => localId(id, 'arena'))
		&& new Set(v['evaluation_ids']).size === v['evaluation_ids'].length;
	if (!valid) return false;
	const v2 = value as RevisionCursor;
	if (v2.phase === 'editing') return v2.snapshot_sha256 === null && v2.child_genome_id === null && v2.evaluation_ids.length === 0;
	if (!v2.snapshot_sha256 || !v2.hypothesis.trim()) return false;
	if (v2.phase === 'record_unknown' || v2.phase === 'record_rejected') return v2.child_genome_id === null && v2.evaluation_ids.length === 0;
	if (!v2.child_genome_id) return false;
	return v2.phase === 'recorded' ? v2.evaluation_ids.length === 0 : v2.evaluation_ids.length > 0;
}

async function privateDirectory(path: string): Promise<void> {
	const stat = await fs.lstat(path);
	if (!stat.isDirectory() || stat.uid !== process.getuid?.() || (stat.mode & 0o077) !== 0) {
		throw new Error('Revision workspace must be an owner-only directory (0700)');
	}
}

async function readBounded(path: string, limit: number, editable = false): Promise<Buffer> {
	const file = await fs.open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
	try {
		const stat = await file.stat();
		if (!stat.isFile() || stat.uid !== process.getuid?.()) throw new Error('Revision file must be an owned regular file');
		if (editable && stat.nlink !== 1) throw new Error('Editable draft must not be hard-linked to another file');
		if (stat.size > limit) throw new Error('Revision file exceeds its byte limit');
		// Editors often replace an inode using the operator's ordinary umask.
		if (editable) await file.chmod(0o600);
		else if ((stat.mode & 0o077) !== 0) throw new Error('Revision file must be owner-only');
		const bytes = Buffer.alloc(limit + 1);
		let length = 0;
		while (length < bytes.length) {
			const chunk = await file.read(bytes, length, bytes.length - length, null);
			if (chunk.bytesRead === 0) break;
			length += chunk.bytesRead;
		}
		if (length > limit) throw new Error('Revision file exceeds its byte limit');
		return bytes.subarray(0, length);
	} finally {
		await file.close();
	}
}

function promptText(bytes: Buffer): string {
	let text: string;
	try {
		// ignoreBOM preserves a leading U+FEFF as part of the exact prompt.
		text = new TextDecoder('utf-8', {fatal: true, ignoreBOM: true}).decode(bytes);
	} catch {
		throw new Error('Prompt must be valid UTF-8');
	}
	if (!text.trim()) throw new Error('Prompt must not be blank');
	return text;
}

async function createFile(path: string, bytes: Buffer, mode: number): Promise<void> {
	const file = await fs.open(path, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, mode);
	let created: Awaited<ReturnType<typeof file.stat>> | undefined;
	try {
		created = await file.stat();
		await file.writeFile(bytes);
		await file.sync();
	} catch (error) {
		// Remove only the incomplete inode created by this call, never an
		// existing snapshot or a file replaced by another writer.
		const current = await fs.lstat(path).catch(() => undefined);
		if (created && current && created.dev === current.dev && created.ino === current.ino) await fs.unlink(path);
		throw error;
	} finally {
		await file.close();
	}
}

export class RevisionWorkspace {
	readonly directory: string;
	constructor(readonly dataDir: string) {
		this.directory = join(resolve(dataDir), 'revision-drafts');
	}

	private async prepare(): Promise<void> {
		await privateDirectory(resolve(this.dataDir));
		try {
			await fs.mkdir(this.directory, {mode: 0o700});
		} catch (error) {
			if (!(error && typeof error === 'object' && 'code' in error && error.code === 'EEXIST')) throw error;
		}
		await privateDirectory(this.directory);
	}

	path(proposalId: string, kind: 'draft' | 'before' | 'cursor', revision = 0): string {
		if (!localId(proposalId, 'rev') || !Number.isSafeInteger(revision) || revision < 0 || revision >= 999999) throw new Error('Invalid revision workspace identity');
		return join(this.directory, `${proposalId}.${kind === 'cursor' ? `cursor-${String(revision).padStart(6, '0')}.json` : `${kind}.txt`}`);
	}

	snapshotPath(proposalId: string, hash: string): string {
		this.path(proposalId, 'draft');
		if (!digest(hash)) throw new Error('Invalid revision snapshot digest');
		return join(this.directory, `${proposalId}.record-${hash}.txt`);
	}

	async create(source: Pick<RevisionCursor, 'source_evaluation_id' | 'selection_event_id' | 'world_id' | 'parent_genome_id'>, verifiedPrompt: string): Promise<RevisionCursor> {
		const bytes = Buffer.from(verifiedPrompt, 'utf8');
		if (bytes.length > MAX_PROMPT_BYTES) throw new Error('Prompt exceeds 1 MiB');
		promptText(bytes);
		const value: RevisionCursor = {
			schema_version: 1, cursor_revision: 0, proposal_id: `tui-rev-${randomUUID()}`, assessment_id: `tui-assess-${randomUUID()}`,
			...source, before_sha256: sha256(bytes), hypothesis: '', phase: 'editing',
			snapshot_sha256: null, child_genome_id: null, evaluation_ids: [],
		};
		if (!cursor(value)) throw new Error('Invalid revision source');
		await this.prepare();
		await createFile(this.path(value.proposal_id, 'before'), bytes, 0o400);
		await createFile(this.path(value.proposal_id, 'draft'), bytes, 0o600);
		await this.publish(value);
		return value;
	}

	private async publish(value: RevisionCursor): Promise<void> {
		if (!cursor(value)) throw new Error('Invalid revision recovery cursor');
		await this.prepare();
		const bytes = Buffer.from(JSON.stringify(value) + '\n');
		if (bytes.length > MAX_CURSOR_BYTES) throw new Error('Revision recovery cursor exceeds its byte limit');
		await this.publishBytes(this.path(value.proposal_id, 'cursor', value.cursor_revision), bytes, 0o600);
	}

	private async publishBytes(destination: string, bytes: Buffer, mode: number): Promise<void> {
		const temporary = join(this.directory, `.publish-${randomUUID()}.tmp`);
		try {
			await createFile(temporary, bytes, mode);
			// A link publishes fully written bytes without replacing an existing
			// revision. Concurrent writers of the same next revision cannot win twice.
			await fs.link(temporary, destination);
		} finally {
			await fs.rm(temporary, {force: true});
		}
		const directory = await fs.open(this.directory, constants.O_RDONLY | constants.O_NOFOLLOW);
		try { await directory.sync(); } finally { await directory.close(); }
	}

	async save(value: RevisionCursor): Promise<RevisionCursor> {
		if (!cursor(value)) throw new Error('Invalid revision recovery cursor');
		const current = await this.load(value.proposal_id);
		if (current.cursor_revision !== value.cursor_revision) throw new Error('Revision cursor changed; reload before continuing');
		for (const key of ['assessment_id', 'source_evaluation_id', 'selection_event_id', 'world_id', 'parent_genome_id', 'before_sha256'] as const) {
			if (current[key] !== value[key]) throw new Error('Revision source binding cannot change');
		}
		const transitions: Record<RevisionPhase, RevisionPhase[]> = {
			editing: ['editing', 'record_unknown'], record_unknown: ['record_unknown', 'recorded', 'record_rejected'], record_rejected: ['record_rejected'],
			recorded: ['recorded', 'compare_unknown'], compare_unknown: ['compare_unknown', 'running', 'completed', 'compare_rejected', 'compare_failed'], compare_rejected: ['compare_rejected', 'compare_unknown'],
			compare_failed: ['compare_failed', 'compare_unknown'], running: ['running', 'completed', 'compare_failed'], completed: ['completed', 'compare_unknown', 'assessment_unknown'],
			assessment_unknown: ['assessment_unknown', 'assessed'], assessed: ['assessed'],
		};
		if (!transitions[current.phase].includes(value.phase)) throw new Error('Invalid revision phase transition');
		if (current.phase !== 'editing' && current.hypothesis !== value.hypothesis) throw new Error('Recorded hypothesis cannot change');
		if (current.snapshot_sha256 !== value.snapshot_sha256 && !(current.phase === 'editing' && value.phase === 'record_unknown')) {
			throw new Error('Recorded snapshot binding cannot change');
		}
		if (current.child_genome_id !== value.child_genome_id && !(current.phase === 'record_unknown' && value.phase === 'recorded')) {
			throw new Error('Recorded child binding cannot change');
		}
		const added = value.evaluation_ids.length - current.evaluation_ids.length;
		if (current.evaluation_ids.some((id, index) => value.evaluation_ids[index] !== id)
			|| (added !== 0 && !(added === 1 && ['recorded', 'completed', 'compare_rejected', 'compare_failed'].includes(current.phase) && value.phase === 'compare_unknown'))) {
			throw new Error('Recorded comparison identities cannot be lost or replaced');
		}
		if (value.phase === 'compare_unknown' && current.phase !== 'compare_unknown' && added !== 1) throw new Error('New comparison requires a new identity');
		const updated = {...value, cursor_revision: value.cursor_revision + 1};
		try { await this.publish(updated); }
		catch (error) {
			if (error && typeof error === 'object' && 'code' in error && error.code === 'EEXIST') throw new Error('Revision cursor changed; reload before continuing');
			throw error;
		}
		return updated;
	}

	async load(proposalId: string): Promise<RevisionCursor> {
		await this.prepare();
		const prefix = `${proposalId}.cursor-`;
		this.path(proposalId, 'cursor');
		const names = (await fs.readdir(this.directory)).filter(name => name.startsWith(prefix) && /^\d{6}\.json$/.test(name.slice(prefix.length))).sort();
		const latest = names.at(-1);
		if (!latest) throw new Error('Revision recovery cursor is unavailable');
		const revision = Number(latest.slice(prefix.length, -5));
		const bytes = await readBounded(this.path(proposalId, 'cursor', revision), MAX_CURSOR_BYTES);
		let value: unknown;
		try { value = JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(bytes)); }
		catch { throw new Error('Revision recovery cursor is unreadable'); }
		if (!cursor(value) || value.proposal_id !== proposalId || value.cursor_revision !== revision) throw new Error('Revision recovery cursor is invalid');
		return value;
	}

	async list(): Promise<RevisionListing> {
		await this.prepare();
		const files = new Map<string, string>();
		for (const name of (await fs.readdir(this.directory)).sort()) {
			const match = /^(.*)\.cursor-\d{6}\.json$/.exec(name);
			if (match && localId(match[1], 'rev')) files.set(match[1], name);
		}
		const sorted = await Promise.all([...files].map(async ([proposal_id, name]) => {
			try { return {proposal_id, modified: (await fs.lstat(join(this.directory, name))).mtimeMs}; }
			catch { return {proposal_id, modified: 0}; }
		}));
		sorted.sort((a, b) => b.modified - a.modified || a.proposal_id.localeCompare(b.proposal_id));
		const revisions: RevisionCursor[] = [];
		const errors: RevisionListing['errors'] = [];
		for (const entry of sorted.slice(0, 200)) {
			try { revisions.push(await this.load(entry.proposal_id)); }
			catch (error) { errors.push({proposal_id: entry.proposal_id, message: error instanceof Error ? error.message : 'Unreadable revision cursor'}); }
		}
		return {revisions, errors, truncated: sorted.length > 200};
	}

	async review(value: RevisionCursor): Promise<{bytes: Buffer; change: PromptChange}> {
		await this.prepare();
		const before = await readBounded(this.path(value.proposal_id, 'before'), MAX_PROMPT_BYTES);
		if (sha256(before) !== value.before_sha256) throw new Error('Original prompt copy changed; reopen the verified source');
		const bytes = await readBounded(this.path(value.proposal_id, 'draft'), MAX_PROMPT_BYTES, true);
		const afterText = promptText(bytes);
		const beforeText = promptText(before);
		if (before.equals(bytes)) throw new Error('Prompt is unchanged; edit and save it before recording');
		return {bytes, change: {
			before_bytes: before.length, after_bytes: bytes.length,
			before_lines: beforeText.split('\n').length, after_lines: afterText.split('\n').length,
			before_crlf: beforeText.match(/\r\n/g)?.length ?? 0, after_crlf: afterText.match(/\r\n/g)?.length ?? 0,
			whitespace_only: beforeText.replace(/\s/g, '') === afterText.replace(/\s/g, ''), sha256: sha256(bytes),
		}};
	}

	/** Keep a separate immutable retry body; never freeze or overwrite the editable draft. */
	async snapshot(value: RevisionCursor, reviewedSha256: string): Promise<RevisionCursor> {
		const current = await this.load(value.proposal_id);
		if (current.cursor_revision !== value.cursor_revision) throw new Error('Revision cursor changed; reload before continuing');
		if (current.phase !== 'editing' || value.phase !== 'editing') throw new Error('Snapshot already recorded; use its retry body');
		if (!value.hypothesis.trim() || Buffer.byteLength(value.hypothesis) > 512 || /[\u0000-\u001f\u007f-\u009f]/u.test(value.hypothesis)) throw new Error('Hypothesis must be 1 to 512 printable UTF-8 bytes');
		const reviewed = await this.review(value);
		if (reviewed.change.sha256 !== reviewedSha256) throw new Error('Draft changed after review; review it again');
		const path = this.snapshotPath(value.proposal_id, reviewedSha256);
		try {
			await this.publishBytes(path, reviewed.bytes, 0o400);
		} catch (error) {
			if (!(error && typeof error === 'object' && 'code' in error && error.code === 'EEXIST')) throw error;
			if (sha256(await readBounded(path, MAX_PROMPT_BYTES)) !== reviewedSha256) throw new Error('Recorded snapshot differs; resume with its original bytes');
		}
		const updated = {...value, snapshot_sha256: reviewedSha256, phase: 'record_unknown' as const};
		return this.save(updated);
	}

	async retryPath(value: RevisionCursor): Promise<string> {
		await this.prepare();
		if (!value.snapshot_sha256) throw new Error('Revision has no recorded snapshot');
		const path = this.snapshotPath(value.proposal_id, value.snapshot_sha256);
		const bytes = await readBounded(path, MAX_PROMPT_BYTES);
		promptText(bytes);
		if (sha256(bytes) !== value.snapshot_sha256) throw new Error('Recorded prompt snapshot changed; retry refused');
		return path;
	}

	async newAttempt(value: RevisionCursor): Promise<RevisionCursor> {
		if (!['recorded', 'completed', 'compare_rejected', 'compare_failed'].includes(value.phase)) throw new Error('Finish or reconcile the previous operation before starting a comparison');
		if (value.evaluation_ids.length >= 100) throw new Error('Revision has reached its 100 comparison attempt limit');
		const updated = {...value, evaluation_ids: [...value.evaluation_ids, `tui-arena-${randomUUID()}`], phase: 'compare_unknown' as const};
		return this.save(updated);
	}
}
