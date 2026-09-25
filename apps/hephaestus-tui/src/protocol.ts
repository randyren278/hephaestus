export type Command =
	| {command: 'status'}
	| {command: 'freeze'}
	| {command: 'unfreeze'}
	| {command: 'kill_all'}
	| {command: 'job_status'; job_id: string}
	| {command: 'job_kill'; job_id: string}
	| {command: 'genome_list'}
	| {command: 'genome_show'; genome_id: string}
	| {command: 'genome_register'; path: string; world_id: string}
	| {command: 'world_list'}
	| {command: 'genome_prompt'; genome_id: string}
	| {command: 'champion_show'; world_id: string}
	| {command: 'champion_rollback'; transition_id: string; world_id: string; reason: string}
	| {command: 'gene_extract'; gene_id: string; promotion_transition_id: string}
	| {command: 'gene_transfer'; trial_id: string; gene_id: string; to_genome_id: string}
	| {command: 'gene_record'; trial_id: string; evaluation_id: string}
	| {command: 'gene_show'; gene_id: string}
	| {command: 'gene_list'}
	| {command: 'gene_speciate'; species_id: string; gene_id: string; domain_world_id: string}
	| {command: 'evaluate_pair'; evaluation_id: string; parent_genome_id: string; candidate_genome_id: string}
	| {command: 'evolve_start'; run_id: string; world_id: string; from_genome_id: string; generations: number; budget: number}
	| {command: 'evolve_status'; run_id: string}
	| {command: 'evolve_cancel'; run_id: string}
	| {command: 'run_list'; limit: number}
	| {command: 'evaluation_list'; limit: number}
	| {command: 'denial_list'; limit: number};

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
export type Gene = {
	gene_id: string; promotion_transition_id: string; world_id: string;
	origin_parent_genome_id: string; origin_child_genome_id: string;
	operation_before: string; operation_after: string;
	evidence_trials: number; evidence_threshold: number;
	event_id: string; sequence: number;
};
export type GeneTransferOutcome = 'positive' | 'neutral' | 'negative';
export type GeneTransfer = {
	trial_id: string; gene_id: string; to_genome_id: string; world_id: string; child: Genome;
	applied_event_id: string; applied_sequence: number;
	evaluation_id: string | null; outcome: GeneTransferOutcome | null;
	estimate_bps: number | null; lower_bps: number | null; upper_bps: number | null;
	recorded_event_id: string | null; recorded_sequence: number | null;
};
export type GeneContradiction = {
	gene_id: string; positive_trial_id: string; positive_world_id: string;
	negative_trial_id: string; negative_world_id: string; event_id: string; sequence: number;
};
export type GeneSpecies = {
	species_id: string; gene_id: string; domain_world_id: string;
	lineage_genome_ids: string[]; average_estimate_bps: number;
	minimum_lineages: number; minimum_effect_bps: number; event_id: string; sequence: number;
};
export type GeneSummary = {
	gene: Gene; lineages: number; positive: number; neutral: number; negative: number;
	contradiction: boolean; species_ids: string[];
};
export type GeneAggregate = {
	gene: Gene; transfers: GeneTransfer[]; contradiction: GeneContradiction | null; species: GeneSpecies[];
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
export type EvolutionRunState = 'running' | 'finished';
export type EvolutionFinishReason = 'generations_exhausted' | 'budget_exhausted' | 'cancelled' | 'interrupted';
export type EvolutionGeneration = {
	generation_index: number; champion_before: string; diagnostic_evaluation_id: string;
	proposal_id: string; child_genome_id: string; child_evaluation_id: string; assessment_id: string;
	promoted: boolean; champion_after: string; event_id: string; sequence: number;
};
export type EvolutionRun = {
	run_id: string; world_id: string; from_genome_id: string; baseline_genome_id: string;
	max_generations: number; max_paired_trials: number; trials_consumed: number;
	state: EvolutionRunState; cancel_requested: boolean; finish_reason: EvolutionFinishReason | null;
	generations: EvolutionGeneration[]; started_event_id: string;
};
export type DenialEntry = {
	kind: 'request_rejected' | 'runtime_capability_denied' | 'mcp_call_denied'; timestamp_millis: number;
	request_id: string | null; command: string | null;
	run_id: string | null; genome_id: string | null; world_id: string | null;
	client_id: string | null;
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
	| {type: 'gene'; gene: Gene}
	| {type: 'genes'; genes: GeneSummary[]}
	| {type: 'gene_transfer'; trial: GeneTransfer}
	| {type: 'gene_species'; species: GeneSpecies}
	| {type: 'gene_aggregate'; aggregate: GeneAggregate}
	| {type: 'evolution'; run: EvolutionRun}
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

const GENE_TRANSFER_OUTCOMES: GeneTransferOutcome[] = ['positive', 'neutral', 'negative'];

function nonnegativeNumber(value: unknown): value is number {
	return typeof value === 'number' && Number.isFinite(value);
}

function parseGene(value: unknown): Gene | undefined {
	if (!record(value) || !record(value['payload']) || !record(value['event'])) return undefined;
	const payload = value['payload'];
	const event = value['event'];
	if (!identifier(payload['gene_id']) || !identifier(payload['promotion_transition_id']) || !identifier(payload['world_id'])
		|| !identifier(payload['origin_parent_genome_id']) || !identifier(payload['origin_child_genome_id'])
		|| !boundedString(payload['operation_before'], 64) || !boundedString(payload['operation_after'], 64)
		|| !boundedCount(payload['evidence_trials']) || !boundedCount(payload['evidence_threshold'])
		|| !identifier(event['event_id']) || !safeInteger(event['sequence'])) return undefined;
	return {
		gene_id: payload['gene_id'], promotion_transition_id: payload['promotion_transition_id'], world_id: payload['world_id'],
		origin_parent_genome_id: payload['origin_parent_genome_id'], origin_child_genome_id: payload['origin_child_genome_id'],
		operation_before: payload['operation_before'], operation_after: payload['operation_after'],
		evidence_trials: payload['evidence_trials'], evidence_threshold: payload['evidence_threshold'],
		event_id: event['event_id'], sequence: event['sequence'],
	};
}

function parseGeneTransfer(value: unknown): GeneTransfer | undefined {
	if (!record(value) || !record(value['applied']) || !record(value['applied_event'])) return undefined;
	const applied = value['applied'];
	const appliedEvent = value['applied_event'];
	const child = parseGenome(applied['child']);
	if (!identifier(applied['trial_id']) || !identifier(applied['gene_id']) || !identifier(applied['to_genome_id'])
		|| !identifier(applied['world_id']) || !child
		|| !identifier(appliedEvent['event_id']) || !safeInteger(appliedEvent['sequence'])) return undefined;
	const recorded = value['recorded'];
	const recordedEvent = value['recorded_event'];
	if (recorded === null) {
		if (recordedEvent !== null) return undefined;
		return {
			trial_id: applied['trial_id'], gene_id: applied['gene_id'], to_genome_id: applied['to_genome_id'],
			world_id: applied['world_id'], child, applied_event_id: appliedEvent['event_id'], applied_sequence: appliedEvent['sequence'],
			evaluation_id: null, outcome: null, estimate_bps: null, lower_bps: null, upper_bps: null,
			recorded_event_id: null, recorded_sequence: null,
		};
	}
	if (!record(recorded) || !record(recordedEvent)
		|| !identifier(recorded['evaluation_id'])
		|| typeof recorded['outcome'] !== 'string' || !GENE_TRANSFER_OUTCOMES.includes(recorded['outcome'] as GeneTransferOutcome)
		|| !Number.isSafeInteger(recorded['estimate_bps']) || !Number.isSafeInteger(recorded['lower_bps']) || !Number.isSafeInteger(recorded['upper_bps'])
		|| !identifier(recordedEvent['event_id']) || !safeInteger(recordedEvent['sequence'])) return undefined;
	return {
		trial_id: applied['trial_id'], gene_id: applied['gene_id'], to_genome_id: applied['to_genome_id'],
		world_id: applied['world_id'], child, applied_event_id: appliedEvent['event_id'], applied_sequence: appliedEvent['sequence'],
		evaluation_id: recorded['evaluation_id'], outcome: recorded['outcome'] as GeneTransferOutcome,
		estimate_bps: recorded['estimate_bps'] as number, lower_bps: recorded['lower_bps'] as number, upper_bps: recorded['upper_bps'] as number,
		recorded_event_id: recordedEvent['event_id'], recorded_sequence: recordedEvent['sequence'],
	};
}

function parseGeneContradiction(value: unknown): GeneContradiction | undefined {
	if (!record(value) || !record(value['payload']) || !record(value['event'])) return undefined;
	const payload = value['payload'];
	const event = value['event'];
	if (!identifier(payload['gene_id']) || !identifier(payload['positive_trial_id']) || !identifier(payload['positive_world_id'])
		|| !identifier(payload['negative_trial_id']) || !identifier(payload['negative_world_id'])
		|| !identifier(event['event_id']) || !safeInteger(event['sequence'])) return undefined;
	return {
		gene_id: payload['gene_id'], positive_trial_id: payload['positive_trial_id'], positive_world_id: payload['positive_world_id'],
		negative_trial_id: payload['negative_trial_id'], negative_world_id: payload['negative_world_id'],
		event_id: event['event_id'], sequence: event['sequence'],
	};
}

function parseGeneSpecies(value: unknown): GeneSpecies | undefined {
	if (!record(value) || !record(value['payload']) || !record(value['event'])) return undefined;
	const payload = value['payload'];
	const event = value['event'];
	if (!identifier(payload['species_id']) || !identifier(payload['gene_id']) || !identifier(payload['domain_world_id'])
		|| !identifierList(payload['lineage_genome_ids']) || !Number.isSafeInteger(payload['average_estimate_bps'])
		|| !boundedCount(payload['minimum_lineages']) || !nonnegativeNumber(payload['minimum_effect_bps'])
		|| !identifier(event['event_id']) || !safeInteger(event['sequence'])) return undefined;
	return {
		species_id: payload['species_id'], gene_id: payload['gene_id'], domain_world_id: payload['domain_world_id'],
		lineage_genome_ids: [...payload['lineage_genome_ids']], average_estimate_bps: payload['average_estimate_bps'] as number,
		minimum_lineages: payload['minimum_lineages'] as number, minimum_effect_bps: payload['minimum_effect_bps'] as number,
		event_id: event['event_id'], sequence: event['sequence'],
	};
}

function parseGeneSummary(value: unknown): GeneSummary | undefined {
	if (!record(value)) return undefined;
	const gene = parseGene(value);
	if (!gene || !boundedCount(value['lineages']) || !boundedCount(value['positive']) || !boundedCount(value['neutral'])
		|| !boundedCount(value['negative']) || typeof value['contradiction'] !== 'boolean'
		|| !identifierList(value['species_ids'])) return undefined;
	return {
		gene, lineages: value['lineages'], positive: value['positive'], neutral: value['neutral'], negative: value['negative'],
		contradiction: value['contradiction'], species_ids: [...value['species_ids']],
	};
}

function parseGeneAggregate(value: unknown): GeneAggregate | undefined {
	if (!record(value)) return undefined;
	const gene = parseGene(value['gene']);
	if (!gene || !Array.isArray(value['transfers']) || value['transfers'].length > MAX_LIST_ITEMS
		|| !Array.isArray(value['species']) || value['species'].length > MAX_LIST_ITEMS) return undefined;
	const transfers = value['transfers'].map(parseGeneTransfer);
	if (transfers.some(transfer => transfer === undefined)) return undefined;
	const species = value['species'].map(parseGeneSpecies);
	if (species.some(item => item === undefined)) return undefined;
	const contradiction = value['contradiction'];
	if (contradiction !== null && !parseGeneContradiction(contradiction)) return undefined;
	return {
		gene, transfers: transfers as GeneTransfer[],
		contradiction: contradiction === null ? null : (parseGeneContradiction(contradiction) as GeneContradiction),
		species: species as GeneSpecies[],
	};
}

const EVOLUTION_STATES: EvolutionRunState[] = ['running', 'finished'];
const EVOLUTION_FINISH_REASONS: EvolutionFinishReason[] = ['generations_exhausted', 'budget_exhausted', 'cancelled', 'interrupted'];
const MAX_EVOLUTION_GENERATIONS = 1_000_000;

function nullableEnum<T extends string>(value: unknown, allowed: readonly T[]): value is T | null {
	return value === null || (typeof value === 'string' && (allowed as readonly string[]).includes(value));
}

function validEvolutionGeneration(value: unknown): boolean {
	if (!record(value) || !record(value['payload']) || !record(value['event'])) return false;
	const payload = value['payload'];
	const event = value['event'];
	return boundedCount(payload['generation_index'])
		&& identifier(payload['champion_before'])
		&& identifier(payload['diagnostic_evaluation_id'])
		&& identifier(payload['proposal_id'])
		&& identifier(payload['child_genome_id'])
		&& identifier(payload['child_evaluation_id'])
		&& identifier(payload['assessment_id'])
		&& typeof payload['promoted'] === 'boolean'
		&& identifier(payload['champion_after'])
		&& identifier(event['event_id'])
		&& safeInteger(event['sequence']);
}

function parseEvolutionGeneration(value: unknown): EvolutionGeneration | undefined {
	if (!validEvolutionGeneration(value)) return undefined;
	const payload = (value as {payload: Record<string, unknown>}).payload;
	const event = (value as {event: Record<string, unknown>}).event;
	return {
		generation_index: payload['generation_index'] as number,
		champion_before: payload['champion_before'] as string,
		diagnostic_evaluation_id: payload['diagnostic_evaluation_id'] as string,
		proposal_id: payload['proposal_id'] as string,
		child_genome_id: payload['child_genome_id'] as string,
		child_evaluation_id: payload['child_evaluation_id'] as string,
		assessment_id: payload['assessment_id'] as string,
		promoted: payload['promoted'] as boolean,
		champion_after: payload['champion_after'] as string,
		event_id: event['event_id'] as string,
		sequence: event['sequence'] as number,
	};
}

function parseEvolutionRun(value: unknown): EvolutionRun | undefined {
	if (!record(value)) return undefined;
	const startedEvent = value['started_event'];
	if (!identifier(value['run_id']) || !identifier(value['world_id']) || !identifier(value['from_genome_id'])
		|| !identifier(value['baseline_genome_id']) || !boundedCount(value['max_generations'])
		|| !boundedCount(value['max_paired_trials']) || !boundedCount(value['trials_consumed'])
		|| typeof value['state'] !== 'string' || !EVOLUTION_STATES.includes(value['state'] as EvolutionRunState)
		|| typeof value['cancel_requested'] !== 'boolean'
		|| !nullableEnum(value['finish_reason'], EVOLUTION_FINISH_REASONS)
		|| !record(startedEvent) || !identifier(startedEvent['event_id'])
		|| !Array.isArray(value['generations']) || value['generations'].length > MAX_EVOLUTION_GENERATIONS) {
		return undefined;
	}
	const generations = value['generations'].map(parseEvolutionGeneration);
	if (generations.some(generation => generation === undefined)) return undefined;
	return {
		run_id: value['run_id'], world_id: value['world_id'], from_genome_id: value['from_genome_id'],
		baseline_genome_id: value['baseline_genome_id'], max_generations: value['max_generations'],
		max_paired_trials: value['max_paired_trials'], trials_consumed: value['trials_consumed'],
		state: value['state'] as EvolutionRunState, cancel_requested: value['cancel_requested'],
		finish_reason: value['finish_reason'] as EvolutionFinishReason | null,
		generations: generations as EvolutionGeneration[], started_event_id: startedEvent['event_id'],
	};
}

const RUN_STATES: JobState[] = ['admitted', 'running', 'cancellation_requested', 'succeeded', 'failed', 'interrupted'];
const COMPLETION_REASONS = ['success', 'provider_failure', 'operator_interrupt', 'wall_budget_exceeded', 'output_budget_exceeded', 'io_failure'];
const FORGE_OUTCOMES = ['metrics_passed', 'metrics_rejected'];
const DENIAL_KINDS = ['request_rejected', 'runtime_capability_denied', 'mcp_call_denied'];
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
		&& nullableBoundedString(value['world_id'], 256)
		&& nullableBoundedString(value['client_id'], 256);
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
		case 'gene': {
			const gene = parseGene(data['gene']);
			if (!gene) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'gene', gene}};
		}
		case 'genes': {
			if (!Array.isArray(data['genes']) || data['genes'].length > MAX_LIST_ITEMS) break;
			const genes = data['genes'].map(parseGeneSummary);
			if (genes.some(gene => gene === undefined)) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'genes', genes: genes as GeneSummary[]}};
		}
		case 'gene_transfer': {
			const trial = parseGeneTransfer(data['trial']);
			if (!trial) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'gene_transfer', trial}};
		}
		case 'gene_species': {
			const species = parseGeneSpecies(data['species']);
			if (!species) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'gene_species', species}};
		}
		case 'gene_aggregate': {
			const aggregate = parseGeneAggregate(data['aggregate']);
			if (!aggregate) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'gene_aggregate', aggregate}};
		}
		case 'evolution': {
			const run = parseEvolutionRun(data['run']);
			if (!run) break;
			return {version: 1, request_id: expectedRequestId, data: {type: 'evolution', run}};
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
