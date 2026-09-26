import {spawn} from 'node:child_process';
import React, {useCallback, useEffect, useRef, useState} from 'react';
import {Box, Text, useApp, useInput, useWindowSize} from 'ink';
import {defaultAgentPath, editorCommand, ensureAgentSource} from './author.js';
import {ControlClient} from './client.js';
import {aggregateCosts, type CostEntry} from './evidence.js';
import {CostsPanel, DenialsPanel, EvidencePanel, RunsPanel} from './evidence-view.js';
import {lineageRows, roleOf} from './lineage.js';
import {GenomeDetail, LineagePanel, WorldList, shortId} from './lineage-view.js';
import {GeneDetailPanel, GeneListPanel} from './gene-view.js';
import {CanaryDetailPanel, CanaryListPanel, DriftDetailPanel, DriftListPanel} from './drift-canary-view.js';
import {MetaEvaluationDetailPanel, MetaEvaluationListPanel, MetaStrategyDetailPanel, MetaStrategyListPanel} from './meta-view.js';
import {CelebrationBurst, CrestClash, ProgressBar, TorchFlicker, Typewriter, type FrameOptions} from './motion.js';
import {BANNER_CHAR_TOKEN, borderColorProps, colorProps, HOME_BANNER_ROWS, TOKEN_ROLE, TUI_BANNER, useTheme, type Role, type Theme} from './theme.js';
import {safeText, type ApiResponse, type ArenaJobProgress, type Canary, type Champion, type Command, type DenialEntry, type Drift, type EvaluationListEntry, type GeneAggregate, type GeneSummary, type Genome, type MetaEvaluation, type MetaStrategy, type ResponseData, type RunListEntry, type World} from './protocol.js';

const MENU = ['Status', 'Freeze', 'Unfreeze', 'Kill all active work', 'Inspect job by ID', 'Cancel job by ID', 'Arena progress by ID', 'Lineage and Champions', 'Evidence & Costs', 'Gene Bank', 'Drift, Canary & Meta-eval', 'Author Markdown agent'] as const;
const EVIDENCE_MENU = ['Runs', 'Evidence receipts', 'Costs', 'Denials'] as const;
const OPERATE_MENU = ['Drift records', 'Canaries', 'Evolver strategies', 'Meta-evaluations'] as const;
type View = 'home' | 'job-id' | 'arena-id' | 'arena-progress' | 'confirm-kill' | 'confirm-kill-all'
	| 'worlds' | 'lineage' | 'genome' | 'rollback-reason' | 'confirm-rollback'
	| 'evidence-menu' | 'runs' | 'evidence' | 'costs' | 'denials'
	| 'genes' | 'gene-detail'
	| 'operate-menu' | 'drifts' | 'drift-detail' | 'canaries' | 'canary-detail'
	| 'meta-strategies' | 'meta-strategy-detail' | 'meta-evaluations' | 'meta-evaluation-detail'
	| 'author-world' | 'author-path' | 'author-register' | 'author-test-parent';
const LINEAGE_VIEWS: View[] = ['worlds', 'lineage', 'genome', 'rollback-reason', 'confirm-rollback'];
const EVIDENCE_LIST_VIEWS: View[] = ['runs', 'evidence', 'costs', 'denials'];
const EVIDENCE_VIEWS: View[] = ['evidence-menu', ...EVIDENCE_LIST_VIEWS];
const GENE_VIEWS: View[] = ['genes', 'gene-detail'];
const OPERATE_LIST_VIEWS: View[] = ['drifts', 'canaries', 'meta-strategies', 'meta-evaluations'];
const OPERATE_VIEWS: View[] = ['operate-menu', ...OPERATE_LIST_VIEWS, 'drift-detail', 'canary-detail', 'meta-strategy-detail', 'meta-evaluation-detail'];
const AUTHOR_VIEWS: View[] = ['author-world', 'author-path', 'author-register', 'author-test-parent'];
const LIST_LIMIT = 200;
type TuiClient = Pick<ControlClient, 'request'>;
type Props = {client?: TuiClient; pollMs?: number};

function waitForPoll(ms: number, signal: AbortSignal): Promise<void> {
	if (signal.aborted) return Promise.reject(new Error('daemon request aborted'));
	return new Promise((resolve, reject) => {
		const finish = (error?: Error) => {
			clearTimeout(timer);
			signal.removeEventListener('abort', abort);
			if (error) reject(error);
			else resolve();
		};
		const abort = () => finish(new Error('daemon request aborted'));
		const timer = setTimeout(() => finish(), ms);
		signal.addEventListener('abort', abort, {once: true});
	});
}

/**
 * The home screen's torch-lit colosseum banner: four rows picked out of the
 * hero's own `TUI_BANNER` pixel grid (see `theme.ts`), rendered as colored
 * block glyphs — the same silhouette as `docs/assets/pixel/tui-banner.svg`,
 * just cropped to the four lines the 80x24 layout budgets for it. The
 * left-hand torch's flame pixel ('E') flickers through the champion's
 * ember → ember-bright → gold cycle; the right-hand torch's pixel ('d',
 * graphite) stays still, echoing the challenger's cold crest.
 */
function HomeBanner({theme, animate, frame}: {theme: Theme} & FrameOptions) {
	return <>
		{HOME_BANNER_ROWS.map((rowIndex, lineIndex) => {
			const row = TUI_BANNER[rowIndex] ?? '';
			return <Text key={lineIndex} wrap="truncate">
				{Array.from(row).map((ch, col) => {
					if (ch === 'E') return <TorchFlicker key={col} animate={animate} frame={frame} />;
					const token = BANNER_CHAR_TOKEN[ch];
					if (!token) return ' ';
					return <Text key={col} {...colorProps(theme.color(TOKEN_ROLE[token]))}>█</Text>;
				})}
			</Text>;
		})}
	</>;
}

function messageFor(response: ApiResponse): string {
	if (response.error) return `Daemon: ${safeText(response.error.code)} — ${safeText(response.error.message)}`;
	if (response.data?.type === 'status') {
		const state = response.data;
		return `Ledger event ${state.event_count} · ${state.genome_count} Genomes`;
	}
	if (response.data?.type === 'acknowledged') return `Acknowledged · evolution ${response.data.frozen ? 'frozen' : 'unfrozen'}`;
	if (response.data?.type === 'job') return `${safeText(response.data.job.job_id)} · ${safeText(response.data.job.state)} · ${response.data.progress.trace_events} trace events`;
	if (response.data?.type === 'arena_job') return `${safeText(response.data.job.evaluation_id)} · ${safeText(response.data.job.phase)} · ${response.data.job.completed_trials}/${response.data.job.total_trials} trials`;
	if (response.data?.type === 'genome') return `Registered ${safeText(response.data.genome.name)} · ${safeText(response.data.genome.genome_id)}`;
	return 'No response data';
}

