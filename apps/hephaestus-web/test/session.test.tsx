import assert from 'node:assert/strict';
import test from 'node:test';
import {clearSession, requestCommand, sessionToken} from '../src/web/session.js';

function storage(): Pick<Storage, 'getItem' | 'setItem' | 'removeItem'> {
	const values = new Map<string, string>();
	return {getItem: key => values.get(key) ?? null, setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); }};
}

test('the fragment establishes a session that survives reloads in the same tab', () => {
	const state = storage(); const token = 'a'.repeat(64);
	assert.equal(sessionToken(`#token=${token}`, state), token);
	assert.equal(sessionToken('', state), token);
	clearSession(state); assert.equal(sessionToken('', state), '');
});

test('a fresh console URL replaces an old session; malformed fragments clear it', () => {
	const state = storage(); sessionToken(`#token=${'a'.repeat(64)}`, state);
	assert.equal(sessionToken(`#token=${'b'.repeat(64)}`, state), 'b'.repeat(64));
	assert.equal(sessionToken('#token=bad', state), '');
	assert.equal(sessionToken('', state), '');
});

test('disabled browser storage still allows the printed URL to work', () => {
	const fail = () => { throw new Error('disabled'); };
	const state = {getItem: fail, setItem: fail, removeItem: fail};
	assert.equal(sessionToken(`#token=${'c'.repeat(64)}`, state), 'c'.repeat(64));
	assert.equal(sessionToken('', state), '');
	assert.doesNotThrow(() => clearSession(state));
});

test('an unauthenticated tab makes no API request and tells the user how to connect', async () => {
	let called = false;
	const result = await requestCommand({command: 'status'}, '', async () => { called = true; throw new Error('must not fetch'); });
	assert.equal(called, false); assert.equal(result.error?.code, 'unauthorized'); assert.match(result.error!.message, /full URL/);
});

test('expired sessions get an actionable error even if the HTTP body is not JSON', async () => {
	const result = await requestCommand({command: 'status'}, 'token', async () => new Response('expired', {status: 401}));
	assert.equal(result.error?.code, 'session_expired'); assert.match(result.error!.message, /new URL/);
});

test('daemon and Origin authentication failures remain distinct from an expired browser session', async () => {
	const state = storage(); const token = sessionToken(`#token=${'a'.repeat(64)}`, state);
	for (const status of [200, 403]) {
		const payload = {version: 1, request_id: 'r1', error: {code: 'unauthorized', message: 'daemon or Origin denied'}};
		const result = await requestCommand({command: 'status'}, token, async () => Response.json(payload, {status}));
		if (result.error?.code === 'session_expired') clearSession(state);
		assert.equal(result.error?.code, 'unauthorized');
		assert.equal(sessionToken('', state), token);
	}
	const expired = await requestCommand({command: 'status'}, token, async () => new Response('', {status: 401}));
	if (expired.error?.code === 'session_expired') clearSession(state);
	assert.equal(sessionToken('', state), '');
});

test('connection failures and HTML error pages return visible errors rather than rejected promises', async () => {
	for (const send of [async () => { throw new TypeError('network failed'); }, async () => new Response('<html>gateway error</html>', {status: 502})]) {
		const result = await requestCommand({command: 'status'}, 'token', send);
		assert.equal(result.error?.code, 'connection'); assert.match(result.error!.message, /Refresh/);
	}
});

test('valid daemon data and daemon errors retain their exact meaning', async () => {
	for (const payload of [
		{version: 1, request_id: 'r1', data: {type: 'status', frozen: true, active_runs: 0, event_count: 0, genome_count: 0}},
		{version: 1, request_id: 'r1', error: {code: 'not_found', message: 'unknown job'}},
	]) {
		assert.deepEqual(await requestCommand({command: 'status'}, 'token', async (_url, init) => {
			assert.equal((init!.headers as Record<string, string>)['x-hephaestus-web-token'], 'token');
			assert.ok(init!.signal instanceof AbortSignal);
			return Response.json(payload);
		}), payload);
	}
});

test('an empty or incompatible response fails visibly', async () => {
	for (const payload of [{}, {version: 2, request_id: 'bad', data: {type: 'status'}}]) {
		assert.equal((await requestCommand({command: 'status'}, 'token', async () => Response.json(payload))).error?.code, 'connection');
	}
});
