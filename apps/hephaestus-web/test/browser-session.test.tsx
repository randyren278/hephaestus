import assert from 'node:assert/strict';
import test from 'node:test';
import {runInNewContext} from 'node:vm';
import {fileURLToPath} from 'node:url';
import {buildSync} from 'esbuild';

// Execute the actual browser entry point with only the DOM surface its
// connection flow needs. This covers wiring that a session helper test misses.
const app = buildSync({entryPoints: [fileURLToPath(new URL('../src/web/app.ts', import.meta.url))], bundle: true, write: false, platform: 'browser', format: 'iife'}).outputFiles[0]!.text;

function startBrowser(send: typeof fetch) {
	const values = new Map<string, string>();
	const storage = {getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); }};
	const elements = new Map<string, {textContent: string; innerHTML: string; className: string; disabled: boolean; hidden: boolean; events: Map<string, () => void>; addEventListener: (name: string, handler: () => void) => void}>();
	function element(id: string) {
		if (!elements.has(id)) {
			const events = new Map<string, () => void>();
			elements.set(id, {textContent: '', innerHTML: '', className: '', disabled: false, hidden: false, events, addEventListener: (name, handler) => { events.set(name, handler); }});
		}
		return elements.get(id)!;
	}
	const events = new Map<string, () => void>();
	const location = {hash: `#token=${'a'.repeat(64)}`, pathname: '/', search: ''};
	runInNewContext(app, {
		window: {sessionStorage: storage, addEventListener: (name: string, handler: () => void) => { events.set(name, handler); }},
		document: {getElementById: element, querySelectorAll: () => []},
		location, history: {replaceState: () => { location.hash = ''; }},
		fetch: send, AbortSignal, URLSearchParams, setTimeout,
	});
	return {element, values, location, connect: (token: string) => { location.hash = `#token=${token}`; events.get('hashchange')!(); }, refresh: () => element('refresh-btn').events.get('click')!()};
}

async function settled(): Promise<void> {
	await new Promise<void>(resolve => setImmediate(resolve));
	await new Promise<void>(resolve => setImmediate(resolve));
}

const status = () => Response.json({version: 1, request_id: 'browser-test', data: {type: 'status', frozen: true, active_runs: 0, event_count: 1, genome_count: 0}});

test('the browser reconnects on a fragment-only navigation after a real 401 response', async () => {
	const sent: string[] = []; let expired = false;
	const browser = startBrowser(async (_url, init) => {
		const token = (init!.headers as Record<string, string>)['x-hephaestus-web-token']!;
		sent.push(token);
		return expired && token === 'a'.repeat(64) ? new Response('', {status: 401}) : status();
	});
	await settled(); assert.equal(browser.location.hash, '');
	expired = true; browser.refresh(); await settled();
	assert.equal(browser.values.size, 0);
	assert.match(browser.element('view-status').innerHTML, /session has expired/);
	browser.connect('b'.repeat(64)); await settled();
	assert.equal(browser.location.hash, '');
	assert.equal(sent.at(-1), 'b'.repeat(64));
	assert.equal(browser.values.size, 1);
	assert.equal(browser.element('token-indicator').textContent, 'session token loaded');
	assert.match(browser.element('view-status').innerHTML, /Daemon Status/);
});

test('the actual browser wiring keeps its token on daemon authentication failure', async () => {
	let denied = false;
	const browser = startBrowser(async () => denied ? Response.json({version: 1, request_id: 'daemon-error', error: {code: 'unauthorized', message: 'daemon authentication failed'}}) : status());
	await settled(); denied = true; browser.refresh(); await settled();
	assert.equal(browser.values.size, 1);
	assert.equal(browser.element('token-indicator').textContent, 'session token loaded');
	assert.match(browser.element('view-status').innerHTML, /daemon authentication failed/);
});

test('an older in-flight 401 cannot erase a newly connected browser session', async () => {
	let finishOld: ((response: Response) => void) | undefined;
	const sent: string[] = [];
	const browser = startBrowser(async (_url, init) => {
		const token = (init!.headers as Record<string, string>)['x-hephaestus-web-token']!; sent.push(token);
		return token === 'a'.repeat(64) ? new Promise<Response>(resolve => { finishOld = resolve; }) : status();
	});
	browser.connect('b'.repeat(64)); await settled();
	finishOld!(new Response('', {status: 401})); await settled();
	assert.equal(browser.values.size, 1);
	assert.equal(browser.element('token-indicator').textContent, 'session token loaded');
	assert.match(browser.element('view-status').innerHTML, /Daemon Status/);
	assert.doesNotMatch(browser.element('view-status').innerHTML, /session has expired/);
	browser.refresh(); await settled();
	assert.equal(sent.at(-1), 'b'.repeat(64));
});
