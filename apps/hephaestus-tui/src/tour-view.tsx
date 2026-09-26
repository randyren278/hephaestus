import {existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join} from 'node:path';
import {fileURLToPath} from 'node:url';
import React, {useEffect, useRef, useState} from 'react';
import {Box, Text, useInput} from 'ink';
import {
	INTRO_FRAME_COUNT, INTRO_FRAME_MS, TOUR_STEPS, TOUR_STEP_COUNT,
	applyTourInput, arenaLights, celebrationBanner, controlsLine,
	quickstartFixtureAvailable, quickstartFixturePaths, renderCandidateTemplate,
	renderWorldTemplate, replaySeal, resolveDataDir, stepProgressLabel, writeTourMarker,
} from './tour.js';
import {safeText, type ApiResponse, type ArenaJobProgress, type Command, type EvaluationListEntry} from './protocol.js';
import {CelebrationBurst, CrestClash, ProgressBar, TorchFlicker} from './motion.js';
import {borderColorProps, colorProps, useTheme} from './theme.js';

export type TourClient = {request: (command: Command, signal?: AbortSignal) => Promise<ApiResponse>};

export type TourScreenProps = {
	client: TourClient;
	dataDir?: string;
	onExit: () => void;
	/** false renders one static frame with no timers; used by deterministic tests. */
	animate?: boolean;
	/** Starting step, for tests. */
	initialStep?: number;
};

export type Phase = 'idle' | 'running' | 'done' | 'error' | 'unavailable';
export type StepResult = {phase: Phase; lines: string[]; canRetry: boolean};

const idleResult: StepResult = {phase: 'idle', lines: [], canRetry: false};

/** Best-effort evaluator binary path: an explicit override, or the packaged
 * layout's `bin/hephaestus-reference-evaluator` relative to this bundle.
 * Undefined in an unbundled dev checkout, where the tour degrades honestly. */
export function resolveEvaluatorPath(): string | undefined {
	const override = process.env['HEPHAESTUS_EVALUATOR'];
	if (override && existsSync(override)) return override;
	try {
		const here = dirname(fileURLToPath(import.meta.url));
		const candidate = join(here, '../../../bin/hephaestus-reference-evaluator');
		if (existsSync(candidate)) return candidate;
	} catch {
		// Not resolvable from a source-checkout dev session; that is fine.
	}
	return undefined;
}

/** Runs a bootstrap step against the real daemon: registers (or reuses) the
 * bundled quickstart World and its parent/candidate Genomes. */
