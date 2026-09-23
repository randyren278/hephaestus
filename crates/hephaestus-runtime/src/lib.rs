//! Capability-scoped sandboxes and swappable agent runtime contracts.

mod deterministic;
mod error;
mod isolation;
mod provider;
mod reference_instruction;
mod runtime;
mod sandbox;
mod spec;
mod supervisor;
mod worker;

pub use deterministic::DeterministicRuntime;
pub use error::RuntimeError;
pub use isolation::{IsolationBackend, IsolationPolicy};
pub use provider::ProviderInvocation;
pub use reference_instruction::{ReferenceInstruction, execute_reference_worker_request};
pub use runtime::{
    AdapterCapabilities, CompletionReason, Provider, RunHandle, RunSnapshot, RunStatus,
    RuntimeAdapter, RuntimeObservation, RuntimeObservationKind,
};
pub use sandbox::{CapabilityToken, Sandbox, SandboxManager};
pub use spec::{Budget, ExperimentContext, RunSpec};
pub use supervisor::SupervisedRuntime;
pub use worker::{IsolatedWorker, WorkerDomain, WorkerLimits, WorkerOutput};
