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
//!
//! # Trait inventory for the control-plane migration
//!
//! Before a second backend existed, every crate besides this one called the
//! concrete `EventStore`/`ArtifactStore` types directly. This inventory lists
//! every method call site this crate found by grepping `crates/hephaestus-control`,
//! `crates/hephaestus-arena`, `crates/hephaestus-genome`, and
//! `crates/hephaestus-experience`, so a later agent can route `ControlPlane`
//! (`crates/hephaestus-control/src/server.rs`) through these traits with confidence
//! nothing was missed.
//!
//! ## `EventStore` call sites
//!
//! | Method | Called from | Purpose |
//! |---|---|---|
//! | `EventStore::open` | `hephaestus-control/src/server.rs` (daemon startup, legacy-ledger tests) | Construct the daemon's durable ledger handle. Backend-specific: each backend opens different configuration (a path for both current backends, but a future backend might take a connection pool or a URL), so it stays an inherent constructor rather than a trait method. |
//! | `EventStore::append` | `hephaestus-control/src/server.rs`, `hephaestus-arena/src/lib.rs`, and every crate's test suites | Record a canonical event and get back its sequenced, hash-linked form. |
//! | `EventStore::replay_verified` | `hephaestus-control/src/server.rs` (projection refresh, evidence rehydration), `hephaestus-arena/src/lib.rs`, `hephaestus-experience/src/{recorder,rehydrate,integration}.rs` | Read back verified canonical history to rebuild in-memory projections. |
//!
//! `EventStore`'s tests (`crates/hephaestus-ledger/tests/durable_spine.rs`) also open a
//! raw `rusqlite::Connection` to inject corruption directly. That is deliberately
//! backend-specific test tooling, not a call the control plane makes, so it has no
//! trait method.
//!
//! ## `ArtifactStore` call sites
//!
//! | Method | Called from | Purpose |
//! |---|---|---|
//! | `ArtifactStore::open` | `hephaestus-control/src/server.rs` | Construct the daemon's CAS root. Same reasoning as `EventStore::open` above: backend-specific configuration, not a trait method. |
//! | `ArtifactStore::put` | `hephaestus-control/src/server.rs`, `hephaestus-genome/src/{genome,registry}.rs` (via tests), `hephaestus-experience/src/recorder.rs` | Store evaluator output, receipts, and compiled Genome artifacts by content address. |
//! | `ArtifactStore::get` | `hephaestus-control/src/server.rs`, `hephaestus-genome/src/{world,genome,registry}.rs`, `hephaestus-experience/src/{recorder,rehydrate,integration}.rs`, `hephaestus-arena/src/lib.rs` | Read back and re-verify stored bytes. |
//! | `ArtifactStore::path_for` | Test suites only (`crates/hephaestus-control/tests/control_plane_e2e.rs`, `crates/hephaestus-genome/tests/compiler_contracts.rs`, `crates/hephaestus-experience/tests/evidence_contracts.rs`, `crates/hephaestus-arena/tests/evaluation_contracts.rs`) | Locate the on-disk blob to inject substitution corruption directly. Inherently filesystem-specific (an in-memory backend has no path), so it is not on the trait; corruption tests use a backend-supplied closure instead (see [`crate::contract::assert_artifact_backend_contract`]). |
//!
//! `ArtifactId::parse`, `ArtifactId::as_str`, and `ArtifactId::for_bytes` are called
//! throughout every crate above but belong to the address value type, not to a store,
//! so they are unaffected by this migration.
//!
//! No call site needs any method beyond `append`/`replay_verified` and `put`/`get`, so
//! [`EventLedger`] and [`ArtifactBackend`] keep exactly that two-method core. Each trait
//! adds one default-provided convenience method (`EventLedger::head`,
//! `ArtifactBackend::contains`) because a corruption-detecting `get` already makes
//! "does this address resolve" a one-line fold over `get`, and a later caller (the
//! control-plane migration, or the arena crate's evaluator submission checks) is likely
//! to want it without re-deriving it per backend.
//!
//! # Backends
//!
//! [`EventLedger`] is implemented by [`crate::EventStore`] (SQLite/WAL) and
//! [`crate::FileEventLedger`] (append-only JSONL). [`ArtifactBackend`] is implemented
//! by [`crate::ArtifactStore`] (filesystem CAS) and [`crate::MemoryArtifactBackend`]
//! (in-process `BTreeMap`). All four are exercised by the `storage_contract` tests
//! below, both against this module's own local contract helpers and against the
//! richer, reusable suite in [`crate::contract`] (which additionally proves
//! reopen-durability and takes a backend-supplied corruption closure).

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

    /// Returns the most recent verified event, or `None` for an empty ledger.
    ///
    /// The default implementation replays and verifies the whole chain; a backend
    /// that already tracks its verified head in memory (as both current backends do)
    /// should override this with a cheap accessor.
    ///
    /// # Errors
    ///
    /// Propagates any error [`EventLedger::replay_verified`] returns.
    fn head(&self) -> Result<Option<StoredEvent>, LedgerError> {
        Ok(self.replay_verified()?.into_iter().next_back())
    }
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

    /// Reports whether `id` currently resolves to verifiable bytes.
    ///
    /// The default implementation is a thin wrapper over [`ArtifactBackend::get`]; it
    /// exists so callers that only need a yes/no answer do not have to discard the
    /// bytes themselves, and so a backend that can answer more cheaply (an index
    /// lookup instead of a full read) can override it.
    fn contains(&self, id: &ArtifactId) -> bool {
        self.get(id).is_ok()
    }
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
    //! and `ArtifactBackend` can be proven against these; all four current
    //! backends (SQLite and JSONL event ledgers, filesystem CAS and in-memory
    //! artifact backends) are exercised, both against this module's own local
    //! helpers below and against the richer, reusable suite in
    //! [`crate::contract`].
    use std::fs;

    use tempfile::tempdir;

    use super::{ArtifactBackend, EventLedger};
    use crate::{
        ArtifactId, ArtifactStore, EventInput, EventStore, FileEventLedger, MemoryArtifactBackend,
    };

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

    #[test]
    fn jsonl_event_ledger_satisfies_the_event_ledger_contract() {
        let directory = tempdir().expect("temp directory");
        let ledger = FileEventLedger::open(directory.path().join("events.jsonl"))
            .expect("open JSONL event ledger");
        assert_event_ledger_contract(ledger);
    }

    #[test]
    fn memory_artifact_backend_satisfies_the_artifact_backend_contract() {
        let backend = MemoryArtifactBackend::new();
        assert_artifact_backend_contract(&backend);
    }

    #[test]
    fn sqlite_event_ledger_satisfies_the_shared_contract_suite() {
        let directory = tempdir().expect("temp directory");
        let database = directory.path().join("ledger.sqlite3");
        crate::contract::assert_event_ledger_contract(|| {
            EventStore::open(&database).expect("open sqlite ledger")
        });
    }

    #[test]
    fn jsonl_event_ledger_satisfies_the_shared_contract_suite() {
        let directory = tempdir().expect("temp directory");
        let log = directory.path().join("ledger.jsonl");
        crate::contract::assert_event_ledger_contract(|| {
            FileEventLedger::open(&log).expect("open jsonl ledger")
        });
    }

    #[test]
    fn filesystem_cas_satisfies_the_shared_contract_suite() {
        let directory = tempdir().expect("temp directory");
        let store = ArtifactStore::open(directory.path().join("blobs")).expect("open cas");
        crate::contract::assert_artifact_backend_contract(&store, |id| {
            fs::write(store.path_for(id), b"substituted").expect("inject substitution");
        });
    }

    #[test]
    fn memory_artifact_backend_satisfies_the_shared_contract_suite() {
        let store = MemoryArtifactBackend::new();
        crate::contract::assert_artifact_backend_contract(&store, |id| {
            store.corrupt_for_test(id, b"substituted".to_vec());
        });
    }
}
