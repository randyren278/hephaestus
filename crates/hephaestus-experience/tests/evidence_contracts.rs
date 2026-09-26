use std::collections::BTreeMap;

use hephaestus_experience::{
    EvidenceRecorder, EvidenceRequest, EvidenceSink, ExperienceError, ExperienceInput,
    ExperienceKind, ExperienceReceipt, Provenance, RedactionPolicy, RetentionLimits, TraceInput,
    TraceKind, TraceReceipt, bounded_evidence_channel, rehydrate_experience,
};
use hephaestus_ledger::{ArtifactId, ArtifactStore, EventInput, EventStore};
use tempfile::tempdir;

#[test]
fn channel_sink_waits_for_canonical_writer_acknowledgement() {
    let directory = tempdir().expect("evidence directory");
    let mut canonical = recorder(&directory, 8, 16_384);
    let (mut sink, requests) = bounded_evidence_channel(1);
    let worker = std::thread::spawn(move || {
        sink.record_trace(trace("channel-trace", 17), 0)
            .expect("writer acknowledgement")
    });
    let request = requests.recv().expect("evidence request");
    assert_eq!(request.run_id(), "run-1");
    assert!(request.provenance().is_some());
    assert!(
        !worker.is_finished(),
        "executor must wait for durable acknowledgement"
    );
    request
        .persist(&mut canonical)
        .expect("persist canonical trace");
    let receipt = worker.join().expect("join evidence sender");
    assert_eq!(receipt.event_id, "channel-trace");
    assert_eq!(
        canonical
            .replay_verified()
            .expect("verified canonical trace")
            .len(),
        1
    );
}

#[test]
fn rejected_evidence_requests_release_both_waiting_executor_paths() {
    let (reply, response) = std::sync::mpsc::channel();
    EvidenceRequest::EnsureCapacity {
        run_id: "run-1".to_owned(),
        needed: 2,
        reply,
    }
    .reject("writer unavailable");
    assert_eq!(
        response.recv().expect("capacity rejection acknowledgement"),
        Err("writer unavailable".to_owned())
    );

    let (reply, response) = std::sync::mpsc::channel();
    EvidenceRequest::RecordTrace {
        input: trace("rejected-trace", 18),
        reserved_after: 0,
        reply,
    }
    .reject("writer unavailable");
    assert_eq!(
        response.recv().expect("trace rejection acknowledgement"),
        Err("writer unavailable".to_owned())
    );
}

#[test]
fn writer_does_not_acknowledge_failed_capacity_reservation() {
    let directory = tempdir().expect("evidence directory");
    let mut canonical = recorder(&directory, 1, 16_384);
    let (reply, response) = std::sync::mpsc::channel();
    let request = EvidenceRequest::EnsureCapacity {
        run_id: "run-1".to_owned(),
        needed: 2,
        reply,
    };

    assert!(request.persist(&mut canonical).is_err());
    assert!(
        response
            .recv()
            .expect("capacity rejection acknowledgement")
            .is_err()
    );
}

#[test]
fn traces_cover_observable_runtime_events_and_redact_before_persistence() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 32, 16_384);
    let provenance = provenance();
    let kinds = [
        TraceKind::LifecycleStarted,
        TraceKind::LifecycleResumed,
        TraceKind::LifecycleCompleted,
        TraceKind::ToolCalled,
        TraceKind::ToolResult,
        TraceKind::ContextComposed,
        TraceKind::MemoryRetrieved,
        TraceKind::SubagentSpawned,
        TraceKind::FileRead,
        TraceKind::FileChanged,
        TraceKind::TestExecuted,
        TraceKind::CapabilityDenied,
        TraceKind::CostObserved,
        TraceKind::CheckpointCreated,
        TraceKind::Error,
        TraceKind::Retry,
        TraceKind::ModelResponse,
    ];

    for (index, kind) in kinds.into_iter().enumerate() {
        let mut fields = BTreeMap::from([
            ("summary".to_owned(), format!("event {index}")),
            ("api_token".to_owned(), "unguarded-token".to_owned()),
            ("diagnostic".to_owned(), "known-secret".to_owned()),
            (
                "provider_output".to_owned(),
                "Bearer provider-token and sk-inline-token".to_owned(),
            ),
        ]);
        if kind == TraceKind::LifecycleCompleted {
            fields.insert("completion_reason".to_owned(), "success".to_owned());
        }
        let receipt = recorder
            .record_trace(
                TraceInput::new(
                    format!("trace-{index}"),
                    provenance.clone(),
                    kind,
                    i64::try_from(index).expect("trace index fits i64"),
                    fields,
                )
                .expect("trace input"),
            )
            .expect("record trace");
        assert_eq!(receipt.provenance, provenance);
        assert_eq!(receipt.redacted_fields, 3);
        let artifact = recorder
            .artifact(&receipt.artifact_id)
            .expect("trace artifact");
        let text = String::from_utf8(artifact).expect("UTF-8 trace artifact");
        assert!(!text.contains("known-secret"));
        assert!(!text.contains("unguarded-token"));
        assert!(!text.contains("provider-token"));
        assert!(!text.contains("sk-inline-token"));
        assert!(text.contains("[REDACTED]"));
    }

    let history = recorder.replay_verified().expect("verified trace history");
    assert_eq!(history.len(), 17);
    assert!(history.iter().all(|event| {
        event.event_type == "trace.recorded"
            && event.aggregate_id == "run:run-1"
            && !String::from_utf8_lossy(&event.payload).contains("known-secret")
    }));
}

