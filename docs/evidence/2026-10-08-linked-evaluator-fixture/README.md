# Installed evaluator fixture on linked Cargo outputs, 2026-10-08

GitHub [run 37857015339](https://github.com/randyren278/hephaestus/actions/runs/37857015339)
passed the corrected PTY and lint checks, native ARM64 package and the 259 control
unit tests, then failed the evaluator recovery integration test. Its matching
restart pointed at the raw Cargo evaluator output, which the executable guard
refused because the file did not meet its single-hard-link requirement.

The fixture helper now copies that evaluator once to its private installation,
sets executable mode 0700 and asserts one hard link. Initial preparation and
matching restarts reuse the same installed copy. The deliberately mismatched
copy remains separate. Production validation, World byte pinning, budgets and
all recovery assertions are unchanged.

The native [red reproduction](red.txt) created a second hard link to the source
Cargo evaluator, then directly launched the existing test binary: it failed with
the exact GitHub rejection. Invoking Cargo first replaces the root output inode
on this Mac, so that preliminary attempt did not reproduce the linked condition
and is not counted as red. Direct launch preserves the controlled metadata.

After the correction, [all three fixture tests pass](green.txt) against the still
two-linked source evaluator, including mismatch rejection, matching restart,
canonical replay and registration-only provider-launch checks. The source stayed
on the same inode with two links during the passing test; the owned extra link
was then removed. [Proof](proof.json) records metadata, hashes and terminal exits.
Exact Rust 1.88 builds and formatting pass; [Rust 1.99 scoped Clippy](clippy.txt)
passes. Actual [read-only Claude review](claude-review.txt) confirms content
pinning and recommends the retained single-copy and explicit-permission choices.

The [732-test full workspace and five PTY checkpoint](../2026-10-08-main-ci/README.md)
belongs to the prior main commit. This later fixture-only edit has the scoped
three-test verification above, rather than an invented new full local mutation
or workspace run. Audited production code and all 442 mutation anchors remain
unchanged. The next normal main push runs GitHub verification again.
No new experimental provider calls, deployment or release was performed.
