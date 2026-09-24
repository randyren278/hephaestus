use std::{fs, process::Command, time::Duration};

use hephaestus_core::authority::CapabilitySet;
use hephaestus_experience::{
    ExperienceError, RUN_RESULT_SCHEMA_VERSION, RunCompletionReason, RunResultReceipt,
    RunResultSigner, RunResultVerifier,
};
use hephaestus_ledger::{ArtifactId, EventStore};
use hephaestus_runtime::{Budget, ExperimentContext, RunSpec};
use tempfile::tempdir;

fn spec(budget: Budget) -> RunSpec {
    let repository = tempdir().expect("repository");
    fs::write(repository.path().join("fixture.txt"), b"fixture").expect("write fixture");
    for arguments in [
        vec!["init", "-q"],
        vec!["add", "fixture.txt"],
        vec![
            "-c",
            "user.name=Hephaestus Test",
            "-c",
            "user.email=hephaestus@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("git")
                .args(arguments)
                .current_dir(repository.path())
                .status()
                .expect("run git")
                .success()
        );
    }
    let prompt = "paired input";
    let experiment = ExperimentContext::new("task-001", prompt, 42, "test-environment-v1")
        .expect("experiment context");
    RunSpec::new_for_experiment(
        "run_001",
        format!(
            "hephaestus:genome:{}",
            ArtifactId::for_bytes(b"genome").as_str()
        ),
        format!(
            "hephaestus:world:{}",
            ArtifactId::for_bytes(b"world").as_str()
        ),
        repository.path(),
        prompt,
        CapabilitySet::new(false, false),
        budget,
        experiment,
    )
    .expect("run spec")
}

fn receipt() -> RunResultReceipt {
    let output = ArtifactId::for_bytes(b"output").as_str().to_owned();
    let diagnostics = ArtifactId::for_bytes(b"diagnostics").as_str().to_owned();
    let trace = ArtifactId::for_bytes(b"trace").as_str().to_owned();
    let spec = spec(Budget::new(Duration::from_secs(10), 1_048_576, 0).expect("budget"));
    RunResultReceipt::from_run_spec(
        &spec,
        RunCompletionReason::Success,
        12,
        0,
        output,
        diagnostics,
        vec![trace],
    )
    .expect("valid receipt")
}

fn signer() -> RunResultSigner {
    RunResultSigner::from_seed([7; 32])
}

fn stored_with(
    receipt: RunResultReceipt,
    signer: &RunResultSigner,
    timestamp_millis: i64,
) -> hephaestus_ledger::StoredEvent {
    let directory = tempdir().expect("temporary ledger");
    let mut ledger =
        EventStore::open(directory.path().join("events.sqlite3")).expect("open ledger");
    ledger
        .append(
            signer
                .issue(receipt, timestamp_millis)
                .expect("event input"),
        )
        .expect("append receipt")
}

fn stored(receipt: RunResultReceipt) -> hephaestus_ledger::StoredEvent {
    stored_with(receipt, &signer(), 1_700_000_000_000)
}

#[test]
fn canonical_receipt_round_trips_from_derived_event_envelope() {
    let expected = receipt();
    let event = stored(expected.clone());
    let verifier = signer().verifier();

    assert_eq!(
        RunResultReceipt::parse_from_event(&event, &verifier).expect("parse canonical receipt"),
        expected
    );
    assert_eq!(expected.schema_version, RUN_RESULT_SCHEMA_VERSION);
    assert_eq!(expected.task_id, "task-001");
    assert_eq!(expected.seed, 42);
    assert_eq!(expected.environment_id, "test-environment-v1");
    assert_eq!(
        expected.input_commitment,
        blake3::hash(b"paired input").to_hex().as_str()
    );
    assert_eq!(expected.budget.wall_millis, 10_000);
    assert_eq!(event.event_id, "result:run_001");
    assert_eq!(event.aggregate_id, "run:run_001");
    assert_eq!(event.actor, "runtime-plane");
    assert_eq!(event.event_type, "run.result_recorded");
}

#[test]
fn parser_rejects_forged_event_envelopes() {
    let verifier = signer().verifier();
    let forgeries: [fn(&mut hephaestus_ledger::StoredEvent); 4] = [
        |event: &mut hephaestus_ledger::StoredEvent| event.actor = "operator".to_owned(),
        |event: &mut hephaestus_ledger::StoredEvent| event.event_id = "result:other".to_owned(),
        |event: &mut hephaestus_ledger::StoredEvent| event.aggregate_id = "run:other".to_owned(),
        |event: &mut hephaestus_ledger::StoredEvent| event.event_type = "run.started".to_owned(),
    ];
    for forge in forgeries {
        let mut event = stored(receipt());
        forge(&mut event);
        assert!(matches!(
            RunResultReceipt::parse_from_event(&event, &verifier),
            Err(ExperienceError::InvalidInput(_))
        ));
    }
}

