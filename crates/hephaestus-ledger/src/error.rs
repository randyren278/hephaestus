use std::{error::Error, fmt, io};

/// Fail-closed storage and integrity errors.
#[derive(Debug)]
pub enum LedgerError {
    /// SQLite rejected a durable ledger operation.
    Sqlite(rusqlite::Error),
    /// The artifact filesystem rejected an operation.
    Io(io::Error),
    /// An event identifier already exists in canonical history.
    DuplicateEventId(String),
    /// A required event field was empty or whitespace-only.
    EmptyEventField(&'static str),
    /// The database tail changed after this single-writer store verified it.
    LedgerHeadChanged,
    /// A stored hash did not contain exactly 32 bytes.
    InvalidHashLength(usize),
    /// The canonical global sequence contains a gap or duplicate.
    SequenceMismatch {
        /// Sequence required by deterministic replay.
        expected: u64,
        /// Sequence found in storage.
        actual: u64,
    },
    /// An event does not link to the preceding event hash.
    PreviousHashMismatch {
        /// Sequence containing the broken link.
        sequence: u64,
    },
    /// Event content does not reproduce its stored hash.
    EventHashMismatch {
        /// Sequence containing the modified event.
        sequence: u64,
    },
    /// An artifact address was not a canonical lowercase BLAKE3 hash.
    InvalidArtifactId(String),
    /// Artifact bytes do not match their requested content address.
    ArtifactHashMismatch {
        /// Requested content address.
        expected: String,
        /// Address computed from the bytes on disk.
        actual: String,
    },
    /// No artifact is stored at the requested content address.
    ArtifactNotFound(String),
    /// A durable event record could not be decoded from its serialized form.
    ///
    /// This is distinct from a torn trailing write, which a crash-safe
    /// [`crate::EventLedger`] backend drops silently instead of reporting.
    MalformedRecord(String),
}

impl fmt::Display for LedgerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for LedgerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for LedgerError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<io::Error> for LedgerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
