use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use hephaestus_arena::{
    ArenaError, EvaluationBinding, EvaluationInputs, EvaluationSources, EvaluationStores,
    IsolatedEvaluator, OperatorEvaluation, ReceiptContext, SelectionReceipt, SuggestedMutation,
    TrialPlan, TrustedManifest, TrustedTask, Visibility, check_failure_clusters,
    check_reference_output_invariants, evaluate_and_record, evaluate_and_record_scored,
    invariant_event_references, load_failure_clusters, load_operator_evaluation,
    load_reference_output_invariants, load_selection, prepare_evaluation, select_and_record,
    verify_cluster_event, verify_reference_output_invariant_event, verify_selection_event,
};
use hephaestus_core::authority::CapabilitySet;
use hephaestus_experience::{
    RunBudgetReceipt, RunCompletionReason, RunResultReceipt, RunResultSigner,
};
use hephaestus_genome::{CompiledWorld, SourceFormat, compile_genome, compile_world};
use hephaestus_ledger::{ArtifactId, ArtifactStore, EventInput, StoredEvent};
use hephaestus_runtime::{Budget, ExperimentContext, IsolationPolicy, RunSpec, WorkerLimits};
use tempfile::TempDir;

const VISIBLE_SECRET: &str = "visible-expected-never-in-candidate-input";
const SEALED_SECRET: &str = "sealed-expected-9f67c2";
const TASKS: [&str; 4] = [
    "task-sealed-a",
    "task-sealed-b",
    "task-visible-a",
    "task-visible-b",
];

struct Fixture {
    stores: EvaluationStores,
    world: CompiledWorld,
    binding: EvaluationBinding,
    visible: TrustedManifest,
    sealed: TrustedManifest,
    parent_genome_id: String,
    candidate_genome_id: String,
    parent: TrialPlan,
    candidate: TrialPlan,
    evaluator: IsolatedEvaluator,
    signer: RunResultSigner,
    repository: PathBuf,
    revision: String,
    alternate_revision: String,
}

fn manifests() -> (TrustedManifest, TrustedManifest) {
    (
        TrustedManifest::new(
            "visible-suite-v1",
            Visibility::Visible,
            vec![
                TrustedTask::new("task-visible-b", "input visible b", "B").unwrap(),
                TrustedTask::new("task-visible-a", "input visible a", VISIBLE_SECRET).unwrap(),
            ],
        )
        .unwrap(),
        TrustedManifest::new(
            "sealed-suite-v1",
            Visibility::Sealed,
            vec![
                TrustedTask::new("task-sealed-b", "sealed prompt 517", "Z").unwrap(),
                TrustedTask::new("task-sealed-a", "sealed prompt 204", SEALED_SECRET).unwrap(),
            ],
        )
        .unwrap(),
    )
}

fn context() -> ReceiptContext {
    ReceiptContext {
        event_id: "arena:evaluation:evaluation-001:recorded".to_owned(),
        evaluation_id: "evaluation-001".to_owned(),
        caller_id: "arena-test".to_owned(),
        timestamp_millis: 1_788_000_123_456,
    }
}

#[allow(clippy::too_many_arguments)]
fn append_run(
    stores: &mut EvaluationStores,
    signer: &RunResultSigner,
    repository: &Path,
    run_id: &str,
    genome_id: &str,
    world_id: &str,
    revision: &str,
    task_id: &str,
    input: &str,
    reason: RunCompletionReason,
    output: &[u8],
) -> String {
    append_run_with_context(
        stores,
        signer,
        repository,
        run_id,
        genome_id,
        world_id,
        revision,
        task_id,
        input,
        42,
        "environment-v1",
        Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap(),
        reason,
        output,
    )
}

#[allow(clippy::too_many_arguments)]
fn append_run_with_context(
    stores: &mut EvaluationStores,
    signer: &RunResultSigner,
    repository: &Path,
    run_id: &str,
    genome_id: &str,
    world_id: &str,
    revision: &str,
    task_id: &str,
    input: &str,
    seed: u64,
    environment_id: &str,
    budget: Budget,
    reason: RunCompletionReason,
    output: &[u8],
) -> String {
    append_run_with_observations(
        stores,
        signer,
        repository,
        run_id,
        genome_id,
        world_id,
        revision,
        task_id,
        input,
        seed,
        environment_id,
        budget,
        reason,
        output,
        1,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn append_run_with_observations(
    stores: &mut EvaluationStores,
    signer: &RunResultSigner,
    repository: &Path,
    run_id: &str,
    genome_id: &str,
    world_id: &str,
    revision: &str,
    task_id: &str,
    input: &str,
    seed: u64,
    environment_id: &str,
    budget: Budget,
    reason: RunCompletionReason,
    output: &[u8],
    latency_millis: u64,
    actual_cost_microusd: u64,
) -> String {
    let stdout = stores.artifacts.put(output).unwrap();
    let stderr = stores.artifacts.put(b"").unwrap();
    let trace = stores
        .artifacts
        .put(format!("trace:{run_id}").as_bytes())
        .unwrap();
    let experiment = ExperimentContext::new(task_id, input, seed, environment_id).unwrap();
    let spec = RunSpec::new_for_experiment_at_revision(
        run_id,
        genome_id,
        world_id,
        repository,
        revision,
        input,
        CapabilitySet::new(false, false),
        budget,
        experiment,
    )
    .unwrap();
    let receipt = RunResultReceipt::from_run_spec(
        &spec,
        reason,
        latency_millis,
        actual_cost_microusd,
        stdout.as_str(),
        stderr.as_str(),
        vec![trace.as_str().to_owned()],
    )
    .unwrap();
    let event_id = receipt.event_id();
    stores
        .events
        .append(signer.issue(receipt, 1_788_000_000_000).unwrap())
        .unwrap();
    event_id
}

fn plan(role: &str, replacement: Option<(&str, &str)>) -> TrialPlan {
    TrialPlan::new(TASKS.map(|task| {
        let event_id = replacement
            .as_ref()
            .filter(|(replaced, _)| *replaced == task)
            .map_or_else(
                || format!("result:{role}-{task}"),
                |(_, id)| (*id).to_owned(),
            );
        (task.to_owned(), event_id)
    }))
    .unwrap()
}

fn make_fixture(directory: &TempDir) -> Fixture {
    make_fixture_with_confidence(directory, 9_500)
}

fn make_fixture_with_confidence(directory: &TempDir, confidence_bps: u16) -> Fixture {
    let evaluator_path = directory.path().join("hephaestus-evaluator");
    fs::copy(env!("CARGO_BIN_EXE_hephaestus-evaluator"), &evaluator_path).unwrap();
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700)).unwrap();
    make_fixture_with_evaluator_and_confidence(directory, evaluator_path, confidence_bps)
}

#[allow(clippy::too_many_lines)]
fn make_fixture_with_evaluator(directory: &TempDir, evaluator_path: PathBuf) -> Fixture {
    make_fixture_with_evaluator_and_confidence(directory, evaluator_path, 9_500)
}

#[allow(clippy::too_many_lines)]
fn make_fixture_with_evaluator_and_confidence(
    directory: &TempDir,
    evaluator_path: PathBuf,
    confidence_bps: u16,
) -> Fixture {
    make_fixture_with_options(
        directory,
        evaluator_path,
        confidence_bps,
        None,
        &BTreeMap::new(),
    )
}

#[allow(clippy::too_many_lines)]
fn make_fixture_with_options(
    directory: &TempDir,
    evaluator_path: PathBuf,
    confidence_bps: u16,
    invariant_manifest: Option<&[u8]>,
    output_overrides: &BTreeMap<String, (RunCompletionReason, Vec<u8>)>,
) -> Fixture {
    let mut stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let (visible, sealed) = manifests();
    let visible_id = stores
        .artifacts
        .put(&serde_json::to_vec(&visible).unwrap())
        .unwrap();
    let sealed_id = stores
        .artifacts
        .put(&serde_json::to_vec(&sealed).unwrap())
        .unwrap();
    let evaluator_id = stores
        .artifacts
        .put(&fs::read(&evaluator_path).unwrap())
        .unwrap();
    let evaluator = IsolatedEvaluator::open_with_policy(
        directory.path().join("evaluator-runs"),
        evaluator_path,
        evaluator_id.as_str(),
        IsolationPolicy::unconfined_for_testing(),
        WorkerLimits::new(Duration::from_secs(5), 16 * 1024 * 1024, 64 * 1024).unwrap(),
    )
    .unwrap();
    let signer = RunResultSigner::from_seed([7; 32]);
    let verifier_id = stores
        .artifacts
        .put(&signer.verifier().public_key_bytes())
        .unwrap();
    let invariant_manifest_id =
        invariant_manifest.map(|bytes| stores.artifacts.put(bytes).unwrap().as_str().to_owned());
    let mut evaluator_artifacts = serde_json::Map::from_iter([
        (
            "arena.visible_manifest".to_owned(),
            serde_json::Value::String(visible_id.as_str().to_owned()),
        ),
        (
            "arena.sealed_manifest".to_owned(),
            serde_json::Value::String(sealed_id.as_str().to_owned()),
        ),
        (
            "arena.evaluator".to_owned(),
            serde_json::Value::String(evaluator_id.as_str().to_owned()),
        ),
        (
            "arena.runtime_verifier".to_owned(),
            serde_json::Value::String(verifier_id.as_str().to_owned()),
        ),
    ]);
    if let Some(invariant_manifest_id) = invariant_manifest_id {
        evaluator_artifacts.insert(
            "arena.invariant_manifest".to_owned(),
            serde_json::Value::String(invariant_manifest_id),
        );
    }
    let source = serde_json::json!({
        "schema_version": 1,
        "name": "arena-world",
        "laws": {
            "candidate_network": false,
            "candidate_evaluator_access": false,
            "maximum_cost_microusd": 100
        },
        "authority_ceiling": {"workspace_write": false, "network": false},
        "mutation_scope": ["harness"],
        "promotion": {
            "minimum_delta_bps": 1,
            "maximum_regressions": 0,
            "confidence_bps": confidence_bps
        },
        "objectives": ["correctness"],
        "evaluator_artifacts": evaluator_artifacts
    })
    .to_string();
    let world = compile_world(&source, SourceFormat::Json, &stores.artifacts).unwrap();
    let parent_genome = compile_genome(
        r#"{"schema_version":1,"name":"parent","parents":[],"model":{"provider":"deterministic","family":"v1"},"authority":{"workspace_write":false,"network":false},"artifacts":{}}"#,
        SourceFormat::Json,
        &world,
        &BTreeMap::new(),
        &stores.artifacts,
    )
    .unwrap();
    let parents = BTreeMap::from([(parent_genome.id().to_owned(), parent_genome.clone())]);
    let candidate_source = format!(
        r#"{{"schema_version":1,"name":"candidate","parents":["{}"],"model":{{"provider":"deterministic","family":"v2"}},"authority":{{"workspace_write":false,"network":false}},"artifacts":{{}}}}"#,
        parent_genome.id()
    );
    let candidate_genome = compile_genome(
        &candidate_source,
        SourceFormat::Json,
        &world,
        &parents,
        &stores.artifacts,
    )
    .unwrap();
    let repository = directory.path().join("source");
    let (revision, alternate_revision) = repository_fixture(&repository);
    let outputs = [
        ("task-visible-a", VISIBLE_SECRET, VISIBLE_SECRET),
        ("task-visible-b", "wrong", "B"),
        ("task-sealed-a", SEALED_SECRET, "wrong"),
        ("task-sealed-b", "Z", "Z"),
    ];
    for (task, parent_output, candidate_output) in outputs {
        let parent_default = (
            RunCompletionReason::Success,
            parent_output.as_bytes().to_vec(),
        );
        let candidate_default = (
            RunCompletionReason::Success,
            candidate_output.as_bytes().to_vec(),
        );
        let (parent_reason, parent_bytes) = output_overrides
            .get(&format!("parent-{task}"))
            .unwrap_or(&parent_default);
        let (candidate_reason, candidate_bytes) = output_overrides
            .get(&format!("candidate-{task}"))
            .unwrap_or(&candidate_default);
        append_run(
            &mut stores,
            &signer,
            &repository,
            &format!("parent-{task}"),
            parent_genome.id(),
            world.id(),
            &revision,
            task,
            task_input(task),
            *parent_reason,
            parent_bytes,
        );
        append_run(
            &mut stores,
            &signer,
            &repository,
            &format!("candidate-{task}"),
            candidate_genome.id(),
            world.id(),
            &revision,
            task,
            task_input(task),
            *candidate_reason,
            candidate_bytes,
        );
    }
    let binding = EvaluationBinding::new(
        world.id(),
        42,
        "environment-v1",
        evaluator_id.as_str(),
        budget_receipt(),
    )
    .unwrap();
    Fixture {
        stores,
        world,
        binding,
        visible,
        sealed,
        parent_genome_id: parent_genome.id().to_owned(),
        candidate_genome_id: candidate_genome.id().to_owned(),
        parent: plan("parent", None),
        candidate: plan("candidate", None),
        evaluator,
        signer,
        repository,
        revision,
        alternate_revision,
    }
}

