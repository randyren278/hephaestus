# Terminal editor handoff, 2026-10-08

The console now accepts editor settings with flags and shell quoting, passes the
source path as a literal argument and reports nonzero exits without advancing to
registration. Native Ink terminal suspension handles output and terminal modes;
a synchronous child launch keeps the parent from competing for keyboard input.
The source is preserved on failure and Enter retries.

- [Packaged-console PTY assertions](pty.json): 80×24 terminal, exact flag and
  path arguments, actual typed Unicode input, two seconds without console writes,
  a single editor launch on double Enter, raw-mode restoration, preserved source
  and terminal restoration on exit. Both success and status-23 failure passed.
- [CI terminal fixture output](ci-pty.txt): the ordinary Evidence & Costs and
  full Markdown registration/comparison flows passed against a real reference
  daemon, followed by both packaged-editor cases. This is a local run of the CI
  fixture, not evidence of a published GitHub Actions run.
- [Claude Code review](claude-editor-review.txt): no blocking editor finding.
  The suggested keyboard-input check and CI integration were incorporated.
- [Final private-storage review](claude-workspace-review.txt): no remaining
  blocker in the foundation; controller responsibilities remain explicit.

All 158 TUI tests passed, including the independent revision-workspace tests,
on the development runtime and Node 22.22.2. TypeScript checks and the package
build passed. Draft-storage helpers remain separate from the upcoming guided
Forge screens; these checks do not establish that those screens exist.

The 17 draft-storage tests cover exact UTF-8 copies, private permissions,
read-only content-addressed snapshots, restart and stale/concurrent cursor
updates. Versioned cursors publish atomically and retain previous comparison
IDs. Corrupt cursors report individually, successful and failed comparisons
have distinct recovery states, and a failed run cannot advance to assessment.
Interrupted snapshot publication can resume with matching bytes or a different
reviewed body. The forthcoming controller must reconcile these local cursors
with the daemon and verify the returned prompt artifact before advancing.

Reproduce after building the Rust CLI and installing the TUI dependencies:

```sh
npm --prefix apps/hephaestus-tui run typecheck
npm --prefix apps/hephaestus-tui test
npm --prefix apps/hephaestus-tui run build:package
TMPDIR=/tmp HEPHAESTUS_TUI_PTY_E2E=1 cargo test -p hephaestus-control --all-features --test control_plane_e2e tui_evidence_screens_and_markdown_authoring_flow_through_a_pty -- --exact --nocapture
```

The fake editor and authored files live in temporary homes. The Rust fixture now
isolates the original authoring driver too. The CI PTY job builds the packaged
console before running these checks. Interrupting a wait-style editor with
Ctrl-C can exit the console; the daemon and saved source remain available.
