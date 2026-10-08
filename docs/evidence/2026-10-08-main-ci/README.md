# Main CI compatibility verification, 2026-10-08

GitHub [run 37854189571](https://github.com/randyren278/hephaestus/actions/runs/37854189571)
failed its Rust 1.99 Clippy and two live-daemon PTY checks. The minimum Rust,
web, TUI unit checks, supply-chain check and native ARM64 package job passed.
The fixes change tests only; daemon, UI behavior and quality gates are unchanged.

Rust 1.99 requires direct empty/nonempty comparisons where values can be printed
on failure. The affected assertions now use equivalent equality/inequality checks.
The 80x24 Runs test navigates the five seeded runs to find the actual parent instead
of assuming it is newest. Revision recovery waits for the exact `COMPLETED:` phase
header instead of matching the earlier Completed Comparisons menu heading.
All existing receipt, disclosure, snapshot, terminal and restart checks remain.

## Current local results

- Exact Rust 1.88: [732 full-workspace tests including docs](full-tests.txt), zero failures or ignored tests; two test threads.
- [Five live-daemon PTY checks](pty-tests.txt) pass, including editor handoff, revision recovery and canonical receipt counts.
- Exact Rust 1.99: [all-feature Clippy](clippy-1.99-all-features.txt) and [default-feature Clippy](clippy-1.99-default.txt) pass.
- Formatting, all 442 source anchors and whitespace checks pass.
- Actual read-only Claude CLI reviews found no blocking assertion change: [PTY review](claude-pty-review.txt), [assertion review](claude-assertion-review.txt), [control review](claude-control-review.txt). Review comments about formatting were resolved by the actual formatting check.

The [proof](proof.json) records current input/export hashes. The prior full 442
mutation audit remains bound to its immutable checkpoint. All audited production
bytes are unchanged: three target files have only `cfg(test)` assertion edits,
whose production prefixes were checked byte-for-byte. Other changes are test-only
files, including the separately compiled server test module. The manifest and
all source anchors remain unchanged. This scoped follow-up does not invent a new
local full mutation run; the next main push starts ordinary GitHub CI.

A local recheck exhausted disk space. Its [Clippy refusal](disk-refusal.txt)
is retained. Interrupted test/build attempts were not counted as passing.
Only completed, task-owned temporary build caches were removed; source,
pilot data, logs and package archives were preserved before successful reruns.

Claude also noticed an older redundant sibling-worktree assertion in the provider
fixture. It remains outside this compatibility change; the review is not evidence
that that assertion verifies isolation. No experimental provider calls were added.
Native Intel, downloaded first-launch acceptance and commercial readiness remain open.
