use std::{error::Error, fmt};

use hephaestus_ledger::LedgerError;

/// Validation, redaction, serialization, and durable evidence failures.
#[derive(Debug)]
pub enum ExperienceError {
    /// A required identifier, field, provenance link, or limit is invalid.
    InvalidInput(&'static str),
    /// A trace or experience artifact exceeds its configured byte ceiling.
    RecordTooLarge { actual: usize, maximum: usize },
    /// A run reached its configured durable trace count ceiling.
    RetentionExceeded { maximum: usize },
    /// An experience cites an event absent from verified canonical history.
    UnknownSourceEvent(String),
    /// A requested canonical experience event does not exist.
    UnknownExperience(String),
    /// A stored experience event, receipt, or artifact is internally inconsistent.
    InvalidStoredExperience(&'static str),
    /// Durable ledger or content-addressed storage failed.
    Ledger(LedgerError),
    /// Canonical JSON serialization or decoding failed.
    Json(serde_json::Error),
    /// The canonical evidence writer is no longer available.
    SinkUnavailable,
    /// The canonical evidence writer rejected a request.
    SinkRejected(String),
}

impl fmt::Display for ExperienceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for ExperienceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Ledger(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidInput(_)
            | Self::RecordTooLarge { .. }
            | Self::RetentionExceeded { .. }
            | Self::UnknownSourceEvent(_)
            | Self::UnknownExperience(_)
            | Self::InvalidStoredExperience(_)
            | Self::SinkUnavailable
            | Self::SinkRejected(_) => None,
        }
    }
}

impl From<LedgerError> for ExperienceError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}

impl From<serde_json::Error> for ExperienceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
