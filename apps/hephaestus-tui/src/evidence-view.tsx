import React from 'react';
import {Box, Text} from 'ink';
import {aggregateCosts, denialSummary, formatLatency, formatMicroUsd, formatWorldLabel, groupByWorld, totalDataRows, windowedGroups, type CostEntry} from './evidence.js';
import {shortId} from './lineage-view.js';
import {safeText, type DenialEntry, type EvaluationListEntry, type RunListEntry} from './protocol.js';

function Header({worldId}: {worldId: string | null}) {
	const label = worldId ? shortId(worldId) : formatWorldLabel(worldId);
	return <Text bold wrap="truncate" color={worldId ? 'cyan' : 'gray'}>── {safeText(label)} ──</Text>;
}

const RUN_STATE_COLOR: Record<string, string> = {
	succeeded: 'green', failed: 'red', interrupted: 'red', cancellation_requested: 'yellow', running: 'yellow', admitted: 'gray',
};

export function RunsPanel({runs, selected, height}: {runs: RunListEntry[]; selected: number; height: number}) {
	const grouped = groupByWorld(runs, run => run.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">RUNS <Text color="gray">({total})</Text></Text>
		{runs.length === 0 && <Text color="gray">No runs recorded yet.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} />
			: <Text key={row.item.run_id} wrap="truncate" color={row.dataIndex === selected ? 'yellow' : 'white'}>
				{row.dataIndex === selected ? '› ' : '  '}
				<Text color={RUN_STATE_COLOR[row.item.state] ?? 'white'}>{safeText(row.item.state.toUpperCase())}</Text>
				{' '}{shortId(row.item.genome_id)} · {formatLatency(row.item.latency_millis)} · {formatMicroUsd(row.item.actual_cost_microusd)}
				{row.item.completion_reason && <Text color="gray"> · {safeText(row.item.completion_reason)}</Text>}
			</Text>)}
	</Box>;
}

export function EvidencePanel({evaluations, selected, height}: {evaluations: EvaluationListEntry[]; selected: number; height: number}) {
	const grouped = groupByWorld(evaluations, entry => entry.evaluation.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">EVIDENCE RECEIPTS <Text color="gray">({total})</Text></Text>
		{evaluations.length === 0 && <Text color="gray">No Arena evaluations recorded yet.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} />
			: <Text key={row.item.evaluation.evaluation_id} wrap="truncate" color={row.dataIndex === selected ? 'yellow' : 'white'}>
				{row.dataIndex === selected ? '› ' : '  '}
				{shortId(row.item.evaluation.parent_genome_id)}→{shortId(row.item.evaluation.candidate_genome_id)}
				{' '}{row.item.evaluation.candidate_visible_correct}/{row.item.evaluation.visible_total} visible
				{row.item.selection && <Text color={row.item.selection.promotion_eligible ? 'green' : 'gray'}> · {row.item.selection.estimate_bps}bps</Text>}
				{row.item.invariants && <Text color={row.item.invariants.candidate_contract_satisfied ? 'green' : 'red'}> · invariants {row.item.invariants.candidate_contract_satisfied ? 'ok' : `${row.item.invariants.total_candidate_violations} viol`}</Text>}
				{row.item.forge_assessment && <Text color={row.item.forge_assessment.outcome === 'metrics_passed' ? 'green' : 'red'}> · Forge {safeText(row.item.forge_assessment.outcome)}</Text>}
				{row.item.champion_transition_ids.length > 0 && <Text color="yellow"> · {row.item.champion_transition_ids.length} Champion transition(s)</Text>}
			</Text>)}
	</Box>;
}

export function CostsPanel({costs, selected, height}: {costs: CostEntry[]; selected: number; height: number}) {
	const grouped = groupByWorld(costs, entry => entry.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	const grandTotal = costs.reduce((sum, entry) => sum + entry.total_microusd, 0);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">COSTS <Text color="gray">({total}) · total {formatMicroUsd(grandTotal)}</Text></Text>
		{costs.length === 0 && <Text color="gray">No costed runs or evaluations yet.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} />
			: <Text key={`${row.item.world_id ?? ''}-${row.item.genome_id}`} wrap="truncate" color={row.dataIndex === selected ? 'yellow' : 'white'}>
				{row.dataIndex === selected ? '› ' : '  '}{shortId(row.item.genome_id)} · {formatMicroUsd(row.item.total_microusd)} <Text color="gray">({row.item.samples} samples)</Text>
			</Text>)}
	</Box>;
}

const DENIAL_KIND_COLOR: Record<string, string> = {request_rejected: 'yellow', runtime_capability_denied: 'red'};

export function DenialsPanel({denials, selected, height}: {denials: DenialEntry[]; selected: number; height: number}) {
	const grouped = groupByWorld(denials, denial => denial.world_id);
	const total = totalDataRows(grouped);
	const view = windowedGroups(grouped, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">DENIALS <Text color="gray">({total})</Text></Text>
		{denials.length === 0 && <Text color="gray">No denials recorded.</Text>}
		{view.rows.map((row, index) => row.kind === 'header'
			? <Header key={`h-${row.worldId ?? 'none'}-${index}`} worldId={row.worldId} />
			: <Text key={`${row.item.kind}-${row.item.timestamp_millis}-${row.dataIndex}`} wrap="truncate" color={row.dataIndex === selected ? 'yellow' : 'white'}>
				{row.dataIndex === selected ? '› ' : '  '}
				<Text color={DENIAL_KIND_COLOR[row.item.kind] ?? 'white'}>{safeText(row.item.kind.replace(/_/g, ' '))}</Text>
				{' '}{safeText(denialSummary(row.item))}
			</Text>)}
	</Box>;
}
