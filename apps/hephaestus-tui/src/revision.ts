import {createHash} from 'node:crypto';
import type {ControlClient} from './client.js';
import type {ArenaJobProgress, Command, ForgeAssessment, ForgeRevision, GenomeProfile, ResponseData, Selection} from './protocol.js';
import {RevisionWorkspace, type RevisionCursor} from './revision-workspace.js';

type Client = Pick<ControlClient, 'request'>;
export type RevisionSource = {selection: Selection; profile: GenomeProfile; prompt: string};
export type RevisionState = {
	cursor: RevisionCursor; parent: GenomeProfile; child?: GenomeProfile;
	job?: ArenaJobProgress; selection?: Selection; assessment?: ForgeAssessment;
	revision?: ForgeRevision;
};

class ApiFailure extends Error {
	constructor(readonly code: string, message: string, readonly rejected = false) { super(`${code}: ${message}`); }
}

function digest(prompt: string): string { return createHash('sha256').update(prompt, 'utf8').digest('hex'); }

/** Compare the settings the operator actually confirmed, including execution limits. */
export function sameExecutionProfile(a: GenomeProfile, b: GenomeProfile): boolean {
	return a.genome.genome_id === b.genome.genome_id && a.world.world_id === b.world.world_id
		&& a.provider === b.provider && a.family === b.family && a.workspace_write === b.workspace_write && a.network === b.network
		&& a.prompt_artifact_id === b.prompt_artifact_id && a.harness_mutation_allowed === b.harness_mutation_allowed
		&& a.output_scoring === b.output_scoring && a.visible_tasks === b.visible_tasks && a.sealed_tasks === b.sealed_tasks
		&& a.paired_trial_wall_millis === b.paired_trial_wall_millis && a.paired_trial_output_bytes === b.paired_trial_output_bytes
		&& a.paired_total_wall_millis === b.paired_total_wall_millis && a.reported_cost_limit_microusd === b.reported_cost_limit_microusd;
}

/** Local cursors are recovery hints. Canonical daemon receipts establish every outcome. */
export class RevisionController {
	private working = false;
	constructor(readonly client: Client, readonly workspace: RevisionWorkspace, readonly signal?: AbortSignal) {}

	private async exclusive<T>(operation: () => Promise<T>): Promise<T> {
		if (this.working) throw new Error('Revision operation is already in progress');
		this.working = true;
		try { return await operation(); } finally { this.working = false; }
	}

	private async request<T extends ResponseData['type']>(command: Command, type: T): Promise<Extract<ResponseData, {type: T}>> {
		const response = await this.client.request(command, this.signal);
		if (response.error) throw new ApiFailure(response.error.code, response.error.message, response.error.rejected);
		if (response.data?.type !== type) throw new Error('Unexpected daemon response; reconcile before retrying');
		return response.data as Extract<ResponseData, {type: T}>;
	}

	private async profile(id: string): Promise<GenomeProfile> {
		const {profile} = await this.request({command: 'genome_profile', genome_id: id}, 'genome_profile');
		if (profile.genome.genome_id !== id || profile.genome.world_id !== profile.world.world_id) throw new Error('Genome profile identity differs');
		return profile;
	}

	private async prompt(id: string): Promise<string> {
		const result = await this.request({command: 'genome_prompt', genome_id: id}, 'genome_prompt');
		if (result.genome_id !== id) throw new Error('Prompt belongs to a different Genome');
		return result.prompt;
	}

	async source(evaluationId: string): Promise<RevisionSource> {
		return this.exclusive(async () => {
			const {selection} = await this.request({command: 'arena_select', evaluation_id: evaluationId}, 'selection');
			if (selection.evaluation_id !== evaluationId) throw new Error('Source selection identity differs');
			const profile = await this.profile(selection.candidate_genome_id);
			if (profile.world.world_id !== selection.world_id) throw new Error('Source selection World differs');
			if (!['claude', 'codex'].includes(profile.provider) || !profile.prompt_artifact_id) throw new Error('Choose a hosted Markdown candidate with a verified prompt');
			if (!profile.harness_mutation_allowed) throw new Error('This World does not allow prompt revisions; choose a World with harness mutation authority');
			return {selection, profile, prompt: await this.prompt(profile.genome.genome_id)};
		});
	}

