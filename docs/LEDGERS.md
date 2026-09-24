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

## Current failure evidence

Integration tests use actual on-disk SQLite and artifact directories. They reopen after writes, inject payload, link, sequence, and artifact corruption through independent filesystem/database handles, and require deterministic integrity errors. No database or artifact store is mocked.
