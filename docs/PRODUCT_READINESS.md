# Product readiness

The owner’s target is a polished, working Hephaestus product that we would
confidently direct people to and that could support revenue. This target is
**not complete**. Passing a reference demo or polishing the console alone
does not satisfy it.

## Codex and Claude review, 2026-10-07 through 2026-10-08

Codex requested a read-only review through the installed Claude Code CLI.
Computer Use refused access to Ghostty, so the existing terminal session
was not operated. Claude inspected current code and identified three
customer-value gaps; Codex confirmed them against the checkout:

1. The bundled evaluator requires byte-exact output. Real provider responses
   need a task-appropriate scoring contract, with the policy pinned to the
   World and used consistently in evaluation and failure-cluster analysis.
2. Forge rejects ordinary prose prompts. An operator needs to record a
   proposed prompt revision, compare it, and assess it through the ordinary
   evidence chain without changing authority, Laws or scoring.
3. The browser showed evaluation costs without a useful comparison verdict.
   Customers need scores, uncertainty, cost, latency, the remaining gates,
   and a report they can review alongside exact agent identities.

The first implementation slice addressed item 3 and console reliability.
The second adds World-pinned scoring for item 1. The third fixes hosted instruction/model delivery and historical replay, a prerequisite for item 2. Item 2 and representative
live-model improvement remain required.

## Acceptance for the full product

| Requirement | Evidence needed | Current status |
|---|---|---|
| Fresh installation and first launch | Source and packaged installation checks; a real six-step terminal tour; understandable setup/recovery messages | Existing implementation; current full acceptance still to run |
| Bring a real task and agent | A documented representative task pack and provider setup; parent and revised prose prompt registered without editing implementation internals | Manual registration and provider instruction/model delivery verified with offline CLI fixtures; guided workflow incomplete |
| Fair scoring for the intended task | World-pinned scoring; exact/normalized structured-output cases; malformed-output rejection; same scoring in clusters; receipt replay and mutation coverage | Exact, ASCII-trimmed and strict JSON scoring implemented; scoped evaluator, clusters and provider replay checks pass; full workspace tests, all 50 coverage floors and 15 affected mutation checks pass; verification below |
| Evidence-bound prose revisions | Proposal binds before/after content, parent, hypothesis and source evidence; conflicting retries, authority escalation, tampering and restart covered | Missing in this checkout |
| Useful result and export | Browser shows scores, confidence interval, cost, latency, separate gates and agent identities; report matches daemon evidence and contains no sealed payloads or tokens | Implemented in this slice; verification recorded below |
| Useful live model evidence | A dated representative parent/revision Arena comparison using real provider output, measured quality/cost, verified receipts and replay | Not established; fake CLIs cannot substitute |
| Reliability and trust | Workspace tests, TypeScript checks, packaging, docs, mutation anchors and affected mutation guard entries pass; no weakened gates | Full final revision gates still required |
| Clear commercial offer | Honest intended audience, use case, capabilities, limits, support and delivery instructions; claims grounded in representative results | Positioning and delivery work remains |

A positive measured verdict alone never grants promotion. A generated report
is a readable summary, not a signed receipt. Live-model quality cannot be
inferred from deterministic Gauntlet evidence.

## Next implementation work

Verified hosted Genome instructions now reach direct, submitted and paired execution. Contract v2 preserves exact instruction bytes separately from task commitments, pins the requested model, and retains v1 history replay. The offline fixture verifies different role instructions and models through the actual stdin pipe and reopens the saved control plane. Real old/new daemon upgrade evidence is recorded below. This proves configuration delivery, not live-model quality.

Next add bounded operator-authored prose prompt proposals with versioned replay validation and the same compiler/authority checks as catalog proposals. Then use that workflow for a representative live-provider comparison before claiming product readiness. Guided onboarding, commercial positioning and full final acceptance remain required.

## Verification of the comparison slice

Verified on macOS, 2026-10-07, against a new scratch daemon and the built
browser bundle. All agents and tasks in this evidence are deterministic;
no hosted-model quality or revenue result is claimed.

- Web typecheck, bundle build and **38/38 tests passed**, including execution
  of the actual browser entry point for token expiry, same-tab fragment
  reconnection, daemon-authentication failures and an older in-flight 401.
- Shared terminal client checks: **139/139 TUI tests** and typecheck passed.
  Python research checks: **44/44**; source-install contracts: **7/7**;
  macOS-package contracts: **8/8**. Workspace binaries built successfully.
- Real Arena evidence: parent visible correctness **0/1**, candidate **1/1**;
  paired improvement **+100 percentage points**, measured gates passed;
  invariant verification and promotion remained false. A deliberate
  regression scored **1/1 → 0/1**, failed selection and recorded a rejected
  Forge assessment. A third evaluation with no selection rendered pending.
  All three were read back in the browser; **123 events** replayed successfully
  before the final inspection requests.
- The [downloaded report](evidence/2026-10-07-comparison/comparison-report.md)
  was compared with the daemon’s
  [evaluation summaries](evidence/2026-10-07-comparison/evaluation-summaries.json):
  scores, agent identities and total latency matched.