	async create(source: RevisionSource): Promise<RevisionCursor> {
		return this.exclusive(async () => this.workspace.create({
			source_evaluation_id: source.selection.evaluation_id, selection_event_id: source.selection.event_id,
			selection_event_hash: source.selection.event_hash,
			world_id: source.selection.world_id, parent_genome_id: source.selection.candidate_genome_id,
		}, source.prompt));
	}

	private async verified(value: RevisionCursor, revision: ForgeRevision): Promise<RevisionState> {
		if (revision.proposal_id !== value.proposal_id || revision.selection_event_id !== value.selection_event_id
			|| revision.selection_event_hash !== value.selection_event_hash
			|| revision.evaluation_id !== value.source_evaluation_id || revision.world_id !== value.world_id
			|| revision.parent_genome_id !== value.parent_genome_id || revision.hypothesis !== value.hypothesis
			|| revision.promotion_eligible !== false || revision.child.world_id !== value.world_id
			|| revision.child.parent_ids.length !== 1 || revision.child.parent_ids[0] !== value.parent_genome_id
			|| (value.child_genome_id !== null && revision.child.genome_id !== value.child_genome_id)) throw new Error('Recorded proposal differs from the recovery cursor');
		const [parent, child, before, after] = await Promise.all([
			this.profile(value.parent_genome_id), this.profile(revision.child.genome_id),
			this.prompt(value.parent_genome_id), this.prompt(revision.child.genome_id),
		]);
		if (digest(before) !== value.before_sha256 || digest(after) !== value.snapshot_sha256
			|| parent.prompt_artifact_id !== revision.prompt_artifact_before || child.prompt_artifact_id !== revision.prompt_artifact_after
			|| parent.world.world_id !== value.world_id || child.world.world_id !== value.world_id
			|| parent.provider !== child.provider || parent.family !== child.family
			|| parent.workspace_write !== child.workspace_write || parent.network !== child.network) throw new Error('Recorded prompt bytes or immutable execution settings differ');
		return {cursor: value, parent, child, revision};
	}

	private async proposal(value: RevisionCursor): Promise<RevisionState | undefined> {
		let revision: ForgeRevision;
		try {
			({revision} = await this.request({command: 'genome_proposal_show', proposal_id: value.proposal_id}, 'forge_revision'));
		} catch (error) {
			if (error instanceof ApiFailure && error.code === 'not_found') return undefined;
			throw error;
		}
		return this.verified(value, revision);
	}

	async record(value: RevisionCursor, hypothesis?: string, reviewedSha256?: string): Promise<RevisionState> {
		return this.exclusive(async () => {
			let cursor = await this.workspace.load(value.proposal_id);
			if (cursor.cursor_revision !== value.cursor_revision) throw new Error('Revision changed; reload before recording');
			if (cursor.phase === 'editing') {
				if (hypothesis === undefined || reviewedSha256 === undefined) throw new Error('Review the changed prompt and enter a hypothesis first');
				cursor = await this.workspace.snapshot({...cursor, hypothesis}, reviewedSha256);
			}
			if (cursor.phase !== 'record_unknown') throw new Error('Revision is not awaiting recording');
			let result = await this.proposal(cursor);
			if (!result) {
				const path = await this.workspace.retryPath(cursor);
				let revision: ForgeRevision;
				try {
					({revision} = await this.request({command: 'genome_revise', proposal_id: cursor.proposal_id,
						selection_event_id: cursor.selection_event_id, parent_genome_id: cursor.parent_genome_id,
						prompt_path: path, hypothesis: cursor.hypothesis}, 'forge_revision'));
				} catch (error) {
					if (error instanceof ApiFailure && error.rejected) await this.workspace.save({...cursor, phase: 'record_rejected'});
					throw error;
				}
				result = await this.verified(cursor, revision);
			}
			const saved = await this.workspace.save({...cursor, phase: 'recorded', child_genome_id: result.child!.genome.genome_id});
			return {...result, cursor: saved};
		});
	}

	private jobMatches(value: RevisionCursor, job: ArenaJobProgress): void {
		if (job.evaluation_id !== value.evaluation_ids.at(-1) || job.parent_genome_id !== value.parent_genome_id
			|| job.candidate_genome_id !== value.child_genome_id) throw new Error('Comparison belongs to a different proposal');
		if (job.state === 'succeeded' && !job.evaluation) throw new Error('Successful comparison has no evaluation receipt');
	}

