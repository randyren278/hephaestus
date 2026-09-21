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
    GenomeRecord, ResponseData, RunCompletionReason, WorldRecord,
};
use hephaestus_experience::{
    RUN_RESULT_SCHEMA_VERSION, RunBudgetReceipt, RunResultReceipt, RunResultSigner,
};
use hephaestus_genome::{CompiledWorld, SourceFormat, compile_genome, compile_world};
use hephaestus_ledger::{ArtifactStore, EventInput, EventStore};
use tempfile::tempdir;

const DAEMON: &str = env!("CARGO_BIN_EXE_hephaestusd");
const CLI: &str = env!("CARGO_BIN_EXE_hephaestus");
const REFERENCE_EVALUATOR: &str = env!("CARGO_BIN_EXE_hephaestus-reference-evaluator");

fn runtime_environment_id() -> String {
    format!(
        "deterministic-v1.runtime-{}.receipt-schema-{}.{}.{}.isolation-private-worktree-v1.backend-git",
        env!("CARGO_PKG_VERSION"),
        RUN_RESULT_SCHEMA_VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH
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
        fs::create_dir_all(data_dir).expect("create daemon data directory");
        let evaluator = data_dir.join("reference-evaluator");
        fs::copy(REFERENCE_EVALUATOR, &evaluator).expect("copy evaluator executable");
        fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
            .expect("protect evaluator executable");
        let mut child = ProcessCommand::new(DAEMON)
            .arg("--data-dir")
            .arg(data_dir)
            .arg("--source-repository")
            .arg(source_repository)
            .arg("--evaluator-executable")
            .arg(evaluator)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start daemon");
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
        vec![TrustedTask::new("visible-task", "visible input", "not inventory").unwrap()],
    )
    .unwrap();
    let sealed = TrustedManifest::new(
        "sealed-v1",
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", "sealed input", "not inventory").unwrap()],
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
        r#"{{"schema_version":1,"name":"daemon-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
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
    let parent = compiled_genome_record("daemon-parent", &world, &artifacts);
    let candidate = compiled_genome_record("daemon-candidate", &world, &artifacts);
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
    for (sequence, genome) in [(2, &parent), (3, &candidate)] {
        ledger
            .append(EventInput::new(
                format!("genome-{sequence}"),
                &genome.genome_id,
                "genome.registered",
                "test-fixture",
                sequence,
                serde_json::to_vec(genome).unwrap(),
            ))
            .unwrap();
    }
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
        assert_eq!(first_evaluation.candidate_visible_correct, 0);
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
            let expected = match receipt.task_id.as_str() {
                "visible-task" => b"visible input".as_slice(),
                "sealed-task" => b"sealed input".as_slice(),
                other => panic!("unexpected daemon-owned task: {other}"),
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
    let restarted = Daemon::start_with_repository(&data_dir, &repository);
    assert!(cli(&data_dir, &["replay"]).status.success());
    restarted.stop();

    let binding = EvaluationBinding::new(
        world.id(),
        42,
        runtime_environment_id(),
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

fn compiled_genome_record(
    name: &str,
    world: &CompiledWorld,
    artifacts: &ArtifactStore,
) -> GenomeRecord {
    let source = format!(
        r#"{{"schema_version":1,"name":"{name}","parents":[],"model":{{"provider":"deterministic","family":"v1"}},"authority":{{"workspace_write":false,"network":false}},"artifacts":{{}}}}"#
    );
    let genome = compile_genome(
        &source,
        SourceFormat::Json,
        world,
        &std::collections::BTreeMap::new(),
        artifacts,
    )
    .unwrap();
    let artifact = artifacts.put(genome.canonical_json()).unwrap();
    GenomeRecord {
        genome_id: genome.id().to_owned(),
        name: genome.name().to_owned(),
        world_id: world.id().to_owned(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: vec![],
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
            killed_runs: 1
        })
    ));
    daemon.stop();

    let final_daemon = Daemon::start(data_dir);
    let final_status = cli(data_dir, &["status"]);
    assert!(matches!(
        response(&final_status).data,
        Some(ResponseData::Status {
            frozen: true,
            active_runs: 0,
            ..
        })
    ));
    assert_private(data_dir, "operator.token", 0o600);
    assert_private(data_dir, "control.sock", 0o600);
    assert_private(data_dir, "events.sqlite3", 0o600);
    assert_private(data_dir, "daemon.lock", 0o600);
    assert_eq!(
        fs::metadata(data_dir)
            .expect("data directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(data_dir.join("blobs"))
            .expect("artifact directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    final_daemon.stop();
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
    let oversized = raw_request(&socket, &vec![b'x'; 65_537]);
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
            .any(|event| event.event_type == "control.genome_show")
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
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("decode CLI response")
}

fn raw_request(socket: &Path, bytes: &[u8]) -> ApiResponse {
    let mut stream = UnixStream::connect(socket).expect("connect control socket");
    stream.write_all(bytes).expect("write request");
    stream.shutdown(Shutdown::Write).expect("finish request");
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
        "CLI failed: {}",
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
