import React from 'react';
import {Box, Text} from 'ink';
import {windowed, shortId} from './lineage-view.js';
import {safeText, type GeneAggregate, type GeneSummary} from './protocol.js';

export function GeneListPanel({genes, selected, height}: {genes: GeneSummary[]; selected: number; height: number}) {
	const view = windowed(genes, selected, height);
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">GENE BANK</Text>
		{genes.length === 0 && <Text color="gray">No Genes extracted yet.</Text>}
		{view.items.map((summary, index) => {
			const active = view.offset + index === selected;
			return <Text key={summary.gene.gene_id} wrap="truncate" color={active ? 'yellow' : summary.contradiction ? 'red' : 'white'}>
				{active ? '› ' : '  '}{shortId(summary.gene.gene_id)} <Text color="gray">· {summary.lineages} lineages</Text>{' '}
				<Text color="green">+{summary.positive}</Text> <Text color="gray">~{summary.neutral}</Text> <Text color="red">-{summary.negative}</Text>
				{summary.contradiction && <Text color="red"> CONTRADICTION</Text>}
				{summary.species_ids.length > 0 && <Text color="gold">{' '}{summary.species_ids.length} species</Text>}
			</Text>;
		})}
	</Box>;
}

export function GeneDetailPanel({aggregate, height}: {aggregate: GeneAggregate | undefined; height: number}) {
	if (!aggregate) return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">GENE</Text>
		<Text color="gray">Loading…</Text>
	</Box>;
	const {gene, transfers, contradiction, species} = aggregate;
	const shown = transfers.slice(0, Math.max(1, height));
	return <Box flexDirection="column" borderStyle="single" borderColor="gray" paddingX={1}>
		<Text bold color="yellow">GENE / {shortId(gene.gene_id)}</Text>
		<Text wrap="truncate">Origin  {shortId(gene.origin_parent_genome_id)} → {shortId(gene.origin_child_genome_id)}</Text>
		<Text wrap="truncate">Evidence {gene.evidence_trials}/{gene.evidence_threshold} trials at extraction</Text>
		<Text color="gray">TRANSFERS ({transfers.length})</Text>
		{shown.length === 0 && <Text color="gray">No transfer trials recorded.</Text>}
		{shown.map(transfer => <Text key={transfer.trial_id} wrap="truncate" color={
			transfer.outcome === 'positive' ? 'green' : transfer.outcome === 'negative' ? 'red' : 'white'
		}>
			{shortId(transfer.trial_id)} → {shortId(transfer.to_genome_id)} <Text color="gray">{safeText(transfer.outcome ?? 'pending')}</Text>
			{transfer.estimate_bps !== null && <Text color="gray"> {transfer.estimate_bps}bps</Text>}
		</Text>)}
		{contradiction && <Text color="red">CONTRADICTION · positive in {shortId(contradiction.positive_world_id)}, negative in {shortId(contradiction.negative_world_id)}</Text>}
		{species.length > 0 && <Text color="gold">SPECIATED · {species.map(single => shortId(single.species_id)).join(', ')}</Text>}
	</Box>;
}