function statusOf(data: ResponseData | undefined): string {
	return data?.type === 'status' ? (data.frozen ? 'FROZEN' : 'RUNNING') : '—';
}

/** The phase of an Arena job maps to which lane is currently "lit": the parent/challenger's trials, the candidate/champion's trials, or the judge sealing the result. */
function phaseRole(phase: ArenaJobProgress['phase']): Role {
	if (phase === 'parent_trials') return 'challenger';
	if (phase === 'candidate_trials') return 'champion';
	if (phase === 'scoring' || phase === 'committing') return 'judge';
	return 'muted';
}

export function ArenaProgressPanel({job, stale, notice, compact = false, animate, frame}: {job?: ArenaJobProgress | undefined; stale: boolean; notice?: string; compact?: boolean} & FrameOptions) {
	const theme = useTheme();
	if (!job) return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color(stale ? 'danger' : 'border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>ARENA PROGRESS</Text>
		<Text {...colorProps(theme.color(stale ? 'danger' : 'ink'))}>{stale ? 'STALE · daemon unavailable' : safeText(notice ?? 'Enter an evaluation ID to inspect its durable progress.')}</Text>
		{!stale && <Box marginTop={compact ? 0 : 1}><CrestClash animate={animate} frame={frame} /></Box>}
		<Text {...colorProps(theme.color('muted'))}>The control API provides progress by known evaluation ID; it has no Arena job list.</Text>
	</Box>;
	const ratio = Math.max(0, Math.min(1, job.completed_trials / job.total_trials));
	const role = phaseRole(job.phase);
	const sealing = job.phase === 'scoring' || job.phase === 'committing';
	const candidateFavored = job.evaluation !== undefined
		&& job.evaluation.candidate_visible_correct > job.evaluation.parent_visible_correct;
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color(stale ? 'danger' : 'border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>ARENA / {safeText(job.evaluation_id)}</Text>
		<Text {...colorProps(theme.color(stale ? 'danger' : role))}>{stale ? 'STALE · ' : ''}{safeText(job.state.toUpperCase())} · {safeText(job.phase.replaceAll('_', ' ').toUpperCase())}{sealing ? ` ${theme.glyphs.seal}` : ''}</Text>
		<Text>Trials {job.completed_trials}/{job.total_trials}  <ProgressBar ratio={ratio} width={16} color={theme.color(role)} highlightColor={theme.color('championBright')} trackColor={theme.color('muted')} animate={animate} frame={frame} /></Text>
		{!compact && <>
			<Text {...colorProps(theme.color('challenger'))}>Parent    {safeText(job.parent_genome_id)}</Text>
			<Text {...colorProps(theme.color('champion'))}>Candidate {safeText(job.candidate_genome_id)}</Text>
			{job.evaluation && <Text>Visible score {job.evaluation.parent_visible_correct} → <Text {...colorProps(theme.color(candidateFavored ? 'improvement' : 'ink'))}>{job.evaluation.candidate_visible_correct}</Text> / {job.evaluation.visible_total}</Text>}
			{candidateFavored && <CelebrationBurst active label="CANDIDATE AHEAD" animate={animate} frame={frame} />}
		</>}
	</Box>;
}

export function App({client: providedClient, pollMs = 1500}: Props) {
	const {exit} = useApp();
	const theme = useTheme();
	const {columns = 80, rows = 24} = useWindowSize();
	const [client] = useState(() => providedClient ?? new ControlClient());
	const [selected, setSelected] = useState(0);
	const [view, setView] = useState<View>('home');
	const [jobId, setJobId] = useState('');
	const [jobPromptAction, setJobPromptAction] = useState<'inspect' | 'cancel'>('inspect');
	const [arenaInput, setArenaInput] = useState('');
	const [arenaJobId, setArenaJobId] = useState('');
	const [arenaJob, setArenaJob] = useState<ArenaJobProgress>();
	const [arenaStale, setArenaStale] = useState(false);
	const [arenaNotice, setArenaNotice] = useState('Enter an evaluation ID to inspect its durable progress.');
	const [worlds, setWorlds] = useState<World[]>([]);
	const [worldIndex, setWorldIndex] = useState(0);
	const [genomes, setGenomes] = useState<Genome[]>([]);
	const [champion, setChampion] = useState<Champion>();
	const [lineageIndex, setLineageIndex] = useState(0);
	const [detailId, setDetailId] = useState('');
	const [prompts, setPrompts] = useState<Record<string, string | null>>({});
	const [reason, setReason] = useState('');
	const [runs, setRuns] = useState<RunListEntry[]>([]);
	const [runIndex, setRunIndex] = useState(0);
	const [evaluations, setEvaluations] = useState<EvaluationListEntry[]>([]);
	const [evaluationIndex, setEvaluationIndex] = useState(0);
	const [denials, setDenials] = useState<DenialEntry[]>([]);
	const [denialIndex, setDenialIndex] = useState(0);
	const [costIndex, setCostIndex] = useState(0);
	const [evidenceMenuIndex, setEvidenceMenuIndex] = useState(0);
	const [genes, setGenes] = useState<GeneSummary[]>([]);
	const [geneIndex, setGeneIndex] = useState(0);
	const [geneDetail, setGeneDetail] = useState<GeneAggregate>();
	const [operateMenuIndex, setOperateMenuIndex] = useState(0);
	const [drifts, setDrifts] = useState<Drift[]>([]);
	const [driftIndex, setDriftIndex] = useState(0);
	const [canaries, setCanaries] = useState<Canary[]>([]);
	const [canaryIndex, setCanaryIndex] = useState(0);
	const [metaStrategies, setMetaStrategies] = useState<MetaStrategy[]>([]);
	const [metaStrategyIndex, setMetaStrategyIndex] = useState(0);
	const [metaEvaluations, setMetaEvaluations] = useState<MetaEvaluation[]>([]);
	const [metaEvaluationIndex, setMetaEvaluationIndex] = useState(0);
	const [authorPath, setAuthorPath] = useState('');
	const [authorGenome, setAuthorGenome] = useState<Genome>();
	const [authorNotice, setAuthorNotice] = useState('');
	const [authorParentIndex, setAuthorParentIndex] = useState(0);
	const [status, setStatus] = useState<ApiResponse>();
	const [stale, setStale] = useState(false);
	const [notice, setNotice] = useState('Connecting to the local control plane…');
	const [busy, setBusy] = useState(false);
	const [lifetime] = useState(() => new AbortController());
	const closing = useRef(false);
	const refreshing = useRef(false);
	const arenaCurrentId = useRef('');
	const arenaRefreshing = useRef<string | null>(null);
	const refresh = useCallback(async (announce = true): Promise<ApiResponse | undefined> => {
		if (closing.current || refreshing.current) return undefined;
		refreshing.current = true;
		try {
			const response = await client.request({command: 'status'}, lifetime.signal);
			setStatus(response);
			setStale(Boolean(response.error));
			if (announce) setNotice(messageFor(response));
			return response;
		} catch (error) {
			if (lifetime.signal.aborted) return undefined;
			setStale(true);
			if (announce) setNotice(error instanceof Error ? safeText(error.message) : 'Local daemon unavailable');
			return undefined;
		} finally {
			refreshing.current = false;
		}
	}, [client, lifetime]);
	useEffect(() => {
		void refresh();
		const timer = setInterval(() => void refresh(false), pollMs);
		return () => clearInterval(timer);
	}, [refresh, pollMs]);
	useEffect(() => () => lifetime.abort(), [lifetime]);

	const act = async (command: Command, pending: string) => {
		if (closing.current || busy) return;
		setBusy(true);
		setNotice(pending);
		try {
			const response = await client.request(command, lifetime.signal);
			setNotice(messageFor(response));
			if (response.error) return;
			if (command.command === 'job_kill') {
				setNotice(`Cancellation requested for ${safeText(command.job_id)}; waiting for daemon confirmation…`);
				const deadline = Date.now() + 20_000;
				let confirmed = false;
				while (Date.now() < deadline) {
					await waitForPoll(350, lifetime.signal);
					const current = await client.request({command: 'job_status', job_id: command.job_id}, lifetime.signal);
					if (current.data?.type === 'job' && ['succeeded', 'failed', 'interrupted'].includes(current.data.job.state)) {
						setNotice(`Daemon confirmed ${safeText(command.job_id)} terminal: ${safeText(current.data.job.state)}${current.data.job.terminal ? ` / ${safeText(current.data.job.terminal)}` : ''}`);
						confirmed = true;
						break;
					}
				}
				if (!confirmed) setNotice(`Cancellation was requested for ${safeText(command.job_id)} but terminal confirmation timed out.`);
			}
			const latest = await refresh(false);
			if (command.command === 'kill_all') {
				const active = latest?.data?.type === 'status' ? latest.data.active_runs : 'unknown';
				setNotice(`Kill-all recorded · ${active} active; terminal states unavailable.`);
			}
		} catch (error) {
			if (lifetime.signal.aborted) return;
			setNotice(error instanceof Error ? safeText(error.message) : 'Request failed');
		} finally {
			if (!lifetime.signal.aborted) setBusy(false);
		}
	};

	const refreshArena = useCallback(async (evaluationId: string) => {
		if (closing.current || !evaluationId || arenaRefreshing.current === evaluationId) return;
		arenaRefreshing.current = evaluationId;
		try {
			const response = await client.request({command: 'job_status', job_id: evaluationId}, lifetime.signal);
			if (arenaCurrentId.current !== evaluationId) return;
			if (response.error) {
				setArenaJob(undefined);
				setArenaStale(false);
				setArenaNotice(`${safeText(response.error.code)} · ${safeText(response.error.message)}`);
				return;
			}
			if (response.data?.type !== 'arena_job') {
				setArenaJob(undefined);
				setArenaStale(false);
				setArenaNotice('No Arena progress projection exists for that ID.');
				return;
			}
			setArenaJob(response.data.job);
			setArenaStale(false);
			setArenaNotice('Live progress from the daemon.');
		} catch (error) {
			if (!lifetime.signal.aborted && arenaCurrentId.current === evaluationId) {
				setArenaStale(true);
				setArenaNotice(error instanceof Error ? safeText(error.message) : 'Arena progress unavailable');
			}
		} finally {
			if (arenaRefreshing.current === evaluationId) arenaRefreshing.current = null;
		}
	}, [client, lifetime]);
	useEffect(() => {
		if (view !== 'arena-progress' || !arenaJobId || !arenaJob || ['succeeded', 'failed', 'interrupted'].includes(arenaJob.state)) return;
		const timer = setInterval(() => void refreshArena(arenaJobId), pollMs);
		return () => clearInterval(timer);
	}, [arenaJobId, arenaJob, pollMs, refreshArena, view]);

	const world = worlds[worldIndex];
	const lineageRowsView = world ? lineageRows(genomes, world.world_id, champion) : [];
	const loadWorlds = useCallback(async () => {
		try {
			const response = await client.request({command: 'world_list'}, lifetime.signal);
			if (response.data?.type === 'worlds') {
				setWorlds(response.data.worlds);
				setWorldIndex(index => Math.min(index, Math.max(0, response.data?.type === 'worlds' ? response.data.worlds.length - 1 : 0)));
				setNotice(`${response.data.worlds.length} registered Worlds`);
			} else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Worlds unavailable');
		}
	}, [client, lifetime]);
	const loadLineage = useCallback(async (worldId: string) => {
		try {
			const [listed, shown] = [
				await client.request({command: 'genome_list'}, lifetime.signal),
				await client.request({command: 'champion_show', world_id: worldId}, lifetime.signal),
			];
			if (listed.data?.type === 'genomes') setGenomes(listed.data.genomes);
			else setNotice(messageFor(listed));
			if (shown.data?.type === 'champion') setChampion(shown.data.champion);
			else { setChampion(undefined); setNotice(messageFor(shown)); }
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Lineage unavailable');
		}
	}, [client, lifetime]);
	const loadPrompt = useCallback(async (genomeId: string) => {
		try {
			const response = await client.request({command: 'genome_prompt', genome_id: genomeId}, lifetime.signal);
			const prompt = response.data?.type === 'genome_prompt' ? response.data.prompt : null;
			setPrompts(current => ({...current, [genomeId]: prompt}));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Prompt unavailable');
		}
	}, [client, lifetime]);
	const openGenome = (genomeId: string) => {
		const genome = genomes.find(candidate => candidate.genome_id === genomeId);
		if (!genome) return;
		setDetailId(genomeId);
		setView('genome');
		const parent = genome.parent_ids.find(id => genomes.some(candidate => candidate.genome_id === id));
		for (const id of [genomeId, ...(parent ? [parent] : [])]) if (!(id in prompts)) void loadPrompt(id);
	};
	const loadRuns = useCallback(async () => {
		try {
			const response = await client.request({command: 'run_list', limit: LIST_LIMIT}, lifetime.signal);
			if (response.data?.type === 'run_list') { setRuns(response.data.runs); setNotice(`${response.data.runs.length} runs`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Runs unavailable');
		}
	}, [client, lifetime]);
	const loadEvaluations = useCallback(async () => {
		try {
			const response = await client.request({command: 'evaluation_list', limit: LIST_LIMIT}, lifetime.signal);
			if (response.data?.type === 'evaluation_list') { setEvaluations(response.data.evaluations); setNotice(`${response.data.evaluations.length} evaluations`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Evidence unavailable');
		}
	}, [client, lifetime]);
	const loadDenials = useCallback(async () => {
		try {
			const response = await client.request({command: 'denial_list', limit: LIST_LIMIT}, lifetime.signal);
			if (response.data?.type === 'denial_list') { setDenials(response.data.denials); setNotice(`${response.data.denials.length} denials`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Denials unavailable');
		}
	}, [client, lifetime]);
	const loadCosts = useCallback(async () => {
		try {
			const [runResponse, evaluationResponse] = [
				await client.request({command: 'run_list', limit: LIST_LIMIT}, lifetime.signal),
				await client.request({command: 'evaluation_list', limit: LIST_LIMIT}, lifetime.signal),
			];
			const nextRuns = runResponse.data?.type === 'run_list' ? runResponse.data.runs : [];
			const nextEvaluations = evaluationResponse.data?.type === 'evaluation_list' ? evaluationResponse.data.evaluations : [];
			setRuns(nextRuns);
			setEvaluations(nextEvaluations);
			setNotice(`${aggregateCosts(nextRuns, nextEvaluations).length} costed Genomes`);
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Costs unavailable');
		}
	}, [client, lifetime]);
	const costs: CostEntry[] = aggregateCosts(runs, evaluations);
	const loadGenes = useCallback(async () => {
		try {
			const response = await client.request({command: 'gene_list'}, lifetime.signal);
			if (response.data?.type === 'genes') { setGenes(response.data.genes); setNotice(`${response.data.genes.length} Genes in the Gene Bank`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Gene Bank unavailable');
		}
	}, [client, lifetime]);
	const loadGeneDetail = useCallback(async (geneId: string) => {
		setGeneDetail(undefined);
		try {
			const response = await client.request({command: 'gene_show', gene_id: geneId}, lifetime.signal);
			if (response.data?.type === 'gene_aggregate') setGeneDetail(response.data.aggregate);
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Gene detail unavailable');
		}
	}, [client, lifetime]);
	const loadDrifts = useCallback(async () => {
		try {
			const response = await client.request({command: 'drift_list', limit: LIST_LIMIT}, lifetime.signal);
			if (response.data?.type === 'drift_list') { setDrifts(response.data.drifts); setNotice(`${response.data.drifts.length} drift records`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Drift records unavailable');
		}
	}, [client, lifetime]);
	const loadCanaries = useCallback(async () => {
		try {
			const response = await client.request({command: 'canary_list', limit: LIST_LIMIT}, lifetime.signal);
			if (response.data?.type === 'canary_list') { setCanaries(response.data.canaries); setNotice(`${response.data.canaries.length} canaries`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Canaries unavailable');
		}
	}, [client, lifetime]);
	const loadMetaStrategies = useCallback(async () => {
		try {
			const response = await client.request({command: 'meta_strategy_list'}, lifetime.signal);
			if (response.data?.type === 'meta_strategies') { setMetaStrategies(response.data.strategies); setNotice(`${response.data.strategies.length} Evolver strategies`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Strategies unavailable');
		}
	}, [client, lifetime]);
	const loadMetaEvaluations = useCallback(async () => {
		try {
			const response = await client.request({command: 'meta_list', limit: LIST_LIMIT}, lifetime.signal);
			if (response.data?.type === 'meta_evaluation_list') { setMetaEvaluations(response.data.receipts); setNotice(`${response.data.receipts.length} meta-evaluations`); }
			else setNotice(messageFor(response));
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Meta-evaluations unavailable');
		}
	}, [client, lifetime]);

	/** Hands the real terminal to `$EDITOR`/`$VISUAL` and restores Ink's raw-mode input afterward. */
	const openEditor = async (path: string): Promise<void> => {
		const wasRaw = Boolean(process.stdin.isTTY && process.stdin.isRaw);
		if (process.stdin.isTTY) process.stdin.setRawMode(false);
		process.stdin.pause();
		await new Promise<void>(resolve => {
			const child = spawn(editorCommand(), [path], {stdio: 'inherit'});
			child.on('exit', () => resolve());
			child.on('error', () => resolve());
		});
		process.stdin.resume();
		if (process.stdin.isTTY && wasRaw) process.stdin.setRawMode(true);
	};
	const registerAgent = async (path: string, worldId: string) => {
		if (busy) return;
		setBusy(true);
		setAuthorNotice('Registering with the daemon…');
		try {
			const response = await client.request({command: 'genome_register', path, world_id: worldId}, lifetime.signal);
			if (response.data?.type === 'genome') {
				setAuthorGenome(response.data.genome);
				setAuthorNotice(`Registered ${safeText(response.data.genome.name)} · ${safeText(response.data.genome.genome_id)}. Press T to test, Esc to finish.`);
			} else {
				setAuthorGenome(undefined);
				setAuthorNotice(response.error ? `Rejected: ${safeText(response.error.message)}` : messageFor(response));
			}
		} catch (error) {
			if (!lifetime.signal.aborted) setAuthorNotice(error instanceof Error ? safeText(error.message) : 'Registration failed');
		} finally {
			if (!lifetime.signal.aborted) setBusy(false);
		}
	};
	const testAgent = async (parentGenomeId: string) => {
		if (!authorGenome || busy) return;
		setBusy(true);
		const evaluationId = `tui-test-${Date.now()}`;
		setNotice(`Starting paired Arena evaluation of ${shortId(parentGenomeId)} vs ${shortId(authorGenome.genome_id)}…`);
		try {
			const response = await client.request({command: 'evaluate_pair', evaluation_id: evaluationId, parent_genome_id: parentGenomeId, candidate_genome_id: authorGenome.genome_id}, lifetime.signal);
			if (response.error) { setNotice(messageFor(response)); return; }
			arenaCurrentId.current = evaluationId;
			setArenaJobId(evaluationId);
			setArenaJob(undefined);
			setArenaStale(false);
			setArenaNotice('Evaluating candidate against parent…');
			setView('arena-progress');
			void refreshArena(evaluationId);
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Test evaluation failed');
		} finally {
			if (!lifetime.signal.aborted) setBusy(false);
		}
	};

	const rollback = async () => {
		if (!world || busy) return;
		setBusy(true);
		setNotice('Requesting Champion rollback…');
		try {
			const response = await client.request({
				command: 'champion_rollback', transition_id: `tui-rollback-${Date.now()}`, world_id: world.world_id, reason: reason.trim(),
			}, lifetime.signal);
			if (response.data?.type === 'champion_transition') {
				const transition = response.data.transition;
				setNotice(`Rolled back · Champion ${shortId(transition.champion_genome_id)} restored · ${shortId(transition.previous_champion_genome_id ?? '')} quarantined`);
			} else setNotice(messageFor(response));
			await loadLineage(world.world_id);
		} catch (error) {
			if (!lifetime.signal.aborted) setNotice(error instanceof Error ? safeText(error.message) : 'Rollback failed');
		} finally {
			if (!lifetime.signal.aborted) setBusy(false);
		}
	};

	const quit = () => {
		if (closing.current) return;
		closing.current = true;
		lifetime.abort();
		exit();
	};

	useInput((input, key) => {
		if (view === 'home' && input.toLowerCase().includes('q')) { quit(); return; }
		if (view === 'job-id') {
			if (key.escape) { setView('home'); return; }
			if (key.return || input.includes('\r') || input.includes('\n')) {
				const typed = input.replace(/[\r\n]/g, '').split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				const cleaned = (jobId + typed).trim().slice(0, 128);
				if (cleaned) {
					setJobId(cleaned);
					if (jobPromptAction === 'cancel') setView('confirm-kill');
					else { void act({command: 'job_status', job_id: cleaned}, 'Looking up job…'); setView('home'); }
				} else setView('home');
				return;
			}
			if (key.backspace || key.delete) setJobId(value => value.slice(0, -1));
			else if (!key.ctrl && !key.meta) {
				const typed = input.split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				if (typed) setJobId(value => (value + typed).slice(0, 128));
			}
			return;
		}
		if (view === 'arena-id') {
			if (key.escape) { setView('home'); return; }
			if (key.return || input.includes('\r') || input.includes('\n')) {
				const typed = input.replace(/[\r\n]/g, '').split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				const cleaned = (arenaInput + typed).trim().slice(0, 128);
				if (cleaned) {
					arenaCurrentId.current = cleaned;
					setArenaJobId(cleaned);
					setArenaJob(undefined);
					setArenaStale(false);
					setArenaNotice('Loading Arena progress…');
					setView('arena-progress');
					void refreshArena(cleaned);
				} else setView('home');
				return;
			}
			if (key.backspace || key.delete) setArenaInput(value => value.slice(0, -1));
			else if (!key.ctrl && !key.meta) {
				const typed = input.split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				if (typed) setArenaInput(value => (value + typed).slice(0, 128));
			}
			return;
		}
		if (view === 'arena-progress') {
			if (input.toLowerCase().includes('q')) { quit(); return; }
			if (key.escape) { arenaCurrentId.current = ''; setView('home'); return; }
			if (input.toLowerCase() === 'r') { void refreshArena(arenaJobId); return; }
			return;
		}
		if (view === 'worlds') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('home'); return; }
			if (key.upArrow || input === 'k') setWorldIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setWorldIndex(value => Math.min(Math.max(0, worlds.length - 1), value + 1));
			if (key.return && world) {
				setLineageIndex(0);
				setGenomes([]);
				setChampion(undefined);
				setView('lineage');
				void loadLineage(world.world_id);
			}
			return;
		}
		if (view === 'evidence-menu') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('home'); return; }
			if (key.upArrow || input === 'k') setEvidenceMenuIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setEvidenceMenuIndex(value => Math.min(EVIDENCE_MENU.length - 1, value + 1));
			if (key.return) {
				switch (evidenceMenuIndex) {
					case 0: setRunIndex(0); setView('runs'); void loadRuns(); break;
					case 1: setEvaluationIndex(0); setView('evidence'); void loadEvaluations(); break;
					case 2: setCostIndex(0); setView('costs'); void loadCosts(); break;
					case 3: setDenialIndex(0); setView('denials'); void loadDenials(); break;
				}
			}
			return;
		}
		if (view === 'runs' || view === 'evidence' || view === 'costs' || view === 'denials') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('evidence-menu'); return; }
			const length = view === 'runs' ? runs.length : view === 'evidence' ? evaluations.length : view === 'costs' ? costs.length : denials.length;
			const setIndex = view === 'runs' ? setRunIndex : view === 'evidence' ? setEvaluationIndex : view === 'costs' ? setCostIndex : setDenialIndex;
			if (key.upArrow || input === 'k') setIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setIndex(value => Math.min(Math.max(0, length - 1), value + 1));
			if (input.toLowerCase() === 'r') {
				if (view === 'runs') void loadRuns();
				else if (view === 'evidence') void loadEvaluations();
				else if (view === 'costs') void loadCosts();
				else void loadDenials();
			}
			return;
		}
		if (view === 'genes') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('home'); return; }
			if (key.upArrow || input === 'k') setGeneIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setGeneIndex(value => Math.min(Math.max(0, genes.length - 1), value + 1));
			if (input.toLowerCase() === 'r') void loadGenes();
			if (key.return && genes[geneIndex]) { setView('gene-detail'); void loadGeneDetail(genes[geneIndex]!.gene.gene_id); }
			return;
		}
		if (view === 'gene-detail') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('genes'); return; }
			if (input.toLowerCase() === 'r' && genes[geneIndex]) void loadGeneDetail(genes[geneIndex]!.gene.gene_id);
			return;
		}
		if (view === 'operate-menu') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('home'); return; }
			if (key.upArrow || input === 'k') setOperateMenuIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setOperateMenuIndex(value => Math.min(OPERATE_MENU.length - 1, value + 1));
			if (key.return) {
				switch (operateMenuIndex) {
					case 0: setDriftIndex(0); setView('drifts'); void loadDrifts(); break;
					case 1: setCanaryIndex(0); setView('canaries'); void loadCanaries(); break;
					case 2: setMetaStrategyIndex(0); setView('meta-strategies'); void loadMetaStrategies(); break;
					case 3: setMetaEvaluationIndex(0); setView('meta-evaluations'); void loadMetaEvaluations(); break;
				}
			}
			return;
		}
		if (view === 'drifts') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('operate-menu'); return; }
			if (key.upArrow || input === 'k') setDriftIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setDriftIndex(value => Math.min(Math.max(0, drifts.length - 1), value + 1));
			if (input.toLowerCase() === 'r') void loadDrifts();
			if (key.return && drifts[driftIndex]) setView('drift-detail');
			return;
		}
		if (view === 'drift-detail') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('drifts'); return; }
			if (input.toLowerCase() === 'r') void loadDrifts();
			return;
		}
		if (view === 'canaries') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('operate-menu'); return; }
			if (key.upArrow || input === 'k') setCanaryIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setCanaryIndex(value => Math.min(Math.max(0, canaries.length - 1), value + 1));
			if (input.toLowerCase() === 'r') void loadCanaries();
			if (key.return && canaries[canaryIndex]) setView('canary-detail');
			return;
		}
		if (view === 'canary-detail') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('canaries'); return; }
			if (input.toLowerCase() === 'r') void loadCanaries();
			return;
		}
		if (view === 'meta-strategies') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('operate-menu'); return; }
			if (key.upArrow || input === 'k') setMetaStrategyIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setMetaStrategyIndex(value => Math.min(Math.max(0, metaStrategies.length - 1), value + 1));
			if (input.toLowerCase() === 'r') void loadMetaStrategies();
			if (key.return && metaStrategies[metaStrategyIndex]) setView('meta-strategy-detail');
			return;
		}
		if (view === 'meta-strategy-detail') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('meta-strategies'); return; }
			if (input.toLowerCase() === 'r') void loadMetaStrategies();
			return;
		}
		if (view === 'meta-evaluations') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('operate-menu'); return; }
			if (key.upArrow || input === 'k') setMetaEvaluationIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setMetaEvaluationIndex(value => Math.min(Math.max(0, metaEvaluations.length - 1), value + 1));
			if (input.toLowerCase() === 'r') void loadMetaEvaluations();
			if (key.return && metaEvaluations[metaEvaluationIndex]) setView('meta-evaluation-detail');
			return;
		}
		if (view === 'meta-evaluation-detail') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('meta-evaluations'); return; }
			if (input.toLowerCase() === 'r') void loadMetaEvaluations();
			return;
		}
		if (view === 'author-world') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('home'); return; }
			if (key.upArrow || input === 'k') setWorldIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setWorldIndex(value => Math.min(Math.max(0, worlds.length - 1), value + 1));
			if (key.return && world) {
				setAuthorPath(defaultAgentPath(world.name));
				setAuthorGenome(undefined);
				setAuthorNotice('');
				setView('author-path');
			}
			return;
		}
		if (view === 'author-path') {
			if (key.escape) { setView('author-world'); return; }
			if (key.return || input.includes('\r') || input.includes('\n')) {
				const typed = input.replace(/[\r\n]/g, '').replace(/[\u0000-\u001f\u007f]/g, '');
				const path = (authorPath + typed).trim();
				if (path && world) {
					setAuthorPath(path);
					(async () => {
						try {
							const resolved = ensureAgentSource(path, world.name);
							setAuthorPath(resolved);
							await openEditor(resolved);
							setAuthorNotice(`Editor closed for ${safeText(resolved)}. Press Enter to register with the daemon.`);
							setView('author-register');
						} catch (error) {
							setAuthorNotice(error instanceof Error ? safeText(error.message) : 'Could not open the Markdown agent source');
						}
					})();
				}
				return;
			}
			if (key.backspace || key.delete) setAuthorPath(value => value.slice(0, -1));
			else if (!key.ctrl && !key.meta) {
				const typed = input.replace(/[\u0000-\u001f\u007f]/g, '');
				if (typed) setAuthorPath(value => (value + typed).slice(0, 4096));
			}
			return;
		}
		if (view === 'author-register') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('home'); return; }
			if (key.return && world && !authorGenome) { void registerAgent(authorPath, world.world_id); return; }
			if (input.toLowerCase() === 't' && authorGenome && world) {
				setAuthorParentIndex(0);
				setView('author-test-parent');
				void loadLineage(world.world_id);
				return;
			}
			return;
		}
		if (view === 'author-test-parent') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('author-register'); return; }
			const candidates = lineageRowsView.filter(row => row.genome_id !== authorGenome?.genome_id);
			if (key.upArrow || input === 'k') setAuthorParentIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setAuthorParentIndex(value => Math.min(Math.max(0, candidates.length - 1), value + 1));
			if (key.return && candidates[authorParentIndex]) void testAgent(candidates[authorParentIndex]!.genome_id);
			return;
		}
		if (view === 'lineage') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) { setView('worlds'); return; }
			if (key.upArrow || input === 'k') setLineageIndex(value => Math.max(0, value - 1));
			if (key.downArrow || input === 'j') setLineageIndex(value => Math.min(Math.max(0, lineageRowsView.length - 1), value + 1));
			if (input.toLowerCase() === 'r' && world) { void loadLineage(world.world_id); return; }
			if (input.toLowerCase() === 'b') {
				if (champion && champion.standby_genome_ids.length > 0) { setReason(''); setView('rollback-reason'); }
				else setNotice('No previous Champion to restore in this World.');
				return;
			}
			if (key.return && lineageRowsView[lineageIndex]) openGenome(lineageRowsView[lineageIndex].genome_id);
			return;
		}
		if (view === 'genome') {
			if (input.toLowerCase() === 'q') { quit(); return; }
			if (key.escape) setView('lineage');
			return;
		}
		if (view === 'rollback-reason') {
			if (key.escape) { setView('lineage'); setNotice('Rollback abandoned.'); return; }
			if (key.return || input.includes('\r') || input.includes('\n')) {
				const typed = input.replace(/[\r\n]/g, '').replace(/[\u0000-\u001f\u007f]/g, '');
				const complete = (reason + typed).slice(0, 200);
				setReason(complete);
				if (complete.trim()) setView('confirm-rollback');
				return;
			}
			if (key.backspace || key.delete) setReason(value => value.slice(0, -1));
			else if (!key.ctrl && !key.meta) {
				const typed = input.replace(/[\u0000-\u001f\u007f]/g, '');
				if (typed) setReason(value => (value + typed).slice(0, 200));
			}
			return;
		}
		if (view === 'confirm-rollback') {
			if (input.toLowerCase() === 'y') { setView('lineage'); void rollback(); }
			else if (input.toLowerCase() === 'n' || key.escape) { setView('lineage'); setNotice('Rollback abandoned.'); }
			return;
		}
		if (view === 'confirm-kill') {
			if (input.toLowerCase() === 'y') { setView('home'); void act({command: 'job_kill', job_id: jobId}, 'Requesting cancellation…'); }
			else if (input.toLowerCase() === 'n' || key.escape) { setView('home'); setNotice('Cancellation abandoned.'); }
			return;
		}
		if (view === 'confirm-kill-all') {
			if (input.toLowerCase() === 'y') { setView('home'); void act({command: 'kill_all'}, 'Requesting cancellation of all active work…'); }
			else if (input.toLowerCase() === 'n' || key.escape) { setView('home'); setNotice('Kill all abandoned.'); }
			return;
		}
		if (key.upArrow || input === 'k') setSelected(value => Math.max(0, value - 1));
		if (key.downArrow || input === 'j') setSelected(value => Math.min(MENU.length - 1, value + 1));
		if (key.return) {
			switch (selected) {
				case 0: void refresh(); break;
				case 1: void act({command: 'freeze'}, 'Applying freeze…'); break;
				case 2: void act({command: 'unfreeze'}, 'Resuming evolution…'); break;
				case 3: setView('confirm-kill-all'); break;
				case 4: setJobPromptAction('inspect'); setView('job-id'); setJobId(''); break;
				case 5: setJobPromptAction('cancel'); setView('job-id'); setJobId(''); break;
				case 7: setView('worlds'); void loadWorlds(); break;
				case 6: arenaCurrentId.current = ''; setArenaInput(''); setArenaJobId(''); setArenaJob(undefined); setArenaStale(false); setArenaNotice('Enter an evaluation ID to inspect its durable progress.'); setView('arena-id'); break;
				case 8: setEvidenceMenuIndex(0); setView('evidence-menu'); break;
				case 9: setGeneIndex(0); setGeneDetail(undefined); setView('genes'); void loadGenes(); break;
				case 10: setOperateMenuIndex(0); setView('operate-menu'); break;
				case 11: setAuthorGenome(undefined); setAuthorPath(''); setAuthorNotice(''); setView('author-world'); void loadWorlds(); break;
			}
		}
	});

	const compact = columns < 72 || rows < 20;
	const lineageMode = LINEAGE_VIEWS.includes(view);
	const evidenceMode = EVIDENCE_VIEWS.includes(view);
	const geneMode = GENE_VIEWS.includes(view);
	const operateMode = OPERATE_VIEWS.includes(view);
	const authorMode = AUTHOR_VIEWS.includes(view);
	const authorCandidates = lineageRowsView.filter(row => row.genome_id !== authorGenome?.genome_id);
	// Reserved for everything outside the lineage panel (header, banner, hint
	// bar, notice bar, help line); see the layout budget below.
	const panelHeight = Math.max(3, rows - (compact ? 10 : 16));
	// Each panel draws its own border plus a few fixed header lines on top of
	// the `height` rows it's given, so the row budget passed to it must be
	// shrunk by that fixed overhead or the panel's total height overruns
	// `panelHeight` and the layout overflows the terminal.
	const worldListHeight = Math.max(1, panelHeight - 3); // border(2) + "WORLDS"(1)
	const lineagePanelHeight = Math.max(1, panelHeight - 4); // border(2) + title + Champion summary
	const genomeDetailHeight = Math.max(1, panelHeight - 8); // border(2) + 5 fixed fields + optional status line
	const detail = genomes.find(genome => genome.genome_id === detailId);
	const detailParent = detail ? genomes.find(genome => detail.parent_ids.includes(genome.genome_id)) : undefined;
	const statusText = statusOf(status?.data);
	const statusRole: Role = stale ? 'danger' : statusText === 'FROZEN' ? 'judge' : status ? 'success' : 'muted';
	return <Box flexDirection="column" width={Math.max(1, columns)} height={Math.max(1, rows)} paddingX={1}>
		<Box justifyContent="space-between">
			<Text bold><Typewriter text="HEPHAESTUS" color={theme.color('judge')} bold /> <Text {...colorProps(theme.color('challengerDim'))}>/ OPERATOR</Text></Text>
			<Text {...colorProps(theme.color(statusRole))}>{stale ? '● STALE' : `● ${statusText}`}</Text>
		</Box>
		<Box marginTop={1} flexDirection="column">
			{!compact && <HomeBanner theme={theme} />}
			<Text bold {...colorProps(theme.color('ink'))}>  LOCAL CONTROL · SCHEMA 1 · OWNER SOCKET</Text>
		</Box>
		{lineageMode && <Box marginTop={compact ? 0 : 1} flexDirection="column">
			{view === 'worlds' && <WorldList worlds={worlds} selected={worldIndex} height={worldListHeight} />}
			{view !== 'worlds' && view !== 'genome' && world && <LineagePanel world={world} rows={lineageRowsView} champion={champion} selected={lineageIndex} height={lineagePanelHeight} />}
			{view === 'genome' && detail && <GenomeDetail genome={detail} parent={detailParent} role={roleOf(detail.genome_id, champion)}
				prompt={prompts[detail.genome_id]} parentPrompt={detailParent ? prompts[detailParent.genome_id] : ''} height={genomeDetailHeight} />}
		</Box>}
		{evidenceMode && <Box marginTop={compact ? 0 : 1} flexDirection="column">
			{view === 'evidence-menu' && <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
				<Text bold {...colorProps(theme.color('judge'))}>EVIDENCE &amp; COSTS</Text>
				{EVIDENCE_MENU.map((label, index) => <Text key={label} {...colorProps(theme.color(evidenceMenuIndex === index ? 'champion' : 'inkDim'), evidenceMenuIndex === index)}>{evidenceMenuIndex === index ? `${theme.glyphs.caret} ` : '  '}{label}</Text>)}
			</Box>}
			{view === 'runs' && <RunsPanel runs={runs} selected={runIndex} height={panelHeight - 2} />}
			{view === 'evidence' && <EvidencePanel evaluations={evaluations} selected={evaluationIndex} height={panelHeight - 2} />}
			{view === 'costs' && <CostsPanel costs={costs} selected={costIndex} height={panelHeight - 2} />}
			{view === 'denials' && <DenialsPanel denials={denials} selected={denialIndex} height={panelHeight - 2} />}
		</Box>}
		{geneMode && <Box marginTop={compact ? 0 : 1} flexDirection="column">
			{view === 'genes' && <GeneListPanel genes={genes} selected={geneIndex} height={panelHeight - 2} />}
			{view === 'gene-detail' && <GeneDetailPanel aggregate={geneDetail} height={panelHeight - 6} />}
		</Box>}
		{operateMode && <Box marginTop={compact ? 0 : 1} flexDirection="column">
			{view === 'operate-menu' && <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
				<Text bold {...colorProps(theme.color('judge'))}>DRIFT, CANARY &amp; META-EVAL</Text>
				{OPERATE_MENU.map((label, index) => <Text key={label} {...colorProps(theme.color(operateMenuIndex === index ? 'champion' : 'inkDim'), operateMenuIndex === index)}>{operateMenuIndex === index ? `${theme.glyphs.caret} ` : '  '}{label}</Text>)}
			</Box>}
			{view === 'drifts' && <DriftListPanel drifts={drifts} selected={driftIndex} height={panelHeight - 2} />}
			{view === 'drift-detail' && <DriftDetailPanel drift={drifts[driftIndex]} height={panelHeight - 2} />}
			{view === 'canaries' && <CanaryListPanel canaries={canaries} selected={canaryIndex} height={panelHeight - 2} />}
			{view === 'canary-detail' && <CanaryDetailPanel canary={canaries[canaryIndex]} height={panelHeight - 2} />}
			{view === 'meta-strategies' && <MetaStrategyListPanel strategies={metaStrategies} selected={metaStrategyIndex} height={panelHeight - 2} />}
			{view === 'meta-strategy-detail' && <MetaStrategyDetailPanel strategy={metaStrategies[metaStrategyIndex]} height={panelHeight - 2} />}
			{view === 'meta-evaluations' && <MetaEvaluationListPanel receipts={metaEvaluations} selected={metaEvaluationIndex} height={panelHeight - 2} />}
			{view === 'meta-evaluation-detail' && <MetaEvaluationDetailPanel receipt={metaEvaluations[metaEvaluationIndex]} height={panelHeight - 2} />}
		</Box>}
		{authorMode && <Box marginTop={compact ? 0 : 1} flexDirection="column">
			{view === 'author-world' && <WorldList worlds={worlds} selected={worldIndex} height={worldListHeight} />}
			{view === 'author-path' && <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
				<Text bold {...colorProps(theme.color('judge'))}>AUTHOR MARKDOWN AGENT / {world ? safeText(world.name) : ''}</Text>
				<Text {...colorProps(theme.color('muted'))}>Markdown Genome source path (created with a starter template if missing):</Text>
				<Text wrap="truncate">{authorPath}</Text>
			</Box>}
			{view === 'author-register' && <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
				<Text bold {...colorProps(theme.color('judge'))}>REGISTER / {world ? safeText(world.name) : ''}</Text>
				<Text wrap="truncate">Path   {safeText(authorPath)}</Text>
				<Text wrap="truncate">Genome {authorGenome ? <Text {...colorProps(theme.color('champion'))}>{safeText(authorGenome.name)} {shortId(authorGenome.genome_id)}</Text> : 'not registered yet'}</Text>
				<Text {...colorProps(theme.color('muted'))}>{safeText(authorNotice)}</Text>
			</Box>}
			{view === 'author-test-parent' && world && <LineagePanel world={world} rows={authorCandidates} champion={champion} selected={authorParentIndex} height={lineagePanelHeight} />}
		</Box>}
		{!lineageMode && !evidenceMode && !geneMode && !operateMode && !authorMode && <Box marginTop={0}>
			<Box flexDirection="column" width={compact ? '100%' : '58%'}>
				<Text {...colorProps(theme.color('muted'))}>OPERATOR ACTIONS</Text>
				{MENU.map((label, index) => <Text key={label} {...colorProps(theme.color(selected === index ? 'champion' : 'inkDim'), selected === index)}>{selected === index ? `${theme.glyphs.caret} ` : '  '}{label}{selected === index ? '  ‹' : ''}</Text>)}
			</Box>
			{!compact && (view === 'arena-progress'
				? <Box flexDirection="column" width="42%"><ArenaProgressPanel job={arenaJob} stale={stale || arenaStale} notice={arenaNotice} /></Box>
				: <Box flexDirection="column" width="42%" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
					<Text {...colorProps(theme.color('muted'))}>CANONICAL STATUS</Text>
					<Text>Active runs  {status?.data?.type === 'status' ? status.data.active_runs : '—'}</Text>
					<Text>Genomes      {status?.data?.type === 'status' ? status.data.genome_count : '—'}</Text>
					<Text>Ledger events {status?.data?.type === 'status' ? status.data.event_count : '—'}</Text>
				</Box>)}
		</Box>}
		{compact && view === 'arena-progress' && <ArenaProgressPanel job={arenaJob} stale={stale || arenaStale} notice={arenaNotice} compact />}
		{view !== 'home' && <Box marginTop={1} borderStyle="round" {...borderColorProps(theme.color('judge'))} paddingX={1}>
			{view === 'job-id' && <Text>Job ID: {jobId}<Text {...colorProps(theme.color('muted'))}>  (Enter {jobPromptAction} · Esc cancel)</Text></Text>}
			{view === 'arena-id' && <Text>Evaluation ID: {arenaInput}<Text {...colorProps(theme.color('muted'))}>  (Enter inspect · Esc back)</Text></Text>}
			{view === 'arena-progress' && <Text {...colorProps(theme.color('muted'))}>Arena progress is read-only · Esc back · R refresh</Text>}
			{view === 'confirm-kill' && <Text {...colorProps(theme.color('judge'))}>Cancel job {safeText(jobId)}? Press Y to request, N/Esc to back out.</Text>}
			{view === 'confirm-kill-all' && <Text {...colorProps(theme.color('danger'))}>Cancel ALL active work? Press Y to request, N/Esc to back out.</Text>}
			{view === 'worlds' && <Text {...colorProps(theme.color('muted'))}>Enter open lineage · Esc back</Text>}
			{view === 'lineage' && <Text {...colorProps(theme.color('muted'))}>Enter inspect Genome · B roll back Champion · R refresh · Esc Worlds</Text>}
			{view === 'genome' && <Text {...colorProps(theme.color('muted'))}>Prompt diff against the first registered parent · Esc back</Text>}
			{view === 'rollback-reason' && <Text>Rollback reason: {reason}<Text {...colorProps(theme.color('muted'))}>  (Enter confirm · Esc cancel)</Text></Text>}
			{view === 'confirm-rollback' && <Text {...colorProps(theme.color('danger'))}>Restore the previous Champion and quarantine {shortId(champion?.champion_genome_id ?? '')}? Press Y to request, N/Esc to back out.</Text>}
			{view === 'evidence-menu' && <Text {...colorProps(theme.color('muted'))}>Enter open · Esc back</Text>}
			{EVIDENCE_LIST_VIEWS.includes(view) && <Text {...colorProps(theme.color('muted'))}>Read-only · R refresh · Esc back</Text>}
			{view === 'genes' && <Text {...colorProps(theme.color('muted'))}>Read-only · Enter inspect Gene · R refresh · Esc back</Text>}
			{view === 'gene-detail' && <Text {...colorProps(theme.color('muted'))}>Read-only · R refresh · Esc back</Text>}
			{view === 'operate-menu' && <Text {...colorProps(theme.color('muted'))}>Enter open · Esc back</Text>}
			{OPERATE_LIST_VIEWS.includes(view) && <Text {...colorProps(theme.color('muted'))}>Read-only · Enter inspect · R refresh · Esc back</Text>}
			{(view === 'drift-detail' || view === 'canary-detail' || view === 'meta-strategy-detail' || view === 'meta-evaluation-detail') && <Text {...colorProps(theme.color('muted'))}>Read-only · R refresh · Esc back</Text>}
			{view === 'author-world' && <Text {...colorProps(theme.color('muted'))}>Enter choose World · Esc back</Text>}
			{view === 'author-path' && <Text>Path: {authorPath}<Text {...colorProps(theme.color('muted'))}>  (Enter open $EDITOR · Esc back)</Text></Text>}
			{view === 'author-register' && <Text {...colorProps(theme.color('muted'))}>{authorGenome ? 'T test against a parent · Esc finish' : 'Enter register · Esc cancel'}</Text>}
			{view === 'author-test-parent' && <Text {...colorProps(theme.color('muted'))}>Enter test against selected parent/Champion · Esc back</Text>}
		</Box>}
		<Box flexGrow={1} />
		<Box borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
			<Text wrap="truncate" {...colorProps(theme.color(busy ? 'judge' : 'ink'))}>{notice}</Text>
		</Box>
		<Text {...colorProps(theme.color('muted'))}>↑↓/JK navigate · Enter select · {view === 'arena-progress' || EVIDENCE_LIST_VIEWS.includes(view) ? 'Esc back · R refresh' : lineageMode || evidenceMode || geneMode || operateMode || authorMode ? 'Esc back' : 'Y/N confirm'} · Q quit</Text>
	</Box>;
}
