# Ledgers and Artifacts

## Canonical event spine

`crates/hephaestus-ledger/src/event_store.rs` owns the first canonical ledger. SQLite runs in WAL mode with full synchronous durability. Each append uses an immediate transaction, rejects duplicate identifiers and empty canonical fields, compares the database tail with the in-memory verified head, assigns the next global sequence, and hashes every event field together with the predecessor hash.

Opening a store verifies the complete chain before accepting writes. Normal appends compare only the tail, avoiding quadratic ingestion while enforcing the single-writer contract. A restart replays and verifies all history before reconstructing the head.

## Deterministic replay

Replay requires contiguous sequences, exact predecessor links, 32-byte hashes, and a recomputed BLAKE3 event hash. Derived projections are disposable: the integration suite rebuilds a projection twice from the on-disk ledger and requires identical state.

## Artifact CAS

`crates/hephaestus-ledger/src/artifact_store.rs` addresses bytes by canonical lowercase BLAKE3 hash and shards them by the first two hexadecimal characters. Writes use a create-new temporary file, file fsync, atomic rename, and directory fsync. Reads always recompute the address and fail on substitution.

The CAS root is a daemon-owned trust boundary. Before an external API exists, the daemon milestone must create it with owner-only permissions and prevent candidate sandboxes from accessing it.

## Arena evaluation evidence

Arena consumes only authenticated `run.result_recorded` events from verified history. Its evaluator-owned submission evidence commits each signed run event identity and hash, source revision, completion reason, latency, actual cost, and bounded output, diagnostic, and trace artifact identities. The final operator receipt binds those submissions to the exact World, seed, environment, evaluator, hard budget, parent and candidate Genomes, and aggregate scores. The resulting evaluation event hash commits to that exact receipt.

An `OperatorEvaluation` derives aggregate `SelectionEvidence` from that detailed receipt. The aggregate deliberately removes task identities and order, inputs, expectations, outputs, and artifact addresses while retaining the event binding, correctness outcomes, reliability, cost, and latency consumed by deterministic measured selection. It is an operator capability, not a replacement for the evaluator-owned receipt or a promotion decision.

`arena select <evaluation-id>` rehydrates that capability and its exact registered `CompiledWorld`, recomputes a deterministic histogram bootstrap and measured Pareto gates, then stores canonical receipt JSON by BLAKE3 address. The `selection.recorded` event binds the evaluation identity, World identity, and receipt artifact address; the receipt itself binds the exact verified evaluation event ID/hash, policy, algorithm version, resample count, seed, outcome histogram, confidence interval, and aggregate fitness dimensions. Retry and history replay rehydrate the source evaluation and recompute the receipt before accepting its canonical bytes. Selection events without a prior verified evaluation, with a different World, an invalid envelope, or a receipt that differs from the recomputation fail closed.

`genome assess` accepts only a recorded child-selection event whose evaluation matches the named Forge proposal's exact parent and proposed child. The `forge.assessed` event binds the proposal event identity/hash, child selection event identity/hash and receipt address, and evaluation provenance. Its stable event ID is `forge-assessment:<assessment-id>:recorded`, under the proposal's `forge:<proposal-id>` aggregate. The assessment outcome is derived from the verified receipt's `metrics_eligible` value (`metrics_passed` or `metrics_rejected`); retries return the same record and conflicting reuse is rejected. Assessment remains evidence only: it does not mutate Genome lineage or authorize a promotion, and its invariant and promotion flags are false.

The current selection receipt distinguishes measured `correctness_regressions` from policy `maximum_regressions`, which remains an invariant regression budget. Reference-output invariant checks are recorded separately; selection and Forge assessment still leave `invariant_gate_verified` and `promotion_eligible` false. Neither receipt can authorize a Champion transition.

`arena invariants <evaluation-id>` rehydrates the exact Arena evaluation, resolves its registered World, and checks parent/candidate outputs against that World's reference-output predicates. The operator receipt contains aggregate trial/check counts, per-predicate parent and candidate violation totals, paired regressions, candidate contract status, and the World's maximum regression allowance. It contains no task identities or raw outputs. The canonical receipt is stored by content address and the `invariants.recorded` event uses ID `arena:invariants:<evaluation-id>:checked` and aggregate `arena:invariants:<evaluation-id>`. Retry, startup and replay recompute the same output-bound receipt before accepting the event. Authenticated non-success results count as completion violations; missing or unauthenticated result evidence fails closed. This record does not modify selection or assessment flags and does not enable promotion.

