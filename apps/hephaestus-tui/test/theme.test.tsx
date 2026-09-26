import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {renderToString} from 'ink';
import {Text} from 'ink';
import {buildTheme, colorFor, PALETTE, resolveColorLevel, useTheme} from '../src/theme.js';

test('resolveColorLevel reports truecolor when COLORTERM says so on a TTY', () => {
	assert.equal(resolveColorLevel({COLORTERM: 'truecolor'}, true), 'truecolor');
	assert.equal(resolveColorLevel({COLORTERM: '24bit'}, true), 'truecolor');
	assert.equal(resolveColorLevel({TERM: 'xterm-direct'}, true), 'truecolor');
});

test('resolveColorLevel falls back to the 16-color basic level on a plain TTY', () => {
	assert.equal(resolveColorLevel({TERM: 'xterm-256color'}, true), 'basic');
	assert.equal(resolveColorLevel({TERM: 'screen'}, true), 'basic');
});

test('resolveColorLevel treats an unset or dumb TERM as no color, even on a TTY', () => {
	assert.equal(resolveColorLevel({}, true), 'none');
	assert.equal(resolveColorLevel({TERM: 'dumb'}, true), 'none');
});

test('resolveColorLevel honors NO_COLOR over everything else', () => {
	assert.equal(resolveColorLevel({NO_COLOR: '1', COLORTERM: 'truecolor'}, true), 'none');
	assert.equal(resolveColorLevel({NO_COLOR: '', COLORTERM: 'truecolor'}, true), 'truecolor', 'an empty NO_COLOR is unset per the no-color.org spec');
});

test('resolveColorLevel lets an explicit FORCE_COLOR override NO_COLOR', () => {
	assert.equal(resolveColorLevel({NO_COLOR: '1', FORCE_COLOR: '3'}, true), 'truecolor');
	assert.equal(resolveColorLevel({NO_COLOR: '1', FORCE_COLOR: '1'}, true), 'basic');
	assert.equal(resolveColorLevel({FORCE_COLOR: '0', COLORTERM: 'truecolor'}, true), 'none');
});

test('resolveColorLevel degrades to plain text when the process is not attached to a TTY', () => {
	assert.equal(resolveColorLevel({COLORTERM: 'truecolor'}, false), 'none');
	assert.equal(resolveColorLevel({TERM: 'xterm-256color'}, false), 'none');
});

test('colorFor resolves hero hex values at truecolor and the 16-color fallback at basic, and nothing at none', () => {
	assert.equal(colorFor('champion', 'truecolor'), PALETTE.ember);
	assert.equal(colorFor('championBright', 'truecolor'), PALETTE.emberBright);
	assert.equal(colorFor('judge', 'truecolor'), PALETTE.gold);
	assert.equal(colorFor('danger', 'truecolor'), PALETTE.danger);
	assert.equal(colorFor('champion', 'basic'), 'red');
	assert.equal(colorFor('judge', 'basic'), 'yellow');
	assert.equal(colorFor('champion', 'none'), undefined);
	assert.equal(colorFor('judge', 'none'), undefined);
});

test('buildTheme reports enabled=false only at level none, and exposes the shared glyphs', () => {
	assert.equal(buildTheme('truecolor').enabled, true);
	assert.equal(buildTheme('basic').enabled, true);
	assert.equal(buildTheme('none').enabled, false);
	assert.equal(buildTheme('truecolor').glyphs.championCrest, '▲');
});

test('useTheme wires resolveColorLevel and colorFor together for a component', () => {
	function Probe() {
		const theme = useTheme({env: {COLORTERM: 'truecolor'}, isTTY: true});
		const color = theme.color('champion');
		return <Text {...(color !== undefined ? {color} : {})}>{theme.level}</Text>;
	}
	const output = renderToString(<Probe />);
	assert.match(output, /truecolor/);
});

test('useTheme renders plain text (no crash, no color) when NO_COLOR is set', () => {
	function Probe() {
		const theme = useTheme({env: {NO_COLOR: '1', COLORTERM: 'truecolor'}, isTTY: true});
		const color = theme.color('champion');
		return <Text {...(color !== undefined ? {color} : {})}>{theme.enabled ? 'colored' : 'plain'}</Text>;
	}
	const output = renderToString(<Probe />);
	assert.match(output, /plain/);
});
