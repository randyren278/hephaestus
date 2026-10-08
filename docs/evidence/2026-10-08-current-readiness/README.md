# Current source delivery and readiness audit, 2026-10-08

Production implementation is held at `ebbb21b` while complete mutation audits
run against owned, detached checkouts. Documentation clarifies the remaining
acceptance requirements without changing code, mutation entries or quality floors.
The completed 432-entry run is tied to `98ce84d`; the running 434-entry audit
is tied to `ebbb21b`. The current audit remains unproven until its terminal
result, source restoration and restored workspace checks are verified.

## Complete checkpoint mutation audit

The [full 432-entry mutation log](checkpoint-432-mutations.txt) ends with
432 killed, zero survivors, zero stale anchors and zero timeouts. It uses the
manifest's ordinary commands and mutation timeouts, Rust 1.88.0 and four test
threads; the initial build baseline has a 900-second allowance.

After the audit exited successfully, every mutation target matched its recorded
SHA-256 and the tracked checkout was clean. The restored checkout was then
rebuilt through the [full workspace suite](checkpoint-432-restored-tests.txt):
727 tests including doc tests passed, with zero failures or ignored tests.
Hashes and tracked cleanliness were checked again after that run. The
[proof](checkpoint-432-proof.json) records the checkpoint, command, source
hashes, terminal result and post-restoration checks.

This result verifies `98ce84d`. The two additional launch-setting guards have
their own affected-scope evidence; a full current 434-entry result is still
required before claiming the complete current trust audit passed.

## Actual source installation

The owned source checkout was updated to `ebbb21b`, then its ordinary
[full installer](source-install.txt) rebuilt release binaries, installed terminal
dependencies, built the browser and linked the private installation prefix.
The [proof](source-proof.json) records the exact checkout, all seven installed
binary hashes and sizes, symlink targets, host Node version and copied pack
hashes. The installer reused the owned Cargo target directory recorded in the
proof; this run does not establish a fresh-target build. The tracked
checkout is clean; pre-existing generated SBOM files are unrelated to this
check and were retained.

The [current pilot acceptance](source-pilot-acceptance.txt) passes from these
installed binaries: frozen native preparation, invalid-cost preflight, exact
profile retries, evaluator mismatch rejection, ignored startup-setting refusal,
explicit attachment, ordinary reopening, real pilot TUI attachment and graceful
restart/replay. Provider markers stay absent, and all nine copied pack files
match the repository. This source run uses host Node 22.22.2 and Python for the
acceptance driver; it does not claim the package's bundled-runtime environment.

A separate fresh data directory completes the [actual first-launch question and
six measured tour steps](source-tour.txt), including fixture registration,
reference Arena, metrics Selection, replay and terminal restoration at 80x24.
No hosted provider is called. The tour driver retains its historical
"packaged_terminal" field name, but its packaged evaluator lookup is false and
`--packaged` was not supplied. Source use is the scope of this result.

Owned source browser, terminal and daemon processes stopped after these checks.
Private token files, daemon data and credentials are not included. An initial
verification script mistakenly counted its own shell as a live app and continued
against the old checkout. That attempt was stopped and excluded from evidence;
the pinned installer and checks above are the successful rerun.

## Actual Claude acceptance review

Claude Code's [read-only readiness review](claude-readiness-review.txt) found no
material contradiction, overclaim or concrete defect in the evidence it read.
Both full audits were still running when that review was requested, and Claude
did not assume their results or execute tests.

Its [updated review](claude-updated-readiness-review.txt) checked the terminal
432-entry result, restored 727 tests, installation evidence and clarified
requirements. It found no material contradiction, overclaim or concrete defect.
Its minor evidence requests were addressed by recording all seven installed
binaries and the reused build target, and by making the live-evidence status
table explicit about the initial pair's limits.

Its two acceptance ambiguities are now explicit in the readiness document.
The first requested real baseline/checklist comparison is 48 invocations and
requires its own quota/model/auth-handoff approval. It cannot prove a real
provider's response to an evidence-bound Forge revision. That follow-up needs
the first Selection, a distinct proposal and a separate authorized comparison.
A tie or regression receives the same evidence checks as a win.

Codex supplies no USD amount. A recorded zero or unavailable figure cannot close
the measured-cost requirement. Actual billing or quota claims need attributable
provider usage evidence. The measured-cost gate remains open.

Real-provider authentication, representative quality and cost, fresh-user
acceptance, hosted release delivery, native Intel/quarantine acceptance and
commercial approval remain open. No real model or revenue result is claimed.
