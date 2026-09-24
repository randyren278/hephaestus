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
- `q` closes the UI only.

The compact status frame fits an 80×24 terminal. Narrower terminals omit the secondary status panel and decorative colosseum. Terminal control bytes are stripped from daemon-provided text.

## Development checks

```sh
npm ci
npm test
npm run typecheck
npm run build:package
```

The live-daemon PTY acceptance fixture can be enabled for the control-plane's existing slow-job integration test with `HEPHAESTUS_TUI_PTY_E2E=1 cargo test -p hephaestus-control --all-features --test control_plane_e2e async_job_status_and_cancellation_remain_responsive_and_confirm_process_death -- --exact --nocapture`. It launches the Rust CLI from a source checkout inside an 80×24 pseudo-terminal, selects a relative data directory while a second daemon is configured as the fallback, exercises confirmed cancellation against the selected daemon, checks stale status when launched without an available daemon, and verifies terminal restoration on `q`.
