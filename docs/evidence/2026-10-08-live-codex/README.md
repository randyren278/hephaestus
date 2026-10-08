# First real Codex comparison, 2026-10-08

The owner authorized this first 48-call comparison and the existing private
ChatGPT authentication-file handoff with “send it.” The source installation
completed exactly one evaluation, `support-triage-live-2026-10-08-001`, with
**48 unique successful provider runs: 24 per prompt**. No API key was used.
The [proof](proof.json) records Codex CLI 0.161.0, its executable digest,
the requested configured model identifier `gpt-6.1-sol`, installed binary
digests and the separate task-source Git revision. This model identifier is
not a dated model snapshot. User configuration was ignored by the adapter.

## Observed result

| Measure | Baseline | Checklist |
|---|---:|---:|
| Protected aggregate correctness | 24 / 24 | 24 / 24 |
| Visible correctness in the browser | 12 / 12 | 12 / 12 |
| Successful runs | 24 / 24 | 24 / 24 |
| Total measured latency | 117,865 ms | 120,307 ms |
| Provider-reported USD | Unavailable | Unavailable |

The [canonical Selection](selection.json) records 24 unchanged pairs, zero
improvements and zero regressions. Its bootstrap interval is [0, 0] basis
points on this observed sample. That does not establish equivalence,
generalization or a zero-uncertainty population result. The checklist's total
latency was 2,442 ms higher in this execution; one run does not isolate prompt
overhead from model/service variability or demonstrate a repeatable effect.
No quality improvement is established.

Measured eligibility, invariant verification and promotion eligibility are all
**false**. There is no Forge assessment or Champion transition for this
evaluation. The tool correctly retains a tie that did not pass its gates.

The [profiles](profiles.json) share the World, provider/model, strict JSON
scoring, read-only source authority and task counts. Prompts differ. The source
repository excludes the task files and expected answers. The pack's “sealed”
cases are publicly available fictional examples, not a confidential holdout.
This run is not independent customer acceptance, broad model evidence or a
commercial outcome.

## Evidence and restart

The [48 signed run-result envelopes](signed-run-receipts.json) link exact
source revision, task commitments and provider-v2 environment. Signed claim
latency totals match [the completed-run readback](completed-runs.json) and
Selection. The matching installed daemon verified canonical history through
[replay](replay-after-restart.json): 2,517 events, frozen, no active runs.
After graceful shutdown and restart, evaluation summaries and the complete
48-run list were identical. Ordinary freeze/read/stop events increase the
ledger count and projection; no extra provider work occurred.

The actual browser [downloaded report](comparison-report.md) was checked
against [daemon summaries](evaluations.json), including identities, scores,
latencies and separate gates. [Desktop](comparison-desktop.png) and
[mobile](comparison-mobile.png) screenshots show the real results. The
[mobile readback](browser-readback.json), after restart and reconnection,
confirms one comparison, a removed token fragment and a 390-pixel page at a
390-pixel viewport. Report downloads are summaries, not signed receipts.

The daemon uses the unchanged source-installed implementation at `ebbb21b`
with host Node 22.22.2. This is not packaged/downloaded-release acceptance.
The durable private pilot remains available locally; credentials, session
tokens, private signing keys, raw databases and sandbox contents are not
included in this export.

## Usage-reporting defect and limits

Codex emits no per-run USD amount. Saved run receipts contain numeric zero,
which is unavailable reporting, not measured free usage or a billing cap.
The initial installed redaction policy also masked all four numeric token
counters because their names contain `token`. The
[receipt-linked usage record](provider-usage-limit.json) preserves that
absence for every run. No counters, USD or quota consumption were reconstructed.

The separately verified [redaction correction](../2026-10-08-usage-redaction/README.md)
applies to new cost traces only. It does not alter this pilot's saved evidence
or installed binaries. Historical artifacts remain unchanged.

[Claude's first actual read-only review](claude-initial-review.txt) confirmed
the code defect and proposed a shorter prompt, but explicitly could not read
the external private pilot files. Its comments on the live result were
conditional. A subsequent review of this safe in-repository export is recorded
in the [export review](claude-review.txt); it confirmed the counts and limits
and found no blocking defect in the scoped patch. Codex independently
recomputed the [paired latency summary](latency-analysis.json).

The first allowance is exhausted. A
[concrete evidence-bound Forge follow-up](followup-plan.md) is registered,
with matching profiles and 48 runs visible while frozen at registration.
The owner subsequently approved its separate 48-call allowance; it was
submitted once and completed. The [second comparison record](../2026-10-08-live-forge/README.md)
retains its result separately from the first comparison.
