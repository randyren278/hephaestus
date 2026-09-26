use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::LedgerError;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Canonical lowercase BLAKE3 content address.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// Parses a canonical 64-character lowercase hexadecimal BLAKE3 address.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::InvalidArtifactId`] for malformed or non-canonical text.
    pub fn parse(value: impl Into<String>) -> Result<Self, LedgerError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(LedgerError::InvalidArtifactId(value));
        }
        Ok(Self(value))
    }

    /// Returns the canonical hexadecimal address.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Calculates the canonical content address without writing the bytes.
    ///
    /// This supports validate-before-publish workflows that must compare a
    /// proposed artifact with an immutable commitment before mutating storage.
    #[must_use]
    pub fn for_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }
}

/// Filesystem-backed content-addressed artifact store.
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    /// Opens an artifact root, creating it when absent.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] when the filesystem cannot create the root.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, LedgerError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Stores bytes atomically and returns their content address.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError`] for filesystem failures or an existing corrupt blob.
    pub fn put(&self, bytes: &[u8]) -> Result<ArtifactId, LedgerError> {
        let id = ArtifactId::for_bytes(bytes);
        let final_path = self.path_for(&id);
        if final_path.exists() {
            self.get(&id)?;
            return Ok(id);
        }

        let Some(parent) = final_path.parent() else {
            return Err(LedgerError::InvalidArtifactId(id.as_str().to_owned()));
        };
        fs::create_dir_all(parent)?;
        let temporary_path = parent.join(format!(
            ".tmp-{}-{}",
            std::process::id(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let write_result = write_and_sync(&temporary_path, bytes)
            .and_then(|()| fs::rename(&temporary_path, &final_path))
            .and_then(|()| File::open(parent)?.sync_all());
        if write_result.is_err() {
            let _ignored = fs::remove_file(&temporary_path);
        }
        write_result?;
        Ok(id)
    }

    /// Reads and verifies bytes at a content address.
    ///
    /// # Errors
    ///
    /// Returns [`LedgerError::ArtifactHashMismatch`] when stored bytes were replaced.
    pub fn get(&self, id: &ArtifactId) -> Result<Vec<u8>, LedgerError> {
        let bytes = fs::read(self.path_for(id))?;
        let actual = ArtifactId::for_bytes(&bytes);
        if actual != *id {
            return Err(LedgerError::ArtifactHashMismatch {
                expected: id.as_str().to_owned(),
                actual: actual.as_str().to_owned(),
            });
        }
        Ok(bytes)
    }

    /// Resolves the sharded on-disk path for an address.
    #[must_use]
    pub fn path_for(&self, id: &ArtifactId) -> PathBuf {
        self.root.join(&id.as_str()[..2]).join(id.as_str())
    }
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
