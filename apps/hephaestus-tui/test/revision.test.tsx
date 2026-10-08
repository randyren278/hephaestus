import test from 'node:test';
import assert from 'node:assert/strict';
import {chmod, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import type {ApiResponse, ArenaJobProgress, Command, ForgeAssessment, ForgeRevision, GenomeProfile, ResponseData, Selection} from '../src/protocol.js';
import {RevisionController} from '../src/revision.js';
import {RevisionWorkspace, type RevisionCursor} from '../src/revision-workspace.js';

const before = '\uFEFFReturn input unchanged.\r\ncafé 🙂  ';
const after = '\uFEFFReturn input uppercase.\r\ncafé 🙂  ';
const hypothesis = 'Explicit uppercase instructions should improve correctness.';
const parent: GenomeProfile = {
	genome: {genome_id: 'candidate', name: 'Candidate', world_id: 'world', artifact_id: 'canonical-parent', parent_ids: ['baseline']},
	world: {world_id: 'world', name: 'Task pack', artifact_id: 'canonical-world'}, provider: 'claude', family: 'sonnet',
	workspace_write: false, network: false, prompt_artifact_id: 'blake3-before', harness_mutation_allowed: true, output_scoring: 'trimmed',
	visible_tasks: 1, sealed_tasks: 1, paired_trial_wall_millis: 300_000, paired_trial_output_bytes: 1_048_576,
	paired_total_wall_millis: 1_210_000, reported_cost_limit_microusd: '1000000',
};
const child: GenomeProfile = {...parent, genome: {genome_id: 'child', name: 'Revision', world_id: 'world', artifact_id: 'canonical-child', parent_ids: ['candidate']}, prompt_artifact_id: 'blake3-after'};
const selection: Selection = {
	evaluation_id: 'source', world_id: 'world', parent_genome_id: 'baseline', candidate_genome_id: 'candidate', event_id: 'selection:source', event_hash: 'hash-source',
	evaluation_event_id: 'evaluated:source', evaluation_event_hash: 'hash-evaluation',
	metrics_eligible: false, estimate_bps: 0, lower_bps: 0, upper_bps: 0, parent_cost_microusd: 0, candidate_cost_microusd: 0,
	parent_latency_millis: 1, candidate_latency_millis: 2, invariant_gate_verified: false, promotion_eligible: false,
};

class Daemon {
	commands: Command[] = [];
	revision: ForgeRevision | undefined;
	jobs = new Map<string, ArenaJobProgress>();
	assessments: ForgeAssessment[] = [];
	loseRecord = false; loseCompare = false; loseAssess = false;
	recordError: string | undefined;
	recordRejected = false;
	childProfile = structuredClone(child);
	childPrompt = after;
	jobOverride: ArenaJobProgress | undefined;
	assessmentWrong = false;
	childPromptMissing = false;
	selectionBusy = false;
	recordGate: Promise<void> | undefined;

	async request(command: Command): Promise<ApiResponse> {
		this.commands.push(structuredClone(command));
		const ok = (data: ResponseData): ApiResponse => ({version: 1, request_id: 'test', data});
		const missing = (): ApiResponse => ({version: 1, request_id: 'test', error: {code: 'not_found', message: 'canonical record not found'}});
		if (command.command === 'genome_prompt' && command.genome_id === 'child' && this.childPromptMissing) return missing();
		if (command.command === 'arena_select' && command.evaluation_id !== 'source' && this.selectionBusy) return {version: 1, request_id: 'test', error: {code: 'busy', message: 'Another job is active'}};
		switch (command.command) {
			case 'arena_selection_show':
			case 'arena_select': return ok({type: 'selection', selection: command.evaluation_id === 'source' ? structuredClone(selection) : {...selection,
					evaluation_id: command.evaluation_id, parent_genome_id: 'candidate', candidate_genome_id: 'child', event_id: `selection:${command.evaluation_id}`,
					metrics_eligible: true, estimate_bps: 10000, lower_bps: 10000, upper_bps: 10000}});
			case 'genome_profile': return ok({type: 'genome_profile', profile: structuredClone(command.genome_id === 'candidate' ? parent : this.childProfile)});
			case 'genome_prompt': return ok({type: 'genome_prompt', genome_id: command.genome_id, prompt: command.genome_id === 'candidate' ? before : this.childPrompt});
			case 'genome_proposal_show': return this.revision ? ok({type: 'forge_revision', revision: structuredClone(this.revision)}) : missing();
			case 'genome_revise': {
				if (this.recordGate) await this.recordGate;
				if (this.recordError) return {version: 1, request_id: 'test', error: {code: this.recordError, message: 'evolution is frozen'}};
				if (this.recordRejected) return {version: 1, request_id: 'test', error: {code: 'invalid_request', message: 'Immutable source refused', rejected: true}};
				assert.equal(await readFile(command.prompt_path, 'utf8'), after);
				this.revision = {proposal_id: command.proposal_id, selection_event_id: command.selection_event_id, evaluation_id: 'source', world_id: 'world',
					parent_genome_id: command.parent_genome_id, child: structuredClone(child.genome), hypothesis: command.hypothesis,
					prompt_artifact_before: 'blake3-before', prompt_artifact_after: 'blake3-after', event_id: `forge:${command.proposal_id}`, promotion_eligible: false,
					selection_event_hash: selection.event_hash, event_hash: 'hash-proposal'};
				if (this.loseRecord) { this.loseRecord = false; throw new Error('daemon request timed out'); }
				return ok({type: 'forge_revision', revision: structuredClone(this.revision)});
			}
			case 'evaluate_pair_confirmed': {
				assert.ok(this.revision, 'comparison requires a proposal');
				assert.ok(!this.jobs.has(command.evaluation_id), 'controller must reconcile an existing job instead of resubmitting');
				const job: ArenaJobProgress = {evaluation_id: command.evaluation_id, parent_genome_id: command.parent_genome_id, candidate_genome_id: command.candidate_genome_id,
					state: 'running', phase: 'parent_trials', completed_trials: 0, total_trials: 4};
				this.jobs.set(command.evaluation_id, job);
				if (this.loseCompare) { this.loseCompare = false; throw new Error('daemon request timed out'); }
				return ok({type: 'arena_job', job: structuredClone(job)});
			}
			case 'job_status': {
				const job = this.jobOverride ?? this.jobs.get(command.job_id);
				return job ? ok({type: 'arena_job', job: structuredClone(job)}) : missing();
			}
			case 'job_kill': {
				const job = this.jobs.get(command.job_id)!;
				job.state = 'cancellation_requested';
				return ok({type: 'arena_job', job: structuredClone(job)});
			}
			case 'genome_assess': {
				const evaluationId = command.selection_event_id.slice('selection:'.length);
				const assessment: ForgeAssessment = {assessment_id: command.assessment_id, proposal_id: command.proposal_id,
					selection_event_id: command.selection_event_id, evaluation_id: evaluationId, world_id: 'world', parent_genome_id: 'candidate', child_genome_id: 'child',
					outcome: this.assessmentWrong ? 'metrics_rejected' : 'metrics_passed', event_id: `assessed:${command.assessment_id}`, promotion_eligible: false, invariant_gate_verified: false,
					proposal_event_id: this.revision!.event_id, proposal_event_hash: this.revision!.event_hash,
					selection_event_hash: selection.event_hash, evaluation_event_id: selection.evaluation_event_id, evaluation_event_hash: selection.evaluation_event_hash};
				this.assessments.push(assessment);
				if (this.loseAssess) { this.loseAssess = false; throw new Error('daemon request timed out'); }
				return ok({type: 'forge_assessment', assessment});
			}
			default: throw new Error(`Unexpected command ${command.command}`);
		}
	}

	finish(id: string, state: 'succeeded' | 'failed' | 'interrupted' = 'succeeded'): void {
		const job = this.jobs.get(id)!;
		job.state = state; job.phase = 'terminal'; job.completed_trials = 4;
		if (state === 'succeeded') job.evaluation = {parent_visible_correct: 0, candidate_visible_correct: 1, visible_total: 1};
	}
}

async function fixture(run: (controller: RevisionController, daemon: Daemon, workspace: RevisionWorkspace, draft: RevisionCursor) => Promise<void>): Promise<void> {
	const root = await mkdtemp(join(tmpdir(), 'heph-controller-'));
	try {
		await chmod(root, 0o700);
		const daemon = new Daemon(); const workspace = new RevisionWorkspace(root);
		const controller = new RevisionController(daemon, workspace);
		const source = await controller.source('source');
		const draft = await controller.create(source);
		await writeFile(workspace.path(draft.proposal_id, 'draft'), after);
		await run(controller, daemon, workspace, draft);
	} finally { await rm(root, {recursive: true, force: true}); }
}

async function record(controller: RevisionController, workspace: RevisionWorkspace, draft: RevisionCursor) {
	const reviewed = await workspace.review(draft);
	return controller.record(draft, hypothesis, reviewed.change.sha256);
}

test('explicit record, comparison and metrics assessment preserve exact bytes and never promote', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	assert.equal(recorded.cursor.phase, 'recorded'); assert.equal(daemon.jobs.size, 0);
	assert.equal(await readFile(workspace.path(draft.proposal_id, 'before'), 'utf8'), before);
	const compared = await controller.compare(recorded.cursor, recorded.child!);
	assert.equal(compared.cursor.phase, 'running'); assert.equal(daemon.jobs.size, 1); assert.equal(daemon.assessments.length, 0);
	await assert.rejects(controller.assess(compared.cursor), /successful comparison/);
	daemon.finish(compared.job!.evaluation_id);
	const complete = await controller.reconcile(draft.proposal_id);
	assert.equal(complete.cursor.phase, 'completed'); assert.equal(daemon.assessments.length, 0);
	const assessed = await controller.assess(complete.cursor);
	assert.equal(assessed.cursor.phase, 'assessed'); assert.equal(assessed.assessment!.outcome, 'metrics_passed');
	assert.equal(assessed.assessment!.promotion_eligible, false); assert.equal(assessed.assessment!.invariant_gate_verified, false);
	assert.ok(daemon.commands.every(command => !['champion_promote', 'canary_start', 'arena_invariants', 'genome_register'].includes(command.command)));
}));

