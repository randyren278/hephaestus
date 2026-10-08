import React, {useEffect, useRef, useState} from 'react';
import {Box, Text, useApp, useInput, useWindowSize} from 'ink';
import {launchEditor} from './author.js';
import type {ControlClient} from './client.js';
import {safeText, type EvaluationListEntry, type GenomeProfile} from './protocol.js';
import {RevisionController, type RevisionSource, type RevisionState} from './revision.js';
import {RevisionWorkspace, type PromptChange, type RevisionCursor, type RevisionListing} from './revision-workspace.js';
import {colorProps, useTheme} from './theme.js';

export function reportedDollars(microusd: string): string {
	const value = BigInt(microusd);
	return `$${value / 1_000_000n}.${String(value % 1_000_000n).padStart(6, '0')}`;
}

export function RevisionProfilePanel({profile}: {profile: GenomeProfile}) {
	const trials = 2 * (profile.visible_tasks + profile.sealed_tasks);
	return <>
		<Text wrap="truncate">World: {safeText(profile.world.name)} / {safeText(profile.world.world_id)}</Text>
		<Text wrap="truncate">Model: {safeText(profile.provider)} / {safeText(profile.family)}</Text>
		<Text wrap="truncate">Authority: write {profile.workspace_write ? 'yes' : 'no'} · network {profile.network ? 'yes' : 'no'}</Text>
		<Text wrap="truncate">Scoring: {profile.output_scoring} · harness revisions {profile.harness_mutation_allowed ? 'allowed' : 'blocked'}</Text>
		<Text wrap="truncate">Tasks: {profile.visible_tasks} visible + {profile.sealed_tasks} sealed · {trials} trials</Text>
		<Text wrap="truncate">Each trial: {profile.paired_trial_wall_millis / 1000}s · {profile.paired_trial_output_bytes} output bytes</Text>
		<Text wrap="truncate">Total wall limit: {profile.paired_total_wall_millis / 1000}s</Text>
		<Text wrap="truncate">Reported cost limit: {reportedDollars(profile.reported_cost_limit_microusd)} / trial</Text>
		<Text wrap="truncate">Total reported limit: {reportedDollars(String(BigInt(profile.reported_cost_limit_microusd) * BigInt(trials)))}</Text>
		<Text dimColor wrap="truncate">Provider billing and subscription quotas are outside these reported limits.</Text>
	</>;
}

export function confirmationFits(profile: GenomeProfile, columns: number, rows: number): boolean {
	return ['claude', 'codex'].includes(profile.provider) && /^[A-Za-z0-9._:\/-]{1,128}$/.test(profile.family)
		&& safeText(profile.family) === profile.family && columns >= 80 && rows >= 22
		&& `Model: ${profile.provider} / ${profile.family}`.length <= columns - 2;
}

type Screen = 'sources' | 'drafts' | 'id' | 'source' | 'hypothesis' | 'review' | 'state' | 'cancel';
type Props = {client: Pick<ControlClient, 'request'>; dataDir: string; onExit: () => void; pollMs?: number};

