import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {mkdtemp, chmod, writeFile, symlink, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import test from 'node:test';
import {performance} from 'node:perf_hooks';
import {createServer} from 'node:net';
import {ControlClient} from '../src/client.js';

test('rejects oversized and non-regular operator tokens before socket access', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tui-token-'));
	const tokenPath = join(dataDir, 'operator.token');
	try {
		await chmod(dataDir, 0o700);
		await writeFile(tokenPath, 'a'.repeat(1024), {mode: 0o600});
		const client = new ControlClient({dataDir});
		await assert.rejects(client.request({command: 'status'}), /operator token file is malformed/);
		await rm(tokenPath);
		await symlink(join(dataDir, 'missing'), tokenPath);
		await assert.rejects(client.request({command: 'status'}), /operator token must be an owner-only file/);
		await rm(tokenPath);
		execFileSync('mkfifo', [tokenPath]);
		await assert.rejects(client.request({command: 'status'}), /operator token must be an owner-only file/);
	} finally {
		await rm(dataDir, {recursive: true, force: true});
	}
});

test('aborting an in-flight daemon request destroys its socket promptly', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tui-abort-'));
	await chmod(dataDir, 0o700);
	await writeFile(join(dataDir, 'operator.token'), 'a'.repeat(64), {mode: 0o600});
	let resolveRequestSeen!: () => void;
	let resolveSocketClosed!: () => void;
	const requestSeen = new Promise<void>(resolve => { resolveRequestSeen = resolve; });
	const socketClosed = new Promise<void>(resolve => { resolveSocketClosed = resolve; });
	const server = createServer(socket => {
		socket.on('data', resolveRequestSeen);
		socket.on('close', resolveSocketClosed);
	});
	const socketPath = join(dataDir, 'control.sock');
	await new Promise<void>(resolve => server.listen(socketPath, resolve));
	await chmod(socketPath, 0o600);
	const controller = new AbortController();
	try {
		const request = new ControlClient({dataDir, timeoutMs: 10_000}).request({command: 'status'}, controller.signal);
		await requestSeen;
		const started = performance.now();
		controller.abort();
		await assert.rejects(request, /daemon request aborted/);
		await socketClosed;
		assert.ok(performance.now() - started < 250, 'abort should destroy the socket without waiting for its deadline');
	} finally {
		server.close();
		await rm(dataDir, {recursive: true, force: true});
	}
});
