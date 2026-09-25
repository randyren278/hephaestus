import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp, readFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {defaultAgentPath, editorCommand, ensureAgentSource, expandPath, slugify} from '../src/author.js';

test('slugify lowercases, collapses separators, and trims edges', () => {
	assert.equal(slugify('Coding World v2!'), 'coding-world-v2');
	assert.equal(slugify('  ---  '), 'agent');
});

test('defaultAgentPath joins the workspace and a slugified World name with a .md extension', () => {
	assert.equal(defaultAgentPath('Coding World', '/tmp/agents'), '/tmp/agents/coding-world.md');
});

test('expandPath resolves a leading tilde against the home directory and leaves other paths unchanged', () => {
	assert.equal(expandPath('~/agents/x.md'), join(process.env['HOME'] ?? '', 'agents/x.md'));
	assert.equal(expandPath('/absolute/path.md'), '/absolute/path.md');
	assert.equal(expandPath('  /padded.md  '), '/padded.md');
});

test('editorCommand prefers VISUAL, falls back to EDITOR, then vi', () => {
	const original = {visual: process.env['VISUAL'], editor: process.env['EDITOR']};
	try {
		process.env['VISUAL'] = 'my-visual';
		process.env['EDITOR'] = 'my-editor';
		assert.equal(editorCommand(), 'my-visual');
		delete process.env['VISUAL'];
		assert.equal(editorCommand(), 'my-editor');
		delete process.env['EDITOR'];
		assert.equal(editorCommand(), 'vi');
	} finally {
		if (original.visual === undefined) delete process.env['VISUAL']; else process.env['VISUAL'] = original.visual;
		if (original.editor === undefined) delete process.env['EDITOR']; else process.env['EDITOR'] = original.editor;
	}
});

test('ensureAgentSource creates the workspace directory and a starter template only when the file is missing', async () => {
	const workspace = await mkdtemp(join(tmpdir(), 'hephaestus-agents-'));
	try {
		const path = join(workspace, 'nested', 'my-world.md');
		const resolved = ensureAgentSource(path, 'My World');
		assert.equal(resolved, path);
		const first = await readFile(path, 'utf8');
		assert.match(first, /My World/);
		// A second call must not clobber operator edits.
		const {writeFile} = await import('node:fs/promises');
		await writeFile(path, 'edited content\n');
		ensureAgentSource(path, 'My World');
		const second = await readFile(path, 'utf8');
		assert.equal(second, 'edited content\n');
	} finally {
		await rm(workspace, {recursive: true, force: true});
	}
});

test('ensureAgentSource refuses a relative path', () => {
	assert.throws(() => ensureAgentSource('relative/path.md', 'World'), /must be absolute/);
});