export async function registerLineage(client: TourClient, dataDir: string): Promise<StepResult> {
	const existingWorlds = await client.request({command: 'world_list'});
	if (existingWorlds.data?.type === 'worlds' && existingWorlds.data.worlds.length > 0) {
		const world = existingWorlds.data.worlds[0]!;
		const genomes = await client.request({command: 'genome_list'});
		const names = genomes.data?.type === 'genomes' ? genomes.data.genomes.map(genome => `${genome.name} ${genome.genome_id.slice(0, 12)}`) : [];
		return {
			phase: 'done',
			canRetry: false,
			lines: [
				`Reusing already-registered World ${safeText(world.name)} (${world.world_id.slice(0, 12)}…)`,
				...names.map(name => `  Genome ${name}`),
			],
		};
	}
	const paths = quickstartFixturePaths(dataDir);
	if (!quickstartFixtureAvailable(paths)) {
		return {
			phase: 'unavailable',
			canRetry: true,
			lines: [
				`No bundled quickstart fixture at ${paths.dir}.`,
				'Run `hephaestus init --fixture quickstart <dir>` first (heph does this automatically), then retry.',
			],
		};
	}
	const evaluator = resolveEvaluatorPath();
	if (!evaluator) {
		return {
			phase: 'unavailable',
			canRetry: true,
			lines: [
				'Could not locate the bundled reference-evaluator binary from this session.',
				'Registering a World needs it; run this tour through `heph` or a packaged install, or register manually with `hephaestus world register`.',
			],
		};
	}
	const visible = await client.request({command: 'manifest_put', path: paths.visibleTasks});
	const sealed = await client.request({command: 'manifest_put', path: paths.sealedTasks});
	const evaluatorArtifact = await client.request({command: 'artifact_put', path: evaluator});
	const verifier = await client.request({command: 'verifier_show'});
	for (const response of [visible, sealed, evaluatorArtifact, verifier]) {
		if (response.error) return {phase: 'error', canRetry: true, lines: [`${response.error.code}: ${response.error.message}`]};
	}
	const visibleId = visible.data?.type === 'artifact' ? visible.data.artifact_id : '';
	const sealedId = sealed.data?.type === 'artifact' ? sealed.data.artifact_id : '';
	const evaluatorId = evaluatorArtifact.data?.type === 'artifact' ? evaluatorArtifact.data.artifact_id : '';
	const verifierId = verifier.data?.type === 'verifier' ? verifier.data.artifact_id : '';
	const workDirectory = join(dataDir, 'tour', 'work');
	mkdirSync(workDirectory, {recursive: true});
	const worldSource = renderWorldTemplate(readFileSync(paths.worldTemplate, 'utf8'), {
		visible: visibleId, sealed: sealedId, evaluator: evaluatorId, verifier: verifierId,
	});
	const worldPath = join(workDirectory, 'world.json');
	writeFileSync(worldPath, worldSource);
	const worldResponse = await client.request({command: 'world_register', path: worldPath});
	if (worldResponse.error || worldResponse.data?.type !== 'world') {
		return {phase: 'error', canRetry: true, lines: [worldResponse.error ? `${worldResponse.error.code}: ${worldResponse.error.message}` : 'World registration returned an unexpected response.']};
	}
	const worldId = worldResponse.data.world.world_id;
	const parentResponse = await client.request({command: 'genome_register', path: paths.parentGenome, world_id: worldId});
	if (parentResponse.error || parentResponse.data?.type !== 'genome') {
		return {phase: 'error', canRetry: true, lines: [parentResponse.error ? `${parentResponse.error.code}: ${parentResponse.error.message}` : 'Parent Genome registration returned an unexpected response.']};
	}
	const parentId = parentResponse.data.genome.genome_id;
	const candidateSource = renderCandidateTemplate(readFileSync(paths.candidateTemplate, 'utf8'), parentId);
	const candidatePath = join(workDirectory, 'candidate.md');
	writeFileSync(candidatePath, candidateSource);
	const candidateResponse = await client.request({command: 'genome_register', path: candidatePath, world_id: worldId});
	if (candidateResponse.error || candidateResponse.data?.type !== 'genome') {
		return {phase: 'error', canRetry: true, lines: [candidateResponse.error ? `${candidateResponse.error.code}: ${candidateResponse.error.message}` : 'Candidate Genome registration returned an unexpected response.']};
	}
	const candidateId = candidateResponse.data.genome.genome_id;
	return {
		phase: 'done',
		canRetry: false,
		lines: [
			`Registered World ${worldResponse.data.world.name} (${worldId.slice(0, 12)}…)`,
			`Registered parent Genome ${parentResponse.data.genome.name} (${parentId.slice(0, 12)}…)`,
			`Registered candidate Genome ${candidateResponse.data.genome.name} (${candidateId.slice(0, 12)}…)`,
		],
	};
}