/** Explicit record, execution and assessment steps over the durable revision controller. */
export function RevisionScreen({client, dataDir, onExit, pollMs = 1500}: Props) {
	const {suspendTerminal} = useApp();
	const theme = useTheme();
	const {columns = 80, rows = 24} = useWindowSize();
	const [lifetime] = useState(() => new AbortController());
	const [workspace] = useState(() => new RevisionWorkspace(dataDir));
	const [controller] = useState(() => new RevisionController(client, workspace, lifetime.signal));
	const [screen, setScreen] = useState<Screen>('sources');
	const [evaluations, setEvaluations] = useState<EvaluationListEntry[]>([]);
	const [drafts, setDrafts] = useState<RevisionListing>({revisions: [], errors: [], truncated: false});
	const [index, setIndex] = useState(0);
	const [source, setSource] = useState<RevisionSource>();
	const [state, setState] = useState<RevisionState>();
	const [change, setChange] = useState<PromptChange>();
	const [hypothesis, setHypothesis] = useState('');
	const [evaluationId, setEvaluationId] = useState('');
	const [notice, setNotice] = useState('Enter records or reads its selection; the candidate becomes the revision parent.');
	const [busy, setBusy] = useState(false);
	const working = useRef(false);
	const polling = useRef(false);
	const generation = useRef(0);
	const current = useRef(state);
	current.current = state;

	async function perform(action: () => Promise<void>): Promise<void> {
		if (working.current || lifetime.signal.aborted) return;
		working.current = true;
		generation.current += 1;
		setBusy(true);
		try { await action(); }
		catch (error) {
			if (lifetime.signal.aborted) return;
			setNotice(error instanceof Error ? safeText(error.message) : 'Revision operation failed; the recovery draft is retained.');
			const previous = current.current;
			if (previous) {
				// A request may have committed after its response was lost. Adopt only
				// the local recovery phase here; R rechecks canonical evidence.
				const cursor = await workspace.load(previous.cursor.proposal_id).catch(() => undefined);
				if (cursor) {
					const {job: _job, assessment: _assessment, selection: _selection, ...bound} = previous;
					setState({...bound, cursor}); setScreen('state');
				}
			}
		} finally {
			working.current = false;
			if (!lifetime.signal.aborted) setBusy(false);
		}
	}

	async function loadLists(): Promise<void> {
		const results = await Promise.allSettled([client.request({command: 'evaluation_list', limit: 200}, lifetime.signal), workspace.list()]);
		if (lifetime.signal.aborted) return;
		const [evidence, local] = results;
		const errors: string[] = [];
		if (evidence.status === 'fulfilled' && evidence.value.data?.type === 'evaluation_list') setEvaluations(evidence.value.data.evaluations);
		else errors.push(evidence.status === 'rejected' ? String(evidence.reason) : evidence.value.error?.message ?? 'Could not load comparisons');
		if (local.status === 'fulfilled') { setDrafts(local.value); if (local.value.errors.length) errors.push(`${local.value.errors.length} unreadable recovery drafts`); }
		else errors.push(String(local.reason));
		if (errors.length) setNotice(safeText(errors.join(' · ')));
	}

	useEffect(() => { void perform(loadLists); return () => lifetime.abort(); }, []);
	useEffect(() => {
		if (!state || !['state', 'cancel'].includes(screen) || !['running', 'compare_unknown'].includes(state.cursor.phase)) return;
		const timer = setInterval(() => {
			if (working.current || polling.current || !current.current) return;
			polling.current = true;
			const version = generation.current;
			const active = () => !lifetime.signal.aborted && generation.current === version;
			void controller.poll(current.current, active).then(latest => {
				if (active()) setState(latest);
			}).catch(error => {
				if (active()) setNotice(error instanceof Error ? safeText(error.message) : 'Comparison progress unavailable; R reconciles evidence.');
			}).finally(() => { polling.current = false; });
		}, pollMs);
		return () => clearInterval(timer);
	}, [state?.cursor.proposal_id, state?.cursor.phase, screen, controller, pollMs]);

	async function selectSource(id: string): Promise<void> {
		const value = await controller.source(id);
		if (lifetime.signal.aborted) return;
		setSource(value); setState(undefined); setChange(undefined); setHypothesis(''); setScreen('source');
		setNotice(value.selection.metrics_eligible ? 'Source metrics passed. Edit one prompt change with an explicit hypothesis.' : 'Source metrics rejected. A failed candidate can still supply revision evidence.');
	}

	async function edit(): Promise<void> {
		let value = state;
		if (!value) {
			if (!source) return;
			const cursor = await controller.create(source);
			value = {cursor, parent: source.profile};
			current.current = value;
			setState(value);
		}
		if (value.cursor.phase !== 'editing') throw new Error('This prompt snapshot is already recorded; start a new revision for another edit');
		const path = workspace.path(value.cursor.proposal_id, 'draft');
		await suspendTerminal(() => launchEditor(path));
		const reviewed = await workspace.review(value.cursor);
		if (lifetime.signal.aborted) return;
		setChange(reviewed.change); setScreen('hypothesis');
		setNotice('Draft saved. Describe why this change should improve the same task pack.');
	}

	async function recover(cursor: RevisionCursor): Promise<void> {
		const value = await controller.reconcile(cursor.proposal_id);
		if (lifetime.signal.aborted) return;
		setState(value); setScreen('state'); setHypothesis(value.cursor.hypothesis);
		setNotice('Recovery reconciled with canonical daemon evidence.');
	}

	useInput((input, key) => {
		if (working.current) return;
		if (screen === 'id' || screen === 'hypothesis') {
			if (key.escape) { setScreen(screen === 'id' ? 'sources' : 'state'); return; }
			const value = screen === 'id' ? evaluationId : hypothesis;
			const update = screen === 'id' ? setEvaluationId : setHypothesis;
			if (key.return || input.includes('\r') || input.includes('\n')) {
				const typed = input.replace(/[\u0000-\u001f\u007f-\u009f]/gu, '');
				const entered = value + typed;
				if (!entered.trim()) { setNotice('Enter a nonblank value.'); return; }
				if (Buffer.byteLength(entered) > 512) { setNotice('Use at most 512 UTF-8 bytes.'); return; }
				update(entered);
				if (screen === 'id') void perform(() => selectSource(entered.trim()));
				else { setScreen('review'); setNotice('Y records the exact reviewed snapshot. Recording does not execute a comparison.'); }
				return;
			}
			if (key.backspace || key.delete) update(Array.from(value).slice(0, -1).join(''));
			else if (!key.ctrl && !key.meta) {
				const next = value + input.replace(/[\u0000-\u001f\u007f-\u009f]/gu, '');
				if (Buffer.byteLength(next) <= 512) update(next);
			}
			return;
		}
		if (key.escape) {
			if (screen === 'cancel') { setScreen('state'); return; }
			if (screen === 'sources' || screen === 'drafts') onExit();
			else { generation.current += 1; current.current = undefined; setState(undefined); setSource(undefined); setScreen('sources'); setIndex(0); void perform(loadLists); }
			return;
		}
		if (screen === 'sources' || screen === 'drafts') {
			const length = screen === 'sources' ? evaluations.length : drafts.revisions.length;
			if (key.upArrow || input === 'k') setIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setIndex(value => Math.min(Math.max(0, length - 1), value + 1));
			if (input.toLowerCase() === 'r') { setScreen('drafts'); setIndex(0); void perform(loadLists); }
			if (input.toLowerCase() === 'n') { setScreen('sources'); setIndex(0); void perform(loadLists); }
			if (input.toLowerCase() === 'i') { setScreen('id'); setEvaluationId(''); }
			if (key.return) {
				if (screen === 'sources' && evaluations[index]) void perform(() => selectSource(evaluations[index]!.evaluation.evaluation_id));
				if (screen === 'drafts' && drafts.revisions[index]) void perform(() => recover(drafts.revisions[index]!));
			}
			return;
		}
		if (screen === 'source') { if (input.toLowerCase() === 'e' || key.return) void perform(edit); return; }
		if (screen === 'review') {
			if (input.toLowerCase() === 'e') void perform(edit);
			if (input.toLowerCase() === 'y' && state && change) void perform(async () => {
				const value = await controller.record(state.cursor, hypothesis, change.sha256);
				setState(value); setScreen('state'); setNotice('Prompt revision recorded. Y separately confirms and starts its comparison.');
			});
			return;
		}
		if (!state) return;
		if (screen === 'cancel') {
			if (input.toLowerCase() === 'y') void perform(async () => {
				const value = await controller.cancel(state);
				setState(value); setScreen('state');
				setNotice(value.job && ['succeeded', 'failed', 'interrupted'].includes(value.job.state)
					? `Daemon confirmed the comparison is already ${value.job.state}.`
					: 'Cancellation requested; waiting for the daemon to confirm a terminal state.');
			});
			return;
		}
		if (input.toLowerCase() === 'r') void perform(() => recover(state.cursor));
		if (state.cursor.phase === 'editing' && input.toLowerCase() === 'e') void perform(edit);
		if (state.cursor.phase === 'record_unknown' && input.toLowerCase() === 'y') void perform(async () => { setState(await controller.record(state.cursor)); setNotice('Revision recorded. Confirm its comparison separately with Y.'); });
		if (['recorded', 'compare_unknown', 'compare_failed', 'compare_rejected'].includes(state.cursor.phase) && input.toLowerCase() === 'y' && state.child) {
			if (!confirmationFits(state.child, columns, rows)) { setNotice('Confirmation requires an ASCII model ID, at least 80×22, and the full model visible.'); return; }
			void perform(async () => { setState(await controller.compare(state.cursor, state.child!)); setNotice('Comparison requested with its saved identity; R reconciles progress.'); });
		}
		if (['completed', 'assessment_unknown', 'assessed'].includes(state.cursor.phase) && input.toLowerCase() === 'a') {
			void perform(async () => { setState(await controller.assess(state.cursor)); setNotice('Metrics assessment verified. Promotion is a separate action.'); });
		}
		if (state.cursor.phase === 'running' && input.toLowerCase() === 'c') setScreen('cancel');
	});

	const selectedList = screen === 'drafts' ? drafts.revisions : evaluations;
	const listHeight = Math.max(1, Math.min(8, rows - 10));
	const start = Math.max(0, index - listHeight + 1);
	const profile = state?.child ?? state?.parent ?? source?.profile;
	const phase = state?.cursor.phase;
	const limitsFit = profile ? confirmationFits(profile, columns, rows) : false;
	const showLimits = !state?.job || ['recorded', 'compare_unknown', 'compare_failed', 'compare_rejected'].includes(phase ?? '');
	let footer = 'Enter choose · R recovery drafts · N comparisons · I evaluation ID · Esc back';
	if (screen === 'source') footer = 'E edit a private prompt copy · Esc comparisons';
	if (screen === 'hypothesis' || screen === 'id') footer = 'Enter continue · Esc back';
	if (screen === 'review') footer = 'Y record reviewed bytes · E edit again · Esc comparisons';
	if (screen === 'state') {
		footer = phase === 'editing' ? 'E edit draft · R verify source · Esc comparisons'
			: phase === 'record_unknown' ? 'R reconcile outcome · Y retry the same proposal · Esc comparisons'
				: phase === 'record_rejected' ? 'Draft retained · Esc comparisons'
					: phase === 'running' ? 'C cancel this comparison · R refresh · Esc comparisons'
						: ['completed', 'assessment_unknown', 'assessed'].includes(phase ?? '') ? 'A record or verify metrics assessment · R refresh · Esc comparisons'
							: 'Y confirm comparison limits · R reconcile · Esc comparisons';
	}
	if (screen === 'cancel') footer = 'Y request cancellation · Esc keep running';
	if (screen === 'state' && ['recorded', 'compare_unknown', 'compare_failed', 'compare_rejected'].includes(phase ?? '') && !limitsFit) footer = 'ASCII model ID + 80×22 + full model required · R refresh · Esc comparisons';

	return <Box width={Math.max(1, columns)} height={Math.max(1, rows)} flexDirection="column" paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>FORGE / REVISE A PROMPT</Text>
		<Text dimColor>Source evidence → edit → record → confirm comparison → assess</Text>
		<Box flexDirection="column" flexGrow={1} overflow="hidden" marginTop={1}>
			{(screen === 'sources' || screen === 'drafts') && <>
				<Text bold>{screen === 'drafts' ? 'RECOVERY DRAFTS' : 'COMPLETED COMPARISONS'} · {selectedList.length}</Text>
				{selectedList.slice(start, start + listHeight).map((entry, offset) => <Text key={start + offset} wrap="truncate" {...colorProps(theme.color(start + offset === index ? 'champion' : 'ink'))}>
					{start + offset === index ? '> ' : '  '}{'proposal_id' in entry ? `${safeText(entry.proposal_id)} / ${entry.phase}` : `${safeText(entry.evaluation.evaluation_id)} / ${entry.evaluation.parent_visible_correct} → ${entry.evaluation.candidate_visible_correct}`}
				</Text>)}
				{!selectedList.length && <Text>No {screen === 'drafts' ? 'recovery drafts' : 'completed comparisons'} found.</Text>}
				{screen === 'sources' && <Text dimColor>Latest 200 comparisons. I opens an older evaluation by its exact ID.</Text>}
				{screen === 'drafts' && drafts.errors.length > 0 && <Text>{drafts.errors.length} unreadable drafts retained; inspect their files.</Text>}
				{screen === 'drafts' && drafts.truncated && <Text dimColor>Showing the latest 200 recovery drafts.</Text>}
			</>}
			{screen === 'id' && <Text wrap="truncate">Evaluation ID: {safeText(evaluationId)}█</Text>}
			{screen === 'hypothesis' && <><Text>Hypothesis (1–512 UTF-8 bytes):</Text><Text wrap="truncate">{safeText(hypothesis)}█</Text></>}
			{screen === 'review' && change && <>
				<Text bold>REVIEW PROMPT CHANGE</Text>
				<Text>Bytes {change.before_bytes} → {change.after_bytes} · lines {change.before_lines} → {change.after_lines}</Text>
				<Text>CRLF {change.before_crlf} → {change.after_crlf} · whitespace only {change.whitespace_only ? 'yes' : 'no'}</Text>
				<Text wrap="truncate">SHA-256: {change.sha256}</Text>
				<Text wrap="truncate">Hypothesis: {safeText(hypothesis)}</Text>
				<Text dimColor>Exact UTF-8 bytes are retained. The original Genome and source file are preserved.</Text>
			</>}
			{(screen === 'source' || screen === 'state') && profile && <>
				<Text bold wrap="truncate">{screen === 'source' ? 'REVISION PARENT' : safeText(phase ?? '').toUpperCase()}: {safeText(profile.genome.name)}</Text>
				{showLimits && !state?.assessment && <RevisionProfilePanel profile={profile} />}
				{state?.cursor && <Text dimColor wrap="truncate">Draft: {safeText(workspace.path(state.cursor.proposal_id, 'draft'))}</Text>}
				{state?.cursor && <Text wrap="truncate">Proposal: {safeText(state.cursor.proposal_id)}</Text>}
				{state?.job && !showLimits && <>
					<Text wrap="truncate">Comparison: {safeText(state.job.evaluation_id)}</Text>
					<Text>{state.job.state.toUpperCase()} / {state.job.phase.replaceAll('_', ' ')} · {state.job.completed_trials}/{state.job.total_trials} trials</Text>
					{state.job.evaluation && <Text>Visible score: {state.job.evaluation.parent_visible_correct} → {state.job.evaluation.candidate_visible_correct} / {state.job.evaluation.visible_total}</Text>}
				</>}
				{state?.job && showLimits && <Text wrap="truncate">Previous attempt: {safeText(state.job.evaluation_id)} / {state.job.state}</Text>}
				{state?.selection && <>
					<Text>Effect: {state.selection.estimate_bps} bps · CI [{state.selection.lower_bps}, {state.selection.upper_bps}]</Text>
					<Text>Reported cost: {reportedDollars(String(state.selection.parent_cost_microusd))} → {reportedDollars(String(state.selection.candidate_cost_microusd))}</Text>
					<Text>Latency: {state.selection.parent_latency_millis} → {state.selection.candidate_latency_millis} ms</Text>
				</>}
				{state?.assessment && <><Text bold>{state.assessment.outcome === 'metrics_passed' ? 'METRICS PASSED' : 'METRICS REJECTED'}</Text><Text dimColor>Invariant verification and Champion promotion are separate actions.</Text></>}
				{phase === 'assessed' && !state?.assessment && <Text>A verifies the saved assessment receipt.</Text>}
				{['compare_failed', 'compare_rejected'].includes(phase ?? '') && <Text>Y starts a fresh attempt with a new ID after confirming current limits.</Text>}
			</>}
			{screen === 'cancel' && <><Text bold>Cancel this comparison?</Text><Text wrap="truncate">{safeText(state?.job?.evaluation_id ?? '')}</Text><Text>Trials stop after the daemon confirms cancellation.</Text></>}
		</Box>
		<Text wrap="truncate" {...colorProps(theme.color('muted'))}>{busy ? 'Working… recovery identities are saved before requests.' : safeText(notice)}</Text>
		<Text wrap="truncate" {...colorProps(theme.color('judge'))}>{footer}</Text>
	</Box>;
}
