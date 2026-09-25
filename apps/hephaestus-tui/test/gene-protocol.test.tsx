import test from 'node:test';
import assert from 'node:assert/strict';
import type {Command} from '../src/protocol.js';
import {parseResponse} from '../src/protocol.js';

const EVENT_HASH = 'a'.repeat(64);

function geneRecordJson(overrides: Record<string, unknown> = {}): string {
	const payload = {
		schema_version: 1, gene_id: 'gene-1', promotion_transition_id: 'promote-1', world_id: 'world-1',
		origin_parent_genome_id: 'parent-1', origin_child_genome_id: 'child-1',
		operation_before: 'identity', operation_after: 'ascii_uppercase',
		evidence_trials: 5, evidence_threshold: 3,
		...overrides,
	};
	const event = {sequence: 3, event_id: 'gene:gene-1:extracted', aggregate_id: 'gene:gene-1', event_hash: EVENT_HASH};
	return JSON.stringify({payload, event});
}

function childGenomeJson(): Record<string, unknown> {
	return {genome_id: 'child-gene-1', name: 'recipient-gene-trial-1', world_id: 'world-2', artifact_id: 'artifact-1', parent_ids: ['recipient-1']};
}

function geneTransferJson(overrides: {applied?: Record<string, unknown>; recorded?: Record<string, unknown> | null} = {}): string {
	const applied = {
		schema_version: 1, trial_id: 'trial-1', gene_id: 'gene-1', gene_event_id: 'gene:gene-1:extracted', gene_event_hash: EVENT_HASH,
		to_genome_id: 'recipient-1', world_id: 'world-2', child: childGenomeJson(),
		prompt_artifact_before: 'artifact-before', prompt_artifact_after: 'artifact-after',
		...overrides.applied,
	};
	const appliedEvent = {sequence: 4, event_id: 'gene:transfer:trial-1:applied', aggregate_id: 'gene:transfer:trial-1', event_hash: EVENT_HASH};
	const recordedProvided = overrides.recorded !== undefined;
	const recorded = recordedProvided ? overrides.recorded : {
		schema_version: 1, trial_id: 'trial-1', gene_id: 'gene-1', applied_event_id: 'gene:transfer:trial-1:applied', applied_event_hash: EVENT_HASH,
		evaluation_id: 'eval-1', selection_event_id: 'arena:selection:eval-1:selected', selection_event_hash: EVENT_HASH,
		selection_receipt_artifact_id: 'receipt-1', outcome: 'positive', estimate_bps: 500, lower_bps: 100, upper_bps: 900,
	};
	const recordedEvent = recorded === null ? null : {sequence: 5, event_id: 'gene:transfer:trial-1:recorded', aggregate_id: 'gene:transfer:trial-1', event_hash: EVENT_HASH};
	return JSON.stringify({applied, applied_event: appliedEvent, recorded, recorded_event: recordedEvent});
}

test('gene commands carry the exact fields the daemon requires', () => {
	const extract: Command = {command: 'gene_extract', gene_id: 'gene-1', promotion_transition_id: 'promote-1'};
	const transfer: Command = {command: 'gene_transfer', trial_id: 'trial-1', gene_id: 'gene-1', to_genome_id: 'recipient-1'};
	const record: Command = {command: 'gene_record', trial_id: 'trial-1', evaluation_id: 'eval-1'};
	const show: Command = {command: 'gene_show', gene_id: 'gene-1'};
	const list: Command = {command: 'gene_list'};
	const speciate: Command = {command: 'gene_speciate', species_id: 'species-1', gene_id: 'gene-1', domain_world_id: 'world-2'};
	assert.deepEqual(JSON.parse(JSON.stringify(extract)), {command: 'gene_extract', gene_id: 'gene-1', promotion_transition_id: 'promote-1'});
	assert.deepEqual(JSON.parse(JSON.stringify(transfer)), {command: 'gene_transfer', trial_id: 'trial-1', gene_id: 'gene-1', to_genome_id: 'recipient-1'});
	assert.deepEqual(JSON.parse(JSON.stringify(record)), {command: 'gene_record', trial_id: 'trial-1', evaluation_id: 'eval-1'});
	assert.deepEqual(JSON.parse(JSON.stringify(show)), {command: 'gene_show', gene_id: 'gene-1'});
	assert.deepEqual(JSON.parse(JSON.stringify(list)), {command: 'gene_list'});
	assert.deepEqual(JSON.parse(JSON.stringify(speciate)), {command: 'gene_speciate', species_id: 'species-1', gene_id: 'gene-1', domain_world_id: 'world-2'});
});