	private async job(value: RevisionCursor): Promise<ArenaJobProgress | undefined> {
		const id = value.evaluation_ids.at(-1);
		if (!id) return undefined;
		try {
			const result = await this.request({command: 'job_status', job_id: id}, 'arena_job');
			this.jobMatches(value, result.job);
			return result.job;
		} catch (error) {
			if (error instanceof ApiFailure && error.code === 'not_found') return undefined;
			throw error;
		}
	}

	private async withJob(state: RevisionState, job: ArenaJobProgress): Promise<RevisionState> {
		this.jobMatches(state.cursor, job);
		let cursor = state.cursor;
		if (['compare_unknown', 'running'].includes(cursor.phase)) {
			const phase = job.state === 'succeeded' ? 'completed'
				: ['failed', 'interrupted'].includes(job.state) ? 'compare_failed' : 'running';
			if (phase !== cursor.phase) cursor = await this.workspace.save({...cursor, phase});
		} else if (['completed', 'assessment_unknown', 'assessed'].includes(cursor.phase) && job.state !== 'succeeded') {
			throw new Error('Completed recovery cursor disagrees with the daemon');
		} else if (cursor.phase === 'compare_failed' && !['failed', 'interrupted'].includes(job.state)) {
			throw new Error('Failed recovery cursor disagrees with the daemon');
		} else if (cursor.phase === 'compare_rejected') throw new Error('Rejected comparison unexpectedly exists; inspect its canonical history');
		return {...state, cursor, job};
	}

	async reconcile(proposalId: string): Promise<RevisionState> {
		return this.exclusive(async () => {
			let cursor = await this.workspace.load(proposalId);
			if (cursor.phase === 'editing') {
				const parent = await this.profile(cursor.parent_genome_id);
				const {selection} = await this.request({command: 'arena_selection_show', evaluation_id: cursor.source_evaluation_id}, 'selection');
				if (selection.event_id !== cursor.selection_event_id || selection.world_id !== cursor.world_id
					|| selection.event_hash !== cursor.selection_event_hash
					|| selection.candidate_genome_id !== cursor.parent_genome_id || parent.world.world_id !== cursor.world_id
					|| digest(await this.prompt(cursor.parent_genome_id)) !== cursor.before_sha256) throw new Error('Draft source binding differs from the canonical selection');
				return {cursor, parent};
			}
			let result = await this.proposal(cursor);
			if (!result) {
				if (!['record_unknown', 'record_rejected'].includes(cursor.phase)) throw new Error('Recorded proposal is missing; execution refused');
				return {cursor, parent: await this.profile(cursor.parent_genome_id)};
			}
			if (cursor.phase === 'record_rejected') throw new Error('Rejected proposal unexpectedly exists; inspect its canonical history');
			if (cursor.phase === 'record_unknown') {
				cursor = await this.workspace.save({...cursor, phase: 'recorded', child_genome_id: result.child!.genome.genome_id});
				result = {...result, cursor};
			}
			if (!cursor.evaluation_ids.length) return result;
			const job = await this.job(cursor);
			if (job) return this.withJob(result, job);
			if (!['compare_unknown', 'compare_rejected'].includes(cursor.phase)) throw new Error('Recorded comparison is missing; reconcile its canonical history');
			return result;
		});
	}

	/** Poll only the already-bound comparison; full recovery remains an explicit action. */
	async poll(value: RevisionState, isCurrent: () => boolean = () => true): Promise<RevisionState> {
		if (!value.revision) throw new Error('Reconcile the proposal before polling its comparison');
		const cursor = await this.workspace.load(value.cursor.proposal_id);
		if (cursor.evaluation_ids.at(-1) !== value.cursor.evaluation_ids.at(-1)) throw new Error('Comparison changed; refresh before confirming cancellation');
		const job = await this.job(cursor);
		if (!isCurrent()) return value;
		if (!job) {
			if (cursor.phase === 'compare_unknown') return {...value, cursor};
			throw new Error('Recorded comparison is missing; reconcile its canonical history');
		}
		return this.withJob({...value, cursor}, job);
	}

