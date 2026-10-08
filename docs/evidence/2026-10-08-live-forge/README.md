# Real evidence-bound Forge comparison, 2026-10-08

The owner approved “48 more calls” for the
[registered policy-reference revision](../2026-10-08-live-codex/followup-plan.md).
Exactly one second evaluation completed:
`support-triage-forge-live-2026-10-08-002`. Its
[proof](proof.json) and [48 signed success receipts](signed-run-receipts.json)
record the additional allowance and exactly **96 total experimental Codex
calls across both evaluations**. No further experiment was authorized.

This compares the original evaluated checklist with its registered Forge
child. Both retain the same installed binaries, requested `gpt-6.1-sol`,
World, authority, task source, strict JSON scoring, seed and budgets.
Receipt pairs have identical task commitments, source revision and
provider-v2 environment. The
[proposal recovered after restart](proposal-after-restart.json) preserves
the original Selection hash, exact before/after prompt artifacts and hypothesis.

## Result and decision

| Measure | Checklist parent | Policy-reference child |
|---|---:|---:|
| Protected aggregate correctness | 24 / 24 | 24 / 24 |
| Visible correctness | 12 / 12 | 12 / 12 |
| Successful runs | 24 / 24 | 24 / 24 |
| Total signed latency | 136,720 ms | 130,974 ms |
| Median signed latency | 5,146.5 ms | 5,199.5 ms |
| Provider-reported USD | Unavailable | Unavailable |

The [Selection](selection.json) records zero regressions and improvements,
24 unchanged pairs, and a [0, 0] basis-point bootstrap interval on this sample.
The primary zero-regression hypothesis is not refuted; **no quality improvement
or broad non-inferiority claim is established**.

The child's lower total latency does not satisfy the secondary hypothesis:
its median is higher, with 11 faster and 13 slower task pairs. The
[latency analysis](latency-analysis.json) preserves both measures. Parent
trials precede child trials, confounding timing drift with role. This is not
a causal or repeatable speed improvement.

The candidate Pareto flag is true, but measured eligibility is **false**.
The pinned selection rule requires the lower confidence bound to exceed the
minimum correctness delta; both are zero here. The authenticated
[Forge assessment](assessment.json) records **`metrics_rejected`**. Invariant
verification and promotion eligibility remain false, with no Champion
transition. A shorter prompt does not override those gates.

## Verified delivery of evidence

The actual browser [downloaded report](comparison-report.md) matches daemon
identities, visible scores, signed latency totals, Forge rejection and cost
disclosures. [Desktop](comparison-desktop.png) and
[mobile](comparison-mobile.png) screenshots show the second comparison after
restart. The [readback](browser-readback.json) confirms two comparisons and a
removed token fragment; the [390-pixel mobile readback](browser-mobile-readback.json)
has no page overflow.

Graceful matching-daemon restart preserved both evaluations, all
[96 completed runs](completed-runs-after-restart.json), the exact proposal and
its assessment. Canonical [replay](replay-after-restart.json) verified 5,312
events, frozen, with no active work. Subsequent ordinary inspection requests
can increase the event count; no additional provider experiment was submitted.

Both comparisons use the unchanged initial installation. The independently
verified [token-redaction correction](../2026-10-08-usage-redaction/README.md)
does not alter these historical records or installed binaries. All 48 new
[receipt-linked cost observations](provider-usage-limit.json) retain masked
token counters and unavailable USD. Recorded zero is not measured free usage,
an invoice or a subscription-quota bound.

This establishes the real source-Selection → prompt revision → protected
comparison → rejected Forge assessment → report → restart/replay path.
The public fictional pack is not private holdout or independent customer
acceptance. Attributable USD cost, native Intel/download acceptance and an
available commercial offer remain open.

Claude’s [actual read-only review](claude-review.txt) identified an overly broad
verdict explanation naming every metric. The current source uses the World’s
measured selection requirements without claiming that each metric failed.
The [new actual browser download](current-template-report.md) preserves every
original report value and changes only that sentence; the
[current-source readback](current-template-browser-readback.json) checks both
comparisons. The original installation’s report above is retained unchanged.
