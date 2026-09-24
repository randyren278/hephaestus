import assert from 'node:assert/strict';
import test from 'node:test';
import {createServer as createNetServer} from 'node:net';
import {request as httpRequest} from 'node:http';
import {mkdtemp, chmod, writeFile, mkdir, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import type {AddressInfo} from 'node:net';
import {ControlClient} from '../../hephaestus-tui/src/client.js';
import {createWebServer} from '../src/server.js';
import {generateSessionToken} from '../src/security.js';

/** Bind a throwaway server to discover a free loopback port, then release it. */
async function freePort(): Promise<number> {
	return new Promise((resolve, reject) => {
		const probe = createNetServer();
		probe.once('error', reject);
		probe.listen(0, '127.0.0.1', () => {
			const port = (probe.address() as AddressInfo).port;
			probe.close(() => resolve(port));
		});
	});
}

/** A fake daemon speaking the exact owner-only Unix socket protocol the real client authenticates against. */
async function withFakeDaemon<T>(run: (dataDir: string, secret: string) => Promise<T>): Promise<T> {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-web-'));
	await chmod(dataDir, 0o700);
	const secret = 'b'.repeat(64);
	await writeFile(join(dataDir, 'operator.token'), secret, {mode: 0o600});
	const server = createNetServer(socket => {
		let body = '';
		socket.on('data', chunk => { body += chunk.toString(); });
		socket.on('end', () => {
			const request = JSON.parse(body) as {request_id: string; command: {command: string}};
			socket.end(JSON.stringify({version: 1, request_id: request.request_id, data: {type: 'status', frozen: false, active_runs: 1, event_count: 9, genome_count: 3}}));
		});
	});
	const socketPath = join(dataDir, 'control.sock');
	await new Promise<void>(resolve => server.listen(socketPath, resolve));
	await chmod(socketPath, 0o600);
	try {
		return await run(dataDir, secret);
	} finally {
		server.close();
		await rm(dataDir, {recursive: true, force: true});
	}
}

async function withStaticRoot<T>(run: (staticRoot: string) => Promise<T>): Promise<T> {
	const staticRoot = await mkdtemp(join(tmpdir(), 'hephaestus-web-static-'));
	await mkdir(staticRoot, {recursive: true});
	await writeFile(join(staticRoot, 'index.html'), '<!doctype html><title>t</title>');
	await writeFile(join(staticRoot, 'app.js'), 'console.log("ok");');
	await writeFile(join(staticRoot, 'styles.css'), 'body { color: red; }');
	try {
		return await run(staticRoot);
	} finally {
		await rm(staticRoot, {recursive: true, force: true});
	}
}

test('proxies an allowed command to the real Unix-socket daemon and returns its typed response', async () => {
	await withFakeDaemon(async (dataDir, _secret) => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			const client = new ControlClient({dataDir});
			const server = createWebServer({requestDaemon: command => client.request(command), sessionToken, port, staticRoot});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				const res = await fetch(`http://127.0.0.1:${port}/api/command`, {
					method: 'POST',
					headers: {'Content-Type': 'application/json', 'x-hephaestus-web-token': sessionToken, Host: `127.0.0.1:${port}`},
					body: JSON.stringify({command: 'status'}),
				});
				assert.equal(res.status, 200);
				const body = await res.json();
				assert.deepEqual(body.data, {type: 'status', frozen: false, active_runs: 1, event_count: 9, genome_count: 3});
			} finally {
				server.close();
			}
		});
	});
});

test('refuses a mutating command before it ever reaches the daemon', async () => {
	await withFakeDaemon(async dataDir => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			let daemonCalled = false;
			const server = createWebServer({
				requestDaemon: async command => { daemonCalled = true; return new ControlClient({dataDir}).request(command); },
				sessionToken, port, staticRoot,
			});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				for (const command of ['freeze', 'kill_all', 'champion_rollback', 'daemon_stop']) {
					const res = await fetch(`http://127.0.0.1:${port}/api/command`, {
						method: 'POST',
						headers: {'Content-Type': 'application/json', 'x-hephaestus-web-token': sessionToken},
						body: JSON.stringify({command}),
					});
					assert.equal(res.status, 400, `${command} should be refused`);
				}
				assert.equal(daemonCalled, false, 'the daemon must never see a refused command');
			} finally {
				server.close();
			}
		});
	});
});

