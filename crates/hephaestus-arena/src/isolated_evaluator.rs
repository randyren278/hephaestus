use std::{
    fs,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

use hephaestus_ledger::ArtifactId;
use hephaestus_runtime::{
    CompletionReason, IsolatedWorker, IsolationPolicy, WorkerDomain, WorkerLimits,
};

use crate::{
    ArenaError,
    evaluator_protocol::{
        EvaluatorRequest, EvaluatorResponse, EvaluatorTrial, MAX_EVALUATOR_REQUEST_BYTES,
        MAX_EVALUATOR_RESPONSE_BYTES,
    },
};

/// Trusted process-backed evaluator whose executable identity is World-bound.
pub struct IsolatedEvaluator {
    backend: EvaluatorBackend,
    evaluator_id: String,
    executable: Option<ExecutableIdentity>,
}

struct ExecutableIdentity {
    path: PathBuf,
    device: u64,
    inode: u64,
}

enum EvaluatorBackend {
    Process(IsolatedWorker),
    #[cfg(feature = "test-support")]
    InProcess,
}

impl IsolatedEvaluator {
    /// Opens an evaluator using the host's verified deny-by-default isolation backend.
    ///
    /// The caller must provide every canonical, candidate, sibling, and credential
    /// root as a protected path. The worker always runs offline with bounded IPC.
    ///
    /// # Errors
    ///
    /// Fails closed if the executable is unsafe, its content does not match the
    /// World-bound evaluator identity, or no verified backend is available at run time.
    pub fn open(
        root: impl Into<PathBuf>,
        executable: impl Into<PathBuf>,
        evaluator_id: impl Into<String>,
        protected_paths: impl IntoIterator<Item = PathBuf>,
        limits: WorkerLimits,
    ) -> Result<Self, ArenaError> {
        Self::open_with_policy(
            root,
            executable,
            evaluator_id,
            IsolationPolicy::detect(protected_paths),
            limits,
        )
    }

    /// Opens an evaluator with an already selected isolation policy.
    ///
    /// Normal builds can construct only detected, fail-closed policies. The
    /// runtime crate's opt-in test-support feature supplies the unconfined policy
    /// used by cross-platform process-contract tests.
    ///
    /// # Errors
    ///
    /// Fails closed if the executable or worker root is unsafe.
    pub fn open_with_policy(
        root: impl Into<PathBuf>,
        executable: impl Into<PathBuf>,
        evaluator_id: impl Into<String>,
        isolation: IsolationPolicy,
        limits: WorkerLimits,
    ) -> Result<Self, ArenaError> {
        let executable = executable.into();
        let evaluator_id = evaluator_id.into();
        let executable_identity = verify_executable(&executable, &evaluator_id)?;
        let worker = IsolatedWorker::open(
            root,
            isolation,
            WorkerDomain::Evaluator,
            executable,
            std::iter::empty::<String>(),
            limits,
        )?;
        Ok(Self {
            backend: EvaluatorBackend::Process(worker),
            evaluator_id,
            executable: Some(executable_identity),
        })
    }

    /// Creates an in-process evaluator for dependency-level receipt tests only.
    ///
    /// This constructor is absent from normal dependency and release builds.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn in_process_for_testing(evaluator_id: impl Into<String>) -> Result<Self, ArenaError> {
        let evaluator_id = evaluator_id.into();
        ArtifactId::parse(evaluator_id.clone())?;
        Ok(Self {
            backend: EvaluatorBackend::InProcess,
            evaluator_id,
            executable: None,
        })
    }

    pub(crate) fn evaluate(
        &self,
        evaluation_id: &str,
        request: &EvaluatorRequest,
    ) -> Result<EvaluatorResponse, ArenaError> {
        self.evaluate_inner(evaluation_id, request, None)
    }

    pub(crate) fn evaluate_guarded(
        &self,
        evaluation_id: &str,
        request: &EvaluatorRequest,
        guardian: &Path,
        cancel: Arc<AtomicBool>,
    ) -> Result<EvaluatorResponse, ArenaError> {
        self.evaluate_inner(evaluation_id, request, Some((guardian, cancel)))
    }

    fn evaluate_inner(
        &self,
        evaluation_id: &str,
        request: &EvaluatorRequest,
        guard: Option<(&Path, Arc<AtomicBool>)>,
    ) -> Result<EvaluatorResponse, ArenaError> {
        if request.evaluator_id != self.evaluator_id {
            return Err(ArenaError::BindingMismatch("evaluator process"));
        }
        if let Some(identity) = &self.executable {
            let current = verify_executable(&identity.path, &self.evaluator_id)?;
            if current.device != identity.device || current.inode != identity.inode {
                return Err(ArenaError::EvaluatorExecution(
                    "evaluator executable identity changed".to_owned(),
                ));
            }
        }
        let bytes = serde_json::to_vec(request)?;
        if bytes.len() > MAX_EVALUATOR_REQUEST_BYTES {
            return Err(ArenaError::EvaluatorProtocol("request exceeds byte limit"));
        }
        let worker = match &self.backend {
            EvaluatorBackend::Process(worker) => worker,
            #[cfg(feature = "test-support")]
            EvaluatorBackend::InProcess => {
                let stdout = crate::evaluator_protocol::evaluate_request(&bytes)?;
                return validate_response(&stdout, &bytes, request);
            }
        };
        let output = if let Some((guardian, cancel)) = guard {
            worker.execute_guarded(evaluation_id, &bytes, guardian, cancel)?
        } else {
            worker.execute(evaluation_id, &bytes)?
        };
        if output.completion_reason != CompletionReason::Success
            || output.exit_code != Some(0)
            || !output.stderr.is_empty()
        {
            return Err(ArenaError::EvaluatorExecution(
                "isolated evaluator did not complete successfully".to_owned(),
            ));
        }
        if output.stdout.len() > MAX_EVALUATOR_RESPONSE_BYTES {
            return Err(ArenaError::EvaluatorProtocol("response exceeds byte limit"));
        }
        validate_response(&output.stdout, &bytes, request)
    }
}

