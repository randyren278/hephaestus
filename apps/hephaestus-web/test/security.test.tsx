import assert from 'node:assert/strict';
import test from 'node:test';
import {generateSessionToken, hostIsAllowed, originIsAllowed, tokensMatch} from '../src/security.js';

test('generates distinct 256-bit hex session tokens', () => {
	const a = generateSessionToken();
	const b = generateSessionToken();
	assert.match(a, /^[0-9a-f]{64}$/);
	assert.notEqual(a, b);
});

test('tokensMatch requires an exact string match and rejects non-string input', () => {
	const token = generateSessionToken();
	assert.equal(tokensMatch(token, token), true);
	assert.equal(tokensMatch(token, `${token}x`), false);
	assert.equal(tokensMatch(token, token.slice(0, -1)), false);
	assert.equal(tokensMatch(token, undefined), false);
	assert.equal(tokensMatch(token, ['a', 'b']), false);
	assert.equal(tokensMatch(token, 12345), false);
});

test('hostIsAllowed accepts only loopback hostnames on the exact bound port', () => {
	assert.equal(hostIsAllowed('127.0.0.1:4100', 4100), true);
	assert.equal(hostIsAllowed('localhost:4100', 4100), true);
	assert.equal(hostIsAllowed('[::1]:4100', 4100), true);
	assert.equal(hostIsAllowed('127.0.0.1:9999', 4100), false, 'wrong port must be refused');
	assert.equal(hostIsAllowed('127.0.0.1', 4100), false, 'missing port must be refused');
	assert.equal(hostIsAllowed('evil.example:4100', 4100), false, 'a rebound hostname must be refused even if it resolves to loopback');
	assert.equal(hostIsAllowed(undefined, 4100), false);
});

test('originIsAllowed permits no Origin header but rejects a foreign or non-http Origin', () => {
	assert.equal(originIsAllowed(undefined, 4100), true);
	assert.equal(originIsAllowed('http://127.0.0.1:4100', 4100), true);
	assert.equal(originIsAllowed('http://localhost:4100', 4100), true);
	assert.equal(originIsAllowed('http://127.0.0.1:9999', 4100), false);
	assert.equal(originIsAllowed('https://127.0.0.1:4100', 4100), false, 'a non-http scheme must be refused');
	assert.equal(originIsAllowed('http://evil.example', 4100), false);
	assert.equal(originIsAllowed('not a url', 4100), false);
});