export async function unfreezeAndRun(client: TourClient, worldId: string | undefined, parentGenomeId: string | undefined): Promise<StepResult> {
	if (!worldId || !parentGenomeId) {
		return {phase: 'unavailable', canRetry: false, lines: ['No registered World/Genome from the previous step; nothing to run yet.']};
	}
	const unfreeze = await client.request({command: 'unfreeze'});
	if (unfreeze.error) return {phase: 'error', canRetry: true, lines: [`${unfreeze.error.code}: ${unfreeze.error.message}`]};
	const acknowledgement = unfreeze.data?.type === 'acknowledged'
		? `Ledgered: evolution ${unfreeze.data.frozen ? 'remains frozen' : 'unfrozen'}.`
		: 'Unfreeze acknowledged.';
	const jobId = `tour-run-${parentGenomeId.slice(0, 12)}`;
	const submitted = await client.request({command: 'run_submit', job_id: jobId, genome_id: parentGenomeId});
	if (submitted.error) {
		return {
			phase: 'unavailable',
			canRetry: true,
			lines: [
				acknowledgement,
				`Run refused: ${submitted.error.code}: ${submitted.error.message}`,
				'On a host with no verified OS sandbox (no macOS Seatbelt), candidate/reference execution is refused by design.',
				'What is still real: registration, replay, and inspection above and below.',
			],
		};
	}
	let terminal: ApiResponse | undefined = submitted;
	for (let attempt = 0; attempt < 100; attempt += 1) {
		if (terminal?.data?.type === 'job' && ['succeeded', 'failed', 'interrupted'].includes(terminal.data.job.state)) break;
		await new Promise(resolve => setTimeout(resolve, 100));
		terminal = await client.request({command: 'job_status', job_id: jobId});
	}
	const job = terminal?.data?.type === 'job' ? terminal.data.job : undefined;
	if (!job) return {phase: 'error', canRetry: true, lines: [acknowledgement, 'Run did not reach a terminal state in time.']};
	return {
		phase: job.state === 'succeeded' ? 'done' : 'error',
		canRetry: job.state !== 'succeeded',
		lines: [
			acknowledgement,
			`Run ${job.run_id} · ${job.state}${job.terminal ? ` (${job.terminal})` : ''}`,
			`Source revision ${job.source_revision.slice(0, 12)}… · ${terminal?.data?.type === 'job' ? terminal.data.progress.trace_events : 0} trace events`,
		],
	};
}

export async function measureInArena(client: TourClient, worldId: string | undefined, parentGenomeId: string | undefined, candidateGenomeId: string | undefined, onProgress: (job: ArenaJobProgress) => void): Promise<StepResult & {evaluationId?: string}> {
	if (!worldId || !parentGenomeId || !candidateGenomeId) {
		return {phase: 'unavailable', canRetry: false, lines: ['No registered World/Genomes from the lineage step; nothing to measure yet.']};
	}
	const evaluationId = `tour-${parentGenomeId.slice(0, 8)}-${candidateGenomeId.slice(0, 8)}`;
	const started = await client.request({command: 'evaluate_pair', evaluation_id: evaluationId, parent_genome_id: parentGenomeId, candidate_genome_id: candidateGenomeId});
	if (started.error) {
		return {
			phase: 'unavailable',
			canRetry: true,
			lines: [
				`Arena refused: ${started.error.code}: ${started.error.message}`,
				'This needs the same OS sandbox as a direct run; on a host without one, measurement is refused by design.',
			],
		};
	}
	let latest = started;
	for (let attempt = 0; attempt < 200; attempt += 1) {
		if (latest.data?.type === 'arena_job') {
			onProgress(latest.data.job);
			if (!['admitted', 'running', 'cancellation_requested'].includes(latest.data.job.state)) break;
		}
		await new Promise(resolve => setTimeout(resolve, 100));
		latest = await client.request({command: 'job_status', job_id: evaluationId});
	}
	const job = latest.data?.type === 'arena_job' ? latest.data.job : undefined;
	if (!job || job.state !== 'succeeded' || !job.evaluation) {
		return {phase: 'error', canRetry: true, lines: [`Arena evaluation did not reach a visible score (state: ${job?.state ?? 'unknown'}).`]};
	}
	const list = await client.request({command: 'evaluation_list', limit: 50});
	const entry: EvaluationListEntry | undefined = list.data?.type === 'evaluation_list'
		? list.data.evaluations.find(candidate => candidate.evaluation.evaluation_id === evaluationId)
		: undefined;
	const lines = [
		`Visible score: parent ${job.evaluation.parent_visible_correct}/${job.evaluation.visible_total} → candidate ${job.evaluation.candidate_visible_correct}/${job.evaluation.visible_total}`,
		'Sealed tasks are never shown here — only visible-task correctness and this aggregate ever leave the Arena.',
	];
	if (entry?.selection) {
		const selection = entry.selection;
		lines.push(
			`Correctness delta ${selection.estimate_bps} bps (95% CI ${selection.lower_bps}..${selection.upper_bps} bps)`,
			`Eligible for promotion: ${selection.promotion_eligible ? 'yes' : 'no'} (metrics ${selection.metrics_eligible ? 'pass' : 'fail'}, invariants ${selection.invariant_gate_verified ? 'verified' : 'unverified'})`,
			celebrationBanner(selection.promotion_eligible),
		);
	} else {
		lines.push('No selection receipt yet for this evaluation; run `hephaestus arena select` to compute one.');
	}
	return {phase: 'done', canRetry: false, lines, evaluationId};
}

