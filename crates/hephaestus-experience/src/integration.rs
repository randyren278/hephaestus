use std::{
    collections::{BTreeMap, VecDeque},
    time::{SystemTime, UNIX_EPOCH},
};

use hephaestus_runtime::{
    AdapterCapabilities, CapabilityToken, CompletionReason, Provider, RunHandle, RunSnapshot,
    RunSpec, RunStatus, RuntimeAdapter, RuntimeError, RuntimeObservation, RuntimeObservationKind,
    Sandbox,
};

use crate::{EvidenceRecorder, EvidenceSink, Provenance, TraceInput, TraceKind};

/// Runtime adapter decorator that makes observable execution evidence mandatory.
pub struct RecordedRuntime<R, E = EvidenceRecorder> {
    inner: R,
    evidence: E,
    runs: BTreeMap<String, RecordedRun>,
    sequence: u64,
    trace_artifact_ids: Vec<String>,
}

struct RecordedRun {
    provenance: Provenance,
    source_revision: String,
    state: RecordedRunState,
    pending_observations: VecDeque<RuntimeObservation>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RecordedRunState {
    Active,
    Terminal,
    ContainmentFailed,
}

impl<R> RecordedRuntime<R> {
    /// Wraps a runtime with a durable evidence recorder.
    ///
    /// # Errors
    ///
    /// Rejects an evidence ledger that cannot be replayed and verified.
    pub fn new(inner: R, evidence: EvidenceRecorder) -> Result<Self, RuntimeError> {
        Self::new_recoverable(inner, evidence).map_err(|recovery| recovery.0)
    }

    /// Wraps a runtime without losing ownership when evidence verification fails.
    ///
    /// # Errors
    ///
    /// Returns the error together with both inputs so a single-writer composition
    /// root can recover its canonical stores.
    pub fn new_recoverable(
        inner: R,
        evidence: EvidenceRecorder,
    ) -> Result<Self, Box<(RuntimeError, R, EvidenceRecorder)>> {
        let history = match evidence.replay_verified() {
            Ok(history) => history,
            Err(error) => return Err(Box::new((evidence_error(error), inner, evidence))),
        };
        let Ok(sequence) = u64::try_from(history.len()) else {
            return Err(Box::new((
                RuntimeError::Evidence("evidence sequence exceeds u64".to_owned()),
                inner,
                evidence,
            )));
        };
        Ok(Self {
            inner,
            evidence,
            runs: BTreeMap::new(),
            sequence,
            trace_artifact_ids: Vec::new(),
        })
    }

    /// Wraps a runtime with a bounded asynchronous evidence sink.
    ///
    /// The sink must acknowledge a trace only after the canonical writer has
    /// persisted it. `initial_sequence` is the writer's verified event count.
    #[must_use]
    pub fn with_sink<E: EvidenceSink>(
        inner: R,
        evidence: E,
        initial_sequence: u64,
    ) -> RecordedRuntime<R, E> {
        RecordedRuntime {
            inner,
            evidence,
            runs: BTreeMap::new(),
            sequence: initial_sequence,
            trace_artifact_ids: Vec::new(),
        }
    }
}

impl<R> RecordedRuntime<R, EvidenceRecorder> {
    /// Returns the durable recorder for verified replay and artifact reads.
    #[must_use]
    pub const fn evidence(&self) -> &EvidenceRecorder {
        &self.evidence
    }
}

impl<R, E: EvidenceSink> RecordedRuntime<R, E> {
    /// Returns the wrapped provider-neutral runtime.
    #[must_use]
    pub const fn inner(&self) -> &R {
        &self.inner
    }

    /// Trace artifacts durably acknowledged by the evidence sink.
    #[must_use]
    pub fn trace_artifact_ids(&self) -> &[String] {
        &self.trace_artifact_ids
    }

    /// Returns the wrapped runtime and evidence recorder to their owner.
    #[must_use]
    pub fn into_parts(self) -> (R, E) {
        (self.inner, self.evidence)
    }

