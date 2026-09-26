import React from 'react';
import {Box, Text} from 'ink';
import {windowed, shortId} from './lineage-view.js';
import {borderColorProps, colorProps, useTheme} from './theme.js';
import {safeText, type MetaEvaluation, type MetaStrategy} from './protocol.js';

function formatX10000(value: number): string {
	return (value / 10000).toFixed(4);
}

export function MetaStrategyListPanel({strategies, selected, height}: {strategies: MetaStrategy[]; selected: number; height: number}) {
	const theme = useTheme();
	const view = windowed(strategies, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>EVOLVER STRATEGIES</Text>
		{strategies.length === 0 && <Text {...colorProps(theme.color('muted'))}>No strategies registered yet.</Text>}
		{view.items.map((strategy, index) => {
			const active = view.offset + index === selected;
			return <Text key={strategy.strategy_id} wrap="truncate" {...colorProps(theme.color(active ? 'judge' : 'ink'))}>
				{active ? theme.glyphs.caret + ' ' : '  '}{safeText(strategy.config.name)} <Text {...colorProps(theme.color('sealed'))}>{shortId(strategy.strategy_id)}</Text>{' '}
				<Text {...colorProps(theme.color('muted'))}>· {safeText(strategy.config.mutation_prioritization)} · {strategy.config.generation_count}gen</Text>
			</Text>;
		})}
	</Box>;
}

export function MetaStrategyDetailPanel({strategy, height}: {strategy: MetaStrategy | undefined; height: number}) {
	const theme = useTheme();
	if (!strategy) return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>STRATEGY</Text>
		<Text {...colorProps(theme.color('muted'))}>Loading…</Text>
	</Box>;
	const {config} = strategy;
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1} height={height}>
		<Text bold {...colorProps(theme.color('judge'))}>STRATEGY / {safeText(config.name)}</Text>
		<Text wrap="truncate">ID {shortId(strategy.strategy_id)}</Text>
		<Text wrap="truncate">Mutation prioritization {safeText(config.mutation_prioritization)}</Text>
		<Text wrap="truncate">Generations {config.generation_count} · Experiment allocation {config.experiment_allocation} · Candidates {config.candidate_count}</Text>
		<Text wrap="truncate">Gene selection {safeText(config.gene_selection)}</Text>
	</Box>;
}

export function MetaEvaluationListPanel({receipts, selected, height}: {receipts: MetaEvaluation[]; selected: number; height: number}) {
	const theme = useTheme();
	const view = windowed(receipts, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>META-EVALUATIONS</Text>
		{receipts.length === 0 && <Text {...colorProps(theme.color('muted'))}>No meta-evaluations recorded yet.</Text>}
		{view.items.map((receipt, index) => {
			const active = view.offset + index === selected;
			return <Text key={receipt.meta_run_id} wrap="truncate" {...colorProps(theme.color(active ? 'judge' : 'ink'))}>
				{active ? theme.glyphs.caret + ' ' : '  '}<Text {...colorProps(theme.color('sealed'))}>{shortId(receipt.meta_run_id)}</Text> <Text {...colorProps(theme.color('muted'))}>· {shortId(receipt.strategy_a_id)} vs {shortId(receipt.strategy_b_id)}</Text>{' '}
				<Text {...colorProps(theme.color('muted'))}>· {receipt.lineages.length} lineages</Text>
			</Text>;
		})}
	</Box>;
}

export function MetaEvaluationDetailPanel({receipt, height}: {receipt: MetaEvaluation | undefined; height: number}) {
	const theme = useTheme();
	if (!receipt) return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>META-EVALUATION</Text>
		<Text {...colorProps(theme.color('muted'))}>Loading…</Text>
	</Box>;
	const shown = receipt.lineages.slice(0, Math.max(1, height));
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>META-EVALUATION / <Text {...colorProps(theme.color('sealed'))}>{shortId(receipt.meta_run_id)}</Text></Text>
		<Text wrap="truncate">A {shortId(receipt.strategy_a_id)} vs B {shortId(receipt.strategy_b_id)}</Text>
		<Text wrap="truncate">Confidence {(receipt.confidence_bps / 100).toFixed(2)}% · {receipt.bootstrap_resamples} resamples · {safeText(receipt.algorithm)}</Text>
		<Text wrap="truncate">Quality delta {formatX10000(receipt.quality_delta.estimate_x10000)} [{formatX10000(receipt.quality_delta.lower_x10000)}, {formatX10000(receipt.quality_delta.upper_x10000)}]</Text>
		<Text wrap="truncate">Cost delta    {formatX10000(receipt.cost_delta.estimate_x10000)} [{formatX10000(receipt.cost_delta.lower_x10000)}, {formatX10000(receipt.cost_delta.upper_x10000)}]</Text>
		<Text {...colorProps(theme.color('muted'))}>LINEAGES ({receipt.lineages.length})</Text>
		{shown.map((lineage, index) => <Text key={`${lineage.world_id}-${index}`} wrap="truncate">
			{shortId(lineage.world_id)} <Text {...colorProps(theme.color('muted'))}>A promotions {lineage.strategy_a_promotions}/{lineage.strategy_a_trials_consumed}</Text>{' '}
			<Text {...colorProps(theme.color('muted'))}>B promotions {lineage.strategy_b_promotions}/{lineage.strategy_b_trials_consumed}</Text>
		</Text>)}
	</Box>;
}