#[test]
fn retention_and_record_size_limits_survive_restart() {
    let directory = tempdir().expect("evidence directory");
    let input = || {
        TraceInput::new(
            "trace-one",
            provenance(),
            TraceKind::LifecycleStarted,
            1,
            BTreeMap::new(),
        )
        .expect("trace input")
    };
    {
        let mut recorder = recorder(&directory, 1, 1_024);
        recorder.record_trace(input()).expect("first trace");
    }
    let mut reopened = recorder(&directory, 1, 1_024);
    assert!(matches!(
        reopened.record_trace(
            TraceInput::new(
                "trace-two",
                provenance(),
                TraceKind::Retry,
                2,
                BTreeMap::new()
            )
            .expect("second trace input")
        ),
        Err(ExperienceError::RetentionExceeded { maximum: 1 })
    ));
    assert!(matches!(
        reopened.record_experience(
            ExperienceInput::new(
                "experience-over-limit",
                provenance(),
                ExperienceKind::Observation,
                2,
                vec!["trace-one".to_owned()],
                Vec::new(),
                5_000,
                BTreeMap::new()
            )
            .expect("experience over limit input")
        ),
        Err(ExperienceError::RetentionExceeded { maximum: 1 })
    ));

    let other = tempdir().expect("oversize evidence directory");
    let mut bounded = recorder(&other, 2, 128);
    assert!(matches!(
        bounded.record_trace(
            TraceInput::new(
                "oversize",
                provenance(),
                TraceKind::ModelResponse,
                3,
                BTreeMap::from([("response".to_owned(), "x".repeat(1_000))])
            )
            .expect("oversize input")
        ),
        Err(ExperienceError::RecordTooLarge { .. })
    ));
    assert!(bounded.replay_verified().expect("empty history").is_empty());
}

#[test]
fn experiences_require_verified_sources_and_keep_contradictions_explicit() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 10, 16_384);
    let first = recorder
        .record_trace(trace("trace-source-1", 1))
        .expect("first source trace");
    recorder
        .record_trace(trace("trace-source-2", 2))
        .expect("second source trace");

    let observation = recorder
        .record_experience(
            ExperienceInput::new(
                "experience-observation",
                provenance(),
                ExperienceKind::Observation,
                3,
                vec!["trace-source-1".to_owned()],
                vec![first.artifact_id.clone()],
                8_000,
                BTreeMap::from([
                    (
                        "observation".to_owned(),
                        "tool retries increased".to_owned(),
                    ),
                    ("password".to_owned(), "known-secret".to_owned()),
                ]),
            )
            .expect("observation input"),
        )
        .expect("record observation");
    assert_eq!(observation.status, "unverified");
    assert_eq!(observation.redacted_fields, 1);
    assert!(
        !String::from_utf8(
            recorder
                .artifact(&observation.artifact_id)
                .expect("experience artifact")
        )
        .expect("UTF-8 experience artifact")
        .contains("known-secret")
    );

    let contradiction = recorder
        .record_experience(
            ExperienceInput::new(
                "experience-contradiction",
                provenance(),
                ExperienceKind::Contradiction,
                4,
                vec![
                    "experience-observation".to_owned(),
                    "trace-source-2".to_owned(),
                ],
                Vec::new(),
                6_000,
                BTreeMap::from([(
                    "conflict".to_owned(),
                    "retry evidence disagrees across tasks".to_owned(),
                )]),
            )
            .expect("contradiction input"),
        )
        .expect("record contradiction");
    assert_eq!(contradiction.status, "unverified");
    assert_eq!(contradiction.source_event_ids.len(), 2);

    assert!(matches!(
        recorder.record_experience(
            ExperienceInput::new(
                "unknown-source",
                provenance(),
                ExperienceKind::Hypothesis,
                5,
                vec!["missing-event".to_owned()],
                Vec::new(),
                5_000,
                BTreeMap::new()
            )
            .expect("unknown source input")
        ),
        Err(ExperienceError::UnknownSourceEvent(source)) if source == "missing-event"
    ));
    assert!(
        recorder
            .record_experience(
                ExperienceInput::new(
                    "missing-evidence",
                    provenance(),
                    ExperienceKind::Evidence,
                    6,
                    vec!["trace-source-1".to_owned()],
                    vec!["0".repeat(64)],
                    5_000,
                    BTreeMap::new()
                )
                .expect("missing evidence input")
            )
            .is_err()
    );
}

