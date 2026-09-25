# Hephaestus — Claude continuation handoff

Written 2026-09-24T16:36:53Z; final snapshot verified immediately afterward. This is a cold-start handoff for a new Claude session, not a declaration that the roadmap is finished. Read the boot section and immediate work first; the rest preserves design intent, evidence, and traps.

## 1. Boot check — run first

```bash
cd /Users/randyren/Developer/hephaestus
git branch --show-current
git rev-parse HEAD
git status --short --branch
git log '@{u}..HEAD' --oneline
git -C /Users/randyren/Developer/hephaestus-reference-invariant-mutations status --short --branch
ps -axo pid,ppid,etime,command | rg 'mutation_guard.py|cargo (test|clippy|check)'
```

Expected primary branch: `full-reign/2026-09-23`.
Expected primary HEAD: `a0d3d0f42955433027593a43e11222ff36b0dbf8`.
Expected primary state: six commits ahead of its actual upstream, with this newly written `HANDOFF.md` untracked. Do not use `origin/HEAD..HEAD` as the unpushed set: the actual tracking branch is `origin/full-reign/2026-09-23`.

The important secondary worktree is `/Users/randyren/Developer/hephaestus-reference-invariant-mutations`, branch `test/reference-output-invariant-mutations`, based at `f6f43da`. It contains valuable UNCOMMITTED tests and mutation/CI changes. They are not present in the primary branch yet. Its production `invariants.rs` may have been temporarily patched by the mutation runner; see the final process snapshot at the end of this document before doing anything with that diff.

If HEAD or status differs, inspect the new commits/diff rather than resetting anything. There are many historical worktrees from this long project. Do not prune or delete them blindly.

## 2. THE ONE NEXT ACTION

Inspect the secondary mutation worktree and confirm the interrupted mutation runner has restored `crates/hephaestus-arena/src/invariants.rs` to its committed bytes, while preserving the five intended modified files. Then resume verification of that pending mutation/test change. This comes before another feature or any push.

Useful first check:

```bash
git -C /Users/randyren/Developer/hephaestus-reference-invariant-mutations diff -- crates/hephaestus-arena/src/invariants.rs
```

An empty production-source diff is expected after cleanup. Never blindly commit an active mutant. If a diff remains, inspect it and the runner/process state before restoring only the known temporary mutation.

## 3. User goal and authority

The persistent goal, quoted from the active goal record, is:

> finish everything listed in the roadmap, commit in reasonable chunks and ensure a full suite of testing. all visuals that need ot be created should follow the pixel art style of the hero image. a milestone that i want to be added is fully interactive tui using ink to control hephaestus. i mean the end goal should be a user can install this tool and test their agents (feasibly md files) in the worlds gauntlets genomes etc. plan using astra and deploy work to luna agents orchestraing with astra and verifiying with astra while letting code generation come from luna subagents

The latest functional steering was to investigate slow Rust compilation and make code-quality feedback much faster. That optimization was implemented and verified before work resumed on the broader roadmap. The latest user request is this in-depth handoff and a prompt for Claude to continue.

Work autonomously on authorized reversible implementation and routine decisions. Keep changes surgical, match repository style, state material assumptions, and use observable success criteria. Commit coherent chunks, push the feature branch, and wait for hosted CI before calling a milestone complete. Do not merge/write main, force-push, rewrite history, lower coverage/mutation gates, delete tests to make CI pass, deploy, incur paid model usage, use new credentials, or make destructive migrations without the required explicit authority. Never claim a local mock or a prepared artifact is live end-to-end evidence.

Astra planning/review and Luna implementation were explicitly requested. If Claude cannot access those exact models/tools, say that plainly and use available capabilities transparently; do not claim another model reviewed the work. Existing Codex agent handles should not be assumed available in Claude. All durable work is in Git/worktrees.

The full roadmap is NOT complete. The current work is a prerequisite slice within item 8. Promotion, rollback, automatic failure clustering, and much of items 9–16 remain.

## 4. Verified Git and CI state

