import React from 'react';
import {Box, Text} from 'ink';
import {windowed, shortId} from './lineage-view.js';
import {safeText, type MetaEvaluation, type MetaStrategy} from './protocol.js';

function formatX10000(value: number): string {
	return (value / 10000).toFixed(4);
}

export function MetaStrategyListPanel({strategies, selected, height}: {strategies: MetaStrategy[]; selected: number; height: number}) {
	const view = windowed(strategies, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">EVOLVER STRATEGIES</Text>
		{strategies.length === 0 && <Text color="gray">No strategies registered yet.</Text>}
		{view.items.map((strategy, index) => {
			const active = view.offset + index === selected;
			return <Text key={strategy.strategy_id} wrap="truncate" color={active ? 'yellow' : 'white'}>
				{active ? '› ' : '  '}{safeText(strategy.config.name)} <Text color="gray">{shortId(strategy.strategy_id)}</Text>{' '}
				<Text color="gray">· {safeText(strategy.config.mutation_prioritization)} · {strategy.config.generation_count}gen</Text>
			</Text>;
		})}
	</Box>;
}

export function MetaStrategyDetailPanel({strategy, height}: {strategy: MetaStrategy | undefined; height: number}) {
	if (!strategy) return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">STRATEGY</Text>
		<Text color="gray">Loading…</Text>
	</Box>;
	const {config} = strategy;
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1} height={height}>
		<Text bold color="yellow">STRATEGY / {safeText(config.name)}</Text>
		<Text wrap="truncate">ID {shortId(strategy.strategy_id)}</Text>
		<Text wrap="truncate">Mutation prioritization {safeText(config.mutation_prioritization)}</Text>
		<Text wrap="truncate">Generations {config.generation_count} · Experiment allocation {config.experiment_allocation} · Candidates {config.candidate_count}</Text>
		<Text wrap="truncate">Gene selection {safeText(config.gene_selection)}</Text>
	</Box>;
}

export function MetaEvaluationListPanel({receipts, selected, height}: {receipts: MetaEvaluation[]; selected: number; height: number}) {
	const view = windowed(receipts, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">META-EVALUATIONS</Text>
		{receipts.length === 0 && <Text color="gray">No meta-evaluations recorded yet.</Text>}
		{view.items.map((receipt, index) => {
			const active = view.offset + index === selected;
			return <Text key={receipt.meta_run_id} wrap="truncate" color={active ? 'yellow' : 'white'}>
				{active ? '› ' : '  '}{shortId(receipt.meta_run_id)} <Text color="gray">· {shortId(receipt.strategy_a_id)} vs {shortId(receipt.strategy_b_id)}</Text>{' '}
				<Text color="gray">· {receipt.lineages.length} lineages</Text>
			</Text>;
		})}
	</Box>;
}

export function MetaEvaluationDetailPanel({receipt, height}: {receipt: MetaEvaluation | undefined; height: number}) {
	if (!receipt) return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">META-EVALUATION</Text>
		<Text color="gray">Loading…</Text>
	</Box>;
	const shown = receipt.lineages.slice(0, Math.max(1, height));
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">META-EVALUATION / {shortId(receipt.meta_run_id)}</Text>
		<Text wrap="truncate">A {shortId(receipt.strategy_a_id)} vs B {shortId(receipt.strategy_b_id)}</Text>
		<Text wrap="truncate">Confidence {(receipt.confidence_bps / 100).toFixed(2)}% · {receipt.bootstrap_resamples} resamples · {safeText(receipt.algorithm)}</Text>
		<Text wrap="truncate">Quality delta {formatX10000(receipt.quality_delta.estimate_x10000)} [{formatX10000(receipt.quality_delta.lower_x10000)}, {formatX10000(receipt.quality_delta.upper_x10000)}]</Text>
		<Text wrap="truncate">Cost delta    {formatX10000(receipt.cost_delta.estimate_x10000)} [{formatX10000(receipt.cost_delta.lower_x10000)}, {formatX10000(receipt.cost_delta.upper_x10000)}]</Text>
		<Text color="gray">LINEAGES ({receipt.lineages.length})</Text>
		{shown.map((lineage, index) => <Text key={`${lineage.world_id}-${index}`} wrap="truncate">
			{shortId(lineage.world_id)} <Text color="gray">A promotions {lineage.strategy_a_promotions}/{lineage.strategy_a_trials_consumed}</Text>{' '}
			<Text color="gray">B promotions {lineage.strategy_b_promotions}/{lineage.strategy_b_trials_consumed}</Text>
		</Text>)}
	</Box>;
}
