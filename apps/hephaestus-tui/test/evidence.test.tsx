import test from 'node:test';
import assert from 'node:assert/strict';
import {parseResponse} from '../src/protocol.js';

function withRequest(dataJson: string): string {
	return `{"version":1,"request_id":"request","data":${dataJson}}`;
}

test('run_list parses a bounded list of run entries newest-first', () => {
	const text = withRequest(`{"type":"run_list","runs":[
		{"run_id":"async-1","job_id":"job-1","genome_id":"genome-1","world_id":"world-1","state":"succeeded","completion_reason":"success","latency_millis":12,"actual_cost_microusd":0},
		{"run_id":"reference-1","job_id":null,"genome_id":"genome-1","world_id":"world-1","state":"succeeded","completion_reason":"success","latency_millis":8,"actual_cost_microusd":0}
	]}`);
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'run_list');
	if (response.data?.type === 'run_list') {
		assert.equal(response.data.runs.length, 2);
		assert.equal(response.data.runs[0]?.job_id, 'job-1');
		assert.equal(response.data.runs[1]?.job_id, null);
	}
});

test('run_list rejects an unknown state or completion reason', () => {
	const badState = withRequest(`{"type":"run_list","runs":[{"run_id":"r","job_id":null,"genome_id":"g","world_id":null,"state":"unknown","completion_reason":null,"latency_millis":null,"actual_cost_microusd":null}]}`);
	assert.throws(() => parseResponse(badState, 'request'), /daemon response variant is invalid/);
	const badReason = withRequest(`{"type":"run_list","runs":[{"run_id":"r","job_id":null,"genome_id":"g","world_id":null,"state":"succeeded","completion_reason":"made_up_reason","latency_millis":null,"actual_cost_microusd":null}]}`);
	assert.throws(() => parseResponse(badReason, 'request'), /daemon response variant is invalid/);
});

test('run_list rejects more than 200 entries', () => {
	const entry = `{"run_id":"r","job_id":null,"genome_id":"g","world_id":null,"state":"succeeded","completion_reason":"success","latency_millis":1,"actual_cost_microusd":0}`;
	const runs = Array.from({length: 201}, () => entry).join(',');
	const text = withRequest(`{"type":"run_list","runs":[${runs}]}`);
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('evaluation_list accepts visible aggregates plus optional selection, invariant, and Forge evidence', () => {
	const text = withRequest(`{"type":"evaluation_list","evaluations":[{
		"evaluation":{"evaluation_id":"eval-1","world_id":"world-1","parent_genome_id":"parent-1","candidate_genome_id":"candidate-1","parent_visible_correct":1,"candidate_visible_correct":2,"visible_total":3},
		"selection":{"metrics_eligible":true,"estimate_bps":10,"lower_bps":-5,"upper_bps":25,"parent_cost_microusd":1,"candidate_cost_microusd":2,"parent_latency_millis":3,"candidate_latency_millis":4,"invariant_gate_verified":true,"promotion_eligible":false},
		"invariants":{"total_checks":4,"total_candidate_violations":0,"total_paired_regressions":0,"maximum_regressions":0,"regressions_within_budget":true,"candidate_contract_satisfied":true},
		"forge_assessment":{"assessment_id":"assessment-1","outcome":"metrics_passed"},
		"champion_transition_ids":["transition-1"]
	}]}`);
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'evaluation_list');
	if (response.data?.type === 'evaluation_list') {
		const entry = response.data.evaluations[0];
		assert.ok(entry);
		assert.equal(entry.evaluation.evaluation_id, 'eval-1');
		assert.equal(entry.selection?.metrics_eligible, true);
		assert.equal(entry.invariants?.candidate_contract_satisfied, true);
		assert.equal(entry.forge_assessment?.assessment_id, 'assessment-1');
		assert.deepEqual(entry.champion_transition_ids, ['transition-1']);
	}
});

test('evaluation_list allows evidence fields to be absent but never sealed or raw content', () => {
	const text = withRequest(`{"type":"evaluation_list","evaluations":[{
		"evaluation":{"evaluation_id":"eval-1","world_id":"world-1","parent_genome_id":"parent-1","candidate_genome_id":"candidate-1","parent_visible_correct":1,"candidate_visible_correct":2,"visible_total":3},
		"selection":null,"invariants":null,"forge_assessment":null,"champion_transition_ids":[]
	}]}`);
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'evaluation_list');
	if (response.data?.type === 'evaluation_list') {
		assert.equal(response.data.evaluations[0]?.selection, null);
	}
	assert.doesNotMatch(text, /sealed|expected_output|task_input|raw_output/i);
});

test('evaluation_list rejects an unrecognized Forge outcome', () => {
	const text = withRequest(`{"type":"evaluation_list","evaluations":[{
		"evaluation":{"evaluation_id":"eval-1","world_id":"world-1","parent_genome_id":"parent-1","candidate_genome_id":"candidate-1","parent_visible_correct":1,"candidate_visible_correct":2,"visible_total":3},
		"selection":null,"invariants":null,"forge_assessment":{"assessment_id":"a","outcome":"maybe"},"champion_transition_ids":[]
	}]}`);
	assert.throws(() => parseResponse(text, 'request'), /daemon response variant is invalid/);
});

test('denial_list accepts request-rejected and runtime-capability-denied entries newest-first', () => {
	const text = withRequest(`{"type":"denial_list","denials":[
		{"kind":"runtime_capability_denied","timestamp_millis":20,"request_id":null,"command":null,"run_id":"run-1","genome_id":"genome-1","world_id":"world-1","client_id":null},
		{"kind":"request_rejected","timestamp_millis":10,"request_id":"","command":"status","run_id":null,"genome_id":null,"world_id":null,"client_id":null}
	]}`);
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'denial_list');
	if (response.data?.type === 'denial_list') {
		assert.equal(response.data.denials.length, 2);
		assert.equal(response.data.denials[0]?.kind, 'runtime_capability_denied');
		assert.equal(response.data.denials[1]?.kind, 'request_rejected');
		assert.equal(response.data.denials[1]?.command, 'status');
	}
});

test('denial_list rejects an unknown kind and an oversized entry count', () => {
	const badKind = withRequest(`{"type":"denial_list","denials":[{"kind":"mystery","timestamp_millis":1,"request_id":null,"command":null,"run_id":null,"genome_id":null,"world_id":null,"client_id":null}]}`);
	assert.throws(() => parseResponse(badKind, 'request'), /daemon response variant is invalid/);
	const entry = `{"kind":"request_rejected","timestamp_millis":1,"request_id":"","command":"status","run_id":null,"genome_id":null,"world_id":null,"client_id":null}`;
	const denials = Array.from({length: 201}, () => entry).join(',');
	const oversized = withRequest(`{"type":"denial_list","denials":[${denials}]}`);
	assert.throws(() => parseResponse(oversized, 'request'), /daemon response variant is invalid/);
});