export async function proveReplay(client: TourClient): Promise<StepResult> {
	const status = await client.request({command: 'status'});
	const replay = await client.request({command: 'replay'});
	if (replay.error) return {phase: 'error', canRetry: true, lines: [`${replay.error.code}: ${replay.error.message}`]};
	if (status.data?.type !== 'status' || replay.data?.type !== 'replay') {
		return {phase: 'error', canRetry: true, lines: ['Unexpected response shape from status/replay.']};
	}
	const matches = status.data.event_count === replay.data.event_count
		&& status.data.frozen === replay.data.frozen
		&& status.data.active_runs === replay.data.active_runs;
	return {
		phase: 'done',
		canRetry: false,
		lines: [
			`Replayed ${replay.data.event_count} events · frozen=${replay.data.frozen} · active_runs=${replay.data.active_runs}`,
			`Projection ${replay.data.projection_hash.slice(0, 16)}…`,
			replaySeal(matches),
			'Freeze/kill controls: `hephaestus freeze` and `hephaestus kill --all` from the home menu.',
		],
	};
}

/** A short torch flicker between the two crests; stops after
 * `INTRO_FRAME_COUNT` frames (well under 1.5s) or immediately when
 * `animate` is false, so tests and non-interactive terminals never spin. */
function IntroAnimation({animate}: {animate: boolean}) {
	const theme = useTheme();
	const [frame, setFrame] = useState(0);
	useEffect(() => {
		if (!animate) return;
		if (frame >= INTRO_FRAME_COUNT - 1) return;
		const timer = setTimeout(() => setFrame(value => value + 1), INTRO_FRAME_MS);
		return () => clearTimeout(timer);
	}, [animate, frame]);
	return <Box>
		<Text {...colorProps(theme.color('challenger'), true)}> CHALLENGER </Text>
		<Text> <TorchFlicker animate={animate} frame={animate ? undefined : 0} /> </Text>
		<Text {...colorProps(theme.color('champion'), true)}> CHAMPION </Text>
	</Box>;
}

function ArenaLightsLine({job, animate}: {job: ArenaJobProgress | undefined; animate: boolean}) {
	const theme = useTheme();
	if (!job) return <Box>
		<CrestClash animate={animate} />
		<Text {...colorProps(theme.color('muted'))}>  Waiting for the Arena to admit trials…</Text>
	</Box>;
	return <Text>
		<Text {...colorProps(theme.color('challenger'))}>parent</Text>{' vs '}<Text {...colorProps(theme.color('champion'))}>candidate</Text>{'  '}
		{arenaLights(job.completed_trials, job.total_trials)} {job.completed_trials}/{job.total_trials}
	</Text>;
}

/**
 * First-run product tour: six short, learn-by-doing steps against the real
 * daemon (Welcome, Lineage, Unfreeze & run, Arena, Replay, Done). Every step
 * shows `Step n of N`, a one-sentence why, its live result, and the
 * Next/Back/Skip/Quit controls; on a host that refuses candidate execution it
 * says so plainly instead of pretending.
 */
