# Preserve numeric provider usage without exposing secrets

The first [real Codex comparison](../2026-10-08-live-codex/README.md) exposed
a persisted-evidence defect: all four usage counters were masked because
generic secret-key matching includes `token`. The provider parser had kept
them, but the recorder removed them before writing content-addressed traces.

The correction permits only `input_tokens`, `cached_input_tokens`,
`output_tokens` and `reasoning_output_tokens`, only in `CostObserved` traces,
and only for canonical unsigned decimal values that fit `u64`. Other trace
kinds and generic experience fields still apply the full token-key rule.
Even valid counts go through known-secret redaction; a collision masks the
entire field. Malformed values and ordinary credentials remain masked.

Tests pass actual Codex NDJSON through the production parser and recorder,
read its persisted artifact and reopen its history. They also cover signs,
whitespace, decimal/exponent notation, leading zeroes, quoted strings, Unicode
digits, overflow, near-miss key names, credentials, wrong trace kinds and
numeric known-secret collisions. Eight mutation entries guard these new
boundaries without weakening any existing coverage or timeout gate.

This changes newly recorded traces. Existing artifacts are immutable and no
historical token values are recovered or inferred. The initial live pilot
continues to use its original matching installation. This patch is verified
offline; it does not consume additional real-provider invocations.

Verification output and exact source hashes are retained alongside this file.
The earlier complete 434-mutation result remains evidence for `ebbb21b`;
the new configured matrix has 442 entries and requires separately identified
current verification. Do not treat the earlier full audit as a pass of this
patch.

Current verification:

- The [workspace coverage run](workspace-coverage.txt) passed **728 unit and
  integration tests**, zero failed or ignored. All
  [51 critical module floors](coverage-gate.txt) passed at 92% or above;
  redaction reached 100% and the recorder 99.6%.
- The default full-workspace mutation baseline passed on exact Rust 1.88.0.
  The [redaction audit](mutations.txt) selected **12 entries**, including four
  existing redaction entries and eight new ones: **12 killed**, no survivors,
  stale anchors or timeouts. A separate [recorder audit](recorder-mutations.txt)
  killed all **8 recorder entries**, also without survivors, stale anchors or
  timeouts. Together these cover **20 affected entries**. Its
  [restored suite](recorder-restored-tests.txt) passed 51 tests including its
  doc test. The restored
  [experience suite](restored-rust188-tests.txt) passed **51 tests including
  its doc test**; [all workspace doc tests](rust188-doc-tests.txt) passed 4/4.
- [Workspace Rust 1.88 checks](rust188-check.txt),
  [all-feature Clippy](clippy-all-stable.txt) and
  [default Clippy](clippy-default-stable.txt) passed. An initial Clippy attempt
  mixed a newer driver with 1.88 dependencies and was
  [refused before source linting](clippy-mixed-toolchain-refusal.txt); the
  successful runs use one matching stable toolchain.
- [Python checks](python-tests.txt) passed 47/47. This exposed stale control
  shard expectations from the earlier 137-entry checkpoint; they now verify
  the current 142 entries as 29/29/28/28/28 and raise the targeted-command
  floor to 142. The experience CI minimum rises from 72 to 80 for the eight
  added entries. No gate, test, timeout or floor is lowered.
- [Exact input hashes](source-hashes.json) identify the patch, tests and CI
  inputs. The mutation target was restored byte-for-byte before the final
  tests. These scoped results do not establish a full fresh 442-entry audit
  or published downloaded-package acceptance.

The current native ARM64 package also passed its ordinary
[build](package-build.txt) and [installed acceptance](package-acceptance.txt):
relocation, bundled Node with host Node/npm hidden, a fresh six-step tour,
offline Arena/replay, packaged TUI and browser, frozen pilot preparation and
three-generation coding Gauntlet. Pilot markers report zero provider launches.
The [proof](proof.json) records the archive and executable/asset hashes.
Native Intel, a published browser download and independent fresh-user
acceptance remain unverified. The current-source browser report also
[uses the corrected verdict wording](../2026-10-08-live-forge/current-template-report.md);
its measured values remain identical to the historical export.
