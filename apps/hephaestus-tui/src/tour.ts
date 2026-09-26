import {existsSync, mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {homedir} from 'node:os';
import {join} from 'node:path';

export type TourStepId = 'welcome' | 'lineage' | 'unfreeze_run' | 'arena' | 'replay' | 'done';

export const TOUR_STEPS: ReadonlyArray<{id: TourStepId; title: string; why: string}> = [
	{
		id: 'welcome',
		title: 'Welcome to Hephaestus',
		why: 'Agents are content-addressed Genomes measured inside Worlds; every claim here carries a receipt.',
	},
	{
		id: 'lineage',
		title: 'Your first World and Genomes',
		why: 'Everything Hephaestus measures starts from one registered World and the Genomes compiled under it.',
	},
	{
		id: 'unfreeze_run',
		title: 'Unfreeze and run',
		why: 'The daemon starts frozen by design; only an explicit operator action lifts that.',
	},
	{
		id: 'arena',
		title: 'Measure in the Arena',
		why: 'A candidate earns trust by beating the parent on the same tasks, not by claiming to.',
	},
	{
		id: 'replay',
		title: 'Prove it',
		why: 'Every action above is canonical history; replay recomputes live state and must match it exactly.',
	},
	{
		id: 'done',
		title: 'Done',
		why: 'Where each screen lives, and how to see this tour again.',
	},
];

export const TOUR_STEP_COUNT = TOUR_STEPS.length;

/** `Step n of N`, the progress affordance every step shows. */
export function stepProgressLabel(stepIndex: number, total = TOUR_STEP_COUNT): string {
	return `Step ${Math.min(total, stepIndex + 1)} of ${total}`;
}

/** A fixed-width filled/unfilled progress bar for `stepIndex` (0-based). */
export function progressBar(stepIndex: number, total = TOUR_STEP_COUNT, width = 24): string {
	const ratio = total <= 0 ? 0 : Math.max(0, Math.min(1, (stepIndex + 1) / total));
	const filled = Math.round(ratio * width);
	return `${'█'.repeat(filled)}${'·'.repeat(Math.max(0, width - filled))}`;
}

const CONTROL_HINT = '[Enter] Next   [b] Back   [s] Skip tour   [q] Quit';

/** The always-visible controls line (Enter/Back/Skip/Quit affordances). */
export function controlsLine(): string {
	return CONTROL_HINT;
}

export type TourInputOutcome = {step: number; finished: 'completed' | 'skipped' | 'quit' | null; retry: boolean};

/**
 * Pure step-transition logic for the tour's Enter/Back/Skip/Quit/Retry
 * controls, kept separate from `TourScreen` so it is unit-testable without
 * rendering Ink. `key` mirrors the subset of Ink's `useInput` key flags used.
 */
export function applyTourInput(step: number, input: string, key: {return?: boolean; escape?: boolean}, total = TOUR_STEP_COUNT): TourInputOutcome {
	if (input === 'q' || input === 'Q' || key.escape) return {step, finished: 'quit', retry: false};
	if (input === 's' || input === 'S') return {step, finished: 'skipped', retry: false};
	if (input === 'r' || input === 'R') return {step, finished: null, retry: true};
	if (key.return) {
		if (step >= total - 1) return {step, finished: 'completed', retry: false};
		return {step: Math.min(total - 1, step + 1), finished: null, retry: false};
	}
	if (input === 'b' || input === 'B') return {step: Math.max(0, step - 1), finished: null, retry: false};
	return {step, finished: null, retry: false};
}

/** Resolves the daemon data directory the same way `ControlClient` does. */
export function resolveDataDir(explicit?: string): string {
	return explicit ?? process.env['HEPHAESTUS_HOME'] ?? join(homedir(), '.hephaestus');
}

export type TourMarker = {completed: boolean};

export function tourMarkerPath(dataDir: string): string {
	return join(dataDir, 'tui', 'tour.json');
}

/** Reads the durable first-run marker; absent or malformed reads as not completed. */
export function readTourMarker(dataDir: string): TourMarker {
	try {
		const parsed = JSON.parse(readFileSync(tourMarkerPath(dataDir), 'utf8')) as unknown;
		if (parsed && typeof parsed === 'object' && typeof (parsed as {completed?: unknown}).completed === 'boolean') {
			return {completed: (parsed as {completed: boolean}).completed};
		}
	} catch {
		// Absent or malformed: the tour has not been completed.
	}
	return {completed: false};
}

/** Persists the first-run marker; failures are swallowed (never blocks the tour). */
export function writeTourMarker(dataDir: string, marker: TourMarker): void {
	try {
		const directory = join(dataDir, 'tui');
		mkdirSync(directory, {recursive: true});
		writeFileSync(join(directory, 'tour.json'), JSON.stringify(marker), {mode: 0o600});
	} catch {
		// Best-effort only; a lost marker just means the tour offers again.
	}
}

export type QuickstartFixturePaths = {
	dir: string;
	worldTemplate: string;
	parentGenome: string;
	candidateTemplate: string;
	visibleTasks: string;
	sealedTasks: string;
};

/** Where `heph`/`hephaestus init --fixture quickstart` puts the bundled fixture. */
export function quickstartFixturePaths(dataDir: string): QuickstartFixturePaths {
	const dir = join(dataDir, 'quickstart');
	return {
		dir,
		worldTemplate: join(dir, 'world.template.json'),
		parentGenome: join(dir, 'agent.md'),
		candidateTemplate: join(dir, 'candidate.md'),
		visibleTasks: join(dir, 'tasks/visible.json'),
		sealedTasks: join(dir, 'tasks/sealed.json'),
	};
}

/** True only when every fixture file the tour needs is actually on disk. */
export function quickstartFixtureAvailable(paths: QuickstartFixturePaths): boolean {
	return [paths.worldTemplate, paths.parentGenome, paths.candidateTemplate, paths.visibleTasks, paths.sealedTasks]
		.every(path => existsSync(path));
}

export function renderWorldTemplate(template: string, ids: {visible: string; sealed: string; evaluator: string; verifier: string}): string {
	return template
		.split('__VISIBLE_MANIFEST__').join(ids.visible)
		.split('__SEALED_MANIFEST__').join(ids.sealed)
		.split('__EVALUATOR__').join(ids.evaluator)
		.split('__VERIFIER__').join(ids.verifier);
}

export function renderCandidateTemplate(template: string, parentGenomeId: string): string {
	return template.split('__PARENT_ID__').join(parentGenomeId);
}

/** A short torch-flame flicker, capped well under 1.5s per loop. */
export const INTRO_FRAME_COUNT = 4;
export const INTRO_FRAME_MS = 300;
const TORCH_FLAMES = ['^', '~', '^', '*'] as const;

export function introTorchGlyph(frame: number): string {
	const index = ((frame % TORCH_FLAMES.length) + TORCH_FLAMES.length) % TORCH_FLAMES.length;
	return TORCH_FLAMES[index] ?? TORCH_FLAMES[0];
}

/** Lit (torch/graphite) vs unlit dots for `completed` of `total` Arena trials. */
export function arenaLights(completed: number, total: number): string {
	const safeTotal = Math.max(1, Math.trunc(total));
	const lit = Math.max(0, Math.min(safeTotal, Math.trunc(completed)));
	return `${'●'.repeat(lit)}${'○'.repeat(safeTotal - lit)}`;
}

export function celebrationBanner(eligible: boolean): string {
	return eligible
		? '*** The candidate is eligible for promotion! ***'
		: 'Recorded — not (yet) eligible for promotion under this World\'s policy.';
}

export function replaySeal(matches: boolean): string {
	return matches
		? 'sealed — replayed state matches live state, byte for byte.'
		: 'unsealed — replayed state diverged from live state.';
}
