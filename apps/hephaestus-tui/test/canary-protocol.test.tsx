import test from 'node:test';
import assert from 'node:assert/strict';
import type {Command} from '../src/protocol.js';
import {parseResponse} from '../src/protocol.js';

function driftResponseText(overrides: Record<string, unknown> = {}): string {
	const payload = {
		schema_version: 1, drift_id: 'drift-1', world_id: 'world-1', kind: 'correctness',
		evidence_evaluation_id: 'evaluation-1', selection_event_id: 'arena:selection:evaluation-1:selected',
		selection_event_hash: 'a'.repeat(64), baseline_genome_id: 'champion-genome', shifted_genome_id: 'candidate-genome',
		threshold_bps: 500, observed_delta_bps: -10000,
		...overrides,
	};
	const event = {sequence: 4, event_id: 'drift:drift-1:recorded', aggregate_id: 'drift:world-1', event_hash: 'b'.repeat(64)};
	const adaptation = {started: false, canary_id: null, canary_stage: null, finished: false, finish_reason: null};
	return `{"version":1,"request_id":"request","data":{"type":"drift","drift":${JSON.stringify({payload, event, adaptation})}}}`;
}

function canaryTransitionResponseText(overrides: Record<string, unknown> = {}): string {
	const payload = {
		schema_version: 1, canary_id: 'canary-1', world_id: 'world-1', kind: 'advanced', stage: 'stage5',
		candidate_genome_id: 'candidate-genome', previous_champion_genome_id: 'champion-genome',
		assessment_id: 'assessment-1',
		evidence: {
			evidence_evaluation_id: 'evaluation-2', selection_event_id: 'arena:selection:evaluation-2:selected',
			selection_event_hash: 'c'.repeat(64), latency_delta_bps: 10, cost_delta_bps: 0,
			correctness_delta_bps: 0, reliability_delta_bps: 0, regressed: false,
		},
		champion_promotion: null, champion_rollback_event_id: null, champion_rollback_event_hash: null, reason: null,
		...overrides,
	};
	const event = {sequence: 7, event_id: 'canary:canary-1:advance:evaluation-2', aggregate_id: 'canary:canary-1', event_hash: 'd'.repeat(64)};
	return `{"version":1,"request_id":"request","data":{"type":"canary_transition","transition":${JSON.stringify({payload, event})}}}`;
}

function canaryResponseText(transitionOverrides: Record<string, unknown> = {}, canaryOverrides: Record<string, unknown> = {}): string {
	const transitionPayload = {
		schema_version: 1, canary_id: 'canary-1', world_id: 'world-1', kind: 'started', stage: 'pending',
		candidate_genome_id: 'candidate-genome', previous_champion_genome_id: 'champion-genome',
		assessment_id: 'assessment-1', evidence: null, champion_promotion: null,
		champion_rollback_event_id: null, champion_rollback_event_hash: null, reason: null,
		...transitionOverrides,
	};
	const transitionEvent = {sequence: 3, event_id: 'canary:canary-1:started', aggregate_id: 'canary:canary-1', event_hash: 'e'.repeat(64)};
	const canary = {
		canary_id: 'canary-1', world_id: 'world-1', candidate_genome_id: 'candidate-genome',
		previous_champion_genome_id: 'champion-genome', stage: 'pending',
		transitions: [{payload: transitionPayload, event: transitionEvent}],
		...canaryOverrides,
	};
	return `{"version":1,"request_id":"request","data":{"type":"canary","canary":${JSON.stringify(canary)}}}`;
}

test('drift and canary commands carry the exact fields the daemon requires', () => {
	const drift: Command = {command: 'drift_record', drift_id: 'drift-1', world_id: 'world-1', kind: 'latency', evidence_evaluation_id: 'evaluation-1'};
	const start: Command = {command: 'canary_start', canary_id: 'canary-1', world_id: 'world-1', candidate_genome_id: 'genome-2', assessment_id: 'assessment-1'};
	const advance: Command = {command: 'canary_advance', canary_id: 'canary-1', evidence_evaluation_id: 'evaluation-2'};
	const liveCheck: Command = {command: 'canary_live_check', canary_id: 'canary-1', evidence_evaluation_id: 'evaluation-3'};
	const show: Command = {command: 'canary_show', canary_id: 'canary-1'};
	assert.deepEqual(JSON.parse(JSON.stringify(drift)), {command: 'drift_record', drift_id: 'drift-1', world_id: 'world-1', kind: 'latency', evidence_evaluation_id: 'evaluation-1'});
	assert.deepEqual(JSON.parse(JSON.stringify(start)), {command: 'canary_start', canary_id: 'canary-1', world_id: 'world-1', candidate_genome_id: 'genome-2', assessment_id: 'assessment-1'});
	assert.deepEqual(JSON.parse(JSON.stringify(advance)), {command: 'canary_advance', canary_id: 'canary-1', evidence_evaluation_id: 'evaluation-2'});
	assert.deepEqual(JSON.parse(JSON.stringify(liveCheck)), {command: 'canary_live_check', canary_id: 'canary-1', evidence_evaluation_id: 'evaluation-3'});
	assert.deepEqual(JSON.parse(JSON.stringify(show)), {command: 'canary_show', canary_id: 'canary-1'});
});

test('parseResponse accepts a drift record', () => {
	const response = parseResponse(driftResponseText(), 'request');
	assert.equal(response.data?.type, 'drift');
	if (response.data?.type !== 'drift') return;
	assert.equal(response.data.drift.drift_id, 'drift-1');
	assert.equal(response.data.drift.kind, 'correctness');
	assert.equal(response.data.drift.observed_delta_bps, -10000);
	assert.equal(response.data.drift.threshold_bps, 500);
});