test('lost record response reconciles after restart without another proposal or changed draft bytes', async () => fixture(async (controller, daemon, workspace, draft) => {
	daemon.loseRecord = true;
	await assert.rejects(record(controller, workspace, draft), /timed out/);
	const unknown = await workspace.load(draft.proposal_id);
	assert.equal(unknown.phase, 'record_unknown');
	await writeFile(workspace.path(draft.proposal_id, 'draft'), 'A later edit is not the recorded body');
	const restarted = new RevisionController(daemon, new RevisionWorkspace(workspace.dataDir));
	const recovered = await restarted.record(unknown);
	assert.equal(recovered.cursor.phase, 'recorded');
	assert.equal(daemon.commands.filter(command => command.command === 'genome_revise').length, 1);
	assert.equal(await readFile(await workspace.retryPath(recovered.cursor), 'utf8'), after);
}));

test('lost comparison response preserves the pending ID and reconciles without another launch', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	daemon.loseCompare = true;
	await assert.rejects(controller.compare(recorded.cursor, recorded.child!), /timed out/);
	const pending = await workspace.load(draft.proposal_id);
	assert.equal(pending.phase, 'compare_unknown'); assert.equal(pending.evaluation_ids.length, 1);
	const restarted = new RevisionController(daemon, workspace);
	const recovered = await restarted.compare(pending, recorded.child!);
	assert.equal(recovered.cursor.phase, 'running'); assert.deepEqual(recovered.cursor.evaluation_ids, pending.evaluation_ids);
	assert.equal(daemon.commands.filter(command => command.command === 'evaluate_pair_confirmed').length, 1);
}));

