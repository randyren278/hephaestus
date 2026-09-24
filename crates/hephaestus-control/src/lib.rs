//! Daemon-owned local control plane and versioned operator protocol.

mod client;
mod error;
mod protocol;
mod server;

pub use client::Client;
pub use error::ControlError;
pub use protocol::{
    API_VERSION, ApiError, ApiErrorCode, ApiRequest, ApiResponse, ArenaJobPhase, ArenaJobProgress,
    ChampionEventRecord, ChampionPromotionEvidence, ChampionRecord, ChampionTransitionKind,
    ChampionTransitionPayload, ChampionTransitionRecord, Command, DenialEntry, DenialKind,
    EvaluationEventRecord, EvaluationForgeSummary, EvaluationInvariantSummary, EvaluationListEntry,
    EvaluationRecord, EvaluationSelectionSummary, ForgeAssessmentEventRecord,
    ForgeAssessmentOutcome, ForgeAssessmentPayload, ForgeAssessmentRecord,
    ForgeProposalEventRecord, ForgeProposalPayload, ForgeProposalRecord, GenomeRecord,
    InvariantRecord, JobProgress, JobRecord, JobState, JobTerminal, MAX_LIST_LIMIT, ResponseData,
    RunCompletionReason, RunListEntry, SelectionEventRecord, SelectionRecord, WorldRecord,
};
pub use server::{ControlPlane, data_dir_from_environment};
