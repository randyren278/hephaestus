import type {EvaluationListEntry} from '../../../hephaestus-tui/src/protocol.js';

export function escapeHtml(value: string): string {
	return value.replace(/[&<>"']/g, char => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'})[char]!);
}

export function comparisonVerdict(entry: EvaluationListEntry): {label: string; explanation: string; tone: string} {
	if (!entry.selection) return {label: 'Selection pending', explanation: 'The trials are recorded. Record a selection receipt to check the improvement and cost gates.', tone: 'pending'};
	if (!entry.selection.metrics_eligible) return {label: 'Measured gates not met', explanation: 'The candidate did not clear the World’s measured improvement, reliability, cost and latency requirements.', tone: 'rejected'};
	if (!entry.invariants) return {label: 'Measured gates passed', explanation: 'The measured improvement passed. Independent invariant checks and a Forge assessment are still required before promotion.', tone: 'pending'};
	if (!entry.invariants.regressions_within_budget || !entry.invariants.candidate_contract_satisfied) return {label: 'Safety checks not met', explanation: 'The measured improvement passed, but the candidate failed its contract or regression budget.', tone: 'rejected'};
	if (!entry.forge_assessment) return {label: 'Assessment pending', explanation: 'Measured and invariant gates passed. A passing Forge assessment is still required for promotion.', tone: 'pending'};
	if (entry.forge_assessment.outcome !== 'metrics_passed') return {label: 'Forge assessment not met', explanation: 'The recorded Forge assessment rejected the candidate. This evaluation does not authorize promotion.', tone: 'rejected'};
	return {label: 'Evidence gates passed', explanation: 'Measured, invariant and Forge checks passed. Champion changes remain a separate policy-gated decision; check the World’s transition history.', tone: 'passed'};
}

function money(value: number): string { return `$${(value / 1_000_000).toFixed(6)}`; }
function percentagePoints(value: number): string { return `${value > 0 ? '+' : ''}${(value / 100).toFixed(2)} pp`; }

export function renderComparison(entry: EvaluationListEntry): string {
	const evaluation = entry.evaluation;
	const selection = entry.selection;
	const invariant = entry.invariants;
	const verdict = comparisonVerdict(entry);
	return `<article class="card comparison ${verdict.tone}">
    <div class="comparison-heading"><h3>${escapeHtml(evaluation.evaluation_id)}</h3><span class="verdict">${verdict.label}</span></div>
    <p class="notice">${verdict.explanation}</p>
    <p class="notice identity">World: ${escapeHtml(evaluation.world_id)}</p>
    <div class="table-scroll"><table class="comparison-table">
      <thead><tr><th scope="col">Measure</th><th scope="col">Parent</th><th scope="col">Candidate</th></tr></thead>
      <tbody>
        <tr><th scope="row">Visible tasks correct</th><td>${evaluation.parent_visible_correct} / ${evaluation.visible_total}</td><td>${evaluation.candidate_visible_correct} / ${evaluation.visible_total}</td></tr>
        <tr><th scope="row">Total measured cost</th><td>${selection ? money(selection.parent_cost_microusd) : 'Selection pending'}</td><td>${selection ? money(selection.candidate_cost_microusd) : 'Selection pending'}</td></tr>
        <tr><th scope="row">Total measured latency</th><td>${selection ? `${selection.parent_latency_millis} ms` : 'Selection pending'}</td><td>${selection ? `${selection.candidate_latency_millis} ms` : 'Selection pending'}</td></tr>
      </tbody>
    </table></div>
    <p class="notice">${selection ? `Paired correctness change: ${percentagePoints(selection.estimate_bps)}. Bootstrap interval: [${percentagePoints(selection.lower_bps)}, ${percentagePoints(selection.upper_bps)}].` : 'No selection receipt has been recorded.'}</p>
    <p class="notice">${invariant ? `Invariant checks: ${invariant.total_checks}; candidate violations: ${invariant.total_candidate_violations}; paired regressions: ${invariant.total_paired_regressions} / ${invariant.maximum_regressions} allowed.` : 'Independent invariant checks: not recorded.'}</p>
    <p class="notice">Forge assessment: ${entry.forge_assessment ? escapeHtml(entry.forge_assessment.outcome) : 'not recorded'}. Champion transitions linked to this evaluation: ${entry.champion_transition_ids.length}.</p>
    <details><summary>Agent identities and report</summary>
      <p class="notice identity">Parent: ${escapeHtml(evaluation.parent_genome_id)}<br>Candidate: ${escapeHtml(evaluation.candidate_genome_id)}</p>
      <button class="btn" data-report="${escapeHtml(evaluation.evaluation_id)}">Download evidence report</button>
    </details>
  </article>`;
}

/** Operator-visible aggregates only: never contains task payloads, sealed answers or credentials. */
export function comparisonReport(entry: EvaluationListEntry): string {
	const e = entry.evaluation;
	const s = entry.selection;
	const i = entry.invariants;
	const verdict = comparisonVerdict(entry);
	return [
		'# Hephaestus comparison evidence', '',
		`Evaluation: ${JSON.stringify(e.evaluation_id)}`, `World: ${JSON.stringify(e.world_id)}`, `Parent: ${JSON.stringify(e.parent_genome_id)}`, `Candidate: ${JSON.stringify(e.candidate_genome_id)}`, '',
		`Verdict: ${verdict.label}`, verdict.explanation, '',
		`Visible correctness: parent ${e.parent_visible_correct}/${e.visible_total}; candidate ${e.candidate_visible_correct}/${e.visible_total}.`,
		s ? `Paired correctness change: ${percentagePoints(s.estimate_bps)}; bootstrap interval [${percentagePoints(s.lower_bps)}, ${percentagePoints(s.upper_bps)}].\nTotal measured cost: parent ${money(s.parent_cost_microusd)}; candidate ${money(s.candidate_cost_microusd)}.\nTotal measured latency: parent ${s.parent_latency_millis} ms; candidate ${s.candidate_latency_millis} ms.\nMeasured gates passed: ${s.metrics_eligible}.` : 'Selection receipt: not recorded.',
		i ? `Invariant checks: ${i.total_checks}; candidate violations: ${i.total_candidate_violations}; paired regressions: ${i.total_paired_regressions}/${i.maximum_regressions} allowed.\nRegression budget satisfied: ${i.regressions_within_budget}. Candidate contract satisfied: ${i.candidate_contract_satisfied}.` : 'Invariant checks: not recorded.',
		`Forge assessment: ${entry.forge_assessment ? `${JSON.stringify(entry.forge_assessment.assessment_id)} (${entry.forge_assessment.outcome})` : 'not recorded'}.`,
		`Linked Champion transitions: ${entry.champion_transition_ids.map(id => JSON.stringify(id)).join(', ') || 'none'}.`, '',
		'This report summarizes daemon evidence; it is not a signed receipt or a current Champion claim. Visible scores exclude sealed tasks; paired selection aggregates may include them. Model cost is reported usage, not a billing statement.',
		'Selection receipts do not carry promotion authority; promotion uses separate verified invariant evidence and a passing Forge assessment.',
		'Run `hephaestus replay` against the same data directory to verify the canonical history. Inspect the World’s Champion history before a rollout.', '',
	].join('\n');
}
