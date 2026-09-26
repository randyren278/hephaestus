use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use crate::{ArtifactBackend, ArtifactId, LedgerError};

/// In-process content-addressed artifact store, keyed by [`ArtifactId`].
///
/// This backend holds every blob in a `BTreeMap` guarded by a mutex; nothing is
/// persisted, so it is meant for tests and short-lived tooling, not for the daemon's
/// canonical CAS. Like [`crate::ArtifactStore`], `get` always recomputes the address
/// of the bytes it is about to return and fails closed on a mismatch, so it upholds
/// exactly the same [`ArtifactBackend`] contract as the filesystem backend.
#[derive(Default)]
pub struct MemoryArtifactBackend {
    blobs: Mutex<BTreeMap<ArtifactId, Vec<u8>>>,
}

impl MemoryArtifactBackend {
    /// Creates an empty in-memory artifact store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores bytes and returns their content address, deduplicating identical bytes
    /// already stored at the same address.
    ///
    /// # Errors
    ///
    /// This backend has no failure mode of its own; the `Result` matches
    /// [`ArtifactBackend::put`]'s signature so callers can treat every backend
    /// uniformly.
    pub fn put(&self, bytes: &[u8]) -> Result<ArtifactId, LedgerError> {
        let id = ArtifactId::for_bytes(bytes);
        let mut blobs = self.blobs.lock().unwrap_or_else(PoisonError::into_inner);
        blobs.entry(id.clone()).or_insert_with(|| bytes.to_vec());
        Ok(id)
    }

    /// Reads and verifies bytes at a content address.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::ArtifactNotFound`] when nothing is stored at `id`, or
    /// [`LedgerError::ArtifactHashMismatch`] when the stored bytes were replaced (see
    /// [`MemoryArtifactBackend::corrupt_for_test`]).
    pub fn get(&self, id: &ArtifactId) -> Result<Vec<u8>, LedgerError> {
        let blobs = self.blobs.lock().unwrap_or_else(PoisonError::into_inner);
        let bytes = blobs
            .get(id)
            .cloned()
            .ok_or_else(|| LedgerError::ArtifactNotFound(id.as_str().to_owned()))?;
        let actual = ArtifactId::for_bytes(&bytes);
        if actual != *id {
            return Err(LedgerError::ArtifactHashMismatch {
                expected: id.as_str().to_owned(),
                actual: actual.as_str().to_owned(),
            });
        }
        Ok(bytes)
    }

    /// Overwrites whatever is stored at `id` with different bytes, without
    /// recomputing the address, so the next [`MemoryArtifactBackend::get`] observes a
    /// digest mismatch instead of the original content.
    ///
    /// This exists to exercise fail-closed digest verification the same way the
    /// filesystem CAS's tests write directly to `ArtifactStore::path_for`'s path; a
    /// backend this small has no other way to simulate an externally substituted
    /// blob. It is real, always-available API rather than `#[cfg(test)]`-gated, so
    /// another crate's own tests against this backend can use it too — like
    /// [`crate::contract::assert_artifact_backend_contract`] does.
    pub fn corrupt_for_test(&self, id: &ArtifactId, bytes: Vec<u8>) {
        let mut blobs = self.blobs.lock().unwrap_or_else(PoisonError::into_inner);
        blobs.insert(id.clone(), bytes);
    }
}

impl ArtifactBackend for MemoryArtifactBackend {
    fn put(&self, bytes: &[u8]) -> Result<ArtifactId, LedgerError> {
        MemoryArtifactBackend::put(self, bytes)
    }

    fn get(&self, id: &ArtifactId) -> Result<Vec<u8>, LedgerError> {
        MemoryArtifactBackend::get(self, id)
    }
}
