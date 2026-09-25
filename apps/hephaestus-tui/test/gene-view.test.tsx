import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {renderToString} from 'ink';
import {GeneDetailPanel, GeneListPanel} from '../src/gene-view.js';
import type {Gene, GeneAggregate, GeneSummary} from '../src/protocol.js';

const gene: Gene = {
	gene_id: 'gene-1', promotion_transition_id: 'transition-1', world_id: 'world-1',
	origin_parent_genome_id: 'genome-parent', origin_child_genome_id: 'genome-child',
	operation_before: 'before', operation_after: 'after',
	evidence_trials: 5, evidence_threshold: 3, event_id: 'event-1', sequence: 1,
};
const summary: GeneSummary = {gene, lineages: 2, positive: 2, neutral: 0, negative: 0, contradiction: false, species_ids: []};
const contested: GeneSummary = {
	gene: {...gene, gene_id: 'gene-2'}, lineages: 3, positive: 1, neutral: 0, negative: 1, contradiction: true, species_ids: ['species-1'],
};

test('GeneListPanel renders each Gene with its transfer tally and flags contradictions', () => {
	const output = renderToString(<GeneListPanel genes={[summary, contested]} selected={0} height={10} />);
	assert.match(output, /GENE BANK/);
	assert.match(output, /2 lineages/);
	assert.match(output, /CONTRADICTION/);
	assert.match(output, /species/);
});

test('GeneListPanel reports an empty Gene Bank explicitly', () => {
	const output = renderToString(<GeneListPanel genes={[]} selected={0} height={10} />);
	assert.match(output, /No Genes extracted yet/);
});

test('GeneDetailPanel shows a loading state before the aggregate resolves', () => {
	const output = renderToString(<GeneDetailPanel aggregate={undefined} height={10} />);
	assert.match(output, /Loading/);
});

test('GeneDetailPanel renders origin, transfer trials, contradiction, and speciation', () => {
	const aggregate: GeneAggregate = {
		gene,
		transfers: [{
			trial_id: 'trial-1', gene_id: 'gene-1', to_genome_id: 'genome-x', world_id: 'world-2', child: {
				genome_id: 'genome-x', name: 'candidate', world_id: 'world-2', artifact_id: 'artifact-1', parent_ids: [],
			},
			applied_event_id: 'event-2', applied_sequence: 2,
			evaluation_id: 'eval-1', outcome: 'positive', estimate_bps: 120, lower_bps: 10, upper_bps: 230,
			recorded_event_id: 'event-3', recorded_sequence: 3,
		}],
		contradiction: null,
		species: [{
			species_id: 'species-1', gene_id: 'gene-1', domain_world_id: 'world-2',
			lineage_genome_ids: ['genome-x'], average_estimate_bps: 120, minimum_lineages: 3, minimum_effect_bps: 50,
			event_id: 'event-4', sequence: 4,
		}],
	};
	const output = renderToString(<GeneDetailPanel aggregate={aggregate} height={10} />);
	assert.match(output, /genome-par/);
	assert.match(output, /5\/3 trials/);
	assert.match(output, /positive/);
	assert.match(output, /120bps/);
	assert.match(output, /SPECIATED/);
});
