# Hosted Genome delivery and upgrade evidence

Verified on macOS on 2026-10-08 with offline fake Claude executables. These checks prove instruction/model delivery and persisted evidence compatibility. They do not establish live-model quality, actual model selection by a hosted service, or revenue.

The runtime contract tests inspect both Codex and Claude argv and exact framed stdin, including CRLF, Unicode, embedded section labels and trailing spaces. They check model identifier bounds, reference/provider conflicts and rejection by deterministic adapters. The control test `provider_prose_instructions_reach_direct_submitted_and_paired_runs_and_replay` drives direct, submitted and paired execution through the actual supervisor stdin pipe. Its fake checks byte equality against independently written golden frames and requires the parent model `sonnet` or candidate model `opus` matching each frame. Task commitments stay task-only. Parent scores 0/1; candidate scores 1/1. The test reopens the control plane and checks the saved evaluation, submitted job and immutable role models.

For a separate upgrade check, the actual previous binary at commit `7bf55d0` generated a succeeded provider-v1 job and paired evaluation (both roles 1/1), then shut down normally. The current daemon opened the same data directory, verified and replayed the old history, completed a provider-v2 job and paired evaluation (both roles 1/1), and replayed the combined history. The old evaluator and reference worker were kept byte-identical throughout because the existing World binds their digests. Run signatures and canonical Genome/World identities were not rewritten. The previous binary also registered and ran a hosted Genome with an older free-form model label. The new daemon replays that Genome and its signed run, then rejects direct, submitted and paired launches with that invalid identifier before admitting work. The reproduction script counts execution admissions/results and parses Arena admission payloads to export only their environment identities and evaluation IDs; public Arena progress omits environment identities.

- [Previous binary readback](legacy-upgrade.json)
- [New binary readback and combined replay](current-upgrade.json)
- [Reproduction script](upgrade-proof.py)

These JSON files contain public CLI response data. Tokens, signing keys, raw task input and sealed expected outputs are excluded. SHA-256 hashes identify both CLI and daemon binaries used in each phase. The private daemon data is not a repository artifact.

To reproduce from the repository root with a fresh private temporary directory:

```sh
upgrade_root=$(mktemp -d /tmp/heph-upgrade.XXXXXX)
git worktree add --detach "$upgrade_root/legacy-src" 7bf55d0
cargo build --manifest-path "$upgrade_root/legacy-src/Cargo.toml" -p hephaestus-control --bins --all-features --target-dir "$upgrade_root/legacy-target"
cargo build -p hephaestus-control --bins --all-features
python3 docs/evidence/2026-10-08-provider/upgrade-proof.py "$upgrade_root" legacy
python3 docs/evidence/2026-10-08-provider/upgrade-proof.py "$upgrade_root" current
```

The script creates and controls only its own scratch daemons, repository and fake provider. It never invokes a hosted model. Reports (legacy-report.json, current-report.json) and daemon logs remain in the scratch directory for inspection. The checked-in reports are copies named `legacy-upgrade.json` and `current-upgrade.json`. It does not print or copy daemon credentials.

CLI requests append ordinary audit/control events, so event counts and projection hashes can change across stop/start and readbacks. The compatibility assertions concern the preserved records and signatures, not equality of the entire projection after additional events.

After inspecting the scratch reports, remove the task-owned checkout registration with `git worktree remove "$upgrade_root/legacy-src"`.