test('parseResponse accepts one extracted Gene', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"gene","gene":${geneRecordJson()}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'gene');
	if (response.data?.type !== 'gene') return;
	assert.equal(response.data.gene.gene_id, 'gene-1');
	assert.equal(response.data.gene.operation_before, 'identity');
	assert.equal(response.data.gene.operation_after, 'ascii_uppercase');
	assert.equal(response.data.gene.evidence_trials, 5);
});

test('parseResponse rejects a Gene payload missing required fields', () => {
	const broken = JSON.stringify({payload: {schema_version: 1, gene_id: 'gene-1'}, event: {sequence: 3, event_id: 'gene:gene-1:extracted'}});
	const text = `{"version":1,"request_id":"request","data":{"type":"gene","gene":${broken}}}`;
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse accepts an unrecorded transfer trial with null recorded fields', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"gene_transfer","trial":${geneTransferJson({recorded: null})}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'gene_transfer');
	if (response.data?.type !== 'gene_transfer') return;
	assert.equal(response.data.trial.trial_id, 'trial-1');
	assert.equal(response.data.trial.outcome, null);
	assert.equal(response.data.trial.evaluation_id, null);
	assert.equal(response.data.trial.child.genome_id, 'child-gene-1');
});

test('parseResponse accepts a recorded transfer trial and its measured outcome', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"gene_transfer","trial":${geneTransferJson()}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'gene_transfer');
	if (response.data?.type !== 'gene_transfer') return;
	assert.equal(response.data.trial.outcome, 'positive');
	assert.equal(response.data.trial.estimate_bps, 500);
	assert.equal(response.data.trial.lower_bps, 100);
	assert.equal(response.data.trial.upper_bps, 900);
});

test('parseResponse rejects negative transfer with an unknown outcome enum value', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"gene_transfer","trial":${geneTransferJson({
		recorded: {
			schema_version: 1, trial_id: 'trial-1', gene_id: 'gene-1', applied_event_id: 'gene:transfer:trial-1:applied', applied_event_hash: EVENT_HASH,
			evaluation_id: 'eval-1', selection_event_id: 'arena:selection:eval-1:selected', selection_event_hash: EVENT_HASH,
			selection_receipt_artifact_id: 'receipt-1', outcome: 'harmful', estimate_bps: -500, lower_bps: -900, upper_bps: -100,
		},
	})}}}`;
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse accepts a negative transfer outcome; negative transfer is never dropped', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"gene_transfer","trial":${geneTransferJson({
		recorded: {
			schema_version: 1, trial_id: 'trial-1', gene_id: 'gene-1', applied_event_id: 'gene:transfer:trial-1:applied', applied_event_hash: EVENT_HASH,
			evaluation_id: 'eval-1', selection_event_id: 'arena:selection:eval-1:selected', selection_event_hash: EVENT_HASH,
			selection_receipt_artifact_id: 'receipt-1', outcome: 'negative', estimate_bps: -500, lower_bps: -900, upper_bps: -100,
		},
	})}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'gene_transfer');
	if (response.data?.type !== 'gene_transfer') return;
	assert.equal(response.data.trial.outcome, 'negative');
});

test('parseResponse accepts a species record admitted from persistent domain advantage', () => {
	const species = {
		payload: {
			schema_version: 1, species_id: 'species-1', gene_id: 'gene-1', gene_event_id: 'gene:gene-1:extracted', gene_event_hash: EVENT_HASH,
			domain_world_id: 'world-2', lineage_genome_ids: ['recipient-1', 'recipient-2', 'recipient-3'],
			supporting_event_ids: ['gene:transfer:t1:recorded', 'gene:transfer:t2:recorded', 'gene:transfer:t3:recorded'],
			average_estimate_bps: 450, minimum_lineages: 3, minimum_effect_bps: 300,
		},
		event: {sequence: 9, event_id: 'gene:species:species-1:created', aggregate_id: 'gene:species:species-1', event_hash: EVENT_HASH},
	};
	const text = `{"version":1,"request_id":"request","data":{"type":"gene_species","species":${JSON.stringify(species)}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'gene_species');
	if (response.data?.type !== 'gene_species') return;
	assert.equal(response.data.species.lineage_genome_ids.length, 3);
	assert.equal(response.data.species.average_estimate_bps, 450);
});

