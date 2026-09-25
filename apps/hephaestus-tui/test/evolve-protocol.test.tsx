import test from 'node:test';
import assert from 'node:assert/strict';
import type {Command} from '../src/protocol.js';
import {parseResponse} from '../src/protocol.js';

function evolutionResponseText(overrides: {generations?: string; run?: Record<string, unknown>} = {}): string {
	const generations = overrides.generations ?? `[{
		"payload": {
			"schema_version": 1, "run_id": "run-1", "generation_index": 0, "champion_before": "champion-0",
			"diagnostic_evaluation_id": "evolve-run-1-g0-d", "proposal_id": "evolve-run-1-g0-p",
			"child_genome_id": "child-0", "child_evaluation_id": "evolve-run-1-g0-c",
			"assessment_id": "evolve-run-1-g0-a", "promoted": true, "champion_after": "child-0"
		},
		"event": {"sequence": 12, "event_id": "evolution:run-1:generation:0", "aggregate_id": "evolution:run-1", "event_type": "evolution.generation", "actor": "local-operator", "event_hash": "${'a'.repeat(64)}"}
	}]`;
	const run = {
		run_id: 'run-1', world_id: 'world-1', from_genome_id: 'champion-0', baseline_genome_id: 'baseline-1',
		max_generations: 3, max_paired_trials: 6, trials_consumed: 2, state: 'running', cancel_requested: false,
		finish_reason: null,
		started_event: {sequence: 1, event_id: 'evolution:run-1:started', aggregate_id: 'evolution:run-1', event_type: 'evolution.started', actor: 'local-operator', event_hash: 'b'.repeat(64)},
		finished_event: null,
		...overrides.run,
	};
	const runJson = JSON.stringify(run).replace(/}$/, `,"generations":${generations}}`);
	return `{"version":1,"request_id":"request","data":{"type":"evolution","run":${runJson}}}`;
}

test('evolve commands carry the exact fields the daemon requires', () => {
	const start: Command = {command: 'evolve_start', run_id: 'run-1', world_id: 'world-1', from_genome_id: 'genome-1', generations: 3, budget: 6};
	const status: Command = {command: 'evolve_status', run_id: 'run-1'};
	const cancel: Command = {command: 'evolve_cancel', run_id: 'run-1'};
	assert.deepEqual(JSON.parse(JSON.stringify(start)), {command: 'evolve_start', run_id: 'run-1', world_id: 'world-1', from_genome_id: 'genome-1', generations: 3, budget: 6});
	assert.deepEqual(JSON.parse(JSON.stringify(status)), {command: 'evolve_status', run_id: 'run-1'});
	assert.deepEqual(JSON.parse(JSON.stringify(cancel)), {command: 'evolve_cancel', run_id: 'run-1'});
});

test('parseResponse accepts a running evolution projection with one recorded generation', () => {
	const response = parseResponse(evolutionResponseText(), 'request');
	assert.equal(response.data?.type, 'evolution');
	if (response.data?.type !== 'evolution') return;
	const {run} = response.data;
	assert.equal(run.run_id, 'run-1');
	assert.equal(run.state, 'running');
	assert.equal(run.cancel_requested, false);
	assert.equal(run.finish_reason, null);
	assert.equal(run.generations.length, 1);
	assert.equal(run.generations[0]?.promoted, true);
	assert.equal(run.generations[0]?.champion_after, 'child-0');
	assert.equal(run.started_event_id, 'evolution:run-1:started');
});

test('parseResponse accepts a finished evolution projection with a finish reason', () => {
	const text = evolutionResponseText({run: {state: 'finished', finish_reason: 'generations_exhausted'}});
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'evolution');
	if (response.data?.type !== 'evolution') return;
	assert.equal(response.data.run.state, 'finished');
	assert.equal(response.data.run.finish_reason, 'generations_exhausted');
});

test('parseResponse rejects an evolution run with an unknown state', () => {
	const text = evolutionResponseText({run: {state: 'evolving'}});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse rejects an evolution run with an unknown finish reason', () => {
	const text = evolutionResponseText({run: {state: 'finished', finish_reason: 'gave_up'}});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse rejects a generation entry missing required payload fields', () => {
	const brokenGenerations = `[{
		"payload": {"schema_version": 1, "run_id": "run-1", "generation_index": 0, "champion_before": "champion-0"},
		"event": {"sequence": 12, "event_id": "evolution:run-1:generation:0", "aggregate_id": "evolution:run-1", "event_type": "evolution.generation", "actor": "local-operator", "event_hash": "${'a'.repeat(64)}"}
	}]`;
	const text = evolutionResponseText({generations: brokenGenerations});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse rejects a generation whose promoted field is not boolean', () => {
	const tamperedGenerations = `[{
		"payload": {
			"schema_version": 1, "run_id": "run-1", "generation_index": 0, "champion_before": "champion-0",
			"diagnostic_evaluation_id": "evolve-run-1-g0-d", "proposal_id": "evolve-run-1-g0-p",
			"child_genome_id": "child-0", "child_evaluation_id": "evolve-run-1-g0-c",
			"assessment_id": "evolve-run-1-g0-a", "promoted": "yes", "champion_after": "child-0"
		},
		"event": {"sequence": 12, "event_id": "evolution:run-1:generation:0", "aggregate_id": "evolution:run-1", "event_type": "evolution.generation", "actor": "local-operator", "event_hash": "${'a'.repeat(64)}"}
	}]`;
	const text = evolutionResponseText({generations: tamperedGenerations});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse rejects an evolution run reported with a negative trial count', () => {
	const text = evolutionResponseText({run: {trials_consumed: -1}});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});