test('an unadmitted comparison retries the same ID and a terminal failure starts a fresh attempt', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const pending = await workspace.newAttempt(recorded.cursor);
	const unchanged = await controller.reconcile(draft.proposal_id);
	assert.equal(unchanged.cursor.phase, 'compare_unknown'); assert.equal(daemon.jobs.size, 0);
	const running = await controller.compare(pending, recorded.child!);
	assert.equal(running.job!.evaluation_id, pending.evaluation_ids[0]);
	daemon.finish(running.job!.evaluation_id, 'failed');
	const failed = await controller.reconcile(draft.proposal_id);
	assert.equal(failed.cursor.phase, 'compare_failed');
	await assert.rejects(controller.assess(failed.cursor), /successful comparison/);
	const retried = await controller.compare(failed.cursor, failed.child!);
	assert.equal(retried.cursor.evaluation_ids.length, 2); assert.notEqual(retried.job!.evaluation_id, running.job!.evaluation_id);
	assert.equal(daemon.jobs.size, 2);
}));

test('lost assessment response retries the same assessment and selection binding after restart', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const running = await controller.compare(recorded.cursor, recorded.child!);
	daemon.finish(running.job!.evaluation_id);
	const complete = await controller.reconcile(draft.proposal_id);
	daemon.loseAssess = true;
	await assert.rejects(controller.assess(complete.cursor), /timed out/);
	const pending = await workspace.load(draft.proposal_id); assert.equal(pending.phase, 'assessment_unknown');
	const restarted = new RevisionController(daemon, workspace);
	assert.equal((await restarted.assess(pending)).cursor.phase, 'assessed');
	assert.equal(daemon.assessments.length, 2);
	assert.deepEqual(daemon.assessments[0], daemon.assessments[1]);
	assert.equal(daemon.jobs.size, 1);
}));