`champion seed|promote|rollback` append `champion.transitioned` events with ID `champion:<transition-id>:recorded` under aggregate `champion:<world-id>`. Every payload after the seed binds the previous Champion and the prior transition's event ID and hash, so transitions form a per-World chain. A promotion additionally binds the Forge assessment event ID/hash, the selection receipt address, and the invariant event ID/hash and receipt address for the same evaluation. Startup, replay, and projection refresh recompute each transition from exactly the history before it, including freeze state and the evidence it joined. A rewritten payload, event type, identity, or order fails closed, and so does a transition the policy would not have admitted at that point.

## Current failure evidence

Integration tests use actual on-disk SQLite and artifact directories. They reopen after writes, inject payload, link, sequence, and artifact corruption through independent filesystem/database handles, and require deterministic integrity errors. No database or artifact store is mocked.

## Storage backends

`crates/hephaestus-ledger/src/storage.rs` defines two traits, each with a two-method core and one default-provided convenience method: `EventLedger` (`append`, `replay_verified`, plus `head`) and `ArtifactBackend` (`put`, `get`, plus `contains`). The file's top doc comment is a call-site inventory: every `EventStore`/`ArtifactStore` method the control plane, arena, genome, and experience crates call, and why each either became a trait method or stayed backend-specific (constructors like `open`, and filesystem-only inspection like `path_for`, used only by test tooling).

Each trait now has two implementors:

- `EventLedger`: `EventStore` (the original SQLite/WAL ledger, `crates/hephaestus-ledger/src/event_store.rs`) and `FileEventLedger` (an append-only JSONL ledger, `crates/hephaestus-ledger/src/file_event_store.rs`). Both call the same shared `hash_event`/`validate_input`/`GENESIS_HASH` (widened to `pub(crate)` in `crates/hephaestus-ledger/src/event_store.rs` and reused directly, not duplicated) so identical inputs produce an identical hash chain regardless of backend. `FileEventLedger` hex-encodes every binary field into a single JSON line per event, appends with one `write_all` followed by `sync_all`, and on open treats a file that does not end in `\n` as having a torn trailing write: everything after the last complete `\n` is dropped silently and the file is truncated back to its last complete line, while a fully newline-terminated line that fails to parse or fails hash-chain verification is reported as a real integrity error, exactly like the SQLite backend reports one.
- `ArtifactBackend`: `ArtifactStore` (the original filesystem CAS, `crates/hephaestus-ledger/src/artifact_store.rs`) and `MemoryArtifactBackend` (an in-process `BTreeMap`, `crates/hephaestus-ledger/src/memory_artifact_store.rs`). Both recompute the requested address from the bytes about to be returned and fail closed on a mismatch.

`crates/hephaestus-ledger/src/contract.rs` is a plain `pub mod`, not test-gated, holding `assert_event_ledger_contract` and `assert_artifact_backend_contract`: backend-agnostic assertions for sequence monotonicity and hash-chain linkage, duplicate-identity rejection, replay verification surviving a reopen, artifact put/get round-tripping and deduplication, fail-closed digest verification given a backend-supplied corruption closure, and rejection of an unknown address. `crates/hephaestus-ledger/src/storage.rs`'s `storage_contract` test module runs it against all four backends. `crates/hephaestus-ledger/tests/file_ledger_durability.rs` is the JSONL backend's counterpart to `crates/hephaestus-ledger/tests/durable_spine.rs`: it tampers with the on-disk file directly (payload byte flip, broken chain link, sequence gap, a removed middle event, a physically reordered pair of lines) and separately proves a torn trailing write is dropped without error while a complete-but-malformed line is not.

The daemon's call-site migration — routing `ControlPlane` (`crates/hephaestus-control/src/server.rs`) through `EventLedger`/`ArtifactBackend` instead of the concrete `EventStore`/`ArtifactStore` types — is tracked in `TECH_DEBT.md` under TD-13 and left to a later change; both trait implementors already pass the same contract suite today.