#[test]
fn trusted_experience_rehydrates_exact_redacted_evidence_after_restart() {
    let directory = tempdir().expect("evidence directory");
    let expected_receipt = {
        let mut recorder = recorder(&directory, 10, 16_384);
        let source = recorder
            .record_trace(trace("hypothesis-source", 1))
            .expect("record source");
        recorder
            .record_experience(
                ExperienceInput::new(
                    "hypothesis-1",
                    provenance(),
                    ExperienceKind::Hypothesis,
                    2,
                    vec!["hypothesis-source".to_owned()],
                    vec![source.artifact_id],
                    8_300,
                    BTreeMap::from([
                        (
                            "failure_class".to_owned(),
                            "premature_completion".to_owned(),
                        ),
                        (
                            "proposed_change".to_owned(),
                            "verify before finish".to_owned(),
                        ),
                        ("api_token".to_owned(), "known-secret".to_owned()),
                    ]),
                )
                .expect("hypothesis input"),
            )
            .expect("record hypothesis")
    };

    let recorder = recorder(&directory, 10, 16_384);
    let (events, artifacts) = recorder.into_stores();
    let expected_hash = events
        .replay_verified()
        .expect("verified history")
        .into_iter()
        .find(|event| event.event_id == "hypothesis-1")
        .map(|event| encode_hash(event.hash))
        .expect("experience event");
    let trusted = rehydrate_experience(&events, &artifacts, "hypothesis-1")
        .expect("rehydrate trusted hypothesis");

    assert_eq!(trusted.experience_id(), expected_receipt.experience_id);
    assert_eq!(trusted.provenance(), &expected_receipt.provenance);
    assert_eq!(trusted.kind(), ExperienceKind::Hypothesis);
    assert_eq!(trusted.confidence_bps(), 8_300);
    assert_eq!(trusted.timestamp_millis(), 2);
    assert_eq!(trusted.source_event_ids(), ["hypothesis-source"]);
    assert_eq!(trusted.evidence_artifact_ids().len(), 1);
    assert_eq!(trusted.status(), "unverified");
    assert_eq!(trusted.fields()["failure_class"], "premature_completion");
    assert_eq!(trusted.fields()["api_token"], "[REDACTED]");
    assert_eq!(trusted.event_hash(), expected_hash);
    assert!(
        trusted
            .event_hash()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    let debug = format!("{trusted:?}");
    assert!(!debug.contains("known-secret"));
}

#[test]
fn trusted_experience_rehydrates_experience_source_chains() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 10, 16_384);
    recorder
        .record_trace(trace("chain-trace", 1))
        .expect("record trace");
    recorder
        .record_experience(
            ExperienceInput::new(
                "chain-observation",
                provenance(),
                ExperienceKind::Observation,
                2,
                vec!["chain-trace".to_owned()],
                Vec::new(),
                6_000,
                BTreeMap::new(),
            )
            .expect("observation input"),
        )
        .expect("record observation");
    recorder
        .record_experience(
            ExperienceInput::new(
                "chain-hypothesis",
                provenance(),
                ExperienceKind::Hypothesis,
                3,
                vec!["chain-observation".to_owned()],
                Vec::new(),
                7_000,
                BTreeMap::new(),
            )
            .expect("hypothesis input"),
        )
        .expect("record hypothesis");
    let (events, artifacts) = recorder.into_stores();

    let trusted = rehydrate_experience(&events, &artifacts, "chain-hypothesis")
        .expect("rehydrate chained hypothesis");
    assert_eq!(trusted.source_event_ids(), ["chain-observation"]);
}

#[test]
fn trusted_experience_rehydrates_shared_ancestry_without_recursive_expansion() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 64, 16_384);
    recorder
        .record_trace(trace("shared-root", 1))
        .expect("record root trace");
    recorder
        .record_experience(
            ExperienceInput::new(
                "shared-0",
                provenance(),
                ExperienceKind::Observation,
                2,
                vec!["shared-root".to_owned()],
                Vec::new(),
                6_000,
                BTreeMap::new(),
            )
            .expect("first shared input"),
        )
        .expect("record first shared experience");
    recorder
        .record_experience(
            ExperienceInput::new(
                "shared-1",
                provenance(),
                ExperienceKind::Observation,
                3,
                vec!["shared-0".to_owned()],
                Vec::new(),
                6_000,
                BTreeMap::new(),
            )
            .expect("second shared input"),
        )
        .expect("record second shared experience");
    for index in 2..40 {
        recorder
            .record_experience(
                ExperienceInput::new(
                    format!("shared-{index}"),
                    provenance(),
                    ExperienceKind::Observation,
                    i64::from(index) + 2,
                    vec![
                        format!("shared-{}", index - 1),
                        format!("shared-{}", index - 2),
                    ],
                    Vec::new(),
                    6_000,
                    BTreeMap::new(),
                )
                .expect("shared ancestry input"),
            )
            .expect("record shared ancestry");
    }
    let (events, artifacts) = recorder.into_stores();

    let trusted =
        rehydrate_experience(&events, &artifacts, "shared-39").expect("rehydrate shared ancestry");
    assert_eq!(trusted.source_event_ids(), ["shared-38", "shared-37"]);
}

