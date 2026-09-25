# Adversarial coverage

Roadmap item 15 asks for a named adversarial suite covering crash,
corruption, hash-tamper, sandbox-escape, evaluator-leakage, budget-bypass,
timeout, partial-promotion, duplicate-event, and rollback attempts. Most of
these categories already have real, fast, focused tests living beside the
code they attack — this table names them instead of duplicating them in a
separate suite. Two new tests fill the two attack shapes nothing else
exercised (see below the table).

| Category | Test | Where |
|---|---|---|
| Crash | `guardian_contains_worker_after_daemon_crash_and_replay_marks_job_interrupted` | `crates/hephaestus-control/tests/control_plane_e2e.rs` |
| Crash | `async_job_status_and_cancellation_remain_responsive_and_confirm_process_death` | `crates/hephaestus-control/tests/control_plane_e2e.rs` |
| Crash | `timeout_output_overrun_and_crash_all_cleanup_worker_roots` | `crates/hephaestus-runtime/tests/worker_contracts.rs` |
| Corruption | `corrupt_or_missing_output_is_rejected_without_evaluation_writes` | `crates/hephaestus-arena/tests/evaluation_contracts.rs` |
| Corruption | `compilers_reject_invalid_policy_and_corrupt_artifacts` | `crates/hephaestus-genome/tests/compiler_contracts.rs` |
| Corruption | `reference_runtime_returns_recorder_when_recovery_detects_tampered_history`, `candidate_runtime_returns_recorder_when_recovery_detects_tampered_history` | `crates/hephaestus-control/src/server_tests.rs` |
| Hash-tamper | `event_replay_detects_payload_and_chain_tampering` | `crates/hephaestus-ledger/tests/durable_spine.rs` |
| Hash-tamper | `artifact_store_deduplicates_and_detects_substitution` | `crates/hephaestus-ledger/tests/durable_spine.rs` |
| Hash-tamper | `trusted_experience_rejects_missing_and_tampered_primary_artifact` | `crates/hephaestus-experience/tests/evidence_contracts.rs` |
| Hash-tamper | `arena_job_record_validation_rejects_plan_and_event_tampering`, `cluster_and_invariant_histories_reject_tampered_events`, `gene_bank_history_rejects_tampering_retyping_and_reordering` | `crates/hephaestus-control/src/server_tests.rs` |
| Sandbox escape — read a sealed/evaluator file | `worker_policy_denies_protected_paths` | `crates/hephaestus-runtime/tests/worker_contracts.rs` (macOS Seatbelt only) |
| Sandbox escape — write outside the worker's own root | `sandbox_escape_write_outside_worker_root_is_denied` (**new**) | `crates/hephaestus-runtime/tests/adversarial.rs` (macOS Seatbelt only) |
| Sandbox escape — open a real network connection | `sandbox_escape_network_connection_is_denied` (**new**) | `crates/hephaestus-runtime/tests/adversarial.rs` (macOS Seatbelt only) |
| Evaluator leakage | `worker_policy_denies_protected_paths` (candidate cannot read bytes placed under the evaluator's protected path; output is checked for the sealed content) | `crates/hephaestus-runtime/tests/worker_contracts.rs` (macOS Seatbelt only) |
| Budget bypass | `runtime_fails_closed_for_expiry_network_and_output_budget`, `supervisor_enforces_combined_output_and_wall_budgets`, `budgets_and_run_specs_reject_invalid_inputs` | `crates/hephaestus-runtime/tests/runtime_contracts.rs` |
| Budget bypass | `evaluation_budget_boundaries_are_validated_before_execution`, `evolve_budget_exhaustion_stops_a_run_before_its_generation_limit` | `crates/hephaestus-control/src/server_tests.rs` |
| Timeout | `arena_overall_deadline_stops_slow_trial_without_committing_evaluation`, `async_wall_timeout_is_durable_replayable_and_keeps_daemon_available` | `crates/hephaestus-control/tests/control_plane_e2e.rs` |
| Timeout | `supervisor_rejects_unrepresentable_deadline_before_spawn` | `crates/hephaestus-runtime/tests/runtime_contracts.rs` |
| Partial promotion | `champion_promotion_requires_satisfied_invariant_contract` (a `metrics_passed` assessment without a satisfying invariant receipt is refused, not partially applied) | `crates/hephaestus-control/src/server_tests.rs` |
| Partial promotion | `evolve_start_completes_three_generations_with_one_promotion_and_replays` | `crates/hephaestus-control/src/server_tests.rs` |
| Duplicate event | `duplicate_event_ids_are_rejected_without_advancing_history` | `crates/hephaestus-ledger/tests/durable_spine.rs` |
| Duplicate event | `identical_retry_returns_existing_event_after_reopen`, `wrong_event_identity_and_conflicting_retry_fail_before_writes` | `crates/hephaestus-arena/tests/evaluation_contracts.rs` |
| Duplicate event | `identical_duplicate_registration_is_idempotent_and_keeps_first_sequence`, `conflicting_duplicate_registration_is_rejected` | `crates/hephaestus-genome/tests/registry_contracts.rs` |
| Duplicate event | `terminal_job_kill_is_idempotent_and_selection_errors_map_to_safe_api_states`, `cluster_analyze_records_deterministic_clusters_and_idempotent_replay` | `crates/hephaestus-control/src/server_tests.rs` |
| Rollback | `champion_seed_promote_and_rollback_join_verified_evidence_and_replay` | `crates/hephaestus-control/src/server_tests.rs` |

## The three new tests

`crates/hephaestus-runtime/tests/adversarial.rs` adds three tests. Two are
gated `#[cfg(target_os = "macos")]` because macOS Seatbelt is the only
isolation backend this repository has (`docs/THREAT_MODEL.md`); on any other
host `IsolationPolicy::detect` reports `Unavailable` and there is no sandbox
to attack:

- `sandbox_escape_write_outside_worker_root_is_denied` — runs `/usr/bin/touch`
  against a path in a separate temp directory outside the worker's own root
  and asserts the file was never created. This is a different property from
  `worker_policy_denies_protected_paths`: it proves the sandbox's *default
  deny* covers the whole filesystem, not just paths explicitly listed as
  protected.
- `sandbox_escape_network_connection_is_denied` — runs `/usr/bin/nc -z` against
  a public IP address through an `IsolatedWorker` and asserts the connection
  attempt fails closed. `IsolationPolicy::worker_command` always builds its
  Seatbelt profile with network denied for every worker domain, so this
  holds regardless of the caller-supplied policy or network reachability.
- `candidate_sandbox_never_receives_merge_or_release_credentials` — the
  self-dogfooding credential-containment proof required by
  `docs/SELF_DOGFOODING.md`. Runs cross-platform (no live sandbox needed): it
  re-executes the test binary as a child whose ambient environment is
  poisoned with `GIT_ASKPASS`, `GIT_SSH_COMMAND`, `GITHUB_TOKEN`, `GH_TOKEN`,
  `SSH_AUTH_SOCK`, `NPM_TOKEN`, `CARGO_REGISTRY_TOKEN`, `COSIGN_PASSWORD`,
  and `SIGSTORE_ID_TOKEN`, has that child spawn a real `IsolatedWorker`
  running `/usr/bin/env`, and asserts none of the poison appears in what the
  worker itself observed. Verified to actually catch a regression: removing
  `IsolatedWorker::execute`'s `Command::env_clear()` call locally made this
  test fail (`OutputBudgetExceeded` from the leaked variables overflowing
  the worker's output budget), confirming it is not a tautology.

## Honest gaps

- Every sandbox-escape and evaluator-leakage test above is macOS-only and
  does not run in `deterministic`, `mutation`, or any other ubuntu-24.04
  CI job — it only runs when a developer executes `cargo test` on a Mac, or
  in a future macOS CI runner that does not exist yet. CI proves the
  Seatbelt *profile text* is correct (`seatbelt_profile_is_deny_by_default_and_capability_exact`
  in `crates/hephaestus-runtime/src/isolation.rs`) but not that a live
  sandbox actually enforces it. Tech debt: add a `macos-14` CI job that runs
  the `#[cfg(target_os = "macos")]` test set.
- "Duplicate-event" above covers idempotent retry and rejection of a
  conflicting resubmission; it does not cover replaying the same event twice
  concurrently from two threads (a race, not a sequential resubmission).
- No fuzzing or property-based testing exists for any parser (Genome
  Markdown, World JSON/YAML, the daemon's wire protocol); coverage here is
  example-based only.