    fn record(
        &mut self,
        provenance: Provenance,
        kind: TraceKind,
        fields: BTreeMap<String, String>,
        reserved_after: usize,
    ) -> Result<(), RuntimeError> {
        let timestamp_millis = unix_millis()?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| RuntimeError::Evidence("evidence sequence overflow".to_owned()))?;
        let identity = format!(
            "{}:{}:{kind:?}:{timestamp_millis}",
            provenance.run_id(),
            self.sequence
        );
        let event_id = format!("trace-{}", blake3::hash(identity.as_bytes()).to_hex());
        let input = TraceInput::new(event_id, provenance, kind, timestamp_millis, fields)
            .map_err(evidence_error)?;
        let receipt = self
            .evidence
            .record_trace(input, reserved_after)
            .map_err(evidence_error)?;
        self.trace_artifact_ids.push(receipt.artifact_id);
        Ok(())
    }

    fn record_failure(
        &mut self,
        provenance: Provenance,
        operation: &'static str,
        error: &RuntimeError,
        reserved_after: usize,
    ) -> Result<(), RuntimeError> {
        let kind = if matches!(error, RuntimeError::CapabilityDenied) {
            TraceKind::CapabilityDenied
        } else {
            TraceKind::Error
        };
        self.record(
            provenance,
            kind,
            BTreeMap::from([
                ("operation".to_owned(), operation.to_owned()),
                ("error".to_owned(), error.to_string()),
            ]),
            reserved_after,
        )
    }

    fn drain_observations(&mut self, run_id: &str) -> Result<(), RuntimeError>
    where
        R: RuntimeAdapter,
    {
        let observations = self.inner.drain_observations(run_id)?;
        let provenance = self.active_provenance(run_id)?;
        self.runs
            .get_mut(run_id)
            .expect("active provenance requires an existing run")
            .pending_observations
            .extend(observations);
        while let Some(observation) = self
            .runs
            .get(run_id)
            .and_then(|run| run.pending_observations.front())
            .cloned()
        {
            self.record(
                provenance.clone(),
                trace_kind(observation.kind),
                observation.fields,
                1,
            )?;
            self.runs
                .get_mut(run_id)
                .expect("active provenance requires an existing run")
                .pending_observations
                .pop_front();
        }
        Ok(())
    }

    fn active_provenance(&self, run_id: &str) -> Result<Provenance, RuntimeError> {
        let run = self
            .runs
            .get(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        if run.state == RecordedRunState::Terminal {
            return Err(RuntimeError::InvalidSpec("run is not active"));
        }
        Ok(run.provenance.clone())
    }

    fn contain_after_evidence_failure(
        &mut self,
        run_id: &str,
        evidence: RuntimeError,
    ) -> RuntimeError
    where
        R: RuntimeAdapter,
    {
        match self.inner.interrupt(run_id) {
            Ok(()) => {
                self.runs.remove(run_id);
                evidence
            }
            Err(interrupt) => {
                if let Some(run) = self.runs.get_mut(run_id) {
                    run.state = RecordedRunState::ContainmentFailed;
                }
                RuntimeError::ContainmentFailed {
                    evidence: evidence.to_string(),
                    interrupt: interrupt.to_string(),
                }
            }
        }
    }
}

impl<R: RuntimeAdapter, E: EvidenceSink> RuntimeAdapter for RecordedRuntime<R, E> {
    fn provider(&self) -> Provider {
        self.inner.provider()
    }

    fn report_capabilities(&self) -> AdapterCapabilities {
        self.inner.report_capabilities()
    }