test('freeze and busy failures retain a retryable snapshot and do not imply recording succeeded', async () => fixture(async (controller, daemon, workspace, draft) => {
	for (const code of ['invalid_request', 'busy', 'internal']) {
		daemon.recordError = code;
		const cursor = await workspace.load(draft.proposal_id);
		await assert.rejects(cursor.phase === 'editing' ? record(controller, workspace, cursor) : controller.record(cursor), new RegExp(code));
		assert.equal((await workspace.load(draft.proposal_id)).phase, 'record_unknown'); assert.equal(daemon.revision, undefined);
	}
	daemon.recordError = undefined;
	assert.equal((await controller.record(await workspace.load(draft.proposal_id))).cursor.phase, 'recorded');
	const commands = daemon.commands.filter(command => command.command === 'genome_revise');
	assert.equal(new Set(commands.map(command => command.proposal_id)).size, 1);
	assert.equal(new Set(commands.map(command => command.prompt_path)).size, 1);
}));

test('a double record action cannot publish or submit two proposals', async () => fixture(async (controller, daemon, workspace, draft) => {
	let release!: () => void;
	daemon.recordGate = new Promise<void>(resolve => { release = resolve; });
	const first = record(controller, workspace, draft);
	while (!daemon.commands.some(command => command.command === 'genome_revise')) await new Promise(resolve => setTimeout(resolve, 5));
	await assert.rejects(record(controller, workspace, draft), /already in progress/);
	release(); assert.equal((await first).cursor.phase, 'recorded');
	assert.equal(daemon.commands.filter(command => command.command === 'genome_revise').length, 1);
}));

test('changed execution limits require a fresh confirmation before allocating an attempt ID', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	daemon.childProfile.paired_trial_wall_millis = 120_000;
	await assert.rejects(controller.compare(recorded.cursor, recorded.child!), /settings changed/);
	assert.deepEqual((await workspace.load(draft.proposal_id)).evaluation_ids, []); assert.equal(daemon.jobs.size, 0);
	const fresh = await controller.reconcile(draft.proposal_id);
	assert.equal((await controller.compare(fresh.cursor, fresh.child!)).cursor.phase, 'running');
}));

test('registered child metadata alone cannot replace a missing canonical Forge proposal', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	daemon.revision = undefined;
	await assert.rejects(controller.reconcile(draft.proposal_id), /proposal is missing/);
	await assert.rejects(controller.compare(recorded.cursor, recorded.child!), /Verified proposal/);
	assert.equal(daemon.jobs.size, 0);
}));

for (const tamper of ['prompt', 'family', 'network', 'artifact', 'source'] as const) {
	test(`canonical revision recovery rejects changed ${tamper} evidence`, async () => fixture(async (controller, daemon, workspace, draft) => {
		await record(controller, workspace, draft);
		if (tamper === 'prompt') daemon.childPrompt = after + 'changed';
		if (tamper === 'family') daemon.childProfile.family = 'opus';
		if (tamper === 'network') daemon.childProfile.network = true;
		if (tamper === 'artifact') daemon.childProfile.prompt_artifact_id = 'different-artifact';
		if (tamper === 'source') daemon.revision!.selection_event_id = 'different-selection';
		await assert.rejects(controller.reconcile(draft.proposal_id), /differs|differ/);
		assert.equal(daemon.jobs.size, 0);
	}));
}

test('comparison pair substitution and successful jobs without receipts fail closed', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const running = await controller.compare(recorded.cursor, recorded.child!);
	daemon.jobOverride = {...running.job!, candidate_genome_id: 'foreign'};
	await assert.rejects(controller.reconcile(draft.proposal_id), /different proposal/);
	daemon.jobOverride = {...running.job!, state: 'succeeded'};
	await assert.rejects(controller.reconcile(draft.proposal_id), /no evaluation receipt/);
	assert.equal((await workspace.load(draft.proposal_id)).phase, 'running');
}));

test('assessment outcome must match verified metrics and cancellation waits for a terminal job', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const running = await controller.compare(recorded.cursor, recorded.child!);
	const cancelling = await controller.cancel(running);
	assert.equal(cancelling.cursor.phase, 'running'); assert.equal(cancelling.job!.state, 'cancellation_requested');
	daemon.finish(running.job!.evaluation_id, 'interrupted');
	const interrupted = await controller.reconcile(draft.proposal_id); assert.equal(interrupted.cursor.phase, 'compare_failed');
	const retried = await controller.compare(interrupted.cursor, interrupted.child!);
	daemon.finish(retried.job!.evaluation_id);
	const complete = await controller.reconcile(draft.proposal_id);
	daemon.assessmentWrong = true;
	await assert.rejects(controller.assess(complete.cursor), /Assessment receipt differs/);
	assert.equal((await workspace.load(draft.proposal_id)).phase, 'assessment_unknown');
}));

