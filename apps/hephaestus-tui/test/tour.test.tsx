import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, mkdir, rm, writeFile, readFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {renderToString} from 'ink';
import {
	TOUR_STEP_COUNT, applyTourInput, arenaLights, celebrationBanner, controlsLine,
	introTorchGlyph, progressBar, quickstartFixtureAvailable, quickstartFixturePaths,
	readTourMarker, renderCandidateTemplate, renderWorldTemplate, replaySeal,
	stepProgressLabel, writeTourMarker,
} from '../src/tour.js';
import {
	TourScreen, measureInArena, proveReplay, registerLineage, unfreezeAndRun,
	type TourClient,
} from '../src/tour-view.js';
import type {ApiResponse, Command, ResponseData} from '../src/protocol.js';

// --- pure helpers -----------------------------------------------------------

test('stepProgressLabel and progressBar report bounded, monotonic progress', () => {
	assert.equal(stepProgressLabel(0, 6), 'Step 1 of 6');
	assert.equal(stepProgressLabel(5, 6), 'Step 6 of 6');
	const bars = [0, 1, 2, 3, 4, 5].map(step => progressBar(step, 6, 12));
	for (const bar of bars) assert.equal(bar.length, 12);
	// Every later step fills at least as many segments as the one before.
	const filledCounts = bars.map(bar => bar.split('').filter(char => char === '█').length);
	for (let index = 1; index < filledCounts.length; index += 1) {
		assert.ok(filledCounts[index]! >= filledCounts[index - 1]!);
	}
	assert.equal(filledCounts.at(-1), 12);
});

test('controlsLine always names Next, Back, Skip, and Quit', () => {
	const line = controlsLine();
	assert.match(line, /Next/);
	assert.match(line, /Back/);
	assert.match(line, /Skip/);
	assert.match(line, /Quit/);
});

test('introTorchGlyph cycles deterministically and never throws on negative frames', () => {
	const frames = [0, 1, 2, 3, 4].map(introTorchGlyph);
	assert.equal(frames[0], frames[4]);
	assert.equal(typeof introTorchGlyph(-1), 'string');
});

test('arenaLights lights exactly the completed trials, clamped to total', () => {
	assert.equal(arenaLights(0, 2), '○○');
	assert.equal(arenaLights(1, 2), '●○');
	assert.equal(arenaLights(2, 2), '●●');
	assert.equal(arenaLights(5, 2), '●●');
	assert.equal(arenaLights(-1, 2), '○○');
});

test('celebrationBanner and replaySeal state the honest outcome plainly', () => {
	assert.match(celebrationBanner(true), /eligible for promotion!/);
	assert.match(celebrationBanner(false), /not \(yet\) eligible/);
	assert.match(replaySeal(true), /sealed/);
	assert.match(replaySeal(false), /unsealed/);
});

test('renderWorldTemplate and renderCandidateTemplate substitute every placeholder', () => {
	const world = renderWorldTemplate('{"a":"__VISIBLE_MANIFEST__","b":"__SEALED_MANIFEST__","c":"__EVALUATOR__","d":"__VERIFIER__"}', {
		visible: 'v1', sealed: 's1', evaluator: 'e1', verifier: 'k1',
	});
	assert.equal(world, '{"a":"v1","b":"s1","c":"e1","d":"k1"}');
	assert.equal(renderCandidateTemplate('parents: ["__PARENT_ID__"]', 'genome-123'), 'parents: ["genome-123"]');
});

test('the first-run marker reads as incomplete until the tour writes it', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tour-'));
	try {
		assert.equal(readTourMarker(dataDir).completed, false);
		writeTourMarker(dataDir, {completed: true});
		assert.equal(readTourMarker(dataDir).completed, true);
		writeTourMarker(dataDir, {completed: false});
		assert.equal(readTourMarker(dataDir).completed, false);
	} finally {
		await rm(dataDir, {recursive: true, force: true});
	}
});

test('a malformed marker file reads as incomplete rather than throwing', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tour-'));
	try {
		await mkdir(join(dataDir, 'tui'), {recursive: true});
		await writeFile(join(dataDir, 'tui', 'tour.json'), 'not json');
		assert.equal(readTourMarker(dataDir).completed, false);
	} finally {
		await rm(dataDir, {recursive: true, force: true});
	}
});