#[test]
fn trusted_experience_rejects_invalid_deserialized_receipt_semantics() {
    for case in [
        "schema",
        "status",
        "sources",
        "duplicate-source",
        "confidence",
        "contradiction",
    ] {
        let directory = tempdir().expect("evidence directory");
        let mut events =
            EventStore::open(directory.path().join("events.sqlite3")).expect("event store");
        let artifacts =
            ArtifactStore::open(directory.path().join("artifacts")).expect("artifact store");
        let mut receipt = experience_receipt(
            "invalid-receipt",
            provenance(),
            vec!["source".to_owned()],
            Vec::new(),
            8_000,
            &"0".repeat(64),
        );
        match case {
            "schema" => receipt.schema_version = 2,
            "status" => receipt.status = "verified".to_owned(),
            "sources" => receipt.source_event_ids.clear(),
            "duplicate-source" => receipt.source_event_ids.push("source".to_owned()),
            "confidence" => receipt.confidence_bps = 0,
            "contradiction" => receipt.kind = ExperienceKind::Contradiction,
            _ => unreachable!(),
        }
        events
            .append(EventInput::new(
                "invalid-receipt",
                "experience:invalid-receipt",
                "experience.recorded",
                "experience-plane",
                2,
                serde_json::to_vec(&receipt).expect("encode receipt"),
            ))
            .expect("append invalid receipt");
        assert!(matches!(
            rehydrate_experience(&events, &artifacts, "invalid-receipt"),
            Err(ExperienceError::InvalidStoredExperience(
                "event receipt metadata"
            ))
        ));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn trusted_experience_rejects_forged_trace_envelopes_and_artifacts() {
    for case in [
        "noncanonical-receipt",
        "receipt-provenance",
        "receipt-schema",
        "receipt-event-id",
        "receipt-aggregate",
        "receipt-actor",
        "artifact-schema",
        "artifact-event-id",
        "artifact-provenance",
        "artifact-kind",
        "artifact-timestamp",
        "artifact-redacted-count",
    ] {
        let directory = tempdir().expect("evidence directory");
        let mut events =
            EventStore::open(directory.path().join("events.sqlite3")).expect("event store");
        let artifacts =
            ArtifactStore::open(directory.path().join("artifacts")).expect("artifact store");
        let artifact_provenance = if case == "artifact-provenance" {
            Provenance::new("run-2", "genome-2", "world-2").expect("foreign provenance")
        } else {
            provenance()
        };
        let trace_artifact = artifacts
            .put(&trace_artifact_bytes(
                if case == "artifact-schema" { 2 } else { 1 },
                if case == "artifact-event-id" {
                    "other-trace"
                } else {
                    "forged-trace"
                },
                &artifact_provenance,
                if case == "artifact-kind" {
                    TraceKind::Retry
                } else {
                    TraceKind::Error
                },
                if case == "artifact-timestamp" { 9 } else { 1 },
            ))
            .expect("store trace artifact");
        let mut trace_receipt = TraceReceipt {
            schema_version: if case == "receipt-schema" { 2 } else { 1 },
            event_id: if case == "receipt-event-id" {
                "other-trace".to_owned()
            } else {
                "forged-trace".to_owned()
            },
            provenance: if case == "receipt-provenance" {
                serde_json::from_value(serde_json::json!({
                    "run_id": "",
                    "genome_id": "genome-1",
                    "world_id": "world-1"
                }))
                .expect("deserialize invalid provenance fixture")
            } else {
                provenance()
            },
            kind: TraceKind::Error,
            artifact_id: trace_artifact.as_str().to_owned(),
            redacted_fields: 0,
        };
        if case == "artifact-redacted-count" {
            trace_receipt.redacted_fields = 1;
        }
        let mut trace_payload = serde_json::to_vec(&trace_receipt).expect("encode trace receipt");
        if case == "noncanonical-receipt" {
            trace_payload.push(b' ');
        }
        events
            .append(EventInput::new(
                "forged-trace",
                if case == "receipt-aggregate" {
                    "run:other"
                } else {
                    "run:run-1"
                },
                "trace.recorded",
                if case == "receipt-actor" {
                    "candidate-plane"
                } else {
                    "experience-plane"
                },
                1,
                trace_payload,
            ))
            .expect("append forged trace");
        let experience_artifact = artifacts
            .put(&experience_artifact_bytes(
                "trace-dependent",
                &provenance(),
                2,
                &["forged-trace"],
                &[],
                8_000,
            ))
            .expect("store experience artifact");
        let receipt = experience_receipt(
            "trace-dependent",
            provenance(),
            vec!["forged-trace".to_owned()],
            Vec::new(),
            8_000,
            experience_artifact.as_str(),
        );
        events
            .append(EventInput::new(
                "trace-dependent",
                "experience:trace-dependent",
                "experience.recorded",
                "experience-plane",
                2,
                serde_json::to_vec(&receipt).expect("encode experience receipt"),
            ))
            .expect("append dependent experience");

        let result = rehydrate_experience(&events, &artifacts, "trace-dependent");
        if case == "receipt-provenance" {
            assert!(matches!(result, Err(ExperienceError::InvalidInput(_))));
        } else {
            let expected = match case {
                "noncanonical-receipt" => "source receipt is not canonical JSON",
                "receipt-schema" | "receipt-event-id" | "receipt-aggregate" | "receipt-actor" => {
                    "trace source metadata"
                }
                _ => "trace source artifact mismatch",
            };
            assert!(matches!(
                result,
                Err(ExperienceError::InvalidStoredExperience(message)) if message == expected
            ));
        }
    }
}

#[test]
fn trusted_experience_rejects_noncanonical_receipt_and_unknown_exact_source() {
    let directory = tempdir().expect("evidence directory");
    let mut events =
        EventStore::open(directory.path().join("events.sqlite3")).expect("event store");
    let artifacts =
        ArtifactStore::open(directory.path().join("artifacts")).expect("artifact store");
    let receipt = experience_receipt(
        "noncanonical",
        provenance(),
        vec!["source".to_owned()],
        Vec::new(),
        8_000,
        &"0".repeat(64),
    );
    let mut payload = serde_json::to_vec(&receipt).expect("encode receipt");
    payload.push(b' ');
    events
        .append(EventInput::new(
            "noncanonical",
            "experience:noncanonical",
            "experience.recorded",
            "experience-plane",
            1,
            payload,
        ))
        .expect("append noncanonical receipt");
    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "noncanonical"),
        Err(ExperienceError::InvalidStoredExperience(
            "receipt is not canonical JSON"
        ))
    ));

    let other = tempdir().expect("unknown source directory");
    let mut known_recorder = recorder(&other, 10, 16_384);
    known_recorder
        .record_trace(trace("known-source", 1))
        .expect("record known source");
    let (mut events, artifacts) = known_recorder.into_stores();
    let artifact_id = artifacts
        .put(&experience_artifact_bytes(
            "unknown-exact-source",
            &provenance(),
            2,
            &["missing-source"],
            &[],
            8_000,
        ))
        .expect("store experience artifact");
    let receipt = experience_receipt(
        "unknown-exact-source",
        provenance(),
        vec!["missing-source".to_owned()],
        Vec::new(),
        8_000,
        artifact_id.as_str(),
    );
    events
        .append(EventInput::new(
            "unknown-exact-source",
            "experience:unknown-exact-source",
            "experience.recorded",
            "experience-plane",
            2,
            serde_json::to_vec(&receipt).expect("encode receipt"),
        ))
        .expect("append unknown source receipt");
    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "unknown-exact-source"),
        Err(ExperienceError::UnknownSourceEvent(id)) if id == "missing-source"
    ));
}

