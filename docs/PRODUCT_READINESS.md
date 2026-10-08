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
The second adds World-pinned scoring for item 1. Item 2 and representative
live-model improvement remain required.

## Acceptance for the full product

| Requirement | Evidence needed | Current status |
|---|---|---|
| Fresh installation and first launch | Source and packaged installation checks; a real six-step terminal tour; understandable setup/recovery messages | Existing implementation; current full acceptance still to run |
| Bring a real task and agent | A documented representative task pack and provider setup; parent and revised prose prompt registered without editing implementation internals | Manual registration exists; registered prose instructions are not yet delivered to hosted providers; guided workflow incomplete |
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

Claude’s second review identified a prerequisite for useful prose revisions;
Codex confirmed it against the current provider launch paths. Paired provider
specs use the task input alone, and direct provider specs use the fixed
inventory task. Both adapters send that spec prompt to stdin; neither path
incorporates the registered `artifacts["agent.prompt"]` bytes. Paired admission also
unconditionally parses both prompts as reference instructions, rejecting
ordinary hosted-provider prose before execution. Changing a hosted agent’s
prompt currently cannot establish a causal improvement. Runtime documentation now records this limitation until the delivery fix
is verified.

Next, wire verified Genome instructions into
provider stdin while retaining the separate task-input commitment. Bind the
new instruction-delivery contract into provider environment identities and
prove through fake CLIs that two prompt artifacts produce different received
instructions. Keep the instruction separate from `RunSpec::prompt()`, whose
bytes must still hash to the task input commitment. Version the common
provider environment, not each prompt: differing parent/candidate instructions
remain comparable, with their exact artifacts bound through Genome identity.
The replay validators in the control state and run records currently recognize
provider-v1 identities; extend them for the new contract while retaining
legacy acceptance before admitting new-format runs. Preserve historical
replay. Then add bounded operator-authored
prose prompt proposals with versioned replay validation and the same
compiler/authority checks as catalog proposals. The current provider argument
builders also omit the registered model family; pin the requested family
before attributing a live result to a particular agent configuration. Use
that workflow for a representative live-provider comparison before claiming product readiness.

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
