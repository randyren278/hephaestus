# Installed browser console and evidence report

This checkpoint adds `heph web` to source and native macOS installations.
The archive includes the server wrapper and payload, HTML, browser JavaScript,
styles and crest beside its pinned Node 24.21.0 runtime. Source `--full`
installation builds the browser console once and requires Node 22+.

The [proof](proof.json) records the base commit, changed runtime source hashes,
archive checksum and packaged file hashes. These are local Apple Silicon checks
against deterministic offline agents. They prove neither live-provider quality
nor hosted delivery, native Intel execution or quarantined-download acceptance.

## Installed acceptance

The [archive-wide acceptance](package-acceptance.txt) installs into a clean home,
moves the prefix, hides host Node/npm and completes the actual six-step tour,
offline Arena, recorded Selection, replay, both consoles and both Gauntlets.
The [final browser boundary check](browser-boundary.txt) additionally records:

- Served bytes match all four installed static assets, with the expected CSP.
- Comparison entries match authenticated CLI readback from the selected daemon.
- Missing tokens, untrusted Host/Origin and six mutating command tags are refused.
- A hostile host `NODE_OPTIONS` value does not affect the bundled runtime.
- A missing daemon with `--no-daemon`, or a missing package asset, creates no data directory.
- A fresh launch bootstraps a frozen daemon, without evaluations or active runs.
- Foreground Ctrl+C closes the browser listener and leaves the daemon available.

Authenticated reads append daemon audit events. The test compares operational
state and immutable comparison evidence rather than requiring an unchanged
audit count. It restores its deliberately hidden asset and stops its fresh daemon.

## Actual browser and source checks

In an isolated Chromium session, the relocated installed `heph web` served the
console while host Node/npm remained excluded. Codex opened **Evidence & activity**,
expanded **Agent identities and report**, and clicked **Download evidence report**.
The [download](comparison-report.md) matches the [CLI entry](evaluation-readback.json)
for exact identities, 0/1 versus 1/1 visible scores, recorded USD, 68/71 ms latency,
measured eligibility and the absence of invariant, Forge and Champion evidence.
This is a readable summary, not a signed receipt or promotion authority.

The [desktop](comparison-desktop.png) and [mobile](comparison-mobile.png) captures
show the result and export control. At a 390 px viewport, the document is 375 px
wide; there is no page overflow. The session fragment was removed from the URL.
The [foreground PID check](foreground-process.txt) confirms Unix exec replaced
`heph` with the relocated bundled Node process. Browser and owned daemon were
stopped afterward. No session tokens or private data directories are copied here.

The [real full source install](source-install.txt) and [Node 22 browser check](source-acceptance.txt)
pass. A [real installed source-launcher check](source-node-preflight.txt) rejects
missing Node and Node 20 before any data-directory or daemon creation.
The [launcher tests](launcher-tests.txt) pass 15 cases and the
[installer contracts](installer-contracts.txt) pass 20 cases. See the pinned
[Rust 1.88 check](rust-1.88-check.txt) and both [all-feature](clippy-all.txt) and
[default-feature](clippy-default.txt) Clippy logs.

The refreshed [workspace](workspace-tests.txt) passes 721 Rust tests including
doc tests, with zero failures or ignored tests. The [TUI](tui-tests.txt) passes
191 tests and the [web console](web-tests.txt) passes 40, both with Node 22 and
successful typechecks. The [Python suite](python-tests.txt) passes 47 tests.
The exact Rust 1.88 Cargo and compiler pass the
[all-target/all-feature workspace check](rust-1.88-all-targets-check.txt) with
the current Rust sources. All 429 existing mutation source anchors remain valid;
this checkpoint does not claim a fresh full mutation or coverage run.

Claude independently [reviewed the initial implementation](claude-review.txt),
identifying late Node preflight and the child-process signal problem. Codex fixed
those, strengthened damaged-package detection and added the daemon stop hint.
Claude's [second review](claude-followup-review.txt) found no material launcher
defect. Its remaining test suggestions were addressed by the later port-closure,
hostile Node-options and freeze/job-kill probes recorded above. Review preceded
those final probe additions; the actual runtime evidence covers them.