test('quickstartFixtureAvailable is false until every fixture file exists', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tour-'));
	try {
		const paths = quickstartFixturePaths(dataDir);
		assert.equal(quickstartFixtureAvailable(paths), false);
		await mkdir(join(dataDir, 'quickstart', 'tasks'), {recursive: true});
		await writeFile(paths.worldTemplate, '{}');
		await writeFile(paths.parentGenome, '---\n---\n');
		await writeFile(paths.candidateTemplate, '---\n---\n');
		assert.equal(quickstartFixtureAvailable(paths), false);
		await writeFile(paths.visibleTasks, '[]');
		await writeFile(paths.sealedTasks, '[]');
		assert.equal(quickstartFixtureAvailable(paths), true);
	} finally {
		await rm(dataDir, {recursive: true, force: true});
	}
});

// --- applyTourInput state machine ------------------------------------------

test('applyTourInput advances, retreats, and clamps at both ends', () => {
	assert.deepEqual(applyTourInput(0, '', {return: true}), {step: 1, finished: null, retry: false});
	assert.deepEqual(applyTourInput(0, '', {}), {step: 0, finished: null, retry: false});
	assert.deepEqual(applyTourInput(0, 'b', {}), {step: 0, finished: null, retry: false});
	assert.deepEqual(applyTourInput(2, 'b', {}), {step: 1, finished: null, retry: false});
	assert.deepEqual(applyTourInput(TOUR_STEP_COUNT - 1, '', {return: true}), {step: TOUR_STEP_COUNT - 1, finished: 'completed', retry: false});
});

test('applyTourInput reports skip, quit, and retry without moving the step', () => {
	assert.deepEqual(applyTourInput(2, 's', {}), {step: 2, finished: 'skipped', retry: false});
	assert.deepEqual(applyTourInput(2, 'q', {}), {step: 2, finished: 'quit', retry: false});
	assert.deepEqual(applyTourInput(2, '', {escape: true}), {step: 2, finished: 'quit', retry: false});
	assert.deepEqual(applyTourInput(2, 'r', {}), {step: 2, finished: null, retry: true});
});

// --- scripted fake-daemon steps ---------------------------------------------

function fakeClient(handlers: Partial<Record<Command['command'], (command: Command) => ApiResponse>>): TourClient {
	return {
		async request(command) {
			const handler = handlers[command.command];
			if (!handler) throw new Error(`unscripted command: ${command.command}`);
			return handler(command);
		},
	};
}

function ok(data: ResponseData): ApiResponse {
	return {version: 1, request_id: 'r', data};
}

function err(message = 'canonical operation failed'): ApiResponse {
	return {version: 1, request_id: 'r', error: {code: 'internal', message}};
}

test('registerLineage reuses an already-registered World and its Genomes', async () => {
	const client = fakeClient({
		world_list: () => ok({type: 'worlds', worlds: [{world_id: 'world-1', name: 'quickstart-world', artifact_id: 'a1'}]}),
		genome_list: () => ok({
			type: 'genomes',
			genomes: [
				{genome_id: 'parent-1', name: 'quickstart-parent', world_id: 'world-1', artifact_id: 'a2', parent_ids: []},
				{genome_id: 'candidate-1', name: 'quickstart-candidate', world_id: 'world-1', artifact_id: 'a3', parent_ids: ['parent-1']},
			],
		}),
	});
	const result = await registerLineage(client, '/nonexistent-data-dir');
	assert.equal(result.phase, 'done');
	assert.match(result.lines.join('\n'), /Reusing already-registered World/);
});

test('registerLineage explains plainly when no bundled fixture exists yet', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tour-'));
	try {
		const client = fakeClient({world_list: () => ok({type: 'worlds', worlds: []})});
		const result = await registerLineage(client, dataDir);
		assert.equal(result.phase, 'unavailable');
		assert.match(result.lines.join('\n'), /No bundled quickstart fixture/);
		assert.equal(result.canRetry, true);
	} finally {
		await rm(dataDir, {recursive: true, force: true});
	}
});