#[test]
fn trusted_experience_rejects_noncanonical_primary_artifact() {
    let third = tempdir().expect("noncanonical artifact directory");
    let mut recorder = recorder(&third, 10, 16_384);
    recorder
        .record_trace(trace("artifact-source", 1))
        .expect("record artifact source");
    let (mut events, artifacts) = recorder.into_stores();
    let mut artifact = experience_artifact_bytes(
        "noncanonical-artifact",
        &provenance(),
        2,
        &["artifact-source"],
        &[],
        8_000,
    );
    artifact.push(b' ');
    let artifact_id = artifacts
        .put(&artifact)
        .expect("store noncanonical artifact");
    let receipt = experience_receipt(
        "noncanonical-artifact",
        provenance(),
        vec!["artifact-source".to_owned()],
        Vec::new(),
        8_000,
        artifact_id.as_str(),
    );
    events
        .append(EventInput::new(
            "noncanonical-artifact",
            "experience:noncanonical-artifact",
            "experience.recorded",
            "experience-plane",
            2,
            serde_json::to_vec(&receipt).expect("encode receipt"),
        ))
        .expect("append noncanonical artifact receipt");
    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "noncanonical-artifact"),
        Err(ExperienceError::InvalidStoredExperience(
            "artifact is not canonical JSON"
        ))
    ));
}

#[test]
fn trusted_experience_rejects_missing_and_tampered_primary_artifact() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 10, 16_384);
    recorder
        .record_trace(trace("tamper-source", 1))
        .expect("record source");
    let receipt = recorder
        .record_experience(
            ExperienceInput::new(
                "tamper-hypothesis",
                provenance(),
                ExperienceKind::Hypothesis,
                2,
                vec!["tamper-source".to_owned()],
                Vec::new(),
                7_500,
                BTreeMap::from([("hypothesis".to_owned(), "bounded retry".to_owned())]),
            )
            .expect("hypothesis input"),
        )
        .expect("record hypothesis");
    let (events, artifacts) = recorder.into_stores();
    let artifact_id = ArtifactId::parse(receipt.artifact_id).expect("artifact ID");
    let cas = ArtifactStore::open(directory.path().join("artifacts")).expect("reopen CAS root");
    std::fs::write(cas.path_for(&artifact_id), b"tampered").expect("tamper artifact");

    assert!(rehydrate_experience(&events, &artifacts, "tamper-hypothesis").is_err());
    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "missing-hypothesis"),
        Err(ExperienceError::UnknownExperience(id)) if id == "missing-hypothesis"
    ));
}

