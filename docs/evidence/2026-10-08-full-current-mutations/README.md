# Full current mutation audit, 2026-10-08

The complete configured matrix was run against an owned detached checkout of
`ebbb21b`. Production code, test commands, mutation entries and timeout gates
remained unchanged during the run. The immediately following main-worktree
commits contained only documentation and evidence. The later usage-redaction
production change is outside this checkpoint.

The [terminal mutation log](mutations.txt) reports **434 run, 434 killed,
zero survivors, zero stale anchors and zero timeouts**. The process exited
zero. Every target matched its recorded SHA-256 afterward and the tracked
checkout was clean. The [proof](proof.json) records those hashes and results.

The manifest's ordinary commands and per-mutation timeouts were used, with
Rust 1.88.0, four test threads and a 900-second initial baseline build allowance.
That baseline passed before mutation execution. The guard classifies a
nonzero mutated command as killed; this result is evidence for the configured
mutation checks, not a claim about every possible defect.

After source restoration, the checkout was rebuilt through the
[complete workspace suite](restored-tests.txt): **728 tests including doc tests
passed, zero failed and zero ignored**, on Rust 1.88.0. The process exited zero.
Every mutation target still matched its recorded hash and the tracked checkout
was clean afterward. The [restored-suite proof](restored-proof.json) records the
command, compiler, terminal result and raw/archived log hashes.

At this audit's completion, the main worktree's mutation targets matched these
hashes and all tracked files outside documentation matched the audited
checkpoint. This verifies the complete configured trust audit at `ebbb21b`,
not subsequent production edits. The older 432-entry checkpoint's separate
result was not combined with affected-scope checks to produce this result.

Claude Code's [read-only review](claude-review.txt) independently counted the
434 killed mutations and 728 passed tests and found no material contradiction
or overclaim. Its stale-status wording requests were addressed in the readiness
documents. Claude did not execute tests or recompute hashes; Codex performed
the recorded hash and worktree checks.

This audit does not establish real-provider quality, measured billing, published
release acceptance, fresh-user onboarding or revenue. Those full product
requirements remain open in the [readiness document](../../PRODUCT_READINESS.md).
