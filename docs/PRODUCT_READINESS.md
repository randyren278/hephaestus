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
The second adds World-pinned scoring for item 1. Subsequent slices fix hosted
instruction/model delivery and historical replay, then implement and verify
evidence-bound prose revisions and guided authoring for item 2. Representative
live-model improvement and full installation acceptance remain required.

## Acceptance for the full product

| Requirement | Evidence needed | Current status |
|---|---|---|
| Fresh installation and first launch | Source and packaged installation checks; a real six-step terminal tour; understandable setup/recovery messages | Clean source installation and native ARM64 package verified through actual first-launch choice and six-step tours. Package relocation, bundled Node with host Node/npm hidden, offline Arena and restart/replay pass. Native Intel acceptance and downloaded-archive first launch remain |
| Bring a real task and agent | A documented representative task pack and provider setup; parent and revised prose prompt registered without editing implementation internals | Guided hosted revision workflow is wired; packaged offline terminal checks cover recording, restart, live comparison and assessment. The 24-case support-triage pack passes frozen-daemon registration, profile readback and replay. Fresh-user provider acceptance still remains |
| Fair scoring for the intended task | World-pinned scoring; exact/normalized structured-output cases; malformed-output rejection; same scoring in clusters; receipt replay and mutation coverage | Exact, ASCII-trimmed and strict JSON scoring implemented; scoped evaluator, clusters and provider replay checks pass; full workspace tests, all 50 coverage floors and 15 affected mutation checks pass; verification below |
| Evidence-bound prose revisions | Proposal binds before/after content, parent, hypothesis and source evidence; conflicting retries, authority escalation, tampering and restart covered | Operator command and guided console implemented; authenticated recovery, explicit profile confirmation, source preservation and restart verified. Current checks pass all 51 coverage floors and 25 affected mutations; prior revision checks killed 30 mutations |
| Useful result and export | Browser shows scores, confidence interval, cost, latency, separate gates and agent identities; report matches daemon evidence and contains no sealed payloads or tokens | Implemented in this slice; verification recorded below |
| Useful live model evidence | A dated representative parent/revision Arena comparison using real provider output, measured quality/cost, verified receipts and replay | Not established; fake CLIs cannot substitute |
| Reliability and trust | Workspace tests, TypeScript checks, packaging, docs, mutation anchors and affected mutation guard entries pass; no weakened gates | Current Node 22 checks pass 191 TUI and 40 web tests; the current Rust workspace run passes 715 tests including doc tests, 47 Python tests, and both Clippy modes pass. All 428 mutations pass at immutable checkpoint 709d965; all 45 affected guards for the subsequent minimum-compiler edits also pass. Earlier coverage passed all 51 floors. Full product acceptance remains |
| Clear commercial offer | Honest intended audience, use case, capabilities, limits, support and delivery instructions; claims grounded in representative results | Engineer audience, one-workflow pilot, evidence deliverables, local delivery and support boundaries are documented in the pilot guide. The assisted paid pilot is a proposed scope; representative model results and an available paid offer remain |

A positive measured verdict alone never grants promotion. A generated report
is a readable summary, not a signed receipt. Live-model quality cannot be
inferred from deterministic Gauntlet evidence.

## Next implementation work

Verified hosted Genome instructions now reach direct, submitted and paired execution. Contract v2 preserves exact instruction bytes separately from task commitments, pins the requested model, and retains v1 history replay. The offline fixture verifies different role instructions and models through the actual stdin pipe and reopens the saved control plane. Real old/new daemon upgrade evidence is recorded below. This proves configuration delivery, not live-model quality.

The operator-authored prose proposal path and guided revision flow now pass
coverage, mutation and real daemon upgrade checks. Continue with fresh
installation and onboarding acceptance, representative live-provider evidence
within authorized usage, and commercial positioning before claiming product
readiness.

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

## Forge revision work in progress, 2026-10-08

The authenticated operator command `genome revise` records ordinary UTF-8 prompt
revisions for registered Codex and Claude agents. It keeps the model, authority,
World and other artifacts, creates a child through the ordinary compiler, and
binds the hypothesis and exact before/after prompt bytes to a verified source
Selection. Catalog proposals retain schema 1; prose revisions use strict schema 2
under the shared proposal-ID namespace.