#[test]
fn trusted_experience_rejects_forged_envelope_and_receipt_artifact_mismatch() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 10, 16_384);
    recorder
        .record_trace(trace("forgery-source", 1))
        .expect("record source");
    let (mut events, artifacts) = recorder.into_stores();
    let artifact_id = artifacts
        .put(&experience_artifact_bytes(
            "forged-hypothesis",
            &provenance(),
            2,
            &["forgery-source"],
            &[],
            8_000,
        ))
        .expect("store forged artifact");
    let receipt = experience_receipt(
        "forged-hypothesis",
        provenance(),
        vec!["forgery-source".to_owned()],
        Vec::new(),
        7_000,
        artifact_id.as_str(),
    );
    events
        .append(EventInput::new(
            "forged-hypothesis",
            "experience:forged-hypothesis",
            "experience.recorded",
            "experience-plane",
            2,
            serde_json::to_vec(&receipt).expect("encode receipt"),
        ))
        .expect("append inconsistent receipt");
    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "forged-hypothesis"),
        Err(ExperienceError::InvalidStoredExperience(
            "experience artifact mismatch"
        ))
    ));

    let other = tempdir().expect("forged envelope directory");
    let mut events = EventStore::open(other.path().join("events.sqlite3")).expect("event store");
    let artifacts = ArtifactStore::open(other.path().join("artifacts")).expect("artifact store");
    let receipt = experience_receipt(
        "wrong-actor",
        provenance(),
        vec!["missing-source".to_owned()],
        Vec::new(),
        8_000,
        &"0".repeat(64),
    );
    events
        .append(EventInput::new(
            "wrong-actor",
            "experience:wrong-actor",
            "experience.recorded",
            "candidate-plane",
            1,
            serde_json::to_vec(&receipt).expect("encode receipt"),
        ))
        .expect("append wrong actor");
    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "wrong-actor"),
        Err(ExperienceError::InvalidStoredExperience(
            "event receipt metadata"
        ))
    ));
}

#[test]
fn trusted_experience_rejects_missing_source_and_evidence_artifacts() {
    for missing_source in [true, false] {
        let directory = tempdir().expect("evidence directory");
        let mut recorder = recorder(&directory, 10, 16_384);
        let source = recorder
            .record_trace(trace("transitive-source", 1))
            .expect("record source");
        let extra = {
            let (events, artifacts) = recorder.into_stores();
            let extra = artifacts
                .put(b"independent evidence")
                .expect("put evidence");
            recorder = EvidenceRecorder::from_stores(
                events,
                artifacts,
                RedactionPolicy::new(["known-secret".to_owned()]),
                RetentionLimits::new(10, 16_384).expect("retention limits"),
            );
            extra
        };
        recorder
            .record_experience(
                ExperienceInput::new(
                    "transitive-hypothesis",
                    provenance(),
                    ExperienceKind::Hypothesis,
                    2,
                    vec!["transitive-source".to_owned()],
                    vec![extra.as_str().to_owned()],
                    8_000,
                    BTreeMap::new(),
                )
                .expect("hypothesis input"),
            )
            .expect("record hypothesis");
        let (events, artifacts) = recorder.into_stores();
        let removed = if missing_source {
            ArtifactId::parse(source.artifact_id).expect("source artifact")
        } else {
            extra
        };
        let cas = ArtifactStore::open(directory.path().join("artifacts")).expect("reopen CAS root");
        std::fs::remove_file(cas.path_for(&removed)).expect("remove evidence");
        assert!(rehydrate_experience(&events, &artifacts, "transitive-hypothesis").is_err());
    }
}

#[test]
fn trusted_experience_rejects_source_provenance_mismatch() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 10, 16_384);
    recorder
        .record_trace(
            TraceInput::new(
                "foreign-source",
                Provenance::new("run-2", "genome-2", "world-2").expect("foreign provenance"),
                TraceKind::Error,
                1,
                BTreeMap::new(),
            )
            .expect("foreign trace input"),
        )
        .expect("record foreign source");
    let (mut events, artifacts) = recorder.into_stores();
    let artifact_id = artifacts
        .put(&experience_artifact_bytes(
            "forged-provenance",
            &provenance(),
            2,
            &["foreign-source"],
            &[],
            8_000,
        ))
        .expect("store experience artifact");
    let receipt = experience_receipt(
        "forged-provenance",
        provenance(),
        vec!["foreign-source".to_owned()],
        Vec::new(),
        8_000,
        artifact_id.as_str(),
    );
    events
        .append(EventInput::new(
            "forged-provenance",
            "experience:forged-provenance",
            "experience.recorded",
            "experience-plane",
            2,
            serde_json::to_vec(&receipt).expect("encode receipt"),
        ))
        .expect("append forged provenance");

    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "forged-provenance"),
        Err(ExperienceError::InvalidStoredExperience(
            "source provenance mismatch"
        ))
    ));
}

