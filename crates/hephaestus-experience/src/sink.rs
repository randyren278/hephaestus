use std::sync::mpsc::{self, Receiver, SyncSender};

use crate::{EvidenceRecorder, ExperienceError, Provenance, TraceInput, TraceReceipt};

/// Writer-facing evidence contract used by recorded runtimes.
pub trait EvidenceSink {
    /// Reserves canonical capacity before a run begins.
    ///
    /// # Errors
    ///
    /// Returns the persistence error or a disconnected-writer error.
    fn ensure_capacity(&mut self, run_id: &str, needed: usize) -> Result<(), ExperienceError>;

    /// Persists one bounded, provenance-bearing trace before returning.
    ///
    /// # Errors
    ///
    /// Returns the persistence error or a disconnected-writer error.
    fn record_trace(
        &mut self,
        input: TraceInput,
        reserved_after: usize,
    ) -> Result<TraceReceipt, ExperienceError>;
}

/// One bounded request for the canonical writer, with a one-shot durable ack.
pub enum EvidenceRequest {
    /// Reserve space for the mandatory lifecycle records.
    EnsureCapacity {
        run_id: String,
        needed: usize,
        reply: mpsc::Sender<Result<(), String>>,
    },
    /// Persist one trace and return its canonical receipt after append.
    RecordTrace {
        input: TraceInput,
        reserved_after: usize,
        reply: mpsc::Sender<Result<TraceReceipt, String>>,
    },
}

impl EvidenceRequest {
    /// Run identity carried by this request.
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            Self::EnsureCapacity { run_id, .. } => run_id,
            Self::RecordTrace { input, .. } => input.provenance().run_id(),
        }
    }

    /// Trace provenance when this request contains a trace.
    #[must_use]
    pub fn provenance(&self) -> Option<&Provenance> {
        match self {
            Self::EnsureCapacity { .. } => None,
            Self::RecordTrace { input, .. } => Some(input.provenance()),
        }
    }

    /// Reject the request and release the waiting executor.
    pub fn reject(self, reason: impl Into<String>) {
        let reason = reason.into();
        match self {
            Self::EnsureCapacity { reply, .. } => {
                let _ignored = reply.send(Err(reason));
            }
            Self::RecordTrace { reply, .. } => {
                let _ignored = reply.send(Err(reason));
            }
        }
    }

    /// Apply the request to the canonical recorder and acknowledge its result.
    ///
    /// # Errors
    ///
    /// Returns the recorder error that is sent to the waiting executor.
    pub fn persist(self, recorder: &mut EvidenceRecorder) -> Result<(), String> {
        match self {
            Self::EnsureCapacity {
                run_id,
                needed,
                reply,
            } => {
                let result = recorder
                    .ensure_capacity(&run_id, needed)
                    .map_err(|error| error.to_string());
                let acknowledged = result.as_ref().map_err(Clone::clone).copied();
                let _ignored = reply.send(result);
                acknowledged
            }
            Self::RecordTrace {
                input,
                reserved_after,
                reply,
            } => {
                let result = recorder
                    .record_trace_reserving(input, reserved_after)
                    .map_err(|error| error.to_string());
                let acknowledged = result.as_ref().map(|_| ()).map_err(Clone::clone);
                let _ignored = reply.send(result);
                acknowledged
            }
        }
    }
}

/// Bounded executor-side adapter. Calls block until the canonical writer acks.
pub struct ChannelEvidenceSink {
    sender: SyncSender<EvidenceRequest>,
}

/// Creates a synchronous bounded evidence path from executor to canonical writer.
#[must_use]
pub fn bounded_evidence_channel(
    capacity: usize,
) -> (ChannelEvidenceSink, Receiver<EvidenceRequest>) {
    let (sender, receiver) = mpsc::sync_channel(capacity.max(1));
    (ChannelEvidenceSink { sender }, receiver)
}

impl EvidenceSink for EvidenceRecorder {
    fn ensure_capacity(&mut self, run_id: &str, needed: usize) -> Result<(), ExperienceError> {
        EvidenceRecorder::ensure_capacity(self, run_id, needed)
    }

    fn record_trace(
        &mut self,
        input: TraceInput,
        reserved_after: usize,
    ) -> Result<TraceReceipt, ExperienceError> {
        self.record_trace_reserving(input, reserved_after)
    }
}

impl EvidenceSink for ChannelEvidenceSink {
    fn ensure_capacity(&mut self, run_id: &str, needed: usize) -> Result<(), ExperienceError> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(EvidenceRequest::EnsureCapacity {
                run_id: run_id.to_owned(),
                needed,
                reply,
            })
            .map_err(|_| ExperienceError::SinkUnavailable)?;
        result
            .recv()
            .map_err(|_| ExperienceError::SinkUnavailable)?
            .map_err(ExperienceError::SinkRejected)
    }

    fn record_trace(
        &mut self,
        input: TraceInput,
        reserved_after: usize,
    ) -> Result<TraceReceipt, ExperienceError> {
        let (reply, result) = mpsc::channel();
        self.sender
            .send(EvidenceRequest::RecordTrace {
                input,
                reserved_after,
                reply,
            })
            .map_err(|_| ExperienceError::SinkUnavailable)?;
        result
            .recv()
            .map_err(|_| ExperienceError::SinkUnavailable)?
            .map_err(ExperienceError::SinkRejected)
    }
}
