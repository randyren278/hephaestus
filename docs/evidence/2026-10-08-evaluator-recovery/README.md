# Evaluator installation recovery, 2026-10-08

The configured evaluator must match the immutable World's pinned artifact.
Previously a mismatched installation returned an ambiguous internal error.
The daemon now refuses with an actionable matching-installation message before
admitting a comparison. Missing, unreadable and unsafe evaluator files also
receive fixed recovery messages. Unknown opening failures stay internal; OS,
protocol and execution details are not returned. Digest verification, executable
safety and isolation remain enforced.

## Native recovery and installed acceptance

The [real-daemon regression](recovery-red.txt) fails before the mapper change
with Internal instead of InvalidRequest. The [all-feature native tests](recovery-green.txt)
pass after the change. The same recovery case also passes with the
[normal production feature configuration](default-recovery.txt) on macOS.

The recovery test prepares an immutable support-triage World, restarts with a
regular executable containing different bytes, then verifies refusal through
both confirmed and ordinary admission. Both proposed job IDs remain unbound,
with no run or evaluation records. Restarting with the matching evaluator
reuses the same rejected evaluation ID and completes all 48 local adapter
fixture trials. Another restart and retry returns the exact finished progress
without recording additional runs. Replay passes throughout. This proves local
recovery and persistence, not provider authentication, availability or quality.

The [physical admission tests](scoped-tests.txt) exercise mismatched, missing,
non-executable, symlinked and hard-linked evaluators without binding jobs.
Unreadable-file classification is unit tested rather than inferred from chmod
on a privileged host. Unknown-error cases contain a sensitive sentinel and
must remain internal; known messages are bounded and do not expose file paths.

The strengthened [installed harness](package-acceptance.txt) creates a separate
World pinned to a different artifact. The delivered daemon rejects with the
recovery message, leaves the job unbound, records no runs/evaluations and never
launches its provider marker stubs. The message exposes neither evaluator path
nor artifact digest. Fixture source hashes remain unchanged. The
[previous archive](package-red.txt) fails specifically at the invalid_request
assertion under this harness; the new archive passes the full acceptance run.

Acceptance also covers a new isolated home, relocated prefix, bundled Node
with host Node/npm hidden, actual six-step terminal tour, offline Arena,
browser boundaries, coding evolution and reference Gauntlet restart/replay.
The pack's nine copied files match the repository and archive byte-for-byte,
including the recovery guide. The installed CLI matches the accepted archive.
The [proof](proof.json) records hashes and exact scopes. These are unsigned local
ARM64 artifacts; native Intel and quarantined-download acceptance remain open.

## Guards and review

The [fresh all-feature workspace run](coverage.txt) passes 723 unit/integration
and binary tests, with no failures or ignored tests. The separate four doc
tests bring the Rust total to 727. All [51 critical-module coverage floors](coverage-gate.txt)
pass at 92% or higher. The old tampered-evaluator e2e expectation was updated
to require InvalidRequest and the matching-installation recovery wording;
its existing no-additional-run and replay assertions remain enforced.

The Rust full run leaves the optional TUI PTY environment switch unset, so
it does not establish PTY behavior by itself. Actual terminal behavior is
covered separately by the installed package tour and console checks above.

The exact minimum Rust compiler passes the [workspace all-target/all-feature
check](rust188-check.txt). Both [all-feature](clippy-all.txt) and
[default-feature](clippy-default.txt) Clippy pass, and all four workspace
[doc tests](doc-tests.txt) pass. [Actionlint](actionlint.txt) accepts the workflow.

Both new permanent [mutation guards](mutations.txt) are killed on exact Rust
1.88: recovery classification and unknown-error privacy. There are no survivors,
stale anchors or timeouts in this two-entry run. Sources were restored
byte-for-byte, then [rebuilt and tested](restored-rust188.txt). This scoped run
does not imply a fresh full 432-entry result. All 432 configured anchors match
exactly once; the five control shards each contain 28 entries, and their CI
floors are 28. Existing admission-ordering guards remain in the matrix.

Actual Claude Code performed three read-only reviews. The
[first review](claude-review.txt) identified a recovery instruction that could
loop through the mismatched installation; the guide was corrected. The
[follow-up](claude-followup.txt) found no remaining material issue in the mapper,
physical tests and native recovery. The [final review](claude-final.txt) found no
material issue in the installed probe, new guards or shard floors. Its optional
privacy observation prompted exclusion of both configured and mismatched
paths/digests, followed by another successful full package run. The first
mutation's description was narrowed to classification. Claude ran no tests;
Codex produced the execution evidence.

Live-model quality, reported-versus-billed usage, customer results and revenue
remain unproved. No credentials or private daemon directories are included.