fn make_invariant_fixture(directory: &TempDir) -> Fixture {
    make_invariant_fixture_with_manifest(
        directory,
        br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4,"forbidden_ascii_bytes":[0,33]}"#,
    )
}

fn make_invariant_fixture_with_manifest(directory: &TempDir, manifest: &[u8]) -> Fixture {
    let evaluator_path = directory.path().join("hephaestus-evaluator");
    fs::copy(env!("CARGO_BIN_EXE_hephaestus-evaluator"), &evaluator_path).unwrap();
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700)).unwrap();
    let overrides = BTreeMap::from([
        (
            "parent-task-visible-a".to_owned(),
            (RunCompletionReason::Success, b"a\0".to_vec()),
        ),
        (
            "parent-task-visible-b".to_owned(),
            (RunCompletionReason::Success, b"abcdef".to_vec()),
        ),
        (
            "candidate-task-visible-a".to_owned(),
            (RunCompletionReason::Success, b"a\0".to_vec()),
        ),
        (
            "candidate-task-visible-b".to_owned(),
            (RunCompletionReason::Success, "éé".as_bytes().to_vec()),
        ),
        (
            "parent-task-sealed-a".to_owned(),
            (RunCompletionReason::ProviderFailure, b"bad".to_vec()),
        ),
        (
            "candidate-task-sealed-a".to_owned(),
            (RunCompletionReason::Success, b"yes".to_vec()),
        ),
        (
            "candidate-task-sealed-b".to_owned(),
            (RunCompletionReason::IoFailure, b"bad\0long".to_vec()),
        ),
    ]);
    make_fixture_with_options(directory, evaluator_path, 9_500, Some(manifest), &overrides)
}

fn budget_receipt() -> RunBudgetReceipt {
    RunBudgetReceipt {
        wall_millis: 10_000,
        maximum_output_bytes: 1_048_576,
        maximum_cost_microusd: 100,
    }
}

fn task_input(task_id: &str) -> &'static str {
    match task_id {
        "task-visible-a" => "input visible a",
        "task-visible-b" => "input visible b",
        "task-sealed-a" => "sealed prompt 204",
        "task-sealed-b" => "sealed prompt 517",
        _ => panic!("unknown task fixture: {task_id}"),
    }
}

fn repository_fixture(path: &Path) -> (String, String) {
    fs::create_dir(path).unwrap();
    run_git(path, &["init", "-q"]);
    fs::write(path.join("fixture"), b"first\n").unwrap();
    run_git(path, &["add", "fixture"]);
    commit(path, "first");
    let first = git_stdout(path, &["rev-parse", "HEAD"]);
    fs::write(path.join("fixture"), b"second\n").unwrap();
    run_git(path, &["add", "fixture"]);
    commit(path, "second");
    let second = git_stdout(path, &["rev-parse", "HEAD"]);
    (first, second)
}