test('registerLineage registers a fresh World and both Genomes from the bundled fixture', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tour-'));
	const evaluator = join(dataDir, 'fake-evaluator');
	const previousOverride = process.env['HEPHAESTUS_EVALUATOR'];
	try {
		const paths = quickstartFixturePaths(dataDir);
		await mkdir(join(dataDir, 'quickstart', 'tasks'), {recursive: true});
		await writeFile(paths.worldTemplate, '{"evaluator":"__EVALUATOR__","verifier":"__VERIFIER__","visible":"__VISIBLE_MANIFEST__","sealed":"__SEALED_MANIFEST__"}');
		await writeFile(paths.parentGenome, '---\nname: quickstart-parent\nparents: []\n---\n');
		await writeFile(paths.candidateTemplate, '---\nname: quickstart-candidate\nparents: ["__PARENT_ID__"]\n---\n');
		await writeFile(paths.visibleTasks, '[]');
		await writeFile(paths.sealedTasks, '[]');
		await writeFile(evaluator, '#!/bin/sh\n');
		process.env['HEPHAESTUS_EVALUATOR'] = evaluator;

		const client = fakeClient({
			world_list: () => ok({type: 'worlds', worlds: []}),
			manifest_put: () => ok({type: 'artifact', artifact_id: 'manifest-id', bytes: 2}),
			artifact_put: () => ok({type: 'artifact', artifact_id: 'evaluator-id', bytes: 4}),
			verifier_show: () => ok({type: 'verifier', artifact_id: 'verifier-id', public_key_hex: 'ab'.repeat(32)}),
			world_register: () => ok({type: 'world', world: {world_id: 'world-9', name: 'quickstart-world', artifact_id: 'wa'}}),
			genome_register: command => {
				const registered = command as Extract<Command, {command: 'genome_register'}>;
				const isCandidate = registered.path.endsWith('candidate.md');
				return ok({
					type: 'genome',
					genome: {
						genome_id: isCandidate ? 'candidate-9' : 'parent-9',
						name: isCandidate ? 'quickstart-candidate' : 'quickstart-parent',
						world_id: registered.world_id,
						artifact_id: 'ga',
						parent_ids: isCandidate ? ['parent-9'] : [],
					},
				});
			},
		});

		const result = await registerLineage(client, dataDir);
		assert.equal(result.phase, 'done');
		assert.match(result.lines.join('\n'), /Registered World quickstart-world/);
		assert.match(result.lines.join('\n'), /Registered parent Genome/);
		assert.match(result.lines.join('\n'), /Registered candidate Genome/);

		const candidateWork = await readFile(join(dataDir, 'tour', 'work', 'candidate.md'), 'utf8');
		assert.match(candidateWork, /parents: \["parent-9"\]/);
		const worldWork = await readFile(join(dataDir, 'tour', 'work', 'world.json'), 'utf8');
		assert.match(worldWork, /evaluator-id/);
		assert.match(worldWork, /verifier-id/);
	} finally {
		if (previousOverride === undefined) delete process.env['HEPHAESTUS_EVALUATOR'];
		else process.env['HEPHAESTUS_EVALUATOR'] = previousOverride;
		await rm(dataDir, {recursive: true, force: true});
	}
});

test('unfreezeAndRun says so plainly when the run is refused (no OS sandbox)', async () => {
	const client = fakeClient({
		unfreeze: () => ok({type: 'acknowledged', frozen: false, killed_runs: 0}),
		run_submit: () => err('canonical operation failed'),
	});
	const result = await unfreezeAndRun(client, 'world-1', 'parent-1');
	assert.equal(result.phase, 'unavailable');
	assert.match(result.lines.join('\n'), /no verified OS sandbox/);
	assert.match(result.lines.join('\n'), /registration, replay, and inspection/);
});

test('unfreezeAndRun reports a successful terminal run', async () => {
	const client = fakeClient({
		unfreeze: () => ok({type: 'acknowledged', frozen: false, killed_runs: 0}),
		run_submit: () => ok({
			type: 'job',
			job: {
				job_id: 'tour-run', genome_id: 'parent-1', run_id: 'run-1', source_revision: 'abcdef123456',
				world_id: 'world-1', task_id: '', input_commitment: '', seed: 0, environment_id: 'env',
				budget: {}, state: 'succeeded', terminal: 'succeeded',
			},
			progress: {trace_events: 3, last_event_sequence: 9, last_phase: null},
		}),
	});
	const result = await unfreezeAndRun(client, 'world-1', 'parent-1');
	assert.equal(result.phase, 'done');
	assert.match(result.lines.join('\n'), /run-1/);
});

test('measureInArena surfaces the visible score, eligibility, and CI from a scripted evaluation', async () => {
	const evaluation = {parent_visible_correct: 0, candidate_visible_correct: 1, visible_total: 1};
	const client = fakeClient({
		evaluate_pair: () => ok({
			type: 'arena_job',
			job: {evaluation_id: 'eval-1', parent_genome_id: 'parent-1', candidate_genome_id: 'candidate-1', state: 'succeeded', phase: 'terminal', completed_trials: 2, total_trials: 2, evaluation},
		}),
		job_status: () => ok({
			type: 'arena_job',
			job: {evaluation_id: 'eval-1', parent_genome_id: 'parent-1', candidate_genome_id: 'candidate-1', state: 'succeeded', phase: 'terminal', completed_trials: 2, total_trials: 2, evaluation},
		}),
		evaluation_list: () => ok({
			type: 'evaluation_list',
			evaluations: [{
				evaluation: {evaluation_id: 'tour-parent-1-candidat', world_id: 'world-1', parent_genome_id: 'parent-1', candidate_genome_id: 'candidate-1', ...evaluation},
				selection: {
					metrics_eligible: true, estimate_bps: 5000, lower_bps: 100, upper_bps: 9000,
					parent_cost_microusd: 0, candidate_cost_microusd: 0, parent_latency_millis: 1, candidate_latency_millis: 1,
					invariant_gate_verified: true, promotion_eligible: false,
				},
				invariants: null, forge_assessment: null, champion_transition_ids: [],
			}],
		}),
	});
	const result = await measureInArena(client, 'world-1', 'parent-1', 'candidate-1', () => {});
	assert.equal(result.phase, 'done');
	const text = result.lines.join('\n');
	assert.match(text, /parent 0\/1 → candidate 1\/1/);
	assert.match(text, /Correctness delta 5000 bps/);
	assert.match(text, /Eligible for promotion: no/);
	assert.match(text, /Sealed tasks are never shown/);
});