fn validate_response(
    stdout: &[u8],
    request_bytes: &[u8],
    request: &EvaluatorRequest,
) -> Result<EvaluatorResponse, ArenaError> {
    let response: EvaluatorResponse = serde_json::from_slice(stdout)?;
    if response.schema_version != 1 {
        return Err(ArenaError::EvaluatorProtocol("unsupported response schema"));
    }
    if serde_json::to_vec(&response)? != stdout {
        return Err(ArenaError::EvaluatorProtocol(
            "response is not canonical JSON",
        ));
    }
    if response.request_artifact_id != ArtifactId::for_bytes(request_bytes).as_str() {
        return Err(ArenaError::EvaluatorProtocol(
            "response request binding mismatch",
        ));
    }
    validate_scores(&response, request)?;
    Ok(response)
}

fn verify_executable(path: &Path, evaluator_id: &str) -> Result<ExecutableIdentity, ArenaError> {
    let expected = ArtifactId::parse(evaluator_id.to_owned())?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.mode() & 0o111 == 0
        || metadata.nlink() != 1
    {
        return Err(ArenaError::EvaluatorExecution(
            "evaluator executable is unsafe".to_owned(),
        ));
    }
    let bytes = fs::read(path)?;
    if ArtifactId::for_bytes(&bytes) != expected {
        return Err(ArenaError::WorldArtifactMismatch("arena.evaluator"));
    }
    Ok(ExecutableIdentity {
        path: fs::canonicalize(path)?,
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn validate_scores(
    response: &EvaluatorResponse,
    request: &EvaluatorRequest,
) -> Result<(), ArenaError> {
    let scores = &response.scores;
    let visible_total =
        u32::try_from(request.visible.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let sealed_total = u32::try_from(request.sealed.len()).map_err(|_| ArenaError::TooManyTasks)?;
    let total = visible_total
        .checked_add(sealed_total)
        .ok_or(ArenaError::TooManyTasks)?;
    let reliable_count = |trials: &[EvaluatorTrial], parent: bool| {
        u32::try_from(
            trials
                .iter()
                .filter(|trial| {
                    if parent {
                        trial.parent_reliable
                    } else {
                        trial.candidate_reliable
                    }
                })
                .count(),
        )
        .map_err(|_| ArenaError::TooManyTasks)
    };
    let parent_visible_reliable = reliable_count(&request.visible, true)?;
    let candidate_visible_reliable = reliable_count(&request.visible, false)?;
    let parent_sealed_reliable = reliable_count(&request.sealed, true)?;
    let candidate_sealed_reliable = reliable_count(&request.sealed, false)?;
    let parent_correct =
        i64::from(scores.parent_visible_correct) + i64::from(scores.parent_sealed_correct);
    let candidate_correct =
        i64::from(scores.candidate_visible_correct) + i64::from(scores.candidate_sealed_correct);
    let paired_delta = i64::from(scores.improvements) - i64::from(scores.regressions);
    if scores.visible_total != visible_total
        || scores.sealed_total != sealed_total
        || scores.parent_visible_correct > parent_visible_reliable
        || scores.candidate_visible_correct > candidate_visible_reliable
        || scores.parent_sealed_correct > parent_sealed_reliable
        || scores.candidate_sealed_correct > candidate_sealed_reliable
        || scores.regressions > total
        || scores.improvements > total
        || scores.regressions.saturating_add(scores.improvements) > total
        || candidate_correct - parent_correct != paired_delta
    {
        return Err(ArenaError::EvaluatorProtocol("invalid aggregate scores"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _, time::Duration};

    use tempfile::tempdir;

    use super::*;
    use crate::evaluator_protocol::{EvaluatorScores, EvaluatorTrial};

    fn executable(directory: &Path, body: &[u8]) -> (PathBuf, String) {
        let path = directory.join("worker");
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let id = ArtifactId::for_bytes(body).as_str().to_owned();
        (path, id)
    }

    fn request(evaluator_id: &str) -> EvaluatorRequest {
        let trial = |task_id: &str| EvaluatorTrial {
            task_id: task_id.to_owned(),
            expected_output: "expected".to_owned(),
            parent_output: "expected".to_owned(),
            candidate_output: "candidate".to_owned(),
            parent_reliable: true,
            candidate_reliable: true,
        };
        EvaluatorRequest {
            schema_version: 1,
            evaluation_id: "evaluation".to_owned(),
            evaluator_id: evaluator_id.to_owned(),
            visible: vec![trial("visible")],
            sealed: vec![trial("sealed")],
        }
    }

    fn valid_response(request_bytes: &[u8]) -> EvaluatorResponse {
        EvaluatorResponse {
            schema_version: 1,
            request_artifact_id: ArtifactId::for_bytes(request_bytes).as_str().to_owned(),
            scores: EvaluatorScores {
                parent_visible_correct: 1,
                candidate_visible_correct: 0,
                parent_sealed_correct: 1,
                candidate_sealed_correct: 0,
                regressions: 2,
                improvements: 0,
                visible_total: 1,
                sealed_total: 1,
            },
        }
    }

    #[test]
    fn evaluator_identity_and_response_shapes_fail_closed() {
        let id = ArtifactId::for_bytes(b"worker").as_str().to_owned();
        let evaluator = IsolatedEvaluator::in_process_for_testing(&id).unwrap();
        let mut mismatched = request(ArtifactId::for_bytes(b"other").as_str());
        assert!(matches!(
            evaluator.evaluate("evaluation", &mismatched),
            Err(ArenaError::BindingMismatch("evaluator process"))
        ));

        mismatched.evaluator_id = id;
        let request_bytes = serde_json::to_vec(&mismatched).unwrap();
        let mut response = valid_response(&request_bytes);
        response.schema_version = 2;
        assert!(matches!(
            validate_response(
                &serde_json::to_vec(&response).unwrap(),
                &request_bytes,
                &mismatched
            ),
            Err(ArenaError::EvaluatorProtocol("unsupported response schema"))
        ));

        response.schema_version = 1;
        let mut noncanonical = serde_json::to_vec(&response).unwrap();
        noncanonical.push(b' ');
        assert!(matches!(
            validate_response(&noncanonical, &request_bytes, &mismatched),
            Err(ArenaError::EvaluatorProtocol(
                "response is not canonical JSON"
            ))
        ));

        response.scores.visible_total = 2;
        assert!(matches!(
            validate_response(
                &serde_json::to_vec(&response).unwrap(),
                &request_bytes,
                &mismatched
            ),
            Err(ArenaError::EvaluatorProtocol("invalid aggregate scores"))
        ));

        response.scores.visible_total = 1;
        response.scores.parent_visible_correct = 0;
        response.scores.candidate_visible_correct = 1;
        response.scores.parent_sealed_correct = 1;
        response.scores.candidate_sealed_correct = 0;
        response.scores.regressions = 1;
        response.scores.improvements = 0;
        assert!(matches!(
            validate_response(
                &serde_json::to_vec(&response).unwrap(),
                &request_bytes,
                &mismatched
            ),
            Err(ArenaError::EvaluatorProtocol("invalid aggregate scores"))
        ));

        response.scores.parent_visible_correct = 1;
        response.scores.candidate_visible_correct = 1;
        response.scores.regressions = 1;
        mismatched.visible[0].candidate_reliable = false;
        let unreliable_request_bytes = serde_json::to_vec(&mismatched).unwrap();
        response.request_artifact_id = ArtifactId::for_bytes(&unreliable_request_bytes)
            .as_str()
            .to_owned();
        assert!(matches!(
            validate_response(
                &serde_json::to_vec(&response).unwrap(),
                &unreliable_request_bytes,
                &mismatched
            ),
            Err(ArenaError::EvaluatorProtocol("invalid aggregate scores"))
        ));
    }

    #[test]
    fn executable_safety_replacement_and_process_failures_are_rejected() {
        let directory = tempdir().unwrap();
        let worker_root = directory.path().join("runs");
        let limits = WorkerLimits::new(Duration::from_secs(2), 1024 * 1024, 128 * 1024).unwrap();
        let bytes = b"#!/bin/sh\nexit 1\n";
        let (path, id) = executable(directory.path(), bytes);
        let evaluator = IsolatedEvaluator::open_with_policy(
            &worker_root,
            &path,
            &id,
            IsolationPolicy::unconfined_for_testing(),
            limits,
        )
        .unwrap();
        assert!(matches!(
            evaluator.evaluate("evaluation", &request(&id)),
            Err(ArenaError::EvaluatorExecution(_))
        ));

        let replacement = directory.path().join("replacement");
        fs::write(&replacement, bytes).unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(matches!(
            evaluator.evaluate("evaluation", &request(&id)),
            Err(ArenaError::EvaluatorExecution(message))
                if message == "evaluator executable identity changed"
        ));

        let unsafe_path = directory.path().join("not-executable");
        fs::write(&unsafe_path, b"unsafe").unwrap();
        assert!(matches!(
            IsolatedEvaluator::open(
                directory.path().join("unsafe-runs"),
                &unsafe_path,
                ArtifactId::for_bytes(b"unsafe").as_str(),
                std::iter::empty(),
                limits,
            ),
            Err(ArenaError::EvaluatorExecution(_))
        ));

        let missing_root = directory.path().join("root-file");
        fs::write(&missing_root, b"not a directory").unwrap();
        let (valid_path, valid_id) = executable(directory.path(), b"#!/bin/sh\nexit 0\n");
        assert!(
            IsolatedEvaluator::open(
                missing_root,
                valid_path,
                valid_id,
                std::iter::empty(),
                limits,
            )
            .is_err()
        );
    }

    #[test]
    fn oversized_worker_response_is_rejected_after_bounded_capture() {
        let directory = tempdir().unwrap();
        let script = b"#!/bin/sh\ncat >/dev/null\nhead -c 65537 /dev/zero\n";
        let (path, id) = executable(directory.path(), script);
        let evaluator = IsolatedEvaluator::open_with_policy(
            directory.path().join("runs"),
            path,
            &id,
            IsolationPolicy::unconfined_for_testing(),
            WorkerLimits::new(Duration::from_secs(2), 1024 * 1024, 128 * 1024).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            evaluator.evaluate("evaluation", &request(&id)),
            Err(ArenaError::EvaluatorProtocol("response exceeds byte limit"))
        ));
    }
}
