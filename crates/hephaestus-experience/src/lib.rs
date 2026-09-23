//! Redacted, bounded, provenance-aware execution evidence.

mod error;
mod integration;
mod record;
mod recorder;
mod redaction;
mod rehydrate;
mod run_result;
mod sink;

pub use error::ExperienceError;
pub use integration::RecordedRuntime;
pub use record::{
    ExperienceInput, ExperienceKind, ExperienceReceipt, Provenance, TraceInput, TraceKind,
    TraceReceipt, TrustedExperience,
};
pub use recorder::{EvidenceRecorder, RetentionLimits};
pub use redaction::RedactionPolicy;
pub use rehydrate::rehydrate_experience;
pub use run_result::{
    RUN_RESULT_SCHEMA_VERSION, RunBudgetReceipt, RunCompletionReason, RunResultReceipt,
    RunResultSigner, RunResultVerifier,
};
pub use sink::{ChannelEvidenceSink, EvidenceRequest, EvidenceSink, bounded_evidence_channel};