Current scoped evidence: six revision integration/contract tests pass and
all-feature workspace clippy passes. The scripted provider checks exact Unicode,
CRLF and trailing-space instruction bytes plus the model flag through stdin.
It produces signed paired evidence for a revised child, then exercises
assessment, invariant verification, Champion promotion and restart. Semantic
tamper checks rebuild valid ledger hash chains before reopening the control
plane; unrelated model and artifact changes are rejected even when their child
Genomes compile. Conflicting retries and cross-kind ID conflicts reject.

Claude reviewed the implementation read-only through the installed CLI. Its
review led to earlier idempotent retry handling, shared event-index reuse on
cold replay, and reservation of automatic proposal IDs. The bounded source
reader now opens once, checks metadata on that handle and limits the stream
read; FIFO and underestimated-size checks passed in the full workspace run.

This is an implementation checkpoint. The full workspace tests and real old/new upgrade plus a second restart now
pass, with schema-1 catalog bytes preserved. See the
[Forge upgrade evidence](evidence/2026-10-08-forge/README.md). All 51 critical coverage floors now pass, including the new revision module,
and both default and all-feature workspace clippy pass. All 30 affected mutation
checks were killed, with no survivors, stale entries or timeouts. The restored
source rebuilt successfully and repeated the actual old/new upgrade and second
restart. Guided authoring and representative live-model quality evidence remain
required. This does not establish full product readiness or revenue.

The shared terminal editor handoff now supports flags and quoted commands,
reports failures on the source-path step and preserves the file for retry.
Actual packaged-console PTY checks type Unicode into the editor, verify no Ink
output during its ownership and verify terminal modes after success, failure and
exit. The existing full registration/comparison fixture passes and now includes
the packaged-editor checks in CI. See the
[editor evidence](evidence/2026-10-08-editor/README.md). Private draft and recovery
storage now supports the guided Forge screens described below.


## Guided revision validation checkpoint, 2026-10-08

`Evidence & Costs → Forge: revise a prompt` now guides an existing hosted
candidate from its canonical source Selection through an editor draft,
explicit hypothesis, immutable proposal, separately confirmed comparison,
and metrics assessment. A failed source candidate remains valid revision
evidence. Models, authority, World and other artifacts remain fixed by the
proposal contract. Original Markdown files are preserved.

The profile exposes model, authority, scoring, task counts and per-trial/total
reported cost and wall limits. Confirmation requires the complete ASCII model
identifier and at least an 80×22 terminal. The daemon validates the displayed
profile and both roles' settings atomically before a new comparison. Existing
IDs return their original admission across an upgrade. Hosted paired trials
use the five-minute provider deadline; reference trials and protected scoring
retain their ten-second bounds, and aggregate work must fit within one day.
Reported costs do not guarantee provider billing or subscription quota limits.

Private content snapshots and recovery cursors persist before write requests.
Unknown outcomes retry saved identities, recovery verifies canonical proposal
and receipt bindings, and background progress reads only the already-bound
job. Cancellation uses that job's directed identity without waiting for prompt
reads. Polling cannot replace the displayed attempt in a cancellation prompt;
a changed saved attempt requires explicit reconciliation. Successful comparisons
cannot be rerun in this guided flow. Assessment
never performs invariant verification or promotion.

The packaged 80×24 offline terminal check exercised real editor input,
Unicode/CRLF snapshot preservation, a restart after recording, another restart
during the live comparison, one canonical proposal/evaluation/assessment,
source preservation and an unchanged Champion. A full Rust workspace coverage
run passed 711 tests and all 51 critical coverage floors. All 185 UI tests pass
on both the development runtime and Node 22; all 44 Python tests pass. Both
Clippy modes, typechecking, packaging, formatting and documentation checks pass.
All 25 affected mutations were killed, with no survivors, stale entries or
timeouts. The restored source was rebuilt before repeating the terminal checks
and reopening actual historical catalog-v1/prose-v2 history, including its
ten-second admissions. See the [guided revision evidence](evidence/2026-10-08-guided/README.md).
Actual Claude Code reviewed these changes and its recovery,
confirmation, parent-budget and terminal-visibility findings were incorporated.
Representative live-model quality, fresh installation acceptance and the
commercial delivery audit still remain; this checkpoint does not complete the
full product goal.

## Packaged installation checkpoint, 2026-10-08

The ARM64 archive now includes `heph` and passes installation and relocation
in an isolated home. The real first-launch choice and all six tour steps passed
at 80×24 with host Node/npm hidden and no evaluator override. The tour displayed
parent 0/1 → candidate 1/1, verified metrics Selection and replay, then restored
the terminal. Offline Arena and both bundled Gauntlets also passed, including
the reference Gauntlet's daemon restart and signed-receipt replay. See the
[installed package evidence](evidence/2026-10-08-install/README.md).