test('an explicit admission refusal retains the draft without treating a temporary freeze as permanent', async () => fixture(async (controller, daemon, workspace, draft) => {
	daemon.recordRejected = true;
	await assert.rejects(record(controller, workspace, draft), /Immutable source refused/);
	const refused = await workspace.load(draft.proposal_id);
	assert.equal(refused.phase, 'record_rejected'); assert.equal(daemon.revision, undefined); assert.equal(daemon.jobs.size, 0);
	assert.equal(await readFile(workspace.path(draft.proposal_id, 'draft'), 'utf8'), after);
	assert.equal((await controller.reconcile(draft.proposal_id)).cursor.phase, 'record_rejected');
}));

test('progress polling reads only the bound job and never replays the proposal or locks cancellation behind it', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const running = await controller.compare(recorded.cursor, recorded.child!);
	const start = daemon.commands.length;
	const polled = await controller.poll(running);
	assert.equal(polled.cursor.phase, 'running');
	assert.deepEqual(daemon.commands.slice(start).map(command => command.command), ['job_status']);
	daemon.revision = undefined;
	const cancelStart = daemon.commands.length;
	const cancelled = await controller.cancel(polled);
	assert.equal(cancelled.job!.state, 'cancellation_requested');
	assert.deepEqual(daemon.commands.slice(cancelStart).map(command => command.command), ['job_status', 'job_kill']);
}));

test('successful comparisons cannot be rerun and invalidated background polls do not publish a phase', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const running = await controller.compare(recorded.cursor, recorded.child!);
	daemon.finish(running.job!.evaluation_id);
	assert.equal((await controller.poll(running, () => false)).cursor.phase, 'running');
	assert.equal((await workspace.load(draft.proposal_id)).phase, 'running');
	const completed = await controller.poll(running);
	assert.equal(completed.cursor.phase, 'completed');
	await assert.rejects(controller.compare(completed.cursor, completed.child!), /Only an unstarted or failed/);
	await assert.rejects(workspace.newAttempt(completed.cursor), /Only an unstarted or failed/);
	assert.equal(daemon.jobs.size, 1);
}));

test('missing nested prompt evidence cannot make an existing proposal appear absent', async () => fixture(async (controller, daemon, workspace, draft) => {
	daemon.loseRecord = true;
	await assert.rejects(record(controller, workspace, draft), /timed out/);
	daemon.childPromptMissing = true;
	const pending = await workspace.load(draft.proposal_id);
	await assert.rejects(controller.record(pending), /not_found/);
	assert.equal((await workspace.load(draft.proposal_id)).phase, 'record_unknown');
	assert.equal(daemon.commands.filter(command => command.command === 'genome_revise').length, 1);
}));

test('a selection refusal before assessment leaves the successful comparison available for a later assessment', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const running = await controller.compare(recorded.cursor, recorded.child!);
	daemon.finish(running.job!.evaluation_id);
	const completed = await controller.poll(running);
	daemon.selectionBusy = true;
	await assert.rejects(controller.assess(completed.cursor), /busy/);
	assert.equal((await workspace.load(draft.proposal_id)).phase, 'completed');
	assert.equal(daemon.assessments.length, 0);
	daemon.selectionBusy = false;
	assert.equal((await controller.assess(completed.cursor)).cursor.phase, 'assessed');
}));

test('a stale cancel confirmation cannot cancel a newer attempt started by another console', async () => fixture(async (controller, daemon, workspace, draft) => {
	const recorded = await record(controller, workspace, draft);
	const first = await controller.compare(recorded.cursor, recorded.child!);
	daemon.finish(first.job!.evaluation_id, 'failed');
	const other = new RevisionController(daemon, workspace);
	const failed = await other.reconcile(draft.proposal_id);
	const second = await other.compare(failed.cursor, failed.child!);
	const beforeCancel = daemon.commands.length;
	await assert.rejects(controller.cancel(first), /Comparison changed/);
	assert.ok(daemon.commands.slice(beforeCancel).every(command => command.command !== 'job_kill'));
	assert.equal(daemon.jobs.get(second.job!.evaluation_id)!.state, 'running');
}));