#[test]
fn trusted_experience_rejects_forward_source_reference() {
    let directory = tempdir().expect("evidence directory");
    let mut events =
        EventStore::open(directory.path().join("events.sqlite3")).expect("event store");
    let artifacts =
        ArtifactStore::open(directory.path().join("artifacts")).expect("artifact store");
    let artifact_id = artifacts
        .put(&experience_artifact_bytes(
            "forward-reference",
            &provenance(),
            1,
            &["future-source"],
            &[],
            8_000,
        ))
        .expect("store experience artifact");
    let receipt = experience_receipt(
        "forward-reference",
        provenance(),
        vec!["future-source".to_owned()],
        Vec::new(),
        8_000,
        artifact_id.as_str(),
    );
    events
        .append(EventInput::new(
            "forward-reference",
            "experience:forward-reference",
            "experience.recorded",
            "experience-plane",
            1,
            serde_json::to_vec(&receipt).expect("encode receipt"),
        ))
        .expect("append forward-reference target");
    let mut recorder = EvidenceRecorder::from_stores(
        Box::new(events),
        Box::new(artifacts),
        RedactionPolicy::new(["known-secret".to_owned()]),
        RetentionLimits::new(10, 16_384).expect("retention limits"),
    );
    recorder
        .record_trace(trace("future-source", 2))
        .expect("record future source");
    let (events, artifacts) = recorder.into_stores();

    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "forward-reference"),
        Err(ExperienceError::InvalidStoredExperience(
            "source does not precede experience"
        ))
    ));
}

#[test]
fn trusted_experience_rejects_unrelated_source_event_types() {
    let directory = tempdir().expect("evidence directory");
    let mut events =
        EventStore::open(directory.path().join("events.sqlite3")).expect("event store");
    let artifacts =
        ArtifactStore::open(directory.path().join("artifacts")).expect("artifact store");
    events
        .append(EventInput::new(
            "control-source",
            "control:global",
            "control.freeze",
            "operator",
            1,
            br#"{"reason":"maintenance"}"#,
        ))
        .expect("append unrelated source");
    let artifact_id = artifacts
        .put(&experience_artifact_bytes(
            "unrelated-rehydration",
            &provenance(),
            2,
            &["control-source"],
            &[],
            8_000,
        ))
        .expect("store experience artifact");
    let receipt = experience_receipt(
        "unrelated-rehydration",
        provenance(),
        vec!["control-source".to_owned()],
        Vec::new(),
        8_000,
        artifact_id.as_str(),
    );
    events
        .append(EventInput::new(
            "unrelated-rehydration",
            "experience:unrelated-rehydration",
            "experience.recorded",
            "experience-plane",
            2,
            serde_json::to_vec(&receipt).expect("encode receipt"),
        ))
        .expect("append experience");

    assert!(matches!(
        rehydrate_experience(&events, &artifacts, "unrelated-rehydration"),
        Err(ExperienceError::InvalidStoredExperience(
            "source is not experience evidence"
        ))
    ));
}

#[test]
fn experience_cannot_claim_provenance_different_from_its_source() {
    let directory = tempdir().expect("evidence directory");
    let mut recorder = recorder(&directory, 10, 16_384);
    recorder
        .record_trace(
            TraceInput::new(
                "other-provenance",
                Provenance::new("run-2", "genome-2", "world-2").expect("other provenance"),
                TraceKind::Error,
                1,
                BTreeMap::new(),
            )
            .expect("other provenance trace"),
        )
        .expect("record other provenance trace");
    assert!(matches!(
        recorder.record_experience(
            ExperienceInput::new(
                "forged-provenance",
                provenance(),
                ExperienceKind::Hypothesis,
                2,
                vec!["other-provenance".to_owned()],
                Vec::new(),
                5_000,
                BTreeMap::new()
            )
            .expect("forged provenance input")
        ),
        Err(ExperienceError::InvalidInput(_))
    ));
}

