//! Durable evidence primitives for Hephaestus.

mod artifact_store;
pub mod contract;
mod error;
mod event_index;
mod event_store;
mod file_event_store;
mod memory_artifact_store;
mod storage;

pub use artifact_store::{ArtifactId, ArtifactStore};
pub use error::LedgerError;
pub use event_index::EventIndex;
pub use event_store::{EventInput, EventStore, StoredEvent};
pub use file_event_store::FileEventLedger;
pub use memory_artifact_store::MemoryArtifactBackend;
pub use storage::{ArtifactBackend, EventLedger};
