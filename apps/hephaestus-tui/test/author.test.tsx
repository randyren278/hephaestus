import test from 'node:test';
import assert from 'node:assert/strict';
import {access, mkdtemp, readFile, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {defaultAgentPath, editorCommand, ensureAgentSource, expandPath, launchEditor, markdownAgentTemplate, slugify} from '../src/author.js';

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
		assert.match(first, /name: my-world/);
		assert.match(first, /```hephaestus-reference-v1\n\{"schema_version":1,"operation":"identity"\}\n```\n$/);
		// A second call must not clobber operator edits.
		await writeFile(path, 'edited content\n');
		ensureAgentSource(path, 'My World');
		const second = await readFile(path, 'utf8');
		assert.equal(second, 'edited content\n');
	} finally {
		await rm(workspace, {recursive: true, force: true});
	}
});

test('markdownAgentTemplate body is exactly the strict fenced reference instruction the deterministic runtime consumes (no surrounding prose)', () => {
	const source = markdownAgentTemplate('My World');
	const closing = source.indexOf('\n---\n');
	const body = source.slice(closing + '\n---\n'.length);
	assert.equal(body, '```hephaestus-reference-v1\n{"schema_version":1,"operation":"identity"}\n```\n');
});

test('ensureAgentSource refuses a relative path', () => {
	assert.throws(() => ensureAgentSource('relative/path.md', 'World'), /must be absolute/);
});

test('launchEditor accepts flags and shell quoting while passing metacharacters in the path literally', async () => {
	const workspace = await mkdtemp(join(tmpdir(), 'hephaestus-editor-'));
	try {
		const editor = join(workspace, 'fake editor.sh');
		const report = join(workspace, 'arguments.txt');
		const injected = join(workspace, 'injected');
		const path = join(workspace, `agent $(touch '${injected}') \`touch '${injected}'\` "quotes".md`);
		await writeFile(editor, '#!/bin/sh\nreport="$1"\nshift\nprintf \'%s\\n\' "$@" > "$report"\n');
		// The report path and command are trusted settings; the agent path is not shell text.
		await launchEditor(path, `sh '${editor}' '${report}' --wait 'two words'`);
		assert.equal(await readFile(report, 'utf8'), `--wait\ntwo words\n${path}\n`);
		await assert.rejects(access(injected), {code: 'ENOENT'});
	} finally {
		await rm(workspace, {recursive: true, force: true});
	}
});

test('launchEditor reports nonzero status, a missing command and interruption as failures', async () => {
	await assert.rejects(launchEditor('/tmp/agent.md', 'false'), /Editor failed \(status 1\)/);
	await assert.rejects(launchEditor('/tmp/agent.md', '/definitely-missing-hephaestus-editor'), /Editor failed \(status 127\)/);
	await assert.rejects(launchEditor('/tmp/agent.md', 'kill -TERM $$; true'), /Editor interrupted \(SIGTERM\)/);
});
