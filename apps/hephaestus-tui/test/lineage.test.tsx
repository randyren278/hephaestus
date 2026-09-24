import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {renderToString} from 'ink';
import {lineDiff, lineageRows, roleOf} from '../src/lineage.js';
import {GenomeDetail, LineagePanel, WorldList, windowed} from '../src/lineage-view.js';
import {parseResponse, type Champion, type Genome} from '../src/protocol.js';

const id = (label: string) => `hephaestus:genome:${label.padEnd(64, '0')}`;
const world = 'hephaestus:world:' + 'w'.repeat(64);
const genome = (label: string, parents: string[] = [], worldId = world): Genome => ({
	genome_id: id(label), name: label, world_id: worldId, artifact_id: 'a'.repeat(64), parent_ids: parents.map(id),
});
const genomes = [
	genome('root'),
	genome('left', ['root']),
	genome('right', ['root']),
	genome('merge', ['left', 'right']),
	genome('grandchild', ['left']),
	genome('elsewhere', [], 'hephaestus:world:other'),
];
const champion: Champion = {
	world_id: world, champion_genome_id: id('left'), standby_genome_ids: [id('root')],
	quarantined_genome_ids: [id('right')], transitions: [],
};

test('lineage rows draw each World Genome once under its first registered parent', () => {
	const rows = lineageRows(genomes, world, champion);
	assert.deepEqual(rows.map(row => `${row.prefix}${row.name}`), [
		'root',
		'├─ left',
		'│  ├─ grandchild',
		'│  └─ merge',
		'└─ right',
	]);
	assert.equal(rows.find(row => row.name === 'merge')?.extra_parents, 1);
	assert.deepEqual(rows.map(row => row.role), ['standby', 'champion', null, null, 'quarantined']);
	assert.equal(rows.some(row => row.name === 'elsewhere'), false, 'other Worlds stay separated');
});

test('lineage keeps Genomes whose parents are outside the World as roots', () => {
	const rows = lineageRows([genome('orphan', ['missing']), genome('solo')], world);
	assert.deepEqual(rows.map(row => [row.prefix, row.name, row.role]), [['', 'orphan', null], ['', 'solo', null]]);
	assert.equal(roleOf(id('solo'), undefined), null);
});

test('line diff reports removed and added lines around shared context', () => {
	assert.deepEqual(lineDiff('a\nb\nc', 'a\nB\nc\nd'), [
		{kind: 'same', text: 'a'},
		{kind: 'remove', text: 'b'},
		{kind: 'add', text: 'B'},
		{kind: 'same', text: 'c'},
		{kind: 'add', text: 'd'},
	]);
	assert.deepEqual(lineDiff('same', 'same'), [{kind: 'same', text: 'same'}]);
	assert.equal(lineDiff('x\n'.repeat(1000), '').length <= 401, true, 'diff input is bounded');
});

test('windowing keeps the selection visible', () => {
	const items = Array.from({length: 10}, (_, index) => index);
	assert.deepEqual(windowed(items, 0, 3), {items: [0, 1, 2], offset: 0});
	assert.deepEqual(windowed(items, 5, 3), {items: [4, 5, 6], offset: 4});
	assert.deepEqual(windowed(items, 9, 3), {items: [7, 8, 9], offset: 7});
});

test('lineage panels render roles, prompt diffs, and unavailable prompts', () => {
	const rows = lineageRows(genomes, world, champion);
	const lineage = renderToString(<LineagePanel world={{world_id: world, name: 'quickstart', artifact_id: 'a'.repeat(64)}} rows={rows} champion={champion} selected={1} height={10} />, {columns: 100});
	assert.match(lineage, /LINEAGE \/ quickstart/);
	assert.match(lineage, /▲ left/);
	assert.match(lineage, /CHAMPION/);
	assert.match(lineage, /quarantined/);
	assert.match(lineage, /\+1 parents/);
	const worlds = renderToString(<WorldList worlds={[]} selected={0} height={5} />);
	assert.match(worlds, /No registered Worlds/);
	const detail = renderToString(<GenomeDetail genome={genomes[1]!} parent={genomes[0]} role="champion"
		prompt={'keep\nnew line'} parentPrompt={'keep\nold line'} height={10} />, {columns: 100});
	assert.match(detail, /\+ new line/);
	assert.match(detail, /- old line/);
	assert.match(detail, /1 added · 1 removed/);
	const missing = renderToString(<GenomeDetail genome={genomes[0]!} parent={undefined} role={null} prompt={null} parentPrompt="" height={5} />);
	assert.match(missing, /No verified Markdown prompt/);
});

test('lineage responses are parsed strictly', () => {
	const wrap = (data: unknown) => JSON.stringify({version: 1, request_id: 'r', data});
	const genomesResponse = parseResponse(wrap({type: 'genomes', genomes: [genomes[0]]}), 'r');
	assert.deepEqual(genomesResponse.data, {type: 'genomes', genomes: [genomes[0]]});
	assert.throws(() => parseResponse(wrap({type: 'genomes', genomes: [{...genomes[0], parent_ids: 'root'}]}), 'r'), /variant is invalid/);
	assert.deepEqual(parseResponse(wrap({type: 'worlds', worlds: [{world_id: world, name: 'w', artifact_id: 'a'}]}), 'r').data,
		{type: 'worlds', worlds: [{world_id: world, name: 'w', artifact_id: 'a'}]});
	assert.throws(() => parseResponse(wrap({type: 'worlds', worlds: [{world_id: world}]}), 'r'), /variant is invalid/);
	assert.deepEqual(parseResponse(wrap({type: 'genome_prompt', genome_id: id('root'), prompt: 'body'}), 'r').data,
		{type: 'genome_prompt', genome_id: id('root'), prompt: 'body'});
	const transition = {
		payload: {
			schema_version: 1, transition_id: 'rollback-1', world_id: world, kind: 'rolled_back', champion_genome_id: id('root'),
			previous_champion_genome_id: id('left'), previous_transition_event_id: 'champion:p:recorded',
			previous_transition_event_hash: 'f'.repeat(64), promotion: null, reason: 'Injected regression',
		},
		event: {sequence: 9, event_id: 'champion:rollback-1:recorded', aggregate_id: `champion:${world}`, event_hash: 'e'.repeat(64)},
	};
	const parsedTransition = parseResponse(wrap({type: 'champion_transition', transition}), 'r').data;
	assert.deepEqual(parsedTransition, {type: 'champion_transition', transition: {
		transition_id: 'rollback-1', world_id: world, kind: 'rolled_back', champion_genome_id: id('root'),
		previous_champion_genome_id: id('left'), reason: 'Injected regression', event_id: 'champion:rollback-1:recorded', sequence: 9,
	}});
	const shown = parseResponse(wrap({type: 'champion', champion: {
		world_id: world, champion_genome_id: null, standby_genome_ids: [], quarantined_genome_ids: [], transitions: [transition],
	}}), 'r').data;
	assert.equal(shown?.type === 'champion' && shown.champion.transitions[0]?.kind, 'rolled_back');
	assert.throws(() => parseResponse(wrap({type: 'champion_transition', transition: {...transition, payload: {...transition.payload, kind: 'crowned'}}}), 'r'), /variant is invalid/);
	assert.throws(() => parseResponse(wrap({type: 'champion', champion: {world_id: world, champion_genome_id: null, standby_genome_ids: [], quarantined_genome_ids: [], transitions: [{}]}}), 'r'), /variant is invalid/);
});