test('proveReplay seals a matching replay and flags a divergent one', async () => {
	const matching = fakeClient({
		status: () => ok({type: 'status', frozen: false, active_runs: 0, event_count: 7, genome_count: 2}),
		replay: () => ok({type: 'replay', event_count: 7, frozen: false, active_runs: 0, projection_hash: 'h'.repeat(32)}),
	});
	const matchResult = await proveReplay(matching);
	assert.equal(matchResult.phase, 'done');
	assert.match(matchResult.lines.join('\n'), /sealed —/);

	const divergent = fakeClient({
		status: () => ok({type: 'status', frozen: false, active_runs: 0, event_count: 8, genome_count: 2}),
		replay: () => ok({type: 'replay', event_count: 7, frozen: false, active_runs: 0, projection_hash: 'h'.repeat(32)}),
	});
	const divergentResult = await proveReplay(divergent);
	assert.match(divergentResult.lines.join('\n'), /unsealed/);
});

// --- rendering ---------------------------------------------------------------

test('TourScreen renders the welcome step with progress, why, and controls', () => {
	const client = fakeClient({});
	const rendered = renderToString(<TourScreen client={client} dataDir="/nonexistent" onExit={() => {}} animate={false} />);
	assert.match(rendered, /Step 1 of 6/);
	assert.match(rendered, /Welcome to Hephaestus/);
	assert.match(rendered, /CHALLENGER/);
	assert.match(rendered, /CHAMPION/);
	assert.match(rendered, /Skip tour/);
});

test('TourScreen can jump straight to the Done step, fully progressed', () => {
	const client = fakeClient({});
	const rendered = renderToString(<TourScreen client={client} dataDir="/nonexistent" onExit={() => {}} animate={false} initialStep={5} />);
	assert.match(rendered, /Step 6 of 6/);
	assert.match(rendered, /Done/);
	assert.match(rendered, /how to see this tour again/);
});

test('TourScreen renders every step of the tour with its progress label, title, and controls', () => {
	// Each step's on-mount work is scripted just enough to avoid an
	// unscripted-command exception when landing directly on that step with no
	// prior steps run: `lineage` sees no Worlds (and no fixture on disk, so it
	// resolves to an honest "unavailable" with no further calls);
	// `unfreeze_run` and `arena` have no World/Genome context yet from a fresh
	// mount and short-circuit before calling the client at all; `replay`
	// always calls status/replay. `renderToString` captures a synchronous
	// snapshot right after mount, before any of these awaited calls settle,
	// so this only asserts the synchronously available chrome (progress,
	// title, controls) that every screen must show immediately — the scripted
	// step-result content itself is covered by the dedicated
	// registerLineage/unfreezeAndRun/measureInArena/proveReplay tests above.
	const client = fakeClient({
		world_list: () => ok({type: 'worlds', worlds: []}),
		status: () => ok({type: 'status', frozen: false, active_runs: 0, event_count: 0, genome_count: 0}),
		replay: () => ok({type: 'replay', event_count: 0, frozen: false, active_runs: 0, projection_hash: 'h'.repeat(32)}),
	});
	const titles = [
		'Welcome to Hephaestus', 'Your first World and Genomes', 'Unfreeze and run',
		'Measure in the Arena', 'Prove it', 'Done',
	];
	assert.equal(titles.length, TOUR_STEP_COUNT);
	for (const [step, title] of titles.entries()) {
		const rendered = renderToString(<TourScreen client={client} dataDir="/nonexistent" onExit={() => {}} animate={false} initialStep={step} />);
		assert.match(rendered, new RegExp(`Step ${step + 1} of ${TOUR_STEP_COUNT}`), `step ${step} progress label`);
		assert.match(rendered, new RegExp(title), `step ${step} title`);
		assert.match(rendered, /Skip tour/, `step ${step} controls`);
	}
});