fn commit(path: &Path, message: &str) {
    run_git(
        path,
        &[
            "-c",
            "user.name=Hephaestus Tests",
            "-c",
            "user.email=hephaestus@example.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
}

fn run_git(path: &Path, arguments: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(path)
            .args(arguments)
            .status()
            .unwrap()
            .success()
    );
}

fn git_stdout(path: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn artifact_file_count(directory: &TempDir) -> usize {
    fs::read_dir(directory.path().join("blobs"))
        .unwrap()
        .map(|entry| fs::read_dir(entry.unwrap().path()).unwrap().count())
        .sum()
}

fn evaluate(fixture: Fixture) -> Result<OperatorEvaluation, ArenaError> {
    evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    )
}

#[test]
#[allow(clippy::too_many_lines)]
fn reference_output_invariants_record_operator_aggregates_and_replay() {
    let directory = TempDir::new().unwrap();
    let fixture = make_invariant_fixture(&directory);
    let world = fixture.world.clone();
    let evaluation = evaluate(fixture).unwrap();
    let stores = evaluation.into_stores();
    let check =
        check_reference_output_invariants(stores, "evaluation-001", &world, 1_788_000_123_500)
            .unwrap();

    assert_eq!(
        check.event().event_id,
        "arena:invariants:evaluation-001:checked"
    );
    assert_eq!(
        check.event().aggregate_id,
        "arena:invariants:evaluation-001"
    );
    assert_eq!(check.event().event_type, "invariants.recorded");
    assert_eq!(check.event().actor, "arena-plane");
    let receipt = serde_json::to_value(check.receipt()).unwrap();
    assert_eq!(receipt["algorithm"], "reference-output-invariants-v1");
    assert_eq!(
        receipt["evaluation_event_id"],
        "arena:evaluation:evaluation-001:recorded"
    );
    assert_eq!(receipt["world_id"], world.id());
    assert_eq!(receipt["total_evaluated_trials"], 4);
    assert_eq!(receipt["total_checks"], 32);
    assert_eq!(receipt["maximum_regressions"], 0);
    assert_eq!(receipt["total_paired_regressions"], 3);
    assert_eq!(receipt["regressions_within_budget"], false);
    assert_eq!(receipt["total_candidate_violations"], 4);
    assert_eq!(receipt["candidate_contract_satisfied"], false);
    assert!(receipt["manifest_artifact_id"].as_str().is_some());
    assert!(receipt["parent_submission_artifact_id"].as_str().is_some());
    assert!(
        receipt["candidate_submission_artifact_id"]
            .as_str()
            .is_some()
    );
    let receipt_bytes = serde_json::to_vec(&receipt).unwrap();
    for secret in [
        "task-visible-a",
        "task-visible-b",
        "task-sealed-a",
        "task-sealed-b",
        "bad\\u0000long",
        "abcdef",
    ] {
        assert!(
            !receipt_bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }
    assert_eq!(
        receipt["predicates"],
        serde_json::json!([
            {
                "predicate": "successful_terminal",
                "forbidden_ascii_byte": null,
                "parent_violations": 1,
                "candidate_violations": 1,
                "paired_regressions": 1
            },
            {
                "predicate": "maximum_output_bytes",
                "forbidden_ascii_byte": null,
                "parent_violations": 1,
                "candidate_violations": 1,
                "paired_regressions": 1
            },
            {
                "predicate": "forbidden_ascii_byte",
                "forbidden_ascii_byte": 0,
                "parent_violations": 1,
                "candidate_violations": 2,
                "paired_regressions": 1
            },
            {
                "predicate": "forbidden_ascii_byte",
                "forbidden_ascii_byte": 33,
                "parent_violations": 0,
                "candidate_violations": 0,
                "paired_regressions": 0
            }
        ])
    );

    let expected_receipt = check.receipt().clone();
    let expected_event = check.event().clone();
    let stores = check.into_stores();
    let retry =
        check_reference_output_invariants(stores, "evaluation-001", &world, 1_788_000_123_501)
            .unwrap();
    assert_eq!(retry.receipt(), &expected_receipt);
    assert_eq!(retry.event(), &expected_event);
    let stores = retry.into_stores();
    let event = stores
        .events
        .replay_verified()
        .unwrap()
        .into_iter()
        .find(|event| event.event_id == "arena:invariants:evaluation-001:checked")
        .unwrap();
    for task_id in ["task-visible-a", "task-sealed-a"] {
        assert!(
            !event
                .payload
                .windows(task_id.len())
                .any(|window| window == task_id.as_bytes())
        );
    }
    let verified = verify_reference_output_invariant_event(stores, &event, &world).unwrap();
    assert_eq!(verified.receipt(), &expected_receipt);
    let mut stores = verified.into_stores();
    let spoofed_event = stores
        .events
        .append(EventInput::new(
            "arena:invariants:evaluation-001:spoofed",
            "arena:invariants:evaluation-001",
            "invariants.recorded",
            "untrusted-actor",
            1_788_000_123_502,
            &event.payload,
        ))
        .unwrap();
    assert!(matches!(
        verify_reference_output_invariant_event(stores, &spoofed_event, &world),
        Err(ArenaError::InvalidInvariantEvent)
    ));

    let mut stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let wrong_type_event = stores
        .events
        .append(EventInput::new(
            "arena:invariants:evaluation-001:wrong-type",
            "arena:invariants:evaluation-001",
            "unrelated.recorded",
            "arena-plane",
            1_788_000_123_503,
            &event.payload,
        ))
        .unwrap();
    assert!(matches!(
        verify_reference_output_invariant_event(stores, &wrong_type_event, &world),
        Err(ArenaError::InvalidInvariantEvent)
    ));

    let mut wrong_actor_event = event.clone();
    wrong_actor_event.actor = "untrusted-actor".to_owned();
    assert!(matches!(
        invariant_event_references(&wrong_actor_event),
        Err(ArenaError::InvalidInvariantEvent)
    ));

    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let mut altered_snapshot = event.clone();
    altered_snapshot.timestamp_millis += 1;
    assert!(matches!(
        verify_reference_output_invariant_event(stores, &altered_snapshot, &world),
        Err(ArenaError::InvalidInvariantEvent)
    ));

    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let reloaded = load_reference_output_invariants(stores, "evaluation-001", &world).unwrap();
    assert_eq!(reloaded.receipt(), &expected_receipt);
    assert_eq!(reloaded.event(), &expected_event);
    drop(reloaded);

    let artifact_id = ArtifactId::parse(expected_event.receipt_artifact_id.clone()).unwrap();
    let artifact_path = directory
        .path()
        .join("blobs")
        .join(&artifact_id.as_str()[..2])
        .join(artifact_id.as_str());
    fs::write(artifact_path, b"tampered invariant receipt").unwrap();
    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    assert!(load_reference_output_invariants(stores, "evaluation-001", &world).is_err());
}

fn append_history_except(
    stores: &mut EvaluationStores,
    history: &[StoredEvent],
    skip_event_id: &str,
) {
    for event in history
        .iter()
        .filter(|event| event.event_id != skip_event_id)
    {
        stores
            .events
            .append(EventInput::new(
                event.event_id.clone(),
                event.aggregate_id.clone(),
                event.event_type.clone(),
                event.actor.clone(),
                event.timestamp_millis,
                &event.payload,
            ))
            .unwrap();
    }
}

/// Rebuilds `history` in a new hash-valid ledger with the invariant event's
/// payload rewritten, so only content verification can reject it.
fn forged_invariant_ledger(
    directory: &TempDir,
    name: &str,
    history: &[StoredEvent],
    original: &StoredEvent,
    payload: String,
) -> EvaluationStores {
    let mut stores = EvaluationStores::open(
        directory.path().join(format!("{name}.sqlite3")),
        directory.path().join("blobs"),
    )
    .unwrap();
    append_history_except(&mut stores, history, &original.event_id);
    stores
        .events
        .append(EventInput::new(
            original.event_id.clone(),
            original.aggregate_id.clone(),
            original.event_type.clone(),
            original.actor.clone(),
            original.timestamp_millis,
            payload,
        ))
        .unwrap();
    stores
}

#[test]
#[allow(clippy::too_many_lines)]
fn reference_output_invariants_reject_cross_world_and_hash_valid_forgeries() {
    let directory = TempDir::new().unwrap();
    let fixture = make_invariant_fixture(&directory);
    let world = fixture.world.clone();
    let mut wrong_source: serde_json::Value =
        serde_json::from_slice(world.canonical_json()).unwrap();
    wrong_source["name"] = serde_json::json!("different-invariant-world");
    let wrong_world = compile_world(
        &wrong_source.to_string(),
        SourceFormat::Json,
        &fixture.stores.artifacts,
    )
    .unwrap();
    let stores = evaluate(fixture).unwrap().into_stores();
    assert!(matches!(
        check_reference_output_invariants(
            stores,
            "evaluation-001",
            &wrong_world,
            1_788_000_123_500
        ),
        Err(ArenaError::WorldArtifactMismatch(
            "arena.invariant_manifest"
        ))
    ));

    let open = || {
        EvaluationStores::open(
            directory.path().join("events.sqlite3"),
            directory.path().join("blobs"),
        )
        .unwrap()
    };
    let check =
        check_reference_output_invariants(open(), "evaluation-001", &world, 1_788_000_123_500)
            .unwrap();
    let receipt_artifact = check.event().receipt_artifact_id.clone();
    let receipt = check.receipt().clone();
    let history = check.into_stores().events.replay_verified().unwrap();
    let original = history
        .iter()
        .find(|event| event.event_id == "arena:invariants:evaluation-001:checked")
        .unwrap()
        .clone();
    assert!(matches!(
        verify_reference_output_invariant_event(open(), &original, &wrong_world),
        Err(ArenaError::WorldArtifactMismatch(
            "arena.invariant_manifest"
        ))
    ));

    // A hash-valid receipt artifact with different aggregates is a conflict.
    let mut forged_receipt = receipt.clone();
    forged_receipt.total_checks += 1;
    let forged_bytes = serde_json::to_vec(&forged_receipt).unwrap();
    let forged_artifact = open().artifacts.put(&forged_bytes).unwrap();
    let forged_payload = String::from_utf8(original.payload.clone())
        .unwrap()
        .replace(&receipt_artifact, forged_artifact.as_str());
    let stores = forged_invariant_ledger(
        &directory,
        "forged-receipt",
        &history,
        &original,
        forged_payload,
    );
    assert!(matches!(
        load_reference_output_invariants(stores, "evaluation-001", &world),
        Err(ArenaError::InvariantConflict(evaluation)) if evaluation == "evaluation-001"
    ));

    // An envelope naming another World cannot rebind the recorded evaluation.
    let foreign_payload = String::from_utf8(original.payload.clone())
        .unwrap()
        .replace(world.id(), wrong_world.id());
    let stores = forged_invariant_ledger(
        &directory,
        "foreign-world",
        &history,
        &original,
        foreign_payload,
    );
    assert!(matches!(
        load_reference_output_invariants(stores, "evaluation-001", &world),
        Err(ArenaError::InvariantConflict(evaluation)) if evaluation == "evaluation-001"
    ));

    // An invariant event recorded before its source evaluation is a conflict.
    let evaluation_index = history
        .iter()
        .position(|event| event.event_id == "arena:evaluation:evaluation-001:recorded")
        .unwrap();
    let mut reordered = EvaluationStores::open(
        directory.path().join("reordered.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let mut order: Vec<&StoredEvent> = history[..evaluation_index].iter().collect();
    order.push(&original);
    order.extend(
        history[evaluation_index..]
            .iter()
            .filter(|event| event.event_id != original.event_id),
    );
    for event in order {
        reordered
            .events
            .append(EventInput::new(
                event.event_id.clone(),
                event.aggregate_id.clone(),
                event.event_type.clone(),
                event.actor.clone(),
                event.timestamp_millis,
                &event.payload,
            ))
            .unwrap();
    }
    assert!(load_reference_output_invariants(reordered, "evaluation-001", &world).is_err());
}

/// Rebuilds `history` in a new hash-valid ledger with `original`'s payload
/// rewritten, so only deeper content verification (not the hash chain) can
/// reject the forgery. Generic over which event is being replaced.
fn forged_event_ledger(
    directory: &TempDir,
    name: &str,
    history: &[StoredEvent],
    original: &StoredEvent,
    payload: String,
) -> EvaluationStores {
    let mut stores = EvaluationStores::open(
        directory.path().join(format!("{name}.sqlite3")),
        directory.path().join("blobs"),
    )
    .unwrap();
    append_history_except(&mut stores, history, &original.event_id);
    stores
        .events
        .append(EventInput::new(
            original.event_id.clone(),
            original.aggregate_id.clone(),
            original.event_type.clone(),
            original.actor.clone(),
            original.timestamp_millis,
            payload,
        ))
        .unwrap();
    stores
}

#[test]
fn reference_output_invariants_reject_out_of_manifest_tasks_and_relabeled_signed_outputs() {
    let directory = TempDir::new().unwrap();
    let fixture = make_invariant_fixture(&directory);
    let world = fixture.world.clone();
    let evaluation = evaluate(fixture).unwrap();
    let parent_submission_bytes = evaluation.operator_parent_submission().unwrap();
    let parent_submission_id = ArtifactId::for_bytes(&parent_submission_bytes)
        .as_str()
        .to_owned();
    let submission_text = String::from_utf8(parent_submission_bytes.clone()).unwrap();
    let stores = evaluation.into_stores();
    let history = stores.events.replay_verified().unwrap();
    let evaluation_event = history
        .iter()
        .find(|event| event.event_id == "arena:evaluation:evaluation-001:recorded")
        .unwrap()
        .clone();
    let original_payload = String::from_utf8(evaluation_event.payload.clone()).unwrap();
    // The evaluation event's payload names the parent submission artifact twice
    // (a business identity and a CAS pointer, kept equal by construction), so a
    // single unqualified replace retargets both consistently.
    assert_eq!(original_payload.matches(&parent_submission_id).count(), 2);

    // A submission whose trial keys diverge from the World's manifest task set,
    // with every referenced signed-run binding otherwise untouched, is rejected
    // even though it is byte-canonical and every artifact it names exists.
    let renamed_task_text = submission_text.replace("\"task-visible-b\":", "\"task-visible-x\":");
    assert_ne!(renamed_task_text, submission_text);
    let renamed_task_id = stores.artifacts.put(renamed_task_text.as_bytes()).unwrap();
    let renamed_task_payload =
        original_payload.replace(&parent_submission_id, renamed_task_id.as_str());
    let renamed_task_stores = forged_event_ledger(
        &directory,
        "renamed-task",
        &history,
        &evaluation_event,
        renamed_task_payload,
    );
    assert!(matches!(
        check_reference_output_invariants(
            renamed_task_stores,
            "evaluation-001",
            &world,
            1_788_000_123_600,
        ),
        Err(ArenaError::InvalidStoredReceipt(
            "invariant submission evidence"
        ))
    ));

    // A submission that relabels one task's signed stdout onto another task's
    // trial: every artifact it names still exists and resolves, but no longer
    // matches what that task's own signed run actually produced.
    let submission_value: serde_json::Value =
        serde_json::from_slice(&parent_submission_bytes).unwrap();
    let stdout_a = submission_value["trials"]["task-visible-a"]["stdout_artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let stdout_b = submission_value["trials"]["task-visible-b"]["stdout_artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(stdout_a, stdout_b);
    let relabeled_text = submission_text.replacen(&stdout_b, &stdout_a, 1);
    assert_ne!(relabeled_text, submission_text);
    let relabeled_id = stores.artifacts.put(relabeled_text.as_bytes()).unwrap();
    let relabeled_payload = original_payload.replace(&parent_submission_id, relabeled_id.as_str());
    let relabeled_stores = forged_event_ledger(
        &directory,
        "relabeled-output",
        &history,
        &evaluation_event,
        relabeled_payload,
    );
    assert!(matches!(
        check_reference_output_invariants(
            relabeled_stores,
            "evaluation-001",
            &world,
            1_788_000_123_601,
        ),
        Err(ArenaError::InvalidStoredReceipt(
            "invariant signed run binding"
        ))
    ));
}

#[test]
fn reference_output_invariant_manifest_is_required_canonical_bounded_and_ordered() {
    let invalid_manifests: &[&[u8]] = &[
        br#"{ "schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4,"forbidden_ascii_bytes":[0]}"#,
        br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4,"forbidden_ascii_bytes":[0,0]}"#,
        br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4,"forbidden_ascii_bytes":[33,0]}"#,
        br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":0,"forbidden_ascii_bytes":[0]}"#,
        br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4097,"forbidden_ascii_bytes":[0]}"#,
        br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4,"forbidden_ascii_bytes":[128]}"#,
    ];
    for manifest in invalid_manifests {
        let directory = TempDir::new().unwrap();
        let fixture = make_invariant_fixture_with_manifest(&directory, manifest);
        let world = fixture.world.clone();
        let stores = evaluate(fixture).unwrap().into_stores();
        let error =
            check_reference_output_invariants(stores, "evaluation-001", &world, 1_788_000_123_500)
                .err()
                .expect("invalid World invariant manifest must fail closed");
        assert!(matches!(
            error,
            ArenaError::InvalidStoredReceipt("invariant manifest")
        ));
    }

    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let world = fixture.world.clone();
    let stores = evaluate(fixture).unwrap().into_stores();
    assert!(matches!(
        check_reference_output_invariants(stores, "evaluation-001", &world, 1_788_000_123_500),
        Err(ArenaError::MissingWorldArtifact("arena.invariant_manifest"))
    ));
}

fn assert_selection_receipt_getters_match_json(receipt: &SelectionReceipt, world_id: &str) {
    let encoded = serde_json::to_value(receipt).unwrap();
    assert_eq!(receipt.schema_version(), encoded["schema_version"]);
    assert_eq!(receipt.algorithm(), encoded["algorithm"]);
    assert_eq!(receipt.resamples(), encoded["resamples"]);
    assert_eq!(receipt.seed(), encoded["seed"]);
    assert_eq!(receipt.evaluation_id(), encoded["evaluation_id"]);
    assert_eq!(
        receipt.evaluation_event_id(),
        encoded["evaluation_event_id"]
    );
    assert_eq!(
        receipt.evaluation_event_hash(),
        encoded["evaluation_event_hash"]
    );
    assert_eq!(receipt.world_id(), world_id);
    assert_eq!(receipt.parent_genome_id(), encoded["parent_genome_id"]);
    assert_eq!(
        receipt.candidate_genome_id(),
        encoded["candidate_genome_id"]
    );
    assert_eq!(receipt.minimum_delta_bps(), encoded["minimum_delta_bps"]);
    assert_eq!(
        receipt.maximum_regressions(),
        encoded["maximum_regressions"]
    );
    assert_eq!(receipt.confidence_bps(), encoded["confidence_bps"]);
    assert_eq!(
        receipt.maximum_cost_microusd(),
        encoded["maximum_cost_microusd"]
    );
    assert_eq!(receipt.metrics_eligible(), encoded["metrics_eligible"]);
    assert_eq!(
        receipt.invariant_gate_verified(),
        encoded["invariant_gate_verified"]
    );
    assert_eq!(receipt.promotion_eligible(), encoded["promotion_eligible"]);
    assert_eq!(
        receipt.correctness_regressions(),
        encoded["correctness_regressions"]
    );
    assert_eq!(
        receipt.correctness_unchanged(),
        encoded["correctness_unchanged"]
    );
    assert_eq!(
        receipt.correctness_improvements(),
        encoded["correctness_improvements"]
    );
    assert_eq!(receipt.estimate_bps(), encoded["estimate_bps"]);
    assert_eq!(receipt.lower_bps(), encoded["lower_bps"]);
    assert_eq!(receipt.upper_bps(), encoded["upper_bps"]);
    assert_eq!(
        receipt.parent_correctness_bps(),
        encoded["parent_correctness_bps"]
    );
    assert_eq!(
        receipt.candidate_correctness_bps(),
        encoded["candidate_correctness_bps"]
    );
    assert_eq!(
        receipt.parent_reliability_bps(),
        encoded["parent_reliability_bps"]
    );
    assert_eq!(
        receipt.candidate_reliability_bps(),
        encoded["candidate_reliability_bps"]
    );
    assert_eq!(
        receipt.parent_cost_microusd(),
        encoded["parent_cost_microusd"]
    );
    assert_eq!(
        receipt.candidate_cost_microusd(),
        encoded["candidate_cost_microusd"]
    );
    assert_eq!(
        receipt.parent_latency_millis(),
        encoded["parent_latency_millis"]
    );
    assert_eq!(
        receipt.candidate_latency_millis(),
        encoded["candidate_latency_millis"]
    );
    assert_eq!(
        receipt.candidate_pareto_dominates(),
        encoded["candidate_pareto_dominates"]
    );
}

fn append_non_selection_history(stores: &mut EvaluationStores, history: &[StoredEvent]) {
    for event in history
        .iter()
        .filter(|event| event.event_type != "selection.recorded")
    {
        stores
            .events
            .append(EventInput::new(
                event.event_id.clone(),
                event.aggregate_id.clone(),
                event.event_type.clone(),
                event.actor.clone(),
                event.timestamp_millis,
                &event.payload,
            ))
            .unwrap();
    }
}

#[test]
fn records_world_bound_runtime_derived_safe_evaluation() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    assert_eq!(fixture.binding.world_id(), fixture.world.id());
    assert_eq!(fixture.binding.seed(), 42);
    assert_eq!(fixture.binding.environment_id(), "environment-v1");
    assert_eq!(fixture.binding.budget(), budget_receipt());
    assert_eq!(
        fixture.binding.evaluator_id(),
        fixture.world.evaluator_artifact("arena.evaluator").unwrap()
    );
    assert!(matches!(
        fixture.sealed.candidate_tasks(),
        Err(ArenaError::VisibilityMismatch)
    ));
    let expected = (
        fixture.parent_genome_id.clone(),
        fixture.candidate_genome_id.clone(),
    );
    let operator = evaluate(fixture).unwrap();
    let recorded = operator.candidate_result();
    assert_eq!(recorded.summary.parent_genome_id, expected.0);
    assert_eq!(recorded.summary.candidate_genome_id, expected.1);
    assert_eq!(recorded.summary.parent_visible_correct, 1);
    assert_eq!(recorded.summary.candidate_visible_correct, 2);
    assert_eq!(recorded.summary.visible_total, 2);
    assert_eq!(recorded.event.actor, "arena-plane");
    assert_eq!(recorded.event.event_type, "evaluation.recorded");
    assert_eq!(operator.operator_scores().parent_visible_correct, 1);
    assert_eq!(operator.operator_scores().candidate_visible_correct, 2);
    assert_eq!(operator.operator_scores().parent_sealed_correct, 2);
    assert_eq!(operator.operator_scores().candidate_sealed_correct, 1);
    let public = serde_json::to_string(&recorded.summary).unwrap();
    for forbidden in ["sealed", "artifact", "source_revision", SEALED_SECRET] {
        assert!(!public.contains(forbidden));
    }
    let summary_value = serde_json::from_str::<serde_json::Value>(&public).unwrap();
    let candidate_debug = format!("{recorded:?}");
    for forbidden in [SEALED_SECRET, "parent_sealed", "candidate_sealed", "hash:"] {
        assert!(!candidate_debug.contains(forbidden));
    }
    assert!(
        directory
            .path()
            .join("evaluator-runs")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
    let keys = summary_value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from([
            "candidate_genome_id",
            "candidate_visible_correct",
            "evaluation_id",
            "parent_genome_id",
            "parent_visible_correct",
            "schema_version",
            "visible_total",
            "world_id",
        ])
    );
    assert!(
        String::from_utf8(operator.operator_parent_submission().unwrap())
            .unwrap()
            .contains("result:parent-task-sealed-a")
    );
    assert!(operator.operator_visible_inputs().unwrap().len() > 1);
    let candidate_result = operator.into_candidate_result();
    let candidate_debug = format!("{candidate_result:?}");
    for forbidden in ["OperatorReceipt", "EvaluationStores", SEALED_SECRET] {
        assert!(!candidate_debug.contains(forbidden));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn operator_selection_evidence_retains_only_authenticated_aggregates() {
    let directory = TempDir::new().unwrap();
    let operator = evaluate(make_fixture(&directory)).unwrap();
    let evidence = operator.selection_evidence();

    assert_eq!(evidence.schema_version(), 1);
    assert_eq!(evidence.evaluation_id(), "evaluation-001");
    assert_eq!(
        evidence.evaluation_event_id(),
        "arena:evaluation:evaluation-001:recorded"
    );
    assert!(evidence.world_id().starts_with("hephaestus:world:"));
    assert_eq!(evidence.seed(), 42);
    assert_eq!(evidence.environment_id(), "environment-v1");
    assert_eq!(evidence.evaluator_id().len(), 64);
    assert_eq!(evidence.budget(), budget_receipt());
    assert!(
        evidence
            .parent_genome_id()
            .starts_with("hephaestus:genome:")
    );
    assert!(
        evidence
            .candidate_genome_id()
            .starts_with("hephaestus:genome:")
    );
    assert_eq!(evidence.visible_total(), 2);
    assert_eq!(evidence.sealed_total(), 2);
    assert_eq!(evidence.correctness_outcomes().regressions(), 1);
    assert_eq!(evidence.correctness_outcomes().unchanged(), 2);
    assert_eq!(evidence.correctness_outcomes().improvements(), 1);
    assert_eq!(evidence.parent_fitness().correct_trials(), 3);
    assert_eq!(evidence.candidate_fitness().correct_trials(), 3);
    assert_eq!(evidence.parent_fitness().total_trials(), 4);
    assert_eq!(evidence.candidate_fitness().total_trials(), 4);
    assert_eq!(evidence.parent_fitness().reliable_trials(), 4);
    assert_eq!(evidence.candidate_fitness().reliable_trials(), 4);
    assert_eq!(evidence.parent_fitness().total_cost_microusd(), 0);
    assert_eq!(evidence.candidate_fitness().total_latency_millis(), 4);

    let serialized = serde_json::to_string(&evidence).unwrap();
    let value = serde_json::from_str::<serde_json::Value>(&serialized).unwrap();
    let keys = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from([
            "budget",
            "candidate_fitness",
            "candidate_genome_id",
            "candidate_sealed_correct",
            "candidate_visible_correct",
            "correctness_outcomes",
            "environment_id",
            "evaluation_event_hash",
            "evaluation_event_id",
            "evaluation_id",
            "evaluator_id",
            "parent_fitness",
            "parent_genome_id",
            "parent_sealed_correct",
            "parent_visible_correct",
            "schema_version",
            "sealed_total",
            "seed",
            "visible_total",
            "world_id",
        ])
    );
    for forbidden in [
        "task-visible-a",
        "task-sealed-a",
        "input visible a",
        "sealed prompt 204",
        VISIBLE_SECRET,
        SEALED_SECRET,
        "manifest_artifact_id",
        "submission_artifact_id",
        "trace_artifact_ids",
    ] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden}");
    }

    let stores = operator.into_stores();
    let event = stores
        .events
        .replay_verified()
        .unwrap()
        .into_iter()
        .find(|event| event.event_id == "arena:evaluation:evaluation-001:recorded")
        .unwrap();
    let mut expected_hash = String::with_capacity(64);
    for byte in event.hash {
        write!(&mut expected_hash, "{byte:02x}").unwrap();
    }
    assert_eq!(evidence.evaluation_event_hash(), expected_hash);
    assert!(
        evidence
            .evaluation_event_hash()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn terminal_failure_is_paired_as_unreliable_incorrect_with_cost_and_latency() {
    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let failed = append_run_with_observations(
        &mut fixture.stores,
        &fixture.signer,
        &fixture.repository,
        "candidate-failed-visible-a",
        &fixture.candidate_genome_id,
        fixture.world.id(),
        &fixture.revision,
        "task-visible-a",
        task_input("task-visible-a"),
        42,
        "environment-v1",
        Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap(),
        RunCompletionReason::OutputBudgetExceeded,
        VISIBLE_SECRET.as_bytes(),
        33,
        0,
    );
    fixture.candidate = plan("candidate", Some(("task-visible-a", &failed)));

    let operator = evaluate(fixture).unwrap();
    let evidence = operator.selection_evidence();
    assert_eq!(evidence.candidate_fitness().reliable_trials(), 3);
    assert_eq!(evidence.candidate_fitness().correct_trials(), 2);
    assert_eq!(evidence.candidate_fitness().total_cost_microusd(), 0);
    assert_eq!(evidence.candidate_fitness().total_latency_millis(), 36);
    assert_eq!(evidence.correctness_outcomes().regressions(), 2);
    assert_eq!(evidence.correctness_outcomes().unchanged(), 1);
    assert_eq!(evidence.correctness_outcomes().improvements(), 1);
}

#[test]
fn prepared_evaluator_scores_commit_only_for_the_same_canonical_pair() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let prepared = prepare_evaluation(
        &fixture.stores,
        &context(),
        &fixture.world,
        EvaluationSources {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
        },
    )
    .unwrap();
    let scored = prepared.score(&fixture.evaluator).unwrap();
    let operator = evaluate_and_record_scored(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
        scored,
    )
    .unwrap();
    assert_eq!(
        operator
            .candidate_result()
            .summary
            .candidate_visible_correct,
        2
    );
    assert_eq!(
        operator.candidate_result().summary.parent_visible_correct,
        1
    );
    drop(operator.into_stores());

    let second_directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&second_directory);
    let prepared = prepare_evaluation(
        &fixture.stores,
        &context(),
        &fixture.world,
        EvaluationSources {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
        },
    )
    .unwrap();
    let scored = prepared.score(&fixture.evaluator).unwrap();
    let alternate = append_run(
        &mut fixture.stores,
        &fixture.signer,
        &fixture.repository,
        "candidate-alternate-visible-a",
        &fixture.candidate_genome_id,
        fixture.world.id(),
        &fixture.revision,
        "task-visible-a",
        task_input("task-visible-a"),
        RunCompletionReason::Success,
        VISIBLE_SECRET.as_bytes(),
    );
    fixture.candidate = plan("candidate", Some(("task-visible-a", &alternate)));
    let result = evaluate_and_record_scored(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
        scored,
    );
    assert!(matches!(
        result,
        Err(ArenaError::BindingMismatch("prepared evaluation sources"))
    ));
}

