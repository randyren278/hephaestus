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
    ChampionTransitionPayload, ChampionTransitionRecord, Command, EvaluationEventRecord,
    EvaluationRecord, ForgeAssessmentEventRecord, ForgeAssessmentOutcome, ForgeAssessmentPayload,
    ForgeAssessmentRecord, ForgeProposalEventRecord, ForgeProposalPayload, ForgeProposalRecord,
    GenomeRecord, InvariantRecord, JobProgress, JobRecord, JobState, JobTerminal, ResponseData,
    RunCompletionReason, SelectionEventRecord, SelectionRecord, WorldRecord,
};
pub use server::{ControlPlane, data_dir_from_environment};
