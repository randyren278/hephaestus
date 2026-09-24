export type Command =
	| {command: 'status'}
	| {command: 'freeze'}
	| {command: 'unfreeze'}
	| {command: 'kill_all'}
	| {command: 'job_status'; job_id: string}
	| {command: 'job_kill'; job_id: string};

export type ApiRequest = {version: 1; request_id: string; token: string; command: Command};
export type JobState = 'admitted' | 'running' | 'cancellation_requested' | 'succeeded' | 'failed' | 'interrupted';
export type JobTerminal = 'succeeded' | 'failed' | 'cancelled' | 'interrupted';
export type Job = {
	job_id: string; genome_id: string; run_id: string; source_revision: string; world_id: string;
	task_id: string; input_commitment: string; seed: number; environment_id: string;
	budget: Record<string, unknown>; state: JobState; terminal: JobTerminal | null;
};
export type ResponseData =
	| {type: 'status'; frozen: boolean; active_runs: number; event_count: number; genome_count: number}
	| {type: 'acknowledged'; frozen: boolean; killed_runs: number}
	| {type: 'job'; job: Job; progress: {trace_events: number; last_event_sequence: number | null; last_phase: string | null}};
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
	}
	throw new Error('daemon response variant is invalid');
}
