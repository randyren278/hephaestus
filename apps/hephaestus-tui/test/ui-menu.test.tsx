import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {renderToString} from 'ink';
import {App} from '../src/ui.js';
import type {ApiResponse} from '../src/protocol.js';

const status: ApiResponse = {version: 1, request_id: 'r', data: {type: 'status', frozen: false, active_runs: 0, event_count: 0, genome_count: 0}};

test('the home menu (including Evidence & Costs and Author Markdown agent) fits an 80x24 terminal without overlap', () => {
	const rendered = renderToString(<App client={{request: async () => status}} pollMs={1000} />);
	assert.match(rendered, /Evidence & Costs/);
	assert.match(rendered, /Gene Bank/);
	assert.match(rendered, /Author Markdown agent/);
	const lines = rendered.split('\n');
	assert.ok(lines.length <= 24, `expected the home frame to fit 24 rows, got ${lines.length}`);
	// A regression that overflows the fixed-height root Box overlaps text on
	// one row instead of wrapping, which shows up as two menu labels glued
	// together on a single line.
	for (const line of lines) assert.ok(!/‹[A-Za-z]/.test(line), `menu row overlapped another row: ${line}`);
});
