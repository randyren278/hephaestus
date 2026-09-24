import assert from 'node:assert/strict';
import test from 'node:test';
import {NOT_ALLOWED_COMMANDS, READ_ONLY_COMMANDS, toAllowedCommand} from '../src/allowlist.js';

test('every read-only command builds a strictly typed command with valid identifiers', () => {
	assert.deepEqual(toAllowedCommand({command: 'status'}), {command: 'status'});
	assert.deepEqual(toAllowedCommand({command: 'world_list'}), {command: 'world_list'});
	assert.deepEqual(toAllowedCommand({command: 'genome_list'}), {command: 'genome_list'});
	assert.deepEqual(toAllowedCommand({command: 'genome_show', genome_id: 'genome:abc123'}), {command: 'genome_show', genome_id: 'genome:abc123'});
	assert.deepEqual(toAllowedCommand({command: 'genome_prompt', genome_id: 'genome:abc123'}), {command: 'genome_prompt', genome_id: 'genome:abc123'});
	assert.deepEqual(toAllowedCommand({command: 'champion_show', world_id: 'world:abc123'}), {command: 'champion_show', world_id: 'world:abc123'});
	assert.deepEqual(toAllowedCommand({command: 'job_status', job_id: 'job-1'}), {command: 'job_status', job_id: 'job-1'});
});

test('refuses every mutating or unexposed daemon command', () => {
	for (const command of NOT_ALLOWED_COMMANDS) {
		assert.equal(toAllowedCommand({command}), undefined, `${command} must be refused`);
		assert.equal(
			toAllowedCommand({command, genome_id: 'g', world_id: 'w', job_id: 'j', transition_id: 't', reason: 'r'}),
			undefined,
			`${command} must be refused even with plausible-looking parameters`,
		);
	}
});

test('refuses unknown, malformed, and injected command shapes', () => {
	assert.equal(toAllowedCommand(undefined), undefined);
	assert.equal(toAllowedCommand(null), undefined);
	assert.equal(toAllowedCommand('status'), undefined);
	assert.equal(toAllowedCommand(['status']), undefined);
	assert.equal(toAllowedCommand({}), undefined);
	assert.equal(toAllowedCommand({command: 'daemon_stop'}), undefined);
	assert.equal(toAllowedCommand({command: 'kill_all'}), undefined);
	assert.equal(toAllowedCommand({command: 'champion_rollback', transition_id: 't', world_id: 'w', reason: 'r'}), undefined);
	// A command outside the entire protocol union must be refused, not merely one outside the allowlist.
	assert.equal(toAllowedCommand({command: 'drop_table_worlds'}), undefined);
});

test('refuses an allowed command tag whose required identifier is missing or malformed', () => {
	assert.equal(toAllowedCommand({command: 'genome_show'}), undefined);
	assert.equal(toAllowedCommand({command: 'genome_show', genome_id: ''}), undefined);
	assert.equal(toAllowedCommand({command: 'genome_show', genome_id: 123}), undefined);
	assert.equal(toAllowedCommand({command: 'genome_show', genome_id: 'a'.repeat(257)}), undefined);
	assert.equal(toAllowedCommand({command: 'genome_show', genome_id: 'has space'}), undefined);
	assert.equal(toAllowedCommand({command: 'champion_show'}), undefined);
	assert.equal(toAllowedCommand({command: 'job_status', job_id: null}), undefined);
});

test('the allowlist and the refusal ledger partition the protocol command set with no overlap', () => {
	const overlap = READ_ONLY_COMMANDS.filter(command => (NOT_ALLOWED_COMMANDS as readonly string[]).includes(command));
	assert.deepEqual(overlap, []);
});
