import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {Box, renderToString} from 'ink';
import {parseResponse, type GenomeProfile} from '../src/protocol.js';
import {confirmationFits, reportedDollars, RevisionProfilePanel} from '../src/revision-view.js';

const genome = {genome_id: 'child', name: 'Revision', world_id: 'world', artifact_id: 'artifact', parent_ids: ['parent']};
const profile: GenomeProfile = {genome, world: {world_id: 'world', name: 'Tasks', artifact_id: 'world-artifact'}, provider: 'claude', family: 'sonnet',
	workspace_write: false, network: false, prompt_artifact_id: 'prompt', harness_mutation_allowed: true, output_scoring: 'exact', visible_tasks: 1, sealed_tasks: 1,
	paired_trial_wall_millis: 300000, paired_trial_output_bytes: 1048576, paired_total_wall_millis: 1210000, reported_cost_limit_microusd: '1000000'};
const metrics = {metrics_eligible: true, estimate_bps: 10000, lower_bps: 10000, upper_bps: 10000, parent_cost_microusd: 0, candidate_cost_microusd: 0,
	parent_latency_millis: 4, candidate_latency_millis: 1, invariant_gate_verified: false, promotion_eligible: false};
const selection = {evaluation_id: 'evaluation', world_id: 'world', receipt: {...metrics, evaluation_id: 'evaluation', world_id: 'world', parent_genome_id: 'parent', candidate_genome_id: 'child', evaluation_event_id: 'evaluated', evaluation_event_hash: 'hash-evaluation'}, event: {event_id: 'selected', event_hash: 'hash-selection'}};
const revision = {payload: {schema_version: 2, proposal_id: 'proposal', selection_event_id: 'source', selection_event_hash: 'hash-source', evaluation_id: 'old-evaluation',
	world_id: 'world', parent_genome_id: 'parent', child: genome, hypothesis: 'One specific change.', artifact_name: 'agent.prompt', prompt_artifact_before: 'before', prompt_artifact_after: 'after'},
	event: {event_id: 'proposed', event_hash: 'hash-proposal'}, promotion_eligible: false};
const assessment = {payload: {schema_version: 1, assessment_id: 'assessment', proposal_id: 'proposal', proposal_event_id: 'proposed', proposal_event_hash: 'hash-proposal',
	selection_event_id: 'selected', selection_event_hash: 'hash-selection', evaluation_id: 'evaluation', evaluation_event_id: 'evaluated', evaluation_event_hash: 'hash-evaluation',
	world_id: 'world', parent_genome_id: 'parent', child_genome_id: 'child', outcome: 'metrics_passed', promotion_eligible: false, invariant_gate_verified: false}, event: {event_id: 'assessed'}};
function parse(data: unknown) { return parseResponse(JSON.stringify({version: 1, request_id: 'r', data}), 'r').data; }

test('execution profiles keep precise decimal limits and reject malformed settings', () => {
	assert.deepEqual(parse({type: 'genome_profile', profile}), {type: 'genome_profile', profile});
	assert.equal((parse({type: 'genome_profile', profile: {...profile, reported_cost_limit_microusd: '18446744073709551615'}}) as {profile: GenomeProfile}).profile.reported_cost_limit_microusd, '18446744073709551615');
	for (const change of [{reported_cost_limit_microusd: 1000000}, {reported_cost_limit_microusd: '18446744073709551616'}, {reported_cost_limit_microusd: '01'}, {network: 'false'}, {paired_trial_wall_millis: 0}, {output_scoring: 'loose'}, {world: {...profile.world, world_id: 'foreign'}}]) {
		assert.throws(() => parse({type: 'genome_profile', profile: {...profile, ...change}}), /invalid|unsupported/);
	}
});

test('selection, revision and assessment responses retain their exact event hash chains', () => {
	const selected = parse({type: 'selection', selection}); assert.equal(selected?.type, 'selection');
	if (selected?.type === 'selection') assert.equal(selected.selection.evaluation_event_hash, 'hash-evaluation');
	const proposed = parse({type: 'forge_revision', revision}); assert.equal(proposed?.type, 'forge_revision');
	if (proposed?.type === 'forge_revision') { assert.equal(proposed.revision.event_hash, 'hash-proposal'); assert.equal(proposed.revision.selection_event_hash, 'hash-source'); }
	const assessed = parse({type: 'forge_assessment', assessment}); assert.equal(assessed?.type, 'forge_assessment');
	if (assessed?.type === 'forge_assessment') { assert.equal(assessed.assessment.proposal_event_id, 'proposed'); assert.equal(assessed.assessment.proposal_event_hash, 'hash-proposal'); assert.equal(assessed.assessment.selection_event_hash, 'hash-selection'); assert.equal(assessed.assessment.evaluation_event_hash, 'hash-evaluation'); }
	assert.throws(() => parse({type: 'selection', selection: {...selection, world_id: 'foreign'}}));
	assert.throws(() => parse({type: 'forge_revision', revision: {...revision, promotion_eligible: true}}));
	assert.throws(() => parse({type: 'forge_revision', revision: {...revision, payload: {...revision.payload, child: {...genome, parent_ids: []}}}}));
	for (const field of ['proposal_event_hash', 'selection_event_hash', 'evaluation_event_hash']) assert.throws(() => parse({type: 'forge_assessment', assessment: {...assessment, payload: {...assessment.payload, [field]: null}}}));
	assert.throws(() => parse({type: 'forge_assessment', assessment: {...assessment, payload: {...assessment.payload, invariant_gate_verified: true}}}));
});

test('explicit rejection markers are preserved only on the compatible invalid_request category', () => {
	const reply = (error: unknown) => parseResponse(JSON.stringify({version: 1, request_id: 'r', error}), 'r');
	assert.equal(reply({code: 'invalid_request', message: 'Permanent admission refusal', rejected: true}).error!.rejected, true);
	assert.equal(reply({code: 'invalid_request', message: 'evolution is frozen'}).error!.rejected, undefined);
	for (const value of [false, 'true', 1, null]) assert.throws(() => reply({code: 'invalid_request', message: 'No', rejected: value}));
	assert.throws(() => reply({code: 'busy', message: 'Active job', rejected: true}));
});

test('comparison confirmation keeps full model and limits visible and separates reported billing', () => {
	const frame = renderToString(<Box width={78} flexDirection="column"><RevisionProfilePanel profile={profile} /></Box>);
	assert.match(frame, /Model: claude \/ sonnet/); assert.match(frame, /Each trial: 300s/);
	assert.match(frame, /Total reported limit: \$4\.000000/);
	assert.match(frame, /Provider billing and subscription quotas are outside/);
	assert.equal(frame.split('\n').length, 10);
	assert.ok(confirmationFits(profile, 80, 22));
	assert.ok(!confirmationFits(profile, 79, 24)); assert.ok(!confirmationFits(profile, 80, 21));
	assert.ok(!confirmationFits({...profile, family: 'a'.repeat(200)}, 80, 24));
	for (const family of ['模'.repeat(32), 'x🙂', 'sonnet ', 'sonnet\n', 'x\u200by', 'x\u202ey']) assert.ok(!confirmationFits({...profile, family}, 80, 24), `Hidden or rewritten model must not be confirmed: ${JSON.stringify(family)}`);
	assert.equal(reportedDollars('18446744073709551615'), '$18446744073709.551615');
});
