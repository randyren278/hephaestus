import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {renderToString} from 'ink';
import {MetaEvaluationDetailPanel, MetaEvaluationListPanel, MetaStrategyDetailPanel, MetaStrategyListPanel} from '../src/meta-view.js';
import type {MetaEvaluation, MetaStrategy} from '../src/protocol.js';

const strategy: MetaStrategy = {
	strategy_id: 'strategy-1',
	config: {
		schema_version: 1, name: 'baseline', mutation_prioritization: 'fifo',
		generation_count: 3, experiment_allocation: 4, candidate_count: 2, gene_selection: 'none', parent_strategy_id: null,
	},
	event_id: 'event-1', sequence: 1,
};

test('MetaStrategyListPanel renders each strategy with its prioritization and generation count', () => {
	const output = renderToString(<MetaStrategyListPanel strategies={[strategy]} selected={0} height={10} />);
	assert.match(output, /EVOLVER STRATEGIES/);
	assert.match(output, /baseline/);
	assert.match(output, /fifo/);
	assert.match(output, /3gen/);
});

test('MetaStrategyListPanel reports no strategies explicitly', () => {
	const output = renderToString(<MetaStrategyListPanel strategies={[]} selected={0} height={10} />);
	assert.match(output, /No strategies registered yet/);
});

test('MetaStrategyDetailPanel shows a loading state before the strategy resolves', () => {
	const output = renderToString(<MetaStrategyDetailPanel strategy={undefined} height={10} />);
	assert.match(output, /Loading/);
});

test('MetaStrategyDetailPanel renders every EvolverStrategyConfig field', () => {
	const output = renderToString(<MetaStrategyDetailPanel strategy={strategy} height={10} />);
	assert.match(output, /baseline/);
	assert.match(output, /fifo/);
	assert.match(output, /Generations 3/);
	assert.match(output, /Candidates 2/);
	assert.match(output, /none/);
});

const receipt: MetaEvaluation = {
	meta_run_id: 'meta-1', strategy_a_id: 'strategy-a', strategy_b_id: 'strategy-b',
	confidence_bps: 9_500, bootstrap_seed: 7, bootstrap_resamples: 10_000, algorithm: 'lineage-paired-histogram-bootstrap-v1', descendant_cheaper_at_equal_quality: null,
	lineages: [{
		world_id: 'world-1', from_genome_id: 'genome-1', strategy_a_run_id: 'run-a', strategy_b_run_id: 'run-b',
		strategy_a_champion_genome_id: 'genome-a', strategy_b_champion_genome_id: 'genome-b',
		strategy_a_promotions: 2, strategy_b_promotions: 3, strategy_a_trials_consumed: 10, strategy_b_trials_consumed: 8,
	}],
	quality_delta: {estimate_x10000: 500, lower_x10000: 100, upper_x10000: 900},
	cost_delta: {estimate_x10000: -200, lower_x10000: -400, upper_x10000: 0},
	event_id: 'event-1', sequence: 1,
};

test('MetaEvaluationListPanel renders each receipt with its strategies and lineage count', () => {
	const output = renderToString(<MetaEvaluationListPanel receipts={[receipt]} selected={0} height={10} />);
	assert.match(output, /META-EVALUATIONS/);
	assert.match(output, /1 lineages/);
});

test('MetaEvaluationListPanel reports no meta-evaluations explicitly', () => {
	const output = renderToString(<MetaEvaluationListPanel receipts={[]} selected={0} height={10} />);
	assert.match(output, /No meta-evaluations recorded yet/);
});

test('MetaEvaluationDetailPanel shows a loading state before the receipt resolves', () => {
	const output = renderToString(<MetaEvaluationDetailPanel receipt={undefined} height={10} />);
	assert.match(output, /Loading/);
});

test('MetaEvaluationDetailPanel renders the bootstrap quality and cost intervals and per-lineage promotions', () => {
	const output = renderToString(<MetaEvaluationDetailPanel receipt={receipt} height={10} />);
	assert.match(output, /Quality delta 0\.0500 \[0\.0100, 0\.0900\]/);
	assert.match(output, /Cost delta\s+-0\.0200 \[-0\.0400, 0\.0000\]/);
	assert.match(output, /A promotions 2\/10/);
	assert.match(output, /B promotions 3\/8/);
});
