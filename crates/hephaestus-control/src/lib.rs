//! Daemon-owned local control plane and versioned operator protocol.

mod client;
mod error;
mod protocol;
mod server;

pub use client::Client;
pub use error::ControlError;
pub use hephaestus_arena::{ClusterAnalysis, FailureCluster, SuggestedMutation};
pub use protocol::{
    API_VERSION, ApiError, ApiErrorCode, ApiRequest, ApiResponse, ArenaJobPhase, ArenaJobProgress,
    ChampionEventRecord, ChampionPromotionEvidence, ChampionRecord, ChampionTransitionKind,
    ChampionTransitionPayload, ChampionTransitionRecord, Command, DenialEntry, DenialKind,
    EvaluationEventRecord, EvaluationForgeSummary, EvaluationInvariantSummary, EvaluationListEntry,
    EvaluationRecord, EvaluationSelectionSummary, EvolutionCancelPayload, EvolutionEventRecord,
    EvolutionFinishReason, EvolutionFinishedPayload, EvolutionGenerationPayload,
    EvolutionGenerationRecord, EvolutionRunRecord, EvolutionRunState, EvolutionStartedPayload,
    ForgeAnalysisBinding, ForgeAnalysisRecord, ForgeAssessmentEventRecord, ForgeAssessmentOutcome,
    ForgeAssessmentPayload, ForgeAssessmentRecord, ForgeProposalEventRecord, ForgeProposalPayload,
    ForgeProposalRecord, GeneAggregateRecord, GeneContradictionPayload, GeneContradictionRecord,
    GeneEventRecord, GeneExtractedPayload, GeneRecord, GeneSpeciesPayload, GeneSpeciesRecord,
    GeneSummary, GeneTransferAppliedPayload, GeneTransferOutcome, GeneTransferRecord,
    GeneTransferRecordedPayload, GenomeRecord, InvariantRecord, JobProgress, JobRecord, JobState,
    JobTerminal, MAX_LIST_LIMIT, ResponseData, RunCompletionReason, RunListEntry,
    SelectionEventRecord, SelectionRecord, WorldRecord,
};
pub use server::{ControlPlane, data_dir_from_environment};
