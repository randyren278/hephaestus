# Hephaestus Operator TUI

An Ink v7 operator console for the existing local daemon protocol. The daemon remains the only writer of canonical state; closing this UI leaves it running.

## Run from a source checkout

Requires Node.js 22 or newer and a running Hephaestus daemon.

```sh
cd apps/hephaestus-tui
npm ci
npm start
```

Alternatively run `hephaestus tui` from a Rust source checkout after running `npm ci` here. A non-default data directory can be selected with `hephaestus --data-dir /path/to/data tui` or `HEPHAESTUS_HOME=/path/to/data npm start`.

The macOS package builder also creates `dist/main.mjs`, a single-file runtime bundle for the installed CLI. The distribution runs that bundle with its pinned adjacent Node runtime, so a packaged user's launch does not use npm or the source checkout. See [macOS package instructions](../../docs/MACOS_INSTALL.md).

## Controls

- `↑` / `↓` or `j` / `k`: select an action; `Enter`: open it.
- `Status`, `Freeze`, and `Unfreeze` read or change canonical daemon state.
- `Kill all active work` and `Cancel job by ID` ask for an explicit `Y` confirmation. Individual cancellation is shown as complete only after a terminal job status arrives from the daemon. Kill-all reports the active-run count and says terminal states are unavailable; this protocol does not provide per-job terminal confirmation for the group request.
- `Inspect job by ID` reads one durable job projection.
- `Lineage and Champions` opens a World list; `Enter` on a World draws its Genome ancestry as a tree, marking each row's Champion status: `▲` Champion, `▪` standby, `✕` quarantined. `Enter` on a Genome opens its detail panel with a line-level prompt diff against its first registered parent. `B` from the lineage view opens a rollback reason prompt (only when a previous Champion exists to restore); `Enter` on the reason then asks for an explicit `Y`/`N` confirmation before issuing `champion_rollback`. `Esc` steps back a level at every stage (Genome → lineage → Worlds → home).
- `Evidence & Costs` opens a submenu for four read-only, World-grouped screens built from the existing `run_list`, `evaluation_list`, and `denial_list` projections: **Runs** (state, completion reason, latency, cost), **Evidence receipts** (visible score, selection estimate in basis points, invariant summary, Forge assessment outcome, and any Champion transition it produced — never sealed content), **Costs** (totals per Genome, aggregated from run and paired-evaluation cost), and **Denials** (refused requests and runtime capability denials). Each screen groups rows under a `── <World> ──` header so incompatible Worlds are never mixed in one list; `↑`/`↓`/`j`/`k` navigate, `R` refreshes, `Esc` steps back.
- `Author Markdown agent` walks through authoring, registering, and testing a Markdown Genome against a real daemon: pick a World, accept (or edit) the default source path under `~/.hephaestus/agents/<world>.md` — created from a starter template (valid frontmatter plus the `identity` reference instruction) if it does not exist — then the TUI hands the real terminal to `$EDITOR`/`$VISUAL` (falling back to `vi`) and restores raw-mode input correctly when the editor exits. `Enter` then registers the file through the existing `genome_register` command; the daemon's compiler is the sole source of truth, and a rejection shows its sanitized reason. `T` starts a paired **Test**: pick a parent/Champion Genome and the TUI submits `evaluate_pair`, reusing `ArenaProgressPanel` for live progress and the visible score. This flow only registers and evaluates — it never unfreezes evolution or promotes a Champion; those remain separate, explicit operator actions.
- `q` closes the UI only.

The compact status frame fits an 80×24 terminal; the lineage, Worlds, Genome-detail, Evidence & Costs, and authoring panels are also verified at 100×30. Narrower terminals omit the secondary status panel and decorative colosseum. Terminal control bytes are stripped from daemon-provided text.

This is roadmap item 9's current slice. Still missing: a packaged-install acceptance run of the authoring flow from a clean, non-source-checkout install, and a recorded demo video. The Markdown starter template's body is the deterministic reference runtime's `identity` instruction, editable to `ascii_uppercase` or extended once richer runtimes exist; the reference runtime's parser only accepts that exact fenced block, so free-form prose must live outside it until a richer runtime reads it.

## Development checks

```sh
npm ci
npm test
npm run typecheck
npm run build:package
```

Three live-daemon PTY acceptance fixtures exist as opt-in Rust integration tests, all gated by `HEPHAESTUS_TUI_PTY_E2E=1` and requiring `npm ci` in this directory first:

- `async_job_status_and_cancellation_remain_responsive_and_confirm_process_death` launches the Rust CLI from a source checkout inside an 80×24 pseudo-terminal, selects a relative data directory while a second daemon is configured as the fallback, exercises confirmed cancellation against the selected daemon, checks stale status when launched without an available daemon, and verifies terminal restoration on `q`. This is also the live-job responsiveness proof: while a real job runs, it navigates to status, issues `Cancel job by ID`, and gets daemon-confirmed termination before the test's own budget expires.
- `tui_lineage_inspects_and_rolls_back_the_champion_through_a_pty` builds a real promoted lineage through the CLI, then drives the Ink lineage screens in a 100×30 pseudo-terminal (`scripts/pty_lineage.py`): it opens the World, navigates the lineage tree to the Champion row, inspects its prompt diff against its parent, and rolls it back through the reason prompt and `Y` confirmation, asserting the daemon's rollback event and the refreshed quarantined/standby state.
- `tui_evidence_screens_and_markdown_authoring_flow_through_a_pty` builds a real World, parent/candidate Genome pair, run, paired evaluation, and denial through the CLI, drives the Evidence & Costs screens in an 80×24 pseudo-terminal (`scripts/pty_evidence.py`) asserting each shows that real data with Worlds visually separated, then drives the full Markdown authoring flow (`scripts/pty_author.py`): World pick, the default path and starter template, an `$EDITOR` hand-off (set to a no-op for determinism) with verified raw-mode restoration, `genome_register` against the real compiler, and a paired Test via `evaluate_pair` that waits for a live visible score from the reused `ArenaProgressPanel`.

Run all three with:

```sh
HEPHAESTUS_TUI_PTY_E2E=1 cargo test -p hephaestus-control --all-features --test control_plane_e2e -- --nocapture \
  async_job_status_and_cancellation_remain_responsive_and_confirm_process_death \
  tui_lineage_inspects_and_rolls_back_the_champion_through_a_pty \
  tui_evidence_screens_and_markdown_authoring_flow_through_a_pty
```