#[test]
fn parser_rejects_unknown_schema_fields_and_invalid_runtime_claims() {
    let canonical = receipt();
    let verifier = signer().verifier();
    let mut unknown = stored(canonical.clone());
    let mut value: serde_json::Value =
        serde_json::from_slice(&unknown.payload).expect("decode signed envelope");
    value["claims"]["untrusted"] = serde_json::json!(true);
    unknown.payload = serde_json::to_vec(&value).expect("encode forged payload");
    assert!(matches!(
        RunResultReceipt::parse_from_event(&unknown, &verifier),
        Err(ExperienceError::Json(_))
    ));

    for (field, replacement) in [
        ("schema_version", serde_json::json!(3)),
        ("run_id", serde_json::json!("../escape")),
        ("actual_cost_microusd", serde_json::json!(1)),
        ("latency_millis", serde_json::json!(86_400_001)),
        ("stdout_artifact_id", serde_json::json!("not-an-artifact")),
        ("source_revision", serde_json::json!("HEAD")),
        ("task_id", serde_json::json!("../task")),
        ("input_commitment", serde_json::json!("not-a-commitment")),
        ("environment_id", serde_json::json!("bad environment")),
    ] {
        let mut event = stored(canonical.clone());
        let mut value: serde_json::Value =
            serde_json::from_slice(&event.payload).expect("decode signed envelope");
        value["claims"][field] = replacement;
        event.payload = serde_json::to_vec(&value).expect("encode forged payload");
        assert!(
            RunResultReceipt::parse_from_event(&event, &verifier).is_err(),
            "{field}"
        );
    }

    for (field, replacement) in [
        ("wall_millis", serde_json::json!(0)),
        ("maximum_output_bytes", serde_json::json!(0)),
        (
            "maximum_cost_microusd",
            serde_json::json!(1_000_000_001_u64),
        ),
    ] {
        let mut event = stored(canonical.clone());
        let mut value: serde_json::Value =
            serde_json::from_slice(&event.payload).expect("decode signed envelope");
        value["claims"]["budget"][field] = replacement;
        event.payload = serde_json::to_vec(&value).expect("encode forged payload");
        assert!(
            RunResultReceipt::parse_from_event(&event, &verifier).is_err(),
            "budget.{field}"
        );
    }

    let mut oversized = stored(canonical);
    oversized.payload = vec![b' '; 131_073];
    assert!(matches!(
        RunResultReceipt::parse_from_event(&oversized, &verifier),
        Err(ExperienceError::RecordTooLarge {
            actual: 131_073,
            maximum: 131_072
        })
    ));

    let unrepresentable =
        spec(Budget::new(Duration::MAX, 1_048_576, 0).expect("runtime accepts broad budget"));
    let artifact = ArtifactId::for_bytes(b"artifact").as_str().to_owned();
    assert!(
        RunResultReceipt::from_run_spec(
            &unrepresentable,
            RunCompletionReason::Success,
            1,
            0,
            &artifact,
            &artifact,
            vec![],
        )
        .is_err()
    );

    let mut interrupted = receipt();
    interrupted.completion_reason = RunCompletionReason::OperatorInterrupt;
    assert!(signer().issue(interrupted, 1).is_err());
    let mut too_many_traces = receipt();
    too_many_traces.trace_artifact_ids = vec![artifact; 1_025];
    assert!(signer().issue(too_many_traces, 1).is_err());

    let mut invalid_schema = receipt();
    invalid_schema.schema_version = 3;
    assert!(signer().issue(invalid_schema, 1).is_err());
    let mut invalid_revision = receipt();
    invalid_revision.source_revision = "HEAD".to_owned();
    assert!(signer().issue(invalid_revision, 1).is_err());
    let mut nonzero_cost = receipt();
    nonzero_cost.actual_cost_microusd = 1;
    assert!(signer().issue(nonzero_cost, 1).is_err());
    let mut invalid_output = receipt();
    invalid_output.stdout_artifact_id = "not-an-artifact".to_owned();
    assert!(signer().issue(invalid_output, 1).is_err());
}

#[test]
fn confirmed_provider_failure_can_be_signed_and_replayed() {
    let mut provider_failure = receipt();
    provider_failure.completion_reason = RunCompletionReason::ProviderFailure;
    let event = stored(provider_failure.clone());
    assert_eq!(
        RunResultReceipt::parse_from_event(&event, &signer().verifier())
            .expect("signed provider failure receipt"),
        provider_failure
    );
}

#[test]
fn supervisor_wall_and_io_failures_can_be_signed_and_replayed() {
    for reason in [
        RunCompletionReason::WallBudgetExceeded,
        RunCompletionReason::IoFailure,
    ] {
        let mut failed = receipt();
        failed.completion_reason = reason;
        let event = stored(failed.clone());
        assert_eq!(
            RunResultReceipt::parse_from_event(&event, &signer().verifier())
                .expect("signed supervised failure receipt"),
            failed
        );
    }
}