test('parseResponse accepts a full Gene aggregate with a contradiction and a species', () => {
	const gene = JSON.parse(geneRecordJson()) as Record<string, unknown>;
	const transfer = JSON.parse(geneTransferJson()) as Record<string, unknown>;
	const negativeTransfer = JSON.parse(geneTransferJson({
		applied: {trial_id: 'trial-2', to_genome_id: 'recipient-2'},
		recorded: {
			schema_version: 1, trial_id: 'trial-2', gene_id: 'gene-1', applied_event_id: 'gene:transfer:trial-2:applied', applied_event_hash: EVENT_HASH,
			evaluation_id: 'eval-2', selection_event_id: 'arena:selection:eval-2:selected', selection_event_hash: EVENT_HASH,
			selection_receipt_artifact_id: 'receipt-2', outcome: 'negative', estimate_bps: -400, lower_bps: -700, upper_bps: -100,
		},
	})) as Record<string, unknown>;
	const contradiction = {
		payload: {
			schema_version: 1, gene_id: 'gene-1', positive_trial_id: 'trial-1', positive_world_id: 'world-2',
			positive_event_id: 'gene:transfer:trial-1:recorded', positive_event_hash: EVENT_HASH,
			negative_trial_id: 'trial-2', negative_world_id: 'world-3',
			negative_event_id: 'gene:transfer:trial-2:recorded', negative_event_hash: EVENT_HASH,
		},
		event: {sequence: 6, event_id: 'gene:gene-1:contradiction', aggregate_id: 'gene:gene-1', event_hash: EVENT_HASH},
	};
	const aggregate = {gene, transfers: [transfer, negativeTransfer], contradiction, species: []};
	const text = `{"version":1,"request_id":"request","data":{"type":"gene_aggregate","aggregate":${JSON.stringify(aggregate)}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'gene_aggregate');
	if (response.data?.type !== 'gene_aggregate') return;
	assert.equal(response.data.aggregate.gene.gene_id, 'gene-1');
	assert.equal(response.data.aggregate.transfers.length, 2);
	assert.equal(response.data.aggregate.contradiction?.positive_world_id, 'world-2');
	assert.equal(response.data.aggregate.contradiction?.negative_world_id, 'world-3');
	assert.equal(response.data.aggregate.species.length, 0);
});

test('parseResponse accepts a Gene aggregate with no contradiction recorded', () => {
	const gene = JSON.parse(geneRecordJson()) as Record<string, unknown>;
	const aggregate = {gene, transfers: [], contradiction: null, species: []};
	const text = `{"version":1,"request_id":"request","data":{"type":"gene_aggregate","aggregate":${JSON.stringify(aggregate)}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'gene_aggregate');
	if (response.data?.type !== 'gene_aggregate') return;
	assert.equal(response.data.aggregate.contradiction, null);
});

test('parseResponse accepts a Gene list with aggregate transfer counts', () => {
	const summary = {
		...JSON.parse(geneRecordJson()) as Record<string, unknown>,
		lineages: 3, positive: 2, neutral: 0, negative: 1, contradiction: true, species_ids: ['species-1'],
	};
	const text = `{"version":1,"request_id":"request","data":{"type":"genes","genes":[${JSON.stringify(summary)}]}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'genes');
	if (response.data?.type !== 'genes') return;
	assert.equal(response.data.genes.length, 1);
	assert.equal(response.data.genes[0]?.lineages, 3);
	assert.equal(response.data.genes[0]?.positive, 2);
	assert.equal(response.data.genes[0]?.negative, 1);
	assert.equal(response.data.genes[0]?.contradiction, true);
	assert.deepEqual(response.data.genes[0]?.species_ids, ['species-1']);
});

test('parseResponse rejects a Gene list entry with a non-boolean contradiction flag', () => {
	const summary = {
		...JSON.parse(geneRecordJson()) as Record<string, unknown>,
		lineages: 3, positive: 2, neutral: 0, negative: 1, contradiction: 'yes', species_ids: [],
	};
	const text = `{"version":1,"request_id":"request","data":{"type":"genes","genes":[${JSON.stringify(summary)}]}}`;
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});
