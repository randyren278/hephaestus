import type {ApiResponse, Champion, Command, Genome, World} from '../../../hephaestus-tui/src/protocol.js';
import {lineDiff, lineageRows, roleOf, type ChampionRole} from '../../../hephaestus-tui/src/lineage.js';

// The session token travels as a URL fragment (never sent to the server by
// the browser, unlike a query string) and is read once, then stripped from
// the visible address bar. Every subsequent API call attaches it as a
// header explicitly.
const hashParams = new URLSearchParams(location.hash.replace(/^#/, ''));
const token = hashParams.get('token') ?? '';
if (token) history.replaceState(null, '', location.pathname);

const tokenIndicator = document.getElementById('token-indicator')!;
tokenIndicator.textContent = token ? 'session token loaded' : 'NO TOKEN — open the printed URL';
tokenIndicator.className = token ? 'hdr-token ok' : 'hdr-token bad';

type View = 'status' | 'worlds' | 'genome' | 'genes' | 'drift-canary' | 'experiments' | 'activity';
const views: Record<View, HTMLElement> = {
	status: document.getElementById('view-status')!,
	worlds: document.getElementById('view-worlds')!,
	genome: document.getElementById('view-genome')!,
	genes: document.getElementById('view-genes')!,
	'drift-canary': document.getElementById('view-drift-canary')!,
	experiments: document.getElementById('view-experiments')!,
	activity: document.getElementById('view-activity')!,
};

function showView(view: View): void {
	for (const [name, element] of Object.entries(views)) element.hidden = name !== view;
	for (const tab of document.querySelectorAll<HTMLButtonElement>('.tab')) {
		tab.classList.toggle('active', tab.dataset['view'] === view);
	}
}

for (const tab of document.querySelectorAll<HTMLButtonElement>('.tab')) {
	tab.addEventListener('click', () => {
		const view = tab.dataset['view'] as View;
		showView(view);
		if (view === 'genes') void loadGenes();
		if (view === 'drift-canary') void loadDriftCanary();
		if (view === 'experiments') void loadExperiments();
		if (view === 'activity') void loadActivity();
	});
}

function escapeHtml(value: string): string {
	return value.replace(/[&<>"']/g, char => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'})[char]!);
}

async function api(command: Command): Promise<ApiResponse> {
	const res = await fetch('/api/command', {
		method: 'POST',
		headers: {'Content-Type': 'application/json', 'x-hephaestus-web-token': token},
		body: JSON.stringify(command),
	});
	return (await res.json()) as ApiResponse;
}

function noticeHtml(response: ApiResponse): string {
	if (!response.error) return '';
	return `<p class="notice error">${escapeHtml(response.error.code)}: ${escapeHtml(response.error.message)}</p>`;
}

/** Deterministic accent hue per World, so unrelated Worlds never share a color and stay visually separated. */
function worldAccent(worldId: string): string {
	let hash = 0;
	for (let index = 0; index < worldId.length; index += 1) hash = (hash * 31 + worldId.charCodeAt(index)) >>> 0;
	const hue = hash % 360;
	return `hsl(${hue} 70% 55%)`;
}

const ROLE_LABEL: Record<ChampionRole, string> = {champion: 'Champion', standby: 'Standby', quarantined: 'Quarantined'};

let currentGenomes: Genome[] = [];
let currentWorlds: World[] = [];
const championByWorld = new Map<string, Champion>();

function shortId(id: string): string {
	const tail = id.split(':').pop() ?? id;
	return tail.length > 16 ? `${tail.slice(0, 16)}…` : tail;
}

async function loadStatus(): Promise<void> {
	const el = views.status;
	el.innerHTML = '<div class="card"><h2>Status</h2><p class="notice">Loading…</p></div>';
	const response = await api({command: 'status'});
	if (response.data?.type !== 'status') {
		el.innerHTML = `<div class="card"><h2>Status</h2>${noticeHtml(response) || '<p class="notice error">unexpected response</p>'}</div>`;
		return;
	}
	const s = response.data;
	el.innerHTML = `
    <div class="card">
      <h2>Daemon Status</h2>
      <div class="grid">
        <div class="stat"><div class="label">Frozen</div><div class="value">${s.frozen ? 'YES' : 'no'}</div></div>
        <div class="stat"><div class="label">Active runs</div><div class="value">${s.active_runs}</div></div>
        <div class="stat"><div class="label">Event count</div><div class="value">${s.event_count}</div></div>
        <div class="stat"><div class="label">Genomes</div><div class="value">${s.genome_count}</div></div>
      </div>
    </div>
    <div class="card">
      <h3>Job lookup</h3>
      <input class="field" id="job-id-input" placeholder="job id" />
      <button class="btn" id="job-lookup-btn">Look up</button>
      <div id="job-result"></div>
    </div>`;
	document.getElementById('job-lookup-btn')!.addEventListener('click', async () => {
		const input = document.getElementById('job-id-input') as HTMLInputElement;
		const result = document.getElementById('job-result')!;
		const jobId = input.value.trim();
		if (!jobId) return;
		result.innerHTML = '<p class="notice">Loading…</p>';
		const jobResponse = await api({command: 'job_status', job_id: jobId});
		if (jobResponse.data?.type !== 'job') {
			result.innerHTML = noticeHtml(jobResponse) || '<p class="notice error">job not found</p>';
			return;
		}
		const job = jobResponse.data.job;
		result.innerHTML = `<table class="kv">
      <tr><td>State</td><td>${escapeHtml(job.state)}</td></tr>
      <tr><td>Terminal</td><td>${job.terminal ? escapeHtml(job.terminal) : 'pending'}</td></tr>
      <tr><td>Genome</td><td>${escapeHtml(job.genome_id)}</td></tr>
      <tr><td>World</td><td>${escapeHtml(job.world_id)}</td></tr>
      <tr><td>Trace events</td><td>${jobResponse.data.progress.trace_events}</td></tr>
    </table>`;
	});
}

function renderTransitions(champion: Champion | undefined): string {
	if (!champion || champion.transitions.length === 0) return '<p class="notice">No Champion transitions recorded.</p>';
	return champion.transitions
		.slice()
		.sort((a, b) => a.sequence - b.sequence)
		.map(
			transition => `<div class="transition-row ${transition.kind}">
        <strong>${escapeHtml(transition.kind)}</strong> · seq ${transition.sequence} · ${escapeHtml(shortId(transition.champion_genome_id))}
        ${transition.previous_champion_genome_id ? ` (was ${escapeHtml(shortId(transition.previous_champion_genome_id))})` : ''}
        ${transition.reason ? `<div class="notice">${escapeHtml(transition.reason)}</div>` : ''}
      </div>`,
		)
		.join('');
}

function openGenome(genomeId: string): void {
	showView('genome');
	void loadGenome(genomeId);
}

function renderWorldCard(world: World, genomes: Genome[], champion: Champion | undefined): string {
	const rows = lineageRows(genomes, world.world_id, champion);
	const byRole = (role: ChampionRole) => rows.filter(row => row.role === role);
	const renderChips = (list: ReturnType<typeof lineageRows>, cls: string) =>
		list.length === 0
			? '<span class="notice">none</span>'
			: list.map(row => `<span class="chip ${cls}" data-genome="${escapeHtml(row.genome_id)}">${escapeHtml(row.name)} <small>${escapeHtml(shortId(row.genome_id))}</small></span>`).join('');
	return `<div class="world-card" style="--world-accent: ${worldAccent(world.world_id)}">
    <h3>${escapeHtml(world.name)} <small class="notice">${escapeHtml(shortId(world.world_id))}</small></h3>
    <div class="role-row"><span class="role-label">Champion</span>${renderChips(byRole('champion'), 'champion')}</div>
    <div class="role-row"><span class="role-label">Standby</span>${renderChips(byRole('standby'), 'standby')}</div>
    <div class="role-row"><span class="role-label">Quarantined</span>${renderChips(byRole('quarantined'), 'quarantined')}</div>
    <details><summary class="notice">Full lineage (${rows.length} Genomes)</summary>
      <div>${rows.map(row => `<div class="notice">${escapeHtml(row.prefix)}<span class="chip" data-genome="${escapeHtml(row.genome_id)}">${escapeHtml(row.name)}</span>${row.role ? ` <em>${ROLE_LABEL[row.role]}</em>` : ''}${row.extra_parents > 0 ? ` +${row.extra_parents} parents` : ''}</div>`).join('')}</div>
    </details>
    <h3>Champion transition history</h3>
    ${renderTransitions(champion)}
  </div>`;
}

async function loadWorlds(): Promise<void> {
	const el = views.worlds;
	el.innerHTML = '<div class="card"><h2>Worlds</h2><p class="notice">Loading…</p></div>';
	const [worldsResponse, genomesResponse] = await Promise.all([api({command: 'world_list'}), api({command: 'genome_list'})]);
	if (worldsResponse.data?.type !== 'worlds' || genomesResponse.data?.type !== 'genomes') {
		el.innerHTML = `<div class="card"><h2>Worlds</h2>${noticeHtml(worldsResponse) || noticeHtml(genomesResponse) || '<p class="notice error">unexpected response</p>'}</div>`;
		return;
	}
	currentWorlds = worldsResponse.data.worlds;
	currentGenomes = genomesResponse.data.genomes;
	if (currentWorlds.length === 0) {
		el.innerHTML = '<div class="card"><h2>Worlds</h2><p class="notice">No registered Worlds.</p></div>';
		return;
	}
	const cards = await Promise.all(
		currentWorlds.map(async world => {
			const championResponse = await api({command: 'champion_show', world_id: world.world_id});
			const champion = championResponse.data?.type === 'champion' ? championResponse.data.champion : undefined;
			if (champion) championByWorld.set(world.world_id, champion);
			return renderWorldCard(world, currentGenomes, champion);
		}),
	);
	el.innerHTML = cards.join('');
	el.querySelectorAll<HTMLElement>('[data-genome]').forEach(chip => {
		chip.addEventListener('click', () => openGenome(chip.dataset['genome']!));
	});
}

async function fetchPrompt(genomeId: string): Promise<string | null> {
	const response = await api({command: 'genome_prompt', genome_id: genomeId});
	if (response.data?.type === 'genome_prompt') return response.data.prompt;
	return null;
}

async function loadGenome(genomeId: string): Promise<void> {
	const el = views.genome;
	el.innerHTML = '<div class="card"><h2>Genome</h2><p class="notice">Loading…</p></div>';
	let genome = currentGenomes.find(candidate => candidate.genome_id === genomeId);
	if (!genome) {
		const response = await api({command: 'genome_show', genome_id: genomeId});
		if (response.data?.type === 'genome') genome = response.data.genome;
	}
	if (!genome) {
		el.innerHTML = `<div class="card"><h2>Genome</h2><p class="notice error">Genome not found: ${escapeHtml(genomeId)}</p></div>`;
		return;
	}
	const champion = championByWorld.get(genome.world_id);
	const role = roleOf(genome.genome_id, champion);
	const parentId = genome.parent_ids.find(id => currentGenomes.some(candidate => candidate.genome_id === id));
	const parent = currentGenomes.find(candidate => candidate.genome_id === parentId);
	const [prompt, parentPrompt] = await Promise.all([fetchPrompt(genome.genome_id), parent ? fetchPrompt(parent.genome_id) : Promise.resolve('')]);
	const diff = prompt !== null ? lineDiff(parentPrompt ?? '', prompt) : [];
	const diffHtml =
		prompt === null
			? '<p class="notice">No verified Markdown prompt for this Genome.</p>'
			: diff.length === 0
				? '<p class="notice">Empty prompt.</p>'
				: diff
						.map(line => {
							const cls = line.kind === 'add' ? 'diff-add' : line.kind === 'remove' ? 'diff-del' : 'diff-ctx';
							const mark = line.kind === 'add' ? '+' : line.kind === 'remove' ? '-' : ' ';
							return `<div class="diff-line ${cls}">${mark} ${escapeHtml(line.text)}</div>`;
						})
						.join('');
	el.innerHTML = `<div class="card">
    <h2>${escapeHtml(genome.name)}</h2>
    <table class="kv">
      <tr><td>Genome ID</td><td>${escapeHtml(genome.genome_id)}</td></tr>
      <tr><td>World</td><td>${escapeHtml(genome.world_id)}</td></tr>
      <tr><td>Role</td><td>${role ? ROLE_LABEL[role] : 'none'}</td></tr>
      <tr><td>Parent</td><td>${parent ? `${escapeHtml(parent.name)} (${escapeHtml(shortId(parent.genome_id))})` : 'none (root)'}</td></tr>
    </table>
  </div>
  <div class="card">
    <h3>Prompt diff ${parent ? 'vs parent' : ''}</h3>
    <pre class="prompt">${diffHtml}</pre>
  </div>`;
}

async function loadGenes(): Promise<void> {
	const el = views.genes;
	el.innerHTML = '<div class="card"><h2>Genes</h2><p class="notice">Loading…</p></div>';
	const response = await api({command: 'gene_list'});
	if (response.data?.type !== 'genes') {
		el.innerHTML = `<div class="card"><h2>Genes</h2>${noticeHtml(response) || '<p class="notice error">unexpected response</p>'}</div>`;
		return;
	}
	const genes = response.data.genes;
	if (genes.length === 0) {
		el.innerHTML = '<div class="card"><h2>Genes</h2><p class="notice">No extracted Genes.</p></div>';
		return;
	}
	el.innerHTML = `<div class="card"><h2>Genes</h2><table class="kv">
    <tr><th>Gene</th><th>World</th><th>Lineages</th><th>+ / ~ / -</th><th>Contradiction</th><th>Species</th></tr>
    ${genes
			.map(
				summary => `<tr>
        <td>${escapeHtml(shortId(summary.gene.gene_id))}</td>
        <td>${escapeHtml(shortId(summary.gene.world_id))}</td>
        <td>${summary.lineages}</td>
        <td>${summary.positive} / ${summary.neutral} / ${summary.negative}</td>
        <td>${summary.contradiction ? 'yes' : 'no'}</td>
        <td>${summary.species_ids.length}</td>
      </tr>`,
			)
			.join('')}
  </table></div>`;
}

function formatCost(microusd: number | null): string {
	if (microusd === null) return '—';
	return `$${(microusd / 1_000_000).toFixed(6)}`;
}

async function loadActivity(): Promise<void> {
	const el = views.activity;
	el.innerHTML = '<div class="card"><h2>Activity</h2><p class="notice">Loading…</p></div>';
	const [runs, evaluations, denials] = await Promise.all([
		api({command: 'run_list', limit: 50}),
		api({command: 'evaluation_list', limit: 50}),
		api({command: 'denial_list', limit: 50}),
	]);
	const runsHtml =
		runs.data?.type === 'run_list'
			? `<table class="kv"><tr><th>Run</th><th>Genome</th><th>State</th><th>Cost</th><th>Latency</th></tr>${runs.data.runs
					.map(
						run => `<tr>
          <td>${escapeHtml(shortId(run.run_id))}</td>
          <td>${escapeHtml(shortId(run.genome_id))}</td>
          <td>${escapeHtml(run.state)}</td>
          <td>${formatCost(run.actual_cost_microusd)}</td>
          <td>${run.latency_millis ?? '—'}</td>
        </tr>`,
					)
					.join('')}</table>`
			: noticeHtml(runs) || '<p class="notice error">unexpected response</p>';
	const evaluationsHtml =
		evaluations.data?.type === 'evaluation_list'
			? `<table class="kv"><tr><th>Evaluation</th><th>World</th><th>Parent cost</th><th>Candidate cost</th></tr>${evaluations.data.evaluations
					.map(
						entry => `<tr>
          <td>${escapeHtml(shortId(entry.evaluation.evaluation_id))}</td>
          <td>${escapeHtml(shortId(entry.evaluation.world_id))}</td>
          <td>${formatCost(entry.selection?.parent_cost_microusd ?? null)}</td>
          <td>${formatCost(entry.selection?.candidate_cost_microusd ?? null)}</td>
        </tr>`,
					)
					.join('')}</table>`
			: noticeHtml(evaluations) || '<p class="notice error">unexpected response</p>';
	const denialsHtml =
		denials.data?.type === 'denial_list'
			? `<table class="kv"><tr><th>Kind</th><th>Command / Tool</th><th>Client</th><th>Genome</th><th>World</th></tr>${denials.data.denials
					.map(
						entry => `<tr>
          <td>${escapeHtml(entry.kind)}</td>
          <td>${entry.command ? escapeHtml(entry.command) : '—'}</td>
          <td>${entry.client_id ? escapeHtml(entry.client_id) : '—'}</td>
          <td>${entry.genome_id ? escapeHtml(shortId(entry.genome_id)) : '—'}</td>
          <td>${entry.world_id ? escapeHtml(shortId(entry.world_id)) : '—'}</td>
        </tr>`,
					)
					.join('')}</table>`
			: noticeHtml(denials) || '<p class="notice error">unexpected response</p>';
	el.innerHTML = `
    <div class="card"><h2>Runs &amp; costs</h2>${runsHtml}</div>
    <div class="card"><h2>Arena evaluations &amp; costs</h2>${evaluationsHtml}</div>
    <div class="card"><h2>Authority &amp; denial history</h2>${denialsHtml}</div>`;
}

function formatX10000(value: number): string {
	return (value / 10000).toFixed(4);
}

async function loadDriftCanary(): Promise<void> {
	const el = views['drift-canary'];
	el.innerHTML = '<div class="card"><h2>Drift &amp; Canary</h2><p class="notice">Loading…</p></div>';
	const [drifts, canaries] = await Promise.all([
		api({command: 'drift_list', limit: 50}),
		api({command: 'canary_list', limit: 50}),
	]);
	const driftsHtml =
		drifts.data?.type === 'drift_list'
			? drifts.data.drifts.length === 0
				? '<p class="notice">No drift recorded.</p>'
				: `<table class="kv"><tr><th>Drift</th><th>World</th><th>Kind</th><th>Observed</th><th>Threshold</th></tr>${drifts.data.drifts
						.map(
							drift => `<tr>
          <td>${escapeHtml(shortId(drift.drift_id))}</td>
          <td>${escapeHtml(shortId(drift.world_id))}</td>
          <td>${escapeHtml(drift.kind)}</td>
          <td>${drift.observed_delta_bps}bps</td>
          <td>${drift.threshold_bps}bps</td>
        </tr>`,
						)
						.join('')}</table>`
			: noticeHtml(drifts) || '<p class="notice error">unexpected response</p>';
	const canariesHtml =
		canaries.data?.type === 'canary_list'
			? canaries.data.canaries.length === 0
				? '<p class="notice">No canaries started.</p>'
				: `<table class="kv"><tr><th>Canary</th><th>World</th><th>Candidate</th><th>Stage</th><th>Transitions</th></tr>${canaries.data.canaries
						.map(
							canary => `<tr>
          <td>${escapeHtml(shortId(canary.canary_id))}</td>
          <td>${escapeHtml(shortId(canary.world_id))}</td>
          <td>${escapeHtml(shortId(canary.candidate_genome_id))}</td>
          <td>${escapeHtml(canary.stage)}</td>
          <td>${canary.transitions.length}</td>
        </tr>`,
						)
						.join('')}</table>`
			: noticeHtml(canaries) || '<p class="notice error">unexpected response</p>';
	el.innerHTML = `
    <div class="card"><h2>Drift records</h2>${driftsHtml}</div>
    <div class="card"><h2>Canaries</h2>${canariesHtml}</div>`;
}

async function loadExperiments(): Promise<void> {
	const el = views.experiments;
	el.innerHTML = '<div class="card"><h2>Experiments</h2><p class="notice">Loading…</p></div>';
	const [strategies, receipts] = await Promise.all([
		api({command: 'meta_strategy_list'}),
		api({command: 'meta_list', limit: 50}),
	]);
	const strategiesHtml =
		strategies.data?.type === 'meta_strategies'
			? strategies.data.strategies.length === 0
				? '<p class="notice">No Evolver strategies registered.</p>'
				: `<table class="kv"><tr><th>Strategy</th><th>Mutation prioritization</th><th>Generations</th><th>Candidates</th><th>Gene selection</th></tr>${strategies.data.strategies
						.map(
							strategy => `<tr>
          <td>${escapeHtml(strategy.config.name)} <small class="notice">${escapeHtml(shortId(strategy.strategy_id))}</small></td>
          <td>${escapeHtml(strategy.config.mutation_prioritization)}</td>
          <td>${strategy.config.generation_count}</td>
          <td>${strategy.config.candidate_count}</td>
          <td>${escapeHtml(strategy.config.gene_selection)}</td>
        </tr>`,
						)
						.join('')}</table>`
			: noticeHtml(strategies) || '<p class="notice error">unexpected response</p>';
	const receiptsHtml =
		receipts.data?.type === 'meta_evaluation_list'
			? receipts.data.receipts.length === 0
				? '<p class="notice">No meta-evaluations recorded.</p>'
				: `<table class="kv"><tr><th>Meta-evaluation</th><th>A</th><th>B</th><th>Lineages</th><th>Quality delta</th><th>Cost delta</th></tr>${receipts.data.receipts
						.map(
							receipt => `<tr>
          <td>${escapeHtml(shortId(receipt.meta_run_id))}</td>
          <td>${escapeHtml(shortId(receipt.strategy_a_id))}</td>
          <td>${escapeHtml(shortId(receipt.strategy_b_id))}</td>
          <td>${receipt.lineages.length}</td>
          <td>${formatX10000(receipt.quality_delta.estimate_x10000)} [${formatX10000(receipt.quality_delta.lower_x10000)}, ${formatX10000(receipt.quality_delta.upper_x10000)}]</td>
          <td>${formatX10000(receipt.cost_delta.estimate_x10000)} [${formatX10000(receipt.cost_delta.lower_x10000)}, ${formatX10000(receipt.cost_delta.upper_x10000)}]</td>
        </tr>`,
						)
						.join('')}</table>`
			: noticeHtml(receipts) || '<p class="notice error">unexpected response</p>';
	el.innerHTML = `
    <div class="card"><h2>Evolver strategies</h2>${strategiesHtml}</div>
    <div class="card"><h2>Meta-evaluation receipts</h2>${receiptsHtml}</div>`;
}

showView('status');
void loadStatus();
void loadWorlds();