#[test]
fn prepared_scoring_requires_the_registered_evaluator_process() {
    let directory = TempDir::new().unwrap();
    let evaluator_path = directory.path().join("failing-evaluator");
    fs::write(&evaluator_path, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = make_fixture_with_evaluator(&directory, evaluator_path);
    let prepared = prepare_evaluation(
        &fixture.stores,
        &context(),
        &fixture.world,
        EvaluationSources {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
        },
    )
    .unwrap();
    assert!(prepared.score(&fixture.evaluator).is_err());
}

#[test]
fn operator_selection_evidence_rehydrates_identically_after_restart() {
    let directory = TempDir::new().unwrap();
    let operator = evaluate(make_fixture(&directory)).unwrap();
    let expected = serde_json::to_vec(&operator.selection_evidence()).unwrap();
    drop(operator.into_stores());

    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let rehydrated = load_operator_evaluation(stores, "evaluation-001").unwrap();
    assert_eq!(
        serde_json::to_vec(&rehydrated.selection_evidence()).unwrap(),
        expected
    );
}

#[test]
fn selection_receipt_recomputes_and_retries_identically_after_restart() {
    let directory = TempDir::new().unwrap();
    let open_stores = || {
        EvaluationStores::open(
            directory.path().join("events.sqlite3"),
            directory.path().join("blobs"),
        )
        .unwrap()
    };
    let mut fixture = make_fixture(&directory);
    let world = fixture.world.clone();

    // Build authenticated paired outcomes with all candidates correct and
    // all parent trials incorrect, keeping reliability and cost equal.
    let expected_outputs = [
        ("task-visible-a", VISIBLE_SECRET),
        ("task-visible-b", "B"),
        ("task-sealed-a", SEALED_SECRET),
        ("task-sealed-b", "Z"),
    ];
    let mut parent_pairs = Vec::new();
    let mut candidate_pairs = Vec::new();
    for (task, expected) in expected_outputs {
        let parent_event = append_run(
            &mut fixture.stores,
            &fixture.signer,
            &fixture.repository,
            &format!("selection-parent-{task}"),
            &fixture.parent_genome_id,
            world.id(),
            &fixture.revision,
            task,
            task_input(task),
            RunCompletionReason::Success,
            b"deliberately incorrect parent",
        );
        let candidate_event = append_run(
            &mut fixture.stores,
            &fixture.signer,
            &fixture.repository,
            &format!("selection-candidate-{task}"),
            &fixture.candidate_genome_id,
            world.id(),
            &fixture.revision,
            task,
            task_input(task),
            RunCompletionReason::Success,
            expected.as_bytes(),
        );
        parent_pairs.push((task.to_owned(), parent_event));
        candidate_pairs.push((task.to_owned(), candidate_event));
    }
    fixture.parent = TrialPlan::new(parent_pairs).unwrap();
    fixture.candidate = TrialPlan::new(candidate_pairs).unwrap();
    let operator = evaluate(fixture).unwrap();
    let selected = select_and_record(
        operator.into_stores(),
        "evaluation-001",
        &world,
        1_788_000_123_999,
    )
    .expect("trusted persisted evaluation can be selected");
    let expected = serde_json::to_vec(selected.receipt()).unwrap();
    assert_selection_receipt_getters_match_json(selected.receipt(), world.id());
    assert!(selected.receipt().metrics_eligible());
    assert!(!selected.receipt().promotion_eligible());
    assert!(!selected.receipt().invariant_gate_verified());
    let event = selected.event().clone();
    drop(selected.into_stores());

    let stores = open_stores();
    let history = stores.events.replay_verified().unwrap();
    let stored_event = history
        .iter()
        .find(|event| event.event_type == "selection.recorded")
        .unwrap()
        .clone();
    let mut forged_snapshot = stored_event.clone();
    forged_snapshot.timestamp_millis += 1;
    let stores = open_stores();
    assert!(matches!(
        verify_selection_event(stores, &forged_snapshot, &world),
        Err(ArenaError::InvalidSelectionEvent)
    ));
    let stores = open_stores();
    let retried = verify_selection_event(stores, &stored_event, &world)
        .expect("startup verification rehydrates and recomputes the receipt");
    assert_eq!(serde_json::to_vec(retried.receipt()).unwrap(), expected);
    assert_eq!(retried.event(), &event);
    let stores = retried.into_stores();
    let retried = select_and_record(stores, "evaluation-001", &world, 1_788_000_124_000)
        .expect("identical operator retry returns the existing receipt");
    assert_eq!(
        retried
            .into_stores()
            .events
            .replay_verified()
            .unwrap()
            .iter()
            .filter(|event| event.event_type == "selection.recorded")
            .count(),
        1
    );
}

#[test]
fn marginally_slower_but_better_child_is_eligible_under_the_tolerant_dominance_rule() {
    // Regression test for the real product defect: a candidate that is
    // strictly better on correctness must not be randomly rejected because
    // it ran a few milliseconds slower on trivial reference tasks. The
    // tolerance is `max(10% of parent latency, 50ms * paired task count)`;
    // here parent total latency is 40ms (10ms * 4 tasks), so the tolerance
    // is `max(4, 200) = 200ms`, and the candidate is only 20ms slower.
    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let world = fixture.world.clone();
    let expected_outputs = [
        ("task-visible-a", VISIBLE_SECRET),
        ("task-visible-b", "B"),
        ("task-sealed-a", SEALED_SECRET),
        ("task-sealed-b", "Z"),
    ];
    let mut parent_pairs = Vec::new();
    let mut candidate_pairs = Vec::new();
    for (task, expected) in expected_outputs {
        let parent_event = append_run_with_observations(
            &mut fixture.stores,
            &fixture.signer,
            &fixture.repository,
            &format!("marginal-parent-{task}"),
            &fixture.parent_genome_id,
            world.id(),
            &fixture.revision,
            task,
            task_input(task),
            42,
            "environment-v1",
            Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap(),
            RunCompletionReason::Success,
            b"deliberately incorrect parent",
            10,
            0,
        );
        let candidate_event = append_run_with_observations(
            &mut fixture.stores,
            &fixture.signer,
            &fixture.repository,
            &format!("marginal-candidate-{task}"),
            &fixture.candidate_genome_id,
            world.id(),
            &fixture.revision,
            task,
            task_input(task),
            42,
            "environment-v1",
            Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap(),
            RunCompletionReason::Success,
            expected.as_bytes(),
            15,
            0,
        );
        parent_pairs.push((task.to_owned(), parent_event));
        candidate_pairs.push((task.to_owned(), candidate_event));
    }
    fixture.parent = TrialPlan::new(parent_pairs).unwrap();
    fixture.candidate = TrialPlan::new(candidate_pairs).unwrap();
    let operator = evaluate(fixture).unwrap();
    let selected = select_and_record(
        operator.into_stores(),
        "evaluation-001",
        &world,
        1_788_000_123_999,
    )
    .unwrap();
    let receipt = selected.receipt();
    assert_eq!(
        receipt.algorithm(),
        "histogram-bootstrap-pareto-tolerant-v2"
    );
    assert_eq!(receipt.parent_latency_millis(), 40);
    assert_eq!(receipt.candidate_latency_millis(), 60);
    assert!(receipt.candidate_correctness_bps() > receipt.parent_correctness_bps());
    assert!(receipt.candidate_pareto_dominates());
    assert!(receipt.metrics_eligible());
}

#[test]
#[allow(clippy::too_many_lines)]
fn stored_v1_selection_receipt_still_verifies_under_the_strict_dominance_rule() {
    // The same marginally-slower-but-better child as above, but this test
    // proves the replay/verification path: an already-recorded receipt
    // computed under the retired strict `histogram-bootstrap-v1` algorithm
    // (candidate latency must be no worse than the parent's, at all) must
    // still recompute byte-identically on replay, even though a fresh
    // selection would use the new tolerant algorithm and reach the opposite
    // eligibility verdict for the same measured evidence.
    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let world = fixture.world.clone();
    let expected_outputs = [
        ("task-visible-a", VISIBLE_SECRET),
        ("task-visible-b", "B"),
        ("task-sealed-a", SEALED_SECRET),
        ("task-sealed-b", "Z"),
    ];
    let mut parent_pairs = Vec::new();
    let mut candidate_pairs = Vec::new();
    for (task, expected) in expected_outputs {
        let parent_event = append_run_with_observations(
            &mut fixture.stores,
            &fixture.signer,
            &fixture.repository,
            &format!("v1replay-parent-{task}"),
            &fixture.parent_genome_id,
            world.id(),
            &fixture.revision,
            task,
            task_input(task),
            42,
            "environment-v1",
            Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap(),
            RunCompletionReason::Success,
            b"deliberately incorrect parent",
            10,
            0,
        );
        let candidate_event = append_run_with_observations(
            &mut fixture.stores,
            &fixture.signer,
            &fixture.repository,
            &format!("v1replay-candidate-{task}"),
            &fixture.candidate_genome_id,
            world.id(),
            &fixture.revision,
            task,
            task_input(task),
            42,
            "environment-v1",
            Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap(),
            RunCompletionReason::Success,
            expected.as_bytes(),
            15,
            0,
        );
        parent_pairs.push((task.to_owned(), parent_event));
        candidate_pairs.push((task.to_owned(), candidate_event));
    }
    fixture.parent = TrialPlan::new(parent_pairs).unwrap();
    fixture.candidate = TrialPlan::new(candidate_pairs).unwrap();
    let operator = evaluate(fixture).unwrap();
    let selected = select_and_record(
        operator.into_stores(),
        "evaluation-001",
        &world,
        1_788_000_123_999,
    )
    .unwrap();
    // Sanity check: the real (current-algorithm) receipt says eligible,
    // exactly like the test above.
    assert!(selected.receipt().metrics_eligible());
    let history = selected.into_stores().events.replay_verified().unwrap();
    let original = history
        .iter()
        .find(|event| event.event_type == "selection.recorded")
        .unwrap()
        .clone();
    let original_payload: serde_json::Value = serde_json::from_slice(&original.payload).unwrap();
    let original_artifact = original_payload["receipt_artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let original_artifact_id = ArtifactId::parse(original_artifact.clone()).unwrap();
    let artifact_root = directory.path().join("blobs");
    let original_bytes = fs::read(
        ArtifactStore::open(&artifact_root)
            .unwrap()
            .path_for(&original_artifact_id),
    )
    .unwrap();

    // Rewrite the receipt to exactly what v1's strict latency comparison
    // would have produced for this same measured evidence: latency-worse
    // means the candidate does not dominate, so it is not eligible.
    let mut v1_receipt: serde_json::Value = serde_json::from_slice(&original_bytes).unwrap();
    v1_receipt["algorithm"] = serde_json::json!("histogram-bootstrap-v1");
    v1_receipt["candidate_pareto_dominates"] = serde_json::json!(false);
    v1_receipt["metrics_eligible"] = serde_json::json!(false);
    let v1_receipt: SelectionReceipt = serde_json::from_value(v1_receipt).unwrap();
    let v1_bytes = serde_json::to_vec(&v1_receipt).unwrap();
    assert_ne!(v1_bytes, original_bytes);

    let mut artifacts =
        EvaluationStores::open(directory.path().join("v1.sqlite3"), &artifact_root).unwrap();
    let v1_artifact = artifacts.artifacts.put(&v1_bytes).unwrap();
    let v1_artifact = v1_artifact.as_str().to_owned();
    append_non_selection_history(&mut artifacts, &history);
    let v1_event_payload = String::from_utf8(original.payload.clone())
        .unwrap()
        .replace(&original_artifact, &v1_artifact);
    assert!(v1_event_payload.contains(&v1_artifact));
    artifacts
        .events
        .append(EventInput::new(
            original.event_id.clone(),
            original.aggregate_id.clone(),
            original.event_type.clone(),
            original.actor.clone(),
            original.timestamp_millis,
            v1_event_payload,
        ))
        .unwrap();

    let verified = load_selection(artifacts, "evaluation-001", &world)
        .expect("a receipt recorded under the retired v1 algorithm still verifies");
    assert_eq!(verified.receipt().algorithm(), "histogram-bootstrap-v1");
    assert!(!verified.receipt().candidate_pareto_dominates());
    assert!(!verified.receipt().metrics_eligible());
    assert_eq!(serde_json::to_vec(verified.receipt()).unwrap(), v1_bytes);
}

#[test]
fn selection_requires_the_evaluation_exact_compiled_world() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let world = fixture.world.clone();
    let mut wrong_source: serde_json::Value =
        serde_json::from_slice(world.canonical_json()).unwrap();
    wrong_source["name"] = serde_json::json!("different-arena-world");
    let wrong_world = compile_world(
        &wrong_source.to_string(),
        SourceFormat::Json,
        &fixture.stores.artifacts,
    )
    .unwrap();
    let operator = evaluate(fixture).unwrap();
    assert!(matches!(
        select_and_record(
            operator.into_stores(),
            "evaluation-001",
            &wrong_world,
            1_788_000_123_999
        ),
        Err(ArenaError::SelectionWorldMismatch)
    ));

    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let operator = load_operator_evaluation(stores, "evaluation-001").unwrap();
    let selected = select_and_record(
        operator.into_stores(),
        "evaluation-001",
        &world,
        1_788_000_123_999,
    )
    .unwrap();
    let stores = selected.into_stores();
    let stored_event = stores
        .events
        .replay_verified()
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == "selection.recorded")
        .unwrap();
    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    assert!(matches!(
        verify_selection_event(stores, &stored_event, &wrong_world),
        Err(ArenaError::SelectionWorldMismatch)
    ));
}

