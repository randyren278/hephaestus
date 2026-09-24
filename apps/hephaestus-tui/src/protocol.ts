export type Command =
	| {command: 'status'}
	| {command: 'freeze'}
	| {command: 'unfreeze'}
	| {command: 'kill_all'}
	| {command: 'job_status'; job_id: string}
	| {command: 'job_kill'; job_id: string}
	| {command: 'world_list'}
	| {command: 'world_show'; world_id: string}
	| {command: 'genome_list'}
	| {command: 'genome_show'; genome_id: string}
	| {command: 'genome_prompt'; genome_id: string}
	| {command: 'champion_show'; world_id: string};

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
export type WorldRecord = {world_id: string; name: string; artifact_id: string};
export type GenomeRecord = {genome_id: string; name: string; world_id: string; artifact_id: string; parent_ids: string[]};
export type ChampionTransitionKind = 'seeded' | 'promoted' | 'rolled_back';
export type ChampionPromotionEvidence = {
	assessment_id: string; assessment_event_id: string; assessment_event_hash: string; evaluation_id: string;
	selection_receipt_artifact_id: string; invariant_event_id: string; invariant_event_hash: string;
	invariant_receipt_artifact_id: string;
};
export type ChampionTransitionRecord = {
	payload: {
		schema_version: number; transition_id: string; world_id: string; kind: ChampionTransitionKind;
		champion_genome_id: string; previous_champion_genome_id: string | null;
		previous_transition_event_id: string | null; previous_transition_event_hash: string | null;
		promotion: ChampionPromotionEvidence | null; reason: string | null;
	};
	event: {sequence: number; event_id: string; aggregate_id: string; event_hash: string};
};
export type ChampionRecord = {
	world_id: string; champion_genome_id: string | null; standby_genome_ids: string[];
	quarantined_genome_ids: string[]; transitions: ChampionTransitionRecord[];
};

export type ResponseData =
	| {type: 'status'; frozen: boolean; active_runs: number; event_count: number; genome_count: number}
	| {type: 'acknowledged'; frozen: boolean; killed_runs: number}
	| {type: 'job'; job: Job; progress: {trace_events: number; last_event_sequence: number | null; last_phase: string | null}}
	| {type: 'arena_job'; job: ArenaJobProgress}
	| {type: 'world'; world: WorldRecord}
	| {type: 'worlds'; worlds: WorldRecord[]}
	| {type: 'genome'; genome: GenomeRecord}
	| {type: 'genomes'; genomes: GenomeRecord[]}
	| {type: 'genome_prompt'; genome_id: string; prompt: string}
	| {type: 'champion'; champion: ChampionRecord};
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

function stringArray(value: unknown, maxItems: number, maxLength: number): value is string[] {
	return Array.isArray(value) && value.length <= maxItems && value.every(item => boundedString(item, maxLength));
}

function validWorld(value: unknown): value is WorldRecord {
	return record(value) && boundedString(value['world_id'], 256) && boundedString(value['name'], 512) && boundedString(value['artifact_id'], 256);
}

function validGenome(value: unknown): value is GenomeRecord {
	return record(value) && boundedString(value['genome_id'], 256) && boundedString(value['name'], 512)
		&& boundedString(value['world_id'], 256) && boundedString(value['artifact_id'], 256)
		&& stringArray(value['parent_ids'], 64, 256);
}

function validChampionTransition(value: unknown): value is ChampionTransitionRecord {
	if (!record(value) || !record(value['payload']) || !record(value['event'])) return false;
	const payload = value['payload'];
	const event = value['event'];
	const kinds: ChampionTransitionKind[] = ['seeded', 'promoted', 'rolled_back'];
	const promotion = payload['promotion'];
	if (promotion !== null && (!record(promotion)
		|| !boundedString(promotion['assessment_id'], 256) || !boundedString(promotion['assessment_event_id'], 256)
		|| !boundedString(promotion['assessment_event_hash'], 256) || !boundedString(promotion['evaluation_id'], 256)
		|| !boundedString(promotion['selection_receipt_artifact_id'], 256) || !boundedString(promotion['invariant_event_id'], 256)
		|| !boundedString(promotion['invariant_event_hash'], 256) || !boundedString(promotion['invariant_receipt_artifact_id'], 256))) return false;
	return typeof payload['schema_version'] === 'number'
		&& boundedString(payload['transition_id'], 256) && boundedString(payload['world_id'], 256)
		&& typeof payload['kind'] === 'string' && kinds.includes(payload['kind'] as ChampionTransitionKind)
		&& boundedString(payload['champion_genome_id'], 256)
		&& (payload['previous_champion_genome_id'] === null || boundedString(payload['previous_champion_genome_id'], 256))
		&& (payload['previous_transition_event_id'] === null || boundedString(payload['previous_transition_event_id'], 256))
		&& (payload['previous_transition_event_hash'] === null || boundedString(payload['previous_transition_event_hash'], 256))
		&& (payload['reason'] === null || boundedString(payload['reason'], 2048))
		&& safeInteger(event['sequence']) && boundedString(event['event_id'], 256)
		&& boundedString(event['aggregate_id'], 256) && boundedString(event['event_hash'], 256);
}

function validChampion(value: unknown): value is ChampionRecord {
	if (!record(value)) return false;
	if (value['champion_genome_id'] !== null && !boundedString(value['champion_genome_id'], 256)) return false;
	if (!stringArray(value['standby_genome_ids'], 4096, 256) || !stringArray(value['quarantined_genome_ids'], 4096, 256)) return false;
	if (!Array.isArray(value['transitions']) || value['transitions'].length > 4096) return false;
	return boundedString(value['world_id'], 256) && value['transitions'].every(validChampionTransition);
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
		case 'world': {
			const world = data['world'];
			if (!validWorld(world)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'world', world}};
		}
		case 'worlds': {
			const worlds = data['worlds'];
			if (!Array.isArray(worlds) || worlds.length > 65_536 || !worlds.every(validWorld)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'worlds', worlds}};
		}
		case 'genome': {
			const genome = data['genome'];
			if (!validGenome(genome)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'genome', genome}};
		}
		case 'genomes': {
			const genomes = data['genomes'];
			if (!Array.isArray(genomes) || genomes.length > 65_536 || !genomes.every(validGenome)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'genomes', genomes}};
		}
		case 'genome_prompt': {
			if (!boundedString(data['genome_id'], 256) || typeof data['prompt'] !== 'string' || data['prompt'].length > 1_048_576) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'genome_prompt', genome_id: data['genome_id'], prompt: data['prompt']}};
		}
		case 'champion': {
			const champion = data['champion'];
			if (!validChampion(champion)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'champion', champion}};
		}
	}
	throw new Error('daemon response variant is invalid');
}
