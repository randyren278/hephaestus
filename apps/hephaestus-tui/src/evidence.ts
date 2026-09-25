import type {DenialEntry, EvaluationListEntry, RunListEntry} from './protocol.js';

/** Formats a micro-US-dollar integer as a human dollar amount, or an em dash when absent. */
export function formatMicroUsd(value: number | null): string {
	if (value === null) return '—';
	return `$${(value / 1_000_000).toFixed(4)}`;
}

/** Formats a millisecond duration compactly, or an em dash when absent. */
export function formatLatency(value: number | null): string {
	if (value === null) return '—';
	if (value < 1000) return `${value}ms`;
	return `${(value / 1000).toFixed(2)}s`;
}

export function formatWorldLabel(worldId: string | null): string {
	return worldId ? worldId : 'UNASSIGNED WORLD';
}

/**
 * One render row inside a World-grouped list: either a group header (never
 * selectable) or a data row carrying its stable index into the flat item
 * array the caller navigates with up/down. Grouping by World, rather than
 * interleaving, is what keeps incompatible Worlds visually separated.
 */
export type RenderRow<T> = {kind: 'header'; worldId: string | null} | {kind: 'row'; item: T; dataIndex: number};

/** Groups items by World identity, Worlds sorted for determinism (unassigned last), items keeping input order within a group. */
export function groupByWorld<T>(items: T[], worldIdOf: (item: T) => string | null): RenderRow<T>[] {
	const order: (string | null)[] = [];
	const buckets = new Map<string | null, T[]>();
	for (const item of items) {
		const worldId = worldIdOf(item);
		if (!buckets.has(worldId)) { buckets.set(worldId, []); order.push(worldId); }
		buckets.get(worldId)!.push(item);
	}
	order.sort((a, b) => (a === null ? 1 : b === null ? -1 : a.localeCompare(b)));
	const rows: RenderRow<T>[] = [];
	let dataIndex = 0;
	for (const worldId of order) {
		rows.push({kind: 'header', worldId});
		for (const item of buckets.get(worldId)!) { rows.push({kind: 'row', item, dataIndex}); dataIndex += 1; }
	}
	return rows;
}

export function totalDataRows<T>(rows: RenderRow<T>[]): number {
	return rows.filter(row => row.kind === 'row').length;
}

/**
 * Returns a contiguous slice of `rows` (headers included) sized to `height`
 * that keeps the data row at `selectedDataIndex` visible, mirroring
 * `windowed()` from lineage-view but operating over header+row entries.
 */
export function windowedGroups<T>(rows: RenderRow<T>[], selectedDataIndex: number, height: number): {rows: RenderRow<T>[]; offset: number} {
	const size = Math.max(1, height);
	const anchor = rows.findIndex(row => row.kind === 'row' && row.dataIndex === selectedDataIndex);
	const safeAnchor = anchor === -1 ? 0 : anchor;
	const offset = Math.min(Math.max(0, safeAnchor - Math.floor(size / 2)), Math.max(0, rows.length - size));
	return {rows: rows.slice(offset, offset + size), offset};
}

export type CostEntry = {world_id: string | null; genome_id: string; total_microusd: number; samples: number};

/**
 * Totals cost per (World, Genome) from both direct/async run results
 * (`actual_cost_microusd`) and paired Arena evaluations (parent and
 * candidate cost attributed separately from `selection`).
 */
export function aggregateCosts(runs: RunListEntry[], evaluations: EvaluationListEntry[]): CostEntry[] {
	const map = new Map<string, CostEntry>();
	const add = (worldId: string | null, genomeId: string, microusd: number) => {
		const key = `${worldId ?? ''}\u0000${genomeId}`;
		const existing = map.get(key) ?? {world_id: worldId, genome_id: genomeId, total_microusd: 0, samples: 0};
		existing.total_microusd += microusd;
		existing.samples += 1;
		map.set(key, existing);
	};
	for (const run of runs) if (run.actual_cost_microusd !== null) add(run.world_id, run.genome_id, run.actual_cost_microusd);
	for (const entry of evaluations) {
		if (!entry.selection) continue;
		add(entry.evaluation.world_id, entry.evaluation.parent_genome_id, entry.selection.parent_cost_microusd);
		add(entry.evaluation.world_id, entry.evaluation.candidate_genome_id, entry.selection.candidate_cost_microusd);
	}
	return [...map.values()].sort((a, b) => b.total_microusd - a.total_microusd || a.genome_id.localeCompare(b.genome_id));
}

export function denialSummary(denial: DenialEntry): string {
	const scope = [denial.world_id, denial.genome_id, denial.run_id].filter((value): value is string => Boolean(value)).join(' · ');
	const subject = denial.command ?? denial.kind.replace(/_/g, ' ');
	return scope ? `${subject} — ${scope}` : subject;
}
