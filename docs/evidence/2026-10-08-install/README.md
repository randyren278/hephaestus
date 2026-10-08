# Packaged first launch, 2026-10-08

The ARM64 macOS archive now includes the existing `heph` launcher. Installation
points users to that command, which bootstraps the quickstart project in its
data directory, starts the daemon frozen and opens the guided tour. Installed
use needs Git but no host Rust, Node, npm or checkout.

The tour now registers its exact fixture and carries the returned World and
directed Genome identities. It does not adopt unrelated registered agents.
Run and comparison keys hash full identities, avoiding the shared
`hephaestus:genome:` prefix. Repeating a step reuses its admission. The Arena
step records or reads Selection and verifies its evaluation, World and roles
before showing confidence and gate results. It never verifies invariants or
promotes a Champion. Transport failures show a retry message.

- [Package proof](package-proof.json): local archive SHA-256, architecture,
  bundled Node version, exact match of the tested bundle with the current
  source build, and the 80×24 tour assertions.
- [Actual installed acceptance](package-acceptance.txt): the archive was
  installed in an isolated home and relocated. With host Node/npm hidden,
  the real first-launch Senate choice appeared and Enter opened the tour.
  No evaluator override was set, so this checks the packaged lookup a fresh
  user uses. All six steps completed, including parent 0/1 → candidate 1/1,
  metrics Selection, replay and restored terminal settings. The ordinary
  offline Arena, bundled TUI, coding Gauntlet and reference Gauntlet also
  passed; the reference Gauntlet stopped/restarted its daemon and replayed.
- [Nine package contracts](package-contracts.txt) and
  [seven source-installer contracts](source-contracts.txt) passed. Source
  contracts use fake build tools. The subsequent
  [actual source installation](source-install.txt) built all seven binaries
  from detached commit `5c2e5f4` with a fresh Cargo target directory and
  installed them into a new prefix. The [source tour](source-tour.txt) passed
  all six steps from an empty data directory after the real first-launch
  choice, including measured Arena, Selection, replay and terminal restoration.
  [Source proof](source-proof.json) records toolchain versions and binary hashes.
- [Node 22 UI checks](ui-node22-tests.txt): all 189 passed. The development
  runtime, typecheck and package build also passed. New checks cover canonical
  job identity collisions, directed roles, fixture binding, selection refusal
  and substituted comparison evidence.
- [Claude's initial review](claude-initial-review.txt) found no production-code
  blocker, but caught PTY chunk races and the untested packaged evaluator
  lookup. Both were corrected before the installed acceptance above.
  Its [follow-up review](claude-final-review.txt) caught a remaining run-wait
  mismatch: the harness now allows 25 seconds around the UI's 20-second deadline.
  The full installed acceptance passed again after that correction.

The acceptance script is executable and its Senate check now captures the
complete persona output before searching it, avoiding an early-exit pipe
that falsely failed with a broken-pipe panic. The tour driver waits for complete
rendered results and checks actual scores instead of inferring success from a
fixed pause.

Reproduce on native Apple Silicon macOS with the documented build prerequisites:

```sh
python3 scripts/package_macos.py --arch arm64
scripts/package_macos_acceptance.sh target/packages/hephaestus-v0.1.0-macos-arm64.tar.gz
python3 -m unittest discover -s scripts/tests -p test_macos_package.py -q
python3 -m unittest discover -s scripts/tests -p test_source_install.py -q
npm --prefix apps/hephaestus-tui run typecheck
npm --prefix apps/hephaestus-tui test
```

This is a locally built, unsigned archive. Native Intel acceptance,
first launch after a quarantined browser download and hosted release-workflow
acceptance gates remain to verify. The
fixtures are deterministic and use no paid model; representative hosted-model
quality and the commercial offer remain open in
[product readiness](../../PRODUCT_READINESS.md).