test('refuses requests with a missing or wrong session token', async () => {
	await withFakeDaemon(async dataDir => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			const server = createWebServer({requestDaemon: command => new ControlClient({dataDir}).request(command), sessionToken, port, staticRoot});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				const noToken = await fetch(`http://127.0.0.1:${port}/api/command`, {
					method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({command: 'status'}),
				});
				assert.equal(noToken.status, 401);
				const wrongToken = await fetch(`http://127.0.0.1:${port}/api/command`, {
					method: 'POST', headers: {'Content-Type': 'application/json', 'x-hephaestus-web-token': 'f'.repeat(64)}, body: JSON.stringify({command: 'status'}),
				});
				assert.equal(wrongToken.status, 401);
			} finally {
				server.close();
			}
		});
	});
});

/** `fetch` refuses to send a custom `Host` header (a forbidden header per spec); use raw `http.request` to simulate a DNS-rebound browser whose address bar named a different host. */
function rawRequestStatus(port: number, headers: Record<string, string>, body: string): Promise<number> {
	return new Promise((resolve, reject) => {
		const req = httpRequest({hostname: '127.0.0.1', port, path: '/api/command', method: 'POST', headers}, res => {
			res.resume();
			resolve(res.statusCode ?? 0);
		});
		req.on('error', reject);
		req.end(body);
	});
}

test('refuses a rebound Host header even when the request otherwise reaches the loopback port', async () => {
	await withFakeDaemon(async dataDir => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			const server = createWebServer({requestDaemon: command => new ControlClient({dataDir}).request(command), sessionToken, port, staticRoot});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				const body = JSON.stringify({command: 'status'});
				const status = await rawRequestStatus(port, {'Content-Type': 'application/json', 'x-hephaestus-web-token': sessionToken, Host: 'evil.example', 'Content-Length': String(Buffer.byteLength(body))}, body);
				assert.equal(status, 421);
			} finally {
				server.close();
			}
		});
	});
});

test('refuses a cross-origin API call even with a valid token', async () => {
	await withFakeDaemon(async dataDir => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			const server = createWebServer({requestDaemon: command => new ControlClient({dataDir}).request(command), sessionToken, port, staticRoot});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				const res = await fetch(`http://127.0.0.1:${port}/api/command`, {
					method: 'POST',
					headers: {'Content-Type': 'application/json', 'x-hephaestus-web-token': sessionToken, Origin: 'http://evil.example'},
					body: JSON.stringify({command: 'status'}),
				});
				assert.equal(res.status, 403);
			} finally {
				server.close();
			}
		});
	});
});

test('never sets a CORS header and applies a strict CSP with no inline sources', async () => {
	await withFakeDaemon(async dataDir => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			const server = createWebServer({requestDaemon: command => new ControlClient({dataDir}).request(command), sessionToken, port, staticRoot});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				const res = await fetch(`http://127.0.0.1:${port}/`);
				assert.equal(res.headers.get('access-control-allow-origin'), null);
				const csp = res.headers.get('content-security-policy') ?? '';
				assert.match(csp, /script-src 'self'/);
				assert.doesNotMatch(csp, /unsafe-inline/);
				const html = await res.text();
				assert.match(html, /<title>t<\/title>/);
			} finally {
				server.close();
			}
		});
	});
});

test('serves the bundled app.js and styles.css as static files', async () => {
	await withFakeDaemon(async dataDir => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			const server = createWebServer({requestDaemon: command => new ControlClient({dataDir}).request(command), sessionToken, port, staticRoot});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				const app = await fetch(`http://127.0.0.1:${port}/app.js`);
				assert.equal(app.status, 200);
				assert.match(await app.text(), /console\.log/);
				const css = await fetch(`http://127.0.0.1:${port}/styles.css`);
				assert.equal(css.status, 200);
				const missing = await fetch(`http://127.0.0.1:${port}/does-not-exist.js`);
				assert.equal(missing.status, 404);
			} finally {
				server.close();
			}
		});
	});
});

test('rejects a request body over the size limit before it is parsed', async () => {
	await withFakeDaemon(async dataDir => {
		await withStaticRoot(async staticRoot => {
			const port = await freePort();
			const sessionToken = generateSessionToken();
			const server = createWebServer({requestDaemon: command => new ControlClient({dataDir}).request(command), sessionToken, port, staticRoot, maxBodyBytes: 64});
			await new Promise<void>(resolve => server.listen(port, '127.0.0.1', resolve));
			try {
				const res = await fetch(`http://127.0.0.1:${port}/api/command`, {
					method: 'POST',
					headers: {'Content-Type': 'application/json', 'x-hephaestus-web-token': sessionToken},
					body: JSON.stringify({command: 'genome_show', genome_id: 'x'.repeat(200)}),
				});
				assert.equal(res.status, 413);
			} finally {
				server.close();
			}
		});
	});
});
