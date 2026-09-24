use std::{
    fs,
    io::{Read, Write},
    net::Shutdown,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use hephaestus_arena::{
    EvaluationBinding, EvaluationInputs, EvaluationStores, IsolatedEvaluator, ReceiptContext,
    TrialPlan, TrustedManifest, TrustedTask, Visibility, evaluate_and_record,
};
use hephaestus_control::{
    API_VERSION, ApiErrorCode, ApiRequest, ApiResponse, Command, ControlError, ControlPlane,
    ForgeAssessmentOutcome, GenomeRecord, JobProgress, JobRecord, JobState, JobTerminal,
    ResponseData, RunCompletionReason, WorldRecord,
};
#[cfg(feature = "test-support")]
use hephaestus_control::{ArenaJobPhase, Client};
use hephaestus_experience::{
    RUN_RESULT_SCHEMA_VERSION, RunBudgetReceipt, RunResultReceipt, RunResultSigner, TraceKind,
    TraceReceipt,
};
use hephaestus_genome::{SourceFormat, compile_genome, compile_world};
use hephaestus_ledger::ArtifactId;
use hephaestus_ledger::{ArtifactStore, EventInput, EventStore};
use rusqlite::Connection;
use tempfile::tempdir;

const DAEMON: &str = env!("CARGO_BIN_EXE_hephaestusd");
const CLI: &str = env!("CARGO_BIN_EXE_hephaestus");
const REFERENCE_EVALUATOR: &str = env!("CARGO_BIN_EXE_hephaestus-reference-evaluator");
const REFERENCE_WORKER: &str = env!("CARGO_BIN_EXE_hephaestus-reference-worker");
const PROCESS_GUARDIAN: &str = env!("CARGO_BIN_EXE_hephaestus-process-guardian");

#[test]
#[allow(clippy::too_many_lines)]
fn markdown_reference_instructions_use_one_pinned_worker_for_paired_trials() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create source repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"sandboxed fixture\n")
        .expect("write repository fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);

    fs::create_dir_all(&data_dir).expect("create daemon data directory");
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700))
        .expect("protect daemon data directory");
    let producer_seed = [71_u8; 32];
    fs::write(data_dir.join("runtime-producer.key"), producer_seed).expect("write producer key");
    fs::set_permissions(
        data_dir.join("runtime-producer.key"),
        fs::Permissions::from_mode(0o600),
    )
    .expect("protect producer key");
    let signer = RunResultSigner::from_seed(producer_seed);
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open CAS");
    let visible = TrustedManifest::new(
        "seatbelt-visible-v1",
        Visibility::Visible,
        vec![TrustedTask::new("visible-task", "lower input", "LOWER INPUT").unwrap()],
    )
    .unwrap();
    let sealed = TrustedManifest::new(
        "seatbelt-sealed-v1",
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", "secret input", "SECRET INPUT").unwrap()],
    )
    .unwrap();
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).unwrap())
        .unwrap();
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).unwrap())
        .unwrap();
    let evaluator_id = artifacts
        .put(&fs::read(REFERENCE_EVALUATOR).unwrap())
        .unwrap();
    let verifier_id = artifacts
        .put(&signer.verifier().public_key_bytes())
        .unwrap();
    let world_source = format!(
        r#"{{"schema_version":1,"name":"seatbelt-reference","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
        visible_id.as_str(),
        sealed_id.as_str(),
        evaluator_id.as_str(),
        verifier_id.as_str()
    );
    let world = compile_world(&world_source, SourceFormat::Json, &artifacts).unwrap();
    let world_artifact = artifacts.put(world.canonical_json()).unwrap();
    let world_record = WorldRecord {
        world_id: world.id().to_owned(),
        name: world.name().to_owned(),
        artifact_id: world_artifact.as_str().to_owned(),
    };
    EventStore::open(data_dir.join("events.sqlite3"))
        .unwrap()
        .append(EventInput::new(
            "seatbelt-world",
            &world_record.world_id,
            "world.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&world_record).unwrap(),
        ))
        .unwrap();

    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    let instruction_file = |name: &str, parents: &str, operation: &str| {
        format!(
            "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{operation}\"}}\n```\n"
        )
    };
    let parent_path = directory.path().join("parent.md");
    fs::write(
        &parent_path,
        instruction_file("seatbelt-parent", "[]", "identity"),
    )
    .unwrap();
    let parent = match response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            parent_path.to_str().unwrap(),
            "--world",
            world.id(),
        ],
    ))
    .data
    .unwrap()
    {
        ResponseData::Genome { genome } => genome,
        other => panic!("unexpected parent registration: {other:?}"),
    };
    let candidate_path = directory.path().join("candidate.md");
    fs::write(
        &candidate_path,
        instruction_file(
            "seatbelt-candidate",
            &format!("[\"{}\"]", parent.genome_id),
            "ascii_uppercase",
        ),
    )
    .unwrap();
    let candidate = match response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            candidate_path.to_str().unwrap(),
            "--world",
            world.id(),
        ],
    ))
    .data
    .unwrap()
    {
        ResponseData::Genome { genome } => genome,
        other => panic!("unexpected candidate registration: {other:?}"),
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let deployed_worker = data_dir.join("reference-worker");
    let worker_bytes = fs::read(&deployed_worker).expect("read deployed worker");
    let worker_path = deployed_worker.clone();
    let snapshot_directory = data_dir.clone();
    let mutation = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let found = fs::read_dir(&snapshot_directory)
                .expect("read data directory")
                .flatten()
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("reference-worker-")
                })
                .is_some();
            if found {
                fs::write(&worker_path, b"replaced during paired evaluation")
                    .expect("replace deployed worker after snapshot");
                return true;
            }
            thread::sleep(Duration::from_micros(100));
        }
        false
    });
    let evaluation_response = cli(
        &data_dir,
        &[
            "arena",
            "evaluate",
            "seatbelt-reference-evaluation",
            &parent.genome_id,
            &candidate.genome_id,
        ],
    );
    let worker_replaced_after_snapshot = mutation.join().expect("join worker replacement");
    fs::write(&deployed_worker, &worker_bytes).expect("restore deployed worker");
    fs::set_permissions(&deployed_worker, fs::Permissions::from_mode(0o700))
        .expect("restore worker permissions");
    assert!(
        worker_replaced_after_snapshot,
        "paired evaluation never exposed its private worker snapshot"
    );
    let evaluation = response(&evaluation_response);
    let evaluation = match evaluation.data.unwrap() {
        ResponseData::Evaluation { evaluation } => evaluation,
        other => panic!("unexpected paired evaluation: {other:?}"),
    };
    assert_eq!(evaluation.parent_visible_correct, 0);
    assert_eq!(evaluation.candidate_visible_correct, 1);

    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .unwrap()
        .replay_verified()
        .unwrap();
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).unwrap();
    let receipts = history
        .iter()
        .filter(|event| event.event_type == "run.result_recorded")
        .map(|event| RunResultReceipt::parse_from_event(event, &signer.verifier()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 4);
    for receipt in receipts {
        let (input, expected) = match receipt.task_id.as_str() {
            "visible-task" => (
                "lower input",
                if receipt.genome_id == parent.genome_id {
                    "lower input"
                } else {
                    "LOWER INPUT"
                },
            ),
            "sealed-task" => (
                "secret input",
                if receipt.genome_id == parent.genome_id {
                    "secret input"
                } else {
                    "SECRET INPUT"
                },
            ),
            task => panic!("unexpected task in signed receipt: {task}"),
        };
        assert_eq!(
            receipt.input_commitment,
            blake3::hash(input.as_bytes()).to_hex().to_string()
        );
        assert_eq!(receipt.environment_id, reference_worker_environment_id());
        let output = artifacts
            .get(&ArtifactId::parse(receipt.stdout_artifact_id).unwrap())
            .unwrap();
        assert_eq!(output, expected.as_bytes());
    }
    let selected = response(&cli(
        &data_dir,
        &["arena", "select", "seatbelt-reference-evaluation"],
    ));
    let ResponseData::Selection { selection } = selected.data.unwrap() else {
        panic!("expected operator selection response");
    };
    assert!(!selection.receipt.invariant_gate_verified());
    assert!(!selection.receipt.promotion_eligible());

    let submitted = response(&cli(
        &data_dir,
        &["submit", "job-async-1", &candidate.genome_id],
    ));
    let ResponseData::Job { job, progress } = submitted.data.expect("job admission") else {
        panic!("expected job response");
    };
    assert_eq!(job.genome_id, candidate.genome_id);
    assert_eq!(progress.trace_events, 0);
    let repeated = response(&cli(
        &data_dir,
        &["submit", "job-async-1", &candidate.genome_id],
    ));
    let Some(ResponseData::Job {
        job: repeated_job, ..
    }) = repeated.data
    else {
        panic!("expected idempotent job response");
    };
    // An idempotent retry can observe a later lifecycle state than admission.
    let mut admitted_binding = job.clone();
    admitted_binding.state = repeated_job.state;
    admitted_binding.terminal = repeated_job.terminal;
    assert_eq!(repeated_job, admitted_binding);
    let conflicting = cli(&data_dir, &["submit", "job-async-1", &parent.genome_id]);
    assert!(!conflicting.status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let terminal = loop {
        let status = response(&cli(&data_dir, &["job", "status", "job-async-1"]));
        let ResponseData::Job { job, .. } = status.data.expect("job status") else {
            panic!("expected job status response");
        };
        if matches!(
            job.state,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        ) {
            break job;
        }
        assert!(Instant::now() < deadline, "asynchronous job did not finish");
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(terminal.state, JobState::Succeeded);
    assert_eq!(terminal.terminal, Some(JobTerminal::Succeeded));
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    daemon.stop();

    let restarted = Daemon::start_with_repository(&data_dir, &repository);
    let recovered = response(&cli(&data_dir, &["job", "status", "job-async-1"]));
    let Some(ResponseData::Job {
        job: recovered_job,
        progress,
    }) = recovered.data
    else {
        panic!("expected replayed job");
    };
    assert_eq!(recovered_job, terminal);
    assert!(progress.trace_events > 0);
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    restarted.stop();
}

fn runtime_environment_id() -> String {
    format!(
        "deterministic-v1.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git",
        env!("CARGO_PKG_VERSION"),
        RUN_RESULT_SCHEMA_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn reference_worker_environment_id() -> String {
    let worker_digest = blake3::hash(&fs::read(REFERENCE_WORKER).expect("read reference worker"))
        .to_hex()
        .to_string();
    let base = runtime_environment_id();
    let identity = format!("{base}|reference-instruction-language-v1|{worker_digest}");
    format!(
        "reference-v1.{}",
        blake3::hash(identity.as_bytes()).to_hex()
    )
}

struct Daemon {
    child: Child,
    data_dir: PathBuf,
}

impl Daemon {
    fn start(data_dir: &Path) -> Self {
        Self::start_with_repository(data_dir, Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    fn start_with_repository(data_dir: &Path, source_repository: &Path) -> Self {
        Self::start_with_worker(data_dir, source_repository, Path::new(REFERENCE_WORKER))
    }

    fn start_with_worker(data_dir: &Path, source_repository: &Path, worker_source: &Path) -> Self {
        Self::start_with_worker_and_arena_timeout(data_dir, source_repository, worker_source, None)
    }

    fn start_with_worker_and_arena_timeout(
        data_dir: &Path,
        source_repository: &Path,
        worker_source: &Path,
        timeout_millis: Option<u64>,
    ) -> Self {
        fs::create_dir_all(data_dir).expect("create daemon data directory");
        let evaluator = data_dir.join("reference-evaluator");
        fs::copy(REFERENCE_EVALUATOR, &evaluator).expect("copy evaluator executable");
        fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
            .expect("protect evaluator executable");
        let worker = data_dir.join("reference-worker");
        fs::copy(worker_source, &worker).expect("copy reference worker executable");
        fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
            .expect("protect reference worker executable");
        let mut command = ProcessCommand::new(DAEMON);
        command
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--source-repository")
            .arg(source_repository)
            .arg("--evaluator-executable")
            .arg(evaluator)
            .arg("--reference-worker-executable")
            .arg(worker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(timeout_millis) = timeout_millis {
            command.env(
                "HEPHAESTUS_TEST_ARENA_OVERALL_WALL_MILLIS",
                timeout_millis.to_string(),
            );
        }
        let mut child = command.spawn().expect("start daemon");
        let deadline = Instant::now() + Duration::from_secs(5);
        let socket = data_dir.join("control.sock");
        while UnixStream::connect(&socket).is_err() {
            if let Some(status) = child.try_wait().expect("inspect daemon") {
                panic!("daemon exited before serving: {status}");
            }
            assert!(Instant::now() < deadline, "daemon socket was not created");
            thread::sleep(Duration::from_millis(20));
        }
        Self {
            child,
            data_dir: data_dir.to_owned(),
        }
    }

    fn stop(mut self) {
        let output = cli(&self.data_dir, &["daemon", "stop"]);
        assert!(
            output.status.success(),
            "graceful daemon stop failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.child.wait().expect("wait for daemon");
    }

    fn crash(mut self) {
        self.child.kill().expect("stop daemon");
        self.child.wait().expect("wait for daemon");
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn daemon_evaluation_results_replay_and_feed_exact_authenticated_arena_events() {
    let directory = tempdir().unwrap();
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"paired daemon fixture\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);

    fs::create_dir_all(&data_dir).unwrap();
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let producer_seed = [17_u8; 32];
    fs::write(data_dir.join("runtime-producer.key"), producer_seed).unwrap();
    fs::set_permissions(
        data_dir.join("runtime-producer.key"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let signer = RunResultSigner::from_seed(producer_seed);
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).unwrap();
    let visible = TrustedManifest::new(
        "visible-v1",
        Visibility::Visible,
        vec![TrustedTask::new("visible-task", "visible input", "VISIBLE INPUT").unwrap()],
    )
    .unwrap();
    let sealed = TrustedManifest::new(
        "sealed-v1",
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", "sealed input", "SEALED INPUT").unwrap()],
    )
    .unwrap();
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).unwrap())
        .unwrap();
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).unwrap())
        .unwrap();
    let evaluator_id = artifacts
        .put(&fs::read(REFERENCE_EVALUATOR).unwrap())
        .unwrap();
    let verifier_id = artifacts
        .put(&signer.verifier().public_key_bytes())
        .unwrap();
    let invariant_manifest_id = artifacts
        .put(
            br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4096,"forbidden_ascii_bytes":[88]}"#,
        )
        .unwrap();
    let world_source = format!(
        r#"{{"schema_version":1,"name":"daemon-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}","arena.invariant_manifest":"{}"}}}}"#,
        visible_id.as_str(),
        sealed_id.as_str(),
        evaluator_id.as_str(),
        verifier_id.as_str(),
        invariant_manifest_id.as_str()
    );
    let world = compile_world(&world_source, SourceFormat::Json, &artifacts).unwrap();
    let world_artifact = artifacts.put(world.canonical_json()).unwrap();
    let world_record = WorldRecord {
        world_id: world.id().to_owned(),
        name: world.name().to_owned(),
        artifact_id: world_artifact.as_str().to_owned(),
    };
    let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).unwrap();
    ledger
        .append(EventInput::new(
            "daemon-arena-world",
            &world_record.world_id,
            "world.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&world_record).unwrap(),
        ))
        .unwrap();
    drop(ledger);

    let producer_key_path = data_dir.join("runtime-producer.key");
    fs::remove_file(&producer_key_path).unwrap();
    let missing_key_error = ControlPlane::open_with_repository(&data_dir, &repository)
        .err()
        .expect("an anchored World must reject a missing producer key");
    assert!(format!("{missing_key_error}").contains("registered World verifier"));
    assert!(!producer_key_path.exists());

    fs::write(&producer_key_path, [18_u8; 32]).unwrap();
    fs::set_permissions(&producer_key_path, fs::Permissions::from_mode(0o600)).unwrap();
    let replaced_key_error = ControlPlane::open_with_repository(&data_dir, &repository)
        .err()
        .expect("an anchored World must reject a replaced producer key");
    assert!(format!("{replaced_key_error}").contains("does not match registered World verifier"));
    fs::write(&producer_key_path, producer_seed).unwrap();
    fs::set_permissions(&producer_key_path, fs::Permissions::from_mode(0o600)).unwrap();

    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    let parent_path = directory.path().join("parent.md");
    let candidate_path = directory.path().join("candidate.md");
    let metadata = |name: &str, parents: &str, operation: &str| {
        format!(
            "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{operation}\"}}\n```\n"
        )
    };
    fs::write(&parent_path, metadata("daemon-parent", "[]", "identity")).unwrap();
    let parent = match response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            parent_path.to_str().unwrap(),
            "--world",
            world.id(),
        ],
    ))
    .data
    .unwrap()
    {
        ResponseData::Genome { genome } => genome,
        other => panic!("unexpected parent registration: {other:?}"),
    };
    fs::write(
        &candidate_path,
        metadata(
            "daemon-candidate",
            &format!("[\"{}\"]", parent.genome_id),
            "ascii_uppercase",
        ),
    )
    .unwrap();
    let candidate = match response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            candidate_path.to_str().unwrap(),
            "--world",
            world.id(),
        ],
    ))
    .data
    .unwrap()
    {
        ResponseData::Genome { genome } => genome,
        other => panic!("unexpected candidate registration: {other:?}"),
    };
    #[cfg(feature = "test-support")]
    let mut recorded_selections = Vec::new();
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let invalid_budget = cli(
        &data_dir,
        &[
            "evaluate",
            &parent.genome_id,
            "--task-id",
            "visible-task",
            "--input",
            "visible input",
            "--wall-millis",
            "10000",
            "--maximum-output-bytes",
            "1048576",
            "--maximum-cost-microusd",
            "1000000001",
        ],
    );
    assert!(!invalid_budget.status.success());
    assert_eq!(
        serde_json::from_slice::<ApiResponse>(&invalid_budget.stdout)
            .expect("decode invalid evaluation budget response")
            .error
            .expect("invalid evaluation budget error")
            .code,
        ApiErrorCode::InvalidRequest
    );
    let budget_history = EventStore::open(data_dir.join("events.sqlite3"))
        .unwrap()
        .replay_verified()
        .unwrap();
    assert!(
        !budget_history
            .iter()
            .any(|event| event.event_type == "run.result_recorded")
    );
    let sandbox_root = data_dir.join("sandboxes");
    assert!(!sandbox_root.exists() || fs::read_dir(sandbox_root).unwrap().next().is_none());
    let direct_run = response(&cli(&data_dir, &["run", &candidate.genome_id]));
    let direct_stdout_id = match direct_run.data.unwrap() {
        ResponseData::Run {
            stdout_artifact_id, ..
        } => stdout_artifact_id,
        other => panic!("unexpected instruction-bearing direct run: {other:?}"),
    };
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).unwrap();
    assert_eq!(
        artifacts
            .get(&ArtifactId::parse(direct_stdout_id).unwrap())
            .unwrap(),
        b"INVENTORY THE ISOLATED REPOSITORY WITHOUT MODIFYING IT OR USING THE NETWORK."
    );
    #[cfg(feature = "test-support")]
    {
        let first = response(&cli(
            &data_dir,
            &[
                "arena",
                "evaluate",
                "daemon-owned-pair",
                &parent.genome_id,
                &candidate.genome_id,
            ],
        ));
        let first_evaluation = match first.data.unwrap() {
            ResponseData::Evaluation { evaluation } => evaluation,
            other => panic!("unexpected paired evaluation response: {other:?}"),
        };
        assert_eq!(first_evaluation.parent_visible_correct, 0);
        assert_eq!(first_evaluation.candidate_visible_correct, 1);
        assert_eq!(first_evaluation.visible_total, 1);
        let same_genome = cli(
            &data_dir,
            &[
                "arena",
                "evaluate",
                "invalid-self-pair",
                &parent.genome_id,
                &parent.genome_id,
            ],
        );
        assert!(!same_genome.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&same_genome.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest
        );
        let before_retry = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        let paired_receipts = before_retry
            .iter()
            .filter(|event| event.event_id.starts_with("result:paired-"))
            .map(|event| RunResultReceipt::parse_from_event(event, &signer.verifier()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(paired_receipts.len(), 4);
        assert_eq!(
            paired_receipts
                .iter()
                .map(|receipt| receipt.source_revision.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            1,
            "every paired trial must use one pinned source revision"
        );
        for receipt in &paired_receipts {
            let task_input = match receipt.task_id.as_str() {
                "visible-task" => "visible input",
                "sealed-task" => "sealed input",
                other => panic!("unexpected daemon-owned task: {other}"),
            };
            assert_eq!(
                receipt.input_commitment,
                blake3::hash(task_input.as_bytes()).to_hex().to_string()
            );
            assert_eq!(receipt.seed, 42);
            assert_eq!(receipt.environment_id, reference_worker_environment_id());
            assert_eq!(receipt.budget.wall_millis, 10_000);
            assert_eq!(receipt.budget.maximum_output_bytes, 1_048_576);
            assert_eq!(receipt.budget.maximum_cost_microusd, 0);
            let expected = if receipt.genome_id == parent.genome_id {
                task_input.as_bytes().to_vec()
            } else {
                task_input.to_ascii_uppercase().into_bytes()
            };
            assert_eq!(
                artifacts
                    .get(
                        &hephaestus_ledger::ArtifactId::parse(receipt.stdout_artifact_id.clone())
                            .unwrap()
                    )
                    .unwrap(),
                expected
            );
        }
        for task_id in ["visible-task", "sealed-task"] {
            let parent_receipt = paired_receipts
                .iter()
                .find(|receipt| receipt.genome_id == parent.genome_id && receipt.task_id == task_id)
                .unwrap();
            let candidate_receipt = paired_receipts
                .iter()
                .find(|receipt| {
                    receipt.genome_id == candidate.genome_id && receipt.task_id == task_id
                })
                .unwrap();
            assert_eq!(
                parent_receipt.input_commitment,
                candidate_receipt.input_commitment
            );
            assert_eq!(parent_receipt.seed, candidate_receipt.seed);
            assert_eq!(
                parent_receipt.environment_id,
                candidate_receipt.environment_id
            );
            assert_eq!(parent_receipt.budget, candidate_receipt.budget);
            assert_ne!(
                parent_receipt.stdout_artifact_id,
                candidate_receipt.stdout_artifact_id
            );
        }
        let retried = response(&cli(
            &data_dir,
            &[
                "arena",
                "evaluate",
                "daemon-owned-pair",
                &parent.genome_id,
                &candidate.genome_id,
            ],
        ));
        assert_eq!(
            retried.data,
            Some(ResponseData::Evaluation {
                evaluation: first_evaluation,
            })
        );
        let unknown_selection = cli(&data_dir, &["arena", "select", "missing-evaluation"]);
        assert!(!unknown_selection.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&unknown_selection.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::NotFound
        );
        let selected = response(&cli(&data_dir, &["arena", "select", "daemon-owned-pair"]));
        let selection = match selected.data.unwrap() {
            ResponseData::Selection { selection } => selection,
            other => panic!("unexpected selection response: {other:?}"),
        };
        assert!(!selection.receipt.invariant_gate_verified());
        assert!(!selection.receipt.promotion_eligible());
        assert_eq!(selection.receipt.world_id(), world.id());
        assert_eq!(selection.receipt.correctness_regressions(), 0);
        assert_eq!(selection.receipt.correctness_improvements(), 2);
        recorded_selections.push(selection.clone());

        let invariants = match response(&cli(
            &data_dir,
            &["arena", "invariants", "daemon-owned-pair"],
        ))
        .data
        .unwrap()
        {
            ResponseData::ArenaInvariants { invariants } => invariants,
            other => panic!("unexpected invariant response: {other:?}"),
        };
        assert_eq!(invariants.receipt.evaluation_id, "daemon-owned-pair");
        assert_eq!(invariants.receipt.world_id, world.id());
        assert_eq!(invariants.receipt.total_evaluated_trials, 2);
        assert!(invariants.receipt.total_checks > 0);
        assert!(invariants.receipt.candidate_contract_satisfied);
        let safe_json = serde_json::to_string(&invariants).unwrap();
        for private in [
            "visible-task",
            "sealed-task",
            "visible input",
            "sealed input",
            "VISIBLE INPUT",
            "SEALED INPUT",
        ] {
            assert!(
                !safe_json.contains(private),
                "invariant API leaked {private}"
            );
        }
        let retried_invariants = match response(&cli(
            &data_dir,
            &["arena", "invariants", "daemon-owned-pair"],
        ))
        .data
        .unwrap()
        {
            ResponseData::ArenaInvariants { invariants } => invariants,
            other => panic!("unexpected invariant retry response: {other:?}"),
        };
        assert_eq!(retried_invariants.as_ref(), invariants.as_ref());
        let invariant_events = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == "invariants.recorded")
            .count();
        assert_eq!(
            invariant_events, 1,
            "idempotent retry duplicated the receipt"
        );

        // Force Arena's consumed-store error path by removing the durable
        // selection receipt bytes while leaving the verified event in history.
        let receipt_id = ArtifactId::parse(selection.event.receipt_artifact_id.clone()).unwrap();
        let artifacts = ArtifactStore::open(data_dir.join("blobs")).unwrap();
        let receipt_bytes = artifacts.get(&receipt_id).unwrap();
        let receipt_path = artifacts.path_for(&receipt_id);
        fs::remove_file(&receipt_path).unwrap();
        let corrupt_retry = cli(&data_dir, &["arena", "select", "daemon-owned-pair"]);
        assert!(!corrupt_retry.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&corrupt_retry.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::Internal
        );
        assert!(cli(&data_dir, &["status"]).status.success());
        assert!(!cli(&data_dir, &["replay"]).status.success());
        fs::write(&receipt_path, &receipt_bytes).unwrap();
        fs::set_permissions(&receipt_path, fs::Permissions::from_mode(0o600)).unwrap();

        assert_eq!(
            response(&cli(&data_dir, &["arena", "select", "daemon-owned-pair"],)).data,
            Some(ResponseData::Selection {
                selection: selection.clone()
            }),
            "selection retry returns the same receipt and event"
        );
        let after_selection_retry = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        assert_eq!(
            after_selection_retry
                .iter()
                .filter(|event| event.event_type == "selection.recorded")
                .count(),
            1
        );
        let after_retry = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        for event_type in ["run.result_recorded", "evaluation.recorded"] {
            assert_eq!(
                before_retry
                    .iter()
                    .filter(|event| event.event_type == event_type)
                    .count(),
                after_retry
                    .iter()
                    .filter(|event| event.event_type == event_type)
                    .count(),
                "retry duplicated {event_type}"
            );
        }
        let deployed_worker = data_dir.join("reference-worker");
        let worker_bytes = fs::read(&deployed_worker).unwrap();
        fs::write(&deployed_worker, b"changed reference worker").unwrap();
        let changed_worker = cli(&data_dir, &["run", &candidate.genome_id]);
        assert!(!changed_worker.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&changed_worker.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest
        );
        fs::write(&deployed_worker, worker_bytes).unwrap();
        fs::set_permissions(&deployed_worker, fs::Permissions::from_mode(0o700)).unwrap();
        for root in [data_dir.join("sandboxes"), data_dir.join("evaluator-runs")] {
            assert!(
                !root.exists() || fs::read_dir(&root).unwrap().next().is_none(),
                "paired evaluation left private execution state in {}",
                root.display()
            );
        }
        let deployed_evaluator = data_dir.join("reference-evaluator");
        fs::write(&deployed_evaluator, b"tampered evaluator").unwrap();
        let rejected_output = cli(
            &data_dir,
            &[
                "arena",
                "evaluate",
                "tampered-evaluator-pair",
                &parent.genome_id,
                &candidate.genome_id,
            ],
        );
        assert!(!rejected_output.status.success());
        let rejected = serde_json::from_slice::<ApiResponse>(&rejected_output.stdout).unwrap();
        assert_eq!(rejected.error.unwrap().code, ApiErrorCode::Internal);
        let after_rejection = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        assert_eq!(
            after_retry
                .iter()
                .filter(|event| event.event_type == "run.result_recorded")
                .count(),
            after_rejection
                .iter()
                .filter(|event| event.event_type == "run.result_recorded")
                .count(),
            "an untrusted evaluator must fail before candidate scheduling"
        );

        fs::write(&deployed_evaluator, fs::read(REFERENCE_EVALUATOR).unwrap()).unwrap();
        fs::set_permissions(&deployed_evaluator, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(cli(&data_dir, &["replay"]).status.success());
    }
    let mut parent_trials = Vec::new();
    let mut candidate_trials = Vec::new();
    for (task, input) in [
        ("visible-task", "visible input"),
        ("sealed-task", "sealed input"),
    ] {
        parent_trials.push((
            task.to_owned(),
            evaluation_run(&data_dir, &parent.genome_id, task, input),
        ));
        candidate_trials.push((
            task.to_owned(),
            evaluation_run(&data_dir, &candidate.genome_id, task, input),
        ));
    }
    daemon.stop();
    #[cfg(feature = "test-support")]
    {
        let prior_revision_id = "prior-revision-pair";
        let prior_prefix = format!(
            "paired-{}",
            &blake3::hash(prior_revision_id.as_bytes())
                .to_hex()
                .to_string()[..24]
        );
        let history = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        let signer = RunResultSigner::from_seed([17_u8; 32]);
        let existing_receipts = history
            .iter()
            .filter(|event| event.event_id.starts_with("result:paired-"))
            .map(|event| RunResultReceipt::parse_from_event(event, &signer.verifier()).unwrap())
            .take(2)
            .collect::<Vec<_>>();
        assert_eq!(existing_receipts.len(), 2);
        let mut first_prior_result = existing_receipts[0].clone();
        first_prior_result.run_id = format!("{prior_prefix}-parent-0");
        first_prior_result.source_revision = "a".repeat(40);
        let mut second_prior_result = existing_receipts[1].clone();
        second_prior_result.run_id = format!("{prior_prefix}-parent-1");
        second_prior_result.source_revision = "b".repeat(40);
        let prior_timestamp = history
            .last()
            .expect("paired history has events")
            .timestamp_millis
            + 1;
        let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).unwrap();
        ledger
            .append(signer.issue(first_prior_result, prior_timestamp).unwrap())
            .unwrap();
        ledger
            .append(
                signer
                    .issue(second_prior_result, prior_timestamp + 1)
                    .unwrap(),
            )
            .unwrap();
        drop(ledger);

        let revision_daemon = Daemon::start_with_repository(&data_dir, &repository);
        let conflicting_revision = cli(
            &data_dir,
            &[
                "arena",
                "evaluate",
                prior_revision_id,
                &parent.genome_id,
                &candidate.genome_id,
            ],
        );
        assert!(!conflicting_revision.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&conflicting_revision.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::Internal,
            "paired admission must reject prior results with conflicting pinned revisions"
        );
        revision_daemon.stop();
        rebuild_ledger_without(&data_dir, |event| {
            event.event_id == format!("result:{prior_prefix}-parent-0")
                || event.event_id == format!("result:{prior_prefix}-parent-1")
        });
    }
    // Simulate a daemon crash after the authenticated evaluation receipt was
    // committed but before its final Arena lifecycle event was persisted.
    rebuild_ledger_without(&data_dir, |event| {
        event.event_id == "arena-job:daemon-owned-pair:terminal"
    });
    let mut restarted = Daemon::start_with_repository(&data_dir, &repository);
    assert!(cli(&data_dir, &["replay"]).status.success());
    let reloaded_invariants = match response(&cli(
        &data_dir,
        &["arena", "invariants", "daemon-owned-pair"],
    ))
    .data
    .unwrap()
    {
        ResponseData::ArenaInvariants { invariants } => invariants,
        other => panic!("restart did not rehydrate invariant receipt: {other:?}"),
    };
    assert_eq!(
        reloaded_invariants.event.event_id,
        "arena:invariants:daemon-owned-pair:checked"
    );
    restarted.stop();
    rewrite_ledger_event_type(
        &data_dir,
        "arena:invariants:daemon-owned-pair:checked",
        "control.status",
    );
    assert!(
        ControlPlane::open_with_repository(&data_dir, &repository).is_err(),
        "replay must reject an invariant identity with a rewritten event type"
    );
    rewrite_ledger_event_type(
        &data_dir,
        "arena:invariants:daemon-owned-pair:checked",
        "invariants.recorded",
    );
    restarted = Daemon::start_with_repository(&data_dir, &repository);
    let recovered = Client::new(&data_dir)
        .request(Command::JobStatus {
            job_id: "daemon-owned-pair".to_owned(),
        })
        .unwrap();
    assert!(matches!(
        recovered.data,
        Some(ResponseData::ArenaJob { job })
            if job.state == JobState::Succeeded && job.evaluation.is_some()
    ));
    #[cfg(feature = "test-support")]
    {
        let expected = recorded_selections
            .pop()
            .expect("selection was recorded in the trusted flow");
        let Some(ResponseData::Selection {
            selection: recovered,
        }) = response(&cli(&data_dir, &["arena", "select", "daemon-owned-pair"])).data
        else {
            panic!("restart did not rehydrate the selection receipt");
        };
        assert_eq!(recovered.receipt, expected.receipt);
        assert_eq!(recovered.event.event_id, expected.event.event_id);
        assert_eq!(
            recovered.event.receipt_artifact_id,
            expected.event.receipt_artifact_id
        );
        assert!(cli(&data_dir, &["freeze"]).status.success());
        let frozen_proposal = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "frozen-proposal",
                "--selection-event",
                &recovered.event.event_id,
                "--parent",
                &candidate.genome_id,
                "--hypothesis",
                "A frozen Forge proposal must be rejected.",
            ],
        );
        assert!(!frozen_proposal.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&frozen_proposal.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest
        );
        let history = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        assert!(
            !history
                .iter()
                .any(|event| event.event_type == "forge.proposed")
        );
        assert!(cli(&data_dir, &["unfreeze"]).status.success());
        let hypothesis =
            "If task-facing behavior needs to preserve case, the child should return identity.";
        let unknown_selection_proposal = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "unknown-selection-proposal",
                "--selection-event",
                "missing-selection-event",
                "--parent",
                &candidate.genome_id,
                "--hypothesis",
                hypothesis,
            ],
        );
        assert!(!unknown_selection_proposal.status.success());
        let unknown_selection_error =
            serde_json::from_slice::<ApiResponse>(&unknown_selection_proposal.stdout)
                .unwrap()
                .error
                .unwrap();
        assert_eq!(unknown_selection_error.code, ApiErrorCode::NotFound);

        let wrong_type_selection_event = format!("genome:{}:registered", candidate.genome_id);
        let wrong_type_selection = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "wrong-type-selection-proposal",
                "--selection-event",
                &wrong_type_selection_event,
                "--parent",
                &candidate.genome_id,
                "--hypothesis",
                hypothesis,
            ],
        );
        assert!(!wrong_type_selection.status.success());
        let wrong_type_error = serde_json::from_slice::<ApiResponse>(&wrong_type_selection.stdout)
            .unwrap()
            .error
            .unwrap();
        assert_eq!(wrong_type_error.code, ApiErrorCode::InvalidRequest);
        assert_eq!(
            wrong_type_error.message,
            "selection_event_id does not identify a selection"
        );

        let invalid_hypothesis = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "invalid-hypothesis-proposal",
                "--selection-event",
                &recovered.event.event_id,
                "--parent",
                &candidate.genome_id,
                "--hypothesis",
                "  ",
            ],
        );
        assert!(!invalid_hypothesis.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&invalid_hypothesis.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest
        );
        let noncanonical_prompt_path = directory.path().join("noncanonical-prompt.md");
        let canonical_uppercase = "{\"schema_version\":1,\"operation\":\"ascii_uppercase\"}";
        let noncanonical_uppercase =
            "{ \"schema_version\": 1, \"operation\": \"ascii_uppercase\" }";
        let noncanonical_source = metadata(
            "daemon-candidate-noncanonical",
            &format!("[\"{}\"]", parent.genome_id),
            "ascii_uppercase",
        )
        .replace(canonical_uppercase, noncanonical_uppercase);
        fs::write(&noncanonical_prompt_path, noncanonical_source).unwrap();
        let noncanonical_genome = match response(&cli(
            &data_dir,
            &[
                "genome",
                "register",
                noncanonical_prompt_path.to_str().unwrap(),
                "--world",
                world.id(),
            ],
        ))
        .data
        .unwrap()
        {
            ResponseData::Genome { genome } => genome,
            other => panic!("unexpected noncanonical prompt Genome: {other:?}"),
        };
        let noncanonical_evaluation_id = "noncanonical-prompt-pair";
        assert!(matches!(
            response(&cli(
                &data_dir,
                &[
                    "arena",
                    "evaluate",
                    noncanonical_evaluation_id,
                    &parent.genome_id,
                    &noncanonical_genome.genome_id,
                ],
            ))
            .data,
            Some(ResponseData::Evaluation { .. })
        ));
        let noncanonical_selection = match response(&cli(
            &data_dir,
            &["arena", "select", noncanonical_evaluation_id],
        ))
        .data
        .unwrap()
        {
            ResponseData::Selection { selection } => selection,
            other => panic!("unexpected noncanonical prompt selection: {other:?}"),
        };
        assert_eq!(
            noncanonical_selection.receipt.candidate_genome_id(),
            noncanonical_genome.genome_id
        );
        let unsupported_prompt_proposal = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "unsupported-prompt-proposal",
                "--selection-event",
                &noncanonical_selection.event.event_id,
                "--parent",
                &noncanonical_genome.genome_id,
                "--hypothesis",
                hypothesis,
            ],
        );
        assert!(!unsupported_prompt_proposal.status.success());
        let unsupported_prompt_error =
            serde_json::from_slice::<ApiResponse>(&unsupported_prompt_proposal.stdout)
                .unwrap()
                .error
                .unwrap();
        assert_eq!(unsupported_prompt_error.code, ApiErrorCode::InvalidRequest);
        assert_eq!(
            unsupported_prompt_error.message,
            "the selected candidate prompt is outside the Forge mutation scope"
        );

        let proposal_args = [
            "genome",
            "propose",
            "identity-child-proposal",
            "--selection-event",
            recovered.event.event_id.as_str(),
            "--parent",
            candidate.genome_id.as_str(),
            "--hypothesis",
            hypothesis,
        ];
        let proposal = match response(&cli(&data_dir, &proposal_args)).data.unwrap() {
            ResponseData::ForgeProposal { proposal } => *proposal,
            other => panic!("unexpected Forge proposal response: {other:?}"),
        };
        assert!(!proposal.promotion_eligible);
        assert_eq!(proposal.payload.parent_genome_id, candidate.genome_id);
        assert_eq!(proposal.payload.child.world_id, world.id());
        assert_eq!(
            proposal.payload.child.parent_ids,
            std::slice::from_ref(&candidate.genome_id)
        );
        assert_eq!(proposal.payload.hypothesis, hypothesis);
        assert_eq!(
            proposal.payload.selection_event_hash,
            recovered.event.event_hash
        );
        assert_eq!(proposal.payload.operation_before, "ascii_uppercase");
        assert_eq!(proposal.payload.operation_after, "identity");
        let prompt = response(&cli(
            &data_dir,
            &["genome", "prompt", &proposal.payload.child.genome_id],
        ));
        assert!(
            matches!(
                &prompt.data,
                Some(ResponseData::GenomePrompt { prompt, .. })
                    if prompt == "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n"
            ),
            "unexpected child prompt: {:?}",
            prompt.data
        );
        assert_eq!(
            response(&cli(&data_dir, &proposal_args)).data,
            Some(ResponseData::ForgeProposal {
                proposal: Box::new(proposal.clone())
            })
        );
        let conflict = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "identity-child-proposal",
                "--selection-event",
                &recovered.event.event_id,
                "--parent",
                &candidate.genome_id,
                "--hypothesis",
                "A different hypothesis must not reuse this proposal key.",
            ],
        );
        assert!(!conflict.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&conflict.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest
        );
        let wrong_parent = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "wrong-parent-proposal",
                "--selection-event",
                &recovered.event.event_id,
                "--parent",
                &parent.genome_id,
                "--hypothesis",
                hypothesis,
            ],
        );
        assert!(!wrong_parent.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&wrong_parent.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest
        );

        let proposal_history = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        assert_eq!(
            proposal_history
                .iter()
                .filter(|event| event.event_type == "forge.proposed")
                .count(),
            1
        );

        let duplicate_child_path = directory.path().join("pre-registered-forge-child.md");
        let candidate_parent = format!("[\"{}\"]", candidate.genome_id);
        fs::write(
            &duplicate_child_path,
            metadata(
                "daemon-candidate-forge-duplicate-child-proposal",
                &candidate_parent,
                "identity",
            ),
        )
        .unwrap();
        let duplicate_child = cli(
            &data_dir,
            &[
                "genome",
                "register",
                duplicate_child_path.to_str().unwrap(),
                "--world",
                world.id(),
            ],
        );
        let duplicate_child_record = match response(&duplicate_child).data.unwrap() {
            ResponseData::Genome { genome } => genome,
            other => panic!("unexpected duplicate child registration: {other:?}"),
        };
        let duplicate_child_proposal = cli(
            &data_dir,
            &[
                "genome",
                "propose",
                "duplicate-child-proposal",
                "--selection-event",
                &recovered.event.event_id,
                "--parent",
                &candidate.genome_id,
                "--hypothesis",
                hypothesis,
            ],
        );
        assert!(!duplicate_child_proposal.status.success());
        let duplicate_child_error =
            serde_json::from_slice::<ApiResponse>(&duplicate_child_proposal.stdout)
                .unwrap()
                .error
                .unwrap();
        assert_eq!(duplicate_child_error.code, ApiErrorCode::InvalidRequest);
        assert_eq!(
            duplicate_child_error.message,
            "derived child identity is already registered"
        );
        assert_eq!(
            duplicate_child_record.name,
            "daemon-candidate-forge-duplicate-child-proposal"
        );

        let append_retry_id = "append-retry-proposal";
        let database = data_dir.join("events.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER reject_fixture_forge_proposal
                 BEFORE INSERT ON events
                 WHEN NEW.event_type = 'forge.proposed'
                  AND NEW.event_id = 'forge:append-retry-proposal:proposed'
                 BEGIN
                   SELECT RAISE(ABORT, 'fixture rejects Forge proposal append');
                 END;",
            )
            .unwrap();
        let append_retry_args = [
            "genome",
            "propose",
            append_retry_id,
            "--selection-event",
            recovered.event.event_id.as_str(),
            "--parent",
            candidate.genome_id.as_str(),
            "--hypothesis",
            hypothesis,
        ];
        let rejected_append = cli(&data_dir, &append_retry_args);
        assert!(!rejected_append.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&rejected_append.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::Internal
        );
        assert!(cli(&data_dir, &["status"]).status.success());
        let history_without_forge = EventStore::open(&database)
            .unwrap()
            .replay_verified()
            .unwrap();
        assert!(
            !history_without_forge
                .iter()
                .any(|event| event.event_id == "forge:append-retry-proposal:proposed")
        );
        connection
            .execute_batch("DROP TRIGGER reject_fixture_forge_proposal;")
            .unwrap();
        let retried_append = response(&cli(&data_dir, &append_retry_args));
        assert!(matches!(
            retried_append.data,
            Some(ResponseData::ForgeProposal { proposal })
                if proposal.payload.proposal_id == append_retry_id
        ));
        let history_after_retry = EventStore::open(&database)
            .unwrap()
            .replay_verified()
            .unwrap();
        assert_eq!(
            history_after_retry
                .iter()
                .filter(|event| event.event_type == "forge.proposed")
                .count(),
            2
        );
        assert_eq!(
            history_after_retry
                .iter()
                .filter(|event| event.event_id == "forge:append-retry-proposal:proposed")
                .count(),
            1
        );

        restarted.stop();
        let history = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        let proposal_event = history
            .iter()
            .find(|event| event.event_type == "forge.proposed")
            .expect("Forge proposal is durable");
        let canonical_payload = proposal_event.payload.clone();
        let canonical_actor = proposal_event.actor.clone();
        let canonical_hash = proposal_event.hash;
        let canonical_sequence = proposal_event.sequence;
        let mut forged: serde_json::Value =
            serde_json::from_slice(&proposal_event.payload).unwrap();
        forged["selection_event_hash"] = serde_json::Value::String("0".repeat(64));
        rewrite_ledger_event_payload(
            &data_dir,
            &proposal_event.event_id,
            &serde_json::to_vec(&forged).unwrap(),
            None,
        );
        let Err(selection_hash_error) = ControlPlane::open_with_repository(&data_dir, &repository)
        else {
            panic!("forged selection hash was accepted");
        };
        assert!(
            matches!(
                selection_hash_error,
                ControlError::Projection(ref message)
                    if message.contains("Forge proposal is not bound to its selected candidate")
            ),
            "unexpected selection hash rejection: {selection_hash_error:?}"
        );
        rewrite_ledger_event_payload(
            &data_dir,
            &proposal_event.event_id,
            &canonical_payload,
            Some("untrusted-operator"),
        );
        let Err(actor_error) = ControlPlane::open_with_repository(&data_dir, &repository) else {
            panic!("untrusted Forge actor was accepted");
        };
        assert!(
            matches!(
                actor_error,
                ControlError::Projection(ref message)
                    if message.contains("Forge proposal event identity is invalid")
            ),
            "unexpected actor rejection: {actor_error:?}"
        );
        rewrite_ledger_event_payload(
            &data_dir,
            &proposal_event.event_id,
            &canonical_payload,
            Some(&canonical_actor),
        );
        let restored_history = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        let restored_proposal = restored_history
            .iter()
            .find(|event| event.event_id == proposal_event.event_id)
            .expect("restored Forge proposal is durable");
        assert_eq!(restored_proposal.payload, canonical_payload);
        assert_eq!(restored_proposal.actor, canonical_actor);
        assert_eq!(restored_proposal.hash, canonical_hash);
        assert_eq!(restored_proposal.sequence, canonical_sequence);
        restarted = Daemon::start_with_repository(&data_dir, &repository);
        assert!(cli(&data_dir, &["replay"]).status.success());

        let child_evaluation_id = "identity-child-assessment-pair";
        let child_evaluation = response(&cli(
            &data_dir,
            &[
                "arena",
                "evaluate",
                child_evaluation_id,
                &proposal.payload.parent_genome_id,
                &proposal.payload.child.genome_id,
            ],
        ));
        assert!(matches!(
            child_evaluation.data,
            Some(ResponseData::Evaluation { ref evaluation })
                if evaluation.parent_genome_id == proposal.payload.parent_genome_id
                    && evaluation.candidate_genome_id == proposal.payload.child.genome_id
        ));
        let child_selection =
            match response(&cli(&data_dir, &["arena", "select", child_evaluation_id]))
                .data
                .expect("child selection")
            {
                ResponseData::Selection { selection } => *selection,
                other => panic!("unexpected child selection response: {other:?}"),
            };
        assert_eq!(
            child_selection.receipt.candidate_genome_id(),
            proposal.payload.child.genome_id
        );
        assert_eq!(
            child_selection.receipt.parent_genome_id(),
            proposal.payload.parent_genome_id
        );

        let proposal_source_selection = cli(
            &data_dir,
            &[
                "genome",
                "assess",
                "wrong-pair-assessment",
                "--proposal",
                &proposal.payload.proposal_id,
                "--selection-event",
                &recovered.event.event_id,
            ],
        );
        assert!(!proposal_source_selection.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&proposal_source_selection.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest,
            "the pre-proposal source selection cannot assess its child"
        );
        let unknown_assessment_proposal = cli(
            &data_dir,
            &[
                "genome",
                "assess",
                "unknown-proposal-assessment",
                "--proposal",
                "missing-proposal",
                "--selection-event",
                &child_selection.event.event_id,
            ],
        );
        assert!(!unknown_assessment_proposal.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&unknown_assessment_proposal.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::NotFound
        );
        let unknown_assessment_selection = cli(
            &data_dir,
            &[
                "genome",
                "assess",
                "unknown-selection-assessment",
                "--proposal",
                &proposal.payload.proposal_id,
                "--selection-event",
                "missing-selection-event",
            ],
        );
        assert!(!unknown_assessment_selection.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&unknown_assessment_selection.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::NotFound
        );

        assert!(cli(&data_dir, &["freeze"]).status.success());
        let assessment_args = [
            "genome",
            "assess",
            "identity-child-assessment",
            "--proposal",
            proposal.payload.proposal_id.as_str(),
            "--selection-event",
            child_selection.event.event_id.as_str(),
        ];
        let assessment = match response(&cli(&data_dir, &assessment_args))
            .data
            .expect("Forge assessment")
        {
            ResponseData::ForgeAssessment { assessment } => *assessment,
            other => panic!("unexpected Forge assessment response: {other:?}"),
        };
        assert_eq!(assessment.payload.schema_version, 1);
        assert_eq!(
            assessment.payload.assessment_id,
            "identity-child-assessment"
        );
        assert_eq!(assessment.payload.proposal_id, proposal.payload.proposal_id);
        assert_eq!(
            assessment.payload.proposal_event_id,
            proposal.event.event_id
        );
        assert_eq!(
            assessment.payload.proposal_event_hash,
            proposal.event.event_hash
        );
        assert_eq!(
            assessment.payload.selection_event_id,
            child_selection.event.event_id
        );
        assert_eq!(
            assessment.payload.selection_event_hash,
            child_selection.event.event_hash
        );
        assert_eq!(
            assessment.payload.selection_receipt_artifact_id,
            child_selection.event.receipt_artifact_id
        );
        assert_eq!(assessment.payload.evaluation_id, child_evaluation_id);
        assert_eq!(
            assessment.payload.evaluation_event_id,
            child_selection.receipt.evaluation_event_id()
        );
        assert_eq!(
            assessment.payload.evaluation_event_hash,
            child_selection.receipt.evaluation_event_hash()
        );
        assert_eq!(
            assessment.payload.parent_genome_id,
            proposal.payload.parent_genome_id
        );
        assert_eq!(assessment.payload.world_id, proposal.payload.world_id);
        assert_eq!(
            assessment.payload.child_genome_id,
            proposal.payload.child.genome_id
        );
        assert_eq!(
            assessment.payload.outcome,
            if child_selection.receipt.metrics_eligible() {
                ForgeAssessmentOutcome::MetricsPassed
            } else {
                ForgeAssessmentOutcome::MetricsRejected
            }
        );
        assert!(!assessment.payload.invariant_gate_verified);
        assert!(!assessment.payload.promotion_eligible);
        assert_eq!(
            assessment.event.event_id,
            "forge-assessment:identity-child-assessment:recorded"
        );
        assert_eq!(
            assessment.event.aggregate_id,
            format!("forge:{}", proposal.payload.proposal_id)
        );
        assert!(proposal.event.sequence < child_selection.event.sequence);
        assert!(child_selection.event.sequence < assessment.event.sequence);
        assert!(matches!(
            response(&cli(&data_dir, &["status"])).data,
            Some(ResponseData::Status { frozen: true, .. })
        ));
        assert_eq!(
            response(&cli(&data_dir, &assessment_args)).data,
            Some(ResponseData::ForgeAssessment {
                assessment: Box::new(assessment.clone())
            })
        );
        let conflicting_assessment_reuse = cli(
            &data_dir,
            &[
                "genome",
                "assess",
                "identity-child-assessment",
                "--proposal",
                &proposal.payload.proposal_id,
                "--selection-event",
                &recovered.event.event_id,
            ],
        );
        assert!(!conflicting_assessment_reuse.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&conflicting_assessment_reuse.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::InvalidRequest
        );
        restarted.stop();
        restarted = Daemon::start_with_repository(&data_dir, &repository);
        assert!(cli(&data_dir, &["replay"]).status.success());
        assert_eq!(
            response(&cli(&data_dir, &assessment_args)).data,
            Some(ResponseData::ForgeAssessment {
                assessment: Box::new(assessment)
            })
        );
        assert!(cli(&data_dir, &["unfreeze"]).status.success());
        assert!(cli(&data_dir, &["replay"]).status.success());

        restarted.stop();
        let slow_worker = directory.path().join("assessment-active-worker.sh");
        fs::write(
            &slow_worker,
            "#!/bin/sh\nprintf started > \"$HOME/assessment-worker-started\"\nexec /bin/sleep 60\n",
        )
        .expect("write bounded active-job worker");
        fs::set_permissions(&slow_worker, fs::Permissions::from_mode(0o700))
            .expect("make active-job worker executable");
        restarted = Daemon::start_with_worker(&data_dir, &repository, &slow_worker);
        let active_job_id = "assessment-active-job";
        let active_assessment_args = [
            "genome",
            "assess",
            "assessment-while-active",
            "--proposal",
            proposal.payload.proposal_id.as_str(),
            "--selection-event",
            child_selection.event.event_id.as_str(),
        ];
        assert!(matches!(
            response(&cli(
                &data_dir,
                &["submit", active_job_id, &proposal.payload.child.genome_id]
            ))
            .data,
            Some(ResponseData::Job { .. })
        ));
        let worker_started_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let started = fs::read_dir(data_dir.join("sandboxes"))
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| {
                    entry
                        .path()
                        .join("execution/assessment-worker-started")
                        .is_file()
                });
            if started {
                break;
            }
            assert!(
                Instant::now() < worker_started_deadline,
                "active assessment worker did not start"
            );
            thread::sleep(Duration::from_millis(10));
        }
        let assessment_while_active = cli(&data_dir, &active_assessment_args);
        assert!(!assessment_while_active.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&assessment_while_active.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::Busy,
            "assessment is refused while a job is active"
        );
        let invariants_while_active = cli(&data_dir, &["arena", "invariants", "anything"]);
        assert!(!invariants_while_active.status.success());
        assert_eq!(
            serde_json::from_slice::<ApiResponse>(&invariants_while_active.stdout)
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::Busy,
            "invariant checking is refused while a job is active"
        );
        let active_history = EventStore::open(data_dir.join("events.sqlite3"))
            .expect("open active assessment ledger")
            .replay_verified()
            .expect("verify active assessment ledger");
        assert_eq!(
            active_history
                .iter()
                .filter(|event| event.event_type == "forge.assessed")
                .count(),
            1,
            "a busy assessment must not append an event"
        );
        assert!(
            !active_history
                .iter()
                .any(|event| event.event_type == "control.arena_invariants")
        );
        assert!(matches!(
            response(&cli(&data_dir, &["kill", "--all"])).data,
            Some(ResponseData::Acknowledged { .. })
        ));
        let active_terminal_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let active_status = response(&cli(&data_dir, &["job", "status", active_job_id]));
            let Some(ResponseData::Job { job, .. }) = active_status.data else {
                panic!("active assessment job status is missing");
            };
            if job.state == JobState::Interrupted {
                assert_eq!(job.terminal, Some(JobTerminal::Cancelled));
                break;
            }
            assert!(
                Instant::now() < active_terminal_deadline,
                "active assessment test job did not terminate"
            );
            thread::sleep(Duration::from_millis(10));
        }
        restarted.stop();
        restarted = Daemon::start_with_repository(&data_dir, &repository);
    }
    restarted.stop();

    #[cfg(feature = "test-support")]
    {
        #[derive(Clone, serde::Serialize)]
        struct RestartedArenaJob {
            job_id: String,
            evaluation_id: String,
            parent_genome_id: String,
            candidate_genome_id: String,
            world_id: String,
            visible_manifest_id: String,
            sealed_manifest_id: String,
            evaluator_id: String,
            source_revision: String,
            worker_digest: String,
            environment_id: String,
            seed: u64,
            trial_budget: RunBudgetReceipt,
            overall_budget: RunBudgetReceipt,
            ordered_trial_run_ids: Vec<String>,
            parent_trial_count: u32,
            total_trials: u32,
            plan_commitment: String,
            caller_id: String,
            receipt_timestamp_millis: i64,
            completed_trials: u32,
            phase: ArenaJobPhase,
            state: JobState,
            terminal: Option<JobTerminal>,
            evaluation: Option<hephaestus_control::EvaluationRecord>,
        }

        let mismatch_daemon = Daemon::start_with_repository(&data_dir, &repository);
        assert!(matches!(
            response(&cli(
                &data_dir,
                &[
                    "arena",
                    "evaluate",
                    "trial-mismatch-pair",
                    &parent.genome_id,
                    &candidate.genome_id,
                ],
            ))
            .data,
            Some(ResponseData::Evaluation { .. })
        ));
        mismatch_daemon.stop();

        let evaluation_id = "crash-recovery-arena-job";
        let worker_digest = blake3::hash(&fs::read(data_dir.join("reference-worker")).unwrap())
            .to_hex()
            .to_string();
        let environment_id = reference_worker_environment_id();
        let revision = String::from_utf8(
            ProcessCommand::new("git")
                .args(["-C", repository.to_str().unwrap(), "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let run_prefix = format!(
            "paired-{}",
            &blake3::hash(evaluation_id.as_bytes()).to_hex().to_string()[..24]
        );
        let run_ids = vec![
            format!("{run_prefix}-parent-0"),
            format!("{run_prefix}-parent-1"),
            format!("{run_prefix}-candidate-0"),
            format!("{run_prefix}-candidate-1"),
        ];
        let plan_commitment = blake3::hash(
            &serde_json::to_vec(&(
                evaluation_id,
                &parent.genome_id,
                &candidate.genome_id,
                world.id(),
                visible_id.as_str(),
                sealed_id.as_str(),
                &revision,
                &environment_id,
                &run_ids,
            ))
            .unwrap(),
        )
        .to_hex()
        .to_string();
        let interrupted = RestartedArenaJob {
            job_id: evaluation_id.to_owned(),
            evaluation_id: evaluation_id.to_owned(),
            parent_genome_id: parent.genome_id.clone(),
            candidate_genome_id: candidate.genome_id.clone(),
            world_id: world.id().to_owned(),
            visible_manifest_id: visible_id.as_str().to_owned(),
            sealed_manifest_id: sealed_id.as_str().to_owned(),
            evaluator_id: evaluator_id.as_str().to_owned(),
            source_revision: revision,
            worker_digest,
            environment_id,
            seed: 42,
            trial_budget: RunBudgetReceipt {
                wall_millis: 10_000,
                maximum_output_bytes: 1_048_576,
                maximum_cost_microusd: 0,
            },
            overall_budget: RunBudgetReceipt {
                wall_millis: 50_000,
                maximum_output_bytes: 4_194_304,
                maximum_cost_microusd: 0,
            },
            ordered_trial_run_ids: run_ids,
            parent_trial_count: 2,
            total_trials: 4,
            plan_commitment,
            caller_id: "control-daemon".to_owned(),
            receipt_timestamp_millis: 1_800_000_000_001,
            completed_trials: 0,
            phase: ArenaJobPhase::Preparing,
            state: JobState::Admitted,
            terminal: None,
            evaluation: None,
        };
        let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).unwrap();
        ledger
            .append(EventInput::new(
                format!("arena-job:{evaluation_id}:admitted"),
                format!("arena-job:{evaluation_id}"),
                "arena.job.admitted",
                "daemon-runtime",
                1_800_000_000_001,
                serde_json::to_vec(&interrupted).unwrap(),
            ))
            .unwrap();
        drop(ledger);
        drop(ControlPlane::open_with_repository(&data_dir, &repository).unwrap());
        let recovered_history = EventStore::open(data_dir.join("events.sqlite3"))
            .unwrap()
            .replay_verified()
            .unwrap();
        let recovery_terminals = recovered_history
            .iter()
            .filter(|event| {
                event.event_type == "arena.job.terminal"
                    && event.event_id == format!("arena-job:{evaluation_id}:terminal")
            })
            .collect::<Vec<_>>();
        assert_eq!(recovery_terminals.len(), 1);
        let recovered: serde_json::Value =
            serde_json::from_slice(&recovery_terminals[0].payload).unwrap();
        assert_eq!(recovered["state"], "interrupted");
        assert_eq!(recovered["terminal"], "interrupted");
        drop(ControlPlane::open_with_repository(&data_dir, &repository).unwrap());

        let incomplete_id = "incomplete-recovery";
        let incomplete_prefix = format!(
            "paired-{}",
            &blake3::hash(incomplete_id.as_bytes()).to_hex().to_string()[..24]
        );
        let incomplete_run_ids = vec![
            format!("{incomplete_prefix}-parent-0"),
            format!("{incomplete_prefix}-parent-1"),
            format!("{incomplete_prefix}-candidate-0"),
            format!("{incomplete_prefix}-candidate-1"),
        ];
        let incomplete_commitment = blake3::hash(
            &serde_json::to_vec(&(
                incomplete_id,
                &parent.genome_id,
                &candidate.genome_id,
                world.id(),
                visible_id.as_str(),
                sealed_id.as_str(),
                &interrupted.source_revision,
                &interrupted.environment_id,
                &incomplete_run_ids,
            ))
            .unwrap(),
        )
        .to_hex()
        .to_string();
        let incomplete = RestartedArenaJob {
            job_id: incomplete_id.to_owned(),
            evaluation_id: incomplete_id.to_owned(),
            ordered_trial_run_ids: incomplete_run_ids,
            plan_commitment: incomplete_commitment,
            ..interrupted.clone()
        };
        let receipt_timestamp = recovered_history
            .last()
            .expect("recovered history has events")
            .timestamp_millis
            + 1;
        let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).unwrap();
        ledger
            .append(EventInput::new(
                format!("arena-job:{incomplete_id}:admitted"),
                format!("arena-job:{incomplete_id}"),
                "arena.job.admitted",
                "daemon-runtime",
                receipt_timestamp,
                serde_json::to_vec(&incomplete).unwrap(),
            ))
            .unwrap();
        ledger
            .append(EventInput::new(
                format!("arena:evaluation:{incomplete_id}:recorded"),
                format!("arena-evaluation:{incomplete_id}"),
                "evaluation.recorded",
                "daemon-runtime",
                receipt_timestamp + 1,
                serde_json::to_vec(&serde_json::json!({"evaluation_id": incomplete_id})).unwrap(),
            ))
            .unwrap();
        drop(ledger);
        assert!(matches!(
            ControlPlane::open_with_repository(&data_dir, &repository),
            Err(ControlError::Projection(message))
                if message.contains("Arena receipt is missing complete signed trial evidence")
        ));

        let cancelled_id = "cancelled-recovery";
        let cancelled_prefix = format!(
            "paired-{}",
            &blake3::hash(cancelled_id.as_bytes()).to_hex().to_string()[..24]
        );
        let cancelled_run_ids = vec![
            format!("{cancelled_prefix}-parent-0"),
            format!("{cancelled_prefix}-parent-1"),
            format!("{cancelled_prefix}-candidate-0"),
            format!("{cancelled_prefix}-candidate-1"),
        ];
        let cancelled_commitment = blake3::hash(
            &serde_json::to_vec(&(
                cancelled_id,
                &parent.genome_id,
                &candidate.genome_id,
                world.id(),
                visible_id.as_str(),
                sealed_id.as_str(),
                &interrupted.source_revision,
                &interrupted.environment_id,
                &cancelled_run_ids,
            ))
            .unwrap(),
        )
        .to_hex()
        .to_string();
        let cancelled = RestartedArenaJob {
            job_id: cancelled_id.to_owned(),
            evaluation_id: cancelled_id.to_owned(),
            ordered_trial_run_ids: cancelled_run_ids,
            plan_commitment: cancelled_commitment,
            state: JobState::CancellationRequested,
            ..incomplete
        };
        let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).unwrap();
        ledger
            .append(EventInput::new(
                format!("arena-job:{cancelled_id}:admitted"),
                format!("arena-job:{cancelled_id}"),
                "arena.job.admitted",
                "daemon-runtime",
                receipt_timestamp + 2,
                serde_json::to_vec(&RestartedArenaJob {
                    state: JobState::Admitted,
                    ..cancelled.clone()
                })
                .unwrap(),
            ))
            .unwrap();
        ledger
            .append(EventInput::new(
                format!("arena-job:{cancelled_id}:cancellation_requested"),
                format!("arena-job:{cancelled_id}"),
                "arena.job.cancellation_requested",
                "daemon-runtime",
                receipt_timestamp + 3,
                serde_json::to_vec(&cancelled).unwrap(),
            ))
            .unwrap();
        ledger
            .append(EventInput::new(
                format!("arena:evaluation:{cancelled_id}:recorded"),
                format!("arena-evaluation:{cancelled_id}"),
                "evaluation.recorded",
                "daemon-runtime",
                receipt_timestamp + 4,
                serde_json::to_vec(&serde_json::json!({"evaluation_id": cancelled_id})).unwrap(),
            ))
            .unwrap();
        drop(ledger);
        assert!(matches!(
            ControlPlane::open_with_repository(&data_dir, &repository),
            Err(ControlError::Projection(message))
                if message.contains("cancelled Arena job has a committed evaluation receipt")
        ));

        let events_path = data_dir.join("events.sqlite3");
        let assert_tamper_rejected = |field: &str, from: &str, to: &str| {
            for suffix in ["", "-wal", "-shm"] {
                let _ignored = fs::remove_file(format!("{}{suffix}", events_path.display()));
            }
            let mut ledger = EventStore::open(&events_path).unwrap();
            let mut altered = false;
            for event in &recovered_history {
                let mut payload = event.payload.clone();
                if !altered
                    && event.event_type == "arena.job.terminal"
                    && String::from_utf8_lossy(&payload).contains("\"state\":\"succeeded\"")
                {
                    let canonical = String::from_utf8(payload).unwrap();
                    let original = format!("\"{field}\":{from}");
                    assert!(canonical.contains(&original));
                    payload = canonical
                        .replace(&original, &format!("\"{field}\":{to}"))
                        .into_bytes();
                    altered = true;
                }
                ledger
                    .append(EventInput::new(
                        event.event_id.clone(),
                        event.aggregate_id.clone(),
                        event.event_type.clone(),
                        event.actor.clone(),
                        event.timestamp_millis,
                        payload,
                    ))
                    .unwrap();
            }
            assert!(altered, "tampered a canonical succeeded Arena terminal");
            drop(ledger);
            assert!(ControlPlane::open_with_repository(&data_dir, &repository).is_err());
        };
        assert_tamper_rejected("candidate_visible_correct", "1", "0");
        assert_tamper_rejected("completed_trials", "4", "3");

        for suffix in ["", "-wal", "-shm"] {
            let _ignored = fs::remove_file(format!("{}{suffix}", events_path.display()));
        }
        let mut ledger = EventStore::open(&events_path).unwrap();
        let committing = recovered_history
            .iter()
            .find(|event| {
                event.event_type == "arena.job.committing"
                    && event.event_id == "arena-job:daemon-owned-pair:committing"
            })
            .expect("the paired evaluation has a committing phase");
        let cancellation_payload = String::from_utf8(committing.payload.clone())
            .unwrap()
            .replace(
                "\"state\":\"running\"",
                "\"state\":\"cancellation_requested\"",
            );
        assert!(cancellation_payload.contains("\"state\":\"cancellation_requested\""));
        let mut inserted_cancellation = false;
        for event in &recovered_history {
            if !inserted_cancellation
                && event.event_type == "evaluation.recorded"
                && event.event_id == "arena:evaluation:daemon-owned-pair:recorded"
            {
                ledger
                    .append(EventInput::new(
                        "arena-job:daemon-owned-pair:cancellation_requested",
                        "arena-job:daemon-owned-pair",
                        "arena.job.cancellation_requested",
                        "daemon-runtime",
                        event.timestamp_millis,
                        cancellation_payload.as_bytes(),
                    ))
                    .unwrap();
                inserted_cancellation = true;
            }
            ledger
                .append(EventInput::new(
                    event.event_id.clone(),
                    event.aggregate_id.clone(),
                    event.event_type.clone(),
                    event.actor.clone(),
                    event.timestamp_millis,
                    event.payload.clone(),
                ))
                .unwrap();
        }
        assert!(
            inserted_cancellation,
            "inserted canonical cancellation before the evaluation receipt"
        );
        drop(ledger);
        assert!(
            ControlPlane::open_with_repository(&data_dir, &repository).is_err(),
            "a canonical succeeded Arena terminal must not follow cancellation"
        );

        for suffix in ["", "-wal", "-shm"] {
            let _ignored = fs::remove_file(format!("{}{suffix}", events_path.display()));
        }
        let mut ledger = EventStore::open(&events_path).unwrap();
        for event in &recovered_history {
            let payload = if event.event_type == "arena.job.terminal"
                && event.event_id == "arena-job:daemon-owned-pair:terminal"
            {
                String::from_utf8(event.payload.clone())
                    .unwrap()
                    .replace("\"state\":\"succeeded\"", "\"state\":\"failed\"")
                    .replace("\"terminal\":\"succeeded\"", "\"terminal\":\"failed\"")
                    .into_bytes()
            } else {
                event.payload.clone()
            };
            ledger
                .append(EventInput::new(
                    event.event_id.clone(),
                    event.aggregate_id.clone(),
                    event.event_type.clone(),
                    event.actor.clone(),
                    event.timestamp_millis,
                    payload,
                ))
                .unwrap();
        }
        drop(ledger);
        assert!(
            ControlPlane::open_with_repository(&data_dir, &repository).is_err(),
            "a failed Arena terminal must not claim a committed evaluation record"
        );

        let mismatch_pair_id = "trial-mismatch-pair";
        let daemon_pair_prefix = format!(
            "paired-{}",
            &blake3::hash(mismatch_pair_id.as_bytes())
                .to_hex()
                .to_string()[..24]
        );
        let mismatched_trial_id = format!("result:{daemon_pair_prefix}-parent-0");
        for suffix in ["", "-wal", "-shm"] {
            let _ignored = fs::remove_file(format!("{}{suffix}", events_path.display()));
        }
        let mut ledger = EventStore::open(&events_path).unwrap();
        let mut altered_trial = false;
        for event in &recovered_history {
            if event.event_id == mismatched_trial_id {
                let mut receipt = RunResultReceipt::parse_from_event(event, &signer.verifier())
                    .expect("valid fixture trial receipt");
                receipt.genome_id = candidate.genome_id.clone();
                ledger
                    .append(signer.issue(receipt, event.timestamp_millis).unwrap())
                    .unwrap();
                altered_trial = true;
            } else {
                ledger
                    .append(EventInput::new(
                        event.event_id.clone(),
                        event.aggregate_id.clone(),
                        event.event_type.clone(),
                        event.actor.clone(),
                        event.timestamp_millis,
                        event.payload.clone(),
                    ))
                    .unwrap();
            }
        }
        assert!(altered_trial, "tampered one signed parent-trial claim");
        drop(ledger);
        let trial_mismatch = ControlPlane::open_with_repository(&data_dir, &repository)
            .err()
            .expect("replay must reject a signed trial bound to the wrong Genome");
        assert!(
            trial_mismatch
                .to_string()
                .contains("Arena run receipt differs from its admitted source"),
            "unexpected trial mismatch rejection: {trial_mismatch}"
        );
        for suffix in ["", "-wal", "-shm"] {
            let _ignored = fs::remove_file(format!("{}{suffix}", events_path.display()));
        }
        let mut ledger = EventStore::open(&events_path).unwrap();
        for event in &recovered_history {
            ledger
                .append(EventInput::new(
                    event.event_id.clone(),
                    event.aggregate_id.clone(),
                    event.event_type.clone(),
                    event.actor.clone(),
                    event.timestamp_millis,
                    event.payload.clone(),
                ))
                .unwrap();
        }
        drop(ledger);

        let admitted = recovered_history
            .iter()
            .find(|event| {
                event.event_type == "arena.job.admitted"
                    && event.event_id == "arena-job:daemon-owned-pair:admitted"
            })
            .expect("successful evaluation has an admitted Arena record");
        let canonical_record =
            |evaluation_id: &str, candidate_genome_id: &str, state: &str, phase: &str| {
                let record: serde_json::Value =
                    serde_json::from_slice(&admitted.payload).expect("decode admitted record");
                let parent_trial_count = record["parent_trial_count"]
                    .as_u64()
                    .expect("parent trial count");
                let parent_trial_count =
                    usize::try_from(parent_trial_count).expect("parent trial count fits usize");
                let total_trials =
                    usize::try_from(record["total_trials"].as_u64().expect("total trial count"))
                        .expect("total trial count fits usize");
                let prefix = format!(
                    "paired-{}",
                    &blake3::hash(evaluation_id.as_bytes()).to_hex().to_string()[..24]
                );
                let run_ids = (0..total_trials)
                    .map(|index| {
                        if index < parent_trial_count {
                            format!("{prefix}-parent-{index}")
                        } else {
                            format!("{prefix}-candidate-{}", index - parent_trial_count)
                        }
                    })
                    .collect::<Vec<_>>();
                let plan_commitment = serde_json::to_vec(&(
                    evaluation_id,
                    record["parent_genome_id"].as_str().unwrap(),
                    candidate_genome_id,
                    record["world_id"].as_str().unwrap(),
                    record["visible_manifest_id"].as_str().unwrap(),
                    record["sealed_manifest_id"].as_str().unwrap(),
                    record["source_revision"].as_str().unwrap(),
                    record["environment_id"].as_str().unwrap(),
                    &run_ids,
                ))
                .expect("encode Arena plan commitment");
                let mut payload = String::from_utf8(admitted.payload.clone())
                    .expect("Arena record is UTF-8 JSON");
                let replace_field = |payload: &mut String, field: &str, old: &str, new: &str| {
                    let old = format!("\"{field}\":{}", serde_json::to_string(old).unwrap());
                    let new = format!("\"{field}\":{}", serde_json::to_string(new).unwrap());
                    assert!(payload.contains(&old), "canonical record has {field}");
                    *payload = payload.replacen(&old, &new, 1);
                };
                replace_field(
                    &mut payload,
                    "evaluation_id",
                    record["evaluation_id"].as_str().unwrap(),
                    evaluation_id,
                );
                replace_field(
                    &mut payload,
                    "job_id",
                    record["job_id"].as_str().unwrap(),
                    evaluation_id,
                );
                replace_field(
                    &mut payload,
                    "candidate_genome_id",
                    record["candidate_genome_id"].as_str().unwrap(),
                    candidate_genome_id,
                );
                replace_field(
                    &mut payload,
                    "state",
                    record["state"].as_str().unwrap(),
                    state,
                );
                replace_field(
                    &mut payload,
                    "phase",
                    record["phase"].as_str().unwrap(),
                    phase,
                );
                let old_run_ids = serde_json::to_string(&record["ordered_trial_run_ids"]).unwrap();
                let new_run_ids = serde_json::to_string(&run_ids).unwrap();
                let old_run_ids_field = format!("\"ordered_trial_run_ids\":{old_run_ids}");
                let new_run_ids_field = format!("\"ordered_trial_run_ids\":{new_run_ids}");
                assert!(payload.contains(&old_run_ids_field));
                payload = payload.replacen(&old_run_ids_field, &new_run_ids_field, 1);
                let commitment = blake3::hash(&plan_commitment).to_hex().to_string();
                replace_field(
                    &mut payload,
                    "plan_commitment",
                    record["plan_commitment"].as_str().unwrap(),
                    &commitment,
                );
                payload.into_bytes()
            };
        let append_replay_case = |event_type: &str, evaluation_id: &str, payload: Vec<u8>| {
            for suffix in ["", "-wal", "-shm"] {
                let _ignored = fs::remove_file(format!("{}{suffix}", events_path.display()));
            }
            let mut ledger = EventStore::open(&events_path).unwrap();
            for event in &recovered_history {
                ledger
                    .append(EventInput::new(
                        event.event_id.clone(),
                        event.aggregate_id.clone(),
                        event.event_type.clone(),
                        event.actor.clone(),
                        event.timestamp_millis,
                        event.payload.clone(),
                    ))
                    .unwrap();
            }
            let suffix = event_type.strip_prefix("arena.job.").unwrap();
            ledger
                .append(EventInput::new(
                    format!("arena-job:{evaluation_id}:{suffix}"),
                    format!("arena-job:{evaluation_id}"),
                    event_type,
                    "daemon-runtime",
                    recovered_history.last().unwrap().timestamp_millis + 1,
                    payload,
                ))
                .unwrap();
            drop(ledger);
        };

        // Replay rejects a structurally valid plan whose candidate Genome was never registered.
        let missing_genome = format!("hephaestus:genome:{}", "f".repeat(64));
        let missing_id = "replay-unregistered-candidate";
        append_replay_case(
            "arena.job.admitted",
            missing_id,
            canonical_record(missing_id, &missing_genome, "admitted", "preparing"),
        );
        assert!(matches!(
            ControlPlane::open_with_repository(&data_dir, &repository),
            Err(ControlError::Projection(message))
                if message.contains("Arena candidate Genome is not registered")
        ));

        // A canonical running event cannot create a job without its admitted predecessor.
        let transition_id = "replay-running-without-admission";
        append_replay_case(
            "arena.job.running",
            transition_id,
            canonical_record(
                transition_id,
                &candidate.genome_id,
                "running",
                "parent_trials",
            ),
        );
        let transition_error = ControlPlane::open_with_repository(&data_dir, &repository)
            .err()
            .expect("replay rejects a running Arena record without admission");
        assert!(
            transition_error
                .to_string()
                .contains("Arena job lifecycle transition is invalid"),
            "unexpected transition rejection: {transition_error}"
        );
    }

    let binding = EvaluationBinding::new(
        world.id(),
        42,
        reference_worker_environment_id(),
        evaluator_id.as_str(),
        RunBudgetReceipt {
            wall_millis: 10_000,
            maximum_output_bytes: 1_048_576,
            maximum_cost_microusd: 0,
        },
    )
    .unwrap();
    let parent_plan = TrialPlan::new(parent_trials).unwrap();
    let candidate_plan = TrialPlan::new(candidate_trials).unwrap();
    let evaluator = IsolatedEvaluator::in_process_for_testing(evaluator_id.as_str()).unwrap();
    let recorded = evaluate_and_record(
        EvaluationStores::open(data_dir.join("events.sqlite3"), data_dir.join("blobs")).unwrap(),
        ReceiptContext {
            event_id: "arena:evaluation:daemon-paired:recorded".to_owned(),
            evaluation_id: "daemon-paired".to_owned(),
            caller_id: "control-e2e".to_owned(),
            timestamp_millis: 1_800_000_000_000,
        },
        &world,
        EvaluationInputs {
            binding: &binding,
            visible: &visible,
            sealed: &sealed,
            parent: &parent_plan,
            candidate: &candidate_plan,
            evaluator: &evaluator,
        },
    )
    .expect("consume exact daemon-authenticated results in Arena");
    assert_eq!(recorded.candidate_result().summary.world_id, world.id());
    assert_eq!(
        recorded.candidate_result().summary.parent_genome_id,
        parent.genome_id
    );
    assert_eq!(
        recorded.candidate_result().summary.candidate_genome_id,
        candidate.genome_id
    );
}

