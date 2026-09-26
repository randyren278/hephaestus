import React from 'react';
import {Box, Text} from 'ink';
import {lineDiff, type ChampionRole, type LineageRow} from './lineage.js';
import {borderColorProps, colorProps, useTheme, type Role} from './theme.js';
import {safeText, type Champion, type Genome, type World} from './protocol.js';

const ROLE_MARK: Record<ChampionRole, {mark: string; role: Role; label: string}> = {
	champion: {mark: '▲', role: 'champion', label: 'CHAMPION'},
	standby: {mark: '▪', role: 'ink', label: 'standby'},
	quarantined: {mark: '✕', role: 'danger', label: 'quarantined'},
};

export function shortId(id: string): string {
	const tail = id.split(':').pop() ?? id;
	return tail.length > 12 ? tail.slice(0, 12) : tail;
}

/** Keeps the selected row visible inside a fixed-height window. */
export function windowed<T>(items: T[], selected: number, height: number): {items: T[]; offset: number} {
	const size = Math.max(1, height);
	const offset = Math.min(Math.max(0, selected - Math.floor(size / 2)), Math.max(0, items.length - size));
	return {items: items.slice(offset, offset + size), offset};
}

export function WorldList({worlds, selected, height}: {worlds: World[]; selected: number; height: number}) {
	const theme = useTheme();
	const view = windowed(worlds, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>WORLDS</Text>
		{worlds.length === 0 && <Text {...colorProps(theme.color('muted'))}>No registered Worlds.</Text>}
		{view.items.map((world, index) => {
			const active = view.offset + index === selected;
			return <Text key={world.world_id} wrap="truncate" {...colorProps(theme.color(active ? 'judge' : 'ink'))}>
				{active ? theme.glyphs.caret + ' ' : '  '}{safeText(world.name)} <Text {...colorProps(theme.color('muted'))}>{shortId(world.world_id)}</Text>
			</Text>;
		})}
	</Box>;
}

export function LineagePanel({world, rows, champion, selected, height}: {
	world: World; rows: LineageRow[]; champion: Champion | undefined; selected: number; height: number;
}) {
	const theme = useTheme();
	const view = windowed(rows, selected, height);
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>LINEAGE / {safeText(world.name)}</Text>
		<Text wrap="truncate" {...colorProps(theme.color('muted'))}>
			Champion {champion?.champion_genome_id ? shortId(champion.champion_genome_id) : 'none'} · {champion?.transitions.length ?? 0} transitions
		</Text>
		{rows.length === 0 && <Text {...colorProps(theme.color('muted'))}>No Genomes registered under this World.</Text>}
		{view.items.map((row, index) => {
			const active = view.offset + index === selected;
			const role = row.role ? ROLE_MARK[row.role] : undefined;
			return <Text key={row.genome_id} wrap="truncate" {...colorProps(theme.color(active ? 'judge' : 'ink'))}>
				{active ? theme.glyphs.caret + ' ' : '  '}<Text {...colorProps(theme.color('muted'))}>{row.prefix}</Text>
				{role ? <Text {...colorProps(theme.color(role.role))}>{role.mark} </Text> : '  '}
				{safeText(row.name)} <Text {...colorProps(theme.color('muted'))}>{shortId(row.genome_id)}</Text>
				{row.extra_parents > 0 && <Text {...colorProps(theme.color('muted'))}> +{row.extra_parents} parents</Text>}
				{role && <Text {...colorProps(theme.color(role.role))}> {role.label}</Text>}
			</Text>;
		})}
	</Box>;
}

export function GenomeDetail({genome, parent, role, prompt, parentPrompt, height}: {
	genome: Genome; parent: Genome | undefined; role: ChampionRole | null;
	/** `undefined` while loading, `null` when the Genome has no verified Markdown prompt. */
	prompt: string | null | undefined; parentPrompt: string | null | undefined; height: number;
}) {
	const theme = useTheme();
	const diff = typeof prompt === 'string' ? lineDiff(parentPrompt ?? '', prompt) : [];
	const changed = diff.filter(line => line.kind !== 'same');
	const shown = (changed.length > 0 ? changed : diff).slice(0, Math.max(1, height));
	return <Box flexDirection="column" borderStyle="single" {...borderColorProps(theme.color('border'))} paddingX={1}>
		<Text bold {...colorProps(theme.color('judge'))}>GENOME / {safeText(genome.name)}</Text>
		<Text wrap="truncate">ID      {safeText(genome.genome_id)}</Text>
		<Text wrap="truncate">Parent  {parent ? `${safeText(parent.name)} ${shortId(parent.genome_id)}` : 'none (root)'}</Text>
		<Text>Role    {role ? <Text {...colorProps(theme.color(ROLE_MARK[role].role))}>{ROLE_MARK[role].label}</Text> : 'none'}</Text>
		<Text {...colorProps(theme.color('muted'))}>PROMPT DIFF {parent ? 'vs parent' : 'vs empty'} · {changed.filter(line => line.kind === 'add').length} added · {changed.filter(line => line.kind === 'remove').length} removed</Text>
		{prompt === undefined && <Text {...colorProps(theme.color('muted'))}>Loading verified prompt…</Text>}
		{prompt === null && <Text {...colorProps(theme.color('muted'))}>No verified Markdown prompt for this Genome.</Text>}
		{typeof prompt === 'string' && changed.length === 0 && <Text {...colorProps(theme.color('muted'))}>Prompt identical to parent.</Text>}
		{shown.map((line, index) => <Text key={index} wrap="truncate" {...colorProps(theme.color(line.kind === 'add' ? 'improvement' : line.kind === 'remove' ? 'regression' : 'muted'))}>
			{line.kind === 'add' ? '+ ' : line.kind === 'remove' ? '- ' : '  '}{safeText(line.text)}
		</Text>)}
	</Box>;
}