    fn start(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        token: &CapabilityToken,
    ) -> Result<RunHandle, RuntimeError> {
        let provenance = Provenance::new(spec.run_id(), spec.genome_id(), spec.world_id())
            .map_err(evidence_error)?;
        self.evidence
            .ensure_capacity(spec.run_id(), 2)
            .map_err(evidence_error)?;
        let handle = match self.inner.start(spec, sandbox, token) {
            Ok(handle) => handle,
            Err(error) => {
                self.record_failure(provenance, "start", &error, 0)?;
                return Err(error);
            }
        };
        self.runs.insert(
            spec.run_id().to_owned(),
            RecordedRun {
                provenance: provenance.clone(),
                source_revision: spec.source_revision().to_owned(),
                state: RecordedRunState::Active,
                pending_observations: VecDeque::new(),
            },
        );
        let fields = BTreeMap::from([
            (
                "provider".to_owned(),
                format!("{:?}", self.inner.provider()),
            ),
            (
                "maximum_output_bytes".to_owned(),
                spec.budget().maximum_output_bytes().to_string(),
            ),
            (
                "maximum_cost_microusd".to_owned(),
                spec.budget().maximum_cost_microusd().to_string(),
            ),
            (
                "wall_budget_millis".to_owned(),
                spec.budget().wall().as_millis().to_string(),
            ),
            (
                "workspace_write".to_owned(),
                spec.capabilities().allows_workspace_write().to_string(),
            ),
            (
                "network".to_owned(),
                spec.capabilities().allows_network().to_string(),
            ),
            (
                "source_revision".to_owned(),
                spec.source_revision().to_owned(),
            ),
        ]);
        if let Err(error) = self.record(provenance, TraceKind::LifecycleStarted, fields, 1) {
            return Err(self.contain_after_evidence_failure(spec.run_id(), error));
        }
        if let Err(error) = self.drain_observations(spec.run_id()) {
            return Err(self.contain_after_evidence_failure(spec.run_id(), error));
        }
        Ok(handle)
    }

    fn resume(
        &mut self,
        spec: &RunSpec,
        sandbox: &Sandbox,
        token: &CapabilityToken,
        checkpoint: &str,
    ) -> Result<RunHandle, RuntimeError> {
        let provenance = Provenance::new(spec.run_id(), spec.genome_id(), spec.world_id())
            .map_err(evidence_error)?;
        let existing = self
            .runs
            .get(spec.run_id())
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        if existing.provenance != provenance
            || existing.source_revision != spec.source_revision()
            || existing.state != RecordedRunState::Terminal
            || !existing.pending_observations.is_empty()
        {
            return Err(RuntimeError::InvalidSpec(
                "only the same provenance from a fully evidenced terminal run can resume",
            ));
        }
        self.evidence
            .ensure_capacity(spec.run_id(), 2)
            .map_err(evidence_error)?;
        let handle = match self.inner.resume(spec, sandbox, token, checkpoint) {
            Ok(handle) => handle,
            Err(error) => {
                self.record_failure(provenance, "resume", &error, 0)?;
                return Err(error);
            }
        };
        self.runs.insert(
            spec.run_id().to_owned(),
            RecordedRun {
                provenance: provenance.clone(),
                source_revision: spec.source_revision().to_owned(),
                state: RecordedRunState::Active,
                pending_observations: VecDeque::new(),
            },
        );
        if let Err(error) = self.record(
            provenance,
            TraceKind::LifecycleResumed,
            BTreeMap::from([
                (
                    "checkpoint_used_hash".to_owned(),
                    blake3::hash(checkpoint.as_bytes()).to_hex().to_string(),
                ),
                (
                    "source_revision".to_owned(),
                    spec.source_revision().to_owned(),
                ),
            ]),
            1,
        ) {
            return Err(self.contain_after_evidence_failure(spec.run_id(), error));
        }
        if let Err(error) = self.drain_observations(spec.run_id()) {
            return Err(self.contain_after_evidence_failure(spec.run_id(), error));
        }
        Ok(handle)
    }