Tour registration now carries the fixture's exact returned identities instead
of choosing arbitrary registered agents. Full identity hashes distinguish
directed runs and comparisons. Selection failures preserve the visible score
and offer retry; substituted evidence refuses. All 189 UI checks pass on the
development runtime and Node 22, and nine package plus seven source-installer
contracts pass. Source contracts use fake build tools; a subsequent actual
clean source installation from detached commit `5c2e5f4`, a fresh Cargo target
directory and a new install prefix also passes its six-step first-launch tour
under Node 22. Native Intel acceptance, downloaded-archive first launch and
hosted release-workflow acceptance remain; this checkpoint establishes neither
representative live-model improvement nor the commercial offer.

## Release gate checkpoint, 2026-10-08

Both native package builds now require installed acceptance before archive
upload or signing, and ordinary pull-request/main CI includes the ARM64 check.
Pinned target-specific Rust inventory generation runs separately from accepted
archives. Signing waits for builds, inventories and reproducibility and uses
pinned Cosign bundles; signing, attestation and publication require a tag push
whose version matches the workspace. Manual rehearsals do not sign or publish.
Verification constrains the repository, workflow and version tag.

Actionlint, actual shell failure/tag checks, both target inventories and real
upstream Cosign bundle/identity verification passed locally. Claude identified
and reviewed the trust gaps addressed here. See the
[release gate evidence](evidence/2026-10-08-release/README.md). A hosted run is
still needed for native Intel acceptance, actual Hephaestus signing and release
delivery; local syntax checks do not establish those results.

## Representative task preparation, 2026-10-08

The [support-triage pack](../examples/support-triage/README.md) defines strict
JSON routing and priority for 24 fictional tickets. Its baseline and checklist
prompts use the same policy, model and authority. Both manifests and Genomes
registered in a fresh frozen production daemon; profile readbacks and replay
passed without any provider launch. Claude checked all labels and found no
conflicts. The [preparation evidence](evidence/2026-10-08-support-triage/README.md)
distinguishes this from model availability, authentication and actual quality.
Real outputs, authorized usage and representative measured results remain open.

## Compiler, cost and pilot checkpoint, 2026-10-08

The advertised Rust 1.85 minimum failed against the locked dependencies. Rust
1.88 passes an explicit Cargo/rustc-pinned all-target/all-feature check, and a
real source installation at the prepared checkpoint passes the first-launch
choice and six-step tour. Current-source syntax edits keep both stable Clippy
modes green without weakening the rules. See the
[minimum compiler evidence](evidence/2026-10-08-msrv/README.md).

Runs, Costs, browser comparisons and downloaded reports now share six-decimal
recorded-USD formatting. Visible disclosures explain that hosted zero can mean
missing reporting, Codex reports no USD, and totals can omit provider usage.
The real 80×24 terminal retains all three disclosure lines. The source tests
cover the selected row in a compact 60×12 slot and a full 80-column panel.
The current native ARM64 archive passes isolated installation, relocation,
the first-launch tour, offline Arena, both Gauntlets and replay with host
Node/npm hidden. The actual browser download matches daemon identities,
visible scores, costs and latency; its mobile comparison has no page overflow.
See the [cost and delivery evidence](evidence/2026-10-08-cost-disclosure/README.md).

The [pilot guide](PILOT_GUIDE.md) names the intended engineer audience, concrete
deliverables, private holdout expectations, provider allowance, report controls
and support boundaries. It describes an assisted paid pilot as a proposed
scope, rather than an active paid plan. The support pack's durable private
directory and Git initialization also pass with global commit signing enabled.

The [complete mutation checkpoint](evidence/2026-10-08-full-mutations/README.md)
killed all 428 mutations at `709d965`, then rebuilt binaries and verified a
clean checkout. Subsequent source edits have their own affected-guard checks.
Claude identified a pre-existing ledger hex-decoding edge case: a non-ASCII
trailing character can be dropped by the character-pair decoder. It cannot
forge a signed event, but strict malformed-record rejection still needs a
focused repair and regression test before the full trust audit closes.

Representative live-provider output, current hosted release/Intel acceptance,
downloaded-package first launch and the remaining trust audit are still open.
The requested provider comparison remains pending explicit quota/auth-handoff
authorization; preparation alone does not prove model availability or quality.