#[cfg(feature = "test-support")]
#[test]
#[allow(clippy::too_many_lines)]
fn arena_overall_deadline_stops_slow_trial_without_committing_evaluation() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create source repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"timeout fixture\n").unwrap();
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    fs::create_dir_all(&data_dir).unwrap();
    fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let producer_seed = [17_u8; 32];
    fs::write(data_dir.join("runtime-producer.key"), producer_seed).unwrap();
    fs::set_permissions(
        data_dir.join("runtime-producer.key"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let signer = RunResultSigner::from_seed(producer_seed);
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).unwrap();
    let visible = TrustedManifest::new(
        "timeout-visible-v1",
        Visibility::Visible,
        vec![TrustedTask::new("visible-task", "slow input", "SLOW INPUT").unwrap()],
    )
    .unwrap();
    let sealed = TrustedManifest::new(
        "timeout-sealed-v1",
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", "slow sealed", "SLOW SEALED").unwrap()],
    )
    .unwrap();
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).unwrap())
        .unwrap();
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).unwrap())
        .unwrap();
    let evaluator_id = artifacts
        .put(&fs::read(REFERENCE_EVALUATOR).unwrap())
        .unwrap();
    let verifier_id = artifacts
        .put(&signer.verifier().public_key_bytes())
        .unwrap();
    let world_source = format!(
        r#"{{"schema_version":1,"name":"deadline-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
        visible_id.as_str(),
        sealed_id.as_str(),
        evaluator_id.as_str(),
        verifier_id.as_str()
    );
    let world = compile_world(&world_source, SourceFormat::Json, &artifacts).unwrap();
    let world_artifact = artifacts.put(world.canonical_json()).unwrap();
    let world_record = WorldRecord {
        world_id: world.id().to_owned(),
        name: world.name().to_owned(),
        artifact_id: world_artifact.as_str().to_owned(),
    };
    EventStore::open(data_dir.join("events.sqlite3"))
        .unwrap()
        .append(EventInput::new(
            "deadline-arena-world",
            world.id(),
            "world.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&world_record).unwrap(),
        ))
        .unwrap();

    let worker = directory.path().join("slow-reference-worker");
    fs::write(
        &worker,
        "#!/bin/sh\n/bin/sleep 60 &\nchild=$!\nprintf '%s' \"$child\" > \"$HOME/arena-child.pid\"\nwait \"$child\"\n",
    )
    .unwrap();
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
    let daemon =
        Daemon::start_with_worker_and_arena_timeout(&data_dir, &repository, &worker, Some(2_000));
    let parent_path = directory.path().join("parent.md");
    let candidate_path = directory.path().join("candidate.md");
    let metadata = |name: &str, parents: &str| {
        format!(
            "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
        )
    };
    fs::write(&parent_path, metadata("deadline-parent", "[]")).unwrap();
    let ResponseData::Genome { genome: parent } = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            parent_path.to_str().unwrap(),
            "--world",
            world.id(),
        ],
    ))
    .data
    .unwrap() else {
        panic!("parent Genome registration failed");
    };
    fs::write(
        &candidate_path,
        metadata("deadline-candidate", &format!("[\"{}\"]", parent.genome_id)),
    )
    .unwrap();
    let ResponseData::Genome { genome: candidate } = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            candidate_path.to_str().unwrap(),
            "--world",
            world.id(),
        ],
    ))
    .data
    .unwrap() else {
        panic!("candidate Genome registration failed");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let accepted = Client::new(&data_dir)
        .request(Command::EvaluatePair {
            evaluation_id: "deadline-pair".to_owned(),
            parent_genome_id: parent.genome_id.clone(),
            candidate_genome_id: candidate.genome_id.clone(),
        })
        .unwrap();
    assert!(matches!(
        accepted.data,
        Some(ResponseData::ArenaJob { job }) if job.state == JobState::Running && job.evaluation.is_none()
    ));

    let child_file_deadline = Instant::now() + Duration::from_secs(5);
    let child_pid = loop {
        let pid_file = fs::read_dir(data_dir.join("sandboxes"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("execution/arena-child.pid"))
            .find(|path| path.is_file());
        if let Some(path) = pid_file {
            if let Some(pid) = fs::read_to_string(path)
                .ok()
                .and_then(|contents| contents.parse::<u32>().ok())
            {
                break pid;
            }
        }
        assert!(
            Instant::now() < child_file_deadline,
            "slow Arena trial did not start"
        );
        thread::sleep(Duration::from_millis(5));
    };
    let terminal_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = Client::new(&data_dir)
            .request(Command::JobStatus {
                job_id: "deadline-pair".to_owned(),
            })
            .unwrap();
        let Some(ResponseData::ArenaJob { job }) = status.data else {
            panic!("Arena job status missing");
        };
        if job.state == JobState::Failed {
            assert_eq!(job.phase, ArenaJobPhase::Terminal);
            assert!(job.evaluation.is_none());
            break;
        }
        assert!(
            Instant::now() < terminal_deadline,
            "overall Arena deadline was not enforced"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let probe = ProcessCommand::new("/bin/kill")
        .args(["-0", &child_pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("probe worker descendant");
    assert!(!probe.success(), "deadline left a worker descendant alive");
    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .unwrap()
        .replay_verified()
        .unwrap();
    assert!(history.iter().any(|event| {
        event.event_type == "arena.job.admitted"
            && String::from_utf8_lossy(&event.payload)
                .contains("\"overall_budget\":{\"wall_millis\":2000")
    }));
    assert!(
        !history
            .iter()
            .any(|event| event.event_type == "evaluation.recorded")
    );
    assert!(cli(&data_dir, &["replay"]).status.success());

    let cancelled = Client::new(&data_dir)
        .request(Command::EvaluatePair {
            evaluation_id: "cancelled-pair".to_owned(),
            parent_genome_id: parent.genome_id.clone(),
            candidate_genome_id: candidate.genome_id.clone(),
        })
        .unwrap();
    assert!(matches!(
        cancelled.data,
        Some(ResponseData::ArenaJob { job }) if job.state == JobState::Running
    ));
    let same_pair_retry = Client::new(&data_dir)
        .request(Command::EvaluatePair {
            evaluation_id: "cancelled-pair".to_owned(),
            parent_genome_id: parent.genome_id.clone(),
            candidate_genome_id: candidate.genome_id.clone(),
        })
        .unwrap();
    assert!(matches!(
        same_pair_retry.data,
        Some(ResponseData::ArenaJob { job }) if job.state == JobState::Running
    ));
    let conflicting_pair = Client::new(&data_dir)
        .request(Command::EvaluatePair {
            evaluation_id: "cancelled-pair".to_owned(),
            parent_genome_id: candidate.genome_id.clone(),
            candidate_genome_id: parent.genome_id.clone(),
        })
        .unwrap();
    assert!(conflicting_pair.error.is_some());
    let second_pair = Client::new(&data_dir)
        .request(Command::EvaluatePair {
            evaluation_id: "second-pair".to_owned(),
            parent_genome_id: parent.genome_id.clone(),
            candidate_genome_id: candidate.genome_id.clone(),
        })
        .unwrap();
    assert_eq!(
        second_pair.error.map(|error| error.code),
        Some(ApiErrorCode::Busy)
    );
    let direct_while_paired = Client::new(&data_dir)
        .request(Command::RunSubmit {
            job_id: "busy-direct".to_owned(),
            genome_id: parent.genome_id.clone(),
        })
        .unwrap();
    assert_eq!(
        direct_while_paired.error.map(|error| error.code),
        Some(ApiErrorCode::Busy)
    );
    let cancellation = Client::new(&data_dir)
        .request(Command::JobKill {
            job_id: "cancelled-pair".to_owned(),
        })
        .unwrap();
    assert!(matches!(
        cancellation.data,
        Some(ResponseData::ArenaJob { job }) if job.state == JobState::CancellationRequested
    ));
    let cancellation_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = Client::new(&data_dir)
            .request(Command::JobStatus {
                job_id: "cancelled-pair".to_owned(),
            })
            .unwrap();
        let Some(ResponseData::ArenaJob { job }) = status.data else {
            panic!("cancelled Arena job status missing");
        };
        if job.state == JobState::Interrupted {
            assert_eq!(job.phase, ArenaJobPhase::Terminal);
            assert!(job.evaluation.is_none());
            break;
        }
        assert!(
            Instant::now() < cancellation_deadline,
            "Arena cancellation did not reach a terminal state"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let repeated_kill = Client::new(&data_dir)
        .request(Command::JobKill {
            job_id: "cancelled-pair".to_owned(),
        })
        .unwrap();
    assert!(matches!(
        repeated_kill.data,
        Some(ResponseData::ArenaJob { job }) if job.state == JobState::Interrupted
    ));

    let stopping = Client::new(&data_dir)
        .request(Command::EvaluatePair {
            evaluation_id: "stop-pair".to_owned(),
            parent_genome_id: parent.genome_id,
            candidate_genome_id: candidate.genome_id,
        })
        .unwrap();
    assert!(matches!(
        stopping.data,
        Some(ResponseData::ArenaJob { job }) if job.state == JobState::Running
    ));
    let deferred_stop = Client::new(&data_dir).request(Command::DaemonStop).unwrap();
    assert_eq!(
        deferred_stop.error.map(|error| error.code),
        Some(ApiErrorCode::Busy),
        "daemon shutdown should first cancel the active Arena job"
    );
    let stop_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = Client::new(&data_dir)
            .request(Command::JobStatus {
                job_id: "stop-pair".to_owned(),
            })
            .unwrap();
        let Some(ResponseData::ArenaJob { job }) = status.data else {
            panic!("Arena job status missing after deferred daemon stop");
        };
        if job.state == JobState::Interrupted {
            assert_eq!(job.phase, ArenaJobPhase::Terminal);
            assert!(job.evaluation.is_none());
            break;
        }
        assert!(
            Instant::now() < stop_deadline,
            "deferred daemon stop did not cancel the Arena job"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(cli(&data_dir, &["replay"]).status.success());
    daemon.stop();
}

fn evaluation_run(data_dir: &Path, genome_id: &str, task_id: &str, input: &str) -> String {
    let output = cli(
        data_dir,
        &[
            "evaluate",
            genome_id,
            "--task-id",
            task_id,
            "--input",
            input,
            "--seed",
            "42",
        ],
    );
    let response = response(&output);
    match response.data.unwrap() {
        ResponseData::Run { run_id, .. } => format!("result:{run_id}"),
        other => panic!("unexpected evaluation response: {other:?}"),
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn reference_runtime_runs_through_real_daemon_and_replays_terminal_evidence() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create source repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("README.md"), b"reference inventory\n").expect("write fixture");
    fs::create_dir(repository.join("src")).expect("create source directory");
    fs::write(
        repository.join("src/lib.rs"),
        b"pub fn answer() -> u8 { 42 }\n",
    )
    .expect("write source fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let source_revision = git_stdout(&repository, &["rev-parse", "HEAD"]);
    let (world, genome) = seed_compiled_genome(&data_dir);

    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    let frozen = cli(&data_dir, &["run", &genome.genome_id]);
    assert!(!frozen.status.success());
    assert_eq!(
        serde_json::from_slice::<ApiResponse>(&frozen.stdout)
            .expect("decode frozen response")
            .error
            .expect("frozen error")
            .code,
        ApiErrorCode::InvalidRequest
    );
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let world_cost_violation = cli(
        &data_dir,
        &[
            "evaluate",
            &genome.genome_id,
            "--task-id",
            "world-law-cost",
            "--input",
            "cost law input",
            "--wall-millis",
            "10000",
            "--maximum-output-bytes",
            "1048576",
            "--maximum-cost-microusd",
            "1",
        ],
    );
    assert!(!world_cost_violation.status.success());
    assert_eq!(
        serde_json::from_slice::<ApiResponse>(&world_cost_violation.stdout)
            .expect("decode World cost Law response")
            .error
            .expect("World cost Law error")
            .code,
        ApiErrorCode::InvalidRequest
    );
    let pre_run_history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open pre-run ledger")
        .replay_verified()
        .expect("verify pre-run history");
    assert!(pre_run_history.iter().all(|event| {
        event.event_type != "trace.recorded" && event.event_type != "run.result_recorded"
    }));
    assert!(!data_dir.join("sandboxes").exists());

    let run = response(&cli(&data_dir, &["run", &genome.genome_id]));
    let (run_id, stdout_artifact_id, stderr_artifact_id, trace_artifact_ids, latency_millis) =
        match run.data.expect("run response") {
            ResponseData::Run {
                run_id,
                genome_id,
                world_id,
                source_revision: actual_source_revision,
                completion_reason: RunCompletionReason::Success,
                latency_millis,
                actual_cost_microusd: 0,
                stdout_artifact_id,
                stderr_artifact_id,
                trace_artifact_ids,
            } => {
                assert_eq!(genome_id, genome.genome_id);
                assert_eq!(world_id, world.world_id);
                assert_eq!(actual_source_revision, source_revision);
                (
                    run_id,
                    stdout_artifact_id,
                    stderr_artifact_id,
                    trace_artifact_ids,
                    latency_millis,
                )
            }
            other => panic!("unexpected run response: {other:?}"),
        };
    assert!(run_id.starts_with("reference-"));
    assert!(latency_millis <= 10_000);
    assert_eq!(trace_artifact_ids.len(), 6);

    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open artifacts");
    let stdout = artifacts
        .get(&hephaestus_ledger::ArtifactId::parse(stdout_artifact_id.clone()).expect("stdout ID"))
        .expect("read stdout artifact");
    let inventory: serde_json::Value = serde_json::from_slice(&stdout).expect("decode inventory");
    assert_eq!(inventory["schema_version"], 1);
    assert_eq!(inventory["genome_id"], genome.genome_id);
    assert_eq!(inventory["world_id"], world.world_id);
    assert_eq!(inventory["source_revision"], source_revision);
    assert!(inventory["checkpoint"].is_null());
    let paths: Vec<_> = inventory["files"]
        .as_array()
        .expect("inventory files")
        .iter()
        .map(|file| file["path"].as_str().expect("file path"))
        .collect();
    assert_eq!(paths, ["README.md", "src/lib.rs"]);
    assert_eq!(
        artifacts
            .get(
                &hephaestus_ledger::ArtifactId::parse(stderr_artifact_id.clone())
                    .expect("stderr ID")
            )
            .expect("read stderr artifact"),
        b""
    );
    let operator_token = fs::read_to_string(data_dir.join("operator.token")).expect("read token");
    for id in &trace_artifact_ids {
        let bytes = artifacts
            .get(&hephaestus_ledger::ArtifactId::parse(id.clone()).expect("trace ID"))
            .expect("read trace artifact");
        assert!(!String::from_utf8_lossy(&bytes).contains(&operator_token));
    }
    let started_bytes = artifacts
        .get(
            &hephaestus_ledger::ArtifactId::parse(trace_artifact_ids[0].clone())
                .expect("started trace ID"),
        )
        .expect("read started trace");
    let started: serde_json::Value =
        serde_json::from_slice(&started_bytes).expect("decode started trace");
    assert_eq!(started["kind"], "lifecycle_started");
    assert_eq!(started["fields"]["workspace_write"], "false");
    assert_eq!(started["fields"]["network"], "false");
    assert_eq!(started["fields"]["source_revision"], source_revision);
    assert!(matches!(
        response(&cli(&data_dir, &["status"])).data,
        Some(ResponseData::Status { active_runs: 0, .. })
    ));
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { active_runs: 0, .. })
    ));
    assert_eq!(
        fs::read_dir(data_dir.join("sandboxes"))
            .expect("read sandbox root")
            .count(),
        0
    );
    daemon.stop();

    let ledger = EventStore::open(data_dir.join("events.sqlite3")).expect("reopen ledger");
    let history = ledger.replay_verified().expect("verify history");
    let run_events: Vec<_> = history
        .iter()
        .filter(|event| event.aggregate_id == format!("run:{run_id}"))
        .collect();
    assert_eq!(run_events.len(), 7);
    assert_eq!(
        run_events.first().expect("first trace").event_type,
        "trace.recorded"
    );
    assert_eq!(
        run_events.last().expect("result receipt").event_type,
        "run.result_recorded"
    );
    let producer_key_path = data_dir.join("runtime-producer.key");
    assert_eq!(
        fs::metadata(&producer_key_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let producer_seed: [u8; 32] = fs::read(producer_key_path).unwrap().try_into().unwrap();
    let verifier = RunResultSigner::from_seed(producer_seed).verifier();
    let result_receipt =
        RunResultReceipt::parse_from_event(run_events.last().expect("result receipt"), &verifier)
            .expect("authenticate result receipt");
    assert_eq!(result_receipt.schema_version, 2);
    assert_eq!(result_receipt.source_revision, source_revision);
    assert_eq!(result_receipt.task_id, "repository-inventory-v1");
    assert_eq!(result_receipt.seed, 0);
    assert_eq!(result_receipt.environment_id, runtime_environment_id());
    assert_eq!(result_receipt.budget.wall_millis, 10_000);
    assert_eq!(result_receipt.budget.maximum_output_bytes, 1_048_576);
    assert_eq!(result_receipt.budget.maximum_cost_microusd, 0);
    assert_eq!(
        result_receipt.input_commitment,
        blake3::hash(
            b"Inventory the isolated repository without modifying it or using the network."
        )
        .to_hex()
        .to_string()
    );
    let terminal_artifact = artifacts
        .get(
            &hephaestus_ledger::ArtifactId::parse(
                trace_artifact_ids.last().expect("terminal trace").clone(),
            )
            .expect("terminal artifact ID"),
        )
        .expect("terminal artifact");
    let terminal: serde_json::Value =
        serde_json::from_slice(&terminal_artifact).expect("decode terminal artifact");
    assert_eq!(terminal["kind"], "lifecycle_completed");
    assert_eq!(terminal["fields"]["completion_reason"], "success");
    assert_eq!(terminal["fields"]["actual_cost_microusd"], "0");
    assert_eq!(
        terminal["fields"]["latency_millis"],
        latency_millis.to_string()
    );

    let restarted = Daemon::start_with_repository(&data_dir, &repository);
    assert!(matches!(
        response(&cli(&data_dir, &["status"])).data,
        Some(ResponseData::Status { active_runs: 0, .. })
    ));
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { active_runs: 0, .. })
    ));
    restarted.stop();

    fs::write(data_dir.join("runtime-producer.key"), [3_u8; 32]).expect("replace producer key");
    assert!(ControlPlane::open_with_repository(&data_dir, &repository).is_err());
    fs::remove_file(data_dir.join("runtime-producer.key")).expect("remove producer key");
    assert!(ControlPlane::open_with_repository(&data_dir, &repository).is_err());
    assert!(!data_dir.join("runtime-producer.key").exists());
}

#[test]
#[allow(clippy::too_many_lines)]
fn synchronous_reference_run_uses_private_worker_snapshot_after_public_replacement() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create source repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("README.md"), b"pinned worker fixture\n")
        .expect("write repository fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let (world, _) = seed_compiled_genome(&data_dir);
    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    let genome_source = directory.path().join("reference.md");
    fs::write(
        &genome_source,
        "---\nschema_version: 1\nname: pinned-reference\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write reference Genome");
    let ResponseData::Genome { genome } = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_source.to_str().expect("UTF-8 Genome path"),
            "--world",
            &world.world_id,
        ],
    ))
    .data
    .expect("registered Genome response") else {
        panic!("unexpected Genome registration response");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());

    let ready = directory.path().join("worktree-hook-ready");
    let release = directory.path().join("worktree-hook-release");
    let hook = repository.join(".git/hooks/post-checkout");
    fs::write(
        &hook,
        "#!/bin/sh\nhook_root=$(cd \"$(dirname \"$0\")/../../..\" && pwd)\nprintf ready > \"$hook_root/worktree-hook-ready\"\nwhile [ ! -f \"$hook_root/worktree-hook-release\" ]; do sleep 0.01; done\n",
    )
    .expect("write bounded worktree hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).expect("enable test hook");

    let deployed_worker = data_dir.join("reference-worker");
    let worker_bytes = fs::read(&deployed_worker).expect("read deployed worker");
    let incompatible_worker = fs::read("/bin/cat").expect("read alternate executable");
    let mut request = ProcessCommand::new(CLI);
    request
        .arg("--data-dir")
        .arg(&data_dir)
        .arg("--json")
        .args(["run", &genome.genome_id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut request = request.spawn().expect("start bounded reference request");
    let request_deadline = Instant::now() + Duration::from_secs(15);
    let mut hook_started = false;
    let mut private_snapshot_matches = false;
    let mut request_status = None;
    while Instant::now() < request_deadline {
        if !hook_started && ready.exists() {
            hook_started = true;
            private_snapshot_matches = fs::read_dir(&data_dir)
                .expect("read worker snapshot directory")
                .flatten()
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("reference-worker-")
                })
                .and_then(|entry| fs::read(entry.path().join("worker")).ok())
                .is_some_and(|bytes| bytes == worker_bytes);
            fs::write(&deployed_worker, &incompatible_worker)
                .expect("replace public worker while worktree hook is paused");
            fs::set_permissions(&deployed_worker, fs::Permissions::from_mode(0o700))
                .expect("keep replacement executable");
            fs::write(&release, b"continue").expect("release worktree hook");
        }
        if let Some(status) = request.try_wait().expect("poll reference request") {
            request_status = Some(status);
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    if !hook_started {
        fs::write(&release, b"continue").expect("release worktree hook");
    }
    let request_timed_out = request_status.is_none();
    let run = if request_timed_out {
        request.kill().expect("stop hung reference request");
        let _status = request.wait().expect("reap timed-out reference request");
        None
    } else {
        Some(
            request
                .wait_with_output()
                .expect("collect reference request"),
        )
    };
    fs::write(&deployed_worker, &worker_bytes).expect("restore deployed worker");
    fs::set_permissions(&deployed_worker, fs::Permissions::from_mode(0o700))
        .expect("restore worker permissions");
    if request_timed_out {
        daemon.crash();
        panic!("reference request exceeded its 15 second test bound");
    }
    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open event ledger")
        .replay_verified();
    let replay = cli(&data_dir, &["replay"]);
    daemon.stop();
    assert!(
        hook_started,
        "worktree hook did not pause execution after pinning"
    );
    assert!(
        private_snapshot_matches,
        "paused worktree hook did not follow creation of the original private snapshot"
    );
    assert!(
        request_status.expect("reference request status").success(),
        "reference request failed: {}",
        String::from_utf8_lossy(&run.as_ref().expect("reference request output").stderr)
    );
    let run = response(&run.expect("reference request output"));
    let Some(ResponseData::Run {
        completion_reason: RunCompletionReason::Success,
        stdout_artifact_id,
        ..
    }) = run.data
    else {
        panic!("synchronous run did not succeed with the private worker snapshot");
    };
    let stdout = ArtifactStore::open(data_dir.join("blobs"))
        .expect("open artifact store")
        .get(&ArtifactId::parse(stdout_artifact_id).expect("parse stdout artifact ID"))
        .expect("read reference output");
    assert_eq!(
        stdout,
        b"Inventory the isolated repository without modifying it or using the network."
    );
    let history = history.expect("verify run history");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_type == "run.result_recorded")
            .count(),
        1
    );
    assert!(matches!(
        response(&replay).data,
        Some(ResponseData::Replay { .. })
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn async_job_status_and_cancellation_remain_responsive_and_confirm_process_death() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"async fixture\n").expect("write fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let (world, _) = seed_compiled_genome(&data_dir);
    let worker = directory.path().join("slow-worker");
    fs::write(
        &worker,
        "#!/bin/sh\n/bin/sleep 60 &\nchild=$!\nprintf '%s' \"$child\" > \"$HOME/slow-child.pid\"\nwait \"$child\"\n",
    )
    .expect("write slow worker");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let daemon = Daemon::start_with_worker(&data_dir, &repository, &worker);
    let genome_path = directory.path().join("agent.md");
    fs::write(
        &genome_path,
        "---\nschema_version: 1\nname: async-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write Genome");
    let registered = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_path.to_str().expect("UTF-8 path"),
            "--world",
            &world.world_id,
        ],
    ));
    let ResponseData::Genome { genome } = registered.data.expect("registered Genome") else {
        panic!("expected registered Genome");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let submitted = response(&cli(&data_dir, &["submit", "slow-job", &genome.genome_id]));
    assert!(matches!(submitted.data, Some(ResponseData::Job { .. })));

    let child_file_deadline = Instant::now() + Duration::from_secs(5);
    let child_pid = loop {
        let pid_file = fs::read_dir(data_dir.join("sandboxes"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("execution/slow-child.pid"))
            .find(|path| path.is_file());
        if let Some(path) = pid_file {
            if let Some(pid) = fs::read_to_string(path)
                .ok()
                .and_then(|contents| contents.parse::<u32>().ok())
            {
                break pid;
            }
        }
        assert!(
            Instant::now() < child_file_deadline,
            "slow worker did not start"
        );
        thread::sleep(Duration::from_millis(5));
    };

    let mut partial_client =
        UnixStream::connect(data_dir.join("control.sock")).expect("connect partial client");
    partial_client
        .write_all(b"{\"version\":")
        .expect("write partial frame");
    let status_started = Instant::now();
    assert!(matches!(
        response(&cli(&data_dir, &["status"])).data,
        Some(ResponseData::Status { .. })
    ));
    assert!(status_started.elapsed() < Duration::from_millis(500));
    assert_eq!(
        error_body(&cli(&data_dir, &["replay"])).code,
        hephaestus_control::ApiErrorCode::Busy,
        "replay must not monopolize the canonical writer during active work"
    );
    let freeze_started = Instant::now();
    assert!(cli(&data_dir, &["freeze"]).status.success());
    assert!(freeze_started.elapsed() < Duration::from_millis(500));
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    drop(partial_client);
    let second = cli(&data_dir, &["submit", "second-job", &genome.genome_id]);
    assert!(
        !second.status.success(),
        "second job must be rejected while busy"
    );
    if std::env::var_os("HEPHAESTUS_TUI_PTY_E2E").is_some() {
        let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/hephaestus-tui");
        let caller_directory = directory.path();
        let other_data_dir = caller_directory.join("other-daemon");
        let other_daemon = Daemon::start_with_repository(&other_data_dir, &repository);
        let before_events = match response(&cli(&data_dir, &["status"])).data {
            Some(ResponseData::Status { event_count, .. }) => event_count,
            other => panic!("expected status before TUI launch, got {other:?}"),
        };
        let output = ProcessCommand::new("python3")
            .arg(app.join("scripts/pty_smoke.py"))
            .arg(&app)
            .arg(&data_dir)
            .arg("slow-job")
            .arg(CLI)
            .arg(caller_directory)
            .arg("data")
            .arg(&other_data_dir)
            .output()
            .expect("run Ink TUI pseudo-terminal test");
        let other_status = response(&cli(&other_data_dir, &["status"]));
        other_daemon.stop();
        assert!(
            output.status.success(),
            "Ink TUI pseudo-terminal test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("Daemon confirmed slow-job terminal"),
            "PTY test did not report daemon confirmation"
        );
        assert!(
            matches!(
                other_status.data,
                Some(ResponseData::Status { active_runs: 0, .. })
            ),
            "the selected relative data directory must leave the default daemon untouched"
        );
        let after_events = match response(&cli(&data_dir, &["status"])).data {
            Some(ResponseData::Status { event_count, .. }) => event_count,
            other => panic!("expected status after TUI exit, got {other:?}"),
        };
        assert!(
            after_events.saturating_sub(before_events) <= 15,
            "polling must be bounded while the TUI is open; observed {} events",
            after_events.saturating_sub(before_events)
        );
    } else {
        let kill = response(&cli(&data_dir, &["kill", "--all"]));
        let ResponseData::Acknowledged { killed_runs, .. } =
            kill.data.expect("kill acknowledgement")
        else {
            panic!("expected kill acknowledgement");
        };
        assert_eq!(killed_runs, 0, "a signal is not terminal confirmation");
        let named_kill = response(&cli(&data_dir, &["job", "kill", "slow-job"]));
        assert!(matches!(
            named_kill.data,
            Some(ResponseData::Job { job, .. })
                if matches!(
                    (job.state, job.terminal),
                    (JobState::CancellationRequested, None)
                        | (JobState::Interrupted, Some(JobTerminal::Cancelled))
                )
        ));
        let repeated_named_kill = response(&cli(&data_dir, &["job", "kill", "slow-job"]));
        assert!(matches!(
            repeated_named_kill.data,
            Some(ResponseData::Job { job, .. })
                if matches!(
                    (job.state, job.terminal),
                    (JobState::CancellationRequested, None)
                        | (JobState::Interrupted, Some(JobTerminal::Cancelled))
                )
        ));
        let requested = match response(&cli(&data_dir, &["job", "status", "slow-job"])).data {
            Some(ResponseData::Job { job, .. }) => job,
            other => panic!("expected cancellation state, got {other:?}"),
        };
        assert!(matches!(
            (requested.state, requested.terminal),
            (JobState::CancellationRequested, None)
                | (JobState::Interrupted, Some(JobTerminal::Cancelled))
        ));
    }
    let terminal_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let status = response(&cli(&data_dir, &["job", "status", "slow-job"]));
        let ResponseData::Job { job, progress } = status.data.expect("job status") else {
            panic!("expected job response");
        };
        assert!(progress.trace_events > 0);
        if job.state == JobState::Interrupted {
            assert_eq!(job.terminal, Some(JobTerminal::Cancelled));
            break;
        }
        assert!(
            Instant::now() < terminal_deadline,
            "worker termination was not confirmed"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let probe = ProcessCommand::new("/bin/kill")
        .args(["-0", &child_pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("probe child process");
    assert!(
        !probe.success(),
        "cancelled job left a worker descendant alive"
    );
    assert!(cli(&data_dir, &["freeze"]).status.success());
    daemon.stop();
    remove_job_terminal_before_restart(&data_dir, "slow-job");
    let restarted = Daemon::start_with_repository(&data_dir, &repository);
    assert!(matches!(
        response(&cli(&data_dir, &["status"])).data,
        Some(ResponseData::Status { frozen: true, .. })
    ));
    assert!(matches!(
        response(&cli(&data_dir, &["job", "status", "slow-job"])).data,
        Some(ResponseData::Job { job, .. }) if job.state == JobState::Interrupted
    ));
    assert!(matches!(
        response(&cli(&data_dir, &["job", "kill", "slow-job"])).data,
        Some(ResponseData::Job { job, .. }) if job.state == JobState::Interrupted
    ));
    restarted.stop();
}

#[test]
#[allow(clippy::too_many_lines)]
fn async_direct_reference_job_persists_signed_output_and_replays_success() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let (world, _) = seed_compiled_genome(&data_dir);
    let daemon = Daemon::start(&data_dir);
    let genome_path = directory.path().join("async-agent.md");
    fs::write(
        &genome_path,
        "---\nschema_version: 1\nname: async-success-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"ascii_uppercase\"}\n```\n",
    )
    .expect("write Genome");
    let registered = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_path.to_str().expect("UTF-8 path"),
            "--world",
            &world.world_id,
        ],
    ));
    let Some(ResponseData::Genome { genome }) = registered.data else {
        panic!("expected registered Genome");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());

    let accepted = response(&cli(
        &data_dir,
        &["submit", "success-job", &genome.genome_id],
    ));
    let Some(ResponseData::Job { job, .. }) = accepted.data else {
        panic!("expected admitted job");
    };
    let retry = response(&cli(
        &data_dir,
        &["submit", "success-job", &genome.genome_id],
    ));
    let Some(ResponseData::Job { job: retried, .. }) = retry.data else {
        panic!("expected idempotent job retry");
    };
    assert_eq!(job.run_id, retried.run_id);

    let deadline = Instant::now() + Duration::from_secs(10);
    let (terminal, progress) = loop {
        let response = response(&cli(&data_dir, &["job", "status", "success-job"]));
        let Some(ResponseData::Job { job, progress }) = response.data else {
            panic!("expected job status");
        };
        if matches!(
            job.state,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        ) {
            break (job, progress);
        }
        assert!(Instant::now() < deadline, "async job did not finish");
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(terminal.state, JobState::Succeeded);
    assert_eq!(terminal.terminal, Some(JobTerminal::Succeeded));
    assert!(progress.trace_events > 0);
    assert!(progress.last_event_sequence.is_some());

    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open canonical ledger")
        .replay_verified()
        .expect("verify canonical ledger");
    let result_event = history
        .iter()
        .find(|event| event.event_type == "run.result_recorded")
        .expect("signed result event");
    let seed: [u8; 32] = fs::read(data_dir.join("runtime-producer.key"))
        .expect("read producer key for verifier")
        .try_into()
        .expect("producer key width");
    let receipt = RunResultSigner::from_seed(seed)
        .verifier()
        .verify_event(result_event)
        .expect("authenticate signed result");
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open CAS");
    let stdout = artifacts
        .get(&ArtifactId::parse(receipt.stdout_artifact_id).expect("stdout artifact ID"))
        .expect("read signed stdout artifact");
    assert_eq!(
        stdout,
        b"INVENTORY THE ISOLATED REPOSITORY WITHOUT MODIFYING IT OR USING THE NETWORK."
    );
    daemon.stop();

    // Rebuild the authenticated ledger without its terminal job record. This
    // models a crash after the signed run result and completion trace committed
    // but before the job terminal event was appended.
    remove_job_terminal_before_restart(&data_dir, "success-job");

    let restarted = Daemon::start(&data_dir);
    let replay = response(&cli(&data_dir, &["replay"]));
    assert!(matches!(replay.data, Some(ResponseData::Replay { .. })));
    let status = response(&cli(&data_dir, &["job", "status", "success-job"]));
    assert!(matches!(
        status.data,
        Some(ResponseData::Job {
            job: JobRecord {
                state: JobState::Succeeded,
                terminal: Some(JobTerminal::Succeeded),
                ..
            },
            progress: JobProgress {
                trace_events: 1..,
                ..
            }
        })
    ));
    restarted.stop();

    // A signed success cannot be recovered if its lifecycle completion evidence
    // is absent, even when the receipt artifact and signature remain valid.
    remove_job_terminal_and_completion_trace(&data_dir, "success-job");
    assert!(ControlPlane::open(&data_dir).is_err());
}

#[test]
#[allow(clippy::too_many_lines)]
fn direct_job_terminal_sqlite_rejection_recovers_signed_success_after_restart() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let (world, _) = seed_compiled_genome(&data_dir);
    let mut daemon = Daemon::start(&data_dir);
    let genome_path = directory.path().join("sqlite-rejected-agent.md");
    fs::write(
        &genome_path,
        "---\nschema_version: 1\nname: sqlite-rejected-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write reference Genome");
    let registered = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_path.to_str().expect("UTF-8 path"),
            "--world",
            &world.world_id,
        ],
    ));
    let Some(ResponseData::Genome { genome }) = registered.data else {
        panic!("expected registered Genome");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());

    let database = data_dir.join("events.sqlite3");
    let connection = Connection::open(&database).expect("open trigger connection");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_direct_success_terminal
             BEFORE INSERT ON events
             WHEN NEW.event_type = 'job.terminal'
              AND NEW.aggregate_id = 'job:sqlite-rejected-success'
             BEGIN
               SELECT RAISE(FAIL, 'fixture rejects direct job terminal');
             END;",
        )
        .expect("install direct terminal write rejection");

    let accepted = response(&cli(
        &data_dir,
        &["submit", "sqlite-rejected-success", &genome.genome_id],
    ));
    assert!(matches!(accepted.data, Some(ResponseData::Job { .. })));

    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let daemon_status = loop {
        if let Some(status) = daemon.child.try_wait().expect("inspect daemon") {
            break status;
        }
        assert!(
            Instant::now() < exit_deadline,
            "SQLite rejected terminal write did not fail the daemon service loop"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert!(
        !daemon_status.success(),
        "terminal append failure should stop the writer before it serves a false status"
    );

    let history = EventStore::open(&database)
        .expect("open history after failed terminal write")
        .replay_verified()
        .expect("verify durable pre-terminal history");
    let run_event = history
        .iter()
        .find(|event| event.event_type == "run.result_recorded")
        .expect("signed successful run result committed before the terminal write");
    let producer_seed: [u8; 32] = fs::read(data_dir.join("runtime-producer.key"))
        .expect("read runtime producer seed")
        .try_into()
        .expect("producer seed width");
    let receipt = RunResultSigner::from_seed(producer_seed)
        .verifier()
        .verify_event(run_event)
        .expect("verify signed success result");
    assert_eq!(receipt.completion_reason, RunCompletionReason::Success);
    assert!(history.iter().any(|event| {
        event.event_type == "trace.recorded"
            && serde_json::from_slice::<TraceReceipt>(&event.payload).is_ok_and(|trace| {
                trace.kind == TraceKind::LifecycleCompleted
                    && trace.provenance.run_id() == receipt.run_id
            })
    }));
    assert!(history.iter().any(|event| {
        event.event_type == "job.running" && event.aggregate_id == "job:sqlite-rejected-success"
    }));
    assert!(!history.iter().any(|event| {
        event.event_type == "job.terminal" && event.aggregate_id == "job:sqlite-rejected-success"
    }));

    connection
        .execute_batch("DROP TRIGGER reject_direct_success_terminal;")
        .expect("remove fixture write rejection");
    drop(connection);

    let restarted = Daemon::start(&data_dir);
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    assert!(matches!(
        response(&cli(
            &data_dir,
            &["job", "status", "sqlite-rejected-success"]
        ))
        .data,
        Some(ResponseData::Job {
            job: JobRecord {
                state: JobState::Succeeded,
                terminal: Some(JobTerminal::Succeeded),
                ..
            },
            progress: JobProgress {
                trace_events: 1..,
                ..
            }
        })
    ));
    restarted.stop();

    let second_restart = Daemon::start(&data_dir);
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    assert!(matches!(
        response(&cli(
            &data_dir,
            &["job", "status", "sqlite-rejected-success"]
        ))
        .data,
        Some(ResponseData::Job {
            job: JobRecord {
                state: JobState::Succeeded,
                terminal: Some(JobTerminal::Succeeded),
                ..
            },
            ..
        })
    ));
    let history = EventStore::open(&database)
        .expect("open stable success history")
        .replay_verified()
        .expect("verify stable success history");
    assert_eq!(
        history
            .iter()
            .filter(|event| {
                event.event_type == "job.terminal"
                    && event.aggregate_id == "job:sqlite-rejected-success"
            })
            .count(),
        1,
        "recovery should persist exactly one terminal across restarts"
    );
    second_restart.stop();
}

fn remove_job_terminal_before_restart(data_dir: &Path, job_id: &str) {
    rebuild_ledger_without(data_dir, |event| {
        event.event_id == format!("job:{job_id}:terminal")
    });
}

fn remove_job_terminal_and_completion_trace(data_dir: &Path, job_id: &str) {
    let digest = blake3::hash(job_id.as_bytes()).to_hex().to_string();
    let run_id = format!("async-{}", &digest[..32]);
    rebuild_ledger_without(data_dir, |event| {
        if event.event_id == format!("job:{job_id}:terminal") {
            return true;
        }
        event.event_type == "trace.recorded"
            && serde_json::from_slice::<TraceReceipt>(&event.payload).is_ok_and(|receipt| {
                receipt.kind == TraceKind::LifecycleCompleted
                    && receipt.provenance.run_id() == run_id
            })
    });
}

fn rebuild_ledger_without(
    data_dir: &Path,
    should_remove: impl Fn(&hephaestus_ledger::StoredEvent) -> bool,
) {
    let ledger_path = data_dir.join("events.sqlite3");
    let history = EventStore::open(&ledger_path)
        .expect("open ledger before recovery fixture")
        .replay_verified()
        .expect("verify ledger before recovery fixture");
    let mut rebuilt =
        EventStore::open(data_dir.join("events.rebuilt.sqlite3")).expect("open rebuilt ledger");
    for event in history.iter().filter(|event| !should_remove(event)) {
        rebuilt
            .append(EventInput::new(
                event.event_id.clone(),
                event.aggregate_id.clone(),
                event.event_type.clone(),
                event.actor.clone(),
                event.timestamp_millis,
                event.payload.clone(),
            ))
            .expect("reappend authenticated history without job terminal");
    }
    drop(rebuilt);
    for suffix in ["-wal", "-shm"] {
        let _ignored = fs::remove_file(format!("{}{suffix}", ledger_path.display()));
    }
    fs::remove_file(&ledger_path).expect("remove pre-recovery ledger");
    fs::rename(data_dir.join("events.rebuilt.sqlite3"), &ledger_path)
        .expect("install pre-recovery ledger");
}

fn rewrite_ledger_event_payload(
    data_dir: &Path,
    event_id: &str,
    payload: &[u8],
    actor: Option<&str>,
) {
    let ledger_path = data_dir.join("events.sqlite3");
    let history = EventStore::open(&ledger_path)
        .expect("open ledger before payload rewrite")
        .replay_verified()
        .expect("verify ledger before payload rewrite");
    let mut rebuilt =
        EventStore::open(data_dir.join("events.rebuilt.sqlite3")).expect("open rebuilt ledger");
    for event in &history {
        rebuilt
            .append(EventInput::new(
                event.event_id.clone(),
                event.aggregate_id.clone(),
                event.event_type.clone(),
                if event.event_id == event_id {
                    actor.unwrap_or(&event.actor).to_owned()
                } else {
                    event.actor.clone()
                },
                event.timestamp_millis,
                if event.event_id == event_id {
                    payload.to_owned()
                } else {
                    event.payload.clone()
                },
            ))
            .expect("reappend history with modified event payload");
    }
    drop(rebuilt);
    for suffix in ["-wal", "-shm"] {
        let _ignored = fs::remove_file(format!("{}{suffix}", ledger_path.display()));
    }
    fs::remove_file(&ledger_path).expect("remove source ledger");
    fs::rename(data_dir.join("events.rebuilt.sqlite3"), &ledger_path)
        .expect("install rewritten ledger");
}

fn rewrite_ledger_event_type(data_dir: &Path, event_id: &str, event_type: &str) {
    let ledger_path = data_dir.join("events.sqlite3");
    let history = EventStore::open(&ledger_path)
        .expect("open ledger before event-type rewrite")
        .replay_verified()
        .expect("verify ledger before event-type rewrite");
    let mut rebuilt =
        EventStore::open(data_dir.join("events.rebuilt.sqlite3")).expect("open rebuilt ledger");
    for event in &history {
        rebuilt
            .append(EventInput::new(
                event.event_id.clone(),
                event.aggregate_id.clone(),
                if event.event_id == event_id {
                    event_type.to_owned()
                } else {
                    event.event_type.clone()
                },
                event.actor.clone(),
                event.timestamp_millis,
                event.payload.clone(),
            ))
            .expect("reappend history with modified event type");
    }
    drop(rebuilt);
    for suffix in ["-wal", "-shm"] {
        let _ignored = fs::remove_file(format!("{}{suffix}", ledger_path.display()));
    }
    fs::remove_file(&ledger_path).expect("remove pre-rewrite ledger");
    fs::rename(data_dir.join("events.rebuilt.sqlite3"), &ledger_path)
        .expect("install event-type rewrite");
}

#[test]
fn async_worker_failure_is_durable_as_provider_failure() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let (world, _) = seed_compiled_genome(&data_dir);
    let worker = directory.path().join("failing-worker");
    fs::write(&worker, "#!/bin/sh\nexit 7\n").expect("write failing worker");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let daemon =
        Daemon::start_with_worker(&data_dir, Path::new(env!("CARGO_MANIFEST_DIR")), &worker);
    let genome_path = directory.path().join("failing-agent.md");
    fs::write(
        &genome_path,
        "---\nschema_version: 1\nname: failing-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write Genome");
    let registered = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_path.to_str().expect("UTF-8 path"),
            "--world",
            &world.world_id,
        ],
    ));
    let Some(ResponseData::Genome { genome }) = registered.data else {
        panic!("expected registered Genome");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let _accepted = response(&cli(
        &data_dir,
        &["submit", "failed-job", &genome.genome_id],
    ));

    let deadline = Instant::now() + Duration::from_secs(10);
    let terminal = loop {
        let status = response(&cli(&data_dir, &["job", "status", "failed-job"]));
        let Some(ResponseData::Job { job, .. }) = status.data else {
            panic!("expected job status");
        };
        if matches!(
            job.state,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        ) {
            break job;
        }
        assert!(
            Instant::now() < deadline,
            "failing worker job did not finish"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    daemon.stop();

    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open canonical ledger")
        .replay_verified()
        .expect("verify failed job history");
    let run_event = history
        .iter()
        .find(|event| event.event_type == "run.result_recorded")
        .expect("confirmed worker exit records a signed failure result");
    let seed: [u8; 32] = fs::read(data_dir.join("runtime-producer.key"))
        .expect("read producer key for verifier")
        .try_into()
        .expect("producer key width");
    let receipt = RunResultSigner::from_seed(seed)
        .verifier()
        .verify_event(run_event)
        .expect("authenticate signed failure result");
    assert_eq!(
        receipt.completion_reason,
        RunCompletionReason::ProviderFailure
    );
    remove_job_terminal_before_restart(&data_dir, "failed-job");
    let restarted = Daemon::start(&data_dir);
    let replay = response(&cli(&data_dir, &["replay"]));
    assert!(matches!(replay.data, Some(ResponseData::Replay { .. })));
    assert!(matches!(
        response(&cli(&data_dir, &["job", "status", "failed-job"])).data,
        Some(ResponseData::Job { job, .. })
            if job.state == JobState::Failed && job.terminal == Some(JobTerminal::Failed)
    ));
    restarted.stop();
}

#[test]
#[allow(clippy::too_many_lines)]
fn async_wall_timeout_is_durable_replayable_and_keeps_daemon_available() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let (world, _) = seed_compiled_genome(&data_dir);
    let worker = directory.path().join("timeout-worker");
    fs::write(
        &worker,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$HOME/timeout-worker.pid\"\nexec /bin/sleep 60\n",
    )
    .expect("write timeout worker");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let daemon =
        Daemon::start_with_worker(&data_dir, Path::new(env!("CARGO_MANIFEST_DIR")), &worker);
    let genome_path = directory.path().join("timeout-agent.md");
    fs::write(
        &genome_path,
        "---\nschema_version: 1\nname: timeout-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write Genome");
    let registered = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_path.to_str().expect("UTF-8 path"),
            "--world",
            &world.world_id,
        ],
    ));
    let Some(ResponseData::Genome { genome }) = registered.data else {
        panic!("expected registered Genome");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let accepted = response(&cli(
        &data_dir,
        &["submit", "timeout-job", &genome.genome_id],
    ));
    assert!(matches!(accepted.data, Some(ResponseData::Job { .. })));

    let pid_deadline = Instant::now() + Duration::from_secs(5);
    let worker_pid = loop {
        let pid_file = fs::read_dir(data_dir.join("sandboxes"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("execution/timeout-worker.pid"))
            .find(|path| path.is_file());
        if let Some(pid) = pid_file
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|contents| contents.parse::<u32>().ok())
        {
            break pid;
        }
        assert!(
            Instant::now() < pid_deadline,
            "timeout worker did not start"
        );
        thread::sleep(Duration::from_millis(5));
    };

    let deadline = Instant::now() + Duration::from_secs(20);
    let terminal = loop {
        let status = response(&cli(&data_dir, &["job", "status", "timeout-job"]));
        let Some(ResponseData::Job { job, .. }) = status.data else {
            panic!("daemon must remain responsive until the hard timeout");
        };
        if matches!(
            job.state,
            JobState::Succeeded | JobState::Failed | JobState::Interrupted
        ) {
            break job;
        }
        assert!(Instant::now() < deadline, "wall timeout was not recorded");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert!(matches!(
        response(&cli(&data_dir, &["status"])).data,
        Some(ResponseData::Status { .. })
    ));
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    daemon.stop();

    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open canonical ledger")
        .replay_verified()
        .expect("verify timeout history");
    let run_event = history
        .iter()
        .find(|event| event.event_type == "run.result_recorded")
        .expect("hard timeout is signed as a terminal run result");
    let seed: [u8; 32] = fs::read(data_dir.join("runtime-producer.key"))
        .expect("read producer key")
        .try_into()
        .expect("producer key width");
    let receipt = RunResultSigner::from_seed(seed)
        .verifier()
        .verify_event(run_event)
        .expect("verify timeout receipt signature");
    assert_eq!(
        receipt.completion_reason,
        RunCompletionReason::WallBudgetExceeded
    );
    let worker_probe = ProcessCommand::new("/bin/kill")
        .args(["-0", &worker_pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("probe timed-out worker");
    assert!(
        !worker_probe.success(),
        "timed-out worker survived its budget"
    );

    let restarted = Daemon::start(&data_dir);
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    assert!(matches!(
        response(&cli(&data_dir, &["job", "status", "timeout-job"])).data,
        Some(ResponseData::Job { job, .. })
            if job.state == JobState::Failed && job.terminal == Some(JobTerminal::Failed)
    ));
    restarted.stop();
}

#[test]
#[allow(clippy::too_many_lines)]
fn async_artifact_store_failure_does_not_sign_success_and_recovers() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"async fixture\n").expect("write fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let (world, _) = seed_compiled_genome(&data_dir);
    let worker = directory.path().join("delayed-failure-worker");
    fs::write(
        &worker,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$HOME/failure-worker.pid\"\n/bin/sleep 3\nexit 7\n",
    )
    .expect("write delayed failing worker");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let mut daemon = Daemon::start_with_worker(&data_dir, &repository, &worker);
    let genome_path = directory.path().join("agent.md");
    fs::write(
        &genome_path,
        "---\nschema_version: 1\nname: store-failure-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write Genome");
    let registered = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_path.to_str().expect("UTF-8 path"),
            "--world",
            &world.world_id,
        ],
    ));
    let Some(ResponseData::Genome { genome }) = registered.data else {
        panic!("expected registered Genome");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    assert!(matches!(
        response(&cli(
            &data_dir,
            &["submit", "store-failure", &genome.genome_id]
        ))
        .data,
        Some(ResponseData::Job { .. })
    ));

    let pid_file_deadline = Instant::now() + Duration::from_secs(5);
    let worker_pid = loop {
        let found = fs::read_dir(data_dir.join("sandboxes"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("execution"))
            .find(|path| path.join("failure-worker.pid").is_file());
        if let Some(path) = found
            && let Some(pid) = fs::read_to_string(path.join("failure-worker.pid"))
                .ok()
                .and_then(|contents| contents.parse::<u32>().ok())
        {
            break pid.to_string();
        }
        assert!(Instant::now() < pid_file_deadline, "worker did not start");
        thread::sleep(Duration::from_millis(5));
    };
    let blobs = data_dir.join("blobs");
    let saved_blobs = data_dir.join("blobs.saved");
    fs::rename(&blobs, &saved_blobs).expect("preserve canonical artifacts");
    fs::write(&blobs, b"injected artifact-store failure").expect("block artifact writes");

    let terminal_deadline = Instant::now() + Duration::from_secs(8);
    let mut daemon_exited = false;
    loop {
        if daemon.child.try_wait().expect("inspect daemon").is_some() {
            daemon_exited = true;
            break;
        }
        let history = EventStore::open(data_dir.join("events.sqlite3"))
            .expect("open canonical ledger")
            .replay_verified()
            .expect("verify job history");
        if history.iter().any(|event| {
            event.event_type == "job.terminal" && event.aggregate_id == "job:store-failure"
        }) {
            break;
        }
        assert!(Instant::now() < terminal_deadline, "job did not terminate");
        thread::sleep(Duration::from_millis(10));
    }
    fs::remove_file(&blobs).expect("remove artifact-store blocker");
    fs::rename(&saved_blobs, &blobs).expect("restore canonical artifacts");
    if !daemon_exited {
        daemon.stop();
    }

    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open canonical ledger")
        .replay_verified()
        .expect("verify failed job history");
    assert!(
        !history
            .iter()
            .any(|event| event.event_type == "run.result_recorded"),
        "a failed evidence append must not produce a signed run result"
    );
    let restarted = Daemon::start(&data_dir);
    let status = response(&cli(&data_dir, &["job", "status", "store-failure"]));
    let Some(ResponseData::Job { job: terminal, .. }) = status.data else {
        panic!("expected job status");
    };
    assert!(matches!(
        terminal.state,
        JobState::Failed | JobState::Interrupted
    ));
    assert!(matches!(
        terminal.terminal,
        Some(JobTerminal::Failed | JobTerminal::Interrupted)
    ));
    let worker_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let worker_alive = ProcessCommand::new("/bin/kill")
            .args(["-0", &worker_pid])
            .stderr(Stdio::null())
            .status()
            .expect("probe failed worker process");
        if !worker_alive.success() {
            break;
        }
        assert!(
            Instant::now() < worker_deadline,
            "failed job left its worker alive"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    restarted.stop();
}

#[test]
#[allow(clippy::too_many_lines)]
fn guardian_contains_worker_after_daemon_crash_and_replay_marks_job_interrupted() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"crash fixture\n").expect("write fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let (world, _) = seed_compiled_genome(&data_dir);
    let worker = directory.path().join("slow-worker");
    fs::write(
        &worker,
        "#!/bin/sh\n/bin/sleep 60 &\nchild=$!\nprintf '%s' \"$child\" > \"$HOME/slow-child.pid\"\nwait \"$child\"\n",
    )
    .expect("write slow worker");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let daemon = Daemon::start_with_worker(&data_dir, &repository, &worker);
    let genome_path = directory.path().join("agent.md");
    fs::write(
        &genome_path,
        "---\nschema_version: 1\nname: crash-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write Genome");
    let registered = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            genome_path.to_str().expect("UTF-8 path"),
            "--world",
            &world.world_id,
        ],
    ));
    let ResponseData::Genome { genome } = registered.data.expect("registered Genome") else {
        panic!("expected Genome response");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    assert!(
        cli(&data_dir, &["submit", "crash-job", &genome.genome_id])
            .status
            .success()
    );
    let pid_deadline = Instant::now() + Duration::from_secs(5);
    let child_pid = loop {
        let pid_file = fs::read_dir(data_dir.join("sandboxes"))
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("execution/slow-child.pid"))
            .find(|path| path.is_file());
        if let Some(child_pid) = pid_file
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|contents| contents.parse::<u32>().ok())
        {
            break child_pid;
        }
        assert!(Instant::now() < pid_deadline, "slow worker did not start");
        thread::sleep(Duration::from_millis(5));
    };
    daemon.crash();
    let death_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let probe = ProcessCommand::new("/bin/kill")
            .args(["-0", &child_pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .expect("probe worker descendant");
        if !probe.success() {
            break;
        }
        assert!(
            Instant::now() < death_deadline,
            "guardian left descendant alive"
        );
        thread::sleep(Duration::from_millis(10));
    }

    let restarted = Daemon::start_with_repository(&data_dir, &repository);
    let status = response(&cli(&data_dir, &["job", "status", "crash-job"]));
    let ResponseData::Job { job, .. } = status.data.expect("recovered job status") else {
        panic!("expected recovered job");
    };
    assert_eq!(job.state, JobState::Interrupted);
    assert_eq!(job.terminal, Some(JobTerminal::Interrupted));
    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open canonical ledger")
        .replay_verified()
        .expect("verify canonical history");
    assert!(!history.iter().any(|event| {
        event.event_type == "run.result_recorded"
            && event.aggregate_id == format!("run:{}", job.run_id)
    }));
    assert!(matches!(
        response(&cli(&data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    restarted.stop();
}

#[test]
#[allow(clippy::too_many_lines)]
fn guardian_monitors_eof_during_large_stdin_and_reaps_leader_descendants() {
    let directory = tempdir().expect("temporary directory");
    let home = directory.path().canonicalize().expect("canonical home");
    let guardian = |command: &str, input_bytes: usize| {
        let mut child = ProcessCommand::new(PROCESS_GUARDIAN)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start guardian");
        let config = serde_json::json!({
            "program": "/bin/sh",
            "arguments": ["-c", command],
            "current_dir": home,
            "home": home,
            "temp": home,
            "path": "/bin:/usr/bin",
            "input_bytes": input_bytes,
        });
        let mut input = child.stdin.take().expect("guardian input");
        writeln!(input, "{config}").expect("write guardian config");
        input
            .write_all(&vec![b'x'; input_bytes])
            .expect("write guardian worker input");
        input.write_all(b"\n").expect("write guardian delimiter");
        (child, input)
    };
    let wait_bounded = |child: &mut std::process::Child, descendant_pid: &Path| {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().expect("poll guardian exit") {
                return Some(status);
            }
            if Instant::now() >= deadline {
                if let Ok(pid) = fs::read_to_string(descendant_pid) {
                    let _ignored = ProcessCommand::new("/bin/kill")
                        .args(["-KILL", pid.trim()])
                        .status();
                }
                let _ignored = child.kill();
                let _ignored = child.wait();
                return None;
            }
            thread::sleep(Duration::from_millis(5));
        }
    };

    let (mut child, guardian_stdin) = guardian(
        "echo $$ > \"$HOME/large-input-worker.pid\"; exec /bin/sleep 60",
        1_048_576,
    );
    let worker_pid_path = home.join("large-input-worker.pid");
    let pid_deadline = Instant::now() + Duration::from_secs(2);
    while !worker_pid_path.is_file() {
        assert!(
            Instant::now() < pid_deadline,
            "large-input worker did not start"
        );
        thread::sleep(Duration::from_millis(5));
    }
    drop(guardian_stdin);
    let status = wait_bounded(&mut child, &worker_pid_path)
        .expect("guardian should stop promptly after liveness EOF");
    assert!(!status.success());
    let worker_pid = fs::read_to_string(worker_pid_path)
        .expect("read worker PID")
        .trim()
        .parse::<u32>()
        .expect("parse worker PID");
    let worker_probe = ProcessCommand::new("/bin/kill")
        .args(["-0", &worker_pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("probe worker");
    assert!(
        !worker_probe.success(),
        "large-input worker survived guardian EOF"
    );

    let command = "(/bin/sleep 60) & child=$!; echo $child > \"$HOME/held-pipe-child.pid\"; exit 0";
    let (mut child, _guardian_stdin) = guardian(command, 0);
    let descendant_pid_path = home.join("held-pipe-child.pid");
    let status = wait_bounded(&mut child, &descendant_pid_path)
        .expect("guardian must close descendant output pipes promptly");
    assert!(status.success());
    let descendant_pid = fs::read_to_string(descendant_pid_path)
        .expect("read descendant PID")
        .trim()
        .parse::<u32>()
        .expect("parse descendant PID");
    let descendant_probe = ProcessCommand::new("/bin/kill")
        .args(["-0", &descendant_pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("probe descendant");
    assert!(
        !descendant_probe.success(),
        "guardian left a pipe-holding descendant alive"
    );
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ignored = self.child.kill();
        let _ignored = self.child.wait();
    }
}

#[test]
fn operator_cli_controls_and_replays_real_daemon_state_across_restarts() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let genome = seed_canonical_state(data_dir);
    let daemon = Daemon::start(data_dir);

    let initial = cli(data_dir, &["status"]);
    assert!(initial.status.success());
    assert!(matches!(
        response(&initial).data,
        Some(ResponseData::Status {
            frozen: true,
            active_runs: 1,
            genome_count: 1,
            ..
        })
    ));
    let unfreeze = cli(data_dir, &["unfreeze"]);
    assert!(matches!(
        response(&unfreeze).data,
        Some(ResponseData::Acknowledged { frozen: false, .. })
    ));

    let duplicate = ProcessCommand::new(DAEMON)
        .arg("--data-dir")
        .arg(data_dir)
        .output()
        .expect("run duplicate daemon");
    assert!(!duplicate.status.success());
    daemon.crash();

    let daemon = Daemon::start(data_dir);
    let restarted = cli(data_dir, &["status"]);
    assert!(matches!(
        response(&restarted).data,
        Some(ResponseData::Status {
            frozen: false,
            active_runs: 1,
            ..
        })
    ));

    let freeze = cli(data_dir, &["freeze"]);
    assert!(matches!(
        response(&freeze).data,
        Some(ResponseData::Acknowledged { frozen: true, .. })
    ));
    let shown = cli(data_dir, &["genome", "show", &genome.genome_id]);
    assert_eq!(
        response(&shown).data,
        Some(ResponseData::Genome {
            genome: genome.clone()
        })
    );
    let replayed = cli(data_dir, &["replay"]);
    assert!(matches!(
        response(&replayed).data,
        Some(ResponseData::Replay {
            frozen: true,
            active_runs: 1,
            ref projection_hash,
            ..
        }) if projection_hash.len() == 64
    ));
    let killed = cli(data_dir, &["kill", "--all"]);
    assert!(matches!(
        response(&killed).data,
        Some(ResponseData::Acknowledged {
            frozen: true,
            killed_runs: 0
        })
    ));
    daemon.stop();

    let final_daemon = Daemon::start(data_dir);
    assert!(matches!(
        response(&cli(data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    let final_status = cli(data_dir, &["status"]);
    assert!(matches!(
        response(&final_status).data,
        Some(ResponseData::Status {
            frozen: true,
            active_runs: 1,
            ..
        })
    ));
    assert_private(data_dir, "operator.token", 0o600);
    assert_private(data_dir, "control.sock", 0o600);
    assert_private(data_dir, "events.sqlite3", 0o600);
    assert_private(data_dir, "daemon.lock", 0o600);
    assert_secure_data_dir(data_dir);
    final_daemon.stop();
}

#[test]
fn markdown_genome_prompt_registers_inspects_and_replays_through_the_daemon() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let base_genome = seed_canonical_state(data_dir);
    let daemon = Daemon::start(data_dir);
    let path = directory.path().join("agent.md");
    let prompt = "Use only trusted task evidence.\nKeep exact body bytes.  \n";
    fs::write(
        &path,
        format!(
            "---\nschema_version: 1\nname: markdown-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n{prompt}"
        ),
    )
    .unwrap();
    let registered = cli(
        data_dir,
        &[
            "genome",
            "register",
            path.to_str().unwrap(),
            "--world",
            &base_genome.world_id,
        ],
    );
    assert!(registered.status.success());
    let markdown = match response(&registered).data.unwrap() {
        ResponseData::Genome { genome } => genome,
        other => panic!("unexpected Markdown Genome response: {other:?}"),
    };
    let store = ArtifactStore::open(data_dir.join("blobs")).unwrap();
    let artifact_id = hephaestus_ledger::ArtifactId::parse(markdown.artifact_id.clone()).unwrap();
    let canonical: serde_json::Value =
        serde_json::from_slice(&store.get(&artifact_id).unwrap()).unwrap();
    let prompt_id = hephaestus_ledger::ArtifactId::parse(
        canonical["artifacts"]["agent.prompt"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
    .unwrap();
    assert_eq!(store.get(&prompt_id).unwrap(), prompt.as_bytes());
    assert_eq!(
        cli_text(data_dir, &["genome", "prompt", &markdown.genome_id]),
        prompt
    );
    assert!(matches!(
        response(&cli(data_dir, &["genome", "show", &markdown.genome_id])).data,
        Some(ResponseData::Genome { genome }) if genome == markdown
    ));
    assert_eq!(
        error_code(&cli(
            data_dir,
            &["genome", "prompt", &base_genome.genome_id]
        )),
        ApiErrorCode::NotFound
    );
    assert!(cli(data_dir, &["unfreeze"]).status.success());
    assert_eq!(
        error_code(&cli(data_dir, &["run", &markdown.genome_id])),
        ApiErrorCode::InvalidRequest,
        "generic prose remains stored and inspectable but is not a reference-worker program"
    );
    daemon.stop();

    let restarted = Daemon::start(data_dir);
    assert_eq!(
        cli_text(data_dir, &["genome", "prompt", &markdown.genome_id]),
        prompt
    );
    assert!(matches!(
        response(&cli(data_dir, &["replay"])).data,
        Some(ResponseData::Replay { .. })
    ));
    restarted.stop();
}

#[test]
fn maximum_markdown_prompt_round_trips_through_the_bounded_client() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path();
    let base_genome = seed_canonical_state(data_dir);
    let daemon = Daemon::start(data_dir);
    let path = directory.path().join("maximum-agent.md");
    let prefix = "---\nschema_version: 1\nname: maximum-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n";
    let prompt = "\u{1}".repeat(1_048_576 - prefix.len());
    let source = format!("{prefix}{prompt}");
    assert_eq!(source.len(), 1_048_576);
    fs::write(&path, source).unwrap();
    let registered = cli(
        data_dir,
        &[
            "genome",
            "register",
            path.to_str().unwrap(),
            "--world",
            &base_genome.world_id,
        ],
    );
    assert!(registered.status.success());
    let markdown = match response(&registered).data.unwrap() {
        ResponseData::Genome { genome } => genome,
        other => panic!("unexpected Markdown Genome response: {other:?}"),
    };
    assert_eq!(
        cli_text(data_dir, &["genome", "prompt", &markdown.genome_id]),
        prompt
    );
    daemon.stop();

    let restarted = Daemon::start(data_dir);
    assert_eq!(
        cli_text(data_dir, &["genome", "prompt", &markdown.genome_id]),
        prompt
    );
    restarted.stop();
}

#[test]
fn local_api_fails_closed_for_bad_auth_versions_and_requests() {
    let directory = tempdir().expect("temporary directory");
    let daemon = Daemon::start(directory.path());
    let socket = directory.path().join("control.sock");

    let unauthorized = raw_request(
        &socket,
        &serde_json::to_vec(&ApiRequest {
            version: API_VERSION,
            request_id: "unauthorized".to_owned(),
            token: "0".repeat(64),
            command: Command::Unfreeze,
        })
        .expect("encode request"),
    );
    assert_eq!(
        unauthorized.error.expect("unauthorized error").code,
        ApiErrorCode::Unauthorized
    );

    let token = fs::read_to_string(directory.path().join("operator.token")).expect("read token");
    let unsupported = raw_request(
        &socket,
        &serde_json::to_vec(&ApiRequest {
            version: 2,
            request_id: "unsupported".to_owned(),
            token,
            command: Command::Status,
        })
        .expect("encode request"),
    );
    assert_eq!(
        unsupported.error.expect("version error").code,
        ApiErrorCode::UnsupportedVersion
    );

    let malformed = raw_request(&socket, br#"{"version":1,"unknown":true}"#);
    assert_eq!(
        malformed.error.expect("schema error").code,
        ApiErrorCode::InvalidRequest
    );
    let oversized = raw_request(&socket, &vec![b'x'; 7 * 1_048_576 + 1]);
    let oversized_error = oversized.error.expect("size error");
    assert_eq!(oversized_error.code, ApiErrorCode::InvalidRequest);
    assert_eq!(oversized_error.message, "request exceeds limit");

    let token = fs::read_to_string(directory.path().join("operator.token")).expect("read token");
    let invalid_identifier = raw_request(
        &socket,
        &serde_json::to_vec(&ApiRequest {
            version: API_VERSION,
            request_id: "empty-genome".to_owned(),
            token,
            command: Command::GenomeShow {
                genome_id: " ".to_owned(),
            },
        })
        .expect("encode request"),
    );
    assert_eq!(
        invalid_identifier.error.expect("identifier error").code,
        ApiErrorCode::InvalidRequest
    );
    let token = fs::read_to_string(directory.path().join("operator.token")).expect("read token");
    for (index, evaluation_id, parent_genome_id, candidate_genome_id) in [
        (0, " ", "parent", "candidate"),
        (1, "evaluation", " ", "candidate"),
        (2, "evaluation", "parent", " "),
    ] {
        let invalid_pair = raw_request(
            &socket,
            &serde_json::to_vec(&ApiRequest {
                version: API_VERSION,
                request_id: format!("invalid-pair-{index}"),
                token: token.clone(),
                command: Command::EvaluatePair {
                    evaluation_id: evaluation_id.to_owned(),
                    parent_genome_id: parent_genome_id.to_owned(),
                    candidate_genome_id: candidate_genome_id.to_owned(),
                },
            })
            .expect("encode invalid pair"),
        );
        assert_eq!(
            invalid_pair.error.expect("pair identifier error").code,
            ApiErrorCode::InvalidRequest
        );
    }

    let status = cli(directory.path(), &["status"]);
    assert!(matches!(
        response(&status).data,
        Some(ResponseData::Status { frozen: true, .. })
    ));
    daemon.stop();

    let ledger = EventStore::open(directory.path().join("events.sqlite3")).expect("reopen ledger");
    let history = ledger.replay_verified().expect("verify audited history");
    assert!(
        history
            .iter()
            .any(|event| event.event_type == "control.status")
    );
}

#[test]
fn daemon_rejects_forged_operator_history_and_unverifiable_genomes() {
    let forged_directory = tempdir().expect("temporary directory");
    let mut forged = EventStore::open(forged_directory.path().join("events.sqlite3"))
        .expect("open forged ledger");
    forged
        .append(EventInput::new(
            "forged-unfreeze",
            "hephaestus-control",
            "control.unfreeze",
            "candidate",
            1,
            br#"{"request_id":"forged","command":{"command":"unfreeze"}}"#,
        ))
        .expect("append forged event");
    drop(forged);
    assert!(matches!(
        ControlPlane::open(forged_directory.path()),
        Err(ControlError::Projection(_))
    ));

    let missing_directory = tempdir().expect("temporary directory");
    let missing_hash = "2".repeat(64);
    let missing = GenomeRecord {
        genome_id: format!("hephaestus:genome:{missing_hash}"),
        name: "missing".to_owned(),
        world_id: format!("hephaestus:world:{}", "3".repeat(64)),
        artifact_id: missing_hash,
        parent_ids: Vec::new(),
    };
    let mut ledger = EventStore::open(missing_directory.path().join("events.sqlite3"))
        .expect("open missing-artifact ledger");
    ledger
        .append(EventInput::new(
            "missing-genome",
            &missing.genome_id,
            "genome.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&missing).expect("encode missing Genome"),
        ))
        .expect("append missing Genome");
    drop(ledger);
    assert!(matches!(
        ControlPlane::open(missing_directory.path()),
        Err(ControlError::Projection(_))
    ));

    let missing_world_directory = tempdir().expect("temporary directory");
    let world_hash = "4".repeat(64);
    let missing_world = WorldRecord {
        world_id: format!("hephaestus:world:{world_hash}"),
        name: "missing-world".to_owned(),
        artifact_id: world_hash,
    };
    let mut world_ledger = EventStore::open(missing_world_directory.path().join("events.sqlite3"))
        .expect("open missing World ledger");
    world_ledger
        .append(EventInput::new(
            "missing-world",
            &missing_world.world_id,
            "world.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&missing_world).expect("encode missing World"),
        ))
        .expect("append missing World");
    drop(world_ledger);
    assert!(matches!(
        ControlPlane::open(missing_world_directory.path()),
        Err(ControlError::Ledger(_))
    ));

    let forged_world_directory = tempdir().expect("temporary directory");
    let forged_artifacts = ArtifactStore::open(forged_world_directory.path().join("blobs"))
        .expect("open forged World artifacts");
    let forged_artifact = forged_artifacts
        .put(br#"{"name":"not-a-World"}"#)
        .expect("store forged World");
    let forged_world = WorldRecord {
        world_id: format!("hephaestus:world:{}", forged_artifact.as_str()),
        name: "not-a-World".to_owned(),
        artifact_id: forged_artifact.as_str().to_owned(),
    };
    let mut forged_world_ledger =
        EventStore::open(forged_world_directory.path().join("events.sqlite3"))
            .expect("open forged World ledger");
    forged_world_ledger
        .append(EventInput::new(
            "forged-world",
            &forged_world.world_id,
            "world.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&forged_world).expect("encode forged World"),
        ))
        .expect("append forged World");
    drop(forged_world_ledger);
    assert!(matches!(
        ControlPlane::open(forged_world_directory.path()),
        Err(ControlError::Projection(_))
    ));
}

#[test]
fn daemon_rejects_a_genome_without_a_registered_world_during_replay() {
    let directory = tempdir().expect("temporary directory");
    seed_genome_without_world(directory.path());
    assert!(matches!(
        ControlPlane::open(directory.path()),
        Err(ControlError::Projection(_))
    ));
}

#[test]
fn reference_run_without_a_resolvable_head_creates_no_runtime_evidence() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("empty-source");
    fs::create_dir_all(&repository).expect("create source repository");
    git(&repository, &["init"]);
    let (_, genome) = seed_compiled_genome(&data_dir);

    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let output = cli(&data_dir, &["run", &genome.genome_id]);
    assert!(!output.status.success());
    daemon.stop();

    let ledger = EventStore::open(data_dir.join("events.sqlite3")).expect("reopen ledger");
    let history = ledger.replay_verified().expect("verify history");
    assert!(history.iter().all(|event| {
        event.event_type != "trace.recorded" && event.event_type != "run.result_recorded"
    }));
    assert!(
        !data_dir.join("sandboxes").exists()
            || fs::read_dir(data_dir.join("sandboxes"))
                .expect("read sandbox root")
                .next()
                .is_none()
    );
}

fn seed_canonical_state(data_dir: &Path) -> GenomeRecord {
    let (_, genome) = seed_compiled_genome(data_dir);
    let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).expect("open event ledger");
    ledger
        .append(EventInput::new(
            "seed-run",
            "run-1",
            "run.started",
            "test-fixture",
            3,
            br#"{"run_id":"run-1"}"#,
        ))
        .expect("append active run");
    genome
}

fn seed_genome_without_world(data_dir: &Path) -> GenomeRecord {
    let artifact_store = ArtifactStore::open(data_dir.join("blobs")).expect("open artifact store");
    let canonical = br#"{"name":"seed"}"#;
    let artifact = artifact_store
        .put(canonical)
        .expect("store canonical Genome");
    let genome = GenomeRecord {
        genome_id: format!("hephaestus:genome:{}", artifact.as_str()),
        name: "seed".to_owned(),
        world_id: format!("hephaestus:world:{}", "1".repeat(64)),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: Vec::new(),
    };
    let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).expect("open event ledger");
    ledger
        .append(EventInput::new(
            "seed-genome",
            &genome.genome_id,
            "genome.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&genome).expect("encode Genome"),
        ))
        .expect("append Genome");
    genome
}

fn seed_compiled_genome(data_dir: &Path) -> (WorldRecord, GenomeRecord) {
    fs::create_dir_all(data_dir).expect("create data directory");
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open artifact store");
    let world_source = r#"{
        "schema_version": 1,
        "name": "reference-world",
        "laws": {
            "candidate_network": false,
            "candidate_evaluator_access": false,
            "maximum_cost_microusd": 0
        },
        "authority_ceiling": { "workspace_write": false, "network": false },
        "mutation_scope": [],
        "promotion": {
            "minimum_delta_bps": 0,
            "maximum_regressions": 0,
            "confidence_bps": 9500
        },
        "objectives": ["inventory"],
        "evaluator_artifacts": {}
    }"#;
    let compiled_world =
        compile_world(world_source, SourceFormat::Json, &artifacts).expect("compile World");
    let world_artifact = artifacts
        .put(compiled_world.canonical_json())
        .expect("store World");
    let world = WorldRecord {
        world_id: compiled_world.id().to_owned(),
        name: compiled_world.name().to_owned(),
        artifact_id: world_artifact.as_str().to_owned(),
    };
    let genome_source = r#"{
        "schema_version": 1,
        "name": "reference-genome",
        "parents": [],
        "model": { "provider": "deterministic", "family": "reference" },
        "authority": { "workspace_write": false, "network": false },
        "artifacts": {}
    }"#;
    let compiled_genome = compile_genome(
        genome_source,
        SourceFormat::Json,
        &compiled_world,
        &std::collections::BTreeMap::new(),
        &artifacts,
    )
    .expect("compile Genome");
    let genome_artifact = artifacts
        .put(compiled_genome.canonical_json())
        .expect("store Genome");
    let genome = GenomeRecord {
        genome_id: compiled_genome.id().to_owned(),
        name: compiled_genome.name().to_owned(),
        world_id: world.world_id.clone(),
        artifact_id: genome_artifact.as_str().to_owned(),
        parent_ids: compiled_genome.parents().to_vec(),
    };
    let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).expect("open event ledger");
    ledger
        .append(EventInput::new(
            "reference-world",
            &world.world_id,
            "world.registered",
            "test-fixture",
            1,
            serde_json::to_vec(&world).expect("encode World"),
        ))
        .expect("register World");
    ledger
        .append(EventInput::new(
            "reference-genome",
            &genome.genome_id,
            "genome.registered",
            "test-fixture",
            2,
            serde_json::to_vec(&genome).expect("encode Genome"),
        ))
        .expect("register Genome");
    (world, genome)
}

#[test]
#[allow(clippy::too_many_lines)]
fn authenticated_reference_and_arena_validation_rejects_invalid_inputs() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create source repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("README.md"), b"validation fixture\n").expect("write fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let (world, parent) = seed_compiled_genome(&data_dir);
    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    let client = Client::new(&data_dir);

    let empty_selection = client
        .request(Command::ArenaSelect {
            evaluation_id: " ".to_owned(),
        })
        .expect("empty selection response");
    assert_eq!(
        empty_selection.error.expect("empty selection error").code,
        ApiErrorCode::InvalidRequest
    );

    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open CAS");
    let unsupported_prompt = artifacts
        .put(b"ordinary prose is not a reference-worker program\n")
        .expect("store unsupported prompt");
    let unsupported_source = directory.path().join("unsupported.json");
    fs::write(
        &unsupported_source,
        serde_json::json!({
            "schema_version": 1,
            "name": "unsupported-reference-instruction",
            "parents": [parent.genome_id],
            "model": { "provider": "deterministic", "family": "reference" },
            "authority": { "workspace_write": false, "network": false },
            "artifacts": { "agent.prompt": unsupported_prompt.as_str() }
        })
        .to_string(),
    )
    .expect("write unsupported Genome");
    let ResponseData::Genome {
        genome: unsupported,
    } = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            unsupported_source.to_str().expect("UTF-8 source path"),
            "--world",
            &world.world_id,
        ],
    ))
    .data
    .expect("registered Genome response")
    else {
        panic!("unexpected Genome registration response");
    };
    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let rejected_run = client
        .request(Command::RunReference {
            genome_id: unsupported.genome_id,
        })
        .expect("unsupported prompt response");
    let run_error = rejected_run.error.expect("unsupported prompt error");
    assert_eq!(run_error.code, ApiErrorCode::InvalidRequest);
    assert_eq!(
        run_error.message,
        "registered reference instruction is invalid"
    );

    let second_source = directory.path().join("second.json");
    fs::write(
        &second_source,
        serde_json::json!({
            "schema_version": 1,
            "name": "unpaired-candidate",
            "parents": [parent.genome_id],
            "model": { "provider": "deterministic", "family": "reference" },
            "authority": { "workspace_write": false, "network": false },
            "artifacts": {}
        })
        .to_string(),
    )
    .expect("write unpaired Genome");
    let ResponseData::Genome { genome: candidate } = response(&cli(
        &data_dir,
        &[
            "genome",
            "register",
            second_source.to_str().expect("UTF-8 source path"),
            "--world",
            &world.world_id,
        ],
    ))
    .data
    .expect("registered candidate response") else {
        panic!("unexpected candidate registration response");
    };
    let rejected_pair = client
        .request(Command::EvaluatePair {
            evaluation_id: "unpaired-evaluation".to_owned(),
            parent_genome_id: parent.genome_id,
            candidate_genome_id: candidate.genome_id,
        })
        .expect("unpaired evaluation response");
    let pair_error = rejected_pair.error.expect("unpaired evaluation error");
    assert_eq!(pair_error.code, ApiErrorCode::InvalidRequest);
    assert_eq!(
        pair_error.message,
        "World does not declare arena.visible_manifest"
    );

    let history = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open event ledger")
        .replay_verified()
        .expect("verify event history");
    assert!(history.iter().all(|event| {
        !matches!(
            event.event_type.as_str(),
            "run.result_recorded" | "arena.job.admitted"
        )
    }));
    daemon.stop();
}

fn git(repository: &Path, arguments: &[&str]) {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(repository: &Path, arguments: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn cli(data_dir: &Path, arguments: &[&str]) -> Output {
    let mut command = ProcessCommand::new(CLI);
    command
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--json")
        .args(arguments)
        .output()
        .expect("run CLI")
}

fn response(output: &Output) -> ApiResponse {
    assert!(
        output.status.success(),
        "CLI failed: {}; stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).expect("decode CLI response")
}

fn raw_request(socket: &Path, bytes: &[u8]) -> ApiResponse {
    let mut stream = UnixStream::connect(socket).expect("connect control socket");
    if let Err(error) = stream.write_all(bytes) {
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe,
            "write request"
        );
    }
    let _ignored = stream.shutdown(Shutdown::Write);
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    serde_json::from_slice(&response).expect("decode response")
}

fn assert_private(data_dir: &Path, name: &str, expected: u32) {
    let path = PathBuf::from(data_dir).join(name);
    assert_eq!(
        fs::metadata(path)
            .expect("private path metadata")
            .permissions()
            .mode()
            & 0o777,
        expected
    );
}

fn assert_secure_data_dir(data_dir: &Path) {
    for path in [data_dir.to_owned(), data_dir.join("blobs")] {
        assert_eq!(
            fs::metadata(path)
                .expect("data directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

const QUICKSTART: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/quickstart");

fn cli_text(data_dir: &Path, arguments: &[&str]) -> String {
    let output = ProcessCommand::new(CLI)
        .arg("--data-dir")
        .arg(data_dir)
        .args(arguments)
        .output()
        .expect("run CLI");
    assert!(
        output.status.success(),
        "CLI command {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 CLI output")
}

fn first_word(text: &str) -> String {
    text.split_whitespace()
        .next()
        .expect("non-empty CLI output")
        .to_owned()
}

fn error_body(output: &Output) -> hephaestus_control::ApiError {
    assert!(!output.status.success(), "CLI unexpectedly succeeded");
    serde_json::from_slice::<ApiResponse>(&output.stdout)
        .expect("decode CLI error response")
        .error
        .expect("error body")
}

fn error_code(output: &Output) -> ApiErrorCode {
    error_body(output).code
}

#[test]
#[allow(clippy::too_many_lines)]
fn operator_registers_worlds_and_genomes_through_the_cli_and_evaluates_them() {
    let directory = tempdir().expect("temporary directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("source");
    fs::create_dir_all(&repository).expect("create source repository");
    git(&repository, &["init"]);
    git(&repository, &["config", "user.name", "Hephaestus Test"]);
    git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("README.md"), b"quickstart fixture\n").expect("write fixture");
    git(&repository, &["add", "."]);
    git(&repository, &["commit", "-m", "fixture"]);
    let quickstart = Path::new(QUICKSTART);
    let scratch = directory.path().join("scratch");
    fs::create_dir_all(&scratch).expect("create scratch directory");

    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    let text = |arguments: &[&str]| cli_text(&data_dir, arguments);

    // Manifests are canonicalized by the daemon; the pretty source bytes are never stored.
    let visible_path = quickstart.join("tasks/visible.json");
    let visible = first_word(&text(&[
        "arena",
        "manifest",
        visible_path.to_str().unwrap(),
    ]));
    let sealed = first_word(&text(&[
        "arena",
        "manifest",
        quickstart.join("tasks/sealed.json").to_str().unwrap(),
    ]));
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open artifact store");
    let canonical_visible = artifacts
        .get(&hephaestus_ledger::ArtifactId::parse(visible.clone()).unwrap())
        .expect("canonical visible manifest");
    assert_ne!(canonical_visible, fs::read(&visible_path).unwrap());
    TrustedManifest::from_canonical_bytes(&canonical_visible, Visibility::Visible)
        .expect("stored manifest is canonical");
    let evaluator = first_word(&text(&[
        "artifact",
        "put",
        data_dir.join("reference-evaluator").to_str().unwrap(),
    ]));
    let verifier_line = text(&["verifier"]);
    let verifier = first_word(&verifier_line);
    assert!(verifier_line.contains("public_key="));

    // A World that anchors somebody else's verifier is rejected before it can poison replay.
    let foreign_key = first_word(&text(&[
        "artifact",
        "put",
        quickstart.join("parent.json").to_str().unwrap(),
    ]));
    let template = fs::read_to_string(quickstart.join("world.template.json")).unwrap();
    let render = |verifier_id: &str| {
        template
            .replace("__VISIBLE_MANIFEST__", &visible)
            .replace("__SEALED_MANIFEST__", &sealed)
            .replace("__EVALUATOR__", &evaluator)
            .replace("__VERIFIER__", verifier_id)
    };
    let foreign_world = scratch.join("foreign-world.json");
    fs::write(&foreign_world, render(&foreign_key)).unwrap();
    let rejected = cli(
        &data_dir,
        &["world", "register", foreign_world.to_str().unwrap()],
    );
    let rejected = error_body(&rejected);
    assert_eq!(rejected.code, ApiErrorCode::InvalidRequest);
    assert!(
        rejected.message.contains("runtime producer key"),
        "operator is told why: {}",
        rejected.message
    );
    let unknown_extension = scratch.join("world.txt");
    fs::write(&unknown_extension, render(&verifier)).unwrap();
    assert_eq!(
        error_code(&cli(
            &data_dir,
            &["world", "register", unknown_extension.to_str().unwrap()],
        )),
        ApiErrorCode::InvalidRequest
    );

    let world_path = scratch.join("world.json");
    fs::write(&world_path, render(&verifier)).unwrap();
    // The daemon never resolves paths relative to its own working directory.
    let token = fs::read_to_string(data_dir.join("operator.token")).expect("read token");
    let relative = raw_request(
        &data_dir.join("control.sock"),
        &serde_json::to_vec(&ApiRequest {
            version: API_VERSION,
            request_id: "relative-path".to_owned(),
            token,
            command: Command::WorldRegister {
                path: "examples/quickstart/world.template.json".to_owned(),
            },
        })
        .unwrap(),
    );
    let relative = relative.error.expect("relative path rejected");
    assert_eq!(relative.code, ApiErrorCode::InvalidRequest);
    assert!(
        relative.message.contains("absolute"),
        "rejected for being relative, not merely unreadable: {}",
        relative.message
    );
    let raw = |command: Command| {
        let token = fs::read_to_string(data_dir.join("operator.token")).expect("read token");
        raw_request(
            &data_dir.join("control.sock"),
            &serde_json::to_vec(&ApiRequest {
                version: API_VERSION,
                request_id: "field-check".to_owned(),
                token,
                command,
            })
            .unwrap(),
        )
        .error
        .expect("request rejected")
    };
    assert_eq!(
        raw(Command::WorldRegister {
            path: String::new()
        })
        .code,
        ApiErrorCode::InvalidRequest
    );
    assert_eq!(
        raw(Command::GenomeRegister {
            path: world_path.to_str().unwrap().to_owned(),
            world_id: "  ".to_owned(),
        })
        .code,
        ApiErrorCode::InvalidRequest
    );
    assert!(
        raw(Command::ArtifactPut {
            path: scratch.to_str().unwrap().to_owned(),
        })
        .message
        .contains("regular file")
    );
    let oversized = scratch.join("oversized.json");
    fs::write(&oversized, vec![b' '; 1_048_577]).unwrap();
    assert!(
        raw(Command::WorldRegister {
            path: oversized.to_str().unwrap().to_owned(),
        })
        .message
        .contains("size limit")
    );
    let world_line = text(&["world", "register", world_path.to_str().unwrap()]);
    let world_id = first_word(&world_line);
    assert!(world_id.starts_with("hephaestus:world:"));
    assert!(world_line.contains("quickstart-world"));
    // Registration is idempotent: the same source yields the same identity and no conflict.
    assert_eq!(
        first_word(&text(&["world", "register", world_path.to_str().unwrap()])),
        world_id
    );
    let listed = response(&cli(&data_dir, &["world", "list"]));
    match listed.data {
        Some(ResponseData::Worlds { worlds }) => {
            assert_eq!(worlds.len(), 1);
            assert_eq!(worlds[0].world_id, world_id);
        }
        other => panic!("unexpected world list: {other:?}"),
    }
    match response(&cli(&data_dir, &["world", "show", &world_id])).data {
        Some(ResponseData::World { world }) => assert_eq!(world.name, "quickstart-world"),
        other => panic!("unexpected world show: {other:?}"),
    }

    let parent_path = quickstart.join("parent.json");
    assert_eq!(
        error_code(&cli(
            &data_dir,
            &[
                "genome",
                "register",
                parent_path.to_str().unwrap(),
                "--world",
                "hephaestus:world:missing",
            ],
        )),
        ApiErrorCode::NotFound
    );
    let parent_id = first_word(&text(&[
        "genome",
        "register",
        parent_path.to_str().unwrap(),
        "--world",
        &world_id,
    ]));
    assert!(parent_id.starts_with("hephaestus:genome:"));
    let candidate_path = scratch.join("candidate.json");
    fs::write(
        &candidate_path,
        fs::read_to_string(quickstart.join("candidate.template.json"))
            .unwrap()
            .replace("__PARENT_ID__", &parent_id),
    )
    .unwrap();
    let candidate_line = text(&[
        "genome",
        "register",
        candidate_path.to_str().unwrap(),
        "--world",
        &world_id,
    ]);
    let candidate_id = first_word(&candidate_line);
    assert!(candidate_line.contains(&format!("parents={parent_id}")));
    assert_eq!(
        first_word(&text(&[
            "genome",
            "register",
            candidate_path.to_str().unwrap(),
            "--world",
            &world_id,
        ])),
        candidate_id
    );
    // A child claiming authority its parent lacks is refused with the compiler's reason.
    let widened_path = scratch.join("widened.json");
    fs::write(
        &widened_path,
        fs::read_to_string(&candidate_path)
            .unwrap()
            .replace("\"network\": false", "\"network\": true"),
    )
    .unwrap();
    let widened = cli(
        &data_dir,
        &[
            "genome",
            "register",
            widened_path.to_str().unwrap(),
            "--world",
            &world_id,
        ],
    );
    let widened = error_body(&widened);
    assert_eq!(widened.code, ApiErrorCode::InvalidRequest);
    assert!(widened.message.contains("Genome source rejected"));
    match response(&cli(&data_dir, &["genome", "list"])).data {
        Some(ResponseData::Genomes { genomes }) => {
            assert_eq!(genomes.len(), 2);
        }
        other => panic!("unexpected genome list: {other:?}"),
    }

    // A World without Arena artifacts supports `run` but not paired evaluation, and a
    // Genome's content identity belongs to exactly one World.
    let run_only_world = scratch.join("run-only-world.yaml");
    fs::write(
        &run_only_world,
        "schema_version: 1\nname: run-only-world\nlaws:\n  candidate_network: false\n  candidate_evaluator_access: false\n  maximum_cost_microusd: 0\nauthority_ceiling:\n  workspace_write: false\n  network: false\nmutation_scope: [harness]\npromotion:\n  minimum_delta_bps: 0\n  maximum_regressions: 0\n  confidence_bps: 9500\nobjectives: [correctness]\nevaluator_artifacts: {}\n",
    )
    .unwrap();
    let run_only_world_id = first_word(&text(&[
        "world",
        "register",
        run_only_world.to_str().unwrap(),
    ]));
    let already_owned = cli(
        &data_dir,
        &[
            "genome",
            "register",
            parent_path.to_str().unwrap(),
            "--world",
            &run_only_world_id,
        ],
    );
    assert!(
        error_body(&already_owned)
            .message
            .contains("already registered under World")
    );
    let mut run_only_ids = Vec::new();
    for name in ["run-only-a", "run-only-b"] {
        let path = scratch.join(format!("{name}.json"));
        fs::write(
            &path,
            fs::read_to_string(&parent_path)
                .unwrap()
                .replace("quickstart-parent", name),
        )
        .unwrap();
        run_only_ids.push(first_word(&text(&[
            "genome",
            "register",
            path.to_str().unwrap(),
            "--world",
            &run_only_world_id,
        ])));
    }

    assert!(cli(&data_dir, &["unfreeze"]).status.success());
    let mixed = cli(
        &data_dir,
        &["arena", "evaluate", "mixed", &parent_id, &run_only_ids[0]],
    );
    assert!(
        error_body(&mixed)
            .message
            .contains("share one registered World")
    );
    let no_manifests = cli(
        &data_dir,
        &[
            "arena",
            "evaluate",
            "run-only",
            &run_only_ids[0],
            &run_only_ids[1],
        ],
    );
    assert!(
        error_body(&no_manifests)
            .message
            .contains("arena.visible_manifest")
    );
    match response(&cli(&data_dir, &["run", &parent_id])).data {
        Some(ResponseData::Run {
            genome_id,
            world_id: run_world,
            completion_reason,
            ..
        }) => {
            assert_eq!(genome_id, parent_id);
            assert_eq!(run_world, world_id);
            assert_eq!(completion_reason, RunCompletionReason::Success);
        }
        other => panic!("unexpected run response: {other:?}"),
    }
    match response(&cli(
        &data_dir,
        &[
            "arena",
            "evaluate",
            "quickstart-1",
            &parent_id,
            &candidate_id,
        ],
    ))
    .data
    {
        Some(ResponseData::Evaluation { evaluation }) => {
            assert_eq!(evaluation.world_id, world_id);
            assert_eq!(evaluation.parent_genome_id, parent_id);
            assert_eq!(evaluation.candidate_genome_id, candidate_id);
            assert_eq!(evaluation.visible_total, 1);
        }
        other => panic!("unexpected evaluation response: {other:?}"),
    }
    let replay_line = text(&["replay"]);
    assert!(replay_line.starts_with("replayed events="));

    // Everything the operator registered survives a restart because it is canonical history.
    daemon.stop();
    let daemon = Daemon::start_with_repository(&data_dir, &repository);
    match response(&cli(&data_dir, &["status"])).data {
        Some(ResponseData::Status { genome_count, .. }) => assert_eq!(genome_count, 4),
        other => panic!("unexpected status: {other:?}"),
    }
    match response(&cli(&data_dir, &["genome", "show", &candidate_id])).data {
        Some(ResponseData::Genome { genome }) => assert_eq!(genome.parent_ids, vec![parent_id]),
        other => panic!("unexpected genome show: {other:?}"),
    }
    daemon.stop();
}
