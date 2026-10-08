# Complete current 442-entry audit, 2026-10-08

The immutable checkpoint `632b4bc03baae5f0461cd5dcaec2d4a66e1a9f57` passed the complete current
mutation matrix on exact **Rust 1.88.0**. The [guard output](mutations.txt)
records **442 run, 442 killed, zero survivors, stale anchors or timeouts**.
Every configured entry ran. The manifest's default full-workspace baseline,
per-entry commands and 120/240/480-second mutation timeouts were retained;
the baseline timeout was 900 seconds. No test, assertion, coverage floor,
threshold or timeout was weakened.

The guard restored every target byte-for-byte. The subsequent ordinary
[restored full-workspace test run](restored-tests.txt) passed **732 tests
including doc tests**, zero failed or ignored, on the same exact toolchain.
Post-test hashes and tracked checkout cleanliness were verified again.
At audit completion the main worktree's target files and manifest matched that
immutable source. Later Rust 1.99 compatibility edits change test assertions only;
the [main CI follow-up](../2026-10-08-main-ci/README.md) verifies unchanged
production code, the same manifest and all 442 source anchors.
The [proof](proof.json) records commands, hashes, compiler, thread count and
terminal exits. Audit plus restored tests took 83.2 minutes.

## Baseline refusal and retry

The first attempt used four test threads and was
[refused before mutations](baseline-refusal.txt): an existing canary timing
fixture's injected 15ms delay did not make the candidate measurably slower.
The guard correctly counted **zero mutations** against that failing baseline.
Its [proof](baseline-refusal-proof.json) records clean source restoration.

The unchanged test passed [three exact diagnostic repetitions](timing-diagnostic.txt).
The completed real pilot's owned browser and daemon were stopped while
preserving its durable 96-call evidence. The full audit was then restarted
with **two test threads**, the concurrency used by the repository's coverage
job to reduce scheduling noise. The new unmutated baseline passed, followed
by all 442 mutations and the restored suite. This is a disclosed scheduling
failure and retry, not a claim that the first attempt passed or that the
fixture is deterministic under arbitrary host load.

## Independent scopes

[Claude's actual read-only pre-main review](claude-review.txt) found no code
blocker in the inspected redaction implementation and evidence. It did not run
tests or establish this full audit. Its minor compiler/review attribution
findings were clarified in the readiness documentation.

The earlier [434-entry audit](../2026-10-08-full-current-mutations/README.md)
remains historical evidence for `ebbb21b`. The
[usage correction checkpoint](../2026-10-08-usage-redaction/README.md) separately
records 51 coverage floors, both stable Clippy modes, exact Rust 1.88 checks,
47 Python tests, 40 web tests/typecheck/build and the updated native ARM64
package's installed acceptance. The present audit closes the complete-matrix
verification for the later correction; it does not establish native Intel,
published-download/fresh-user acceptance, attributable USD or customer value.

Neither the audit nor installed offline acceptance launches a model. Both
real provider allowances remain exhausted; no further experiment is authorized.
