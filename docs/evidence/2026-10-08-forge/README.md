# Forge revision checkpoint, 2026-10-08

This evidence uses offline provider scripts on macOS. It verifies the command,
immutable configuration, evidence chain and history upgrade. Representative
hosted-model quality remains unproven.

The actual archived binaries from commit `7bf55d0` created a schema-1 catalog
proposal under a World with harness mutation and invariant policies. The new
daemon opened that data directory and returned the identical old proposal on
retry. Its canonical payload SHA-256 stayed unchanged after the upgrade and a
second restart. The old evaluator and worker stayed byte-identical because the
World binds their digests.

The new CLI then registered two hosted prose agents, ran their source
comparison, and recorded a schema-2 prompt revision with a hypothesis. The
scripted Claude adapter checked the exact UTF-8 instruction frame and explicit
model flag. The revised child scored 1/1 visible tasks against its parent's 0/1.
A fresh Selection and Forge assessment passed measured metrics; verified
invariants and explicit operator commands then seeded and promoted the child.
Mixed history replayed 122 events before shutdown. A subsequent daemon restart
preserved the proposal bytes, exact Unicode/CRLF/trailing-space prompt and
Champion identity.

- [Old daemon report](legacy-upgrade.json): binary digests, schema-1 proposal,
  canonical payload digest and replay.
- [New daemon report](current-upgrade.json): old payload preservation, schema-2
  revision, comparison, assessment, invariants, promotion and mixed replay.
- [Restart report](restart-upgrade.json): preserved payloads, Champion and replay.
- [Reproduction script](upgrade-proof.py): creates only scratch data, its own
  repository and an offline provider executable; daemon credentials stay there.
- [Mutation guard output](mutation-guard.txt): all 30 affected mutations killed;
  no survivors, stale entries or timeouts.
- [Claude Code review](claude-review.txt): read-only final code review, with no
  blocking findings; it independently notes the missing UI and live-model proof.

Six scoped revision tests cover command validation, historical schema-1 bytes,
exact instruction delivery, retry conflicts, World/provider limits, child
configuration preservation, assessment, promotion and restart. Tamper tests
rebuild valid ledger hash chains before reopening, and also test modified model
and unrelated-artifact children that pass the Genome compiler. The source-reader
checks include a FIFO, bounded reads when metadata underestimates size, and a
separate intentionally blocked reader to verify the mutation fixture's recovery.

The full workspace test run passed after correcting the macOS canonical-path
expectation in the new CLI test. Coverage initially found the FIFO fixture's
unexercised recovery path; a deliberately blocked reader now exercises that
cleanup. The resulting full report passes all 51 critical floors: the source
reader is at 93.2% and the revision module at 92.5%, above the unchanged 92% floor.
The 18 new mutation entries have targeted tests and ratcheted CI counts. Their
guard and 12 affected existing Forge checks all passed: 30 killed, none survived,
and none were stale or timed out. The unmutated workspace baseline passed before
the guard. Afterward, the restored source rebuilt and repeated all three actual
binary upgrade phases in a fresh scratch directory. These reports contain those
rebuilt binary hashes. This remains a scoped implementation checkpoint; guided
terminal authoring and representative hosted-model quality are still required.

To reproduce from the repository root:

```sh
upgrade_root=$(mktemp -d /tmp/heph-forge.XXXXXX)
git worktree add --detach "$upgrade_root/legacy-src" 7bf55d0
cargo build --manifest-path "$upgrade_root/legacy-src/Cargo.toml" -p hephaestus-control --bins --all-features --target-dir "$upgrade_root/legacy-target"
cargo build -p hephaestus-control --bins --all-features
python3 docs/evidence/2026-10-08-forge/upgrade-proof.py "$upgrade_root" legacy
python3 docs/evidence/2026-10-08-forge/upgrade-proof.py "$upgrade_root" current
python3 docs/evidence/2026-10-08-forge/upgrade-proof.py "$upgrade_root" restart
```

The data directory upgrade is forward-only. Older binaries cannot decode a new
revision command audit or schema-2 proposal. Original history stays preserved in
the initial report, and the script does not rewrite stored events.

The script writes legacy-report.json, current-report.json and
restart-report.json in the scratch root. This evidence directory copies those
reports as `legacy-upgrade.json`, `current-upgrade.json` and
`restart-upgrade.json`; their JSON contents are unchanged.
