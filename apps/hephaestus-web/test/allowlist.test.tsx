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
	assert.deepEqual(toAllowedCommand({command: 'drift_show', drift_id: 'drift-1'}), {command: 'drift_show', drift_id: 'drift-1'});
	assert.deepEqual(toAllowedCommand({command: 'drift_list', limit: 20}), {command: 'drift_list', limit: 20});
	assert.deepEqual(toAllowedCommand({command: 'drift_list'}), {command: 'drift_list', limit: 20});
	assert.deepEqual(toAllowedCommand({command: 'canary_show', canary_id: 'canary-1'}), {command: 'canary_show', canary_id: 'canary-1'});
	assert.deepEqual(toAllowedCommand({command: 'canary_list', limit: 20}), {command: 'canary_list', limit: 20});
	assert.deepEqual(toAllowedCommand({command: 'meta_strategy_show', strategy_id: 'strategy-1'}), {command: 'meta_strategy_show', strategy_id: 'strategy-1'});
	assert.deepEqual(toAllowedCommand({command: 'meta_strategy_list'}), {command: 'meta_strategy_list'});
	assert.deepEqual(toAllowedCommand({command: 'meta_show', meta_run_id: 'meta-1'}), {command: 'meta_show', meta_run_id: 'meta-1'});
	assert.deepEqual(toAllowedCommand({command: 'meta_list', limit: 20}), {command: 'meta_list', limit: 20});
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
	assert.equal(toAllowedCommand({command: 'drift_show'}), undefined);
	assert.equal(toAllowedCommand({command: 'canary_show', canary_id: ''}), undefined);
	assert.equal(toAllowedCommand({command: 'meta_strategy_show', strategy_id: 123}), undefined);
	assert.equal(toAllowedCommand({command: 'meta_show', meta_run_id: 'has space'}), undefined);
});

test('refuses drift, canary, and meta mutating commands even though their read siblings are allowed', () => {
	assert.equal(toAllowedCommand({command: 'drift_record', drift_id: 'd', world_id: 'w', kind: 'latency', evidence_evaluation_id: 'e'}), undefined);
	assert.equal(toAllowedCommand({command: 'canary_start', canary_id: 'c', world_id: 'w', candidate_genome_id: 'g', assessment_id: 'a'}), undefined);
	assert.equal(toAllowedCommand({command: 'canary_advance', canary_id: 'c', evidence_evaluation_id: 'e'}), undefined);
	assert.equal(toAllowedCommand({command: 'canary_live_check', canary_id: 'c', evidence_evaluation_id: 'e'}), undefined);
	assert.equal(toAllowedCommand({command: 'meta_strategy_register', path: '/tmp/strategy.json'}), undefined);
	assert.equal(toAllowedCommand({command: 'meta_evaluate', meta_run_id: 'm', strategy_a_id: 'a', strategy_b_id: 'b', lineages: [], confidence_bps: 9500, bootstrap_seed: 1}), undefined);
});

test('the allowlist and the refusal ledger partition the protocol command set with no overlap', () => {
	const overlap = READ_ONLY_COMMANDS.filter(command => (NOT_ALLOWED_COMMANDS as readonly string[]).includes(command));
	assert.deepEqual(overlap, []);
});
