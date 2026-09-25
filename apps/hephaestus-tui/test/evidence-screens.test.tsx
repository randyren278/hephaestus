import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {Box, renderToString} from 'ink';
import {aggregateCosts, formatLatency, formatMicroUsd, groupByWorld, totalDataRows, windowedGroups} from '../src/evidence.js';
import {CostsPanel, DenialsPanel, EvidencePanel, RunsPanel} from '../src/evidence-view.js';
import type {DenialEntry, EvaluationListEntry, RunListEntry} from '../src/protocol.js';

const runA: RunListEntry = {
	run_id: 'run-a', job_id: 'job-a', genome_id: 'genome-a', world_id: 'world-1',
	state: 'succeeded', completion_reason: 'success', latency_millis: 500, actual_cost_microusd: 1_500_000,
};
const runB: RunListEntry = {
	run_id: 'run-b', job_id: null, genome_id: 'genome-b', world_id: 'world-2',
	state: 'failed', completion_reason: 'provider_failure', latency_millis: 12_000, actual_cost_microusd: 250_000,
};
const runC: RunListEntry = {
	run_id: 'run-c', job_id: null, genome_id: 'genome-c', world_id: null,
	state: 'running', completion_reason: null, latency_millis: null, actual_cost_microusd: null,
};

test('formatMicroUsd and formatLatency render absent values as an em dash', () => {
	assert.equal(formatMicroUsd(null), '—');
	assert.equal(formatMicroUsd(1_500_000), '$1.5000');
	assert.equal(formatLatency(null), '—');
	assert.equal(formatLatency(500), '500ms');
	assert.equal(formatLatency(12_000), '12.00s');
});

test('groupByWorld sorts Worlds deterministically with unassigned last and preserves item order within a group', () => {
	const rows = groupByWorld([runB, runA, runC], run => run.world_id);
	assert.deepEqual(rows.map(row => row.kind === 'header' ? `H:${row.worldId ?? 'none'}` : `R:${row.item.run_id}`), [
		'H:world-1', 'R:run-a', 'H:world-2', 'R:run-b', 'H:none', 'R:run-c',
	]);
	assert.equal(totalDataRows(rows), 3);
});

test('windowedGroups keeps the selected data row inside the slice and headers are never selectable', () => {
	const rows = groupByWorld([runA, runB, runC], run => run.world_id);
	const view = windowedGroups(rows, 2, 3);
	assert.ok(view.rows.some(row => row.kind === 'row' && row.dataIndex === 2));
	for (const row of view.rows) if (row.kind === 'row') assert.notEqual(row.item, undefined);
});

test('aggregateCosts sums run costs and paired-evaluation parent/candidate costs per World and Genome', () => {
	const evaluation: EvaluationListEntry = {
		evaluation: {evaluation_id: 'e1', world_id: 'world-1', parent_genome_id: 'genome-a', candidate_genome_id: 'genome-d', parent_visible_correct: 1, candidate_visible_correct: 2, visible_total: 2},
		selection: {
			metrics_eligible: true, estimate_bps: 10, lower_bps: -5, upper_bps: 25,
			parent_cost_microusd: 500_000, candidate_cost_microusd: 700_000,
			parent_latency_millis: 1, candidate_latency_millis: 1, invariant_gate_verified: true, promotion_eligible: false,
		},
		invariants: null, forge_assessment: null, champion_transition_ids: [],
	};
	const costs = aggregateCosts([runA, runB], [evaluation]);
	const genomeA = costs.find(entry => entry.genome_id === 'genome-a');
	assert.ok(genomeA);
	assert.equal(genomeA?.total_microusd, 1_500_000 + 500_000);
	assert.equal(genomeA?.samples, 2);
	const genomeD = costs.find(entry => entry.genome_id === 'genome-d');
	assert.equal(genomeD?.total_microusd, 700_000);
	// Highest total first.
	assert.equal(costs[0]?.genome_id, 'genome-a');
});

test('aggregateCosts ignores runs with no verified cost', () => {
	const costs = aggregateCosts([runC], []);
	assert.equal(costs.length, 0);
});

test('RunsPanel renders World-separated headers and the run state/cost/latency', () => {
	const output = renderToString(<RunsPanel runs={[runA, runB]} selected={0} height={10} />);
	assert.match(output, /RUNS/);
	assert.match(output, /world-1/);
	assert.match(output, /world-2/);
	assert.match(output, /SUCCEEDED/);
	assert.match(output, /\$1\.5000/);
});

test('EvidencePanel renders selection, invariant, and Forge summaries without sealed content', () => {
	const evaluation: EvaluationListEntry = {
		evaluation: {evaluation_id: 'e1', world_id: 'world-1', parent_genome_id: 'genome-a', candidate_genome_id: 'genome-d', parent_visible_correct: 1, candidate_visible_correct: 2, visible_total: 2},
		selection: {
			metrics_eligible: true, estimate_bps: 42, lower_bps: -5, upper_bps: 90,
			parent_cost_microusd: 1, candidate_cost_microusd: 1, parent_latency_millis: 1, candidate_latency_millis: 1,
			invariant_gate_verified: true, promotion_eligible: true,
		},
		invariants: {total_checks: 4, total_candidate_violations: 0, total_paired_regressions: 0, maximum_regressions: 0, regressions_within_budget: true, candidate_contract_satisfied: true},
		forge_assessment: {assessment_id: 'a1', outcome: 'metrics_passed'},
		champion_transition_ids: ['t1'],
	};
	const output = renderToString(<EvidencePanel evaluations={[evaluation]} selected={0} height={10} />);
	assert.match(output, /42bps/);
	assert.match(output, /invariants ok/);
	assert.match(output, /Forge metrics_pas/);
	assert.doesNotMatch(output, /sealed|expected_output/i);
	const wide = renderToString(<Box width={200}><EvidencePanel evaluations={[evaluation]} selected={0} height={10} /></Box>);
	assert.match(wide, /1 Champion transition/);
});

test('CostsPanel shows a grand total and per-Genome totals grouped by World', () => {
	const costs = aggregateCosts([runA, runB], []);
	const output = renderToString(<CostsPanel costs={costs} selected={0} height={10} />);
	assert.match(output, /COSTS/);
	assert.match(output, /total \$1\.7500/);
});

test('DenialsPanel renders kind, command, and scope for both denial kinds', () => {
	const denials: DenialEntry[] = [
		{kind: 'runtime_capability_denied', timestamp_millis: 20, request_id: null, command: null, run_id: 'run-1', genome_id: 'genome-1', world_id: 'world-1', client_id: null},
		{kind: 'request_rejected', timestamp_millis: 10, request_id: 'r1', command: 'status', run_id: null, genome_id: null, world_id: null, client_id: null},
	];
	const output = renderToString(<DenialsPanel denials={denials} selected={0} height={10} />);
	assert.match(output, /runtime capability denied/);
	assert.match(output, /request rejected/);
	assert.match(output, /status/);
});
