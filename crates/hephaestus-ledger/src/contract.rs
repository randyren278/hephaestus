//! A backend-agnostic contract suite for [`crate::EventLedger`] and
//! [`crate::ArtifactBackend`] implementors.
//!
//! These assertions do not belong to any one backend's test module: they exist so
//! `hephaestus-ledger` can prove its two [`crate::EventLedger`] backends (SQLite and
//! JSONL) and its two [`crate::ArtifactBackend`] backends (filesystem CAS and
//! in-memory) uphold the exact same invariants, and so a future backend or another
//! crate's own mock can be checked against the same suite instead of re-deriving it.
//! It is a plain `pub mod`, not `#[cfg(test)]`-gated, precisely so an integration test
//! in another crate can depend on `hephaestus-ledger` normally and call it directly.

use crate::{ArtifactBackend, ArtifactId, EventInput, EventLedger, LedgerError};

/// Asserts sequence monotonicity, hash-chain linkage, duplicate-identity rejection, and
/// (for a durable backend) that verification survives a fresh open of the same storage.
///
/// `open` constructs a fresh handle onto the ledger's backing storage; it is called
/// once to obtain the ledger under test and again after that ledger is dropped, so it
/// must point at the same durable location both times (for example, the same file
/// path) for the "survives reopen" assertions to mean anything.
///
/// # Panics
///
/// Panics with a descriptive message at the first invariant an implementation
/// violates. This is a test helper: a panic here is the point.
pub fn assert_event_ledger_contract<L: EventLedger>(mut open: impl FnMut() -> L) {
    let mut ledger = open();

    let first = ledger
        .append(EventInput::new(
            "contract-event-1",
            "contract:aggregate",
            "created",
            "contract-suite",
            1_000,
            b"first",
        ))
        .expect("append first event");
    assert_eq!(first.sequence, 1, "the first canonical sequence must be 1");
    assert_eq!(
        first.previous_hash, [0u8; 32],
        "the first event must link to the all-zero genesis hash"
    );

    let second = ledger
        .append(EventInput::new(
            "contract-event-2",
            "contract:aggregate",
            "updated",
            "contract-suite",
            2_000,
            b"second",
        ))
        .expect("append second event");
    assert_eq!(second.sequence, 2, "sequences must be contiguous");
    assert_eq!(
        second.previous_hash, first.hash,
        "an event must link to its immediate predecessor's hash"
    );
    assert_eq!(
        ledger.head().expect("head after two appends"),
        Some(second.clone()),
        "head must report the most recently appended event"
    );

    let duplicate = ledger.append(EventInput::new(
        "contract-event-1",
        "contract:aggregate",
        "created",
        "contract-suite",
        3_000,
        b"replay attempt",
    ));
    assert!(
        matches!(duplicate, Err(LedgerError::DuplicateEventId(ref id)) if id == "contract-event-1"),
        "a repeated event identity must be rejected, got {duplicate:?}"
    );
    let history_after_duplicate = ledger
        .replay_verified()
        .expect("verified replay after a rejected duplicate");
    assert_eq!(
        history_after_duplicate.len(),
        2,
        "a rejected duplicate append must not advance history"
    );

    let blank = ledger.append(EventInput::new(
        "",
        "contract:aggregate",
        "created",
        "contract-suite",
        4_000,
        b"blank identity",
    ));
    assert!(
        matches!(blank, Err(LedgerError::EmptyEventField("event_id"))),
        "a blank required field must be rejected, got {blank:?}"
    );

    drop(ledger);
    let reopened = open();
    let replay = reopened
        .replay_verified()
        .expect("verified replay after reopening the same durable storage");
    assert_eq!(
        replay,
        vec![first, second],
        "replay after reopening must reproduce exactly the events that were durably appended"
    );
}

/// Asserts put/get round-tripping, deduplication, fail-closed digest verification, and
/// rejection of an unknown address.
///
/// `corrupt` overwrites whatever `backend` stored for one [`ArtifactId`] with different
/// bytes, without going through [`ArtifactBackend::put`] (which would just compute a
/// new address). It is backend-specific — a filesystem CAS writes to
/// `ArtifactStore::path_for`'s path, an in-memory backend replaces its own entry
/// directly — so the caller supplies it instead of this suite trying to reach into
/// private backend storage.
///
/// # Panics
///
/// Panics with a descriptive message at the first invariant an implementation
/// violates. This is a test helper: a panic here is the point.
pub fn assert_artifact_backend_contract<B: ArtifactBackend>(
    backend: &B,
    corrupt: impl Fn(&ArtifactId),
) {
    let bytes: &[u8] = b"contract-suite candidate artifact";
    let id = backend.put(bytes).expect("put artifact");
    let deduplicated = backend.put(bytes).expect("put identical bytes again");
    assert_eq!(
        id, deduplicated,
        "identical bytes must produce the same content address"
    );
    assert!(
        backend.contains(&id),
        "a stored address must be reported present"
    );
    assert_eq!(
        backend.get(&id).expect("get stored artifact"),
        bytes,
        "get must return exactly the bytes that were stored"
    );

    let unknown = ArtifactId::for_bytes(b"contract-suite never stored");
    assert!(
        backend.get(&unknown).is_err(),
        "an address nothing was stored at must fail to read"
    );
    assert!(
        !backend.contains(&unknown),
        "an address nothing was stored at must not be reported present"
    );

    corrupt(&id);
    assert!(
        matches!(
            backend.get(&id),
            Err(LedgerError::ArtifactHashMismatch { .. })
        ),
        "corrupted bytes must be rejected instead of returned"
    );
}