Repository: `https://github.com/randyren278/hephaestus`.
Primary local HEAD: `a0d3d0f`.
Last pushed / fully green HEAD: `1ba5b784ae159b577682d5d5e9ee82a37de609b5`.
Last green full CI: https://github.com/randyren278/hephaestus/actions/runs/36015394372 — all 22 jobs passed, including coverage, TUI, and all mutation shards. Live `gh run list` reconfirmed this at handoff.

Six LOCAL, UNPUSHED commits on the primary branch:

| Commit | Meaning |
|---|---|
| `dcc4890` | World-bound Arena output invariant receipts and focused Arena tests |
| `122a84f` | CLI/protocol/server routing, daemon E2E, and docs for `arena invariants` |
| `44cc717` | Clear operator error when an otherwise valid World lacks the optional invariant profile |
| `f6f43da` | Replay discovers invariant events by type OR ID/aggregate prefix, rejecting rewritten event types |
| `558c5de` | Docs clarify separate invariant evidence does not authorize promotion |
| `a0d3d0f` | E2E Busy assertion compares audit count before/after the request |

Original isolated commit hashes, already cherry-picked into primary, are `9dbc269` (Arena), `04f8632`, `6b4e6e9`, `8c81987` (control). Do not cherry-pick these again.

Earlier fully integrated assessment commits are `b564826`, `0127c9f`, `94a0071`, `8272f4f`, `352838d`, `1ba5b78`. The last commit added missing input/evidence guard tests to recover server coverage.

CI run `36014414978` failed only the server 95% coverage floor at 94.9%. Added meaningful invalid-input/missing-reference tests; run `36015394372` then passed all 31 critical modules at >=95% and all 22 jobs. The current new invariant module has NOT yet had hosted coverage or the full matrix run.

## 5. Current pending mutation work — preserve it

