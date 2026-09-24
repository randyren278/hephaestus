# Offline reference-instruction Gauntlet fixture

This fixture is a small, repeatable foundation for exercising the production
Hephaestus daemon, CLI, Arena, and persistent receipt path. It runs three paired
comparisons through the real binaries: an improvement, a regression, and a tie.
Every pair uses the same registered World and the same visible and sealed task
inputs. Tasks cover multiline text and non-ASCII characters.

The reference instruction language has only two operations: `identity` and
`ascii_uppercase`. This fixture does not implement the seven behavioral Gauntlet
modes or unattended multi-generation evolution from roadmap item 10.

## Run

On macOS, build the production binaries and invoke the script with explicit
paths:

```sh
cargo build --release --workspace
python3 scripts/gauntlet_reference.py \
  --daemon-bin "$PWD/target/release/hephaestusd" \
  --cli-bin "$PWD/target/release/hephaestus" \
  --evaluator-bin "$PWD/target/release/hephaestus-reference-evaluator" \
  --worker-bin "$PWD/target/release/hephaestus-reference-worker"
```

The script starts an isolated daemon against a new owner-only scratch Git repo
and data directory. It never attaches to an existing daemon, removes a supplied
path, or calls the CLI's daemon-stop command. It terminates only the daemon
process it started and retains the scratch directory printed in the bounded JSON
report. Subprocess output is streamed into bounded captures; the fixture daemon
keeps only a 16 KiB diagnostic tail in memory. Pass `--work-dir` to choose a new,
nonexistent scratch directory.

The runner publishes task manifests from outside the candidate repo, registers
the World and Markdown Genomes through the CLI, unfreezes its isolated daemon,
and performs exactly three bounded Arena evaluations. It records evaluation and
selection event metadata, kills and restarts its own daemon, then replays the
signed runtime results and selection receipts and confirms that each persisted
selection is unchanged.
Arena executions use the World's zero-cost ceiling and daemon-owned bounded
wall/output limits. No hosted model or paid service is used.

The JSON output contains a compact summary and scratch path, not task payloads,
evaluator internals, or arbitrary command output. Promotion remains disabled:
the selection receipt's independent invariant gate is unverified.