#[test]
fn experience_cannot_cite_unrelated_ledger_events() {
    let directory = tempdir().expect("evidence directory");
    let database = directory.path().join("events.sqlite3");
    {
        let mut events = EventStore::open(&database).expect("open event store");
        events
            .append(EventInput::new(
                "control-source",
                "control:global",
                "control.freeze",
                "operator",
                1,
                br#"{"reason":"maintenance"}"#,
            ))
            .expect("append unrelated event");
    }

    let mut recorder = recorder(&directory, 10, 16_384);
    assert!(matches!(
        recorder.record_experience(
            ExperienceInput::new(
                "unrelated-source",
                provenance(),
                ExperienceKind::Hypothesis,
                2,
                vec!["control-source".to_owned()],
                Vec::new(),
                5_000,
                BTreeMap::new(),
            )
            .expect("unrelated source input")
        ),
        Err(ExperienceError::InvalidInput(_))
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn invalid_provenance_experience_shapes_and_limits_fail_closed() {
    assert!(Provenance::new("", "genome", "world").is_err());
    assert!(Provenance::new("run", "g".repeat(257), "world").is_err());
    let valid = provenance();
    assert_eq!(valid.run_id(), "run-1");
    assert_eq!(valid.genome_id(), "genome-1");
    assert_eq!(valid.world_id(), "world-1");
    assert!(RetentionLimits::new(0, 1).is_err());
    assert!(RetentionLimits::new(1, 0).is_err());
    assert!(TraceInput::new("", provenance(), TraceKind::Error, 1, BTreeMap::new()).is_err());
    let too_many_fields: BTreeMap<_, _> = (0..65)
        .map(|index| (format!("field-{index}"), String::new()))
        .collect();
    assert!(
        TraceInput::new(
            "too-many-fields",
            provenance(),
            TraceKind::Error,
            1,
            too_many_fields
        )
        .is_err()
    );
    assert!(
        TraceInput::new(
            "empty-field-key",
            provenance(),
            TraceKind::Error,
            1,
            BTreeMap::from([(String::new(), "value".to_owned())])
        )
        .is_err()
    );
    assert!(
        ExperienceInput::new(
            "experience",
            provenance(),
            ExperienceKind::Observation,
            1,
            Vec::new(),
            Vec::new(),
            1,
            BTreeMap::new()
        )
        .is_err()
    );
    assert!(
        ExperienceInput::new(
            "contradiction",
            provenance(),
            ExperienceKind::Contradiction,
            1,
            vec!["only-one".to_owned()],
            Vec::new(),
            1,
            BTreeMap::new()
        )
        .is_err()
    );
    assert!(
        ExperienceInput::new(
            "duplicate-sources",
            provenance(),
            ExperienceKind::Observation,
            1,
            vec!["same-source".to_owned(), "same-source".to_owned()],
            Vec::new(),
            1,
            BTreeMap::new()
        )
        .is_err()
    );
    assert!(
        ExperienceInput::new(
            "confidence",
            provenance(),
            ExperienceKind::Hypothesis,
            1,
            vec!["source".to_owned()],
            Vec::new(),
            0,
            BTreeMap::new()
        )
        .is_err()
    );
    assert!(
        ExperienceInput::new(
            "",
            provenance(),
            ExperienceKind::Hypothesis,
            1,
            vec!["source".to_owned()],
            Vec::new(),
            1,
            BTreeMap::new()
        )
        .is_err()
    );
    assert!(
        ExperienceInput::new(
            "blank-source",
            provenance(),
            ExperienceKind::Hypothesis,
            1,
            vec![String::new()],
            Vec::new(),
            1,
            BTreeMap::new()
        )
        .is_err()
    );
}

fn recorder(
    directory: &tempfile::TempDir,
    maximum_records: usize,
    maximum_bytes: usize,
) -> EvidenceRecorder {
    EvidenceRecorder::open(
        directory.path().join("events.sqlite3"),
        directory.path().join("artifacts"),
        RedactionPolicy::new(["known-secret".to_owned()]),
        RetentionLimits::new(maximum_records, maximum_bytes).expect("retention limits"),
    )
    .expect("open evidence recorder")
}

fn provenance() -> Provenance {
    Provenance::new("run-1", "genome-1", "world-1").expect("provenance")
}

fn trace(event_id: &str, timestamp_millis: i64) -> TraceInput {
    TraceInput::new(
        event_id,
        provenance(),
        TraceKind::Error,
        timestamp_millis,
        BTreeMap::from([("error".to_owned(), "injected failure".to_owned())]),
    )
    .expect("trace input")
}

fn experience_receipt(
    experience_id: &str,
    provenance: Provenance,
    source_event_ids: Vec<String>,
    evidence_artifact_ids: Vec<String>,
    confidence_bps: u16,
    artifact_id: &str,
) -> ExperienceReceipt {
    ExperienceReceipt {
        schema_version: 1,
        experience_id: experience_id.to_owned(),
        provenance,
        kind: ExperienceKind::Hypothesis,
        source_event_ids,
        evidence_artifact_ids,
        confidence_bps,
        artifact_id: artifact_id.to_owned(),
        status: "unverified".to_owned(),
        redacted_fields: 0,
    }
}

fn experience_artifact_bytes(
    experience_id: &str,
    provenance: &Provenance,
    timestamp_millis: i64,
    source_event_ids: &[&str],
    evidence_artifact_ids: &[&str],
    confidence_bps: u16,
) -> Vec<u8> {
    #[derive(serde::Serialize)]
    struct Artifact<'a> {
        schema_version: u16,
        experience_id: &'a str,
        provenance: &'a Provenance,
        kind: ExperienceKind,
        timestamp_millis: i64,
        source_event_ids: &'a [&'a str],
        evidence_artifact_ids: &'a [&'a str],
        confidence_bps: u16,
        fields: BTreeMap<String, String>,
        status: &'a str,
    }
    serde_json::to_vec(&Artifact {
        schema_version: 1,
        experience_id,
        provenance,
        kind: ExperienceKind::Hypothesis,
        timestamp_millis,
        source_event_ids,
        evidence_artifact_ids,
        confidence_bps,
        fields: BTreeMap::new(),
        status: "unverified",
    })
    .expect("encode experience artifact")
}

fn trace_artifact_bytes(
    schema_version: u16,
    event_id: &str,
    provenance: &Provenance,
    kind: TraceKind,
    timestamp_millis: i64,
) -> Vec<u8> {
    #[derive(serde::Serialize)]
    struct Artifact<'a> {
        schema_version: u16,
        event_id: &'a str,
        provenance: &'a Provenance,
        kind: TraceKind,
        timestamp_millis: i64,
        fields: BTreeMap<String, String>,
    }
    serde_json::to_vec(&Artifact {
        schema_version,
        event_id,
        provenance,
        kind,
        timestamp_millis,
        fields: BTreeMap::new(),
    })
    .expect("encode trace artifact")
}

fn encode_hash(hash: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(64);
    for byte in hash {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}
