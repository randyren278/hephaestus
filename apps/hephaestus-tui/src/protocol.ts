export type Command =
	| {command: 'status'}
	| {command: 'freeze'}
	| {command: 'unfreeze'}
	| {command: 'kill_all'}
	| {command: 'job_status'; job_id: string}
	| {command: 'job_kill'; job_id: string}
	| {command: 'genome_list'}
	| {command: 'genome_show'; genome_id: string}
	| {command: 'world_list'}
	| {command: 'genome_prompt'; genome_id: string}
	| {command: 'champion_show'; world_id: string}
	| {command: 'champion_rollback'; transition_id: string; world_id: string; reason: string};

export type ApiRequest = {version: 1; request_id: string; token: string; command: Command};
export type JobState = 'admitted' | 'running' | 'cancellation_requested' | 'succeeded' | 'failed' | 'interrupted';
export type JobTerminal = 'succeeded' | 'failed' | 'cancelled' | 'interrupted';
export type Job = {
	job_id: string; genome_id: string; run_id: string; source_revision: string; world_id: string;
	task_id: string; input_commitment: string; seed: number; environment_id: string;
	budget: Record<string, unknown>; state: JobState; terminal: JobTerminal | null;
};
export type ArenaJobPhase = 'preparing' | 'parent_trials' | 'candidate_trials' | 'scoring' | 'committing' | 'terminal';
export type ArenaJobProgress = {
	evaluation_id: string; parent_genome_id: string; candidate_genome_id: string;
	state: JobState; phase: ArenaJobPhase; completed_trials: number; total_trials: number;
	evaluation?: {parent_visible_correct: number; candidate_visible_correct: number; visible_total: number};
};
export type Genome = {genome_id: string; name: string; world_id: string; artifact_id: string; parent_ids: string[]};
export type World = {world_id: string; name: string; artifact_id: string};
export type ChampionTransitionKind = 'seeded' | 'promoted' | 'rolled_back';
export type ChampionTransition = {
	transition_id: string; world_id: string; kind: ChampionTransitionKind; champion_genome_id: string;
	previous_champion_genome_id: string | null; reason: string | null; event_id: string; sequence: number;
};
export type Champion = {
	world_id: string; champion_genome_id: string | null; standby_genome_ids: string[];
	quarantined_genome_ids: string[]; transitions: ChampionTransition[];
};
export const MAX_LIST_ITEMS = 10_000;
export type RunListEntry = {
	run_id: string; job_id: string | null; genome_id: string; world_id: string | null;
	state: JobState; completion_reason: string | null; latency_millis: number | null; actual_cost_microusd: number | null;
};
export type EvaluationSelectionSummary = {
	metrics_eligible: boolean; estimate_bps: number; lower_bps: number; upper_bps: number;
	parent_cost_microusd: number; candidate_cost_microusd: number;
	parent_latency_millis: number; candidate_latency_millis: number;
	invariant_gate_verified: boolean; promotion_eligible: boolean;
};
export type EvaluationInvariantSummary = {
	total_checks: number; total_candidate_violations: number; total_paired_regressions: number;
	maximum_regressions: number; regressions_within_budget: boolean; candidate_contract_satisfied: boolean;
};
export type EvaluationForgeSummary = {assessment_id: string; outcome: 'metrics_passed' | 'metrics_rejected'};
export type EvaluationListEntry = {
	evaluation: {
		evaluation_id: string; world_id: string; parent_genome_id: string; candidate_genome_id: string;
		parent_visible_correct: number; candidate_visible_correct: number; visible_total: number;
	};
	selection: EvaluationSelectionSummary | null;
	invariants: EvaluationInvariantSummary | null;
	forge_assessment: EvaluationForgeSummary | null;
	champion_transition_ids: string[];
};
export type DenialEntry = {
	kind: 'request_rejected' | 'runtime_capability_denied'; timestamp_millis: number;
	request_id: string | null; command: string | null;
	run_id: string | null; genome_id: string | null; world_id: string | null;
};
export type ResponseData =
	| {type: 'status'; frozen: boolean; active_runs: number; event_count: number; genome_count: number}
	| {type: 'acknowledged'; frozen: boolean; killed_runs: number}
	| {type: 'job'; job: Job; progress: {trace_events: number; last_event_sequence: number | null; last_phase: string | null}}
	| {type: 'arena_job'; job: ArenaJobProgress}
	| {type: 'genome'; genome: Genome}
	| {type: 'genomes'; genomes: Genome[]}
	| {type: 'worlds'; worlds: World[]}
	| {type: 'genome_prompt'; genome_id: string; prompt: string}
	| {type: 'champion'; champion: Champion}
	| {type: 'champion_transition'; transition: ChampionTransition}
	| {type: 'run_list'; runs: RunListEntry[]}
	| {type: 'evaluation_list'; evaluations: EvaluationListEntry[]}
	| {type: 'denial_list'; denials: DenialEntry[]};
