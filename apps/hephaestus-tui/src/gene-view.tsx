import React from 'react';
import {Box, Text} from 'ink';
import {windowed, shortId} from './lineage-view.js';
import {GeneHelix} from './motion.js';
import {borderColorProps, colorProps, useTheme} from './theme.js';
import {safeText, type GeneAggregate, type GeneSummary} from './protocol.js';

export function GeneListPanel({genes, selected, height, animate}: {genes: GeneSummary[]; selected: number; height: number; animate?: boolean}) {
	const theme = useTheme();
	const view = windowed(genes, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}><GeneHelix animate={animate} /> GENE BANK</Text>
		{genes.length === 0 && <Text {...colorProps(theme.color('muted'))}>No Genes extracted yet.</Text>}
		{view.items.map((summary, index) => {
			const active = view.offset + index === selected;
			return <Text key={summary.gene.gene_id} wrap="truncate" {...colorProps(theme.color(active ? 'judge' : summary.contradiction ? 'danger' : 'ink'))}>
				{active ? theme.glyphs.caret + ' ' : '  '}
				<Text {...colorProps(theme.color('sealed'))}>{shortId(summary.gene.gene_id)}</Text> <Text {...colorProps(theme.color('muted'))}>· {summary.lineages} lineages</Text>{' '}
				<Text {...colorProps(theme.color('improvement'))}>+{summary.positive}</Text> <Text {...colorProps(theme.color('muted'))}>~{summary.neutral}</Text> <Text {...colorProps(theme.color('regression'))}>-{summary.negative}</Text>
				{summary.contradiction && <Text {...colorProps(theme.color('danger'))}> CONTRADICTION</Text>}
				{summary.species_ids.length > 0 && <Text {...colorProps(theme.color('judge'))}>{' '}{summary.species_ids.length} species</Text>}
			</Text>;
		})}
	</Box>;
}

export function GeneDetailPanel({aggregate, height, animate}: {aggregate: GeneAggregate | undefined; height: number; animate?: boolean}) {
	const theme = useTheme();
	if (!aggregate) return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}><GeneHelix animate={animate} /> GENE</Text>
		<Text {...colorProps(theme.color('muted'))}>Loading…</Text>
	</Box>;
	const {gene, transfers, contradiction, species} = aggregate;
	const shown = transfers.slice(0, Math.max(1, height));
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}><GeneHelix animate={animate} /> GENE / <Text {...colorProps(theme.color('sealed'))}>{shortId(gene.gene_id)}</Text></Text>
		<Text wrap="truncate">Origin  {shortId(gene.origin_parent_genome_id)} → {shortId(gene.origin_child_genome_id)}</Text>
		<Text wrap="truncate">Evidence {gene.evidence_trials}/{gene.evidence_threshold} trials at extraction</Text>
		<Text {...colorProps(theme.color('muted'))}>TRANSFERS ({transfers.length})</Text>
		{shown.length === 0 && <Text {...colorProps(theme.color('muted'))}>No transfer trials recorded.</Text>}
		{shown.map(transfer => <Text key={transfer.trial_id} wrap="truncate" {...colorProps(theme.color(
			transfer.outcome === 'positive' ? 'improvement' : transfer.outcome === 'negative' ? 'regression' : 'ink',
		))}>
			{shortId(transfer.trial_id)} → {shortId(transfer.to_genome_id)} <Text {...colorProps(theme.color('muted'))}>{safeText(transfer.outcome ?? 'pending')}</Text>
			{transfer.estimate_bps !== null && <Text {...colorProps(theme.color('muted'))}> {transfer.estimate_bps}bps</Text>}
		</Text>)}
		{contradiction && <Text {...colorProps(theme.color('danger'))}>CONTRADICTION · positive in {shortId(contradiction.positive_world_id)}, negative in {shortId(contradiction.negative_world_id)}</Text>}
		{species.length > 0 && <Text {...colorProps(theme.color('judge'))}>SPECIATED · {species.map(single => shortId(single.species_id)).join(', ')}</Text>}
	</Box>;
}
