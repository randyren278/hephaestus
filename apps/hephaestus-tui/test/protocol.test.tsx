import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:net';
import {mkdtemp, chmod, writeFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {renderToString} from 'ink';
import {ControlClient} from '../src/client.js';
import {parseResponse, safeText} from '../src/protocol.js';
import {App, ArenaProgressPanel} from '../src/ui.js';

test('client authenticates over the owner-only Unix socket without displaying its token', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tui-'));
	await chmod(dataDir, 0o700);
	const secret = 'a'.repeat(64);
	await writeFile(join(dataDir, 'operator.token'), secret, {mode: 0o600});
	const server = createServer(socket => {
		let body = '';
		socket.on('data', chunk => { body += chunk.toString(); });
		socket.on('end', () => {
			const request = JSON.parse(body) as {request_id: string; token: string; command: {command: string}};
			assert.equal(request.token, secret);
			assert.ok(['status', 'job_status'].includes(request.command.command));
			const result = request.command.command === 'job_status'
				? {version: 1, request_id: request.request_id, error: {code: 'internal', message: `bad token ${secret}`}}
				: {version: 1, request_id: request.request_id, data: {type: 'status', frozen: true, active_runs: 0, event_count: 4, genome_count: 2}};
			socket.end(JSON.stringify(result));
		});
	});
	const socketPath = join(dataDir, 'control.sock');
	await new Promise<void>(resolve => server.listen(socketPath, resolve));
	await chmod(socketPath, 0o600);
	try {
		const response = await new ControlClient({dataDir}).request({command: 'status'});
		assert.deepEqual(response.data, {type: 'status', frozen: true, active_runs: 0, event_count: 4, genome_count: 2});
		const rejected = await new ControlClient({dataDir}).request({command: 'job_status', job_id: 'safe-id'});
		assert.equal(rejected.error?.message.includes(secret), false);
		const rendered = renderToString(<App client={{request: async () => response}} pollMs={1000} />);
		assert.match(rendered, /LOCAL CONTROL/);
		assert.match(rendered, /OPERATOR ACTIONS/);
		assert.doesNotMatch(rendered, new RegExp(secret));
	} finally {
		server.close();
		await rm(dataDir, {recursive: true, force: true});
	}
});

test('client refuses unsafe token permissions and oversized frames', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tui-'));
	await chmod(dataDir, 0o700);
	await writeFile(join(dataDir, 'operator.token'), 'b'.repeat(64), {mode: 0o644});
	try {
		await assert.rejects(new ControlClient({dataDir}).request({command: 'status'}), /owner-only/);
		await chmod(join(dataDir, 'operator.token'), 0o600);
		await assert.rejects(new ControlClient({dataDir, maxFrameBytes: 1}).request({command: 'status'}), /request exceeds protocol limit/);
	} finally {
		await rm(dataDir, {recursive: true, force: true});
	}
});

test('terminal text strips control characters and the interface renders a safe compact frame', () => {
	assert.equal(safeText('north\n\u001b[2J\u0000south'), 'north [2J south');
	assert.throws(() => parseResponse('{not-json}', 'request'), /daemon response is malformed/);
	assert.throws(() => parseResponse(JSON.stringify({version: 1, request_id: 'request', data: {type: 'status', frozen: false, active_runs: 'many', event_count: 1, genome_count: 0}}), 'request'), /daemon response variant is invalid/);
	const output = renderToString(<App client={{request: async () => ({version: 1, request_id: 'test', data: {type: 'status', frozen: true, active_runs: 0, event_count: 0, genome_count: 0}})}} />);
	assert.match(output, /Kill all active work/);
	assert.match(output, /Arena progress by ID/);
	assert.match(output, /Q quit/);
});

test('job projections accept full-width u64 seeds without displaying them', () => {
	const response = parseResponse(`{"version":1,"request_id":"request","data":{"type":"job","job":{"job_id":"job-1","genome_id":"genome","run_id":"run","source_revision":"rev","world_id":"world","task_id":"task","input_commitment":"commit","seed":18446744073709551615,"environment_id":"env","budget":{},"state":"running","terminal":null},"progress":{"trace_events":0,"last_event_sequence":null,"last_phase":null}}}`, 'request');
	assert.equal(response.data?.type, 'job');
	if (response.data?.type === 'job') assert.equal(response.data.job.job_id, 'job-1');
});

test('Arena job progress accepts authoritative trial and visible-score projections only', () => {
	const text = `{"version":1,"request_id":"request","data":{"type":"arena_job","job":{"evaluation_id":"eval-1","parent_genome_id":"parent-id","candidate_genome_id":"candidate-id","state":"running","phase":"candidate_trials","completed_trials":2,"total_trials":5}}}`;
	const response = parseResponse(text, 'request');
	assert.equal(response.data?.type, 'arena_job');
	if (response.data?.type === 'arena_job') {
		assert.equal(response.data.job.completed_trials, 2);
		const frame = renderToString(<ArenaProgressPanel job={response.data.job} stale={false} />);
		assert.match(frame, /CANDIDATE TRIALS/);
		assert.match(frame, /Trials 2\/5/);
		assert.match(frame, /candidate-id/);
	}
	assert.throws(() => parseResponse(text.replace('"completed_trials":2', '"completed_trials":7'), 'request'), /daemon response variant is invalid/);
	assert.throws(() => parseResponse(text.replace('"total_trials":5', '"total_trials":4294967296'), 'request'), /daemon response variant is invalid/);
});

test('terminal Arena progress renders visible aggregate receipt fields without sealed metrics', () => {
	const response = parseResponse(`{"version":1,"request_id":"request","data":{"type":"arena_job","job":{"evaluation_id":"eval-2","parent_genome_id":"p","candidate_genome_id":"c","state":"succeeded","phase":"terminal","completed_trials":4,"total_trials":4,"evaluation":{"evaluation_id":"eval-2","world_id":"world","parent_genome_id":"p","candidate_genome_id":"c","parent_visible_correct":1,"candidate_visible_correct":2,"visible_total":3,"event":{"sequence":7,"event_id":"e","aggregate_id":"a","event_type":"evaluation.completed","actor":"arena","timestamp_millis":1}}}}}`, 'request');
	assert.equal(response.data?.type, 'arena_job');
	if (response.data?.type === 'arena_job') {
		const frame = renderToString(<ArenaProgressPanel job={response.data.job} stale={false} />);
		assert.match(frame, /Visible score 1 → 2 \/ 3/);
		assert.doesNotMatch(frame, /receipt|cost|sealed/i);
	}
});

test('Arena progress shows an explicit stale state when the daemon is unavailable', () => {
	const frame = renderToString(<ArenaProgressPanel stale notice="daemon socket is unavailable" />);
	assert.match(frame, /STALE/);
	assert.match(frame, /daemon unavailable/);
	assert.match(frame, /job list\./i);
});