#[test]
fn selection_load_requires_a_preexisting_receipt() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let world = fixture.world.clone();
    let operator = evaluate(fixture).unwrap();
    assert!(matches!(
        load_selection(operator.into_stores(), "evaluation-001", &world),
        Err(ArenaError::UnknownSelection(evaluation_id)) if evaluation_id == "evaluation-001"
    ));
}

#[test]
fn selection_expands_authenticated_mixed_histogram_in_canonical_order() {
    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture_with_confidence(&directory, 2_500);
    let world = fixture.world.clone();
    let regressed_parent_event = append_run(
        &mut fixture.stores,
        &fixture.signer,
        &fixture.repository,
        "parent-visible-a-regressed",
        &fixture.parent_genome_id,
        world.id(),
        &fixture.revision,
        "task-visible-a",
        task_input("task-visible-a"),
        RunCompletionReason::Success,
        b"incorrect",
    );
    fixture.parent = plan("parent", Some(("task-visible-a", &regressed_parent_event)));
    let operator = evaluate(fixture).unwrap();
    let selection = select_and_record(
        operator.into_stores(),
        "evaluation-001",
        &world,
        1_788_000_123_999,
    )
    .unwrap();

    let receipt = selection.receipt();
    assert_eq!(receipt.correctness_regressions(), 1);
    assert_eq!(receipt.correctness_unchanged(), 1);
    assert_eq!(receipt.correctness_improvements(), 2);
    assert_eq!(receipt.estimate_bps(), 2_500);
    assert_eq!(receipt.lower_bps(), 0);
    assert_eq!(receipt.upper_bps(), 5_000);
}