    fn interrupt(&mut self, run_id: &str) -> Result<(), RuntimeError> {
        let run = self
            .runs
            .get(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        if run.state == RecordedRunState::Terminal {
            return Ok(());
        }
        let provenance = run.provenance.clone();
        if let Err(error) = self.inner.interrupt(run_id) {
            if let Err(evidence) = self.record_failure(provenance, "interrupt", &error, 1) {
                return Err(self.contain_after_evidence_failure(run_id, evidence));
            }
            return Err(error);
        }
        let snapshot = self.snapshot(run_id)?;
        if snapshot.status == RunStatus::Running {
            return Err(RuntimeError::Evidence(
                "interrupt returned before runtime termination".to_owned(),
            ));
        }
        Ok(())
    }

    fn snapshot(&mut self, run_id: &str) -> Result<RunSnapshot, RuntimeError> {
        let run = self
            .runs
            .get(run_id)
            .ok_or(RuntimeError::InvalidSpec("run does not exist"))?;
        let provenance = run.provenance.clone();
        let was_terminal = run.state == RecordedRunState::Terminal;
        let snapshot = match self.inner.snapshot(run_id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if let Err(evidence) = self.record_failure(provenance, "snapshot", &error, 1) {
                    return Err(self.contain_after_evidence_failure(run_id, evidence));
                }
                return Err(error);
            }
        };
        if was_terminal {
            return Ok(snapshot);
        }
        if let Err(error) = self.drain_observations(run_id) {
            if snapshot.status == RunStatus::Running {
                return Err(self.contain_after_evidence_failure(run_id, error));
            }
            return Err(error);
        }
        if snapshot.status != RunStatus::Running {
            let reason = snapshot.completion_reason.ok_or_else(|| {
                RuntimeError::Evidence("terminal snapshot omitted completion reason".to_owned())
            })?;
            let mut fields = BTreeMap::from([
                ("status".to_owned(), format!("{:?}", snapshot.status)),
                (
                    "completion_reason".to_owned(),
                    completion_reason(reason).to_owned(),
                ),
                (
                    "latency_millis".to_owned(),
                    snapshot.elapsed.as_millis().to_string(),
                ),
            ]);
            if let Some(exit_code) = snapshot.exit_code {
                fields.insert("exit_code".to_owned(), exit_code.to_string());
            }
            if self.inner.provider() == Provider::Deterministic {
                fields.insert("actual_cost_microusd".to_owned(), "0".to_owned());
            }
            self.record(provenance, TraceKind::LifecycleCompleted, fields, 0)?;
            self.runs
                .get_mut(run_id)
                .expect("run existence checked before snapshot")
                .state = RecordedRunState::Terminal;
        }
        Ok(snapshot)
    }

    fn drain_observations(
        &mut self,
        _run_id: &str,
    ) -> Result<Vec<hephaestus_runtime::RuntimeObservation>, RuntimeError> {
        Ok(Vec::new())
    }
}

fn unix_millis() -> Result<i64, RuntimeError> {
    unix_millis_at(SystemTime::now())
}

fn unix_millis_at(now: SystemTime) -> Result<i64, RuntimeError> {
    let millis = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeError::Evidence("system clock precedes Unix epoch".to_owned()))?
        .as_millis();
    i64::try_from(millis)
        .map_err(|_| RuntimeError::Evidence("system clock exceeds i64 milliseconds".to_owned()))
}

const fn completion_reason(reason: CompletionReason) -> &'static str {
    match reason {
        CompletionReason::Success => "success",
        CompletionReason::ProviderFailure => "provider_failure",
        CompletionReason::OperatorInterrupt => "operator_interrupt",
        CompletionReason::WallBudgetExceeded => "wall_budget_exceeded",
        CompletionReason::OutputBudgetExceeded => "output_budget_exceeded",
        CompletionReason::IoFailure => "io_failure",
    }
}