export type ApiResponse = {version: number; request_id: string; data?: ResponseData; error?: {code: string; message: string}};

export const MAX_FRAME_BYTES = 7 * 1_048_576;
export const SOCKET_TIMEOUT_MS = 20_000;

export function safeText(value: unknown): string {
	return String(value ?? '').replace(/[\u0000-\u001f\u007f-\u009f\u001b\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 240);
}

function record(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function boundedString(value: unknown, maximum: number): value is string {
	return typeof value === 'string' && value.length > 0 && value.length <= maximum;
}

function safeInteger(value: unknown): value is number {
	return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

function boundedCount(value: unknown): value is number {
	return safeInteger(value) && value <= 0xffff_ffff;
}

function nonnegativeInteger(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value) && Number.isInteger(value) && value >= 0;
}

function validJob(value: unknown): value is Job {
	if (!record(value)) return false;
	const states: JobState[] = ['admitted', 'running', 'cancellation_requested', 'succeeded', 'failed', 'interrupted'];
	const terminals: JobTerminal[] = ['succeeded', 'failed', 'cancelled', 'interrupted'];
	return boundedString(value['job_id'], 128) && /^[a-zA-Z0-9._-]+$/.test(value['job_id'])
		&& boundedString(value['genome_id'], 256) && boundedString(value['run_id'], 256)
		&& boundedString(value['source_revision'], 256) && boundedString(value['world_id'], 256)
		&& boundedString(value['task_id'], 256) && boundedString(value['input_commitment'], 256)
		&& nonnegativeInteger(value['seed']) && boundedString(value['environment_id'], 256)
		&& record(value['budget']) && typeof value['state'] === 'string' && states.includes(value['state'] as JobState)
		&& (value['terminal'] === null || (typeof value['terminal'] === 'string' && terminals.includes(value['terminal'] as JobTerminal)));
}

function identifier(value: unknown): value is string {
	return boundedString(value, 256) && !/[\u0000-\u001f\u007f]/.test(value);
}

function identifierList(value: unknown): value is string[] {
	return Array.isArray(value) && value.length <= MAX_LIST_ITEMS && value.every(identifier);
}

function parseGenome(value: unknown): Genome | undefined {
	if (!record(value) || !identifier(value['genome_id']) || !boundedString(value['name'], 256)
		|| !identifier(value['world_id']) || !identifier(value['artifact_id']) || !identifierList(value['parent_ids'])) return undefined;
	return {genome_id: value['genome_id'], name: value['name'], world_id: value['world_id'], artifact_id: value['artifact_id'], parent_ids: [...value['parent_ids']]};
}

function parseWorld(value: unknown): World | undefined {
	if (!record(value) || !identifier(value['world_id']) || !boundedString(value['name'], 256) || !identifier(value['artifact_id'])) return undefined;
	return {world_id: value['world_id'], name: value['name'], artifact_id: value['artifact_id']};
}

function optionalIdentifier(value: unknown): value is string | null {
	return value === null || identifier(value);
}

function parseTransition(value: unknown): ChampionTransition | undefined {
	if (!record(value) || !record(value['payload']) || !record(value['event'])) return undefined;
	const payload = value['payload'];
	const event = value['event'];
	const kinds: ChampionTransitionKind[] = ['seeded', 'promoted', 'rolled_back'];
	const reason = payload['reason'];
	if (!identifier(payload['transition_id']) || !identifier(payload['world_id'])
		|| typeof payload['kind'] !== 'string' || !kinds.includes(payload['kind'] as ChampionTransitionKind)
		|| !identifier(payload['champion_genome_id']) || !optionalIdentifier(payload['previous_champion_genome_id'])
		|| !(reason === null || boundedString(reason, 512))
		|| !identifier(event['event_id']) || !safeInteger(event['sequence'])) return undefined;
	return {
		transition_id: payload['transition_id'], world_id: payload['world_id'], kind: payload['kind'] as ChampionTransitionKind,
		champion_genome_id: payload['champion_genome_id'], previous_champion_genome_id: payload['previous_champion_genome_id'],
		reason: reason as string | null, event_id: event['event_id'], sequence: event['sequence'],
	};
}

function parseChampion(value: unknown): Champion | undefined {
	if (!record(value) || !identifier(value['world_id']) || !optionalIdentifier(value['champion_genome_id'])
		|| !identifierList(value['standby_genome_ids']) || !identifierList(value['quarantined_genome_ids'])
		|| !Array.isArray(value['transitions']) || value['transitions'].length > MAX_LIST_ITEMS) return undefined;
	const transitions = value['transitions'].map(parseTransition);
	if (transitions.some(transition => transition === undefined)) return undefined;
	return {
		world_id: value['world_id'], champion_genome_id: value['champion_genome_id'],
		standby_genome_ids: [...value['standby_genome_ids']], quarantined_genome_ids: [...value['quarantined_genome_ids']],
		transitions: transitions as ChampionTransition[],
	};
}

const RUN_STATES: JobState[] = ['admitted', 'running', 'cancellation_requested', 'succeeded', 'failed', 'interrupted'];
const COMPLETION_REASONS = ['success', 'provider_failure', 'operator_interrupt', 'wall_budget_exceeded', 'output_budget_exceeded', 'io_failure'];
const FORGE_OUTCOMES = ['metrics_passed', 'metrics_rejected'];
const DENIAL_KINDS = ['request_rejected', 'runtime_capability_denied'];
const MAX_LIST_ENTRIES = 200;

function nullableBoundedString(value: unknown, maximum: number): value is string | null {
	return value === null || boundedString(value, maximum);
}

function nullableString(value: unknown, maximum: number): value is string | null {
	return value === null || (typeof value === 'string' && value.length <= maximum);
}

function nullableNonnegativeInteger(value: unknown): value is number | null {
	return value === null || nonnegativeInteger(value);
}

function validRunListEntry(value: unknown): value is RunListEntry {
	if (!record(value)) return false;
	return boundedString(value['run_id'], 256)
		&& nullableBoundedString(value['job_id'], 128)
		&& boundedString(value['genome_id'], 256)
		&& nullableBoundedString(value['world_id'], 256)
		&& typeof value['state'] === 'string' && RUN_STATES.includes(value['state'] as JobState)
		&& (value['completion_reason'] === null || (typeof value['completion_reason'] === 'string' && COMPLETION_REASONS.includes(value['completion_reason'])))
		&& nullableNonnegativeInteger(value['latency_millis'])
		&& nullableNonnegativeInteger(value['actual_cost_microusd']);
}

function validSelectionSummary(value: unknown): value is EvaluationSelectionSummary {
	if (!record(value)) return false;
	return typeof value['metrics_eligible'] === 'boolean'
		&& typeof value['invariant_gate_verified'] === 'boolean'
		&& typeof value['promotion_eligible'] === 'boolean'
		&& Number.isSafeInteger(value['estimate_bps']) && Number.isSafeInteger(value['lower_bps']) && Number.isSafeInteger(value['upper_bps'])
		&& nonnegativeInteger(value['parent_cost_microusd']) && nonnegativeInteger(value['candidate_cost_microusd'])
		&& nonnegativeInteger(value['parent_latency_millis']) && nonnegativeInteger(value['candidate_latency_millis']);
}

function validInvariantSummary(value: unknown): value is EvaluationInvariantSummary {
	if (!record(value)) return false;
	return boundedCount(value['total_checks']) && boundedCount(value['total_candidate_violations'])
		&& boundedCount(value['total_paired_regressions']) && boundedCount(value['maximum_regressions'])
		&& typeof value['regressions_within_budget'] === 'boolean'
		&& typeof value['candidate_contract_satisfied'] === 'boolean';
}

function validForgeSummary(value: unknown): value is EvaluationForgeSummary {
	if (!record(value)) return false;
	return boundedString(value['assessment_id'], 128) && typeof value['outcome'] === 'string' && FORGE_OUTCOMES.includes(value['outcome']);
}

function validEvaluationListEntry(value: unknown): value is EvaluationListEntry {
	if (!record(value)) return false;
	const evaluation = value['evaluation'];
	if (!record(evaluation) || !boundedString(evaluation['evaluation_id'], 128) || !boundedString(evaluation['world_id'], 256)
		|| !boundedString(evaluation['parent_genome_id'], 256) || !boundedString(evaluation['candidate_genome_id'], 256)
		|| !boundedCount(evaluation['parent_visible_correct']) || !boundedCount(evaluation['candidate_visible_correct'])
		|| !boundedCount(evaluation['visible_total'])) return false;
	if (value['selection'] !== null && !validSelectionSummary(value['selection'])) return false;
	if (value['invariants'] !== null && !validInvariantSummary(value['invariants'])) return false;
	if (value['forge_assessment'] !== null && !validForgeSummary(value['forge_assessment'])) return false;
	const transitions = value['champion_transition_ids'];
	return Array.isArray(transitions) && transitions.length <= MAX_LIST_ENTRIES && transitions.every(id => boundedString(id, 128));
}

function validDenialEntry(value: unknown): value is DenialEntry {
	if (!record(value)) return false;
	return typeof value['kind'] === 'string' && DENIAL_KINDS.includes(value['kind'])
		&& Number.isSafeInteger(value['timestamp_millis'])
		&& nullableString(value['request_id'], 256)
		&& nullableBoundedString(value['command'], 128)
		&& nullableBoundedString(value['run_id'], 256)
		&& nullableBoundedString(value['genome_id'], 256)
		&& nullableBoundedString(value['world_id'], 256);
}

export function parseResponse(text: string, expectedRequestId: string): ApiResponse {
	let parsed: unknown;
	try {
		parsed = JSON.parse(text) as unknown;
	} catch {
		throw new Error('daemon response is malformed');
	}
	if (!record(parsed) || parsed['version'] !== 1 || parsed['request_id'] !== expectedRequestId) {
		throw new Error('daemon response does not match the request');
	}
	const hasData = Object.hasOwn(parsed, 'data');
	const hasError = Object.hasOwn(parsed, 'error');
	if (hasData === hasError) throw new Error('daemon response has an invalid shape');
	if (hasError) {
		const error = parsed['error'];
		if (!record(error) || !boundedString(error['code'], 64) || !boundedString(error['message'], 1024)) {
			throw new Error('daemon response has an invalid error');
		}
		return {version: 1, request_id: expectedRequestId, error: {code: error['code'], message: safeText(error['message'])}};
	}
	const data = parsed['data'];
	if (!record(data)) throw new Error('daemon response has an invalid data object');
	switch (data['type']) {
		case 'status':
			if (typeof data['frozen'] !== 'boolean' || !safeInteger(data['active_runs']) || !safeInteger(data['event_count']) || !safeInteger(data['genome_count'])) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'status', frozen: data['frozen'], active_runs: data['active_runs'], event_count: data['event_count'], genome_count: data['genome_count']}};
		case 'acknowledged':
			if (typeof data['frozen'] !== 'boolean' || !safeInteger(data['killed_runs'])) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'acknowledged', frozen: data['frozen'], killed_runs: data['killed_runs']}};
		case 'job': {
			const job = data['job'];
			const progress = data['progress'];
			if (!validJob(job) || !record(progress) || !safeInteger(progress['trace_events'])
				|| !(progress['last_event_sequence'] === null || safeInteger(progress['last_event_sequence']))
				|| !(progress['last_phase'] === null || boundedString(progress['last_phase'], 128))) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'job', job, progress: {trace_events: progress['trace_events'], last_event_sequence: progress['last_event_sequence'] as number | null, last_phase: progress['last_phase'] as string | null}}};
		}
		case 'arena_job': {
			const job = data['job'];
			const states: JobState[] = ['admitted', 'running', 'cancellation_requested', 'succeeded', 'failed', 'interrupted'];
			const phases: ArenaJobPhase[] = ['preparing', 'parent_trials', 'candidate_trials', 'scoring', 'committing', 'terminal'];
			const evaluation = job && record(job) ? job['evaluation'] : undefined;
			if (!record(job) || !boundedString(job['evaluation_id'], 128) || !/^[a-zA-Z0-9._-]+$/.test(job['evaluation_id'])
				|| !boundedString(job['parent_genome_id'], 128) || !boundedString(job['candidate_genome_id'], 128)
				|| typeof job['state'] !== 'string' || !states.includes(job['state'] as JobState)
				|| typeof job['phase'] !== 'string' || !phases.includes(job['phase'] as ArenaJobPhase)
				|| !boundedCount(job['completed_trials']) || !boundedCount(job['total_trials'])
				|| job['total_trials'] === 0 || job['completed_trials'] > job['total_trials']
				|| (evaluation !== undefined && (!record(evaluation)
					|| !boundedCount(evaluation['parent_visible_correct']) || !boundedCount(evaluation['candidate_visible_correct'])
					|| !boundedCount(evaluation['visible_total']) || evaluation['visible_total'] === 0
					|| evaluation['parent_visible_correct'] > evaluation['visible_total']
					|| evaluation['candidate_visible_correct'] > evaluation['visible_total']))) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'arena_job', job: {
				evaluation_id: job['evaluation_id'], parent_genome_id: job['parent_genome_id'], candidate_genome_id: job['candidate_genome_id'],
				state: job['state'] as JobState, phase: job['phase'] as ArenaJobPhase,
				completed_trials: job['completed_trials'], total_trials: job['total_trials'],
				...(evaluation && record(evaluation) ? {evaluation: {
					parent_visible_correct: evaluation['parent_visible_correct'] as number,
					candidate_visible_correct: evaluation['candidate_visible_correct'] as number,
					visible_total: evaluation['visible_total'] as number,
				}} : {}),
			}}};
		}
		case 'genome': {
			const genome = parseGenome(data['genome']);
			if (!genome) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'genome', genome}};
		}
		case 'genomes': {
			if (!Array.isArray(data['genomes']) || data['genomes'].length > MAX_LIST_ITEMS) break;
			const genomes = data['genomes'].map(parseGenome);
			if (genomes.some(genome => genome === undefined)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'genomes', genomes: genomes as Genome[]}};
		}
		case 'worlds': {
			if (!Array.isArray(data['worlds']) || data['worlds'].length > MAX_LIST_ITEMS) break;
			const worlds = data['worlds'].map(parseWorld);
			if (worlds.some(world => world === undefined)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'worlds', worlds: worlds as World[]}};
		}
		case 'genome_prompt':
			if (!identifier(data['genome_id']) || typeof data['prompt'] !== 'string' || data['prompt'].length > MAX_FRAME_BYTES) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'genome_prompt', genome_id: data['genome_id'], prompt: data['prompt']}};
		case 'champion': {
			const champion = parseChampion(data['champion']);
			if (!champion) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'champion', champion}};
		}
		case 'champion_transition': {
			const transition = parseTransition(data['transition']);
			if (!transition) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'champion_transition', transition}};
		}
		case 'run_list': {
			const runs = data['runs'];
			if (!Array.isArray(runs) || runs.length > MAX_LIST_ENTRIES || !runs.every(validRunListEntry)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'run_list', runs}};
		}
		case 'evaluation_list': {
			const evaluations = data['evaluations'];
			if (!Array.isArray(evaluations) || evaluations.length > MAX_LIST_ENTRIES || !evaluations.every(validEvaluationListEntry)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'evaluation_list', evaluations}};
		}
		case 'denial_list': {
			const denials = data['denials'];
			if (!Array.isArray(denials) || denials.length > MAX_LIST_ENTRIES || !denials.every(validDenialEntry)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'denial_list', denials}};
		}
	}
	throw new Error('daemon response variant is invalid');
}
