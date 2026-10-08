# Coding evolution through a short daemon address

The native `evolve coding` convenience command previously resolved its data
directory before connecting. On macOS, an otherwise working short socket address
could become too long after `/tmp` resolution or a parent alias. The ordinary
CLI reached the daemon, but coding evolution returned `SUN_LEN` and incorrectly
suggested starting an already running daemon.

The helper now keeps the original supplied directory for its `Client` and
resolves only setup-file storage. Canonical World/Genome registration paths are
unchanged, and connection-error guidance names the supplied address. The daemon
and its final-component symlink protection are unchanged.

## Real daemon and installation checks

The existing three-generation real CLI/daemon test now uses a short parent alias
to a long real backing directory. The socket address is under 104 bytes while
the backing address exceeds 108 bytes, covering both platform limits. The data
directory itself is real. Before the implementation change, the
[test fails with SUN_LEN](daemon-red.txt). After the fix it
[completes three generations](daemon-green.txt) and consumes six paired trials.
A graceful daemon restart followed by the complete helper retry returns the exact
same durable run, including all generation and event identities. Replay passes.

The strengthened [installed acceptance](package-acceptance.txt) uses a 57-byte
original socket address whose resolved storage address is 141 bytes. It installs
the ARM64 archive into a new home, relocates the prefix and hides host Node/npm.
CLI reads, a direct run, Arena, Selection, replay, TUI, browser boundaries and
coding evolution exercise this aliased address. The six-step tour, support-triage
preparation and reference Gauntlet use their existing separate data directories
and also pass. No hosted provider is called.

Against the same strengthened harness, the [previous archive](package-red.txt)
passes the earlier CLI/Arena/replay/console/pilot steps and fails specifically at
coding evolution's status preflight with `SUN_LEN`. The [new archive build](package-build.txt)
and complete installed acceptance pass. This is an actual delivered regression,
not just a path-length assertion or source-only test.

The [source CLI rebuild](source-build.txt) updates the existing source-install
prefix. [Actual source execution](source-acceptance.json) completes three
reference generations at the previously failing 98/106-byte paths, then stops,
restarts and retries the helper. The complete prior outcome is unchanged and
replay passes. Provider-launch markers remain absent. Owned browser/daemon
processes were stopped after checks; private directories and tokens are not
copied into this evidence.

The [proof](proof.json) records the base commit, changed source hashes, both
archive checksums and sizes, delivered native CLI hash, and exact check scopes.
The installed CLI bytes match the accepted fixed archive. These are local
unsigned ARM64 artifacts, without native Intel, hosted release or quarantined
download acceptance.

## Guard and verification scope

The exact Rust 1.88 Cargo/compiler pass the [targeted real-daemon test](rust-1.88-test.txt)
and [workspace all-target/all-feature check](rust-1.88-check.txt). Current
[CLI tests](cli-tests.txt) pass 23 cases; both [all-feature](clippy-all.txt) and
[default](clippy-default.txt) Clippy pass.

The new permanent [mutation guard](mutation.txt) replaces the retained socket
address with the resolved directory. It first verifies the unmutated suite is
green, then kills that regression: 1 run, 1 killed, no survivor/stale/timeout.
This runs in an owned checkout with its own Cargo target, and changed source is
restored byte-for-byte, then [rebuilt and rerun](rust-1.88-restored-test.txt)
successfully. All 430 configured anchors match exactly once. Control
shard 3's minimum rises from 27 to 28; the shard sizes are 28/28/28/27/27.
[Actionlint](actionlint.txt) passes. The existing matrix structure follows
[GitHub's matrix include syntax](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax#jobsjob_idstrategymatrixinclude).
No action versions, permissions or publication behavior changed; no hosted CI
result is claimed.

The latest full workspace checkpoint remains 724 tests at `5d231c4`. This
surgical change has the scoped checks above. It does not establish a fresh full
430-entry mutation pass or full critical-module coverage run.

## Actual Claude review

Claude Code [reviewed the implementation and strengthened daemon test](claude-review.txt)
read-only and found no material defect. Its [follow-up](claude-followup.txt)
reviewed the installed harness and also found no material defect. It correctly
limited aliased-address coverage to the main CLI/Arena/replay/consoles/evolve
steps; the other fixtures retain separate directories. It also withdrew an
incorrect cleanup statement after reading the existing daemon Drop guard,
which kills and waits on panic. Claude ran no tests; Codex produced the runtime
evidence. The later mutation entry and CI floor increase are recorded by the
actual guard run and Actionlint, rather than covered by those earlier reviews.

Real-provider authentication, availability, representative output quality,
reported-versus-billed usage and revenue remain unproved. Evaluator-installation
mismatch recovery, fresh complete trust checks and release delivery acceptance
remain required for the full product objective.