export function TourScreen({client, dataDir: dataDirProp, onExit, animate = true, initialStep = 0}: TourScreenProps) {
	const theme = useTheme();
	const dataDir = dataDirProp ?? resolveDataDir();
	const [step, setStep] = useState(Math.max(0, Math.min(TOUR_STEP_COUNT - 1, initialStep)));
	const [results, setResults] = useState<Record<number, StepResult>>({});
	const context = useRef<{worldId?: string; parentGenomeId?: string; candidateGenomeId?: string; evaluationId?: string}>({});
	const [arenaJob, setArenaJob] = useState<ArenaJobProgress>();
	const running = useRef(false);

	const runCurrentStep = React.useCallback(async () => {
		if (running.current) return;
		running.current = true;
		setResults(previous => ({...previous, [step]: {phase: 'running', lines: [], canRetry: false}}));
		try {
			let result: StepResult;
			switch (TOUR_STEPS[step]?.id) {
				case 'welcome':
					result = {phase: 'done', canRetry: false, lines: ['Every claim below is backed by a receipt in the canonical event ledger.']};
					break;
				case 'lineage': {
					result = await registerLineage(client, dataDir);
					if (result.phase === 'done') {
						const worlds = await client.request({command: 'world_list'});
						const genomes = await client.request({command: 'genome_list'});
						if (worlds.data?.type === 'worlds' && worlds.data.worlds[0]) context.current.worldId = worlds.data.worlds[0].world_id;
						if (genomes.data?.type === 'genomes') {
							const parent = genomes.data.genomes.find(genome => genome.parent_ids.length === 0);
							const candidate = genomes.data.genomes.find(genome => genome.parent_ids.length > 0);
							if (parent) context.current.parentGenomeId = parent.genome_id;
							if (candidate) context.current.candidateGenomeId = candidate.genome_id;
						}
					}
					break;
				}
				case 'unfreeze_run':
					result = await unfreezeAndRun(client, context.current.worldId, context.current.parentGenomeId);
					break;
				case 'arena': {
					const arenaResult = await measureInArena(client, context.current.worldId, context.current.parentGenomeId, context.current.candidateGenomeId, setArenaJob);
					if (arenaResult.evaluationId) context.current.evaluationId = arenaResult.evaluationId;
					result = arenaResult;
					break;
				}
				case 'replay':
					result = await proveReplay(client);
					break;
				default:
					result = {
						phase: 'done',
						canRetry: false,
						lines: [
							'Lineage & Champions, Evidence & Costs, Gene Bank, and Drift/Canary/Meta-eval are on the home menu.',
							'Replay this tour any time with `heph --tour` (or `hephaestus tui --tour`).',
							'More: docs/GETTING_STARTED.md',
						],
					};
			}
			setResults(previous => ({...previous, [step]: result}));
		} finally {
			running.current = false;
		}
	}, [client, dataDir, step]);

	useEffect(() => {
		if (!results[step]) void runCurrentStep();
	}, [step, results, runCurrentStep]);

	const finish = (completed: boolean) => {
		writeTourMarker(dataDir, {completed});
		onExit();
	};

	useInput((input, key) => {
		if (running.current) return;
		const outcome = applyTourInput(step, input, key);
		if (outcome.retry) { setResults(previous => { const next = {...previous}; delete next[step]; return next; }); return; }
		if (outcome.finished === 'quit') { finish(false); return; }
		if (outcome.finished === 'skipped' || outcome.finished === 'completed') { finish(true); return; }
		if (outcome.step !== step) setStep(outcome.step);
	});

	const current = TOUR_STEPS[step];
	const result = results[step] ?? idleResult;
	return <Box flexDirection="column" borderStyle="round" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Box justifyContent="space-between">
			<Text bold {...colorProps(theme.color('judge'))}>{stepProgressLabel(step)}</Text>
			<ProgressBar
				ratio={(step + 1) / TOUR_STEP_COUNT} width={24} animate={animate}
				color={theme.color('champion')} highlightColor={theme.color('championBright')} trackColor={theme.color('muted')}
			/>
		</Box>
		{step === 0 && <Box marginY={1}><IntroAnimation animate={animate} /></Box>}
		<Text bold {...colorProps(theme.color('ink'))}>{current?.title}</Text>
		<Text {...colorProps(theme.color('inkDim'))}>{current?.why}</Text>
		<Box marginTop={1} flexDirection="column">
			{result.phase === 'running' && <Text {...colorProps(theme.color('judge'))}>Working…</Text>}
			{TOUR_STEPS[step]?.id === 'arena' && result.phase !== 'done' && <ArenaLightsLine job={arenaJob} animate={animate} />}
			{result.lines.map((line, index) => /eligible for promotion!/.test(line)
				? <CelebrationBurst key={index} active label="PROMOTION" animate={animate} />
				: <Text key={index}>{line}</Text>)}
			{result.phase === 'error' && <Text {...colorProps(theme.color('danger'))}>Something went wrong; press r to retry.</Text>}
		</Box>
		<Box marginTop={1}>
			<Text {...colorProps(theme.color('muted'))}>{controlsLine()}{result.canRetry ? '   [r] Retry' : ''}</Text>
		</Box>
	</Box>;
}
