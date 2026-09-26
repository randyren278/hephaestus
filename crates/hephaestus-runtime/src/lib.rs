//! Capability-scoped sandboxes and swappable agent runtime contracts.

mod deterministic;
mod error;
mod guardian;
mod isolation;
mod mutation_catalog;
mod provider;
mod provider_events;
mod reference_instruction;
mod runtime;
mod sandbox;
mod spec;
mod supervisor;
mod worker;

pub use deterministic::DeterministicRuntime;
pub use error::RuntimeError;
pub use guardian::{hold_process_group_anchor, run_process_guardian};
pub use isolation::{IsolationBackend, IsolationPolicy};
pub use mutation_catalog::{
    ALL_OPERATIONS as MUTATION_CATALOG_OPERATIONS, CATALOG_VERSION as MUTATION_CATALOG_VERSION,
    EdgeKind as MutationEdgeKind, casing_flip as mutation_casing_flip,
    edge_kind as mutation_edge_kind, family_fix_for as mutation_family_fix_for,
    family_name as mutation_family_name, is_catalog_edge,
    is_known_operation as is_known_mutation_operation,
};
pub use provider::ProviderInvocation;
pub use provider_events::{
    ProviderEventCursor, extract_actual_cost_microusd, extract_final_answer,
};
pub use reference_instruction::{
    ReferenceInstruction, execute_reference_worker_request, frame_reference_instruction,
};
pub use runtime::{
    AdapterCapabilities, CompletionReason, Provider, RunHandle, RunSnapshot, RunStatus,
    RuntimeAdapter, RuntimeObservation, RuntimeObservationKind,
};
pub use sandbox::{CapabilityToken, Sandbox, SandboxManager};
pub use spec::{Budget, ExperimentContext, RunSpec};
pub use supervisor::SupervisedRuntime;
#[cfg(feature = "test-support")]
pub use supervisor::{
    clear_test_reference_delay, clear_test_reference_delays_in, forget_test_reference_scope,
    set_test_reference_baseline_delay, set_test_reference_baseline_delay_in,
    set_test_reference_delay, set_test_reference_delay_in,
};
pub use worker::{IsolatedWorker, WorkerDomain, WorkerLimits, WorkerOutput};
