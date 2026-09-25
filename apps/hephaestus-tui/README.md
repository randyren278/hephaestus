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
- `q` closes the UI only.

The compact status frame fits an 80×24 terminal; the lineage, Worlds, and Genome-detail panels are also verified at 100×30. Narrower terminals omit the secondary status panel and decorative colosseum. Terminal control bytes are stripped from daemon-provided text.

Lineage and Champions is the current slice of roadmap item 9's TUI-first operator experience. It does not yet include runs, evidence receipts, costs, or denials screens, and there is no packaged-install authoring flow (author/validate/register a Markdown agent) from the TUI; those remain open roadmap item 9 work.

## Development checks

```sh
npm ci
npm test
npm run typecheck
npm run build:package
```

Two live-daemon PTY acceptance fixtures exist as opt-in Rust integration tests, both gated by `HEPHAESTUS_TUI_PTY_E2E=1` and requiring `npm ci` in this directory first:

- `async_job_status_and_cancellation_remain_responsive_and_confirm_process_death` launches the Rust CLI from a source checkout inside an 80×24 pseudo-terminal, selects a relative data directory while a second daemon is configured as the fallback, exercises confirmed cancellation against the selected daemon, checks stale status when launched without an available daemon, and verifies terminal restoration on `q`.
- `tui_lineage_inspects_and_rolls_back_the_champion_through_a_pty` builds a real promoted lineage through the CLI, then drives the Ink lineage screens in a 100×30 pseudo-terminal (`scripts/pty_lineage.py`): it opens the World, navigates the lineage tree to the Champion row, inspects its prompt diff against its parent, and rolls it back through the reason prompt and `Y` confirmation, asserting the daemon's rollback event and the refreshed quarantined/standby state.

Run both with:

```sh
HEPHAESTUS_TUI_PTY_E2E=1 cargo test -p hephaestus-control --all-features --test control_plane_e2e -- --nocapture \
  async_job_status_and_cancellation_remain_responsive_and_confirm_process_death \
  tui_lineage_inspects_and_rolls_back_the_champion_through_a_pty
```
