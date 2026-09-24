import type {Champion, Genome} from './protocol.js';

export type ChampionRole = 'champion' | 'standby' | 'quarantined';
export type LineageRow = {
	genome_id: string;
	name: string;
	prefix: string;
	role: ChampionRole | null;
	/** Registered parents beyond the one this row is drawn under. */
	extra_parents: number;
};

export function roleOf(genomeId: string, champion: Champion | undefined): ChampionRole | null {
	if (!champion) return null;
	if (champion.champion_genome_id === genomeId) return 'champion';
	if (champion.quarantined_genome_ids.includes(genomeId)) return 'quarantined';
	if (champion.standby_genome_ids.includes(genomeId)) return 'standby';
	return null;
}

/**
 * Lays out one World's ancestry DAG as a tree drawn under each Genome's first
 * registered parent. Every Genome appears exactly once; additional parents are
 * counted rather than duplicated.
 */
export function lineageRows(genomes: Genome[], worldId: string, champion?: Champion): LineageRow[] {
	const inWorld = genomes.filter(genome => genome.world_id === worldId);
	const known = new Set(inWorld.map(genome => genome.genome_id));
	const order = (left: Genome, right: Genome) => left.name.localeCompare(right.name) || left.genome_id.localeCompare(right.genome_id);
	const children = new Map<string, Genome[]>();
	const roots: Genome[] = [];
	for (const genome of inWorld) {
		const parent = genome.parent_ids.find(id => known.has(id));
		if (parent === undefined) roots.push(genome);
		else children.set(parent, [...(children.get(parent) ?? []), genome]);
	}
	const rows: LineageRow[] = [];
	const visited = new Set<string>();
	const visit = (genome: Genome, lead: string, branch: string) => {
		if (visited.has(genome.genome_id)) return;
		visited.add(genome.genome_id);
		const registeredParents = genome.parent_ids.filter(id => known.has(id)).length;
		rows.push({
			genome_id: genome.genome_id,
			name: genome.name,
			prefix: lead + branch,
			role: roleOf(genome.genome_id, champion),
			extra_parents: Math.max(0, registeredParents - 1),
		});
		const next = [...(children.get(genome.genome_id) ?? [])].sort(order);
		const childLead = lead + (branch === '' ? '' : branch === '└─ ' ? '   ' : '│  ');
		next.forEach((child, index) => visit(child, childLead, index === next.length - 1 ? '└─ ' : '├─ '));
	};
	for (const root of [...roots].sort(order)) visit(root, '', '');
	// A parent cycle cannot be registered, but never drop a Genome from view.
	for (const genome of [...inWorld].sort(order)) visit(genome, '', '');
	return rows;
}

export type DiffLine = {kind: 'same' | 'add' | 'remove'; text: string};

export const MAX_DIFF_LINES = 400;

/** Line diff by longest common subsequence, bounded to keep rendering cheap. */
export function lineDiff(before: string, after: string): DiffLine[] {
	const left = before.split('\n').slice(0, MAX_DIFF_LINES);
	const right = after.split('\n').slice(0, MAX_DIFF_LINES);
	const table: number[][] = Array.from({length: left.length + 1}, () => new Array<number>(right.length + 1).fill(0));
	for (let i = left.length - 1; i >= 0; i -= 1) {
		for (let j = right.length - 1; j >= 0; j -= 1) {
			table[i]![j] = left[i] === right[j] ? table[i + 1]![j + 1]! + 1 : Math.max(table[i + 1]![j]!, table[i]![j + 1]!);
		}
	}
	const lines: DiffLine[] = [];
	let i = 0;
	let j = 0;
	while (i < left.length && j < right.length) {
		if (left[i] === right[j]) {
			lines.push({kind: 'same', text: left[i]!});
			i += 1;
			j += 1;
		} else if (table[i + 1]![j]! >= table[i]![j + 1]!) {
			lines.push({kind: 'remove', text: left[i]!});
			i += 1;
		} else {
			lines.push({kind: 'add', text: right[j]!});
			j += 1;
		}
	}
	for (; i < left.length; i += 1) lines.push({kind: 'remove', text: left[i]!});
	for (; j < right.length; j += 1) lines.push({kind: 'add', text: right[j]!});
	return lines;
}
