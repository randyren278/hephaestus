# Installed support-triage pilot preparation

This checkpoint bundles the complete public support-triage pack in native macOS
archives and adds native, registration-only preparation. It uses ordinary daemon
commands and does not launch providers, set up authentication or validate model
availability. The user workflow requires Git, with no source checkout or host
Node/npm/Python. Python is used by the acceptance harness only.

## Operator workflow

`hephaestus init --fixture support-triage /path/to/new-pilot` copies nine files
and initializes a separate `repository` containing only README.md. Task inputs,
expected labels, prompts and provider instructions remain outside that source.
An existing destination rejects and is preserved. The copied pack guide and
provider setup instructions are self-contained.

Launch the same installation's `heph web` against the pilot data directory with
`HEPHAESTUS_SOURCE_REPOSITORY` pointing to its repository. The daemon boots frozen.
`hephaestus --data-dir DATA pilot prepare PACK --provider codex --model MODEL
--cost-microusd CEILING` registers both manifests, the evaluator and verifier,
World, baseline and child through the ordinary authenticated API. Claude is the
other explicit provider choice. Both complete profiles and immutable identities
are printed for review. The command never unfreezes or evaluates.

The positive per-trial reported-cost ceiling is bounded by the existing daemon
limit. This is not an API bill or subscription quota cap; Codex reports no USD.
Identifiers are validated, then JSON-quoted as YAML scalars. Frozen/no-active-run
checks bracket registration. Each attempt that passes the first check retains a
new 0700 setup directory, including failures and retries. Registration is
resumable, not atomic. Same-input retries return the same identities.

## Actual installation and runtime evidence

The [proof](proof.json) identifies the base commit, current sources, archive and
installed binary/fixture hashes. All nine installed fixture files match current
source and archive bytes. This is a locally built, unsigned native ARM64 archive.
It establishes no hosted release, native Intel or downloaded/quarantine result.

The [package build](package-build.txt) and [archive-wide acceptance](package-acceptance.txt)
pass. Acceptance installs into a new home, relocates the prefix, hides host
Node/npm, exercises the actual six-step tour and both consoles, measures the
offline reference Arena, records Selection and replay, and completes both
Gauntlets. Its pilot phase registers Codex and Claude placeholder models with
marker executables. Zero providers launch, no evaluations occur, and the daemon
remains frozen with no active runs.

The pilot phase verifies profiles, 12 visible plus 12 sealed tasks, strict JSON
scoring, per-trial cost and paired wall budgets, evaluator binding, 0700 storage,
source preservation, same-ID retries and graceful stop/restart/replay. Invalid
costs are rejected before daemon contact; World/Genome/setup sets and audit-count
deltas verify the refusal boundaries. Unfrozen preparation makes only its status
preflight and rejects without registrations or setup storage.

The [actual source installation](source-install.txt) and
[source pilot acceptance](source-acceptance.txt) also pass. The source acceptance
uses a 98-byte original socket path whose resolved path is 106 bytes. This
reproduced a macOS socket-path bug before correction. The command now preserves
the operator's original socket address while canonicalizing setup-file storage.
Owned browser processes and daemons were stopped, and provider markers remain
absent. No private data directories, tokens or producer keys are copied here.

The [real CLI/daemon test](pilot-daemon-test.txt) additionally checks model `null`
stays a string, newline injection rejects, existing initialization is preserved,
partial failure keeps files and recovers with identical identities, and a crash
restart replays the same profiles. Its short parent alias resolves to a socket
path exceeding both macOS/Linux limits; the data directory itself is real.
In an isolated Rust 1.88 checkout, [reverting the Client path](socket-red.txt)
fails specifically with `SUN_LEN`, and [restored current source](socket-green.txt)
passes. Mutated source is restored byte-for-byte. This is a focused regression
check, not a new full mutation-matrix run.

Fixture Git commands suppress global signing, hooks, templates and repository
location/config-injection environment overrides. Marker and foreign-index checks
verify these boundaries. Other global/system configuration and author environment
can still apply; these checks do not imply all user Git configuration is ignored.

## Checks and independent review

The [workspace run](workspace-tests.txt) passes 724 Rust tests including doc tests,
with no failed or ignored cases. The final real-daemon test also passes separately.
Both [all-feature](clippy-all.txt) and [default](clippy-default.txt) Clippy pass.
The exact [Rust 1.88 Cargo/compiler](rust-1.88-check.txt) pass the all-target,
all-feature check. Node 22 typechecks pass; [TUI](tui-tests.txt) passes 191 tests
and [web](web-tests.txt) passes 40. [Python](python-tests.txt) passes 47 and
[installer contracts](installer-contracts.txt) pass 20. Formatting, documentation
and all 429 existing mutation anchors pass. Current full coverage and a fresh
full 429-entry mutation result are not implied.

Actual Claude Code performed independent read-only reviews through its installed
CLI. The [first review](claude-review.txt) led to early cost rejection, isolated
fixture Git commands and consistent data-directory instructions. The
[follow-up](claude-followup-review.txt) found no material defect and suggested
stronger refusal probes, retained-directory documentation and cleanup handling;
those were addressed after that review.

The [initial socket review](claude-socket-review.txt) inspected an intermediate
test draft and correctly identified its final-component symlink and short Linux
backing-path problems. Those were corrected to a parent alias with explicit
short/long path bounds. The [final socket review](claude-socket-followup.txt)
found no defect in the current pilot or test. It identified the same path issue
in the existing `evolve coding` helper. Codex [reproduced that remaining defect](evolve-socket-reproduction.txt)
through the installed source CLI at the same 98/106-byte paths; the ordinary
status succeeds, but the helper fails before unfreeze or provider work. That
remains a separate reliability fix. Claude did not reproduce the runtime
results; Codex ran the recorded checks.

Real authentication, requested-model availability, representative hosted quality,
provider billing/quota and revenue remain unproved. The frozen preparation uses
no actual credentials or subscription/API invocations. The same-install CLI,
daemon and reference evaluator requirement is documented, and protected scoring
still checks the evaluator digest before trials.
