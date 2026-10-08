# Product readiness

The owner’s target is a polished, working Hephaestus product that we would
confidently direct people to and that could support revenue. This target is
**not complete**. Passing a reference demo or polishing the console alone
does not satisfy it.

## Codex and Claude review, 2026-10-07

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

The first implementation slice addresses item 3 and console reliability.
It does not complete items 1 or 2 or establish live-model improvement.

## Acceptance for the full product

| Requirement | Evidence needed | Current status |
|---|---|---|
| Fresh installation and first launch | Source and packaged installation checks; a real six-step terminal tour; understandable setup/recovery messages | Existing implementation; current full acceptance still to run |
| Bring a real task and agent | A documented representative task pack and provider setup; parent and revised prose prompt registered without editing implementation internals | Manual registration exists; guided customer workflow incomplete |
| Fair scoring for the intended task | World-pinned scoring; exact/normalized structured-output cases; malformed-output rejection; same scoring in clusters; receipt replay and mutation coverage | Bundled exact-output evaluator only |
| Evidence-bound prose revisions | Proposal binds before/after content, parent, hypothesis and source evidence; conflicting retries, authority escalation, tampering and restart covered | Missing in this checkout |
| Useful result and export | Browser shows scores, confidence interval, cost, latency, separate gates and agent identities; report matches daemon evidence and contains no sealed payloads or tokens | Implemented in this slice; verification recorded below |
| Useful live model evidence | A dated representative parent/revision Arena comparison using real provider output, measured quality/cost, verified receipts and replay | Not established; fake CLIs cannot substitute |
| Reliability and trust | Workspace tests, TypeScript checks, packaging, docs, mutation anchors and affected mutation guard entries pass; no weakened gates | Full final revision gates still required |
| Clear commercial offer | Honest intended audience, use case, capabilities, limits, support and delivery instructions; claims grounded in representative results | Positioning and delivery work remains |

A positive measured verdict alone never grants promotion. A generated report
is a readable summary, not a signed receipt. Live-model quality cannot be
inferred from deterministic Gauntlet evidence.

## Next implementation work

Implement scoring as a versioned, World-bound policy while preserving old
World identities and receipt replay. Confirm how a new evaluator digest is
registered rather than treating a rebuilt binary as the old evaluator.
Then add bounded operator-authored prose prompt proposals with versioned
replay validation and the same compiler/authority checks as catalog proposals.
Use that workflow for a representative live-provider comparison before
making a product-wide readiness claim.

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
