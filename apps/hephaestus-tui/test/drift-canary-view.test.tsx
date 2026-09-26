import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {renderToString} from 'ink';
import {CanaryDetailPanel, CanaryListPanel, DriftDetailPanel, DriftListPanel} from '../src/drift-canary-view.js';
import type {Canary, Drift} from '../src/protocol.js';

const drift: Drift = {
	drift_id: 'drift-1', world_id: 'world-1', kind: 'latency', evidence_evaluation_id: 'eval-1',
	selection_event_id: 'event-sel', baseline_genome_id: 'genome-base', shifted_genome_id: 'genome-shift',
	threshold_bps: 500, observed_delta_bps: 750, event_id: 'event-1', sequence: 1,
};

test('DriftListPanel renders each drift record with its kind and observed delta', () => {
	const output = renderToString(<DriftListPanel drifts={[drift]} selected={0} height={10} />);
	assert.match(output, /DRIFT RECORDS/);
	assert.match(output, /latency/);
	assert.match(output, /750bps/);
});

test('DriftListPanel reports no drift explicitly', () => {
	const output = renderToString(<DriftListPanel drifts={[]} selected={0} height={10} />);
	assert.match(output, /No drift recorded yet/);
});

test('DriftDetailPanel shows a loading state before the record resolves', () => {
	const output = renderToString(<DriftDetailPanel drift={undefined} height={10} />);
	assert.match(output, /Loading/);
});

test('DriftDetailPanel renders baseline, shifted Genome, and threshold', () => {
	const output = renderToString(<DriftDetailPanel drift={drift} height={10} />);
	assert.match(output, /genome-bas/);
	assert.match(output, /genome-shi/);
	assert.match(output, /threshold 500bps/);
	assert.match(output, /never directly replaces a Champion/);
});

const canary: Canary = {
	canary_id: 'canary-1', world_id: 'world-1', candidate_genome_id: 'genome-candidate', previous_champion_genome_id: 'genome-champion',
	stage: 'stage25',
	transitions: [
		{
			canary_id: 'canary-1', world_id: 'world-1', kind: 'started', stage: 'pending',
			candidate_genome_id: 'genome-candidate', previous_champion_genome_id: 'genome-champion',
			assessment_id: 'assessment-1', evidence: null, reason: null, event_id: 'event-a', sequence: 1,
		},
		{
			canary_id: 'canary-1', world_id: 'world-1', kind: 'advanced', stage: 'stage25',
			candidate_genome_id: 'genome-candidate', previous_champion_genome_id: 'genome-champion',
			assessment_id: 'assessment-1',
			evidence: {
				evidence_evaluation_id: 'eval-2', selection_event_id: 'event-sel-2',
				latency_delta_bps: 10, cost_delta_bps: 5, correctness_delta_bps: 0, reliability_delta_bps: 0, regressed: false,
			},
			reason: null, event_id: 'event-b', sequence: 2,
		},
	],
};

test('CanaryListPanel renders each canary with its stage and transition count', () => {
	const output = renderToString(<CanaryListPanel canaries={[canary]} selected={0} height={10} animate={false} />);
	assert.match(output, /CANARIES/);
	assert.match(output, /STAGE25/);
	assert.match(output, /2 transitions/);
});

test('CanaryListPanel reports no canaries explicitly', () => {
	const output = renderToString(<CanaryListPanel canaries={[]} selected={0} height={10} animate={false} />);
	assert.match(output, /No canaries started yet/);
});

test('CanaryDetailPanel shows a loading state before the projection resolves', () => {
	const output = renderToString(<CanaryDetailPanel canary={undefined} height={10} animate={false} />);
	assert.match(output, /Loading/);
});

test('CanaryDetailPanel renders candidate, stage, and transition evidence health', () => {
	const output = renderToString(<CanaryDetailPanel canary={canary} height={10} animate={false} />);
	assert.match(output, /genome-can/);
	assert.match(output, /STAGE25/);
	assert.match(output, /started/);
	assert.match(output, /advanced/);
	assert.match(output, /healthy/);
});