#[test]
fn selection_rehydration_requires_the_receipt_cas_blob() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let world = fixture.world.clone();
    let operator = evaluate(fixture).unwrap();
    let selected = select_and_record(
        operator.into_stores(),
        "evaluation-001",
        &world,
        1_788_000_123_999,
    )
    .unwrap();
    let receipt_id = ArtifactId::parse(selected.event().receipt_artifact_id.clone()).unwrap();
    let stores = selected.into_stores();
    let cas = ArtifactStore::open(directory.path().join("blobs")).unwrap();
    fs::remove_file(cas.path_for(&receipt_id)).unwrap();
    assert!(matches!(
        load_selection(stores, "evaluation-001", &world),
        Err(ArenaError::Ledger(_))
    ));
}

#[test]
fn selection_rehydration_rejects_hash_valid_forged_receipt_and_event() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let world = fixture.world.clone();
    let operator = evaluate(fixture).unwrap();
    let selected = select_and_record(
        operator.into_stores(),
        "evaluation-001",
        &world,
        1_788_000_123_999,
    )
    .unwrap();
    let history = selected.into_stores().events.replay_verified().unwrap();
    let original = history
        .iter()
        .find(|event| event.event_type == "selection.recorded")
        .unwrap();
    let (evaluation_id, world_id) = hephaestus_arena::selection_event_references(original).unwrap();
    let original_payload: serde_json::Value = serde_json::from_slice(&original.payload).unwrap();
    let original_artifact = original_payload["receipt_artifact_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let original_artifact_id = ArtifactId::parse(original_artifact.clone()).unwrap();
    let artifact_root = directory.path().join("blobs");
    let original_bytes = fs::read(
        ArtifactStore::open(&artifact_root)
            .unwrap()
            .path_for(&original_artifact_id),
    )
    .unwrap();

    let mut forged_receipt: serde_json::Value = serde_json::from_slice(&original_bytes).unwrap();
    let eligible = forged_receipt["metrics_eligible"].as_bool().unwrap();
    forged_receipt["metrics_eligible"] = serde_json::json!(!eligible);
    let forged_receipt: SelectionReceipt = serde_json::from_value(forged_receipt).unwrap();
    let forged_bytes = serde_json::to_vec(&forged_receipt).unwrap();
    assert_ne!(forged_bytes, original_bytes);
    let mut artifacts =
        EvaluationStores::open(directory.path().join("forged.sqlite3"), &artifact_root).unwrap();
    let forged_artifact = artifacts.artifacts.put(&forged_bytes).unwrap();
    let forged_artifact = forged_artifact.as_str().to_owned();

    append_non_selection_history(&mut artifacts, &history);
    let forged_event_payload = String::from_utf8(original.payload.clone())
        .unwrap()
        .replace(&original_artifact, &forged_artifact);
    assert!(forged_event_payload.contains(&forged_artifact));
    artifacts
        .events
        .append(EventInput::new(
            original.event_id.clone(),
            original.aggregate_id.clone(),
            original.event_type.clone(),
            original.actor.clone(),
            original.timestamp_millis,
            forged_event_payload,
        ))
        .unwrap();

    assert!(matches!(
        load_selection(artifacts, &evaluation_id, &world),
        Err(ArenaError::SelectionConflict(_))
    ));
    assert_eq!(world_id, world.id());

    let mut invalid_event_stores = EvaluationStores::open(
        directory.path().join("invalid-event.sqlite3"),
        &artifact_root,
    )
    .unwrap();
    append_non_selection_history(&mut invalid_event_stores, &history);
    let wrong_world_id = "hephaestus:world:not-the-source-world";
    let invalid_event_payload = String::from_utf8(original.payload.clone())
        .unwrap()
        .replace(&world_id, wrong_world_id);
    assert!(invalid_event_payload.contains(wrong_world_id));
    invalid_event_stores
        .events
        .append(EventInput::new(
            original.event_id.clone(),
            original.aggregate_id.clone(),
            original.event_type.clone(),
            original.actor.clone(),
            original.timestamp_millis,
            invalid_event_payload,
        ))
        .unwrap();
    assert!(matches!(
        load_selection(invalid_event_stores, &evaluation_id, &world),
        Err(ArenaError::InvalidSelectionEvent)
    ));
}

#[test]
fn restart_rehydration_requires_world_bound_evaluator_evidence() {
    let directory = TempDir::new().unwrap();
    let operator = evaluate(make_fixture(&directory)).unwrap();
    let evaluator_id = ArtifactId::parse(operator.selection_evidence().evaluator_id()).unwrap();
    let stores = operator.into_stores();
    let cas = ArtifactStore::open(directory.path().join("blobs")).unwrap();
    fs::remove_file(cas.path_for(&evaluator_id)).unwrap();

    assert!(matches!(
        load_operator_evaluation(stores, "evaluation-001"),
        Err(ArenaError::Ledger(_))
    ));
}