#[test]
fn latency_contract_accepts_long_evaluations_and_rejects_above_shared_limit() {
    let artifact = ArtifactId::for_bytes(b"artifact").as_str().to_owned();
    let long = spec(Budget::new(Duration::from_secs(20), 1_048_576, 0).expect("long budget"));
    assert!(
        RunResultReceipt::from_run_spec(
            &long,
            RunCompletionReason::Success,
            10_001,
            0,
            &artifact,
            &artifact,
            vec![],
        )
        .is_ok()
    );

    let maximum =
        spec(Budget::new(Duration::from_secs(86_400), 1_048_576, 0).expect("maximum wall budget"));
    assert!(
        RunResultReceipt::from_run_spec(
            &maximum,
            RunCompletionReason::Success,
            86_400_000,
            0,
            &artifact,
            &artifact,
            vec![],
        )
        .is_ok()
    );
    assert!(
        RunResultReceipt::from_run_spec(
            &maximum,
            RunCompletionReason::Success,
            86_400_001,
            0,
            &artifact,
            &artifact,
            vec![],
        )
        .is_err()
    );
}

#[test]
fn signer_is_deterministic_and_verifier_round_trips_from_public_bytes() {
    let signer = signer();
    let verifier = signer.verifier();
    let reconstructed = RunResultVerifier::from_public_key_bytes(verifier.public_key_bytes())
        .expect("valid public key");
    assert_eq!(reconstructed, verifier);
    assert_eq!(verifier.key_id().len(), 64);

    let claims = receipt();
    let first = stored_with(claims.clone(), &signer, 1_700_000_000_000);
    let second = stored_with(claims, &signer, 1_700_000_000_000);
    assert_eq!(first, second);
    assert!(RunResultReceipt::parse_from_event(&first, &reconstructed).is_ok());
}

#[test]
fn correct_actor_forgery_and_wrong_key_are_rejected() {
    let trusted = signer();
    let attacker = RunResultSigner::from_seed([9; 32]);
    let forged = stored_with(receipt(), &attacker, 1_700_000_000_000);
    assert_eq!(forged.actor, "runtime-plane");
    assert!(matches!(
        RunResultReceipt::parse_from_event(&forged, &trusted.verifier()),
        Err(ExperienceError::InvalidInput(_))
    ));

    let directory = tempdir().expect("temporary ledger");
    let mut ledger = EventStore::open(directory.path().join("events.sqlite3")).unwrap();
    let claims = receipt();
    let unsigned = ledger
        .append(hephaestus_ledger::EventInput::new(
            claims.event_id(),
            claims.aggregate_id(),
            "run.result_recorded",
            "runtime-plane",
            1_700_000_000_000,
            serde_json::to_vec(&claims).unwrap(),
        ))
        .unwrap();
    assert!(RunResultReceipt::parse_from_event(&unsigned, &trusted.verifier()).is_err());
}

#[test]
fn signature_binds_timestamp_envelope_and_every_claim() {
    let verifier = signer().verifier();
    let mut timestamp = stored(receipt());
    timestamp.timestamp_millis += 1;
    assert!(RunResultReceipt::parse_from_event(&timestamp, &verifier).is_err());

    let mut claim = stored(receipt());
    let mut value: serde_json::Value = serde_json::from_slice(&claim.payload).unwrap();
    value["claims"]["seed"] = serde_json::json!(43);
    claim.payload = serde_json::to_vec(&value).unwrap();
    assert!(RunResultReceipt::parse_from_event(&claim, &verifier).is_err());

    let mut key_id = stored(receipt());
    let mut value: serde_json::Value = serde_json::from_slice(&key_id.payload).unwrap();
    value["producer_key_id"] = serde_json::json!("0".repeat(64));
    key_id.payload = serde_json::to_vec(&value).unwrap();
    assert!(RunResultReceipt::parse_from_event(&key_id, &verifier).is_err());

    let mut signature = stored(receipt());
    let mut value: serde_json::Value = serde_json::from_slice(&signature.payload).unwrap();
    value["signature"] = serde_json::json!("A".repeat(128));
    signature.payload = serde_json::to_vec(&value).unwrap();
    assert!(RunResultReceipt::parse_from_event(&signature, &verifier).is_err());

    let mut short_signature = stored(receipt());
    let mut value: serde_json::Value = serde_json::from_slice(&short_signature.payload).unwrap();
    value["signature"] = serde_json::json!("00");
    short_signature.payload = serde_json::to_vec(&value).unwrap();
    assert!(RunResultReceipt::parse_from_event(&short_signature, &verifier).is_err());
}

#[test]
fn signature_cannot_be_copied_to_different_valid_claims() {
    let signer = signer();
    let verifier = signer.verifier();
    let first = stored_with(receipt(), &signer, 1_700_000_000_000);
    let mut changed_receipt = receipt();
    changed_receipt.seed = 43;
    let mut second = stored_with(changed_receipt, &signer, 1_700_000_000_000);
    let first_value: serde_json::Value = serde_json::from_slice(&first.payload).unwrap();
    let mut second_value: serde_json::Value = serde_json::from_slice(&second.payload).unwrap();
    second_value["signature"] = first_value["signature"].clone();
    second.payload = serde_json::to_vec(&second_value).unwrap();
    assert!(RunResultReceipt::parse_from_event(&second, &verifier).is_err());
}