	/** Called only after an explicit confirmation of the displayed execution profile. */
	async compare(value: RevisionCursor, confirmed: GenomeProfile): Promise<RevisionState> {
		return this.exclusive(async () => {
			const cursor = await this.workspace.load(value.proposal_id);
			if (cursor.cursor_revision !== value.cursor_revision) throw new Error('Revision changed; reload before comparing');
			if (!['recorded', 'compare_unknown', 'compare_failed', 'compare_rejected'].includes(cursor.phase)) throw new Error('Only an unstarted or failed comparison can start an attempt');
			const state = await this.proposal(cursor);
			if (!state?.child) throw new Error('Verified proposal is required before comparing');
			if (cursor.phase === 'compare_rejected' && await this.job(cursor)) throw new Error('Rejected comparison unexpectedly exists; reconcile before starting another attempt');
			if (cursor.phase === 'compare_unknown') {
				const existing = await this.job(cursor);
				if (existing) return this.withJob(state, existing);
			}
			if (!sameExecutionProfile(state.child, confirmed)) throw new Error('Execution settings changed; refresh and confirm the current limits');
			const next = cursor.phase === 'compare_unknown' ? cursor : await this.workspace.newAttempt(cursor);
			let job: ArenaJobProgress;
			try {
				({job} = await this.request({command: 'evaluate_pair_confirmed', evaluation_id: next.evaluation_ids.at(-1)!,
					parent_genome_id: next.parent_genome_id, candidate_genome_id: next.child_genome_id!, expected_profile: confirmed}, 'arena_job'));
			} catch (error) {
				if (error instanceof ApiFailure && error.rejected) await this.workspace.save({...next, phase: 'compare_rejected'});
				throw error;
			}
			return this.withJob({...state, cursor: next}, job);
		});
	}

	async assess(value: RevisionCursor): Promise<RevisionState> {
		return this.exclusive(async () => {
			let cursor = await this.workspace.load(value.proposal_id);
			if (!['completed', 'assessment_unknown', 'assessed'].includes(cursor.phase)) throw new Error('A successful comparison is required for assessment');
			const state = await this.proposal(cursor);
			const job = await this.job(cursor);
			if (!state || job?.state !== 'succeeded') throw new Error('Verified proposal and successful comparison are required');
			const {selection} = await this.request({command: 'arena_select', evaluation_id: job.evaluation_id}, 'selection');
			if (selection.evaluation_id !== job.evaluation_id || selection.world_id !== cursor.world_id
				|| selection.parent_genome_id !== cursor.parent_genome_id || selection.candidate_genome_id !== cursor.child_genome_id) throw new Error('Assessment selection belongs to a different comparison');
			if (cursor.phase === 'completed') cursor = await this.workspace.save({...cursor, phase: 'assessment_unknown'});
			const {assessment} = await this.request({command: 'genome_assess', assessment_id: cursor.assessment_id,
				proposal_id: cursor.proposal_id, selection_event_id: selection.event_id}, 'forge_assessment');
			if (assessment.assessment_id !== cursor.assessment_id || assessment.proposal_id !== cursor.proposal_id
				|| assessment.proposal_event_id !== state.revision!.event_id || assessment.proposal_event_hash !== state.revision!.event_hash
				|| assessment.selection_event_hash !== selection.event_hash || assessment.evaluation_event_id !== selection.evaluation_event_id
				|| assessment.evaluation_event_hash !== selection.evaluation_event_hash
				|| assessment.selection_event_id !== selection.event_id || assessment.evaluation_id !== job.evaluation_id
				|| assessment.world_id !== cursor.world_id || assessment.parent_genome_id !== cursor.parent_genome_id
				|| assessment.child_genome_id !== cursor.child_genome_id || assessment.promotion_eligible !== false
				|| assessment.invariant_gate_verified !== false
				|| (assessment.outcome === 'metrics_passed') !== selection.metrics_eligible) throw new Error('Assessment receipt differs from the verified comparison');
			if (cursor.phase !== 'assessed') cursor = await this.workspace.save({...cursor, phase: 'assessed'});
			return {...state, cursor, job, selection, assessment};
		});
	}

	/** Request cancellation; the returned job remains active until the daemon confirms a terminal state. */
	async cancel(value: RevisionState): Promise<RevisionState> {
		return this.exclusive(async () => {
			const cursor = await this.workspace.load(value.cursor.proposal_id);
			if (!value.job || value.job.evaluation_id !== cursor.evaluation_ids.at(-1)) throw new Error('Comparison changed; refresh before confirming cancellation');
			this.jobMatches(cursor, value.job);
			const state = {...value, cursor};
			const job = await this.job(cursor);
			if (!job) throw new Error('Verified comparison is required before cancelling');
			if (['succeeded', 'failed', 'interrupted'].includes(job.state)) return this.withJob(state, job);
			const result = await this.request({command: 'job_kill', job_id: job.evaluation_id}, 'arena_job');
			return this.withJob(state, result.job);
		});
	}
}