- Browser reload preserved the session and removed the token from the URL.
  Stopping the console showed a connection error. Restarting on the same
  port returned a real HTTP 401 and cleared the old token. Opening the fresh
  printed URL in the same tab reconnected and refreshed the current view.
  A live foreign-Origin request returned HTTP 403.
- [Desktop](evidence/2026-10-07-comparison/desktop.png) and
  [mobile](evidence/2026-10-07-comparison/mobile.png) layouts were inspected;
  the 390-pixel viewport had no page-level horizontal overflow. World accents
  rendered under the unchanged strict CSP and agent links are keyboard buttons.
- Claude reviewed the implementation, caught the browser/daemon token-error
  distinction, and confirmed the session, verdict, latency and report-escaping
  corrections. Browser verification then exposed the fragment-only
  reconnection case; it was corrected and given an entry-point regression test.
- The documentation gate, mutation-source anchors and whitespace check passed.
  Full workspace coverage, all mutation shards, clean packaged tour acceptance
  and live-provider Arena evidence remain part of the final product gate;
  they are not implied by these scoped results.

## Verification of World-bound scoring

Verified on macOS, 2026-10-08. These checks establish deterministic scoring
and provider-adapter behavior; the provider CLIs in the tests are fixtures.
They do not establish hosted-model quality or the effect of prose revisions.

- Exact defaults preserve the historical canonical World bytes. Both
  non-default modes have distinct identities and round-trip through compilation.
  Golden requests pin legacy schema 1 and both schema-2 policy spellings.
- Isolated evaluator tests cover ASCII trimming, JSON key ordering, exact
  decimal equivalence without floating-point rounding, malformed JSON,
  duplicate decoded keys, reliability and bounded nesting/numeric magnitude.
  Evaluation, visible/sealed failure counts and receipt replay agree.
- Fake hosted-provider jobs complete through the guarded runtime for both
  normalized modes, cluster with zero false failures and reopen from canonical
  history. Invalid expected JSON fails before admission. The library's prepared
  evaluation path also rejects it without writing evaluator artifacts.
- Full workspace tests with all features passed under coverage. Every one of
  **50 critical modules** met the **92%** floor; scoring reached **98.4%**,
  evaluator protocol **98.2%**, World compilation **100%** and failure clusters
  **97.2%**. The full uninstrumented mutation baseline also passed.
- **15 affected mutations ran: 15 killed, 0 survived, 0 stale, 0 timed out.**
  This includes 13 new invariants and the two updated reliability/privacy
  anchors. The configured matrix now contains 381 entries; this scoped run
  does not imply a fresh pass of every matrix shard.
- Python research/check tooling: **44/44 passed**. Default and all-feature lints, rebuilt workspace binaries, mutation
  source anchors, documentation checks and whitespace checks passed.
- Claude reviewed the comparison, World binding, request versions and replay;
  its decimal-boundary finding was fixed by bounding normalized magnitude.
  Its subsequent provider review found the instruction-delivery prerequisite
  described above, which Codex confirmed in the current source.

## Verification of hosted instruction and model delivery

Verified on macOS, 2026-10-08, using offline provider fixtures. Hosted quality and revenue remain unproven.

- Both adapters frame exact verified Genome instruction bytes separately from the committed task input and pass the requested model as one CLI argument. The control fixture checks per-role golden stdin bytes and distinct requested models through direct, submitted and paired execution. Parent scores 0/1 and candidate 1/1; those fixture scores establish delivery, not real-model improvement.
- Historical provider-v1 records and free-form model labels remain replayable. New registrations reject malformed model identifiers; direct, submitted and paired launches of old invalid labels fail before admission. Compiler and reopened-control-plane regression tests cover this boundary.
- The real previous binary at `7bf55d0` created v1 history; the new daemon reopened the same data, completed v2 jobs/evaluations, and replayed **107 events**. [Upgrade readbacks and reproduction](evidence/2026-10-08-provider/README.md) include binary hashes and both Arena environment versions. Canonical World/Genome bytes and signed receipts were preserved.
- Full workspace all-feature tests and all **50 critical coverage floors** passed. Supplemental instrumented tests cover the final historical-label cases. Provider invocation coverage is **100%**, run specs **96.7%**, and Genome compilation **100%**. The full uninstrumented mutation baseline also passed.
- **21 new mutations ran: 21 killed, 0 survived, 0 stale, 0 timed out.** [Mutation results](evidence/2026-10-08-provider/mutation-results.txt) record the commands and verdicts. The configured matrix is now **402** entries; this does not establish a fresh pass of all matrix shards.
- Python checks: **44/44 passed**. Default and all-feature Clippy, documentation, formatting and mutation-source anchors passed. Workspace binaries are rebuilt from restored source after the mutation run.
- Actual Claude Code reviewed configuration delivery, role pairing, replay and migration evidence. Its compatibility-test findings were incorporated. Prose Forge proposals, guided acceptance, representative live evidence and commercial positioning remain required.