#[test]
fn restart_rehydration_requires_transitive_run_artifacts() {
    let directory = TempDir::new().unwrap();
    let operator = evaluate(make_fixture(&directory)).unwrap();
    let stores = operator.into_stores();
    let stdout_id = stores
        .events
        .replay_verified()
        .unwrap()
        .into_iter()
        .find_map(|event| {
            serde_json::from_slice::<serde_json::Value>(&event.payload)
                .ok()?
                .pointer("/claims/stdout_artifact_id")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap();
    let stdout_id = ArtifactId::parse(stdout_id).unwrap();
    let cas = ArtifactStore::open(directory.path().join("blobs")).unwrap();
    fs::remove_file(cas.path_for(&stdout_id)).unwrap();

    assert!(matches!(
        load_operator_evaluation(stores, "evaluation-001"),
        Err(ArenaError::Ledger(_))
    ));
}

#[test]
fn restart_rehydration_rejects_hash_valid_impossible_aggregates() {
    let directory = TempDir::new().unwrap();
    let operator = evaluate(make_fixture(&directory)).unwrap();
    let stores = operator.into_stores();
    let history = stores.events.replay_verified().unwrap();
    let mut forged = EvaluationStores::open(
        directory.path().join("forged.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    for event in history {
        let payload = if event.event_id == "arena:evaluation:evaluation-001:recorded" {
            let text = String::from_utf8(event.payload).unwrap();
            text.replace(
                "\"regressions\":1,\"improvements\":1",
                "\"regressions\":1,\"improvements\":0",
            )
            .into_bytes()
        } else {
            event.payload
        };
        forged
            .events
            .append(EventInput::new(
                event.event_id,
                event.aggregate_id,
                event.event_type,
                event.actor,
                event.timestamp_millis,
                payload,
            ))
            .unwrap();
    }

    assert!(matches!(
        load_operator_evaluation(forged, "evaluation-001"),
        Err(ArenaError::InvalidStoredReceipt("aggregate metrics"))
    ));
}

#[test]
fn constructors_reject_malformed_duplicate_empty_and_oversized_inputs() {
    assert!(
        EvaluationBinding::new("not-a-world", 1, "env", "0".repeat(64), budget_receipt(),).is_err()
    );
    assert!(
        EvaluationBinding::new(
            format!("hephaestus:world:{}", "G".repeat(64)),
            1,
            "env",
            "0".repeat(64),
            budget_receipt(),
        )
        .is_err()
    );
    assert!(TrustedTask::new("not valid", "input", "output").is_err());
    assert!(matches!(
        TrustedTask::new("task", "input", "x".repeat(65 * 1024)),
        Err(ArenaError::TextTooLarge { .. })
    ));
    assert!(matches!(
        TrustedManifest::new("empty", Visibility::Visible, vec![]),
        Err(ArenaError::EmptyManifest)
    ));
    assert!(matches!(
        TrustedManifest::new(
            "duplicates",
            Visibility::Visible,
            vec![
                TrustedTask::new("task", "a", "a").unwrap(),
                TrustedTask::new("task", "b", "b").unwrap(),
            ],
        ),
        Err(ArenaError::DuplicateTaskId(task)) if task == "task"
    ));
    assert!(matches!(
        TrialPlan::new([
            ("task".to_owned(), "result:one".to_owned()),
            ("task".to_owned(), "result:two".to_owned()),
        ]),
        Err(ArenaError::DuplicateTaskId(task)) if task == "task"
    ));
    let oversized =
        (0..=1_000).map(|index| (format!("task-{index}"), format!("result:run-{index}")));
    assert!(matches!(
        TrialPlan::new(oversized),
        Err(ArenaError::TooManyTasks)
    ));
    assert!(TrialPlan::new([("task".to_owned(), format!("result:{}", "r".repeat(128)),)]).is_ok());
    assert!(TrialPlan::new([("task".to_owned(), "not-a-result".to_owned())]).is_err());
}

#[test]
fn world_manifest_and_evaluator_semantics_fail_before_publication() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let altered_visible = TrustedManifest::new(
        "visible-suite-v1",
        Visibility::Visible,
        vec![
            TrustedTask::new("task-visible-a", "changed input", VISIBLE_SECRET).unwrap(),
            TrustedTask::new("task-visible-b", "input visible b", "B").unwrap(),
        ],
    )
    .unwrap();
    let before = artifact_file_count(&directory);
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &altered_visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(
        result,
        Err(ArenaError::WorldArtifactMismatch("arena.visible_manifest"))
    ));
    assert_eq!(artifact_file_count(&directory), before);

    let evaluator_path = directory.path().join("wrong-id-evaluator");
    fs::copy(env!("CARGO_BIN_EXE_hephaestus-evaluator"), &evaluator_path).unwrap();
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700)).unwrap();
    let wrong_id = ArtifactId::for_bytes(b"unimplemented-evaluator-v2");
    assert!(matches!(
        IsolatedEvaluator::open_with_policy(
            directory.path().join("wrong-evaluator"),
            evaluator_path,
            wrong_id.as_str(),
            IsolationPolicy::unconfined_for_testing(),
            WorkerLimits::new(Duration::from_secs(1), 1024, 1024).unwrap(),
        ),
        Err(ArenaError::WorldArtifactMismatch("arena.evaluator"))
    ));
}

#[test]
fn isolated_evaluator_response_must_bind_the_exact_request() {
    let directory = TempDir::new().unwrap();
    let evaluator_path = directory.path().join("binding-forger");
    fs::write(
        &evaluator_path,
        concat!(
            "#!/bin/sh\n",
            "cat >/dev/null\n",
            "printf '%s' '",
            "{\"schema_version\":1,\"request_artifact_id\":\"",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "\",\"scores\":{\"parent_visible_correct\":0,",
            "\"candidate_visible_correct\":0,\"parent_sealed_correct\":0,",
            "\"candidate_sealed_correct\":0,\"regressions\":0,",
            "\"improvements\":0,\"visible_total\":2,\"sealed_total\":2}}'\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700)).unwrap();
    let fixture = make_fixture_with_evaluator(&directory, evaluator_path);
    let before = artifact_file_count(&directory);
    let Err(error) = evaluate(fixture) else {
        panic!("a forged response must fail closed");
    };
    assert!(
        matches!(
            error,
            ArenaError::EvaluatorProtocol("response request binding mismatch")
        ),
        "unexpected evaluator error: {error:?}"
    );
    assert_eq!(artifact_file_count(&directory), before);
    assert!(
        directory
            .path()
            .join("evaluator-runs")
            .read_dir()
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn trial_plan_cannot_supply_a_fake_genome_identity() {
    assert!(matches!(
        TrialPlan::new([
            ("task-a".to_owned(), "result:run".to_owned()),
            ("task-b".to_owned(), "result:run".to_owned()),
        ]),
        Err(ArenaError::DuplicateRunEvent(event)) if event == "result:run"
    ));
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let actual = fixture.candidate_genome_id.clone();
    let operator = evaluate(fixture).unwrap();
    let recorded = operator.candidate_result();
    assert_eq!(recorded.summary.candidate_genome_id, actual);
    assert_ne!(
        recorded.summary.candidate_genome_id,
        format!("hephaestus:genome:{}", "f".repeat(64))
    );
}

#[test]
fn unknown_and_forged_events_fail_without_writes() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let before = artifact_file_count(&directory);
    let unknown = plan("parent", Some(("task-visible-a", "result:unknown")));
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &unknown,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(result, Err(ArenaError::UnknownRunEvent(_))));
    assert_eq!(artifact_file_count(&directory), before);

    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let attacker = RunResultSigner::from_seed([9; 32]);
    let forged_event_id = append_run(
        &mut fixture.stores,
        &attacker,
        &fixture.repository,
        "forged",
        &fixture.parent_genome_id,
        fixture.world.id(),
        &fixture.revision,
        "task-visible-a",
        task_input("task-visible-a"),
        RunCompletionReason::Success,
        b"forged",
    );
    let forged = plan("parent", Some(("task-visible-a", &forged_event_id)));
    let before = artifact_file_count(&directory);
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &forged,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    match result {
        Err(ArenaError::RunReceipt(_)) => {}
        Err(error) => panic!("unexpected error: {error:?}"),
        Ok(_) => panic!("forged runtime event was accepted"),
    }
    assert_eq!(artifact_file_count(&directory), before);
}

#[derive(Clone, Copy, Debug)]
enum ContextForgery {
    Task,
    Input,
    Seed,
    Environment,
    Budget,
}

#[test]
fn signed_results_cannot_be_relabelled_or_cross_experiment_boundaries() {
    for forgery in [
        ContextForgery::Task,
        ContextForgery::Input,
        ContextForgery::Seed,
        ContextForgery::Environment,
        ContextForgery::Budget,
    ] {
        let directory = TempDir::new().unwrap();
        let mut fixture = make_fixture(&directory);
        let genome_id = fixture.parent_genome_id.clone();
        let world_id = fixture.world.id().to_owned();
        let task_id = if matches!(forgery, ContextForgery::Task) {
            "other-task"
        } else {
            "task-visible-a"
        };
        let input = if matches!(forgery, ContextForgery::Input) {
            "altered visible input"
        } else {
            task_input("task-visible-a")
        };
        let seed = if matches!(forgery, ContextForgery::Seed) {
            43
        } else {
            42
        };
        let environment = if matches!(forgery, ContextForgery::Environment) {
            "other-environment-v1"
        } else {
            "environment-v1"
        };
        let budget = if matches!(forgery, ContextForgery::Budget) {
            Budget::new(Duration::from_secs(9), 1_048_576, 0).unwrap()
        } else {
            Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap()
        };
        let forged = append_run_with_context(
            &mut fixture.stores,
            &fixture.signer,
            &fixture.repository,
            &format!("context-forgery-{forgery:?}"),
            &genome_id,
            &world_id,
            &fixture.revision,
            task_id,
            input,
            seed,
            environment,
            budget,
            RunCompletionReason::Success,
            VISIBLE_SECRET.as_bytes(),
        );
        let plan = plan("parent", Some(("task-visible-a", &forged)));
        let before = artifact_file_count(&directory);
        let result = evaluate_and_record(
            fixture.stores,
            context(),
            &fixture.world,
            EvaluationInputs {
                binding: &fixture.binding,
                visible: &fixture.visible,
                sealed: &fixture.sealed,
                parent: &plan,
                candidate: &fixture.candidate,
                evaluator: &fixture.evaluator,
            },
        );
        assert!(
            matches!(
                result,
                Err(ArenaError::BindingMismatch("runtime experiment context"))
            ),
            "{forgery:?}"
        );
        assert_eq!(artifact_file_count(&directory), before, "{forgery:?}");
    }
}

#[test]
fn failed_run_is_retained_and_cross_world_is_rejected() {
    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let world_id = fixture.world.id().to_owned();
    let genome_id = fixture.parent_genome_id.clone();
    let failed = append_run(
        &mut fixture.stores,
        &fixture.signer,
        &fixture.repository,
        "failed",
        &genome_id,
        &world_id,
        &fixture.revision,
        "task-visible-a",
        task_input("task-visible-a"),
        RunCompletionReason::OutputBudgetExceeded,
        b"partial",
    );
    let failed_plan = plan("parent", Some(("task-visible-a", &failed)));
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &failed_plan,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(result.is_ok());

    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let genome_id = fixture.parent_genome_id.clone();
    let other_world = format!("hephaestus:world:{}", "f".repeat(64));
    let cross = append_run(
        &mut fixture.stores,
        &fixture.signer,
        &fixture.repository,
        "cross-world",
        &genome_id,
        &other_world,
        &fixture.revision,
        "task-visible-a",
        task_input("task-visible-a"),
        RunCompletionReason::Success,
        VISIBLE_SECRET.as_bytes(),
    );
    let cross_plan = plan("parent", Some(("task-visible-a", &cross)));
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &cross_plan,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(result, Err(ArenaError::RunWorldMismatch(_))));
}

#[test]
fn corrupt_or_missing_output_is_rejected_without_evaluation_writes() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let history = fixture.stores.events.replay_verified().unwrap();
    let receipt = RunResultReceipt::parse_from_event(
        history
            .iter()
            .find(|event| event.event_id == "result:parent-task-visible-a")
            .unwrap(),
        &fixture.signer.verifier(),
    )
    .unwrap();
    let id = ArtifactId::parse(receipt.stdout_artifact_id).unwrap();
    let cas = ArtifactStore::open(directory.path().join("blobs")).unwrap();
    fs::write(cas.path_for(&id), b"tampered").unwrap();
    assert!(matches!(evaluate(fixture), Err(ArenaError::Ledger(_))));

    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let empty = fixture.stores.artifacts.put(b"").unwrap();
    let experiment = ExperimentContext::new(
        "task-visible-a",
        task_input("task-visible-a"),
        42,
        "environment-v1",
    )
    .unwrap();
    let spec = RunSpec::new_for_experiment_at_revision(
        "missing",
        &fixture.parent_genome_id,
        fixture.world.id(),
        &fixture.repository,
        &fixture.revision,
        task_input("task-visible-a"),
        CapabilitySet::new(false, false),
        Budget::new(Duration::from_secs(10), 1_048_576, 100).unwrap(),
        experiment,
    )
    .unwrap();
    let receipt = RunResultReceipt::from_run_spec(
        &spec,
        RunCompletionReason::Success,
        1,
        0,
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        empty.as_str(),
        vec![],
    )
    .unwrap();
    fixture
        .stores
        .events
        .append(fixture.signer.issue(receipt.clone(), 1).unwrap())
        .unwrap();
    let missing_event_id = receipt.event_id();
    let missing = plan("parent", Some(("task-visible-a", &missing_event_id)));
    let before = artifact_file_count(&directory);
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &missing,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(result, Err(ArenaError::Ledger(_))));
    assert_eq!(artifact_file_count(&directory), before);
}

