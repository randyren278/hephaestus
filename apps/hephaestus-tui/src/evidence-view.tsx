import React from 'react';
import {Box, Text} from 'ink';
import {aggregateCosts, denialSummary, formatLatency, formatMicroUsd, formatWorldLabel, groupByWorld, totalDataRows, windowedGroups, type CostEntry} from './evidence.js';
import {shortId} from './lineage-view.js';
import {borderColorProps, colorProps, useTheme, type Role, type Theme} from './theme.js';
import {safeText, type DenialEntry, type EvaluationListEntry, type RunListEntry} from './protocol.js';

function Header({worldId, theme}: {worldId: string | null; theme: Theme}) {
	const label = worldId ? shortId(worldId) : formatWorldLabel(worldId);
	return <Text bold wrap="truncate" {...colorProps(theme.color(worldId ? 'judge' : 'muted'))}>── {safeText(label)} ──</Text>;
}

const RUN_STATE_ROLE: Record<string, Role> = {
	succeeded: 'improvement', failed: 'regression', interrupted: 'regression', cancellation_requested: 'judge', running: 'judge', admitted: 'muted',
};

/** Ids and hashes read as the evidence layer's "receipts" — gold, like the judge's seal, everywhere they appear in these panels. */
function Receipt({children, theme}: {children: React.ReactNode; theme: Theme}) {
	return <Text {...colorProps(theme.color('sealed'))}>{children}</Text>;
}

export function RunsPanel({runs, selected, height}: {runs: RunListEntry[]; selected: number; height: number}) {
	const theme = useTheme();
	const grouped = groupByWorld(runs, run => run.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>RUNS <Text {...colorProps(theme.color('muted'))}>({total})</Text></Text>
		{runs.length === 0 && <Text {...colorProps(theme.color('muted'))}>No runs recorded yet.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} theme={theme} />
			: <Text key={row.item.run_id} wrap="truncate" {...colorProps(theme.color(row.dataIndex === selected ? 'judge' : 'ink'))}>
				{row.dataIndex === selected ? theme.glyphs.caret + ' ' : '  '}
				<Text {...colorProps(theme.color(RUN_STATE_ROLE[row.item.state] ?? 'ink'))}>{safeText(row.item.state.toUpperCase())}</Text>
				{' '}<Receipt theme={theme}>{shortId(row.item.genome_id)}</Receipt> · {formatLatency(row.item.latency_millis)} · {formatMicroUsd(row.item.actual_cost_microusd)}
				{row.item.completion_reason && <Text {...colorProps(theme.color('muted'))}> · {safeText(row.item.completion_reason)}</Text>}
			</Text>)}
	</Box>;
}

export function EvidencePanel({evaluations, selected, height}: {evaluations: EvaluationListEntry[]; selected: number; height: number}) {
	const theme = useTheme();
	const grouped = groupByWorld(evaluations, entry => entry.evaluation.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>EVIDENCE RECEIPTS <Text {...colorProps(theme.color('muted'))}>({total})</Text></Text>
		{evaluations.length === 0 && <Text {...colorProps(theme.color('muted'))}>No Arena evaluations recorded yet.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} theme={theme} />
			: <Text key={row.item.evaluation.evaluation_id} wrap="truncate" {...colorProps(theme.color(row.dataIndex === selected ? 'judge' : 'ink'))}>
				{row.dataIndex === selected ? theme.glyphs.caret + ' ' : '  '}
				<Receipt theme={theme}>{shortId(row.item.evaluation.parent_genome_id)}→{shortId(row.item.evaluation.candidate_genome_id)}</Receipt>
				{' '}{row.item.evaluation.candidate_visible_correct}/{row.item.evaluation.visible_total} visible
				{row.item.selection && <Text {...colorProps(theme.color(row.item.selection.promotion_eligible ? 'improvement' : 'muted'))}> · {row.item.selection.estimate_bps}bps</Text>}
				{row.item.invariants && <Text {...colorProps(theme.color(row.item.invariants.candidate_contract_satisfied ? 'improvement' : 'regression'))}> · invariants {row.item.invariants.candidate_contract_satisfied ? 'ok' : `${row.item.invariants.total_candidate_violations} viol`}</Text>}
				{row.item.forge_assessment && <Text {...colorProps(theme.color(row.item.forge_assessment.outcome === 'metrics_passed' ? 'improvement' : 'regression'))}> · Forge {safeText(row.item.forge_assessment.outcome)}</Text>}
				{row.item.champion_transition_ids.length > 0 && <Text {...colorProps(theme.color('judge'))}> · {row.item.champion_transition_ids.length} Champion transition(s)</Text>}
			</Text>)}
	</Box>;
}

export function CostsPanel({costs, selected, height}: {costs: CostEntry[]; selected: number; height: number}) {
	const theme = useTheme();
	const grouped = groupByWorld(costs, entry => entry.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	const grandTotal = costs.reduce((sum, entry) => sum + entry.total_microusd, 0);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>COSTS <Text {...colorProps(theme.color('muted'))}>({total}) · total {formatMicroUsd(grandTotal)}</Text></Text>
		{costs.length === 0 && <Text {...colorProps(theme.color('muted'))}>No costed runs or evaluations yet.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} theme={theme} />
			: <Text key={`${row.item.world_id ?? ''}-${row.item.genome_id}`} wrap="truncate" {...colorProps(theme.color(row.dataIndex === selected ? 'judge' : 'ink'))}>
				{row.dataIndex === selected ? theme.glyphs.caret + ' ' : '  '}<Receipt theme={theme}>{shortId(row.item.genome_id)}</Receipt> · {formatMicroUsd(row.item.total_microusd)} <Text {...colorProps(theme.color('muted'))}>({row.item.samples} samples)</Text>
			</Text>)}
	</Box>;
}

const DENIAL_KIND_ROLE: Record<string, Role> = {request_rejected: 'judge', runtime_capability_denied: 'danger'};

export function DenialsPanel({denials, selected, height}: {denials: DenialEntry[]; selected: number; height: number}) {
	const theme = useTheme();
	const grouped = groupByWorld(denials, denial => denial.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>DENIALS <Text {...colorProps(theme.color('muted'))}>({total})</Text></Text>
		{denials.length === 0 && <Text {...colorProps(theme.color('muted'))}>No denials recorded.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} theme={theme} />
			: <Text key={`${row.item.kind}-${row.item.timestamp_millis}-${row.dataIndex}`} wrap="truncate" {...colorProps(theme.color(row.dataIndex === selected ? 'judge' : 'ink'))}>
				{row.dataIndex === selected ? theme.glyphs.caret + ' ' : '  '}
				<Text {...colorProps(theme.color(DENIAL_KIND_ROLE[row.item.kind] ?? 'ink'))}>{safeText(row.item.kind.replace(/_/g, ' '))}</Text>
				{' '}{safeText(denialSummary(row.item))}
			</Text>)}
	</Box>;
}