Worktree: `/Users/randyren/Developer/hephaestus-reference-invariant-mutations`.
Branch: `test/reference-output-invariant-mutations`.
Base: `f6f43da` (so it lacks primary's later docs wording and Busy audit fix).
No pending mutation work has been committed or integrated as of handoff preparation.

Intended modified files:

- `.github/workflows/ci.yml`: Arena minimum rises to 58; control becomes 60 split `[15,15,15,15]`.
- `checks/checks.json`: adds `invariants.rs` as critical module 32; adds 9 Arena + 2 control mutations; fixes stale selection replay anchor; adds a focused command for the invariant module.
- `python/tests/test_mutation_guard.py`: verifies updated control partition count/shards.
- `crates/hephaestus-arena/tests/evaluation_contracts.rs`: adds exact actor/snapshot assertions and fixes the paired-regression fixture gap.
- `crates/hephaestus-control/tests/control_plane_e2e.rs`: adds a hash-valid rewritten-actor startup rejection test.

Expected final manifest: 299 mutations total, Arena 58, control 60, other scopes 181, 32 critical modules. Actual JSON counts were independently checked. The primary branch still has 288 mutations/31 critical modules until this patch is integrated.

The primary `checks/target_gate.py` currently fails exactly one stale anchor: `control-selection-history-unverified-on-replay`. Adding `verify_invariant_history` beside the old selection/Forge replay calls changed the source anchor. This is repaired in the pending secondary manifest; do not remove the old mutant.

### Mutation evidence and the discovered test gap

The initial new Arena run completed **8/9 KILLED**, one SURVIVED: `arena-invariant-paired-regressions-require-parent-pass`. The fixture originally had candidate violations and paired regressions both equal to 4, so the incorrect formula counting every candidate violation was indistinguishable.

The pending fixture now makes parent and candidate both violate forbidden NUL on visible task A. Candidate violations stay 4; total paired regressions become 3. For the NUL predicate, parent violations=1, candidate violations=2, paired regressions=1. A separate exact-cap UTF-8 output remains to test bytes and the inclusive cap. Maximum-output parent violations becomes 1 after the replacement output. The corrected focused baseline passed **2/2**.

A rerun of all nine began after that passing baseline. Before the handoff interruption, the first four were KILLED, including the formerly surviving paired-regression mutant; later agent update reported six KILLED, with snapshot checking underway. Do not claim 9/9 for this corrected rerun unless the final snapshot below confirms it. The two new control mutants have NOT been run. The agent was instructed to stop safely, restore source, leave changes uncommitted, and start no new work.

Nine Arena mutants cover successful completion, inclusive output cap, forbidden byte, parent-pass regression direction, both-genome check totals, zero-violation contract status, exact supplied event snapshot, fixed event actor, and manifest output-bound range.

Two control mutants cover startup invariant-history verification and refusal of invariant checking while another job owns storage.

### Integrating pending work safely

Finish the focused checks in the secondary worktree, inspect its production-source diff for leftover mutants, then commit only its intended files. Cherry-pick that commit into the primary branch. (Verified 2026-09-24T16:44Z: `git -C <secondary> diff | git apply --check` against primary `a0d3d0f` applies all five files cleanly, so no cherry-pick conflict is expected.) The E2E test file changed independently on primary in `a0d3d0f`; preserve the before/after Busy audit-count assertion if a cherry-pick conflict occurs. Preserve primary docs wording from `558c5de`.

Then update README's two mutation counts (currently 288) to 299 and AUDIT's mutation/module counts (currently 288/31) to 299/32. The mutation agent was explicitly told primary owns these documentation edits.

## 6. What the new invariant slice does

This is finite reference-output contract evidence, not proof of all constitutional Laws or sandbox safety. It does not promote anything.

Worlds bind an optional immutable artifact under `arena.invariant_manifest`. No World schema migration was needed: the existing evaluator artifact map already supports named content-addressed artifacts. Old Worlds remain valid for prior commands but get a clear InvalidRequest when this command is requested without the profile.

The exact canonical profile shape is:

```json
{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4096,"forbidden_ascii_bytes":[0]}
```

The implementation requires byte-exact canonical serialization, schema 1, the fixed algorithm name, maximum bytes 1..=4096, and sorted unique ASCII forbidden bytes. Unknown fields and unsupported values fail. Semantics are versioned; change algorithm identity if semantics change.

For each authenticated visible AND sealed parent/candidate trial, predicates are:

1. Signed terminal completion is Success.
2. Captured stdout byte length is <= the profile maximum.
3. One predicate per forbidden ASCII byte: it is absent from captured stdout.

Signed unsuccessful trials are completion violations, not missing evidence errors. Their actual captured stdout is still checked for size and forbidden bytes. Missing, corrupt, mismatched, or unauthenticated evidence is an error. A regression is counted per `(paired task,predicate)` only when parent passes and candidate fails. Candidate violations are separately retained; zero regressions does not imply zero violations.

The receipt binds evaluation ID/event ID/hash, immutable World, parent/candidate Genome IDs, manifest artifact, both submission artifacts, trial/check denominators, per-predicate counts, candidate violations, paired regressions, World maximum regressions, `regressions_within_budget`, and `candidate_contract_satisfied`.

Receipt bytes are canonical CAS content. Event type is `invariants.recorded`; event ID `arena:invariants:<evaluation-id>:checked`; aggregate `arena:invariants:<evaluation-id>`; actor `arena-plane`. Event envelope contains schema_version, evaluation_id, world_id, receipt_artifact_id. Identical retries return the original receipt/event; replay recomputes expected content from original evidence.

Public Arena functions: `check_reference_output_invariants`, `load_reference_output_invariants`, `verify_reference_output_invariant_event`, `invariant_event_references`. `OperatorInvariantCheck` exposes aggregate receipt/event and retains stores until `into_stores`; task IDs/raw output stay out of public response.

CLI: `hephaestus arena invariants <evaluation-id>`.
Protocol: `Command::ArenaInvariants`; response `ResponseData::ArenaInvariants` containing `InvariantRecord`.
Control derives World from the exact rehydrated evaluation; caller cannot substitute World/metrics. It uses the single-writer storage path and global active-job exclusion. Startup, explicit replay, and projection refresh verify invariant history.

Selection and Forge assessment retain `invariant_gate_verified=false` and `promotion_eligible=false`. A later decision must join exact assessment and invariant evidence and define Champion transition semantics.

## 7. Review findings, decisions, and remaining concerns

- Inspect signed failed-run stdout directly from its authenticated artifact. Existing `resolve_plan` uses empty scoring output for unsuccessful runs; that is not sufficient for these output predicates.
- New checker verifies World-bound run-result signatures plus task/input commitment, World/Genome, seed/environment/budget, completion, cost/latency, and output/trace artifact correspondence. Run events must precede evaluation.
- Existing `verify_submission_evidence` rehydration checks hashes/artifacts/metrics but does not itself call the signature parser. Do not assume every old rehydration helper independently re-verifies a signature. The new checker explicitly does.
- A grounded unit test rejects a forged signer on a valid ledger chain through the same verifier helper. A complete forged checker-path fixture was not added: it requires rewriting multiple mutually bound evaluation/submission artifacts. This is a remaining test-strength consideration, not a claim that checker-path adversarial coverage is exhaustive.
- History scanning only by `event_type` would skip a rewritten invariant type. New control code also recognizes invariant event-ID and aggregate prefixes, then lets the canonical verifier reject the mutation. Real E2E rewrites and re-chains the ledger, requires startup rejection, restores it, then restarts.
- The initial E2E Busy assertion incorrectly required zero lifetime invariant audit events despite an earlier successful call. Primary `a0d3d0f` correctly asserts unchanged count across the Busy request.
- Do not equate exact-output correctness regressions with invariant regressions. They are different receipts and meanings.
- Do not retrofit new flags into old receipts or enable promotion just because an output contract passed.
- New `invariants.rs` must satisfy the 95% critical-module floor. Coverage has not been measured on the current slice; expect additional targeted error-path tests may be needed. `server.rs` was only just above its floor at the last green commit, so it may also require tests.
- No new runtime defect was observed in the passing focused E2E. This does not substitute for the pending full CI run.

## 8. Test evidence and exact commands

Do not rerun expensive suites merely to recreate old evidence. Run the pending changed tests and full hosted gate; broaden after failures or new concerns.

macOS environment: use `DEVELOPER_DIR=/Library/Developer/CommandLineTools`. For isolated worktrees, agents shared `CARGO_TARGET_DIR=/Users/randyren/Developer/hephaestus/target`; coordinate one Cargo process at a time to avoid target locks and invalidation. Approximately 17 GiB disk was free near handoff. Avoid a large local llvm-cov rebuild without checking space; hosted Linux coverage is authoritative here.

Primary current slice evidence:

- Arena `cargo test -p hephaestus-arena --all-features invariant -- --nocapture`: 3 focused tests passed before pending mutation-fixture additions.
- Arena all-target/all-feature Clippy with `-D warnings`: passed.
- Control real daemon E2E below: passed 1/1, 31.55 seconds, on integrated code after Busy assertion fix.
- CLI parser test below: passed 1/1.
- Control all-target/all-feature Clippy: passed, 46.33 seconds.
- Docs gate and diagram gate: passed after primary docs correction.
- Current primary full hosted CI: NOT RUN / NOT PUSHED.
- Corrected secondary Arena baseline: passed 2/2. New mutation pass partial; see final snapshot.
- Secondary target_gate, Python shard test, and diff check passed before the subsequent fixture edit. Recheck after its final edits.

```bash
export DEVELOPER_DIR=/Library/Developer/CommandLineTools
export CARGO_TARGET_DIR=/Users/randyren/Developer/hephaestus/target

# Fast local code-quality loop; optional package argument narrows scope.
scripts/check-fast.sh
scripts/check-fast.sh hephaestus-arena

# Focused Arena invariant contracts.
cargo test -p hephaestus-arena --all-features --test evaluation_contracts reference_output_invariant -- --nocapture

# Real daemon integration.
cargo test -p hephaestus-control --all-features --test control_plane_e2e daemon_evaluation_results_replay_and_feed_exact_authenticated_arena_events -- --exact --nocapture

# CLI parser.
cargo test -p hephaestus-control --all-features --bin hephaestus arena_invariants_maps_evaluation_identity_to_authenticated_command

# Arena new mutants, in the pending worktree after a passing baseline.
python3 checks/mutation_guard.py --manifest checks/checks.json --root . --file-prefix crates/hephaestus-arena/src/invariants.rs --assert-min 9 --skip-baseline

# Static gates.
python3 checks/target_gate.py --manifest checks/checks.json --root .
PYTHONPATH=python python3 -m unittest discover -s python/tests -p 'test_mutation_guard.py' -v
python3 checks/docs_gate.py --root . README.md AUDIT.md ROADMAP.md docs/*.md
python3 checks/docs_gate.py --root . --skip-claims --min-diagrams 1 README.md docs/ARCHITECTURE.md
git diff --check
```

The harness supports repeated `--only` flags (verified during handoff). Use the exact remaining-case commands in the final snapshot below. Do not change the committed ratchet to run a subset. The full hosted control matrix will run all 60.

After integration and local gates:

```bash
git push origin full-reign/2026-09-23
gh run list --branch full-reign/2026-09-23 --limit 3 --json databaseId,headSha,status,conclusion
gh run view RUN_ID --json status,conclusion,jobs
```

Hosted jobs include TUI, full Rust coverage/test/doc gate, and mutation matrix. If coverage fails, download `rust-lcov-1` (attempt suffix may differ): `gh run download RUN_ID -n rust-lcov-1 -D /tmp/hephaestus-lcov-RUN_ID`. Inspect `LF/LH` for the gated file using the actual coverage parser; a naive count of `DA` records did not agree with LLVM's reported LF/LH in a prior run. Do not lower the 95% floor. Add meaningful paths and rerun.

Mutation harness prints `(timeout 240s)` as the configured budget even beside a KILLED result. That label is not a timeout verdict. Trust the final summary's timed-out count and actual failure output. Compilation failures also warrant checking that a mutant failed for its intended behavioral reason.

A few local test binaries paused in `_dyld_start` before printing test output, then eventually passed. This was a launch/scheduling delay, not proof of a Rust test deadlock. Inspect parent/child processes and live tool output before killing anything. Avoid running unrelated Cargo commands simultaneously.

## 9. Compilation turnaround work already completed

`scripts/check-fast.sh` runs Rust formatting and Clippy over all workspace targets/features with warnings denied, or a supplied package. It avoids doing a full test/link/coverage cycle for routine code-quality feedback. Measured warm run was about 0.54 seconds; a source-change rebuild was roughly 33–35 seconds in this session. These are observed host timings, not guaranteed budgets.

Control mutation CI was split into four shards, with deterministic partition tests and source-anchor preflight. Full mutation/coverage suites remain the acceptance gate. They still take minutes because they recompile intentional source changes and exercise real signed/sandboxed fixtures. Do not promise subsecond full-suite verification.

Current pending changes raise control from 58 mutations `[15,15,14,14]` to 60 `[15,15,15,15]`. Keep the four-shard partition exact; do not duplicate or omit entries.

## 10. Roadmap continuation after this slice is green

Read `ROADMAP.md` and `HEPHAESTUS_MASTER_PLAN.md`; they are the authoritative detailed requirements. Do not infer completion from a section heading or existing scaffolding.

Items 1–7 have substantial implemented foundations and prior evidence, but this handoff does not independently recertify every requirement. Item 8 is the active implementation frontier. Item 9 has partial Ink UI/package/Markdown work already present. Items 10–16 have significant remaining work; fixtures or type scaffolding do not establish complete features.

Next item 8 design work after invariant evidence is green:

- Join exact proposal/assessment/evaluation/selection/invariant receipts under one deterministic decision policy.
- Define initial Champion bootstrap authority, current-parent comparison, stale-parent rejection, idempotency, archive/retirement state, and replay projections.
- Preserve reconstructable previous Champion for rollback and add real injected-regression proof.
- Automatic failure clusters and hypothesis generation still need actual evidence; current Forge proposal is an operator-directed single reference-prompt operation flip.
- Do not add a decorative eligibility boolean instead of missing evidence or transitions.

Item 9 must eventually offer fully interactive Ink navigation for home, Arena progress, runs, lineage DAG, Genome diff, receipts, costs, denials, freeze/kill/rollback; responsive real-job PTY proof; usable package installation without source checkout; Markdown agent author/validate/register/inspect/test flows; readable common terminal sizes; a recorded real demo. Existing `apps/hephaestus-tui/src/ui.tsx` currently mainly provides status, freeze/unfreeze, kill, job inspect/cancel by ID, and Arena progress by ID. Do not describe the full UI as complete.

The Markdown/runtime baseline already supports a narrow offline reference instruction language (identity/ascii_uppercase) and actual prompt-dependent behavior. Preserve that truthful boundary; arbitrary hosted-agent capability needs separate real evidence and any paid usage approval.

Later remaining areas: multi-generation evolution and sealed Gauntlet (10), evidence-backed Gene Bank/transfer/speciation (11), drift/shadow/canary/rollback (12), evolving Evolver strategies under immutable Laws (13), web/MCP/remote workers (14), public hardening/release/self-dogfooding (15), and a real monitored 30-day operation proof (16). The final item requires elapsed time and operating resources; never fabricate it or mark it done after a short demo. Continue independent implementation where external prerequisites block a live run.

## 11. File map

Paths below are relative to `/Users/randyren/Developer/hephaestus` unless stated otherwise. Line numbers are navigation hints at a0d3d0f and will drift.

| File / anchor | Role |
|---|---|
| `ROADMAP.md:56` | Current item 8 and later milestone acceptance criteria |
| `HEPHAESTUS_MASTER_PLAN.md` | Full product/master build intent |
| `README.md:268` | Fast local check documentation; mutation count nearby needs update |
| `AUDIT.md:41` | Current evidence/limitations; count/module total at ~45 needs update |
| `scripts/check-fast.sh` | Short formatting/Clippy feedback loop |
| `.github/workflows/ci.yml` | Hosted full suite, coverage, TUI, mutation shards |
| `checks/checks.json` | Critical coverage list, mutation commands/anchors/ratchets |
| `checks/mutation_guard.py` | Source-mutating guard with restoration/process cleanup |
| `checks/coverage_gate.py` | Per-critical-module 95% gate |
| `python/tests/test_mutation_guard.py` | Lifecycle and shard partition proof |
| `crates/hephaestus-arena/src/invariants.rs:41` | Receipt schema |
| `crates/hephaestus-arena/src/invariants.rs:158` | Check/record public entry point |
| `crates/hephaestus-arena/src/invariants.rs:278` | Deterministic aggregate computation |
| `crates/hephaestus-arena/src/invariants.rs:383` | Signed trial output verification |
| `crates/hephaestus-arena/src/invariants.rs:504` | Strict World-bound manifest loading |
| `crates/hephaestus-arena/tests/evaluation_contracts.rs:573` | Main signed invariant fixture and replay assertions |
| `crates/hephaestus-arena/tests/evaluation_contracts.rs:753` | Invalid/missing manifest matrix |
| `crates/hephaestus-arena/src/selection.rs` | Existing measured selection receipt and trust model |
| `crates/hephaestus-control/src/server.rs:2277` | Operator invariant command implementation |
| `crates/hephaestus-control/src/server.rs:3954` | Invariant history verification |
| `crates/hephaestus-control/src/server.rs:3622` | Forge assessment binding logic |
| `crates/hephaestus-control/src/protocol.rs` | Typed API and aggregate response records |
| `crates/hephaestus-control/src/bin/hephaestus.rs` | CLI mapping and human output |
| `crates/hephaestus-control/tests/control_plane_e2e.rs:452` | Large real-daemon paired/selection/Forge/invariant E2E |
| `crates/hephaestus-control/src/server_tests.rs:140` | Forge assessment trust/retry/tamper tests |
| `crates/hephaestus-genome/src/world.rs` | Immutable World artifact map and promotion policy |
| `crates/hephaestus-genome/src/registry.rs` | Sole trusted registration projection |
| `apps/hephaestus-tui/src/ui.tsx` | Current limited Ink operator UI |
| `apps/hephaestus-tui/scripts/pty_smoke.py` | Existing terminal interaction proof harness |
| `docs/GENOMES.md`, `docs/LEDGERS.md`, `docs/CONTROL_PLANE.md` | Current protocol/evidence meanings |
| `docs/MACOS_INSTALL.md`, `scripts/package_macos_acceptance.sh` | Package/install acceptance workflow |

## 12. Divergence and explicit uncertainty

- The earlier fully green result applies to `1ba5b78`, not local `a0d3d0f` or the dirty mutation worktree.
- ROADMAP's invariant-slice description is implementation documentation; full milestone acceptance is still pending coverage/mutation/hosted CI.
- Primary mutation counts remain stale until the secondary patch and docs count update land.
- The corrected mutation rerun is partial at interruption. The initial 8/9 verdict is for the older fixture; do not combine the two into an invented complete result.
- Agent reports about tests are preserved above with their scope. Handoff creation did not rerun the expensive suite, because no new implementation was made for the handoff and the guard was in flight.
- The native CLI parser launch stall eventually resolved and its test passed; it is not an outstanding parser failure.
- No meaningful TODO/FIXME/XXX/HACK findings were found by the handoff scan; the only matches were `mktemp` X placeholders. The roadmap, not TODO comments, holds the outstanding work.

## 13. Definition of done for the immediate slice

- Pending mutation work is safely restored, reviewed, committed, and integrated without losing primary's Busy test fix.
- Corrected Arena baseline passes; nine new Arena mutants and two new control mutants are killed for intended reasons, with zero stale/surviving/timed-out mutations in acceptance.
- `invariants.rs` is critical module 32 and all critical coverage floors remain >=95%.
- Target anchors and shard partition proof pass; actual count 299 agrees with docs/CI minimums.
- Combined formatting/Clippy, meaningful focused tests, docs/diagram checks pass.
- Branch is pushed; complete hosted CI passes on the exact resulting HEAD, including all mutation scopes and TUI.
- Only then proceed to the next item 8 slice. Do not mark the whole goal complete.

## 14. Resume prompt for Claude

Read `/Users/randyren/Developer/hephaestus/HANDOFF.md` completely, run its Boot check, and resume from THE ONE NEXT ACTION. Continue the full `ROADMAP.md` goal autonomously, preserving the user's Astra planning/review and Luna implementation preference where those models are actually available. Finish the pending invariant mutation/coverage/CI milestone first. Preserve both worktrees and all uncommitted work; distinguish local test evidence from exact-HEAD hosted CI. Keep compilation feedback fast with `scripts/check-fast.sh`, use coherent commits, and keep all visual work in the hero's pixel-art style. Do not stop at a plan, do not lower gates, and do not claim the roadmap or 30-day proof is complete without its real evidence.

## 15. Final process / worktree snapshot

This section is finalized after the mutation runner cleanup below. Treat it as newer than earlier in-flight descriptions.

Finalized 2026-09-24T16:36:53Z:

- Mutation rerun tool session `22720` was interrupted with SIGINT on request. The guard ran its cleanup and reported KeyboardInterrupt. This is an intentional interruption, not a passing complete mutation summary.
- No Cargo or mutation runner remains active. A subsequent process scan only matched the handoff's own inspection command. Do not try to resume old tool session IDs in Claude; launch fresh commands.
- Independently verified: secondary `git diff -- crates/hephaestus-arena/src/invariants.rs` is EMPTY. Temporary production mutation restored.
- Exactly five intended files remain modified in the secondary worktree, as listed in section 5. Agent's final stat: 142 insertions, 16 deletions. No commit was made.
- Primary remains `a0d3d0f`, six ahead, plus untracked `HANDOFF.md` only.
- Post-cleanup source anchor gate PASS; all 13 Python mutation-guard tests PASS (6.136 seconds); secondary diff whitespace check PASS.
- Corrected fixture baseline: 2/2 PASS.
- Corrected-run confirmed KILLED: successful-terminal, inclusive cap, forbidden byte, parent-pass regression direction, both-genome total-check count, and zero-violation candidate-contract status.
- Corrected-run PENDING: altered-snapshot mutant (interrupted while active), fixed-actor mutant, manifest-output-bound mutant. Actor and manifest-bound were killed on the earlier fixture, but that is not the final-fixture acceptance result.
- Both new control mutants remain PENDING. No final Clippy on the secondary test additions and no hosted coverage/full CI on the invariant slice yet.
- Mutation runner output was in Codex tool sessions, not a durable log file. Redirect fresh runs through `tee` with shell `pipefail` if you want durable evidence; do not invent a log path from the old session.

Run the remaining three Arena cases from the secondary worktree (with the CLT and shared target environment above):

```bash
python3 checks/mutation_guard.py --manifest checks/checks.json --root . \
  --only arena-invariant-event-rejects-altered-snapshot \
  --only arena-invariant-event-actor-is-fixed \
  --only arena-invariant-manifest-output-bound-is-limited \
  --assert-min 3
```

Then the two control cases:

```bash
python3 checks/mutation_guard.py --manifest checks/checks.json --root . \
  --only control-invariant-history-unverified-at-startup \
  --only control-arena-invariants-allowed-during-active-job \
  --assert-min 2
```

These commands keep baseline checking enabled. IMPORTANT: the secondary control E2E lacks primary's Busy audit fix `a0d3d0f`, so its unmutated control baseline may fail for that already-fixed assertion. Before the two control cases, bring that narrow fix into the secondary worktree while preserving its uncommitted actor-tamper addition, or integrate the pending commit into primary and run the control cases there. Do not accept a mutant killed merely by the known baseline assertion failure. Prefer committing/reviewing the five intended files after Arena checks, cherry-picking into primary (preserving both E2E edits), then running control mutants against the combined corrected E2E.

The handoff itself is intentionally not committed or pushed. No memory files were changed.

## 16. Independent re-verification (Claude, 2026-09-24T16:44:52Z)

A fresh Claude session re-checked this document against ground truth, read-only (no builds, no mutation runs, no file changes besides this document):

| Claim | Result |
|---|---|
| Primary branch/HEAD `full-reign/2026-09-23` @ `a0d3d0f`, ahead 6, only `HANDOFF.md` untracked | CONFIRMED |
| Six unpushed commits `dcc4890`..`a0d3d0f` | CONFIRMED |
| Secondary @ `f6f43da`, exactly the five listed files modified, 142+/16− | CONFIRMED |
| Secondary `invariants.rs` diff empty (no active mutant) | CONFIRMED |
| No Cargo/mutation processes running | CONFIRMED |
| Manifest counts: primary 288 / 31 critical; secondary 299 / 32 (Arena 49→58, control 58→60) | CONFIRMED |
| Secondary ci.yml: Arena min 58; control shards 3–4 min 14→15 | CONFIRMED |
| All five pending `--only` IDs exist once in the secondary manifest; `--only` is repeatable | CONFIRMED |
| Primary `target_gate.py` fails only `control-selection-history-unverified-on-replay`; secondary passes | CONFIRMED |
| README counts at lines 247 and 280, AUDIT count at line 45, all still 288 (/31) | CONFIRMED |
| Last green CI `36015394372` on `1ba5b78`; previous `36014414978` failed | CONFIRMED |
| Pending patch applies cleanly onto primary | CONFIRMED (new finding) |
| ~17 GiB free disk | CONFIRMED |

Recommended order, based on the clean apply: (1) run the three remaining Arena mutants in the secondary worktree; (2) commit the five files there; (3) cherry-pick into primary; (4) run the two control mutants on primary, where the E2E already has the Busy fix, so the control baseline problem in section 15 doesn't come up; (5) update the README/AUDIT counts, run local gates, push, and watch hosted CI.

## 17. How to resume

Run `/clear`, then paste the resume prompt in section 14.
