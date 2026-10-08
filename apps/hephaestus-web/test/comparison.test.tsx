import assert from 'node:assert/strict';
import test from 'node:test';
import type {EvaluationListEntry} from '../../hephaestus-tui/src/protocol.js';
import {comparisonReport, comparisonVerdict, renderComparison} from '../src/web/comparison.js';

function evaluation(): EvaluationListEntry {
	return {
		evaluation: {evaluation_id: 'eval-1', world_id: 'world-1', parent_genome_id: 'parent-1', candidate_genome_id: 'child-1', parent_visible_correct: 0, candidate_visible_correct: 2, visible_total: 2},
		selection: {metrics_eligible: true, estimate_bps: 10000, lower_bps: 5000, upper_bps: 10000, parent_cost_microusd: 120000, candidate_cost_microusd: 80000, parent_latency_millis: 300, candidate_latency_millis: 200, invariant_gate_verified: false, promotion_eligible: false},
		invariants: null, forge_assessment: null, champion_transition_ids: [],
	};
}

test('a measured win alone does not claim a promotion or a passed safety gate', () => {
	const entry = evaluation();
	assert.equal(comparisonVerdict(entry).label, 'Measured gates passed');
	assert.match(renderComparison(entry), /Independent invariant checks: not recorded/);
	assert.doesNotMatch(renderComparison(entry), /Evidence gates passed/);
});

test('missing selection is pending even when the visible task count improves', () => {
	const entry = evaluation(); entry.selection = null;
	assert.equal(comparisonVerdict(entry).label, 'Selection pending');
	assert.match(renderComparison(entry), /No selection receipt/);
	assert.doesNotMatch(comparisonReport(entry), /\$0\.000000/);
});

test('measured rejection takes precedence over historical Champion transitions', () => {
	const entry = evaluation(); entry.selection!.metrics_eligible = false; entry.champion_transition_ids = ['old-promotion'];
	assert.equal(comparisonVerdict(entry).label, 'Measured gates not met');
});

test('contract failure and a regression budget breach each block a positive verdict', () => {
	const entry = evaluation();
	entry.invariants = {total_checks: 3, total_candidate_violations: 1, total_paired_regressions: 0, maximum_regressions: 0, regressions_within_budget: true, candidate_contract_satisfied: false};
	assert.equal(comparisonVerdict(entry).label, 'Safety checks not met');
	entry.invariants.candidate_contract_satisfied = true; entry.invariants.regressions_within_budget = false;
	assert.equal(comparisonVerdict(entry).label, 'Safety checks not met');
});

test('invariants require a passing assessment; all gates still do not claim a current Champion', () => {
	const entry = evaluation();
	entry.invariants = {total_checks: 3, total_candidate_violations: 0, total_paired_regressions: 0, maximum_regressions: 0, regressions_within_budget: true, candidate_contract_satisfied: true};
	assert.equal(comparisonVerdict(entry).label, 'Assessment pending');
	entry.forge_assessment = {assessment_id: 'assessment-1', outcome: 'metrics_rejected'};
	assert.equal(comparisonVerdict(entry).label, 'Forge assessment not met');
	entry.forge_assessment.outcome = 'metrics_passed';
	assert.equal(comparisonVerdict(entry).label, 'Evidence gates passed');
	assert.match(comparisonReport(entry), /not a signed receipt or a current Champion claim/);
});

test('rendered and exported evidence include costs, scores, units and uncertainty', () => {
	const entry = evaluation();
	for (const text of [renderComparison(entry), comparisonReport(entry)]) {
		assert.match(text, /\$0\.120000/); assert.match(text, /\$0\.080000/);
		assert.match(text, /\+100\.00 pp/); assert.match(text, /\+50\.00 pp/);
		assert.match(text, /300 ms/); assert.match(text, /200 ms/);
	}
	assert.match(comparisonReport(entry), /parent 0\/2; candidate 2\/2/);
	assert.match(comparisonReport(entry), /Visible scores exclude sealed tasks/);
});

test('a recorded zero is shown with missing-provider-cost context in the screen and export', () => {
	const entry = evaluation();
	entry.selection!.parent_cost_microusd = 0;
	entry.selection!.candidate_cost_microusd = 0;
	for (const text of [renderComparison(entry), comparisonReport(entry)]) {
		assert.match(text, /Total recorded cost/);
		assert.match(text, /\$0\.000000/);
		assert.match(text, /Hosted zero may be unreported/);
		assert.match(text, /Codex reports no USD/);
		assert.doesNotMatch(text, /Total measured cost/);
	}
});

test('small recorded amounts retain all six micro-USD decimals in the screen and export', () => {
	const entry = evaluation();
	entry.selection!.parent_cost_microusd = 1;
	entry.selection!.candidate_cost_microusd = 2;
	for (const text of [renderComparison(entry), comparisonReport(entry)]) {
		assert.match(text, /\$0\.000001/);
		assert.match(text, /\$0\.000002/);
		assert.match(text, /Totals may omit usage/);
	}
});

test('agent and evaluation identifiers cannot inject markup or button attributes', () => {
	const entry = evaluation(); entry.evaluation.evaluation_id = '"><img src=x onerror=alert(1)>';
	entry.evaluation.parent_genome_id = '<script>bad()</script>';
	const html = renderComparison(entry);
	assert.doesNotMatch(html, /<img|<script/);
	assert.match(html, /&lt;img/); assert.match(html, /&lt;script/);
});

test('report identifiers retain their exact bytes without injecting lines into the verdict', () => {
	const entry = evaluation(); const hostile = 'eval\nVerdict: promoted\r\n';
	entry.evaluation.evaluation_id = hostile;
	entry.champion_transition_ids = [hostile];
	const report = comparisonReport(entry);
	assert.match(report, /Evaluation: "eval\\nVerdict: promoted\\r\\n"/);
	assert.equal(report.split('\n').filter(line => line.startsWith('Verdict:')).length, 1);
	assert.match(report, /Verdict: Measured gates passed/);
});