const fn trace_kind(kind: RuntimeObservationKind) -> TraceKind {
    match kind {
        RuntimeObservationKind::ToolCalled => TraceKind::ToolCalled,
        RuntimeObservationKind::ToolResult => TraceKind::ToolResult,
        RuntimeObservationKind::ContextComposed => TraceKind::ContextComposed,
        RuntimeObservationKind::MemoryRetrieved => TraceKind::MemoryRetrieved,
        RuntimeObservationKind::SubagentSpawned => TraceKind::SubagentSpawned,
        RuntimeObservationKind::FileRead => TraceKind::FileRead,
        RuntimeObservationKind::FileChanged => TraceKind::FileChanged,
        RuntimeObservationKind::TestExecuted => TraceKind::TestExecuted,
        RuntimeObservationKind::CostObserved => TraceKind::CostObserved,
        RuntimeObservationKind::CheckpointCreated => TraceKind::CheckpointCreated,
        RuntimeObservationKind::Error => TraceKind::Error,
        RuntimeObservationKind::Retry => TraceKind::Retry,
        RuntimeObservationKind::ModelResponse => TraceKind::ModelResponse,
    }
}

fn evidence_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Evidence(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::tempdir;

    use super::*;
    use crate::{RedactionPolicy, RetentionLimits};

    #[test]
    fn clock_status_and_sequence_edges_fail_or_map_explicitly() {
        assert!(unix_millis_at(UNIX_EPOCH - Duration::from_secs(1)).is_err());
        assert!(unix_millis_at(UNIX_EPOCH + Duration::from_secs(i64::MAX as u64)).is_err());
        assert_eq!(
            [
                CompletionReason::Success,
                CompletionReason::ProviderFailure,
                CompletionReason::OperatorInterrupt,
                CompletionReason::WallBudgetExceeded,
                CompletionReason::OutputBudgetExceeded,
                CompletionReason::IoFailure,
            ]
            .map(completion_reason),
            [
                "success",
                "provider_failure",
                "operator_interrupt",
                "wall_budget_exceeded",
                "output_budget_exceeded",
                "io_failure",
            ]
        );
        assert_eq!(
            [
                RuntimeObservationKind::ToolCalled,
                RuntimeObservationKind::ToolResult,
                RuntimeObservationKind::ContextComposed,
                RuntimeObservationKind::MemoryRetrieved,
                RuntimeObservationKind::SubagentSpawned,
                RuntimeObservationKind::FileRead,
                RuntimeObservationKind::FileChanged,
                RuntimeObservationKind::TestExecuted,
                RuntimeObservationKind::CostObserved,
                RuntimeObservationKind::CheckpointCreated,
                RuntimeObservationKind::Error,
                RuntimeObservationKind::Retry,
                RuntimeObservationKind::ModelResponse,
            ]
            .map(trace_kind),
            [
                TraceKind::ToolCalled,
                TraceKind::ToolResult,
                TraceKind::ContextComposed,
                TraceKind::MemoryRetrieved,
                TraceKind::SubagentSpawned,
                TraceKind::FileRead,
                TraceKind::FileChanged,
                TraceKind::TestExecuted,
                TraceKind::CostObserved,
                TraceKind::CheckpointCreated,
                TraceKind::Error,
                TraceKind::Retry,
                TraceKind::ModelResponse,
            ]
        );

        let directory = tempdir().expect("evidence directory");
        let recorder = EvidenceRecorder::open(
            directory.path().join("events.sqlite3"),
            directory.path().join("artifacts"),
            RedactionPolicy::new([]),
            RetentionLimits::new(1, 1_024).expect("retention limits"),
        )
        .expect("open evidence recorder");
        let mut runtime = RecordedRuntime::new((), recorder).expect("create runtime");
        runtime.sequence = u64::MAX;
        assert!(matches!(
            runtime.record(
                Provenance::new("run", "genome", "world").expect("provenance"),
                TraceKind::Error,
                BTreeMap::new(),
                0
            ),
            Err(RuntimeError::Evidence(_))
        ));
    }
}
