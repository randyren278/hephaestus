# Handoff: lane/fable-closeout (2026-09-26)

Written by the cloud orchestration session for the owner's local Claude Code session. Everything below is on branch `lane/fable-closeout` (pushed). Delete this file and `handoff/` once absorbed.

## Ground rules that still apply
- Push only to `lane/fable-closeout`. Never force-push, never touch `main`, no tags/releases, no real `codex`/`claude` CLI runs, no secrets, never delete or weaken tests.
- Gates before every push: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --all-features -- -D warnings`; `cargo build --workspace --bins --all-features`; `cargo test --workspace --all-features --no-fail-fast`; `PYTHONPATH=python python3 -m unittest discover -s python/tests`; `npm run typecheck && npm test` in `apps/hephaestus-tui` and `apps/hephaestus-web`; `python3 checks/target_gate.py --manifest checks/checks.json --root .`; `python3 checks/docs_gate.py --root . README.md AUDIT.md docs/*.md` (ROADMAP.md is no longer part of this command; CI was updated to match).
- `ROADMAP.md` and `HEPHAESTUS_MASTER_PLAN.md` are gitignored and removed from the tree (owner request). They still exist in history (`git show df3d7a0:ROADMAP.md > ROADMAP.md`, same for the master plan) and the local copy was never edited after df3d7a0. Keep maintaining them locally if you want; do not commit them. Purging them from history needs a force-push, which is forbidden.

## State of the branch (newest first)
| Commit | What | Gate status |
|---|---|---|
| `518e5d8` | Merge: pin the reference worker once per daemon, re-verify digest before each use (A.2). Lib suite 504 s -> 382 s on its base per the worker's quiet-machine runs. Two tests changed semantics (see below). | fmt, clippy, mutation anchors green on merged tip; **full `cargo test --workspace` on this merged tip NOT completed** (run was killed for budget). Worker ran lib (128/128) and E2E (29/29) on its own base. |
| `12be66f` | Merge: operator console theme + motion (`apps/hephaestus-tui/src/theme.ts`, `motion.tsx`, all screens restyled, `docs/TUI_THEME.md`). | TUI 118/118, typecheck, docs gate green. |
| `c8e3e3f`, `cb61738` | Merge: Forge mutation catalog (`crates/hephaestus-runtime/src/mutation_catalog.rs`), `failure-cluster-v2`, World `mutation_scope` enforcement at propose time, `evolve start --strategy`, `mutation_prioritization` + `gene_selection` actionable, per-Gauntlet-mode discovery test `evolve_promotes_the_fix_for_every_gauntlet_mode_from_the_bundled_fixtures`. | fmt, clippy, mutation anchors, TUI/web green; worker ran arena/runtime suites and targeted control tests; **full control lib suite and E2E on this tip NOT completed**. |
| `a57bf59`.. | README slimmed to 110 lines; `docs/STATUS.md`, `docs/CLI.md`, threat-model "Verifying it yourself"; roadmap/master plan untracked; README icon row removed. | docs gate green. |
| `140d90f`, `2da6d0f` | Zombie-aware liveness probes in E2E/supervisor tests (this container's PID 1 does not reap orphans, so `kill -0` succeeds on zombies); clippy dead-code fix for a macOS-only test helper. | green |
| `589f11f` | Spawned-daemon E2Es for MCP stdio framing and remote workers (TD-12 partly); gateway answers malformed JSON with -32700. | green |
| `0d90e7a` | Python lab cross-check of Rust selection + meta-evolution receipts (item 7), Rust-pinned fixtures under `python/tests/fixtures/`, `docs/LAB_CROSSCHECK.md`. | green |
| `bf171de` | JSONL event ledger + memory artifact backend behind the storage traits, shared contract suite (TD-13 half). | green |
| `6c1d5c9` | Arena verifiers verify against the already-replayed history (`EventIndex`, `*_in` variants); `EvidenceCache` KEPT as optimization (removing it made the lib suite 891 s). TD-16 updated, not closed. | green (full workspace run at that commit: 2 load flakes, both pass alone) |

**First thing to do locally:** run the full gate list on `518e5d8`. Expected trouble spots: `drift_record_derives_from_verified_evidence_and_replays`, `canary_*` latency tests, `real_listener_services_status_and_shutdown_through_the_socket_writer`, `provider_adapter_maps_exit_status_and_interrupt_to_completion_reasons` flake under load and pass alone.

Test-semantics changes made by workers that you should eyeball: `daemon_evaluation_results_replay_and_feed_exact_authenticated_arena_events` now expects a `run` after replacing the public worker binary to SUCCEED from the private snapshot (previously expected rejection); `assert_runtime_directories_clean` allows exactly one `reference-worker-*` snapshot; in `forge_proposal_replays_and_rejects_tampered_selection_and_metadata` the hash-flip sub-case moved to the ledger layer (`EventStore::open` rejects it); two "stores unavailable" sub-cases were removed because per-event store reopening no longer exists.

## Timings (4-core cloud box, so noisy)
- Baseline `cargo test -p hephaestus-control --all-features --lib`: 368 s (123 tests). After A.1: 386 s under load. After A.2 (worker's quiet runs): 504 s baseline vs 382 s (128 tests). Per-test profile: the wall clock is bounded by `meta_evaluate_shows_a_descendant_strategy_reaching_equal_champions_at_lower_cost` (321 s under load, ~18 paired Arena evaluations); next `gene_transfer_trials_record_contradiction_and_speciation` 123 s, `meta_evaluate_runs_two_lineages...` 117 s, `injected_arena_scoring_failure...` 101 s, `canary_staged_rollout...` 86 s. Sum of all test times ~1272 s.
- `--test control_plane_e2e`: 40 s baseline (4 env failures), 44-47 s after with 27-29/29 passing.
- Get per-test times with `RUSTC_BOOTSTRAP=1 cargo test -p hephaestus-control --all-features --lib -- -Z unstable-options --report-time`.

## In-progress work saved as patches (`handoff/wip/*.patch`)
Each patch header names its base commit. Apply with `git apply --3way handoff/wip/<name>.patch` on a branch from that base (or from the tip and resolve), then finish per the brief in `handoff/briefs/`. All were stopped mid-flight for budget; none is verified.
- `b3-drift-to-canary.patch` (2281 lines, base b3f2cd8): World Law `auto_canary_on_drift`, daemon-owned adaptation pipeline (shadow evaluation -> canary stages -> promote/abort), replay verifier, tests. Brief: `handoff/briefs/b3_brief.md`. Roadmap item 12.
- `b4b-storage-traits-remote-arena.patch` (2539 lines, base b3f2cd8): ControlPlane routed through `EventLedger`/`ArtifactBackend`, remote workers leasing Arena trials. Brief: `handoff/briefs/b4b_brief.md`. TD-13 second half, TD-12 last item, item 14.
- `t-heph-launcher-tour.patch` (1724 lines, base 3aeb23a): `heph` binary (`crates/hephaestus-control/src/bin/heph.rs`), `scripts/install.sh`, first-run tour screens in the TUI, `docs/GETTING_STARTED.md`. Owner requirements: `heph` launches (starts daemon if needed, opens TUI), first launch runs a short learn-by-doing tour (4-6 steps, progress shown, Skip/Back/Next, `heph --tour` replays), VERY visual in the hero palette (use `apps/hephaestus-tui/src/theme.ts` + `motion.tsx` now on the branch), fun and enlightening. The README already advertises `scripts/install.sh` then `heph`; `docs/GETTING_STARTED.md` is a placeholder until this lands.
- `b1b-sealed-holdout-improvement.patch` (633 lines, base c8e3e3f): `examples/gauntlet/sealed-holdout-improvement/` fixture + test proving a bootstrap CI above zero on sealed tasks with zero regressions, cross-checked by the Python lab. Item 10's last gap.
- `b1c-multi-candidate-generations.patch` (336 lines, base c8e3e3f): make `candidate_count` actionable (TD-17): several ranked proposals per generation, `EvolutionGenerationPayload.candidates`, trials = 1 + candidates.
- `a3-arena-profiling.patch` (77 lines, base cbbcea1): only instrumentation; the profiling brief `handoff/briefs/a3_brief.md` is the useful part. Suspected per-trial costs: `git worktree add` per trial, guardian+worker spawn, evaluator spawn, `refresh_projection` after every appended event.

## Remaining roadmap gaps after the above land
- Item 12: drift-to-canary automation (patch above).
- Item 14: ControlPlane on the storage traits with the second backend proven end to end; remote Arena-trial leasing; TD-12 leftovers (mutation entries, CI job, coverage for the two binaries).
- Item 10: sealed-holdout improvement proof (patch above); TD-17 candidate_count.
- Perf: the lib suite is still ~6 min; the fix is per-evaluation overhead (brief A3), not verification any more.
- Docs truthfulness sweep of AUDIT.md/TECH_DEBT.md after the patches land; TECH_DEBT next free number is TD-18.
- Skipped by instruction: live Codex/Claude runs, the 30-day proof, repo settings.

## Environment notes for a cloud box
- Worktree agents created by the harness sometimes start from a stale commit; create worktrees yourself (`git worktree add --detach <path> lane/fable-closeout`) and hand agents the path.
- Never share one `CARGO_TARGET_DIR` between worktrees (binaries collide); each worktree needs ~3 GB.
- Medium.com is blocked by the egress proxy.