test('parseResponse rejects a drift record with an unknown kind', () => {
	const text = driftResponseText({kind: 'network'});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse rejects a drift record with a non-integer observed delta', () => {
	const text = driftResponseText({observed_delta_bps: 1.5});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse accepts a healthy canary advance transition', () => {
	const response = parseResponse(canaryTransitionResponseText(), 'request');
	assert.equal(response.data?.type, 'canary_transition');
	if (response.data?.type !== 'canary_transition') return;
	const {transition} = response.data;
	assert.equal(transition.stage, 'stage5');
	assert.equal(transition.kind, 'advanced');
	assert.equal(transition.evidence?.regressed, false);
	assert.equal(transition.evidence?.latency_delta_bps, 10);
});

test('parseResponse accepts an aborted canary transition with a reason and no evidence regression gap', () => {
	const text = canaryTransitionResponseText({
		kind: 'aborted', stage: 'aborted', reason: 'staged health evidence regressed beyond the documented threshold',
		evidence: {
			evidence_evaluation_id: 'evaluation-2', selection_event_id: 'arena:selection:evaluation-2:selected',
			selection_event_hash: 'c'.repeat(64), latency_delta_bps: 0, cost_delta_bps: 0,
			correctness_delta_bps: -6000, reliability_delta_bps: 0, regressed: true,
		},
	});
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'canary_transition');
	if (response.data?.type !== 'canary_transition') return;
	assert.equal(response.data.transition.stage, 'aborted');
	assert.equal(response.data.transition.evidence?.regressed, true);
	assert.ok(response.data.transition.reason?.includes('regressed'));
});

test('parseResponse rejects a canary transition with an unknown stage', () => {
	const text = canaryTransitionResponseText({stage: 'stage10'});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse rejects a canary transition whose evidence is malformed', () => {
	const text = canaryTransitionResponseText({evidence: {evidence_evaluation_id: 'evaluation-2'}});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse accepts a canary projection with its transition history', () => {
	const response = parseResponse(canaryResponseText(), 'request');
	assert.equal(response.data?.type, 'canary');
	if (response.data?.type !== 'canary') return;
	const {canary} = response.data;
	assert.equal(canary.canary_id, 'canary-1');
	assert.equal(canary.stage, 'pending');
	assert.equal(canary.transitions.length, 1);
	assert.equal(canary.transitions[0]?.kind, 'started');
	assert.equal(canary.transitions[0]?.evidence, null);
});

test('parseResponse rejects a canary projection with a tampered transition list', () => {
	const text = canaryResponseText({}, {transitions: [{payload: {}, event: {}}]});
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('drift_list and canary_list commands carry the exact fields the daemon requires', () => {
	const driftList: Command = {command: 'drift_list', limit: 50};
	const canaryList: Command = {command: 'canary_list', limit: 50};
	assert.deepEqual(JSON.parse(JSON.stringify(driftList)), {command: 'drift_list', limit: 50});
	assert.deepEqual(JSON.parse(JSON.stringify(canaryList)), {command: 'canary_list', limit: 50});
});

function driftRecord(): unknown {
	const payload = {
		schema_version: 1, drift_id: 'drift-1', world_id: 'world-1', kind: 'correctness',
		evidence_evaluation_id: 'evaluation-1', selection_event_id: 'arena:selection:evaluation-1:selected',
		selection_event_hash: 'a'.repeat(64), baseline_genome_id: 'champion-genome', shifted_genome_id: 'candidate-genome',
		threshold_bps: 500, observed_delta_bps: -10000,
	};
	const event = {sequence: 4, event_id: 'drift:drift-1:recorded', aggregate_id: 'drift:world-1', event_hash: 'b'.repeat(64)};
	const adaptation = {started: false, canary_id: null, canary_stage: null, finished: false, finish_reason: null};
	return {payload, event, adaptation};
}

function canaryRecord(): unknown {
	const transitionPayload = {
		schema_version: 1, canary_id: 'canary-1', world_id: 'world-1', kind: 'started', stage: 'pending',
		candidate_genome_id: 'candidate-genome', previous_champion_genome_id: 'champion-genome',
		assessment_id: 'assessment-1', evidence: null, champion_promotion: null,
		champion_rollback_event_id: null, champion_rollback_event_hash: null, reason: null,
	};
	const transitionEvent = {sequence: 3, event_id: 'canary:canary-1:started', aggregate_id: 'canary:canary-1', event_hash: 'e'.repeat(64)};
	return {
		canary_id: 'canary-1', world_id: 'world-1', candidate_genome_id: 'candidate-genome',
		previous_champion_genome_id: 'champion-genome', stage: 'pending',
		transitions: [{payload: transitionPayload, event: transitionEvent}],
	};
}

test('parseResponse accepts a bounded, newest-first drift list', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"drift_list","drifts":[${JSON.stringify(driftRecord())}]}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'drift_list');
	if (response.data?.type !== 'drift_list') return;
	assert.equal(response.data.drifts.length, 1);
	assert.equal(response.data.drifts[0]?.drift_id, 'drift-1');
});

test('parseResponse rejects a drift list containing an invalid entry', () => {
	const text = '{"version":1,"request_id":"request","data":{"type":"drift_list","drifts":[{}]}}';
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('parseResponse accepts a bounded, newest-first canary list', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"canary_list","canaries":[${JSON.stringify(canaryRecord())}]}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'canary_list');
	if (response.data?.type !== 'canary_list') return;
	assert.equal(response.data.canaries.length, 1);
	assert.equal(response.data.canaries[0]?.canary_id, 'canary-1');
});

test('parseResponse rejects a canary list containing an invalid entry', () => {
	const text = '{"version":1,"request_id":"request","data":{"type":"canary_list","canaries":[{}]}}';
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});
