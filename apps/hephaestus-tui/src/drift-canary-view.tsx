import React from 'react';
import {Box, Text} from 'ink';
import {windowed, shortId} from './lineage-view.js';
import {DangerFlash, type FrameOptions} from './motion.js';
import {borderColorProps, colorProps, useTheme, type Role, type Theme} from './theme.js';
import {safeText, type Canary, type CanaryStage, type Drift} from './protocol.js';

const STAGE_ROLE: Record<CanaryStage, Role> = {
	pending: 'muted', stage5: 'ink', stage25: 'ink', stage50: 'ink', completed: 'improvement', aborted: 'danger',
};

/** The 5/25/50/100 canary rollout rail: a tick per stage, lit ember once reached, gold on the current stage, dim border otherwise. */
const STAGE_RAIL: ReadonlyArray<{stage: CanaryStage; label: string}> = [
	{stage: 'stage5', label: '5%'},
	{stage: 'stage25', label: '25%'},
	{stage: 'stage50', label: '50%'},
	{stage: 'completed', label: '100%'},
];
const STAGE_ORDER: CanaryStage[] = ['pending', 'stage5', 'stage25', 'stage50', 'completed'];

function StageRail({stage, theme}: {stage: CanaryStage; theme: Theme}) {
	const reachedIndex = STAGE_ORDER.indexOf(stage);
	return <Text>
		{STAGE_RAIL.map((tick, index) => {
			const tickIndex = STAGE_ORDER.indexOf(tick.stage);
			const isCurrent = tick.stage === stage;
			const reached = stage === 'aborted' ? tickIndex <= reachedIndex : tickIndex <= reachedIndex;
			const role: Role = stage === 'aborted' ? 'danger' : isCurrent ? 'judge' : reached ? 'champion' : 'muted';
			return <Text key={tick.stage}>
				{index > 0 && <Text {...colorProps(theme.color('muted'))}>─</Text>}
				<Text {...colorProps(theme.color(role), isCurrent)}>[{tick.label}]</Text>
			</Text>;
		})}
	</Text>;
}

export function DriftListPanel({drifts, selected, height}: {drifts: Drift[]; selected: number; height: number}) {
	const theme = useTheme();
	const view = windowed(drifts, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>DRIFT RECORDS</Text>
		{drifts.length === 0 && <Text {...colorProps(theme.color('muted'))}>No drift recorded yet.</Text>}
		{view.items.map((drift, index) => {
			const active = view.offset + index === selected;
			return <Text key={drift.drift_id} wrap="truncate" {...colorProps(theme.color(active ? 'judge' : 'ink'))}>
				{active ? theme.glyphs.caret + ' ' : '  '}<Text {...colorProps(theme.color('sealed'))}>{shortId(drift.drift_id)}</Text> <Text {...colorProps(theme.color('muted'))}>· {safeText(drift.kind)}</Text>{' '}
				<Text {...colorProps(theme.color(drift.observed_delta_bps < 0 ? 'regression' : 'ink'))}>{drift.observed_delta_bps}bps</Text>{' '}
				<Text {...colorProps(theme.color('muted'))}>(threshold {drift.threshold_bps}bps)</Text>
			</Text>;
		})}
	</Box>;
}

export function DriftDetailPanel({drift, height}: {drift: Drift | undefined; height: number}) {
	const theme = useTheme();
	if (!drift) return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>DRIFT</Text>
		<Text {...colorProps(theme.color('muted'))}>Loading…</Text>
	</Box>;
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1} height={height}>
		<Text bold {...colorProps(theme.color('judge'))}>DRIFT / <Text {...colorProps(theme.color('sealed'))}>{shortId(drift.drift_id)}</Text></Text>
		<Text wrap="truncate">World    {shortId(drift.world_id)}</Text>
		<Text wrap="truncate">Kind     {safeText(drift.kind)}</Text>
		<Text wrap="truncate">Baseline {shortId(drift.baseline_genome_id)} → Shifted {shortId(drift.shifted_genome_id)}</Text>
		<Text wrap="truncate">Observed {drift.observed_delta_bps}bps <Text {...colorProps(theme.color('muted'))}>(threshold {drift.threshold_bps}bps)</Text></Text>
		<Text wrap="truncate">Evidence {shortId(drift.evidence_evaluation_id)}</Text>
		<Text {...colorProps(theme.color('muted'))}>Drift never directly replaces a Champion.</Text>
	</Box>;
}

export function CanaryListPanel({canaries, selected, height, animate, frame}: {canaries: Canary[]; selected: number; height: number} & FrameOptions) {
	const theme = useTheme();
	const view = windowed(canaries, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>CANARIES</Text>
		{canaries.length === 0 && <Text {...colorProps(theme.color('muted'))}>No canaries started yet.</Text>}
		{view.items.map((canary, index) => {
			const active = view.offset + index === selected;
			return <Text key={canary.canary_id} wrap="truncate" {...colorProps(theme.color(active ? 'judge' : STAGE_ROLE[canary.stage]))}>
				{active ? theme.glyphs.caret + ' ' : '  '}<Text {...colorProps(theme.color('sealed'))}>{shortId(canary.canary_id)}</Text> <Text {...colorProps(theme.color('muted'))}>· {shortId(canary.candidate_genome_id)}</Text>{' '}
				<DangerFlash active={canary.stage === 'aborted'} animate={animate} frame={frame}><Text bold>{safeText(canary.stage.toUpperCase())}</Text></DangerFlash> <Text {...colorProps(theme.color('muted'))}>{canary.transitions.length} transitions</Text>
			</Text>;
		})}
	</Box>;
}

export function CanaryDetailPanel({canary, height, animate, frame}: {canary: Canary | undefined; height: number} & FrameOptions) {
	const theme = useTheme();
	if (!canary) return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>CANARY</Text>
		<Text {...colorProps(theme.color('muted'))}>Loading…</Text>
	</Box>;
	const shown = canary.transitions.slice(0, Math.max(1, height));
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>CANARY / <Text {...colorProps(theme.color('sealed'))}>{shortId(canary.canary_id)}</Text></Text>
		<Text wrap="truncate">Candidate {shortId(canary.candidate_genome_id)} <Text {...colorProps(theme.color('muted'))}>from {shortId(canary.previous_champion_genome_id)}</Text></Text>
		<Text wrap="truncate">Stage <DangerFlash active={canary.stage === 'aborted'} animate={animate} frame={frame}><Text bold {...colorProps(theme.color(STAGE_ROLE[canary.stage]))}>{safeText(canary.stage.toUpperCase())}</Text></DangerFlash></Text>
		<StageRail stage={canary.stage} theme={theme} />
		<Text {...colorProps(theme.color('muted'))}>TRANSITIONS ({canary.transitions.length})</Text>
		{shown.map((transition, index) => <Text key={`${transition.event_id}-${index}`} wrap="truncate" {...colorProps(theme.color(
			transition.kind === 'aborted' || transition.kind === 'live_regression_detected' ? 'danger' : 'ink',
		))}>
			{safeText(transition.kind)} → {safeText(transition.stage)}
			{transition.evidence && <Text {...colorProps(theme.color(transition.evidence.regressed ? 'regression' : 'improvement'))}> {transition.evidence.regressed ? 'regressed' : 'healthy'}</Text>}
			{transition.reason && <Text {...colorProps(theme.color('muted'))}> · {safeText(transition.reason)}</Text>}
		</Text>)}
	</Box>;
}
