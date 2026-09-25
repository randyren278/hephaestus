//! Storage backend trait boundary (roadmap item 14: "swappable storage
//! backends without changing domain semantics").
//!
//! `EventLedger` and `ArtifactBackend` name exactly the operations the
//! control plane needs from the canonical event ledger and the
//! content-addressed artifact store. The existing SQLite-backed
//! `EventStore` and filesystem-backed `ArtifactStore` implement them with
//! no behavior change; `storage_contract` proves that implementation
//! against a backend-agnostic contract test suite. A second backend can
//! implement these same traits and be proven with the same suite, without
//! touching domain semantics in `hephaestus-control`, `hephaestus-arena`,
//! or `hephaestus-genome`.

use crate::{ArtifactId, EventInput, EventStore, LedgerError, StoredEvent};

/// The hash-linked, append-only canonical event ledger.
pub trait EventLedger {
    /// Appends one validated event, returning its stored, hash-chained form.
    ///
    /// # Errors
    ///
    /// Rejects invalid input, a duplicate event identity, or a concurrent
    /// change to the ledger tail since it was last verified.
    fn append(&mut self, input: EventInput) -> Result<StoredEvent, LedgerError>;

    /// Replays and verifies the complete hash chain, oldest first.
    ///
    /// # Errors
    ///
    /// Rejects a broken hash chain or any other persisted corruption.
    fn replay_verified(&self) -> Result<Vec<StoredEvent>, LedgerError>;
}

impl EventLedger for EventStore {
    fn append(&mut self, input: EventInput) -> Result<StoredEvent, LedgerError> {
        Self::append(self, input)
    }

    fn replay_verified(&self) -> Result<Vec<StoredEvent>, LedgerError> {
        Self::replay_verified(self)
    }
}

/// The content-addressed artifact store.
pub trait ArtifactBackend {
    /// Stores bytes under their BLAKE3 content address, returning that address.
    ///
    /// # Errors
    ///
    /// Rejects an I/O failure or a stored digest mismatch.
    fn put(&self, bytes: &[u8]) -> Result<ArtifactId, LedgerError>;

    /// Reads bytes back by content address, verifying the digest on every read.
    ///
    /// # Errors
    ///
    /// Rejects a missing object or a digest mismatch.
    fn get(&self, id: &ArtifactId) -> Result<Vec<u8>, LedgerError>;
}

impl ArtifactBackend for crate::ArtifactStore {
    fn put(&self, bytes: &[u8]) -> Result<ArtifactId, LedgerError> {
        Self::put(self, bytes)
    }

    fn get(&self, id: &ArtifactId) -> Result<Vec<u8>, LedgerError> {
        Self::get(self, id)
    }
}

#[cfg(test)]
mod storage_contract {
    //! Backend-agnostic contract tests. Any type implementing `EventLedger`
    //! and `ArtifactBackend` can be proven against these; today only the
    //! existing SQLite/CAS backend is exercised.
    use tempfile::tempdir;

    use super::{ArtifactBackend, EventLedger};
    use crate::{ArtifactId, ArtifactStore, EventInput, EventStore};

    fn assert_event_ledger_contract(mut ledger: impl EventLedger) {
        let first = ledger
            .append(EventInput::new(
                "contract-1",
                "aggregate",
                "contract.tested",
                "actor",
                0,
                b"payload-1",
            ))
            .expect("append first event");
        assert_eq!(first.sequence, 1);

        let second = ledger
            .append(EventInput::new(
                "contract-2",
                "aggregate",
                "contract.tested",
                "actor",
                1,
                b"payload-2",
            ))
            .expect("append second event");
        assert_eq!(second.sequence, 2);
        assert_eq!(second.previous_hash, first.hash);

        assert!(
            ledger
                .append(EventInput::new(
                    "contract-1",
                    "aggregate",
                    "contract.tested",
                    "actor",
                    2,
                    b"duplicate",
                ))
                .is_err(),
            "a duplicate event identity must be rejected"
        );

        let history = ledger.replay_verified().expect("replay verifies");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].event_id, "contract-1");
        assert_eq!(history[1].event_id, "contract-2");
    }

    fn assert_artifact_backend_contract(backend: &impl ArtifactBackend) {
        let id = backend
            .put(b"hello, storage backend")
            .expect("put succeeds");
        assert_eq!(id, ArtifactId::for_bytes(b"hello, storage backend"));
        let bytes = backend.get(&id).expect("get succeeds");
        assert_eq!(bytes, b"hello, storage backend");
        assert!(
            backend
                .get(&ArtifactId::for_bytes(b"never stored"))
                .is_err(),
            "an unstored address must fail closed"
        );
    }

    #[test]
    fn sqlite_event_store_satisfies_the_event_ledger_contract() {
        let directory = tempdir().expect("temp directory");
        let ledger = EventStore::open(directory.path().join("events.sqlite3"))
            .expect("open SQLite event store");
        assert_event_ledger_contract(ledger);
    }

    #[test]
    fn filesystem_artifact_store_satisfies_the_artifact_backend_contract() {
        let directory = tempdir().expect("temp directory");
        let backend = ArtifactStore::open(directory.path()).expect("open artifact store");
        assert_artifact_backend_contract(&backend);
    }
}