#[test]
fn mixed_genome_revision_and_task_set_mismatches_are_rejected() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let mixed = plan(
        "parent",
        Some(("task-visible-a", "result:candidate-task-visible-a")),
    );
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &mixed,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(result, Err(ArenaError::MixedSubmissionGenome)));

    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let genome_id = fixture.parent_genome_id.clone();
    let world_id = fixture.world.id().to_owned();
    let changed = append_run(
        &mut fixture.stores,
        &fixture.signer,
        &fixture.repository,
        "changed-revision",
        &genome_id,
        &world_id,
        &fixture.alternate_revision,
        "task-visible-a",
        task_input("task-visible-a"),
        RunCompletionReason::Success,
        VISIBLE_SECRET.as_bytes(),
    );
    let changed_plan = plan("parent", Some(("task-visible-a", &changed)));
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &changed_plan,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(
        matches!(result, Err(ArenaError::SourceRevisionMismatch(task)) if task == "task-visible-a")
    );

    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let incomplete = TrialPlan::new([(
        "task-visible-a".to_owned(),
        "result:parent-task-visible-a".to_owned(),
    )])
    .unwrap();
    let result = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &incomplete,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(result, Err(ArenaError::TaskSetMismatch { .. })));
}

#[test]
fn identical_retry_returns_existing_event_after_reopen() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let first = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    )
    .unwrap();
    let event = first.candidate_result().event.clone();
    let reopened = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let second = evaluate_and_record(
        reopened,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    )
    .unwrap();
    assert_eq!(second.candidate_result().event, event);
    let stores = second.into_stores();
    assert_eq!(
        stores
            .events
            .replay_verified()
            .unwrap()
            .iter()
            .filter(|item| item.event_type == "evaluation.recorded")
            .count(),
        1
    );
}

#[test]
fn wrong_event_identity_and_conflicting_retry_fail_before_writes() {
    let directory = TempDir::new().unwrap();
    let fixture = make_fixture(&directory);
    let before = artifact_file_count(&directory);
    let mut invalid = context();
    invalid.event_id = "caller-event".to_owned();
    let result = evaluate_and_record(
        fixture.stores,
        invalid,
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(
        result,
        Err(ArenaError::InvalidId {
            field: "event_id",
            ..
        })
    ));
    assert_eq!(artifact_file_count(&directory), before);

    let directory = TempDir::new().unwrap();
    let mut fixture = make_fixture(&directory);
    let genome_id = fixture.candidate_genome_id.clone();
    let world_id = fixture.world.id().to_owned();
    let alternate_event = append_run(
        &mut fixture.stores,
        &fixture.signer,
        &fixture.repository,
        "candidate-visible-a-alternate",
        &genome_id,
        &world_id,
        &fixture.revision,
        "task-visible-a",
        task_input("task-visible-a"),
        RunCompletionReason::Success,
        b"changed",
    );
    let alternate = plan("candidate", Some(("task-visible-a", &alternate_event)));
    let first = evaluate_and_record(
        fixture.stores,
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &fixture.candidate,
            evaluator: &fixture.evaluator,
        },
    )
    .unwrap();
    let before = artifact_file_count(&directory);
    let result = evaluate_and_record(
        first.into_stores(),
        context(),
        &fixture.world,
        EvaluationInputs {
            binding: &fixture.binding,
            visible: &fixture.visible,
            sealed: &fixture.sealed,
            parent: &fixture.parent,
            candidate: &alternate,
            evaluator: &fixture.evaluator,
        },
    );
    assert!(matches!(result, Err(ArenaError::EvaluationConflict(_))));
    assert_eq!(artifact_file_count(&directory), before);
}

fn make_cluster_fixture(directory: &TempDir) -> Fixture {
    let evaluator_path = directory.path().join("hephaestus-evaluator");
    fs::copy(env!("CARGO_BIN_EXE_hephaestus-evaluator"), &evaluator_path).unwrap();
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700)).unwrap();
    let overrides = BTreeMap::from([
        (
            "candidate-task-visible-a".to_owned(),
            (
                RunCompletionReason::Success,
                VISIBLE_SECRET.to_uppercase().into_bytes(),
            ),
        ),
        (
            "candidate-task-visible-b".to_owned(),
            (RunCompletionReason::ProviderFailure, Vec::new()),
        ),
        (
            "candidate-task-sealed-a".to_owned(),
            (RunCompletionReason::IoFailure, Vec::new()),
        ),
    ]);
    make_fixture_with_options(directory, evaluator_path, 9_500, None, &overrides)
}

#[test]
#[allow(clippy::too_many_lines)]
fn failure_clusters_record_operator_aggregates_and_replay() {
    let directory = TempDir::new().unwrap();
    let fixture = make_cluster_fixture(&directory);
    let world = fixture.world.clone();
    let evaluation = evaluate(fixture).unwrap();
    let stores = evaluation.into_stores();
    let check = check_failure_clusters(
        stores,
        "analysis-001",
        "evaluation-001",
        &world,
        Some("identity"),
        1_788_000_123_500,
    )
    .unwrap();

    assert_eq!(
        check.event().event_id,
        "forge:analysis:analysis-001:clustered"
    );
    assert_eq!(check.event().aggregate_id, "forge:analysis:analysis-001");
    assert_eq!(check.event().event_type, "forge.clustered");
    assert_eq!(check.event().actor, "arena-plane");

    let analysis = check.analysis().clone();
    assert_eq!(analysis.algorithm, "failure-cluster-v2");
    assert_eq!(analysis.candidate_operation.as_deref(), Some("identity"));
    assert_eq!(analysis.analysis_id, "analysis-001");
    assert_eq!(analysis.evaluation_id, "evaluation-001");
    assert_eq!(analysis.world_id, world.id());
    assert_eq!(analysis.total_visible_failed_trials, 2);
    assert_eq!(analysis.total_sealed_failed_trials, 1);
    assert_eq!(analysis.clusters.len(), 3);

    let by_signature: BTreeMap<_, _> = analysis
        .clusters
        .iter()
        .map(|cluster| (cluster.signature.clone(), cluster))
        .collect();
    let case_mismatch = by_signature["shape_case_mismatch"];
    assert_eq!(case_mismatch.visible_count, 1);
    assert_eq!(case_mismatch.sealed_count, 0);
    assert_eq!(
        case_mismatch.suggested_mutation,
        Some(SuggestedMutation::ReferenceOperation {
            operation_after: "ascii_uppercase".to_owned()
        })
    );
    let provider_failure = by_signature["completion_provider_failure"];
    assert_eq!(provider_failure.visible_count, 1);
    assert_eq!(provider_failure.suggested_mutation, None);
    let io_failure = by_signature["completion_io_failure"];
    assert_eq!(io_failure.visible_count, 0);
    assert_eq!(io_failure.sealed_count, 1);
    assert_eq!(io_failure.suggested_mutation, None);

    // Sealed task identities, inputs, and expected outputs never leave the
    // analysis, even though the sealed candidate's I/O failure is counted.
    let analysis_bytes = serde_json::to_vec(&analysis).unwrap();
    for secret in [
        SEALED_SECRET,
        "task-sealed-a",
        "task-sealed-b",
        "sealed prompt 204",
        "sealed prompt 517",
    ] {
        assert!(
            !analysis_bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
        );
    }

    // An exact retry recomputes and returns the identical event and analysis.
    let expected_analysis = analysis.clone();
    let expected_event = check.event().clone();
    let stores = check.into_stores();
    let retry = check_failure_clusters(
        stores,
        "analysis-001",
        "evaluation-001",
        &world,
        Some("identity"),
        1_788_000_123_501,
    )
    .unwrap();
    assert_eq!(retry.analysis(), &expected_analysis);
    assert_eq!(retry.event(), &expected_event);

    // Startup/replay-style verification recomputes byte-identical evidence.
    let stores = retry.into_stores();
    let event = stores
        .events
        .replay_verified()
        .unwrap()
        .into_iter()
        .find(|event| event.event_id == "forge:analysis:analysis-001:clustered")
        .unwrap();
    let verified = verify_cluster_event(stores, &event, &world, Some("identity")).unwrap();
    assert_eq!(verified.analysis(), &expected_analysis);

    // A rewritten event type is rejected even though the ID prefix matches.
    let mut stores = verified.into_stores();
    let wrong_type_event = stores
        .events
        .append(EventInput::new(
            "forge:analysis:analysis-001:wrong-type",
            "forge:analysis:analysis-001",
            "unrelated.recorded",
            "arena-plane",
            1_788_000_123_502,
            &event.payload,
        ))
        .unwrap();
    assert!(matches!(
        verify_cluster_event(stores, &wrong_type_event, &world, Some("identity")),
        Err(ArenaError::InvalidClusterEvent)
    ));

    // A forged actor on an otherwise-canonical event is rejected.
    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let mut spoofed_event = event.clone();
    spoofed_event.actor = "untrusted-actor".to_owned();
    assert!(matches!(
        verify_cluster_event(stores, &spoofed_event, &world, Some("identity")),
        Err(ArenaError::InvalidClusterEvent)
    ));

    // Loading from fresh stores recomputes the same canonical analysis.
    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    let reloaded = load_failure_clusters(
        stores,
        "analysis-001",
        "evaluation-001",
        &world,
        Some("identity"),
    )
    .unwrap();
    assert_eq!(reloaded.analysis(), &expected_analysis);
    drop(reloaded);

    // A tampered analysis artifact fails closed on reload.
    let artifact_id = ArtifactId::parse(expected_event.analysis_artifact_id.clone()).unwrap();
    let artifact_path = directory
        .path()
        .join("blobs")
        .join(&artifact_id.as_str()[..2])
        .join(artifact_id.as_str());
    fs::write(artifact_path, b"tampered cluster analysis").unwrap();
    let stores = EvaluationStores::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("blobs"),
    )
    .unwrap();
    assert!(
        load_failure_clusters(
            stores,
            "analysis-001",
            "evaluation-001",
            &world,
            Some("identity")
        )
        .is_err()
    );
}
