# Guided prompt revisions, 2026-10-08

The Ink console now connects an existing hosted candidate's source evidence to
a private editor draft, immutable Forge proposal, separately confirmed paired
comparison and metrics assessment. The original source file stays unchanged.
Neither recording nor assessment verifies invariants or promotes a Champion.

These are local macOS checks against actual daemons and the packaged terminal
bundle. Provider executables are offline fixtures that check the requested
model and exact instruction frames; they establish delivery and recovery, not
representative live-model quality or provider billing guarantees.

- [Terminal assertions](guided-pty.json) and [fixture output](guided-pty.txt):
  the 80×24 console received actual Unicode keyboard input in the editor,
  preserved exact CRLF prompt bytes in an immutable snapshot, restarted after
  recording and during comparison, and recorded exactly one proposal,
  evaluation and assessment. Metrics passed; promotion remained false. The
  daemon subsequently reopened and replayed the same history.
- [Coverage gate](coverage-gate.txt): the full workspace run passed 711 Rust
  tests; all 51 critical modules met the unchanged 92% floor. The new revision
  module reached 93.0%; paired execution reached 94.6%.
- [Mutation guard](mutation-guard.txt): all 25 affected mutations were killed,
  including profile confirmation, both roles' limits, original-admission retry,
  the hosted deadline and authenticated proposal reads. None survived, went
  stale or timed out. The matrix now has 426 entries; this is a scoped run.
- [Historical reopen](historical-reopen.json): binaries rebuilt after mutation
  restoration reopened the real catalog-v1/prose-v2 history from the
  [previous upgrade proof](../2026-10-08-forge/README.md). Proposal records,
  exact prompt bytes, Champion and the original ten-second admission limits
  remained intact. Verified replay passed. [The readback script](compatibility-proof.py)
  appends only ordinary command audits and keeps credentials and logs private.
- [Actual Claude Code review](claude-review.txt): read-only review found no
  remaining blockers in model visibility, stale state, atomic confirmation and
  role-budget checks. Its suggested precise rejection-message assertion was
  added and passed. Earlier findings also led to bounded recovery retries,
  receipt-hash verification, explicit permanent refusals and inexpensive
  progress polling. Code review is separate from runtime evidence.

All 184 TUI tests passed on the development runtime and Node 22.22.2. The tests
cover lost responses, authenticated recovery, stale/concurrent cursors, exact
bytes, profile drift, substituted evidence, terminal fitting and cancellation
of the displayed attempt only. All 44 Python tests, TypeScript checks, package
build, default/all-feature Clippy, formatting and documentation checks passed.
The [existing registration/evidence terminal flow](existing-pty.txt) and packaged
editor success and failure checks also passed after restoration.

A subsequent [Claude cancellation review](claude-cancellation-review.txt)
confirmed the fix for a poll that could replace the attempt shown in an open
cancellation prompt. Polling now refuses a different saved attempt; the existing
confirmation remains bound to its displayed job. Both polling and cancellation
refuse after another console starts a new attempt, and no kill request reaches
that newer job. The expanded suite passes 185 tests, and the
[packaged guided flow passed again](cancellation-pty.txt) after this correction.

Reproduce the main checks from the repository root:

```sh
cargo build --workspace --bins --all-features
npm --prefix apps/hephaestus-tui run typecheck
npm --prefix apps/hephaestus-tui test
npm --prefix apps/hephaestus-tui run build:package
TMPDIR=/tmp HEPHAESTUS_TUI_PTY_E2E=1 cargo test -p hephaestus-control --all-features --test control_plane_e2e tui_hosted_prompt_revision_recovers_and_assesses_through_a_pty -- --exact --nocapture
TMPDIR=/tmp HEPHAESTUS_TUI_PTY_E2E=1 cargo test -p hephaestus-control --all-features --test control_plane_e2e tui_evidence_screens_and_markdown_authoring_flow_through_a_pty -- --exact --nocapture
cargo llvm-cov --workspace --all-features --lcov --output-path /tmp/hephaestus-guided.lcov
python3 checks/coverage_gate.py --manifest checks/checks.json --report /tmp/hephaestus-guided.lcov
python3 checks/target_gate.py --manifest checks/checks.json
```

For the historical readback, first reproduce the old/current/restart phases in
the previous upgrade proof, then run this directory's compatibility script with
that scratch root and the current binary directory. Original upgrade reports
remain unchanged. This checkpoint does not establish fresh installation
acceptance, representative hosted-model improvement or full product readiness.
