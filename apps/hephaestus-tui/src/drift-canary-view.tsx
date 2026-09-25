import React from 'react';
import {Box, Text} from 'ink';
import {windowed, shortId} from './lineage-view.js';
import {safeText, type Canary, type CanaryStage, type Drift} from './protocol.js';

const STAGE_COLOR: Record<CanaryStage, string> = {
	pending: 'gray', stage5: 'white', stage25: 'white', stage50: 'white', completed: 'green', aborted: 'red',
};

export function DriftListPanel({drifts, selected, height}: {drifts: Drift[]; selected: number; height: number}) {
	const view = windowed(drifts, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">DRIFT RECORDS</Text>
		{drifts.length === 0 && <Text color="gray">No drift recorded yet.</Text>}
		{view.items.map((drift, index) => {
			const active = view.offset + index === selected;
			return <Text key={drift.drift_id} wrap="truncate" color={active ? 'yellow' : 'white'}>
				{active ? '› ' : '  '}{shortId(drift.drift_id)} <Text color="gray">· {safeText(drift.kind)}</Text>{' '}
				<Text color={drift.observed_delta_bps < 0 ? 'red' : 'white'}>{drift.observed_delta_bps}bps</Text>{' '}
				<Text color="gray">(threshold {drift.threshold_bps}bps)</Text>
			</Text>;
		})}
	</Box>;
}

export function DriftDetailPanel({drift, height}: {drift: Drift | undefined; height: number}) {
	if (!drift) return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">DRIFT</Text>
		<Text color="gray">Loading…</Text>
	</Box>;
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1} height={height}>
		<Text bold color="yellow">DRIFT / {shortId(drift.drift_id)}</Text>
		<Text wrap="truncate">World    {shortId(drift.world_id)}</Text>
		<Text wrap="truncate">Kind     {safeText(drift.kind)}</Text>
		<Text wrap="truncate">Baseline {shortId(drift.baseline_genome_id)} → Shifted {shortId(drift.shifted_genome_id)}</Text>
		<Text wrap="truncate">Observed {drift.observed_delta_bps}bps <Text color="gray">(threshold {drift.threshold_bps}bps)</Text></Text>
		<Text wrap="truncate">Evidence {shortId(drift.evidence_evaluation_id)}</Text>
		<Text color="gray">Drift never directly replaces a Champion.</Text>
	</Box>;
}

export function CanaryListPanel({canaries, selected, height}: {canaries: Canary[]; selected: number; height: number}) {
	const view = windowed(canaries, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">CANARIES</Text>
		{canaries.length === 0 && <Text color="gray">No canaries started yet.</Text>}
		{view.items.map((canary, index) => {
			const active = view.offset + index === selected;
			return <Text key={canary.canary_id} wrap="truncate" color={active ? 'yellow' : STAGE_COLOR[canary.stage]}>
				{active ? '› ' : '  '}{shortId(canary.canary_id)} <Text color="gray">· {shortId(canary.candidate_genome_id)}</Text>{' '}
				<Text bold>{safeText(canary.stage.toUpperCase())}</Text> <Text color="gray">{canary.transitions.length} transitions</Text>
			</Text>;
		})}
	</Box>;
}

export function CanaryDetailPanel({canary, height}: {canary: Canary | undefined; height: number}) {
	if (!canary) return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">CANARY</Text>
		<Text color="gray">Loading…</Text>
	</Box>;
	const shown = canary.transitions.slice(0, Math.max(1, height));
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">CANARY / {shortId(canary.canary_id)}</Text>
		<Text wrap="truncate">Candidate {shortId(canary.candidate_genome_id)} <Text color="gray">from {shortId(canary.previous_champion_genome_id)}</Text></Text>
		<Text wrap="truncate">Stage <Text bold color={STAGE_COLOR[canary.stage]}>{safeText(canary.stage.toUpperCase())}</Text></Text>
		<Text color="gray">TRANSITIONS ({canary.transitions.length})</Text>
		{shown.map((transition, index) => <Text key={`${transition.event_id}-${index}`} wrap="truncate" color={
			transition.kind === 'aborted' || transition.kind === 'live_regression_detected' ? 'red' : 'white'
		}>
			{safeText(transition.kind)} → {safeText(transition.stage)}
			{transition.evidence && <Text color={transition.evidence.regressed ? 'red' : 'green'}> {transition.evidence.regressed ? 'regressed' : 'healthy'}</Text>}
			{transition.reason && <Text color="gray"> · {safeText(transition.reason)}</Text>}
		</Text>)}
	</Box>;
}
