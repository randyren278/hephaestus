use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use hephaestus_arena::TrustedTask;
use hephaestus_experience::{Provenance, TraceInput, TraceKind, TraceReceipt};
use hephaestus_genome::{SourceFormat, compile_world};
use hephaestus_ledger::{EventInput, EventStore};
use tempfile::{TempDir, tempdir};

use super::*;
use crate::{
    ApiError, CanaryRecord, CanaryStage, CanaryTransitionKind, CanaryTransitionPayload,
    CanaryTransitionRecord, ChampionRecord, ChampionTransitionKind, ChampionTransitionPayload,
    ChampionTransitionRecord, DriftAdaptationFinishReason, DriftKind, DriftRecord,
    DriftRecordPayload, GeneExtractedPayload, GeneRecord, GeneTransferAppliedPayload,
    GeneTransferOutcome, GeneTransferRecordedPayload, MetaLineageSpec,
};

#[test]
fn forge_mutation_changes_only_the_supported_compact_instruction_token() {
    let canonical = reference_instruction_document(ReferenceInstruction::Identity);
    let framed = format!("{canonical}\n");
    let expected = format!(
        "{}\n",
        reference_instruction_document(ReferenceInstruction::AsciiUppercase)
    );
    assert_eq!(
        mutate_reference_instruction_document(
            &framed,
            ReferenceInstruction::Identity,
            ReferenceInstruction::AsciiUppercase
        ),
        Ok(expected)
    );
    for extra in [
        format!("context\n{canonical}"),
        format!("{canonical}\ncontext"),
    ] {
        assert!(ReferenceInstruction::parse(&extra).is_err());
        assert!(
            mutate_reference_instruction_document(
                &extra,
                ReferenceInstruction::Identity,
                ReferenceInstruction::AsciiUppercase
            )
            .is_err()
        );
    }
    let reformatted = canonical.replace(',', ", ");
    assert!(ReferenceInstruction::parse(&reformatted).is_ok());
    assert!(
        mutate_reference_instruction_document(
            &reformatted,
            ReferenceInstruction::Identity,
            ReferenceInstruction::AsciiUppercase
        )
        .is_err()
    );
}

fn forge_history_with_payload_edit(
    history: &[StoredEvent],
    forge_event_id: &str,
    edit: impl FnOnce(&mut ForgeProposalPayload),
) -> Vec<StoredEvent> {
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == forge_event_id)
        .expect("Forge event exists in canonical history");
    let mut payload: ForgeProposalPayload =
        serde_json::from_slice(&event.payload).expect("decode canonical Forge payload");
    edit(&mut payload);
    let canonical = serde_json::to_value(&payload).expect("canonicalize tampered Forge payload");
    event.payload = serde_json::to_vec(&canonical).expect("encode tampered Forge payload");
    tampered
}

fn forge_history_with_selection_edit(
    history: &[StoredEvent],
    selection_event_id: &str,
    edit: impl FnOnce(&mut StoredEvent),
) -> Vec<StoredEvent> {
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == selection_event_id)
        .expect("selection event exists in canonical history");
    edit(event);
    tampered
}

fn complete_arena_test_job(
    plane: &mut ControlPlane,
    evaluation_id: &str,
    parent_genome_id: &str,
    candidate_genome_id: &str,
) {
    assert!(matches!(
        plane
            .submit_arena_job(evaluation_id, parent_genome_id, candidate_genome_id, false)
            .expect("admit Arena evidence job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    drain_active_arena_test_job(plane, evaluation_id);
}

fn drain_active_arena_test_job(plane: &mut ControlPlane, evaluation_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist Arena trials and score evidence");
        assert!(
            Instant::now() < deadline,
            "Arena evidence job did not finish"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(
        plane.state.arena_jobs[evaluation_id].terminal,
        Some(JobTerminal::Succeeded)
    );
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create copied control-plane directory");
    for entry in fs::read_dir(source).expect("read control-plane fixture") {
        let entry = entry.expect("read control-plane fixture entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_directory(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).expect("copy control-plane fixture file");
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn forge_assessment_records_verified_child_selection_and_replays() {
    let directory = tempdir().expect("Forge assessment fixture");
    let (mut plane, initial_parent, initial_candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();

    for (request_id, assessment_id, proposal_id, selection_event_id, expected) in [
        (
            "assess-invalid-id",
            "invalid id",
            "proposal",
            "selection:missing",
            "assessment_id is invalid",
        ),
        (
            "assess-invalid-proposal",
            "assessment-valid",
            "invalid proposal",
            "selection:missing",
            "proposal_id is invalid",
        ),
        (
            "assess-empty-selection",
            "assessment-valid",
            "proposal",
            "",
            "selection_event_id is required",
        ),
    ] {
        let response = dispatch_call(
            &mut plane,
            &token,
            request_id,
            Command::GenomeAssess {
                assessment_id: assessment_id.to_owned(),
                proposal_id: proposal_id.to_owned(),
                selection_event_id: selection_event_id.to_owned(),
            },
        );
        assert!(matches!(
            response.error,
            Some(error) if error.code == ApiErrorCode::InvalidRequest && error.message == expected
        ));
    }

    complete_arena_test_job(
        &mut plane,
        "assessment-source-evaluation",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
    );
    let ResponseData::Selection {
        selection: source_selection,
    } = plane
        .select_arena_evaluation("assessment-source-evaluation")
        .expect("select source candidate for proposal")
    else {
        panic!("source selection should produce a receipt");
    };
    let ResponseData::ForgeProposal { proposal } = plane
        .propose_genome(
            "assessment-proposal",
            &source_selection.event.event_id,
            &initial_candidate.genome_id,
            "Assess a single causal prompt mutation.",
        )
        .expect("record compiler-backed Forge proposal")
    else {
        panic!("proposal should return its durable record");
    };
    let ResponseData::ForgeProposal {
        proposal: sibling_proposal,
    } = plane
        .propose_genome(
            "assessment-sibling-proposal",
            &source_selection.event.event_id,
            &initial_candidate.genome_id,
            "Assess a distinct child of the same selected parent.",
        )
        .expect("record sibling Forge proposal for directed-pair rejection")
    else {
        panic!("sibling proposal should return its durable record");
    };

    let child = proposal.payload.child.clone();
    assert!(matches!(
        plane.assess_genome(
            "assessment-missing-proposal",
            "missing-proposal",
            &proposal.event.event_id
        ),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.assess_genome(
            "assessment-missing-selection",
            "assessment-proposal",
            "selection:missing"
        ),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.assess_genome("assessment-wrong-event", "assessment-proposal", &proposal.event.event_id),
        Err(ExecuteError::Rejected(message))
            if message == "selection_event_id does not identify a selection"
    ));
    assert!(matches!(
        plane
            .submit_arena_job(
                "assessment-child-evaluation",
                &initial_candidate.genome_id,
                &child.genome_id, false)
            .expect("admit child evaluation"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let busy = dispatch_call(
        &mut plane,
        &token,
        "assess-while-active",
        Command::GenomeAssess {
            assessment_id: "assessment-while-active".to_owned(),
            proposal_id: "assessment-proposal".to_owned(),
            selection_event_id: "selection:pending".to_owned(),
        },
    );
    assert_eq!(
        busy.error.expect("active Arena blocks assessment").code,
        ApiErrorCode::Busy
    );
    // The handlers keep their own guard behind the dispatcher's.
    assert!(matches!(
        plane.assess_genome(
            "assessment-while-active",
            "assessment-proposal",
            "selection:pending"
        ),
        Err(ExecuteError::Busy)
    ));
    assert!(matches!(
        plane.transition_champion(
            "rollback-while-active",
            &ChampionRequest::Rollback {
                world_id: "world".to_owned(),
                reason: "Active job.".to_owned(),
            }
        ),
        Err(ExecuteError::Busy)
    ));
    drain_active_arena_test_job(&mut plane, "assessment-child-evaluation");
    let ResponseData::Selection {
        selection: child_selection,
    } = plane
        .select_arena_evaluation("assessment-child-evaluation")
        .expect("select child evaluation receipt")
    else {
        panic!("child selection should produce a receipt");
    };
    assert_eq!(
        child_selection.receipt.candidate_genome_id(),
        child.genome_id
    );

    let out_of_order = dispatch_call(
        &mut plane,
        &token,
        "assess-before-proposed-evaluation",
        Command::GenomeAssess {
            assessment_id: "assessment-out-of-order".to_owned(),
            proposal_id: "assessment-proposal".to_owned(),
            selection_event_id: source_selection.event.event_id.clone(),
        },
    );
    assert!(matches!(
        out_of_order.error,
        Some(error)
            if error.code == ApiErrorCode::InvalidRequest
                && error.message == "Forge assessment evidence is out of order"
    ));

    assert!(matches!(
        dispatch_call(
            &mut plane,
            &token,
            "freeze-before-assessment",
            Command::Freeze
        )
        .data,
        Some(ResponseData::Acknowledged { frozen: true, .. })
    ));
    assert_eq!(
        sibling_proposal.payload.proposal_id,
        "assessment-sibling-proposal"
    );
    let command = Command::GenomeAssess {
        assessment_id: "assessment-child-result".to_owned(),
        proposal_id: "assessment-proposal".to_owned(),
        selection_event_id: child_selection.event.event_id.clone(),
    };
    let response = dispatch_call(&mut plane, &token, "assess-child", command.clone());
    assert!(
        response.error.is_none(),
        "assessment failed: {:?}",
        response.error
    );
    let Some(ResponseData::ForgeAssessment { assessment }) = response.data else {
        panic!("assessment should return its canonical record");
    };
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
    assert_eq!(
        assessment.payload.evaluation_id,
        child_selection.receipt.evaluation_id()
    );
    assert_eq!(
        assessment.payload.evaluation_event_id,
        child_selection.receipt.evaluation_event_id()
    );
    assert_eq!(
        assessment.payload.evaluation_event_hash,
        child_selection.receipt.evaluation_event_hash()
    );
    assert_eq!(assessment.payload.world_id, proposal.payload.world_id);
    assert_eq!(
        assessment.payload.parent_genome_id,
        proposal.payload.parent_genome_id
    );
    assert_eq!(assessment.payload.child_genome_id, child.genome_id);
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
    assert!(proposal.event.sequence < child_selection.event.sequence);
    assert!(child_selection.event.sequence < assessment.event.sequence);

    let conflicting_retry = dispatch_call(
        &mut plane,
        &token,
        "assess-child-conflict",
        Command::GenomeAssess {
            assessment_id: "assessment-child-result".to_owned(),
            proposal_id: "assessment-sibling-proposal".to_owned(),
            selection_event_id: child_selection.event.event_id.clone(),
        },
    );
    assert!(matches!(
        conflicting_retry.error,
        Some(error)
            if error.code == ApiErrorCode::InvalidRequest
                && error.message == "assessment_id is already bound to different assessment content"
    ));
    let wrong_pair = dispatch_call(
        &mut plane,
        &token,
        "assess-sibling-against-primary-child",
        Command::GenomeAssess {
            assessment_id: "assessment-wrong-child".to_owned(),
            proposal_id: "assessment-sibling-proposal".to_owned(),
            selection_event_id: child_selection.event.event_id.clone(),
        },
    );
    assert!(matches!(
        wrong_pair.error,
        Some(error)
            if error.code == ApiErrorCode::InvalidRequest
                && error.message == "selection evidence does not match the proposed child"
    ));

    let before_retry = plane
        .storage
        .as_ref()
        .expect("canonical Forge ledger")
        .ledger
        .replay_verified()
        .expect("verify assessment event")
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
        .count();
    let retry = dispatch_call(&mut plane, &token, "assess-child-retry", command);
    assert_eq!(
        retry.data,
        Some(ResponseData::ForgeAssessment {
            assessment: assessment.clone()
        })
    );
    let after_retry = plane
        .storage
        .as_ref()
        .expect("canonical Forge ledger")
        .ledger
        .replay_verified()
        .expect("verify idempotent assessment retry")
        .iter()
        .filter(|event| event.event_type == "forge.assessed")
        .count();
    assert_eq!(before_retry, after_retry);
    assert!(matches!(
        dispatch_call(&mut plane, &token, "replay-assessment", Command::Replay).data,
        Some(ResponseData::Replay { .. })
    ));

    let data_dir = plane.data_dir.clone();
    let repository = plane.source_repository.clone();
    let evaluator = plane.evaluator_executable.clone();
    let worker = plane.reference_worker_executable.clone();
    drop(plane);
    let mut reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("startup verifies durable Forge assessment");
    assert!(matches!(
        reopened.replay_response().expect("replay assessed history"),
        ResponseData::Replay { .. }
    ));
    let restarted_retry = dispatch_call(
        &mut reopened,
        &token,
        "assess-child-retry-after-restart",
        Command::GenomeAssess {
            assessment_id: "assessment-child-result".to_owned(),
            proposal_id: "assessment-proposal".to_owned(),
            selection_event_id: child_selection.event.event_id.clone(),
        },
    );
    assert_eq!(
        restarted_retry.data,
        Some(ResponseData::ForgeAssessment {
            assessment: assessment.clone()
        })
    );
    let changed_selection_retry = dispatch_call(
        &mut reopened,
        &token,
        "assess-child-changed-selection-after-restart",
        Command::GenomeAssess {
            assessment_id: "assessment-child-result".to_owned(),
            proposal_id: "assessment-proposal".to_owned(),
            selection_event_id: "selection:changed-after-restart".to_owned(),
        },
    );
    assert!(matches!(
        changed_selection_retry.error,
        Some(error)
            if error.code == ApiErrorCode::InvalidRequest
                && error.message == "assessment_id is already bound to different assessment content"
    ));
    drop(reopened);

    for case in [
        "outcome",
        "invariant-flag",
        "promotion-flag",
        "receipt-artifact",
        "selection-hash",
        "selection-id",
        "causal-order",
        "evaluation-hash",
        "evaluation-id",
        "schema",
        "actor",
        "aggregate",
        "event-id",
        "noncanonical",
    ] {
        let case_directory = tempdir().expect("isolated assessment tamper fixture");
        let case_data_dir = case_directory.path().join("control");
        copy_directory(&data_dir, &case_data_dir);
        let mut case_plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
            &case_data_dir,
            &repository,
            &evaluator,
            &worker,
        )
        .expect("open copied verified assessment history");
        let assessment_id = format!("assessment-tampered-{case}");
        let mut tampered = assessment.payload.clone();
        tampered.assessment_id.clone_from(&assessment_id);
        let mut actor = OPERATOR_ACTOR.to_owned();
        let mut aggregate_id = "forge:assessment-proposal".to_owned();
        let mut event_id = format!("forge-assessment:{assessment_id}:recorded");
        match case {
            "outcome" => {
                tampered.outcome = match tampered.outcome {
                    ForgeAssessmentOutcome::MetricsPassed => {
                        ForgeAssessmentOutcome::MetricsRejected
                    }
                    ForgeAssessmentOutcome::MetricsRejected => {
                        ForgeAssessmentOutcome::MetricsPassed
                    }
                };
            }
            "invariant-flag" => tampered.invariant_gate_verified = true,
            "promotion-flag" => tampered.promotion_eligible = true,
            "receipt-artifact" => {
                tampered.selection_receipt_artifact_id = "sha256:missing-receipt".to_owned();
            }
            "selection-hash" => tampered.selection_event_hash = "0".repeat(64),
            "selection-id" => tampered.selection_event_id = "selection:missing".to_owned(),
            "causal-order" => {
                tampered.selection_event_id = source_selection.event.event_id.clone();
            }
            "evaluation-hash" => tampered.evaluation_event_hash = "0".repeat(64),
            "evaluation-id" => tampered.evaluation_event_id = "evaluation:missing".to_owned(),
            "schema" => tampered.schema_version = 2,
            "actor" => actor = "untrusted-actor".to_owned(),
            "aggregate" => aggregate_id = "forge:wrong-proposal".to_owned(),
            "event-id" => event_id.push_str(":wrong"),
            "noncanonical" => {}
            _ => unreachable!("case is declared in the table above"),
        }
        let payload_value = serde_json::to_value(&tampered).expect("canonical test payload");
        let mut payload_bytes = serde_json::to_vec(&payload_value).expect("encode test payload");
        if case == "noncanonical" {
            payload_bytes.push(b' ');
        }
        case_plane
            .storage
            .as_mut()
            .expect("canonical ledger")
            .ledger
            .append(EventInput::new(
                event_id,
                aggregate_id,
                "forge.assessed",
                actor,
                timestamp_millis().expect("event timestamp"),
                payload_bytes,
            ))
            .expect("append tampered assessment with a valid ledger hash chain");
        assert!(
            matches!(case_plane.replay_response(), Err(ExecuteError::Internal)),
            "explicit replay accepted the {case} tamper"
        );
        drop(case_plane);
        assert!(
            matches!(
                ControlPlane::open_with_repository_evaluator_and_reference_worker(
                    &case_data_dir,
                    &repository,
                    &evaluator,
                    &worker,
                ),
                Err(ControlError::Projection(_))
            ),
            "startup accepted the {case} tamper"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn forge_proposal_replays_and_rejects_tampered_selection_and_metadata() {
    let directory = tempdir().expect("Forge projection fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let evaluation_id = "forge-projection-evaluation";
    assert!(matches!(
        plane
            .submit_arena_job(evaluation_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit genuine Arena evaluation"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));

    let deadline = Instant::now() + Duration::from_secs(30);
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist trials and score Forge selection fixture");
        assert!(
            Instant::now() < deadline,
            "genuine Arena evaluation did not finish"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(
        plane.state.arena_jobs[evaluation_id].terminal,
        Some(JobTerminal::Succeeded)
    );

    let mismatched_record = {
        let job = plane
            .state
            .arena_jobs
            .get_mut(evaluation_id)
            .expect("completed Arena record");
        job.evaluation.take()
    };
    let history = plane
        .storage
        .as_ref()
        .expect("canonical Arena ledger")
        .ledger
        .replay_verified()
        .expect("verify genuine Arena evaluation history");
    assert!(matches!(
        verify_arena_evaluation_records(
            &plane.storage.as_ref().expect("canonical Arena stores").artifacts,
            &history,
            &plane.state,
        ),
        Err(ControlError::Projection(message))
            if message == "Arena terminal differs from trusted evaluation evidence"
    ));
    plane
        .state
        .arena_jobs
        .get_mut(evaluation_id)
        .expect("completed Arena record")
        .evaluation = mismatched_record;

    let ResponseData::Selection { selection } = plane
        .select_arena_evaluation(evaluation_id)
        .expect("select completed Arena evidence")
    else {
        panic!("Arena selection should produce a canonical selection receipt");
    };
    let selected_candidate = selection.receipt.candidate_genome_id().to_owned();
    assert_eq!(selected_candidate, candidate.genome_id);

    assert!(matches!(
        plane.propose_genome(
            "forge-missing-selection",
            "selection:missing",
            &selected_candidate,
            "Flip the supported reference operation.",
        ),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.propose_genome(
            "forge-invalid-hypothesis",
            &selection.event.event_id,
            &selected_candidate,
            " \n",
        ),
        Err(ExecuteError::Invalid(
            "hypothesis must be 1 to 512 printable UTF-8 bytes"
        ))
    ));
    assert!(matches!(
        plane.propose_genome(
            "forge-wrong-parent",
            &selection.event.event_id,
            &parent.genome_id,
            "Flip the supported reference operation.",
        ),
        Err(ExecuteError::Rejected(message))
            if message == "the parent must be the selected candidate under the same World"
    ));
    let proposal_id = "forge-projection-child";
    let ResponseData::ForgeProposal { proposal } = plane
        .propose_genome(
            proposal_id,
            &selection.event.event_id,
            &selected_candidate,
            "Flip the supported reference operation.",
        )
        .expect("record Forge proposal bound to selected candidate")
    else {
        panic!("valid Forge proposal should return its recorded envelope");
    };
    assert!(!proposal.promotion_eligible);
    assert_eq!(proposal.payload.parent_genome_id, selected_candidate);
    assert_eq!(
        proposal.payload.child.parent_ids.as_slice(),
        std::slice::from_ref(&selected_candidate)
    );
    let forge_event_id = proposal.event.event_id.clone();
    let history = plane
        .storage
        .as_ref()
        .expect("canonical Forge ledger")
        .ledger
        .replay_verified()
        .expect("verify genuine selection and proposal history");
    verify_forge_history(
        &plane
            .storage
            .as_ref()
            .expect("canonical Forge stores")
            .artifacts,
        &history,
        &plane.state.registered,
    )
    .expect("valid Forge proposal replays against its selection receipt");

    let selection_event_id = selection.event.event_id.clone();

    assert!(matches!(
        plane.propose_genome(
            "forge-event-is-not-selection",
            &forge_event_id,
            &selected_candidate,
            "Flip the supported reference operation.",
        ),
        Err(ExecuteError::Rejected(message))
            if message == "selection_event_id does not identify a selection"
    ));

    let duplicate = plane
        .propose_genome(
            proposal_id,
            &selection.event.event_id,
            &selected_candidate,
            "Flip the supported reference operation.",
        )
        .expect("identical proposal retry returns its canonical event");
    assert!(matches!(
        duplicate,
        ResponseData::ForgeProposal { proposal } if proposal.event.event_id == forge_event_id
    ));
    let after_duplicate = plane
        .storage
        .as_ref()
        .expect("canonical Forge ledger")
        .ledger
        .replay_verified()
        .expect("verify idempotent Forge retry");
    assert_eq!(after_duplicate.len(), history.len());
    assert!(matches!(
        plane.propose_genome(
            proposal_id,
            &selection.event.event_id,
            &selected_candidate,
            "Change the proposal hypothesis.",
        ),
        Err(ExecuteError::Rejected(message))
            if message == "proposal_id is already bound to different proposal content"
    ));

    let missing_selection = forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
        payload.selection_event_id = "selection:missing".to_owned();
    });
    let missing_selection_result = verify_forge_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &missing_selection,
        &plane.state.registered,
    );
    assert!(
        matches!(
            &missing_selection_result,
            Err(ControlError::Projection(message))
                if message == "Forge proposal source selection is missing or out of order"
        ),
        "unexpected missing-selection replay result: {missing_selection_result:?}"
    );

    let invalid_selection_envelope =
        forge_history_with_selection_edit(&history, &selection_event_id, |event| {
            event.payload.push(b' ');
        });
    assert!(matches!(
        verify_forge_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &invalid_selection_envelope,
            &plane.state.registered
        ),
        Err(ControlError::Projection(message))
            if message == "Forge source selection is invalid"
    ));

    let mut wrong_world = history.clone();
    let selection_event = wrong_world
        .iter_mut()
        .find(|event| event.event_id == selection_event_id)
        .expect("selection event exists");
    // Preserve the typed payload's field order while changing only its routed World.
    let old_payload =
        std::str::from_utf8(&selection_event.payload).expect("selection payload is UTF-8");
    let old_world = selection.receipt.world_id();
    let replacement = old_payload.replace(old_world, "world:missing");
    selection_event.payload = replacement.into_bytes();
    assert!(matches!(
        verify_forge_history(&plane.storage.as_ref().unwrap().artifacts, &wrong_world, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge source World is not registered"
    ));

    // Post-TD-16, `verify_forge_history` trusts its caller-supplied `history`
    // (already hash-chain verified by `EventLedger::replay_verified()` in the
    // same operation) instead of independently re-replaying the real ledger
    // per event, so a lone in-memory hash-byte edit on a `history` slice no
    // longer has an independent source to be caught against inside the
    // verifier itself; a selection event whose ledger hash was actually
    // tampered with is instead caught earlier, when the ledger is next
    // opened. Prove that guarantee still holds, on a copy of the real ledger
    // so the live `plane` is untouched.
    let tampered_data_dir = directory.path().join("forge-selection-hash-tamper");
    copy_directory(&plane.data_dir, &tampered_data_dir);
    let tampered_ledger_path = tampered_data_dir.join("events.sqlite3");
    let connection =
        rusqlite::Connection::open(&tampered_ledger_path).expect("open hash-tamper ledger");
    let changed = connection
        .execute(
            "UPDATE events SET hash = ?1 WHERE event_id = ?2",
            rusqlite::params![vec![0_u8; 32], selection_event_id],
        )
        .expect("tamper with the selection event's ledger hash");
    assert_eq!(changed, 1);
    drop(connection);
    assert!(
        EventStore::open(&tampered_ledger_path).is_err(),
        "a selection event whose ledger hash was tampered with must fail chain verification"
    );

    let noncanonical_proposal =
        forge_history_with_selection_edit(&history, &forge_event_id, |event| {
            event.payload.push(b' ');
        });
    assert!(matches!(
        verify_forge_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &noncanonical_proposal,
            &plane.state.registered
        ),
        Err(ControlError::Projection(message))
            if message == "Forge proposal payload is not canonical"
    ));

    let wrong_parent = forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
        payload.parent_genome_id = parent.genome_id.clone();
    });
    assert!(matches!(
        verify_forge_history(&plane.storage.as_ref().unwrap().artifacts, &wrong_parent, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge proposal is not bound to its selected candidate"
    ));

    let wrong_selection_hash =
        forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
            payload.selection_event_hash = "0".repeat(64);
        });
    assert!(matches!(
        verify_forge_history(&plane.storage.as_ref().unwrap().artifacts, &wrong_selection_hash, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge proposal is not bound to its selected candidate"
    ));

    let wrong_child = forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
        payload.child.name.push_str("-tampered");
    });
    assert!(matches!(
        verify_forge_history(&plane.storage.as_ref().unwrap().artifacts, &wrong_child, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge proposal child differs from its registered lineage"
    ));

    let wrong_operation = forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
        payload.operation_after = "identity".to_owned();
    });
    assert!(matches!(
        verify_forge_history(&plane.storage.as_ref().unwrap().artifacts, &wrong_operation, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge prompt mutation is not a representable one-step catalog edge"
    ));

    let invalid_hypothesis =
        forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
            payload.hypothesis = " \n".to_owned();
        });
    assert!(matches!(
        verify_forge_history(&plane.storage.as_ref().unwrap().artifacts, &invalid_hypothesis, &plane.state.registered),
        Err(ControlError::Projection(message)) if message == "Forge hypothesis is invalid"
    ));

    let mut invalid_actor = history.clone();
    invalid_actor
        .iter_mut()
        .find(|event| event.event_id == forge_event_id)
        .expect("Forge event exists")
        .actor = "untrusted-actor".to_owned();
    assert!(matches!(
        verify_forge_history(&plane.storage.as_ref().unwrap().artifacts, &invalid_actor, &plane.state.registered),
        Err(ControlError::Projection(message)) if message == "Forge proposal event identity is invalid"
    ));

    assert!(matches!(
        plane
            .replay_response()
            .expect("explicit replay verifies persisted Forge history"),
        ResponseData::Replay { .. }
    ));
    let data_dir = plane.data_dir.clone();
    let repository = plane.source_repository.clone();
    let evaluator = plane.evaluator_executable.clone();
    let worker = plane.reference_worker_executable.clone();
    drop(plane);
    let reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("restart verifies and preserves valid Forge proposal");
    assert!(
        reopened
            .state
            .registered
            .genome(&proposal.payload.child.genome_id)
            .is_some()
    );
    assert!(matches!(
        reopened.replay_response().expect("replay after restart"),
        ResponseData::Replay { .. }
    ));
}

#[test]
fn forge_prompt_mutation_rejects_missing_unsupported_and_reformatted_prompts() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (world, _, _) = register_dispatch_objects(&mut plane, &token, &directory);

    let promptless_path = directory.path().join("forge-promptless.json");
    fs::write(
        &promptless_path,
        r#"{"schema_version":1,"name":"forge-promptless","parents":[],"model":{"provider":"deterministic","family":"reference"},"authority":{"workspace_write":false,"network":false},"artifacts":{}}"#,
    )
    .expect("write promptless Genome");
    let Some(ResponseData::Genome { genome: promptless }) = dispatch_call(
        &mut plane,
        &token,
        "register-forge-promptless",
        Command::GenomeRegister {
            path: promptless_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("promptless Genome should register");
    };
    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical Forge artifacts");
    let compiled_world = plane
        .registered_world(&world.world_id)
        .expect("registered dispatch world");
    assert!(matches!(
        forge_prompt_mutation(
            &artifacts,
            &plane.state.registered,
            &promptless.genome_id,
            &compiled_world,
            None,
        ),
        Err(ExecuteError::Rejected(message))
            if message == "the selected candidate has no supported prompt to mutate"
    ));

    let unsupported = register_json_genome_with_prompt(
        &mut plane,
        &token,
        &directory,
        &world.world_id,
        "forge-unsupported-prompt",
        b"plain unsupported prompt\n",
    );
    assert!(matches!(
        forge_prompt_mutation(
            &artifacts,
            &plane.state.registered,
            &unsupported.genome_id,
            &compiled_world,
            None,
        ),
        Err(ExecuteError::Rejected(message))
            if message == "the selected candidate prompt is outside the supported mutation language"
    ));

    let reformatted =
        reference_instruction_document(ReferenceInstruction::Identity).replace(',', ", ");
    let out_of_scope = register_json_genome_with_prompt(
        &mut plane,
        &token,
        &directory,
        &world.world_id,
        "forge-reformatted-prompt",
        reformatted.as_bytes(),
    );
    assert!(matches!(
        forge_prompt_mutation(
            &artifacts,
            &plane.state.registered,
            &out_of_scope.genome_id,
            &compiled_world,
            None,
        ),
        Err(ExecuteError::Rejected(message))
            if message == "the selected candidate prompt is outside the Forge mutation scope"
    ));
}

#[cfg(feature = "test-support")]
#[test]
fn test_arena_wall_override_only_lowers_the_admitted_bound() {
    assert_eq!(test_overall_wall(50_000, None), Ok(50_000));
    assert_eq!(test_overall_wall(50_000, Some("1000")), Ok(1_000));
    assert!(test_overall_wall(50_000, Some("50001")).is_err());
    assert!(test_overall_wall(50_000, Some("0")).is_err());
    assert!(test_overall_wall(50_000, Some("invalid")).is_err());
}

#[test]
fn progress_phase_names_cover_each_persisted_trace_kind() {
    let kinds = [
        (TraceKind::LifecycleStarted, "started"),
        (TraceKind::LifecycleResumed, "resumed"),
        (TraceKind::LifecycleCompleted, "completed"),
        (TraceKind::ToolCalled, "tool_called"),
        (TraceKind::ToolResult, "tool_result"),
        (TraceKind::ContextComposed, "context_composed"),
        (TraceKind::MemoryRetrieved, "memory_retrieved"),
        (TraceKind::SubagentSpawned, "subagent_spawned"),
        (TraceKind::FileRead, "file_read"),
        (TraceKind::FileChanged, "file_changed"),
        (TraceKind::TestExecuted, "test_executed"),
        (TraceKind::CapabilityDenied, "capability_denied"),
        (TraceKind::CostObserved, "cost_observed"),
        (TraceKind::CheckpointCreated, "checkpoint_created"),
        (TraceKind::Error, "error"),
        (TraceKind::Retry, "retry"),
        (TraceKind::ModelResponse, "model_response"),
    ];
    for (kind, expected) in kinds {
        assert_eq!(trace_phase(kind), expected);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn command_audit_types_and_source_extensions_are_stable() {
    let commands = [
        (Command::Status, "control.status"),
        (Command::Freeze, "control.freeze"),
        (Command::Unfreeze, "control.unfreeze"),
        (Command::KillAll, "control.kill_all"),
        (
            Command::GenomeShow {
                genome_id: "g".into(),
            },
            "control.genome_show",
        ),
        (
            Command::GenomePrompt {
                genome_id: "g".into(),
            },
            "control.genome_prompt",
        ),
        (Command::GenomeList, "control.genome_list"),
        (
            Command::GenomeRegister {
                path: "a.md".into(),
                world_id: "w".into(),
            },
            "control.genome_register",
        ),
        (
            Command::GenomePropose {
                proposal_id: "proposal-1".into(),
                selection_event_id: "selection-event".into(),
                parent_genome_id: "candidate".into(),
                hypothesis: Some("The child should preserve case.".into()),
                analysis_id: None,
                cluster_index: None,
            },
            "control.genome_propose",
        ),
        (
            Command::WorldShow {
                world_id: "w".into(),
            },
            "control.world_show",
        ),
        (Command::WorldList, "control.world_list"),
        (
            Command::WorldRegister {
                path: "a.json".into(),
            },
            "control.world_register",
        ),
        (
            Command::ManifestPut {
                path: "a.json".into(),
            },
            "control.manifest_put",
        ),
        (
            Command::ArtifactPut {
                path: "a.bin".into(),
            },
            "control.artifact_put",
        ),
        (Command::VerifierShow, "control.verifier_show"),
        (
            Command::RunSubmit {
                job_id: "j".into(),
                genome_id: "g".into(),
            },
            "control.run_submit",
        ),
        (
            Command::JobStatus { job_id: "j".into() },
            "control.job_status",
        ),
        (Command::JobKill { job_id: "j".into() }, "control.job_kill"),
        (
            Command::RunReference {
                genome_id: "g".into(),
            },
            "control.run_reference",
        ),
        (
            Command::RunEvaluation {
                genome_id: "g".into(),
                task_id: "t".into(),
                input: "i".into(),
                seed: 1,
                wall_millis: 1,
                maximum_output_bytes: 1,
                maximum_cost_microusd: 0,
            },
            "control.run_evaluation",
        ),
        (
            Command::EvaluatePair {
                evaluation_id: "e".into(),
                parent_genome_id: "p".into(),
                candidate_genome_id: "c".into(),
                remote: false,
            },
            "control.evaluate_pair",
        ),
        (
            Command::ArenaSelect {
                evaluation_id: "e".into(),
            },
            "control.arena_select",
        ),
        (Command::Replay, "control.replay"),
        (Command::DaemonStop, "control.daemon_stop"),
    ];
    for (command, expected) in commands {
        assert_eq!(event_type(&command), expected);
        assert!(require_command_fields(&command).is_ok());
    }
    assert_eq!(source_format("world.json").unwrap(), SourceFormat::Json);
    assert_eq!(source_format("world.yaml").unwrap(), SourceFormat::Yaml);
    assert_eq!(source_format("world.yml").unwrap(), SourceFormat::Yaml);
    assert!(source_format("agent.md").is_err());
}

#[test]
fn bounded_source_reader_rejects_invalid_utf8_and_oversized_files() {
    let directory = tempdir().expect("source directory");
    let source = directory.path().join("source.yaml");
    fs::write(&source, b"key: value\n").expect("write source");
    assert_eq!(
        read_source_text(source.to_str().unwrap(), 32).unwrap(),
        "key: value\n"
    );
    assert!(read_bounded_file(source.to_str().unwrap(), 2).is_err());
    fs::write(&source, [0xff]).expect("write invalid UTF-8");
    assert!(read_source_text(source.to_str().unwrap(), 32).is_err());
    assert!(read_bounded_file(directory.path().to_str().unwrap(), 32).is_err());
}

#[test]
#[allow(clippy::too_many_lines)]
fn bounded_socket_handler_routes_valid_requests_and_rejects_bad_or_saturated_clients() {
    let (sender, receiver) = mpsc::sync_channel(1);
    let (server, mut client) = UnixStream::pair().expect("create local socket pair");
    let active = Arc::new(AtomicUsize::new(1));
    let handler_active = Arc::clone(&active);
    let handler = thread::spawn(move || serve_connection(server, &sender, handler_active));
    let request = ApiRequest {
        version: API_VERSION,
        request_id: "socket-request".to_owned(),
        token: "token".to_owned(),
        command: Command::Status,
    };
    client
        .write_all(&serde_json::to_vec(&request).expect("encode request"))
        .expect("write request");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("finish request frame");
    let queued = receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("writer receives request");
    assert_eq!(queued.request, request);
    queued
        .reply
        .send(ApiResponse::success(
            request.request_id.clone(),
            ResponseData::Status {
                frozen: true,
                active_runs: 0,
                event_count: 1,
                genome_count: 0,
            },
        ))
        .expect("reply to socket handler");
    let mut response_bytes = Vec::new();
    client
        .read_to_end(&mut response_bytes)
        .expect("read socket response");
    handler.join().expect("join socket handler");
    assert_eq!(active.load(Ordering::Acquire), 0);
    assert_eq!(
        serde_json::from_slice::<ApiResponse>(&response_bytes)
            .expect("decode socket response")
            .request_id,
        "socket-request"
    );

    let (sender, receiver) = mpsc::sync_channel(1);
    let (server, mut client) = UnixStream::pair().expect("create dropped-reply socket pair");
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("bound dropped-reply response wait");
    let active = Arc::new(AtomicUsize::new(1));
    let handler_active = Arc::clone(&active);
    let handler = thread::spawn(move || serve_connection(server, &sender, handler_active));
    let request = ApiRequest {
        version: API_VERSION,
        request_id: "dropped-reply-request".to_owned(),
        token: "token".to_owned(),
        command: Command::Status,
    };
    client
        .write_all(&serde_json::to_vec(&request).expect("encode dropped-reply request"))
        .expect("write dropped-reply request");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("finish dropped-reply request frame");
    let queued = receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("writer receives dropped-reply request");
    assert_eq!(queued.request, request);
    drop(queued.reply);
    let mut response_bytes = Vec::new();
    client
        .read_to_end(&mut response_bytes)
        .expect("read immediate dropped-reply response");
    handler.join().expect("join dropped-reply handler");
    assert_eq!(active.load(Ordering::Acquire), 0);
    let response: ApiResponse =
        serde_json::from_slice(&response_bytes).expect("decode dropped-reply response");
    assert_eq!(
        response.error.expect("safe dropped-reply error").code,
        ApiErrorCode::Internal
    );

    let (sender, _receiver) = mpsc::sync_channel(1);
    let malformed = serve_test_connection(&sender, b"{");
    assert_eq!(
        malformed.error.expect("malformed response").code,
        ApiErrorCode::InvalidRequest
    );

    let oversized = vec![b'x'; MAX_REQUEST_BYTES * 2];
    let response = serve_test_connection(&sender, &oversized);
    assert_eq!(
        response.error.expect("oversized response"),
        crate::ApiError {
            code: ApiErrorCode::InvalidRequest,
            message: "request exceeds limit".to_owned(),
        }
    );
    let just_over_limit = vec![b'x'; MAX_REQUEST_BYTES + 1];
    let response = serve_test_connection(&sender, &just_over_limit);
    assert_eq!(
        response.error.expect("boundary response").message,
        "request exceeds limit"
    );

    let (sender, receiver) = mpsc::sync_channel(1);
    let (reply_sender, reply_receiver) = mpsc::sync_channel(1);
    sender
        .try_send(QueuedRequest {
            request: request.clone(),
            reply: reply_sender,
        })
        .expect("saturate writer queue");
    let full = serve_test_connection(&sender, &serde_json::to_vec(&request).unwrap());
    assert_eq!(full.error.expect("busy response").code, ApiErrorCode::Busy);
    drop(receiver);
    drop(reply_receiver);

    let (sender, receiver) = mpsc::sync_channel(1);
    drop(receiver);
    let stopped = serve_test_connection(&sender, &serde_json::to_vec(&request).unwrap());
    assert_eq!(
        stopped.error.expect("stopped response").code,
        ApiErrorCode::Internal
    );

    let (sender, _receiver) = mpsc::sync_channel(1);
    let timeout = serve_silent_test_connection(&sender);
    assert_eq!(
        timeout.error.expect("timeout response").code,
        ApiErrorCode::InvalidRequest
    );

    let (sender, receiver) = mpsc::sync_channel(1);
    let (server, mut client) = UnixStream::pair().expect("create disconnect socket pair");
    let active = Arc::new(AtomicUsize::new(1));
    let handler_active = Arc::clone(&active);
    let handler_sender = sender.clone();
    let handler = thread::spawn(move || serve_connection(server, &handler_sender, handler_active));
    client
        .write_all(&serde_json::to_vec(&request).expect("encode disconnect request"))
        .expect("write disconnect request");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("finish disconnect request");
    let queued = receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("writer receives disconnect request");
    drop(client);
    queued
        .reply
        .send(ApiResponse::success(
            request.request_id,
            ResponseData::Status {
                frozen: true,
                active_runs: 0,
                event_count: 1,
                genome_count: 0,
            },
        ))
        .expect("reply remains independent of client disconnect");
    handler.join().expect("join disconnected handler");
    assert_eq!(active.load(Ordering::Acquire), 0);
}

fn serve_test_connection(sender: &mpsc::SyncSender<QueuedRequest>, bytes: &[u8]) -> ApiResponse {
    let (server, mut client) = UnixStream::pair().expect("create local socket pair");
    let active = Arc::new(AtomicUsize::new(1));
    let handler_active = Arc::clone(&active);
    let handler_sender = sender.clone();
    let handler = thread::spawn(move || serve_connection(server, &handler_sender, handler_active));
    client.write_all(bytes).expect("write request bytes");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("finish request frame");
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .expect("read error response");
    handler.join().expect("join socket handler");
    assert_eq!(active.load(Ordering::Acquire), 0);
    serde_json::from_slice(&response).expect("decode error response")
}

fn serve_silent_test_connection(sender: &mpsc::SyncSender<QueuedRequest>) -> ApiResponse {
    let (server, mut client) = UnixStream::pair().expect("create silent socket pair");
    let active = Arc::new(AtomicUsize::new(1));
    let handler_active = Arc::clone(&active);
    let handler_sender = sender.clone();
    let handler = thread::spawn(move || serve_connection(server, &handler_sender, handler_active));
    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .expect("read timeout response");
    handler.join().expect("join timed out handler");
    assert_eq!(active.load(Ordering::Acquire), 0);
    serde_json::from_slice(&response).expect("decode timeout response")
}

#[test]
fn real_listener_services_status_and_shutdown_through_the_socket_writer() {
    let directory = tempdir().expect("daemon directory");
    let data_dir = directory.path().join("daemon-data");
    let plane = ControlPlane::open(&data_dir).expect("open control plane");
    let server = thread::spawn(move || plane.serve());
    let socket = data_dir.join("control.sock");
    let deadline = Instant::now() + Duration::from_secs(2);
    let token = loop {
        if let Ok(token) = fs::read_to_string(data_dir.join("operator.token"))
            && UnixStream::connect(&socket).is_ok()
        {
            break token;
        }
        assert!(Instant::now() < deadline, "listener did not start");
        thread::sleep(Duration::from_millis(2));
    };
    let mut slow_clients: Vec<_> = (0..=MAX_SOCKET_HANDLERS)
        .map(|_| UnixStream::connect(&socket).expect("connect slow client"))
        .collect();
    for client in &slow_clients {
        client
            .set_nonblocking(true)
            .expect("make slow client nonblocking");
    }
    let mut response_bytes = vec![Vec::new(); slow_clients.len()];
    let saturation_deadline = Instant::now() + Duration::from_secs(1);
    while !response_bytes.iter().any(|bytes| {
        bytes
            .windows(b"daemon connection limit reached".len())
            .any(|window| window == b"daemon connection limit reached")
    }) {
        for (client, bytes) in slow_clients.iter_mut().zip(&mut response_bytes) {
            let mut buffer = [0_u8; 256];
            if let Ok(read) = client.read(&mut buffer) {
                bytes.extend_from_slice(&buffer[..read]);
            }
        }
        assert!(
            Instant::now() < saturation_deadline,
            "handler limit was not enforced"
        );
        thread::sleep(Duration::from_millis(1));
    }
    drop(slow_clients);
    let status = send_test_api_request(&socket, &token, "status-1", Command::Status);
    assert!(matches!(status.data, Some(ResponseData::Status { .. })));
    let stopped = send_test_api_request(&socket, &token, "stop-1", Command::DaemonStop);
    assert!(matches!(
        stopped.data,
        Some(ResponseData::Acknowledged { .. })
    ));
    server
        .join()
        .expect("join listener thread")
        .expect("serve requests");
}

#[test]
fn authenticated_command_dispatch_covers_safe_read_and_control_paths() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    assert_dispatch_auth_and_control_paths(&mut plane, &token);
    assert_dispatch_missing_command_paths(&mut plane, &token, &directory);
    let (world, genome, prompt) = register_dispatch_objects(&mut plane, &token, &directory);
    assert_registered_dispatch_objects(&mut plane, &token, &world, &genome, prompt);
    exercise_dispatch_job(&mut plane, &genome.genome_id);
    assert!(matches!(
        dispatch_call(&mut plane, &token, "replay", Command::Replay).data,
        Some(ResponseData::Replay { .. })
    ));
    assert!(matches!(
        dispatch_call(&mut plane, &token, "stop", Command::DaemonStop).data,
        Some(ResponseData::Acknowledged { .. })
    ));
}

fn dispatch_call(
    plane: &mut ControlPlane,
    token: &str,
    request_id: &str,
    command: Command,
) -> ApiResponse {
    plane.handle(ApiRequest {
        version: API_VERSION,
        request_id: request_id.to_owned(),
        token: token.to_owned(),
        command,
    })
}

fn assert_dispatch_auth_and_control_paths(plane: &mut ControlPlane, token: &str) {
    for (request, expected) in [
        (
            ApiRequest {
                version: API_VERSION + 1,
                request_id: "version".to_owned(),
                token: token.to_owned(),
                command: Command::Status,
            },
            ApiErrorCode::UnsupportedVersion,
        ),
        (
            ApiRequest {
                version: API_VERSION,
                request_id: "auth".to_owned(),
                token: "wrong-token".to_owned(),
                command: Command::Status,
            },
            ApiErrorCode::Unauthorized,
        ),
    ] {
        assert_eq!(
            plane.handle(request).error.expect("request rejection").code,
            expected
        );
    }
    assert_eq!(
        dispatch_call(plane, token, "", Command::Status)
            .error
            .expect("request ID error")
            .code,
        ApiErrorCode::InvalidRequest
    );
    for (request_id, command) in [
        ("status", Command::Status),
        ("freeze", Command::Freeze),
        ("unfreeze", Command::Unfreeze),
        ("kill-all", Command::KillAll),
        ("genomes", Command::GenomeList),
        ("worlds", Command::WorldList),
    ] {
        assert!(
            dispatch_call(plane, token, request_id, command)
                .error
                .is_none()
        );
    }
}

fn assert_dispatch_missing_command_paths(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
) {
    for (index, command) in [
        Command::GenomeShow {
            genome_id: "hephaestus:genome:missing".to_owned(),
        },
        Command::WorldShow {
            world_id: "hephaestus:world:missing".to_owned(),
        },
        Command::JobStatus {
            job_id: "missing".to_owned(),
        },
        Command::JobKill {
            job_id: "missing".to_owned(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            dispatch_call(plane, token, &format!("not-found-{index}"), command)
                .error
                .expect("not-found response")
                .code,
            ApiErrorCode::NotFound
        );
    }
    let absent_genome = format!("hephaestus:genome:{}", "f".repeat(64));
    let missing = |name: &str| directory.path().join(name).display().to_string();
    let commands = [
        Command::GenomePrompt {
            genome_id: absent_genome.clone(),
        },
        Command::RunSubmit {
            job_id: "missing-genome-job".to_owned(),
            genome_id: absent_genome.clone(),
        },
        Command::RunReference {
            genome_id: absent_genome.clone(),
        },
        Command::RunEvaluation {
            genome_id: absent_genome.clone(),
            task_id: "task".to_owned(),
            input: "input".to_owned(),
            seed: 0,
            wall_millis: 10_000,
            maximum_output_bytes: 1_048_576,
            maximum_cost_microusd: 0,
        },
        Command::EvaluatePair {
            evaluation_id: "missing-pair".to_owned(),
            parent_genome_id: absent_genome.clone(),
            candidate_genome_id: format!("hephaestus:genome:{}", "e".repeat(64)),
            remote: false,
        },
        Command::ArenaSelect {
            evaluation_id: "missing-evaluation".to_owned(),
        },
        Command::GenomeRegister {
            path: missing("missing.md"),
            world_id: format!("hephaestus:world:{}", "d".repeat(64)),
        },
        Command::WorldRegister {
            path: missing("missing.json"),
        },
        Command::ManifestPut {
            path: missing("missing-manifest.json"),
        },
        Command::ArtifactPut {
            path: missing("missing-artifact"),
        },
    ];
    for (index, command) in commands.into_iter().enumerate() {
        assert!(
            dispatch_call(plane, token, &format!("invalid-{index}"), command)
                .error
                .is_some()
        );
    }
}

fn register_dispatch_objects(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
) -> (WorldRecord, GenomeRecord, &'static str) {
    let world_path = directory.path().join("world.json");
    fs::write(
        &world_path,
        r#"{"schema_version":1,"name":"dispatch-world","laws":{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0},"authority_ceiling":{"workspace_write":false,"network":false},"mutation_scope":["harness"],"promotion":{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500},"objectives":["correctness"],"evaluator_artifacts":{}}"#,
    )
    .expect("write World source");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        "register-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("World registration should succeed");
    };
    let prompt =
        "```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n";
    let genome_path = directory.path().join("agent.md");
    fs::write(
        &genome_path,
        format!(
            "---\nschema_version: 1\nname: dispatch-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n{prompt}"
        ),
    )
    .expect("write Genome source");
    let Some(ResponseData::Genome { genome }) = dispatch_call(
        plane,
        token,
        "register-genome",
        Command::GenomeRegister {
            path: genome_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("Genome registration should succeed");
    };
    (world, genome, prompt)
}

#[test]
fn json_genome_non_utf8_prompt_is_rejected_before_run_admission() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (world, _, _) = register_dispatch_objects(&mut plane, &token, &directory);

    let invalid = dispatch_json_genome_with_prompt(
        &mut plane,
        &token,
        &directory,
        &world.world_id,
        "json-non-utf8-prompt",
        &[0xff],
    );
    assert_eq!(
        invalid
            .error
            .expect("compiler rejects non-UTF-8 prompt before registration")
            .code,
        ApiErrorCode::InvalidRequest
    );
    assert!(plane.state.jobs.is_empty());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after rejected Genome");
    assert!(!history.iter().any(|event| {
        matches!(
            event.event_type.as_str(),
            "job.admitted" | "run.result_recorded"
        )
    }));
}

#[test]
fn authenticated_run_submit_rejects_slash_job_id_without_admission() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "unfreeze-before-invalid-submit",
            Command::Unfreeze
        )
        .error
        .is_none()
    );

    let response = dispatch_call(
        &mut plane,
        &token,
        "invalid-job-id-submit",
        Command::RunSubmit {
            job_id: "bad/id".to_owned(),
            genome_id: genome.genome_id,
        },
    );
    assert_eq!(
        response.error.expect("slash job ID is invalid").code,
        ApiErrorCode::InvalidRequest
    );
    assert!(plane.state.jobs.is_empty());
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "status-after-invalid-submit",
            Command::Status
        )
        .error
        .is_none(),
        "authenticated control remains available"
    );
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after invalid submit");
    assert!(
        !history
            .iter()
            .any(|event| event.event_type == "job.admitted")
    );
}

#[test]
fn source_revision_rejects_initialized_uncommitted_repository_without_mutation() {
    let directory = tempdir().expect("fixture directory");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    let source = repository.join("source.txt");
    let contents = b"uncommitted source bytes\n";
    fs::write(&source, contents).expect("write uncommitted source");

    assert!(matches!(
        resolve_source_revision(&repository),
        Err(ExecuteError::Internal)
    ));
    assert_eq!(
        fs::read(source).expect("read source after rejection"),
        contents
    );
}

fn register_json_genome_with_prompt(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    world_id: &str,
    name: &str,
    prompt_bytes: &[u8],
) -> GenomeRecord {
    let response =
        dispatch_json_genome_with_prompt(plane, token, directory, world_id, name, prompt_bytes);
    let Some(ResponseData::Genome { genome }) = response.data else {
        panic!(
            "JSON Genome registration should succeed: {:?}",
            response.error
        );
    };
    genome
}

fn dispatch_json_genome_with_prompt(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    world_id: &str,
    name: &str,
    prompt_bytes: &[u8],
) -> ApiResponse {
    let prompt_id = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .artifacts
        .put(prompt_bytes)
        .expect("store prompt bytes");
    let source_path = directory.path().join(format!("{name}.json"));
    fs::write(
        &source_path,
        format!(
            r#"{{"schema_version":1,"name":"{name}","parents":[],"model":{{"provider":"deterministic","family":"reference"}},"authority":{{"workspace_write":false,"network":false}},"artifacts":{{"agent.prompt":"{}"}}}}"#,
            prompt_id.as_str()
        ),
    )
    .expect("write JSON Genome source");
    dispatch_call(
        plane,
        token,
        &format!("register-{name}"),
        Command::GenomeRegister {
            path: source_path.display().to_string(),
            world_id: world_id.to_owned(),
        },
    )
}

#[test]
fn json_genome_prompt_reads_valid_cas_and_registration_rejects_invalid_contents() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (world, _, _) = register_dispatch_objects(&mut plane, &token, &directory);

    let valid_prompt = b"bounded JSON Genome prompt\n";
    let valid = register_json_genome_with_prompt(
        &mut plane,
        &token,
        &directory,
        &world.world_id,
        "json-valid-prompt",
        valid_prompt,
    );
    assert!(matches!(
        dispatch_call(
            &mut plane,
            &token,
            "valid-prompt",
            Command::GenomePrompt {
                genome_id: valid.genome_id,
            }
        )
        .data,
        Some(ResponseData::GenomePrompt { prompt, .. }) if prompt.as_bytes() == valid_prompt
    ));

    let whitespace = dispatch_json_genome_with_prompt(
        &mut plane,
        &token,
        &directory,
        &world.world_id,
        "json-whitespace-prompt",
        b" \n\t ",
    );
    assert_eq!(
        whitespace
            .error
            .expect("whitespace prompt is rejected at registration")
            .code,
        ApiErrorCode::InvalidRequest
    );

    let oversized_bytes =
        vec![b'x'; usize::try_from(MAX_SOURCE_FILE_BYTES).expect("source limit fits usize") + 1];
    let oversized = dispatch_json_genome_with_prompt(
        &mut plane,
        &token,
        &directory,
        &world.world_id,
        "json-oversized-prompt",
        &oversized_bytes,
    );
    assert_eq!(
        oversized
            .error
            .expect("oversized prompt is rejected at registration")
            .code,
        ApiErrorCode::InvalidRequest
    );
}

fn assert_registered_dispatch_objects(
    plane: &mut ControlPlane,
    token: &str,
    world: &WorldRecord,
    genome: &GenomeRecord,
    prompt: &str,
) {
    assert!(matches!(
        dispatch_call(
            plane,
            token,
            "world-show",
            Command::WorldShow {
                world_id: world.world_id.clone(),
            }
        )
        .data,
        Some(ResponseData::World { .. })
    ));
    assert!(matches!(
        dispatch_call(
            plane,
            token,
            "genome-show",
            Command::GenomeShow {
                genome_id: genome.genome_id.clone(),
            }
        )
        .data,
        Some(ResponseData::Genome { .. })
    ));
    assert!(matches!(
        dispatch_call(plane, token, "genome-prompt", Command::GenomePrompt {
            genome_id: genome.genome_id.clone(),
        }).data,
        Some(ResponseData::GenomePrompt { prompt: actual, .. }) if actual == prompt
    ));
    assert!(matches!(
        dispatch_call(plane, token, "verifier", Command::VerifierShow).data,
        Some(ResponseData::Verifier { .. })
    ));
}

fn exercise_dispatch_job(plane: &mut ControlPlane, genome_id: &str) {
    assert!(matches!(
        plane.submit_job("dispatch-run", genome_id)
            .expect("admit bounded reference job"),
        ResponseData::Job { job, .. } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist worker evidence");
        assert!(Instant::now() < deadline, "direct reference job stalled");
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(plane.state.jobs["dispatch-run"].state, JobState::Succeeded);
}

#[test]
fn canonical_writer_rejects_evidence_without_an_admitted_job() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let (sender, receiver) = mpsc::sync_channel(1);
    let (reply, result) = mpsc::channel();
    sender
        .send(EvidenceRequest::EnsureCapacity {
            run_id: "unadmitted-run".to_owned(),
            needed: 2,
            reply,
        })
        .expect("queue unadmitted evidence");
    plane.job_evidence_receiver = Some(receiver);
    plane
        .service_async_messages()
        .expect("reject unadmitted evidence safely");
    assert!(
        result
            .recv_timeout(Duration::from_secs(1))
            .expect("writer returns evidence rejection")
            .is_err()
    );
    assert!(
        plane.storage.is_some(),
        "canonical storage remains available"
    );
}

#[test]
fn canonical_writer_unavailable_cancels_and_fails_an_admitted_direct_job() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    plane
        .submit_job("writer-unavailable", &genome.genome_id)
        .expect("admit direct reference job");
    let run_id = plane
        .active_job
        .as_ref()
        .expect("active direct job")
        .spec
        .run_id()
        .to_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    let (reply, result) = mpsc::channel();
    sender
        .send(EvidenceRequest::EnsureCapacity {
            run_id,
            needed: 2,
            reply,
        })
        .expect("queue admitted evidence request");
    plane.job_evidence_receiver = Some(receiver);
    let storage = plane.storage.take().expect("canonical storage");
    plane
        .service_async_messages()
        .expect("reject evidence while canonical writer is unavailable");
    assert!(
        result
            .recv_timeout(Duration::from_secs(1))
            .expect("executor receives writer rejection")
            .is_err()
    );
    plane.storage = Some(storage);
    assert!(
        plane
            .active_job
            .as_ref()
            .expect("job remains active until executor unwinds")
            .cancel
            .load(Ordering::Acquire)
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist cancelled direct job terminal");
        assert!(Instant::now() < deadline, "writer cancellation stalled");
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let terminal = &plane.state.jobs["writer-unavailable"];
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert!(matches!(
        plane.replay_response().expect("replay failed direct job"),
        ResponseData::Replay { .. }
    ));
}

#[test]
fn canonical_writer_cancels_active_job_on_cross_run_evidence() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    plane
        .submit_job("evidence-cancel", &genome.genome_id)
        .expect("admit bounded reference job");
    let cancel = Arc::clone(
        &plane
            .active_job
            .as_ref()
            .expect("active reference job")
            .cancel,
    );

    let (sender, receiver) = mpsc::sync_channel(1);
    let (reply, result) = mpsc::channel();
    sender
        .send(EvidenceRequest::EnsureCapacity {
            run_id: "different-run".to_owned(),
            needed: 2,
            reply,
        })
        .expect("queue cross-run evidence");
    plane.job_evidence_receiver = Some(receiver);
    plane
        .service_async_messages()
        .expect("reject cross-run evidence safely");
    assert!(
        result
            .recv_timeout(Duration::from_secs(1))
            .expect("writer returns evidence rejection")
            .is_err()
    );
    assert!(
        cancel.load(Ordering::Acquire),
        "cross-run evidence cancels the active worker"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("drain cancelled worker result");
        assert!(Instant::now() < deadline, "cancelled job did not unwind");
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let terminal = &plane.state.jobs["evidence-cancel"];
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert!(matches!(
        plane.replay_response().expect("replay rejected evidence"),
        ResponseData::Replay { .. }
    ));
}

#[test]
fn canonical_writer_cas_failure_rejects_trace_and_cancels_admitted_job() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (world, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    plane
        .submit_job("cas-failure", &genome.genome_id)
        .expect("admit direct reference job");
    let run_id = plane
        .active_job
        .as_ref()
        .expect("active direct job")
        .spec
        .run_id()
        .to_owned();
    let (sender, receiver) = mpsc::sync_channel(1);
    let (reply, result) = mpsc::channel();
    sender
        .send(EvidenceRequest::RecordTrace {
            input: TraceInput::new(
                "trace:cas-failure",
                Provenance::new(run_id, genome.genome_id.clone(), world.world_id)
                    .expect("valid admitted provenance"),
                TraceKind::LifecycleStarted,
                timestamp_millis().expect("valid timestamp"),
                BTreeMap::new(),
            )
            .expect("valid trace input"),
            reserved_after: 0,
            reply,
        })
        .expect("queue admitted trace");
    plane.job_evidence_receiver = Some(receiver);

    let blobs = plane.data_dir.join("blobs");
    let saved_blobs = plane.data_dir.join("blobs-before-trace-failure");
    fs::rename(&blobs, &saved_blobs).expect("temporarily hide canonical artifacts");
    fs::write(&blobs, b"blocked").expect("make artifact root unwritable as a directory");
    plane
        .service_async_messages()
        .expect("reject trace after actual CAS write failure");
    assert!(
        result
            .recv_timeout(Duration::from_secs(1))
            .expect("executor receives negative durable acknowledgement")
            .is_err()
    );
    assert!(
        plane
            .active_job
            .as_ref()
            .expect("job stays active until executor cleanup")
            .cancel
            .load(Ordering::Acquire)
    );
    fs::remove_file(&blobs).expect("remove temporary blocker");
    fs::rename(saved_blobs, blobs).expect("restore canonical artifacts");

    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist failed direct job terminal");
        assert!(Instant::now() < deadline, "cancelled job did not terminate");
        thread::sleep(Duration::from_millis(2));
    }
    let terminal = &plane.state.jobs["cas-failure"];
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert!(
        !plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verified failure history")
            .iter()
            .any(|event| event.event_id == "trace:cas-failure"),
        "failed CAS write must not publish a trace receipt"
    );
    exercise_dispatch_job(&mut plane, &genome.genome_id);
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay after restored artifact storage"),
        ResponseData::Replay { .. }
    ));
}

#[test]
fn missing_direct_result_after_real_execution_records_interruption_and_releases_slot() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    plane.drop_next_direct_result_after_execution = true;
    let database = rusqlite::Connection::open(directory.path().join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    database
        .execute_batch(
            "CREATE TRIGGER reject_lost_result_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'job:lost-result:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture interrupted terminal append failure'); END;",
        )
        .expect("reject interrupted terminal append");
    plane
        .submit_job("lost-result", &genome.genome_id)
        .expect("admit direct reference job");
    assert_interrupted_terminal_append_failure(&mut plane);
    database
        .execute_batch("DROP TRIGGER reject_lost_result_terminal;")
        .expect("restore interrupted terminal writes");
    drop(database);
    plane
        .service_async_messages()
        .expect("retry interrupted terminal after storage recovers");
    assert!(
        plane.active_job.is_none(),
        "successful retry releases the active slot"
    );
    let terminal_history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify retried terminal history");
    assert_eq!(
        terminal_history
            .iter()
            .filter(|event| event.event_id == "job:lost-result:terminal")
            .count(),
        1,
        "retry must persist exactly one interruption terminal"
    );
    let interrupted = &plane.state.jobs["lost-result"];
    assert_eq!(interrupted.state, JobState::Interrupted);
    assert_eq!(interrupted.terminal, Some(JobTerminal::Interrupted));
    assert!(
        plane.state.job_progress["lost-result"].trace_events > 0,
        "the real worker must persist evidence before losing its result"
    );
    assert!(matches!(
        plane.replay_response().expect("replay interrupted result"),
        ResponseData::Replay { .. }
    ));
    exercise_dispatch_job(&mut plane, &genome.genome_id);
    drop(plane);
    let reopened = ControlPlane::open(directory.path()).expect("reopen after result loss");
    assert_eq!(
        reopened.state.jobs["lost-result"].state,
        JobState::Interrupted
    );
    assert_eq!(
        reopened.state.jobs["dispatch-run"].state,
        JobState::Succeeded
    );
}

fn assert_interrupted_terminal_append_failure(plane: &mut ControlPlane) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut terminal_append_rejected = false;
    while plane.active_job.is_some() {
        match plane.service_async_messages() {
            Ok(()) => {}
            Err(ControlError::Projection(message))
                if message == "interrupted job could not be recorded" =>
            {
                terminal_append_rejected = true;
                break;
            }
            Err(error) => panic!("unexpected direct-job service failure: {error}"),
        }
        assert!(Instant::now() < deadline, "executor did not finish");
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    assert!(
        terminal_append_rejected,
        "injected terminal append must fail"
    );
    assert!(
        matches!(
            plane
                .job_result_receiver
                .as_ref()
                .expect("result receiver remains available")
                .try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ),
        "executor and supervisor must fully unwind before terminal retry"
    );
    assert!(
        plane.active_job.is_some(),
        "failed append retains the active projection"
    );
    let history_before_retry = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after rejected terminal append");
    assert!(
        !history_before_retry
            .iter()
            .any(|event| { event.event_id == "job:lost-result:terminal" })
    );
}

#[test]
fn arena_job_record_validation_rejects_plan_and_event_tampering() {
    let directory = tempdir().expect("fixture directory");
    let artifacts =
        ArtifactStore::open(directory.path().join("blobs")).expect("open fixture artifact store");
    let artifact = |contents: &[u8]| {
        artifacts
            .put(contents)
            .expect("write fixture artifact")
            .as_str()
            .to_owned()
    };
    let evaluation_id = "validated-pair";
    let parent_genome_id = format!("hephaestus:genome:{}", "1".repeat(64));
    let candidate_genome_id = format!("hephaestus:genome:{}", "2".repeat(64));
    let world_id = format!("hephaestus:world:{}", "3".repeat(64));
    let visible_manifest_id = artifact(b"visible");
    let sealed_manifest_id = artifact(b"sealed");
    let evaluator_id = artifact(b"evaluator");
    let environment_digest = artifact(b"environment");
    let ordered_trial_run_ids = vec![
        paired_run_id(evaluation_id, "parent", 0),
        paired_run_id(evaluation_id, "candidate", 0),
    ];
    let source_revision = "4".repeat(40);
    let plan_commitment = blake3::hash(
        &serde_json::to_vec(&(
            evaluation_id,
            &parent_genome_id,
            &candidate_genome_id,
            &world_id,
            &visible_manifest_id,
            &sealed_manifest_id,
            &source_revision,
            &format!("reference-v1.{environment_digest}"),
            &format!("reference-v1.{environment_digest}"),
            &ordered_trial_run_ids,
        ))
        .expect("serialize committed plan"),
    )
    .to_hex()
    .to_string();
    let record = ArenaJobRecord {
        job_id: evaluation_id.to_owned(),
        evaluation_id: evaluation_id.to_owned(),
        parent_genome_id,
        candidate_genome_id,
        world_id,
        visible_manifest_id,
        sealed_manifest_id,
        evaluator_id,
        source_revision,
        worker_digest: "5".repeat(64),
        environment_id: format!("reference-v1.{environment_digest}"),
        candidate_environment_id: None,
        seed: PAIRED_EVALUATION_SEED,
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
        ordered_trial_run_ids,
        parent_trial_count: 1,
        total_trials: 2,
        plan_commitment,
        caller_id: "control-daemon".to_owned(),
        receipt_timestamp_millis: 1,
        completed_trials: 0,
        phase: ArenaJobPhase::Preparing,
        state: JobState::Admitted,
        terminal: None,
        evaluation: None,
        remote: false,
    };
    let event_for = |record: &ArenaJobRecord| StoredEvent {
        sequence: 1,
        event_id: format!("arena-job:{}:admitted", record.evaluation_id),
        aggregate_id: format!("arena-job:{}", record.evaluation_id),
        event_type: "arena.job.admitted".to_owned(),
        actor: RUNTIME_ACTOR.to_owned(),
        timestamp_millis: 1,
        payload: serde_json::to_vec(record).expect("serialize Arena job"),
        previous_hash: [0; 32],
        hash: [0; 32],
    };

    validate_arena_job_record(&event_for(&record), &record)
        .expect("canonical admitted plan validates");

    let mut wrong_order = record.clone();
    wrong_order.ordered_trial_run_ids.swap(0, 1);
    assert!(validate_arena_job_record(&event_for(&wrong_order), &wrong_order).is_err());

    let mut wrong_commitment = record.clone();
    wrong_commitment.plan_commitment = "6".repeat(64);
    assert!(validate_arena_job_record(&event_for(&wrong_commitment), &wrong_commitment).is_err());

    let mut wrong_event = event_for(&record);
    wrong_event.actor = "operator".to_owned();
    assert!(validate_arena_job_record(&wrong_event, &record).is_err());
}

fn admitted_arena_record(
    evaluation_id: &str,
    world_id: String,
    parent_genome_id: String,
    candidate_genome_id: String,
) -> ArenaJobRecord {
    let visible_manifest_id = "a".repeat(64);
    let sealed_manifest_id = "b".repeat(64);
    let environment_digest = "c".repeat(64);
    let environment_id = format!("reference-v1.{environment_digest}");
    let ordered_trial_run_ids = vec![
        paired_run_id(evaluation_id, "parent", 0),
        paired_run_id(evaluation_id, "candidate", 0),
    ];
    let source_revision = "d".repeat(40);
    let plan_commitment = blake3::hash(
        &serde_json::to_vec(&(
            evaluation_id,
            &parent_genome_id,
            &candidate_genome_id,
            &world_id,
            &visible_manifest_id,
            &sealed_manifest_id,
            &source_revision,
            &environment_id,
            &environment_id,
            &ordered_trial_run_ids,
        ))
        .expect("serialize admitted plan"),
    )
    .to_hex()
    .to_string();
    ArenaJobRecord {
        job_id: evaluation_id.to_owned(),
        evaluation_id: evaluation_id.to_owned(),
        parent_genome_id,
        candidate_genome_id,
        world_id,
        visible_manifest_id,
        sealed_manifest_id,
        evaluator_id: "e".repeat(64),
        source_revision,
        worker_digest: "f".repeat(64),
        environment_id,
        candidate_environment_id: None,
        seed: PAIRED_EVALUATION_SEED,
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
        ordered_trial_run_ids,
        parent_trial_count: 1,
        total_trials: 2,
        plan_commitment,
        caller_id: "control-daemon".to_owned(),
        receipt_timestamp_millis: 1,
        completed_trials: 0,
        phase: ArenaJobPhase::Preparing,
        state: JobState::Admitted,
        terminal: None,
        evaluation: None,
        remote: false,
    }
}

fn admitted_arena_event(record: &ArenaJobRecord) -> StoredEvent {
    StoredEvent {
        sequence: 1,
        event_id: format!("arena-job:{}:admitted", record.evaluation_id),
        aggregate_id: format!("arena-job:{}", record.evaluation_id),
        event_type: "arena.job.admitted".to_owned(),
        actor: RUNTIME_ACTOR.to_owned(),
        timestamp_millis: 1,
        payload: serde_json::to_vec(record).expect("serialize Arena job"),
        previous_hash: [0; 32],
        hash: [0; 32],
    }
}

fn open_projection_test_plane(directory: &TempDir) -> ControlPlane {
    let executable = env::current_exe().expect("test executable");
    ControlPlane::open_with_repository_evaluator_and_reference_worker(
        directory.path(),
        env::current_dir().expect("repository working directory"),
        &executable,
        &executable,
    )
    .expect("open control plane with explicit test executables")
}

fn append_projection_event(plane: &mut ControlPlane, event: StoredEvent) {
    plane
        .storage
        .as_mut()
        .expect("canonical storage")
        .ledger
        .append(EventInput::new(
            event.event_id,
            event.aggregate_id,
            event.event_type,
            event.actor,
            event.timestamp_millis,
            event.payload,
        ))
        .expect("append malformed domain event with a valid hash chain");
}

fn assert_restart_rejects(directory: &TempDir) {
    assert!(
        ControlPlane::open_with_repository_evaluator_and_reference_worker(
            directory.path(),
            env::current_dir().expect("repository working directory"),
            env::current_exe().expect("test evaluator executable"),
            env::current_exe().expect("test worker executable"),
        )
        .is_err(),
        "restart must reject malformed canonical Arena history"
    );
}

fn register_thread_failure_arena_objects(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
) -> (WorldRecord, GenomeRecord, GenomeRecord) {
    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    let visible = TrustedManifest::new(
        "thread-failure-visible",
        Visibility::Visible,
        vec![TrustedTask::new("visible-task", "visible", "VISIBLE").expect("visible task")],
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        "thread-failure-sealed",
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", "sealed", "SEALED").expect("sealed task")],
    )
    .expect("sealed manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator = env::current_exe().expect("test evaluator executable");
    let evaluator_id = artifacts
        .put(&fs::read(evaluator).expect("read test evaluator executable"))
        .expect("store test evaluator");
    let verifier_id = artifacts
        .put(&plane.run_result_verifier.public_key_bytes())
        .expect("store runtime verifier");
    drop(artifacts);

    let world_path = directory.path().join("thread-failure-world.json");
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"thread-failure-world","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
        ),
    )
    .expect("write Arena World source");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        "register-thread-failure-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("Arena World registration should succeed");
    };

    let register_genome = |plane: &mut ControlPlane, name: &str, parents: &str| {
        let path = directory.path().join(format!("{name}.md"));
        fs::write(
            &path,
            format!(
                "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
            ),
        )
        .expect("write reference Genome source");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            token,
            name,
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("Arena Genome registration should succeed");
        };
        genome
    };
    let parent = register_genome(plane, "thread-failure-parent", "[]");
    let candidate = register_genome(
        plane,
        "thread-failure-candidate",
        &format!("[\"{}\"]", parent.genome_id),
    );
    (world, parent, candidate)
}

#[test]
fn arena_restart_rejects_malformed_evaluation_and_job_history() {
    for (index, malformed_payload) in [br"{}".as_slice(), br#"{"evaluation_id":7}"#.as_slice()]
        .into_iter()
        .enumerate()
    {
        let directory = tempdir().expect("daemon directory");
        let mut plane = open_projection_test_plane(&directory);
        let token = plane.token_hex.clone();
        let _ = register_thread_failure_arena_objects(&mut plane, &token, &directory);
        let mut event = stored_event(
            0,
            "evaluation.recorded",
            "arena:evaluation:malformed",
            "arena-plane",
            malformed_payload,
        );
        event.event_id = format!("evaluation.recorded:malformed:{index}");
        append_projection_event(&mut plane, event);
        drop(plane);
        assert_restart_rejects(&directory);
    }

    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let _ = register_thread_failure_arena_objects(&mut plane, &token, &directory);
    for index in 0..2 {
        let mut event = stored_event(
            0,
            "evaluation.recorded",
            "arena:evaluation:duplicate",
            "arena-plane",
            br#"{"evaluation_id":"duplicate-evaluation"}"#,
        );
        event.event_id = format!("evaluation.recorded:duplicate:{index}");
        append_projection_event(&mut plane, event);
    }
    drop(plane);
    assert_restart_rejects(&directory);

    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (world, parent, candidate) =
        register_thread_failure_arena_objects(&mut plane, &token, &directory);
    let mut running = admitted_arena_record(
        "orphan-running",
        world.world_id,
        parent.genome_id,
        candidate.genome_id,
    );
    running.state = JobState::Running;
    running.phase = ArenaJobPhase::ParentTrials;
    let mut event = admitted_arena_event(&running);
    event.event_type = "arena.job.running".to_owned();
    event.event_id = format!("arena-job:{}:running", running.evaluation_id);
    append_projection_event(&mut plane, event);
    drop(plane);
    assert_restart_rejects(&directory);

    for malformed_plan in ["zero-trial-budget", "invalid-worker-digest"] {
        let directory = tempdir().expect("daemon directory");
        let mut plane = open_projection_test_plane(&directory);
        let token = plane.token_hex.clone();
        let (world, parent, candidate) =
            register_thread_failure_arena_objects(&mut plane, &token, &directory);
        let mut record = admitted_arena_record(
            malformed_plan,
            world.world_id,
            parent.genome_id,
            candidate.genome_id,
        );
        if malformed_plan == "zero-trial-budget" {
            record.trial_budget.wall_millis = 0;
        } else {
            record.worker_digest = "g".repeat(64);
        }
        append_projection_event(&mut plane, admitted_arena_event(&record));
        drop(plane);
        assert_restart_rejects(&directory);
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn direct_thread_launch_failure_persists_interruption_and_releases_slot() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (_, genome, candidate) =
        register_thread_failure_arena_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    plane.thread_spawn_failures.direct = true;
    assert!(matches!(
        plane.submit_job("launch-failed-one", &genome.genome_id),
        Err(ExecuteError::Internal)
    ));
    let first = &plane.state.jobs["launch-failed-one"];
    assert_eq!(first.state, JobState::Interrupted);
    assert_eq!(first.terminal, Some(JobTerminal::Interrupted));
    assert!(plane.active_job.is_none());
    assert!(plane.job_result_receiver.is_none());
    assert!(!plane.state.active_runs.contains(&first.run_id));

    plane.thread_spawn_failures.direct = true;
    assert!(
        matches!(
            plane.submit_job("launch-failed-two", &genome.genome_id),
            Err(ExecuteError::Internal)
        ),
        "second admission must reach the injected launch boundary instead of returning Busy"
    );
    assert_eq!(
        plane.state.jobs["launch-failed-two"].state,
        JobState::Interrupted
    );
    assert!(plane.active_job.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify launch-failure history");
    for job_id in ["launch-failed-one", "launch-failed-two"] {
        assert_eq!(
            history
                .iter()
                .filter(|event| {
                    event.event_type == "job.terminal"
                        && event.aggregate_id == format!("job:{job_id}")
                })
                .count(),
            1
        );
    }
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay interrupted admissions"),
        ResponseData::Replay { .. }
    ));

    let failed_terminal_id = "launch-failed-terminal-write";
    let database = rusqlite::Connection::open(directory.path().join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    database
        .execute_batch(
            "CREATE TRIGGER reject_direct_launch_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'job:launch-failed-terminal-write:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture direct launch terminal failure'); END;",
        )
        .expect("reject direct launch terminal append");
    plane.thread_spawn_failures.direct = true;
    assert!(matches!(
        plane.submit_job(failed_terminal_id, &genome.genome_id),
        Err(ExecuteError::Internal)
    ));
    let still_running = &plane.state.jobs[failed_terminal_id];
    assert_eq!(still_running.state, JobState::Running);
    assert_eq!(still_running.terminal, None);
    assert!(plane.active_job.is_none());
    assert!(plane.job_result_receiver.is_none());
    assert!(plane.job_evidence_receiver.is_none());
    let events_before_arena = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify direct launch failure history");
    assert!(events_before_arena.iter().any(|event| {
        event.event_id == format!("job:{failed_terminal_id}:running")
            && event.event_type == "job.running"
    }));
    assert!(
        !events_before_arena
            .iter()
            .any(|event| event.event_id == format!("job:{failed_terminal_id}:terminal"))
    );

    let admission = dispatch_call(
        &mut plane,
        &token,
        "arena-blocked-by-running-history",
        Command::EvaluatePair {
            evaluation_id: "blocked-after-launch-failure".to_owned(),
            parent_genome_id: genome.genome_id.clone(),
            candidate_genome_id: candidate.genome_id.clone(),
            remote: false,
        },
    );
    assert_eq!(
        admission
            .error
            .expect("running durable job blocks Arena")
            .code,
        ApiErrorCode::Busy
    );
    assert!(
        !plane
            .state
            .arena_jobs
            .contains_key("blocked-after-launch-failure")
    );
    assert!(
        !plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify denied Arena admission")
            .iter()
            .any(|event| event.event_id == "arena-job:blocked-after-launch-failure:admitted")
    );

    database
        .execute_batch("DROP TRIGGER reject_direct_launch_terminal;")
        .expect("restore direct job terminal writes");
    drop(database);

    drop(plane);
    let reopened = open_projection_test_plane(&directory);
    for job_id in ["launch-failed-one", "launch-failed-two"] {
        assert_eq!(reopened.state.jobs[job_id].state, JobState::Interrupted);
    }
    assert!(reopened.active_job.is_none());
    let recovered = &reopened.state.jobs[failed_terminal_id];
    assert_eq!(recovered.state, JobState::Interrupted);
    assert_eq!(recovered.terminal, Some(JobTerminal::Interrupted));
    assert_eq!(
        reopened
            .storage
            .as_ref()
            .expect("reopened canonical storage")
            .ledger
            .replay_verified()
            .expect("verify recovered direct launch terminal")
            .iter()
            .filter(|event| event.event_id == format!("job:{failed_terminal_id}:terminal"))
            .count(),
        1
    );
}

#[test]
fn arena_thread_launch_failure_persists_interruption_and_releases_slot() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (_, parent, candidate) =
        register_thread_failure_arena_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    plane.thread_spawn_failures.arena = true;
    assert!(matches!(
        plane.submit_arena_job(
            "arena-launch-failed-one",
            &parent.genome_id,
            &candidate.genome_id,
            false
        ),
        Err(ExecuteError::Internal)
    ));
    let first = &plane.state.arena_jobs["arena-launch-failed-one"];
    assert_eq!(first.state, JobState::Interrupted);
    assert_eq!(first.phase, ArenaJobPhase::Terminal);
    assert_eq!(first.terminal, Some(JobTerminal::Interrupted));
    assert!(first.evaluation.is_none());
    assert!(plane.active_arena_job.is_none());
    assert!(plane.arena_message_receiver.is_none());
    assert!(plane.arena_message_sender.is_none());

    plane.thread_spawn_failures.arena = true;
    assert!(
        matches!(
            plane.submit_arena_job(
                "arena-launch-failed-two",
                &parent.genome_id,
                &candidate.genome_id,
                false
            ),
            Err(ExecuteError::Internal)
        ),
        "second Arena admission must reach the injected launch boundary instead of returning Busy"
    );
    assert_eq!(
        plane.state.arena_jobs["arena-launch-failed-two"].state,
        JobState::Interrupted
    );
    assert!(plane.active_arena_job.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify Arena launch-failure history");
    for evaluation_id in ["arena-launch-failed-one", "arena-launch-failed-two"] {
        assert_eq!(
            history
                .iter()
                .filter(|event| {
                    event.event_type == "arena.job.terminal"
                        && event.aggregate_id == format!("arena-job:{evaluation_id}")
                })
                .count(),
            1
        );
    }
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay interrupted Arena admissions"),
        ResponseData::Replay { .. }
    ));

    drop(plane);
    let reopened = open_projection_test_plane(&directory);
    for evaluation_id in ["arena-launch-failed-one", "arena-launch-failed-two"] {
        assert_eq!(
            reopened.state.arena_jobs[evaluation_id].state,
            JobState::Interrupted
        );
        assert_eq!(
            reopened.state.arena_jobs[evaluation_id].phase,
            ArenaJobPhase::Terminal
        );
    }
    assert!(reopened.active_arena_job.is_none());
}

#[test]
#[allow(clippy::too_many_lines)]
fn arena_replay_rejects_signed_trials_before_running_and_out_of_admitted_order() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (_, parent, candidate) =
        register_thread_failure_arena_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    // The injected spawn failure occurs after real admitted and running events
    // have been persisted, and before any worker can contribute trial results.
    plane.thread_spawn_failures.arena = true;
    assert!(matches!(
        plane.submit_arena_job(
            "signed-trial-order",
            &parent.genome_id,
            &candidate.genome_id,
            false
        ),
        Err(ExecuteError::Internal)
    ));
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify canonical admission history");
    let admitted_index = history
        .iter()
        .position(|event| event.event_id == "arena-job:signed-trial-order:admitted")
        .expect("real admitted event");
    let running_index = history
        .iter()
        .position(|event| event.event_id == "arena-job:signed-trial-order:running")
        .expect("real running event");
    assert!(admitted_index < running_index);
    let record: ArenaJobRecord =
        serde_json::from_slice(&history[running_index].payload).expect("running record");
    assert_eq!(record.parent_genome_id, parent.genome_id);
    assert_eq!(record.candidate_genome_id, candidate.genome_id);
    assert_eq!(record.state, JobState::Running);

    let (stdout_artifact_id, stderr_artifact_id) = put_fixture_output_artifacts(
        &plane,
        b"signed provider failure stdout",
        b"signed provider failure stderr",
    );
    let signed_trial = |index: usize, timestamp_millis: i64| {
        let parent_trial_count =
            usize::try_from(record.parent_trial_count).expect("parent trial count");
        let is_candidate = index >= parent_trial_count;
        let task_index = index % parent_trial_count;
        let (task_id, input) = if task_index == 0 {
            ("visible-task", "visible")
        } else {
            ("sealed-task", "sealed")
        };
        let receipt = RunResultReceipt {
            schema_version: RUN_RESULT_SCHEMA_VERSION,
            run_id: record.ordered_trial_run_ids[index].clone(),
            genome_id: if is_candidate {
                record.candidate_genome_id.clone()
            } else {
                record.parent_genome_id.clone()
            },
            world_id: record.world_id.clone(),
            source_revision: record.source_revision.clone(),
            task_id: task_id.to_owned(),
            input_commitment: blake3::hash(input.as_bytes()).to_hex().to_string(),
            seed: record.seed,
            environment_id: record.environment_id.clone(),
            budget: record.trial_budget,
            completion_reason: RunCompletionReason::ProviderFailure,
            latency_millis: 1,
            actual_cost_microusd: 0,
            stdout_artifact_id: stdout_artifact_id.clone(),
            stderr_artifact_id: stderr_artifact_id.clone(),
            trace_artifact_ids: Vec::new(),
        };
        plane
            .run_result_signer
            .issue(receipt, timestamp_millis)
            .expect("sign trial result")
    };
    let candidate_trial_index =
        usize::try_from(record.parent_trial_count).expect("parent trial count");
    let candidate_timestamp = history[running_index]
        .timestamp_millis
        .checked_add(1)
        .expect("trial timestamp range");
    let candidate_result = signed_trial(candidate_trial_index, candidate_timestamp);
    let parent_result = signed_trial(
        0,
        candidate_timestamp
            .checked_add(1)
            .expect("parent trial timestamp range"),
    );
    let pre_running_parent_result = signed_trial(0, history[admitted_index].timestamp_millis);
    drop(plane);

    let events_path = directory.path().join("events.sqlite3");
    let rebuild_history = |prefix: &[StoredEvent], trial_events: Vec<EventInput>| {
        for suffix in ["", "-wal", "-shm"] {
            let _ignored = fs::remove_file(format!("{}{suffix}", events_path.display()));
        }
        let mut ledger = EventStore::open(&events_path).expect("create replay fixture ledger");
        for event in prefix {
            ledger
                .append(EventInput::new(
                    event.event_id.clone(),
                    event.aggregate_id.clone(),
                    event.event_type.clone(),
                    event.actor.clone(),
                    event.timestamp_millis,
                    &event.payload,
                ))
                .expect("copy authenticated fixture prefix");
        }
        for event in trial_events {
            ledger.append(event).expect("append signed trial");
        }
        drop(ledger);
    };
    let reopen_error = || {
        let executable = env::current_exe().expect("test executable");
        ControlPlane::open_with_repository_evaluator_and_reference_worker(
            directory.path(),
            env::current_dir().expect("repository directory"),
            &executable,
            &executable,
        )
        .err()
        .expect("replay must reject invalid signed trial order")
    };

    rebuild_history(
        &history[..=running_index],
        vec![candidate_result.clone(), parent_result.clone()],
    );
    let candidate_first = reopen_error();
    assert!(
        candidate_first
            .to_string()
            .contains("Arena trial result is out of admitted order"),
        "unexpected candidate-first rejection: {candidate_first}"
    );

    rebuild_history(
        &history[..=admitted_index],
        vec![
            pre_running_parent_result,
            history[running_index..]
                .iter()
                .find(|event| event.event_id == "arena-job:signed-trial-order:running")
                .map(|event| {
                    EventInput::new(
                        event.event_id.clone(),
                        event.aggregate_id.clone(),
                        event.event_type.clone(),
                        event.actor.clone(),
                        event.timestamp_millis,
                        &event.payload,
                    )
                })
                .expect("real running transition"),
        ],
    );
    let parent_before_running = reopen_error();
    assert!(
        parent_before_running
            .to_string()
            .contains("Arena trial result is out of admitted order"),
        "unexpected pre-running rejection: {parent_before_running}"
    );
}

#[test]
fn arena_replay_rejects_admitted_plan_without_its_registered_bindings() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let missing_id = |kind: &str| format!("hephaestus:{kind}:{}", "1".repeat(64));

    let missing_world = admitted_arena_record(
        "missing-world",
        missing_id("world"),
        missing_id("genome"),
        format!("hephaestus:genome:{}", "2".repeat(64)),
    );
    let event = admitted_arena_event(&missing_world);
    plane
        .storage
        .as_mut()
        .expect("canonical storage")
        .ledger
        .append(EventInput::new(
            event.event_id,
            event.aggregate_id,
            event.event_type,
            event.actor,
            event.timestamp_millis,
            event.payload,
        ))
        .expect("persist structurally valid Arena admission");
    assert!(matches!(
        plane.replay_response(),
        Err(ExecuteError::Internal)
    ));

    drop(plane);
    let directory = tempdir().expect("clean daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (world, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let missing_parent = admitted_arena_record(
        "missing-parent",
        world.world_id.clone(),
        missing_id("genome"),
        format!("hephaestus:genome:{}", "2".repeat(64)),
    );
    assert!(matches!(
        plane
            .state
            .apply_arena_job_record(&admitted_arena_event(&missing_parent)),
        Err(ControlError::Projection(message)) if message == "Arena parent Genome is not registered"
    ));

    let missing_candidate = admitted_arena_record(
        "missing-candidate",
        world.world_id.clone(),
        genome.genome_id.clone(),
        format!("hephaestus:genome:{}", "2".repeat(64)),
    );
    assert!(matches!(
        plane
            .state
            .apply_arena_job_record(&admitted_arena_event(&missing_candidate)),
        Err(ControlError::Projection(message)) if message == "Arena candidate Genome is not registered"
    ));

    let candidate_path = directory.path().join("bound-candidate.md");
    fs::write(
        &candidate_path,
        "---\nschema_version: 1\nname: bound-candidate\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write bound candidate Genome");
    let Some(ResponseData::Genome { genome: candidate }) = dispatch_call(
        &mut plane,
        &token,
        "register-bound-candidate",
        Command::GenomeRegister {
            path: candidate_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("candidate Genome should register");
    };
    let mismatched_world_artifact_binding = admitted_arena_record(
        "mismatched-world-artifact-binding",
        world.world_id.clone(),
        genome.genome_id.clone(),
        candidate.genome_id,
    );
    assert!(matches!(
        plane
            .state
            .apply_arena_job_record(&admitted_arena_event(&mismatched_world_artifact_binding)),
        Err(ControlError::Projection(message))
            if message == "Arena job differs from registered World and Genome bindings"
    ));
}

#[test]
fn control_projection_rejects_arena_admission_without_mutating_state() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (world, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let candidate_path = directory.path().join("unbound-admission-candidate.md");
    fs::write(
        &candidate_path,
        "---\nschema_version: 1\nname: unbound-admission-candidate\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write candidate Genome");
    let Some(ResponseData::Genome { genome: candidate }) = dispatch_call(
        &mut plane,
        &token,
        "register-unbound-admission-candidate",
        Command::GenomeRegister {
            path: candidate_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("candidate Genome should register");
    };
    let before = plane.state.snapshot();
    let record = admitted_arena_record(
        "unbound-arena-admission",
        world.world_id,
        genome.genome_id,
        candidate.genome_id,
    );
    let mut event = admitted_arena_event(&record);
    event.sequence = before.event_count + 1;

    assert!(matches!(
        plane
            .state
            .apply(&event, &plane.operator_token, &plane.run_result_verifier),
        Err(ControlError::Projection(message))
            if message == "Arena job differs from registered World and Genome bindings"
    ));
    assert!(plane.state.snapshot() == before);
}

#[test]
fn arena_trial_source_rejection_preserves_admitted_progress() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (world, parent, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let candidate_path = directory.path().join("trial-candidate.md");
    fs::write(
        &candidate_path,
        "---\nschema_version: 1\nname: trial-candidate\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write candidate Genome");
    let Some(ResponseData::Genome { genome: candidate }) = dispatch_call(
        &mut plane,
        &token,
        "register-trial-candidate",
        Command::GenomeRegister {
            path: candidate_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("candidate Genome should register");
    };
    let mut job = admitted_arena_record(
        "trial-source-mismatch",
        world.world_id.clone(),
        parent.genome_id,
        candidate.genome_id.clone(),
    );
    job.state = JobState::Running;
    job.phase = ArenaJobPhase::ParentTrials;
    job.completed_trials = 0;
    plane
        .state
        .arena_jobs
        .insert(job.evaluation_id.clone(), job.clone());
    let receipt = RunResultReceipt {
        schema_version: RUN_RESULT_SCHEMA_VERSION,
        run_id: job.ordered_trial_run_ids[0].clone(),
        genome_id: job.parent_genome_id.clone(),
        world_id: job.world_id.clone(),
        source_revision: "e".repeat(40),
        task_id: "reference-inventory-v1".to_owned(),
        input_commitment: "1".repeat(64),
        seed: job.seed,
        environment_id: job.environment_id.clone(),
        budget: job.trial_budget,
        completion_reason: RunCompletionReason::Success,
        latency_millis: 1,
        actual_cost_microusd: 0,
        stdout_artifact_id: "2".repeat(64),
        stderr_artifact_id: "3".repeat(64),
        trace_artifact_ids: Vec::new(),
    };

    assert!(matches!(
        plane.state.advance_arena_trial(&receipt),
        Err(ControlError::Projection(message))
            if message == "Arena run receipt differs from its admitted source"
    ));
    assert_eq!(plane.state.arena_jobs[&job.evaluation_id], job);
}

#[test]
fn selection_history_rejects_unregistered_world_reference() {
    let directory = tempdir().expect("daemon directory");
    let plane = open_projection_test_plane(&directory);
    let payload = br#"{"schema_version":1,"evaluation_id":"forged-selection","world_id":"hephaestus:world:1111111111111111111111111111111111111111111111111111111111111111","receipt_artifact_id":"2222222222222222222222222222222222222222222222222222222222222222"}"#;
    let event = StoredEvent {
        sequence: 1,
        event_id: "arena:selection:forged-selection:selected".to_owned(),
        aggregate_id: "arena:selection:forged-selection".to_owned(),
        event_type: "selection.recorded".to_owned(),
        actor: "arena-plane".to_owned(),
        timestamp_millis: 1,
        payload: payload.to_vec(),
        previous_hash: [0; 32],
        hash: [0; 32],
    };

    assert!(matches!(
        verify_selection_history(&plane.storage.as_ref().unwrap().artifacts, &[event], &plane.state.registered),
        Err(ControlError::Projection(message)) if message == "selection World is not registered"
    ));
}

fn invariant_event_payload(
    evaluation_id: &str,
    world_id: &str,
    receipt_artifact_id: &str,
) -> Vec<u8> {
    format!(
        r#"{{"schema_version":1,"evaluation_id":"{evaluation_id}","world_id":"{world_id}","receipt_artifact_id":"{receipt_artifact_id}"}}"#
    )
    .into_bytes()
}

fn invariant_checked_event(evaluation_id: &str, payload: Vec<u8>) -> StoredEvent {
    StoredEvent {
        sequence: 1,
        event_id: format!("arena:invariants:{evaluation_id}:checked"),
        aggregate_id: format!("arena:invariants:{evaluation_id}"),
        event_type: "invariants.recorded".to_owned(),
        actor: "arena-plane".to_owned(),
        timestamp_millis: 1,
        payload,
        previous_hash: [0; 32],
        hash: [0; 32],
    }
}

#[test]
fn invariant_history_rejects_unregistered_world_and_unknown_evaluation() {
    let directory = tempdir().expect("daemon directory");
    let plane = open_projection_test_plane(&directory);
    let unregistered_world =
        "hephaestus:world:3333333333333333333333333333333333333333333333333333333333333333";
    let receipt_artifact_id = "4444444444444444444444444444444444444444444444444444444444444444";
    let event = invariant_checked_event(
        "forged-invariants",
        invariant_event_payload("forged-invariants", unregistered_world, receipt_artifact_id),
    );
    assert!(matches!(
        verify_invariant_history(&plane.storage.as_ref().unwrap().artifacts, &[event], &plane.state.registered),
        Err(ControlError::Projection(message)) if message == "invariant World is not registered"
    ));

    let mut plane = plane;
    let token = plane.token_hex.clone();
    let (world, _genome, _task) = register_dispatch_objects(&mut plane, &token, &directory);
    let event = invariant_checked_event(
        "no-such-evaluation",
        invariant_event_payload("no-such-evaluation", &world.world_id, receipt_artifact_id),
    );
    assert!(matches!(
        verify_invariant_history(&plane.storage.as_ref().unwrap().artifacts, &[event], &plane.state.registered),
        Err(ControlError::Projection(message)) if message == "canonical invariant receipt is invalid"
    ));
}

#[test]
fn signed_success_result_without_completed_run_rejects_job_terminal() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let job_id = "signed-terminal";
    let run_id = job_run_id(job_id);
    let source_revision = "1".repeat(40);
    let environment_id = format!("reference-v1.{}", "2".repeat(64));
    let input = "Inventory the isolated repository without modifying it or using the network.";
    let base = JobRecord {
        job_id: job_id.to_owned(),
        genome_id: genome.genome_id,
        run_id: run_id.clone(),
        source_revision,
        world_id: genome.world_id,
        task_id: "repository-inventory-v1".to_owned(),
        input_commitment: blake3::hash(input.as_bytes()).to_hex().to_string(),
        seed: 0,
        environment_id: environment_id.clone(),
        budget: RunBudgetReceipt {
            wall_millis: 10_000,
            maximum_output_bytes: 1_048_576,
            maximum_cost_microusd: 0,
        },
        state: JobState::Admitted,
        terminal: None,
    };
    plane
        .append_job_record(&base)
        .expect("persist valid job admission");
    plane
        .append_job_record(&JobRecord {
            state: JobState::Running,
            ..base.clone()
        })
        .expect("persist valid running transition");

    let (stdout_artifact_id, stderr_artifact_id) =
        put_fixture_output_artifacts(&plane, b"signed success stdout", b"signed success stderr");
    let receipt = RunResultReceipt {
        schema_version: RUN_RESULT_SCHEMA_VERSION,
        run_id: run_id.clone(),
        genome_id: base.genome_id.clone(),
        world_id: base.world_id.clone(),
        source_revision: base.source_revision.clone(),
        task_id: base.task_id.clone(),
        input_commitment: base.input_commitment.clone(),
        seed: base.seed,
        environment_id,
        budget: base.budget,
        completion_reason: RunCompletionReason::Success,
        latency_millis: 1,
        actual_cost_microusd: 0,
        stdout_artifact_id,
        stderr_artifact_id,
        trace_artifact_ids: vec![],
    };
    let signed_event = plane
        .run_result_signer
        .issue(receipt, 1)
        .expect("sign valid run result");
    let stored_result = plane
        .storage
        .as_mut()
        .expect("canonical storage")
        .ledger
        .append(signed_event)
        .expect("persist signed run result");
    plane
        .state
        .apply(
            &stored_result,
            &plane.operator_token,
            &plane.run_result_verifier,
        )
        .expect("replay signed success result");
    assert!(plane.state.run_results.contains_key(&run_id));
    assert!(!plane.state.completed_runs.contains(&run_id));

    reject_success_terminal_without_lifecycle(&mut plane, &base);
}

fn reject_success_terminal_without_lifecycle(plane: &mut ControlPlane, base: &JobRecord) {
    let terminal = JobRecord {
        state: JobState::Succeeded,
        terminal: Some(JobTerminal::Succeeded),
        ..base.clone()
    };
    let mut event = stored_event(
        plane.state.event_count + 1,
        "job.terminal",
        &format!("job:{}", base.job_id),
        RUNTIME_ACTOR,
        &serde_json::to_vec(&terminal).expect("serialize successful terminal"),
    );
    event.event_id = format!("job:{}:terminal", base.job_id);
    assert!(matches!(
        plane
            .state
            .apply(&event, &plane.operator_token, &plane.run_result_verifier),
        Err(ControlError::Projection(message))
            if message == "successful job terminal lacks matching signed result and lifecycle completion"
    ));
}

#[allow(clippy::too_many_lines)]
fn append_signed_result_fixture(
    plane: &mut ControlPlane,
    genome: &GenomeRecord,
    cancellation_requested: bool,
    mismatch_source_revision: bool,
    completion_reason: RunCompletionReason,
) -> JobRecord {
    let job_id = "interrupted-recovery";
    let run_id = job_run_id(job_id);
    let input = "Inventory the isolated repository without modifying it or using the network.";
    let base = JobRecord {
        job_id: job_id.to_owned(),
        genome_id: genome.genome_id.clone(),
        run_id: run_id.clone(),
        source_revision: "1".repeat(40),
        world_id: genome.world_id.clone(),
        task_id: "repository-inventory-v1".to_owned(),
        input_commitment: blake3::hash(input.as_bytes()).to_hex().to_string(),
        seed: 0,
        environment_id: format!("reference-v1.{}", "2".repeat(64)),
        budget: RunBudgetReceipt {
            wall_millis: 10_000,
            maximum_output_bytes: 1_048_576,
            maximum_cost_microusd: 0,
        },
        state: JobState::Admitted,
        terminal: None,
    };
    plane
        .append_job_record(&base)
        .expect("persist valid job admission");
    plane
        .append_job_record(&JobRecord {
            state: JobState::Running,
            ..base.clone()
        })
        .expect("persist valid running transition");
    if cancellation_requested {
        plane
            .append_job_record(&JobRecord {
                state: JobState::CancellationRequested,
                ..base.clone()
            })
            .expect("persist cancellation request");
    }

    let (stdout_artifact_id, stderr_artifact_id) = put_fixture_output_artifacts(
        plane,
        b"signed interrupted stdout",
        b"signed interrupted stderr",
    );
    let receipt = RunResultReceipt {
        schema_version: RUN_RESULT_SCHEMA_VERSION,
        run_id,
        genome_id: base.genome_id.clone(),
        world_id: base.world_id.clone(),
        source_revision: if mismatch_source_revision {
            "6".repeat(40)
        } else {
            base.source_revision.clone()
        },
        task_id: base.task_id.clone(),
        input_commitment: base.input_commitment.clone(),
        seed: base.seed,
        environment_id: base.environment_id.clone(),
        budget: base.budget,
        completion_reason,
        latency_millis: 1,
        actual_cost_microusd: 0,
        stdout_artifact_id,
        stderr_artifact_id,
        trace_artifact_ids: Vec::new(),
    };
    let signed_event = plane
        .run_result_signer
        .issue(receipt, 1)
        .expect("sign run result");
    let stored_result = plane
        .storage
        .as_mut()
        .expect("canonical storage")
        .ledger
        .append(signed_event)
        .expect("persist signed run result");
    plane
        .state
        .apply(
            &stored_result,
            &plane.operator_token,
            &plane.run_result_verifier,
        )
        .expect("project signed run result");
    base
}

fn put_fixture_output_artifacts(
    plane: &ControlPlane,
    stdout: &[u8],
    stderr: &[u8],
) -> (String, String) {
    let artifacts = ArtifactStore::open(plane.data_dir.join("blobs")).expect("open CAS");
    let stdout_id = artifacts
        .put(stdout)
        .expect("store stdout")
        .as_str()
        .to_owned();
    let stderr_id = artifacts
        .put(stderr)
        .expect("store stderr")
        .as_str()
        .to_owned();
    (stdout_id, stderr_id)
}

#[test]
fn non_cancellation_operator_interrupt_completes_as_interrupted() {
    let directory = tempdir().expect("daemon directory");
    let slow_worker = directory.path().join("interrupt-worker");
    fs::write(&slow_worker, "#!/bin/sh\nexec /bin/sleep 60\n").expect("write slow worker");
    fs::set_permissions(&slow_worker, fs::Permissions::from_mode(0o700))
        .expect("make slow worker executable");
    let executable = env::current_exe().expect("test executable");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        directory.path(),
        env::current_dir().expect("repository working directory"),
        &executable,
        &slow_worker,
    )
    .expect("open control plane with slow worker");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let job_id = "operator-interrupt-completion";
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    plane
        .submit_job(job_id, &genome.genome_id)
        .expect("admit direct job");
    let cancel = Arc::clone(&plane.active_job.as_ref().expect("active direct job").cancel);
    thread::sleep(Duration::from_millis(100));
    cancel.store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("service interrupted worker");
        assert!(Instant::now() < deadline, "interrupted worker stalled");
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let terminal = plane.state.jobs.get(job_id).expect("terminal job");
    assert_eq!(terminal.state, JobState::Interrupted);
    assert_eq!(terminal.terminal, Some(JobTerminal::Interrupted));
    let run_id = job_run_id(job_id);
    assert_eq!(
        plane.state.run_results[&run_id].completion_reason,
        RunCompletionReason::OperatorInterrupt
    );
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify interrupted history");
    assert_eq!(
        history
            .iter()
            .filter(|event| {
                event.event_type == "job.terminal" && event.aggregate_id == format!("job:{job_id}")
            })
            .count(),
        1
    );
    assert!(plane.active_job.is_none(), "interrupted job releases slot");
}

#[test]
fn signed_interrupted_result_recovers_once_and_preserves_cancellation() {
    for (cancellation_requested, terminal) in [
        (false, JobTerminal::Interrupted),
        (true, JobTerminal::Cancelled),
    ] {
        let directory = tempdir().expect("daemon directory");
        let mut plane = open_projection_test_plane(&directory);
        let token = plane.token_hex.clone();
        let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
        let job = append_signed_result_fixture(
            &mut plane,
            &genome,
            cancellation_requested,
            false,
            RunCompletionReason::OperatorInterrupt,
        );
        let before_forged_terminal = plane.state.snapshot();
        let mut forged_success = plane.state.jobs[&job.job_id].clone();
        forged_success.state = JobState::Succeeded;
        forged_success.terminal = Some(JobTerminal::Succeeded);
        let mut event = stored_event(
            before_forged_terminal.event_count + 1,
            "job.terminal",
            &format!("job:{}", job.job_id),
            RUNTIME_ACTOR,
            &serde_json::to_vec(&forged_success).expect("serialize forged success"),
        );
        event.event_id = format!("job:{}:terminal", job.job_id);
        assert!(matches!(
            plane
                .state
                .apply(&event, &plane.operator_token, &plane.run_result_verifier),
            Err(ControlError::Projection(message))
                if message == "successful job terminal lacks matching signed result and lifecycle completion"
        ));
        assert!(plane.state.snapshot() == before_forged_terminal);
        drop(plane);

        for _ in 0..2 {
            let reopened = open_projection_test_plane(&directory);
            let recovered = reopened.state.jobs.get(&job.job_id).expect("recovered job");
            assert_eq!(recovered.state, JobState::Interrupted);
            assert_eq!(recovered.terminal, Some(terminal));
            let history = EventStore::open(directory.path().join("events.sqlite3"))
                .expect("open recovered history")
                .replay_verified()
                .expect("verify recovered history");
            assert_eq!(
                history
                    .iter()
                    .filter(|event| {
                        event.event_type == "job.terminal"
                            && event.aggregate_id == format!("job:{}", job.job_id)
                    })
                    .count(),
                1,
                "recovery must append exactly one terminal event"
            );
            drop(reopened);
        }
    }
}

#[test]
fn signed_provider_failure_recovers_as_failed_once() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let job = append_signed_result_fixture(
        &mut plane,
        &genome,
        false,
        false,
        RunCompletionReason::ProviderFailure,
    );
    let run_id = job.run_id.clone();
    assert_eq!(
        plane.state.run_results[&run_id].completion_reason,
        RunCompletionReason::ProviderFailure
    );
    drop(plane);

    for _ in 0..2 {
        let reopened = open_projection_test_plane(&directory);
        let recovered = reopened.state.jobs.get(&job.job_id).expect("recovered job");
        assert_eq!(recovered.state, JobState::Failed);
        assert_eq!(recovered.terminal, Some(JobTerminal::Failed));
        assert_eq!(
            reopened.state.run_results[&run_id].completion_reason,
            RunCompletionReason::ProviderFailure
        );
        assert!(matches!(
            reopened.replay_response().expect("replay failed run"),
            ResponseData::Replay { .. }
        ));
        let history = EventStore::open(directory.path().join("events.sqlite3"))
            .expect("open recovered history")
            .replay_verified()
            .expect("verify recovered history");
        assert_eq!(
            history
                .iter()
                .filter(|event| {
                    event.event_type == "job.terminal"
                        && event.aggregate_id == format!("job:{}", job.job_id)
                })
                .count(),
            1,
            "recovery must append exactly one failed terminal event"
        );
        drop(reopened);
    }
}

#[test]
fn mismatched_signed_interrupted_result_fails_recovery_without_terminal_append() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let job = append_signed_result_fixture(
        &mut plane,
        &genome,
        false,
        true,
        RunCompletionReason::OperatorInterrupt,
    );
    let before = EventStore::open(directory.path().join("events.sqlite3"))
        .expect("open pre-recovery history")
        .replay_verified()
        .expect("verify pre-recovery history");
    assert!(!before.iter().any(|event| {
        event.event_type == "job.terminal" && event.aggregate_id == format!("job:{}", job.job_id)
    }));
    drop(plane);

    let executable = env::current_exe().expect("test executable");
    let reopen = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        directory.path(),
        env::current_dir().expect("repository working directory"),
        &executable,
        &executable,
    );
    assert!(matches!(
        reopen,
        Err(ControlError::Projection(message))
            if message == "signed job result differs from its admitted spec"
    ));
    let after = EventStore::open(directory.path().join("events.sqlite3"))
        .expect("open history after rejected recovery")
        .replay_verified()
        .expect("verify history after rejected recovery");
    assert_eq!(after.len(), before.len());
    assert!(!after.iter().any(|event| {
        event.event_type == "job.terminal" && event.aggregate_id == format!("job:{}", job.job_id)
    }));
}

#[test]
fn paired_admission_rejects_world_missing_sealed_manifest() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    let visible = TrustedManifest::new(
        "visible-only",
        Visibility::Visible,
        vec![TrustedTask::new("visible-task", "visible", "VISIBLE").expect("visible task")],
    )
    .expect("visible manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    drop(artifacts);

    let world_path = directory.path().join("visible-only-world.json");
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"visible-only-world","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}"}}}}"#,
            visible_id.as_str()
        ),
    )
    .expect("write visible-only World");
    let Some(ResponseData::World { world }) = dispatch_call(
        &mut plane,
        &token,
        "visible-only-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("World should register");
    };

    let register_genome = |plane: &mut ControlPlane, name: &str| {
        let path = directory.path().join(format!("{name}.md"));
        fs::write(
            &path,
            format!(
                "---\nschema_version: 1\nname: {name}\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
            ),
        )
        .expect("write paired Genome");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            &token,
            name,
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("Genome should register");
        };
        genome
    };
    let parent = register_genome(&mut plane, "visible-only-parent");
    let candidate = register_genome(&mut plane, "visible-only-candidate");
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    assert!(matches!(
        plane.submit_arena_job("missing-sealed", &parent.genome_id, &candidate.genome_id, false),
        Err(ExecuteError::Rejected(message))
            if message == "World does not declare arena.sealed_manifest"
    ));
    assert!(!plane.state.arena_jobs.contains_key("missing-sealed"));
}

#[test]
fn paired_admission_rejects_world_missing_evaluator_after_valid_manifests() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let visible = TrustedManifest::new(
        "visible-without-evaluator",
        Visibility::Visible,
        vec![TrustedTask::new("visible-task", "visible", "VISIBLE").expect("visible task")],
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        "sealed-without-evaluator",
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", "sealed", "SEALED").expect("sealed task")],
    )
    .expect("sealed manifest");
    let world = register_manifest_world(
        &mut plane,
        &token,
        &directory,
        "missing-evaluator",
        &visible,
        &sealed,
        false,
    );
    let parent = register_identity_genome(
        &mut plane,
        &token,
        &directory,
        &world,
        "missing-evaluator-parent",
        "[]",
    );
    let candidate = register_identity_genome(
        &mut plane,
        &token,
        &directory,
        &world,
        "missing-evaluator-candidate",
        &format!("[\"{}\"]", parent.genome_id),
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    assert!(matches!(
        plane.submit_arena_job("missing-evaluator", &parent.genome_id, &candidate.genome_id, false),
        Err(ExecuteError::Rejected(message)) if message == "World does not declare arena.evaluator"
    ));
    assert!(!plane.state.arena_jobs.contains_key("missing-evaluator"));
    assert_no_arena_admission(&plane, "missing-evaluator");
}

#[test]
fn paired_admission_rejects_combined_manifest_tasks_above_bound() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = open_projection_test_plane(&directory);
    let token = plane.token_hex.clone();
    let visible_tasks = (0..1_000)
        .map(|index| {
            TrustedTask::new(format!("visible-{index}"), "visible input", "VISIBLE")
                .expect("valid visible task")
        })
        .collect();
    let visible = TrustedManifest::new(
        "maximum-visible-manifest",
        Visibility::Visible,
        visible_tasks,
    )
    .expect("manifest at individual task limit");
    let sealed = TrustedManifest::new(
        "one-sealed-task",
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", "sealed", "SEALED").expect("sealed task")],
    )
    .expect("sealed manifest");
    let world = register_manifest_world(
        &mut plane,
        &token,
        &directory,
        "combined-over-limit",
        &visible,
        &sealed,
        true,
    );
    let parent = register_identity_genome(
        &mut plane,
        &token,
        &directory,
        &world,
        "combined-limit-parent",
        "[]",
    );
    let candidate = register_identity_genome(
        &mut plane,
        &token,
        &directory,
        &world,
        "combined-limit-candidate",
        &format!("[\"{}\"]", parent.genome_id),
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    assert!(matches!(
        plane.submit_arena_job("combined-over-limit", &parent.genome_id, &candidate.genome_id, false),
        Err(ExecuteError::Invalid(message))
            if message == "paired task count is outside the bounded range"
    ));
    assert!(!plane.state.arena_jobs.contains_key("combined-over-limit"));
    assert_no_arena_admission(&plane, "combined-over-limit");
}

fn register_manifest_world(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    name: &str,
    visible: &TrustedManifest,
    sealed: &TrustedManifest,
    include_evaluator: bool,
) -> WorldRecord {
    let artifacts = ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical CAS");
    let visible_id = artifacts
        .put(&serde_json::to_vec(visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator_fields = if include_evaluator {
        let evaluator = env::current_exe().expect("locate test evaluator executable");
        let evaluator_id = artifacts
            .put(&fs::read(evaluator).expect("read test evaluator"))
            .expect("store test evaluator");
        let verifier_id = artifacts
            .put(&plane.run_result_verifier.public_key_bytes())
            .expect("store run result verifier");
        format!(
            r#","arena.evaluator":"{}","arena.runtime_verifier":"{}""#,
            evaluator_id.as_str(),
            verifier_id.as_str()
        )
    } else {
        String::new()
    };
    drop(artifacts);
    let world_path = directory.path().join(format!("{name}.json"));
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"{name}","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}"{evaluator_fields}}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
        ),
    )
    .expect("write World source");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        &format!("register-{name}"),
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("World registration should succeed");
    };
    world
}

fn register_identity_genome(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    world: &WorldRecord,
    name: &str,
    parents: &str,
) -> GenomeRecord {
    let path = directory.path().join(format!("{name}.md"));
    fs::write(
        &path,
        format!(
            "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
        ),
    )
    .expect("write reference Genome");
    let Some(ResponseData::Genome { genome }) = dispatch_call(
        plane,
        token,
        &format!("register-{name}"),
        Command::GenomeRegister {
            path: path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("Genome registration should succeed");
    };
    genome
}

fn assert_no_arena_admission(plane: &ControlPlane, evaluation_id: &str) {
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify admission rejection history");
    assert!(!history.iter().any(|event| {
        event.event_type == "arena.job.admitted"
            && event.aggregate_id == format!("arena-job:{evaluation_id}")
    }));
}

#[test]
fn late_arena_worker_messages_are_ignored_or_rejected_after_job_closes() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let (sender, receiver) = mpsc::sync_channel(2);
    let (reply, response) = mpsc::channel();
    sender
        .send(ArenaWorkerMessage::Trial {
            job_id: "closed-pair".to_owned(),
            index: 0,
            output: Err("late trial output".to_owned()),
            reply,
        })
        .expect("queue late trial");
    plane.arena_message_receiver = Some(receiver);
    plane
        .service_arena_message()
        .expect("reject a late trial without failing the daemon");
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(1))
            .expect("late worker receives rejection")
            .expect_err("closed Arena job cannot accept a trial"),
        "paired job is no longer active"
    );

    sender
        .send(ArenaWorkerMessage::Trials {
            job_id: "closed-pair".to_owned(),
            result: Err("late completion".to_owned()),
        })
        .expect("queue late completion");
    plane
        .service_arena_message()
        .expect("ignore a completion after job close");

    sender
        .send(ArenaWorkerMessage::Scoring {
            job_id: "closed-pair".to_owned(),
            result: Err("late score".to_owned()),
        })
        .expect("queue late score");
    assert!(plane.service_arena_message().is_err());
}

fn service_test_arena_evidence(plane: &mut ControlPlane) {
    for _ in 0..8 {
        let request = plane
            .job_evidence_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some(request) = request else { break };
        let active = plane
            .active_arena_job
            .as_ref()
            .expect("evidence belongs to active Arena job");
        assert!(active.trials.iter().any(|trial| {
            trial.spec.run_id() == request.run_id()
                && request.provenance().is_none_or(|provenance| {
                    provenance.run_id() == trial.spec.run_id()
                        && provenance.genome_id() == trial.spec.genome_id()
                        && provenance.world_id() == trial.spec.world_id()
                })
        }));

        let storage = plane.storage.take().expect("canonical writer available");
        let mut recorder = EvidenceRecorder::from_stores(
            storage.ledger,
            storage.artifacts,
            RedactionPolicy::new([plane.token_hex.clone()]),
            RetentionLimits::new(10_000, 65_536).expect("valid trace limits"),
        );
        let result = request.persist(&mut recorder);
        let (ledger, artifacts) = recorder.into_stores();
        plane.storage = Some(CanonicalStorage { ledger, artifacts });
        result.expect("persist authentic worker evidence");
        plane
            .refresh_projection()
            .expect("project authentic worker evidence");
    }
}

fn wait_for_test_arena_trial(plane: &mut ControlPlane, deadline: Instant) -> ArenaWorkerMessage {
    loop {
        service_test_arena_evidence(plane);
        match plane
            .arena_message_receiver
            .as_ref()
            .expect("Arena worker channel")
            .try_recv()
        {
            Ok(message @ ArenaWorkerMessage::Trial { .. }) => return message,
            Ok(ArenaWorkerMessage::Trials { result, .. }) => {
                panic!("worker finished before returning its first Trial: {result:?}");
            }
            Ok(ArenaWorkerMessage::Scoring { .. }) => {
                panic!("worker scored before returning a Trial");
            }
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "worker did not return a Trial");
                thread::sleep(Duration::from_millis(2));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("Arena worker disconnected before returning a Trial");
            }
        }
    }
}

fn deliver_test_evidence_request_through_control_plane(
    plane: &mut ControlPlane,
    request: EvidenceRequest,
) {
    let original_receiver = plane
        .job_evidence_receiver
        .take()
        .expect("active evidence receiver");
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send(request)
        .expect("queue captured evidence request");
    plane.job_evidence_receiver = Some(receiver);
    let result = plane.service_async_messages();
    drop(plane.job_evidence_receiver.take());
    plane.job_evidence_receiver = Some(original_receiver);
    result.expect("production evidence service returns");
}

fn take_test_record_trace_request(plane: &mut ControlPlane, deadline: Instant) -> EvidenceRequest {
    loop {
        let request = plane
            .job_evidence_receiver
            .as_ref()
            .expect("active evidence receiver")
            .try_recv();
        match request {
            Ok(request @ EvidenceRequest::RecordTrace { .. }) => return request,
            Ok(request @ EvidenceRequest::EnsureCapacity { .. }) => {
                deliver_test_evidence_request_through_control_plane(plane, request);
            }
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "worker did not request a trace");
                thread::sleep(Duration::from_millis(2));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("worker disconnected before requesting a trace");
            }
        }
    }
}

fn real_worker_arena_fixture(directory: &TempDir) -> (ControlPlane, GenomeRecord, GenomeRecord) {
    real_worker_arena_fixture_with_invariants(directory, None)
}

fn real_worker_arena_fixture_with_invariants(
    directory: &TempDir,
    invariant_manifest: Option<&[u8]>,
) -> (ControlPlane, GenomeRecord, GenomeRecord) {
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"Arena evidence fixture\n")
        .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);

    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        cargo_evaluator.is_file(),
        "missing evaluator {cargo_evaluator:?}"
    );
    let evaluator = directory.path().join("fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(worker.is_file(), "missing worker {worker:?}");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open real worker fixture");
    let token = plane.token_hex.clone();
    let (_, parent, candidate) = register_dispatch_arena_objects_with_invariants(
        &mut plane,
        &token,
        directory,
        invariant_manifest,
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-evidence", Command::Unfreeze)
            .error
            .is_none()
    );
    (plane, parent, candidate)
}

fn trace_count_for_run(plane: &ControlPlane, run_id: &str) -> usize {
    plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify trace count history")
        .iter()
        .filter(|event| {
            if event.event_type != "trace.recorded" {
                return false;
            }
            serde_json::from_slice::<TraceReceipt>(&event.payload)
                .is_ok_and(|receipt| receipt.provenance.run_id() == run_id)
        })
        .count()
}

fn drain_rejected_evidence_arena_job(plane: &mut ControlPlane, job_id: &str, deadline: Instant) {
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("drain rejected evidence worker messages");
        assert!(
            Instant::now() < deadline,
            "rejected evidence worker stalled"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let terminal = plane
        .state
        .arena_jobs
        .get(job_id)
        .expect("failed Arena job");
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert!(terminal.evaluation.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify failed evidence history");
    assert_eq!(
        history
            .iter()
            .filter(|event| {
                event.event_id == format!("arena-job:{job_id}:terminal")
                    && event.event_type == "arena.job.terminal"
            })
            .count(),
        1
    );
    assert!(!history.iter().any(|event| {
        event.event_id == format!("arena:evaluation:{job_id}:recorded")
            && event.event_type == "evaluation.recorded"
    }));
    assert!(matches!(
        plane.replay_response().expect("replay failed evidence job"),
        ResponseData::Replay { .. }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn arena_evidence_failures_cancel_through_production_writer_and_replay_once() {
    let directory = tempdir().expect("Arena evidence fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let data_dir = plane.data_dir.clone();

    for (job_id, invalidate_cas) in [
        ("arena-writer-unavailable", false),
        ("arena-cas-unavailable", true),
    ] {
        let trial_run_id = paired_run_id(job_id, "parent", 0);
        assert!(matches!(
            plane
                .submit_arena_job(job_id, &parent.genome_id, &candidate.genome_id, false)
                .expect("admit real Arena job"),
            ResponseData::ArenaJob { job } if job.state == JobState::Running
        ));
        let deadline = Instant::now() + Duration::from_secs(30);
        let request = take_test_record_trace_request(&mut plane, deadline);
        assert!(matches!(&request, EvidenceRequest::RecordTrace { .. }));
        assert_eq!(request.run_id(), trial_run_id);
        let traces_before = trace_count_for_run(&plane, &trial_run_id);

        if invalidate_cas {
            let blobs = data_dir.join("blobs");
            let saved_blobs = data_dir.join("blobs-before-evidence-failure");
            fs::rename(&blobs, &saved_blobs).expect("hide canonical CAS root");
            fs::write(&blobs, b"not a directory").expect("replace CAS root with a file");
            deliver_test_evidence_request_through_control_plane(&mut plane, request);
            fs::remove_file(&blobs).expect("remove invalid CAS root file");
            fs::rename(saved_blobs, blobs).expect("restore canonical CAS root");
        } else {
            let storage = plane.storage.take().expect("canonical writer present");
            deliver_test_evidence_request_through_control_plane(&mut plane, request);
            assert!(plane.storage.is_none(), "unavailable writer stays absent");
            plane.storage = Some(storage);
        }

        assert!(
            plane
                .active_arena_job
                .as_ref()
                .expect("active rejected evidence job")
                .cancel
                .load(Ordering::Acquire),
            "production evidence failure must cancel the active Arena worker"
        );
        assert_eq!(
            trace_count_for_run(&plane, &trial_run_id),
            traces_before,
            "rejected trace must not be committed"
        );
        drain_rejected_evidence_arena_job(&mut plane, job_id, deadline);
        assert!(
            !plane
                .storage
                .as_ref()
                .unwrap()
                .ledger
                .replay_verified()
                .unwrap()
                .iter()
                .any(|event| event.event_type == "run.result_recorded"
                    && event.event_id == format!("result:{trial_run_id}"))
        );
    }

    drop(plane);
    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let evaluator = directory.path().join("fixture-evaluator");
    let reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        directory.path().join("repository"),
        &evaluator,
        &worker,
    )
    .expect("restart and replay both failed evidence jobs");
    for job_id in ["arena-writer-unavailable", "arena-cas-unavailable"] {
        assert_eq!(reopened.state.arena_jobs[job_id].state, JobState::Failed);
        assert_eq!(
            reopened.state.arena_jobs[job_id].terminal,
            Some(JobTerminal::Failed)
        );
        assert!(reopened.state.arena_jobs[job_id].evaluation.is_none());
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn arena_trial_append_rejection_is_acknowledged_and_worker_failure_is_drained() {
    let directory = tempdir().expect("Arena fixture directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"Arena handshake fixture\n")
        .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);

    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        cargo_evaluator.is_file(),
        "missing evaluator {cargo_evaluator:?}"
    );
    let evaluator = directory.path().join("fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(worker.is_file(), "missing worker {worker:?}");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open real worker fixture");
    let token = plane.token_hex.clone();
    let (_, parent, candidate) = register_dispatch_arena_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-handshake", Command::Unfreeze)
            .error
            .is_none()
    );

    let job_id = "trial-ack-rejection";
    let first_run_id = paired_run_id(job_id, "parent", 0);
    let database = rusqlite::Connection::open(data_dir.join("events.sqlite3"))
        .expect("open trial failure trigger connection");
    database
        .execute_batch(&format!(
            "CREATE TRIGGER reject_fixture_trial_result BEFORE INSERT ON events
             WHEN NEW.event_id = 'result:{first_run_id}'
             BEGIN SELECT RAISE(ABORT, 'fixture trial result rejection'); END;"
        ))
        .expect("reject the first trial result append");
    assert!(matches!(
        plane
            .submit_arena_job(job_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit real Arena worker job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));

    let deadline = Instant::now() + Duration::from_secs(30);
    let message = wait_for_test_arena_trial(&mut plane, deadline);
    let ArenaWorkerMessage::Trial {
        job_id: trial_job_id,
        index,
        output,
        reply,
    } = message
    else {
        unreachable!("helper returns only trial messages");
    };
    assert_eq!(trial_job_id, job_id);
    assert_eq!(index, 0);
    assert!(output.is_ok(), "real reference worker must return a Trial");
    plane
        .arena_message_sender
        .as_ref()
        .expect("Arena message sender")
        .send(ArenaWorkerMessage::Trial {
            job_id: trial_job_id,
            index,
            output,
            reply,
        })
        .expect("return trial to canonical writer");
    plane
        .service_arena_message()
        .expect("persist trial failure acknowledgement");
    assert!(
        plane
            .active_arena_job
            .as_ref()
            .expect("failed acknowledgement retains active job until final message")
            .cancel
            .load(Ordering::Acquire),
        "failed canonical trial append cancels the worker"
    );

    while plane.active_arena_job.is_some() {
        service_test_arena_evidence(&mut plane);
        plane
            .service_arena_message()
            .expect("drain worker's final Trials failure");
        assert!(Instant::now() < deadline, "worker failure was not drained");
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let terminal = plane.state.arena_jobs.get(job_id).expect("failed job");
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert!(terminal.evaluation.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify failed worker handshake history");
    assert!(
        !history
            .iter()
            .any(|event| event.event_id == format!("result:{first_run_id}"))
    );
    assert!(history.iter().any(|event| {
        event.event_id == format!("arena-job:{job_id}:terminal")
            && event.event_type == "arena.job.terminal"
    }));
    assert!(!history.iter().any(|event| {
        event.event_id == format!("arena:evaluation:{job_id}:recorded")
            && event.event_type == "evaluation.recorded"
    }));
    drop(database);
}

#[test]
#[allow(clippy::too_many_lines)]
fn arena_trial_acknowledgement_disconnect_fails_and_replays_without_next_trial() {
    let directory = tempdir().expect("Arena fixture directory");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let job_id = "trial-ack-disconnected";
    assert!(matches!(
        plane
            .submit_arena_job(job_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit real Arena worker job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));

    let deadline = Instant::now() + Duration::from_secs(30);
    let message = wait_for_test_arena_trial(&mut plane, deadline);
    let ArenaWorkerMessage::Trial {
        job_id: trial_job_id,
        index,
        output,
        reply,
    } = message
    else {
        unreachable!("helper returns only trial messages");
    };
    assert_eq!(trial_job_id, job_id);
    assert_eq!(index, 0);
    assert!(output.is_ok(), "real reference worker must return a Trial");
    drop(reply);

    let final_message = loop {
        service_test_arena_evidence(&mut plane);
        match plane
            .arena_message_receiver
            .as_ref()
            .expect("Arena worker channel")
            .try_recv()
        {
            Ok(message @ ArenaWorkerMessage::Trials { .. }) => break message,
            Ok(ArenaWorkerMessage::Trial { index, .. }) => {
                panic!("worker returned unexpected second trial at index {index}");
            }
            Ok(ArenaWorkerMessage::Scoring { .. }) => {
                panic!("worker scored after losing its trial acknowledgement");
            }
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "worker failure was not drained");
                thread::sleep(Duration::from_millis(2));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                panic!("Arena worker disconnected before its final failure message");
            }
        }
    };
    let ArenaWorkerMessage::Trials {
        job_id: final_job_id,
        result,
    } = &final_message
    else {
        unreachable!("loop accepts only final Trials messages");
    };
    assert_eq!(final_job_id, job_id);
    assert!(
        result.is_err(),
        "lost acknowledgement must fail worker execution"
    );
    plane
        .arena_message_sender
        .as_ref()
        .expect("Arena message sender")
        .send(final_message)
        .expect("return actual worker failure to canonical writer");
    plane
        .service_arena_message()
        .expect("persist failed worker terminal");

    let terminal = plane.state.arena_jobs.get(job_id).expect("failed job");
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert_eq!(terminal.completed_trials, 0);
    assert!(terminal.evaluation.is_none());
    assert!(plane.active_arena_job.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify failed worker handshake history");
    let first_run_id = paired_run_id(job_id, "parent", 0);
    assert!(!history.iter().any(|event| {
        event.event_id == format!("result:{first_run_id}")
            || event.event_type == "evaluation.recorded"
    }));
    assert_eq!(
        history
            .iter()
            .filter(|event| {
                event.event_id == format!("arena-job:{job_id}:terminal")
                    && event.event_type == "arena.job.terminal"
            })
            .count(),
        1,
        "one durable terminal record"
    );
    assert!(matches!(
        plane.replay_response().expect("replay failed job history"),
        ResponseData::Replay { .. }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn injected_arena_scoring_failure_persists_failed_terminal_and_replays() {
    let directory = tempdir().expect("fixture directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    let git = ProcessCommand::new("git")
        .args([
            "-C",
            repository.to_str().expect("repository path"),
            "init",
            "-q",
        ])
        .status()
        .expect("run git init");
    assert!(git.success(), "initialize source repository");
    fs::write(repository.join("fixture.txt"), b"Arena scoring fixture\n")
        .expect("write repository fixture");
    for args in [
        vec![
            "-C",
            repository.to_str().expect("repository path"),
            "config",
            "user.name",
            "Hephaestus Test",
        ],
        vec![
            "-C",
            repository.to_str().expect("repository path"),
            "config",
            "user.email",
            "hephaestus@example.invalid",
        ],
        vec![
            "-C",
            repository.to_str().expect("repository path"),
            "add",
            ".",
        ],
        vec![
            "-C",
            repository.to_str().expect("repository path"),
            "commit",
            "-m",
            "fixture",
            "-q",
        ],
    ] {
        assert!(
            ProcessCommand::new("git")
                .args(args)
                .status()
                .expect("run git fixture command")
                .success(),
            "prepare git fixture"
        );
    }
    let bin_directory = env::current_exe()
        .expect("locate test executable")
        .parent()
        .and_then(Path::parent)
        .expect("locate Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        cargo_evaluator.is_file(),
        "Cargo evaluator binary missing: {cargo_evaluator:?}"
    );
    let evaluator = directory.path().join("fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into a private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("mark fixture evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(worker.is_file(), "Cargo worker binary missing: {worker:?}");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open control plane");
    let token = plane.token_hex.clone();
    let (world, parent, candidate) =
        register_dispatch_arena_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-arena", Command::Unfreeze)
            .error
            .is_none()
    );
    let registered_world = plane
        .registered_world(&world.world_id)
        .expect("registered Arena World");
    let evaluator_id = registered_world
        .evaluator_artifact("arena.evaluator")
        .expect("World evaluator artifact");
    plane
        .open_evaluator(
            evaluator_id,
            WorkerLimits::new(
                Duration::from_millis(PAIRED_EVALUATION_WALL_MILLIS),
                16 * 1024 * 1024,
                128 * 1024,
            )
            .expect("evaluator limits"),
        )
        .expect("preflight evaluator identity and sandbox");
    plane
        .pin_reference_worker()
        .expect("preflight reference worker snapshot");
    plane
        .paired_revision("admission-preflight")
        .expect("preflight pinned Git revision");
    assert!(matches!(
        plane
            .submit_arena_job("channel-drop", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit channel-drop job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let (trial_reply, trial_response) = mpsc::channel();
    plane
        .arena_message_sender
        .as_ref()
        .expect("Arena worker channel")
        .send(ArenaWorkerMessage::Trial {
            job_id: "channel-drop".to_owned(),
            index: 1,
            output: Err("out of order fixture trial".to_owned()),
            reply: trial_reply,
        })
        .expect("queue out-of-order trial");
    plane
        .service_arena_message()
        .expect("reject out-of-order Arena trial");
    assert_eq!(
        trial_response
            .recv_timeout(Duration::from_secs(1))
            .expect("worker receives order rejection")
            .expect_err("out-of-order trial must fail closed"),
        "paired trial arrived outside admitted order"
    );
    plane
        .active_arena_job
        .as_ref()
        .expect("active channel-drop job")
        .cancel
        .store(true, Ordering::Release);
    let (closed_sender, closed_receiver) = mpsc::sync_channel(1);
    drop(closed_sender);
    plane.arena_message_receiver = Some(closed_receiver);
    let disconnect_database = rusqlite::Connection::open(data_dir.join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    disconnect_database
        .execute_batch(
            "CREATE TRIGGER reject_channel_drop_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:channel-drop:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture disconnect terminal failure'); END;",
        )
        .expect("reject disconnect terminal append");
    assert!(matches!(
        plane.service_arena_message(),
        Err(ControlError::Projection(message))
            if message == "interrupted Arena job could not be recorded"
    ));
    assert_eq!(
        plane.state.arena_jobs["channel-drop"].state,
        JobState::Running
    );
    assert!(plane.active_arena_job.is_some());
    assert!(plane.arena_message_receiver.is_some());
    assert!(
        !plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify history after rejected disconnect terminal")
            .iter()
            .any(|event| event.event_id == "arena-job:channel-drop:terminal")
    );

    disconnect_database
        .execute_batch("DROP TRIGGER reject_channel_drop_terminal;")
        .expect("restore fixture terminal writes");
    plane
        .service_arena_message()
        .expect("retry unexpected worker disconnect terminal");
    assert_eq!(
        plane.state.arena_jobs["channel-drop"].terminal,
        Some(JobTerminal::Interrupted)
    );
    assert!(plane.active_arena_job.is_none());
    assert!(plane.arena_message_receiver.is_none());
    assert_eq!(
        plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify recovered disconnect terminal")
            .iter()
            .filter(|event| event.event_id == "arena-job:channel-drop:terminal")
            .count(),
        1
    );
    drop(disconnect_database);

    let trials_error_id = "trials-error-terminal-write-failure";
    assert!(matches!(
        plane
            .submit_arena_job(trials_error_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit Trials Err terminal fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    plane
        .kill_job(trials_error_id)
        .expect("persist cancellation before Trials Err");
    let (injected_sender, injected_receiver) = mpsc::sync_channel(1);
    plane.arena_message_receiver = Some(injected_receiver);
    injected_sender
        .send(ArenaWorkerMessage::Trials {
            job_id: trials_error_id.to_owned(),
            result: Err("fixture paired trials failure".to_owned()),
        })
        .expect("queue Trials Err for admitted job");
    let trials_error_database = rusqlite::Connection::open(data_dir.join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    trials_error_database
        .execute_batch(
            "CREATE TRIGGER reject_trials_error_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:trials-error-terminal-write-failure:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture Trials Err terminal failure'); END;",
        )
        .expect("reject Trials Err terminal append");
    assert!(matches!(
        plane.service_arena_message(),
        Err(ControlError::Projection(message))
            if message == "Arena terminal state could not be recorded"
    ));
    assert_eq!(
        plane.state.arena_jobs[trials_error_id].state,
        JobState::CancellationRequested
    );
    assert!(plane.active_arena_job.is_some());
    assert!(
        !plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify history after rejected Trials Err terminal")
            .iter()
            .any(|event| event.event_id == format!("arena-job:{trials_error_id}:terminal"))
    );

    trials_error_database
        .execute_batch("DROP TRIGGER reject_trials_error_terminal;")
        .expect("restore fixture terminal writes");
    drop(injected_sender);
    plane
        .service_arena_message()
        .expect("persist interrupted terminal after failed Trials Err append");
    assert_eq!(
        plane.state.arena_jobs[trials_error_id].terminal,
        Some(JobTerminal::Interrupted)
    );
    assert!(plane.active_arena_job.is_none());
    assert_eq!(
        plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify recovered Trials Err terminal")
            .iter()
            .filter(|event| event.event_id == format!("arena-job:{trials_error_id}:terminal"))
            .count(),
        1
    );
    drop(trials_error_database);

    let cross_run_id = "arena-cross-run-evidence";
    assert!(matches!(
        plane
            .submit_arena_job(cross_run_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit cross-run Arena evidence fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let cross_run_cancel = Arc::clone(
        &plane
            .active_arena_job
            .as_ref()
            .expect("active cross-run Arena job")
            .cancel,
    );
    let (cross_run_sender, cross_run_receiver) = mpsc::sync_channel(1);
    let (cross_run_reply, cross_run_response) = mpsc::channel();
    cross_run_sender
        .send(EvidenceRequest::EnsureCapacity {
            run_id: "foreign-arena-trial".to_owned(),
            needed: 2,
            reply: cross_run_reply,
        })
        .expect("queue evidence from a non-admitted run");
    plane.job_evidence_receiver = Some(cross_run_receiver);
    plane
        .service_async_messages()
        .expect("reject cross-run Arena evidence");
    assert!(
        cross_run_response
            .recv_timeout(Duration::from_secs(1))
            .expect("writer returns evidence rejection")
            .is_err()
    );
    assert!(cross_run_cancel.load(Ordering::Acquire));
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist failed terminal after cross-run Arena evidence");
        assert!(
            Instant::now() < deadline,
            "cross-run Arena job did not unwind"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let cross_run_terminal = &plane.state.arena_jobs[cross_run_id];
    assert_eq!(cross_run_terminal.state, JobState::Failed);
    assert_eq!(cross_run_terminal.terminal, Some(JobTerminal::Failed));
    assert!(cross_run_terminal.evaluation.is_none());
    assert_eq!(
        plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify cross-run Arena failure")
            .iter()
            .filter(|event| event.event_id == format!("arena-job:{cross_run_id}:terminal"))
            .count(),
        1
    );

    assert!(matches!(
        plane
            .submit_arena_job("scoring-failure", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit Arena job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let cancel = Arc::clone(
        &plane
            .active_arena_job
            .as_ref()
            .expect("active Arena job")
            .cancel,
    );
    plane
        .arena_message_sender
        .as_ref()
        .expect("Arena worker channel")
        .send(ArenaWorkerMessage::Trials {
            job_id: "other-pair".to_owned(),
            result: Ok(()),
        })
        .expect("queue mismatched worker completion");
    assert!(plane.service_arena_message().is_err());

    // Exercise the scorer completion boundary with a genuine admitted job;
    // the injected failure represents the worker's `Scoring::Err` message.
    plane
        .finish_arena_scoring("scoring-failure", Err("fixture scoring failure".to_owned()))
        .expect("record scorer failure as a terminal state");
    cancel.store(true, Ordering::Release);
    let failed = plane
        .state
        .arena_jobs
        .get("scoring-failure")
        .expect("failed Arena job remains projected");
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.terminal, Some(JobTerminal::Failed));
    assert!(failed.evaluation.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify failed-job history");
    assert!(history.iter().any(|event| {
        event.event_id == "arena-job:scoring-failure:terminal"
            && event.event_type == "arena.job.terminal"
    }));
    assert!(!history.iter().any(|event| {
        event.event_id == "arena:evaluation:scoring-failure:recorded"
            && event.event_type == "evaluation.recorded"
    }));
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay failed Arena terminal"),
        ResponseData::Replay { .. }
    ));
    assert_eq!(world.world_id, failed.world_id);

    assert!(matches!(
        plane
            .submit_arena_job("scorer-launch-failure", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit scorer launch failure job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane
        .active_arena_job
        .as_ref()
        .is_some_and(|active| active.record.completed_trials != active.record.total_trials)
    {
        plane
            .service_async_messages()
            .expect("persist signed trials before scorer launch");
        assert!(Instant::now() < deadline, "Arena trials did not finish");
        if plane
            .active_arena_job
            .as_ref()
            .is_some_and(|active| active.record.completed_trials != active.record.total_trials)
        {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify signed trial history");
    let active = plane.active_arena_job.as_ref().expect("active Arena job");
    assert_eq!(active.record.completed_trials, active.record.total_trials);
    assert_eq!(active.trials.len(), active.record.total_trials as usize);
    for trial in &active.trials {
        let event_id = format!("result:{}", trial.spec.run_id());
        assert!(history.iter().any(|event| {
            event.event_id == event_id && event.event_type == "run.result_recorded"
        }));
    }

    let scorer_terminal_database = rusqlite::Connection::open(data_dir.join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    scorer_terminal_database
        .execute_batch(
            "CREATE TRIGGER reject_scorer_launch_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:scorer-launch-failure:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture scorer launch terminal failure'); END;",
        )
        .expect("reject scorer launch terminal append");
    plane.thread_spawn_failures.arena_scoring = true;
    let scorer_failure_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match plane.service_async_messages() {
            Err(ControlError::Projection(message)) => {
                assert_eq!(message, "Arena scorer launch failure could not be recorded");
                break;
            }
            Err(error) => panic!("unexpected scorer launch service error: {error}"),
            Ok(()) => {
                assert!(
                    Instant::now() < scorer_failure_deadline,
                    "injected scorer launch failure did not reach terminal append"
                );
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
    let active = plane
        .active_arena_job
        .as_ref()
        .expect("active job retained after rejected scorer terminal");
    assert_eq!(active.record.state, JobState::Running);
    assert_eq!(active.record.phase, ArenaJobPhase::Scoring);
    assert_eq!(active.record.completed_trials, active.record.total_trials);
    assert_eq!(
        plane.state.arena_jobs["scorer-launch-failure"].phase,
        ArenaJobPhase::Scoring
    );
    assert!(
        plane.state.arena_jobs["scorer-launch-failure"]
            .evaluation
            .is_none()
    );
    assert!(plane.arena_message_receiver.is_some());
    assert!(plane.arena_message_sender.is_some());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify scorer launch failure history after rejected terminal");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == "arena-job:scorer-launch-failure:scoring")
            .count(),
        1
    );
    assert!(!history.iter().any(|event| {
        event.event_id == "arena-job:scorer-launch-failure:terminal"
            || event.event_id == "arena:evaluation:scorer-launch-failure:recorded"
    }));

    scorer_terminal_database
        .execute_batch("DROP TRIGGER reject_scorer_launch_terminal;")
        .expect("restore scorer launch terminal writes");
    drop(scorer_terminal_database);
    drop(plane);
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("recover scorer launch failure after rejected terminal append");
    let launch_failed = &plane.state.arena_jobs["scorer-launch-failure"];
    assert_eq!(launch_failed.state, JobState::Interrupted);
    assert_eq!(launch_failed.phase, ArenaJobPhase::Terminal);
    assert_eq!(launch_failed.terminal, Some(JobTerminal::Interrupted));
    assert!(launch_failed.evaluation.is_none());
    assert!(plane.active_arena_job.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("reopened canonical storage")
        .ledger
        .replay_verified()
        .expect("verify recovered scorer launch failure history");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == "arena-job:scorer-launch-failure:terminal")
            .count(),
        1
    );
    assert!(!history.iter().any(|event| {
        event.event_id == "arena:evaluation:scorer-launch-failure:recorded"
            && event.event_type == "evaluation.recorded"
    }));
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay recovered scorer launch terminal"),
        ResponseData::Replay { .. }
    ));

    let success_id = "scorer-launch-success";
    assert!(matches!(
        plane
            .submit_arena_job(success_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit successful scorer launch failure fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let success_deadline = Instant::now() + Duration::from_secs(20);
    while plane
        .active_arena_job
        .as_ref()
        .is_some_and(|active| active.record.completed_trials != active.record.total_trials)
    {
        plane
            .service_async_messages()
            .expect("persist signed trials before successful scorer launch failure");
        assert!(
            Instant::now() < success_deadline,
            "Arena trials did not finish for successful scorer launch failure"
        );
        if plane
            .active_arena_job
            .as_ref()
            .is_some_and(|active| active.record.completed_trials != active.record.total_trials)
        {
            thread::sleep(Duration::from_millis(2));
        }
    }
    plane.thread_spawn_failures.arena_scoring = true;
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("append interrupted terminal after injected scorer launch failure");
        assert!(
            Instant::now() < success_deadline,
            "successful scorer launch failure did not release its active slot"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let launch_succeeded = &plane.state.arena_jobs[success_id];
    assert_eq!(launch_succeeded.state, JobState::Interrupted);
    assert_eq!(launch_succeeded.phase, ArenaJobPhase::Terminal);
    assert_eq!(launch_succeeded.terminal, Some(JobTerminal::Interrupted));
    assert!(launch_succeeded.evaluation.is_none());
    assert!(plane.active_arena_job.is_none());
    assert!(plane.arena_message_receiver.is_none());
    assert!(plane.arena_message_sender.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify successful scorer launch terminal history");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == format!("arena-job:{success_id}:terminal"))
            .count(),
        1
    );
    assert!(!history.iter().any(|event| {
        event.event_id == format!("arena:evaluation:{success_id}:recorded")
            && event.event_type == "evaluation.recorded"
    }));
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay successful scorer launch interruption"),
        ResponseData::Replay { .. }
    ));

    assert!(matches!(
        plane
            .submit_arena_job("scorer-launch-retry", &parent.genome_id, &candidate.genome_id, false)
            .expect("retry Arena admission"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    plane
        .kill_job("scorer-launch-retry")
        .expect("cancel retry fixture");
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist retry cancellation");
        assert!(
            Instant::now() < deadline,
            "retry cancellation did not finish"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }

    assert!(matches!(
        plane
            .submit_arena_job("cancelled-scoring", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit cancellation job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    plane
        .kill_job("cancelled-scoring")
        .expect("request Arena cancellation");
    plane
        .finish_arena_scoring(
            "cancelled-scoring",
            Err("scorer completed after cancellation".to_owned()),
        )
        .expect("cancellation wins over late scorer failure");
    let cancelled = &plane.state.arena_jobs["cancelled-scoring"];
    assert_eq!(cancelled.state, JobState::Interrupted);
    assert_eq!(cancelled.terminal, Some(JobTerminal::Cancelled));
    assert!(cancelled.evaluation.is_none());
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay cancelled Arena terminal"),
        ResponseData::Replay { .. }
    ));

    assert!(matches!(
        plane
            .submit_arena_job("scoring-timeout", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit scoring-timeout job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    plane
        .active_arena_job
        .as_mut()
        .expect("active scoring-timeout job")
        .overall_deadline = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("monotonic clock supports one millisecond lookback");
    plane
        .finish_arena_scoring(
            "scoring-timeout",
            Err("scorer completed after the overall deadline".to_owned()),
        )
        .expect("persist deadline terminal after late scorer result");
    let timed_out = &plane.state.arena_jobs["scoring-timeout"];
    assert_eq!(timed_out.state, JobState::Failed);
    assert_eq!(timed_out.terminal, Some(JobTerminal::Failed));
    assert!(timed_out.evaluation.is_none());
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay timed-out Arena terminal"),
        ResponseData::Replay { .. }
    ));

    let deadline_replay_id = "deadline-terminal-replay";
    assert!(matches!(
        plane
            .submit_arena_job(deadline_replay_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit Arena deadline replay fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    plane
        .active_arena_job
        .as_mut()
        .expect("active Arena deadline replay fixture")
        .overall_deadline = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("monotonic clock supports one millisecond lookback");
    let deadline_database = rusqlite::Connection::open(data_dir.join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    deadline_database
        .execute_batch(
            "CREATE TRIGGER reject_deadline_cancellation BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:deadline-terminal-replay:cancellation_requested'
             BEGIN SELECT RAISE(ABORT, 'fixture deadline append failure'); END;",
        )
        .expect("reject deadline cancellation append");
    assert!(matches!(
        plane.enforce_arena_deadline(),
        Err(ControlError::Projection(message))
            if message == "Arena deadline could not be persisted"
    ));
    assert_eq!(
        plane.state.arena_jobs[deadline_replay_id].state,
        JobState::Running
    );
    let active = plane
        .active_arena_job
        .as_ref()
        .expect("deadline remains active after rejected append");
    assert!(
        !active.overall_timed_out,
        "failed persistence remains retryable"
    );
    assert!(active.cancel.load(Ordering::Acquire));
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after rejected deadline append");
    assert!(!history.iter().any(|event| {
        event.event_id == format!("arena-job:{deadline_replay_id}:cancellation_requested")
            || event.event_id == format!("arena-job:{deadline_replay_id}:terminal")
    }));
    deadline_database
        .execute_batch("DROP TRIGGER reject_deadline_cancellation;")
        .expect("restore fixture deadline writes");
    plane
        .enforce_arena_deadline()
        .expect("retry and persist durable deadline cancellation request");
    assert_eq!(
        plane.state.arena_jobs[deadline_replay_id].state,
        JobState::CancellationRequested
    );
    assert!(
        plane
            .active_arena_job
            .as_ref()
            .expect("active timed-out Arena job")
            .overall_timed_out
    );
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify durable deadline request");
    assert_eq!(
        history
            .iter()
            .filter(|event| {
                event.event_id == format!("arena-job:{deadline_replay_id}:cancellation_requested")
            })
            .count(),
        1
    );
    assert!(
        !history
            .iter()
            .any(|event| event.event_id == format!("arena-job:{deadline_replay_id}:terminal"))
    );
    drop(deadline_database);
    drop(plane);
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("recover timed-out Arena job after restart");
    let recovered_deadline = &plane.state.arena_jobs[deadline_replay_id];
    assert_eq!(recovered_deadline.state, JobState::Interrupted);
    assert_eq!(recovered_deadline.terminal, Some(JobTerminal::Cancelled));
    assert!(recovered_deadline.evaluation.is_none());
    assert!(plane.active_arena_job.is_none());
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify recovered deadline terminal");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == format!("arena-job:{deadline_replay_id}:terminal"))
            .count(),
        1
    );
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay recovered Arena deadline terminal"),
        ResponseData::Replay { .. }
    ));

    assert!(matches!(
        plane
            .submit_arena_job("scoring-success", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit successful Arena job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    assert!(plane.start_arena_scoring().is_err());
    assert!(
        plane
            .finish_arena_scoring("other-pair", Err("wrong identity".to_owned()))
            .is_err()
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist Arena trials and scoring result");
        assert!(
            Instant::now() < deadline,
            "successful Arena scoring did not complete"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let succeeded = &plane.state.arena_jobs["scoring-success"];
    assert_eq!(succeeded.state, JobState::Succeeded);
    assert_eq!(succeeded.terminal, Some(JobTerminal::Succeeded));
    assert_eq!(succeeded.completed_trials, succeeded.total_trials);
    assert!(succeeded.evaluation.is_some());
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay successful Arena commit"),
        ResponseData::Replay { .. }
    ));

    assert!(matches!(
        plane
            .submit_arena_job("scoring-commit-failure", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit scoring-commit-failure job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane
        .active_arena_job
        .as_ref()
        .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
    {
        plane
            .service_async_messages()
            .expect("persist trials before scorer completion");
        assert!(
            Instant::now() < deadline,
            "Arena trials did not reach scoring"
        );
        if plane
            .active_arena_job
            .as_ref()
            .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
        {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let scored = loop {
        let message = plane
            .arena_message_receiver
            .as_ref()
            .expect("Arena scorer channel")
            .try_recv();
        match message {
            Ok(ArenaWorkerMessage::Scoring { job_id, result }) => break (job_id, result),
            Ok(_) => panic!("unexpected message after scoring began"),
            Err(mpsc::TryRecvError::Disconnected) => panic!("Arena scorer disconnected"),
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "Arena scoring did not finish");
                thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert_eq!(scored.0, "scoring-commit-failure");
    assert!(
        scored.1.is_ok(),
        "fixture evaluator must produce valid scores"
    );
    let blobs = plane.data_dir.join("blobs");
    let saved_blobs = plane.data_dir.join("blobs-before-commit-failure");
    fs::rename(&blobs, &saved_blobs)
        .expect("temporarily hide fixture blobs to force receipt commit failure");
    plane
        .finish_arena_scoring(&scored.0, scored.1)
        .expect("persist failed commit terminal after reopening storage");
    fs::remove_dir_all(&blobs).expect("remove reopened empty blob directory");
    fs::rename(saved_blobs, blobs).expect("restore fixture blobs for verified replay");
    let commit_failed = &plane.state.arena_jobs["scoring-commit-failure"];
    assert_eq!(commit_failed.state, JobState::Failed);
    assert_eq!(commit_failed.terminal, Some(JobTerminal::Failed));
    assert!(commit_failed.evaluation.is_none());
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay failed Arena receipt commit"),
        ResponseData::Replay { .. }
    ));

    assert!(matches!(
        plane
            .submit_arena_job("terminal-write-failure", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit terminal-write-failure job"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let cancel = Arc::clone(
        &plane
            .active_arena_job
            .as_ref()
            .expect("active terminal-write-failure job")
            .cancel,
    );
    let database = rusqlite::Connection::open(data_dir.join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    database
        .execute_batch(
            "CREATE TRIGGER reject_fixture_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:terminal-write-failure:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture terminal write failure'); END;",
        )
        .expect("reject fixture terminal append");
    assert!(
        plane
            .finish_arena_scoring(
                "terminal-write-failure",
                Err("fixture scoring failure".to_owned()),
            )
            .is_err(),
        "a rejected terminal append must not be acknowledged"
    );
    assert_eq!(
        plane.state.arena_jobs["terminal-write-failure"].state,
        JobState::Running
    );
    assert!(
        !plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify history after failed append")
            .iter()
            .any(|event| event.event_id == "arena-job:terminal-write-failure:terminal"),
        "the failed append must leave no canonical terminal event"
    );
    database
        .execute_batch("DROP TRIGGER reject_fixture_terminal;")
        .expect("restore fixture ledger writes");
    plane
        .finish_arena_scoring(
            "terminal-write-failure",
            Err("fixture scoring failure".to_owned()),
        )
        .expect("persist failed terminal after storage recovers");
    cancel.store(true, Ordering::Release);
    let terminal = &plane.state.arena_jobs["terminal-write-failure"];
    assert_eq!(terminal.state, JobState::Failed);
    assert_eq!(terminal.terminal, Some(JobTerminal::Failed));
    assert!(terminal.evaluation.is_none());
    assert!(matches!(
        plane.replay_response().expect("replay recovered terminal"),
        ResponseData::Replay { .. }
    ));

    assert!(matches!(
        plane
            .submit_arena_job("cancel-terminal-write-failure", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit cancellation write fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    plane
        .kill_job("cancel-terminal-write-failure")
        .expect("persist cancellation request");
    database
        .execute_batch(
            "CREATE TRIGGER reject_fixture_cancel_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:cancel-terminal-write-failure:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture cancellation terminal write failure'); END;",
        )
        .expect("reject cancellation terminal append");
    assert!(
        plane
            .finish_arena_scoring(
                "cancel-terminal-write-failure",
                Err("late scorer result".to_owned()),
            )
            .is_err()
    );
    assert_eq!(
        plane.state.arena_jobs["cancel-terminal-write-failure"].state,
        JobState::CancellationRequested
    );
    database
        .execute_batch("DROP TRIGGER reject_fixture_cancel_terminal;")
        .expect("restore cancellation terminal writes");
    plane
        .finish_arena_scoring(
            "cancel-terminal-write-failure",
            Err("late scorer result".to_owned()),
        )
        .expect("persist cancelled terminal after storage recovers");
    assert_eq!(
        plane.state.arena_jobs["cancel-terminal-write-failure"].terminal,
        Some(JobTerminal::Cancelled)
    );

    assert!(matches!(
        plane
            .submit_arena_job("timeout-terminal-write-failure", &parent.genome_id, &candidate.genome_id, false)
            .expect("admit timeout write fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    plane
        .active_arena_job
        .as_mut()
        .expect("active timeout write fixture")
        .overall_deadline = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("monotonic clock supports one millisecond lookback");
    database
        .execute_batch(
            "CREATE TRIGGER reject_fixture_timeout_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:timeout-terminal-write-failure:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture timeout terminal write failure'); END;",
        )
        .expect("reject timeout terminal append");
    assert!(
        plane
            .finish_arena_scoring(
                "timeout-terminal-write-failure",
                Err("late scorer result".to_owned()),
            )
            .is_err()
    );
    assert_eq!(
        plane.state.arena_jobs["timeout-terminal-write-failure"].state,
        JobState::CancellationRequested
    );
    database
        .execute_batch("DROP TRIGGER reject_fixture_timeout_terminal;")
        .expect("restore timeout terminal writes");
    plane
        .finish_arena_scoring(
            "timeout-terminal-write-failure",
            Err("late scorer result".to_owned()),
        )
        .expect("persist timed-out terminal after storage recovers");
    assert_eq!(
        plane.state.arena_jobs["timeout-terminal-write-failure"].terminal,
        Some(JobTerminal::Failed)
    );
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay recovered Arena write failures"),
        ResponseData::Replay { .. }
    ));

    let failed_commit_id = "failed-commit-terminal-write-failure";
    assert!(matches!(
        plane
            .submit_arena_job(failed_commit_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit failed commit terminal write fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane
        .active_arena_job
        .as_ref()
        .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
    {
        plane
            .service_async_messages()
            .expect("persist signed trials before failed receipt commit");
        assert!(
            Instant::now() < deadline,
            "Arena trials did not reach scoring"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let scored = loop {
        match plane
            .arena_message_receiver
            .as_ref()
            .expect("Arena scorer channel")
            .try_recv()
        {
            Ok(ArenaWorkerMessage::Scoring { job_id, result }) => break (job_id, result),
            Ok(_) => panic!("unexpected message after scoring began"),
            Err(mpsc::TryRecvError::Disconnected) => panic!("Arena scorer disconnected"),
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "Arena scoring did not finish");
                thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert_eq!(scored.0, failed_commit_id);
    assert!(
        scored.1.is_ok(),
        "fixture evaluator must produce valid scores"
    );
    let blobs = plane.data_dir.join("blobs");
    let saved_blobs = plane.data_dir.join("blobs-before-failed-terminal-write");
    fs::rename(&blobs, &saved_blobs)
        .expect("temporarily hide fixture blobs to force receipt commit failure");
    database
        .execute_batch(
            "CREATE TRIGGER reject_fixture_failed_commit_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:failed-commit-terminal-write-failure:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture failed commit terminal write failure'); END;",
        )
        .expect("reject failed commit terminal append");
    assert!(matches!(
        plane.finish_arena_scoring(&scored.0, scored.1),
        Err(ControlError::Projection(message))
            if message == "failed Arena terminal state could not be recorded"
    ));
    fs::remove_dir_all(&blobs).expect("remove reopened empty blob directory");
    fs::rename(saved_blobs, blobs).expect("restore canonical artifacts");
    database
        .execute_batch("DROP TRIGGER reject_fixture_failed_commit_terminal;")
        .expect("restore failed commit terminal writes");
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after failed receipt and terminal write");
    assert!(!history.iter().any(|event| {
        event.event_id == format!("arena:evaluation:{failed_commit_id}:recorded")
            || event.event_id == format!("arena-job:{failed_commit_id}:terminal")
    }));
    drop(plane);
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("restart after failed receipt and terminal write");
    let interrupted = &plane.state.arena_jobs[failed_commit_id];
    assert_eq!(interrupted.state, JobState::Interrupted);
    assert_eq!(interrupted.terminal, Some(JobTerminal::Interrupted));
    assert!(interrupted.evaluation.is_none());

    let phase_id = "scoring-phase-write-failure";
    assert!(matches!(
        plane
            .submit_arena_job(phase_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit scoring-phase write fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane
        .active_arena_job
        .as_ref()
        .is_some_and(|active| active.record.completed_trials < active.record.total_trials)
    {
        plane
            .service_async_messages()
            .expect("persist real signed Arena trials");
        assert!(Instant::now() < deadline, "Arena trials did not complete");
        thread::sleep(Duration::from_millis(2));
    }
    database
        .execute_batch(
            "CREATE TRIGGER reject_fixture_scoring_phase BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:scoring-phase-write-failure:scoring'
             BEGIN SELECT RAISE(ABORT, 'fixture scoring phase write failure'); END;",
        )
        .expect("reject scoring phase append");
    loop {
        match plane.service_arena_message() {
            Err(ControlError::Projection(message)) => {
                assert_eq!(message, "Arena scoring phase could not be recorded");
                break;
            }
            Ok(()) => {
                assert!(
                    Instant::now() < deadline,
                    "Arena completion was not delivered"
                );
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("unexpected Arena scoring failure: {error}"),
        }
    }
    let before_restart = &plane.state.arena_jobs[phase_id];
    assert_eq!(before_restart.completed_trials, before_restart.total_trials);
    assert_ne!(before_restart.phase, ArenaJobPhase::Scoring);
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after scoring phase rejection");
    assert!(!history.iter().any(|event| {
        event.event_id == format!("arena-job:{phase_id}:scoring")
            || event.event_id == format!("arena:evaluation:{phase_id}:recorded")
    }));
    database
        .execute_batch("DROP TRIGGER reject_fixture_scoring_phase;")
        .expect("restore scoring phase writes");
    drop(plane);
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("restart after scoring phase write failure");
    let interrupted = &plane.state.arena_jobs[phase_id];
    assert_eq!(interrupted.state, JobState::Interrupted);
    assert_eq!(interrupted.terminal, Some(JobTerminal::Interrupted));
    assert!(interrupted.evaluation.is_none());

    let committing_id = "committing-phase-write-failure";
    assert!(matches!(
        plane
            .submit_arena_job(committing_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit committing-phase write fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane
        .active_arena_job
        .as_ref()
        .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
    {
        plane
            .service_async_messages()
            .expect("persist signed trials before committing phase");
        assert!(
            Instant::now() < deadline,
            "Arena trials did not reach scoring"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let scored = loop {
        match plane
            .arena_message_receiver
            .as_ref()
            .expect("Arena scorer channel")
            .try_recv()
        {
            Ok(ArenaWorkerMessage::Scoring { job_id, result }) => break (job_id, result),
            Ok(_) => panic!("unexpected message after scoring began"),
            Err(mpsc::TryRecvError::Disconnected) => panic!("Arena scorer disconnected"),
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "Arena scoring did not finish");
                thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert_eq!(scored.0, committing_id);
    assert!(
        scored.1.is_ok(),
        "fixture evaluator must produce valid scores"
    );
    database
        .execute_batch(
            "CREATE TRIGGER reject_fixture_committing_phase BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:committing-phase-write-failure:committing'
             BEGIN SELECT RAISE(ABORT, 'fixture committing phase write failure'); END;",
        )
        .expect("reject committing phase append");
    assert!(matches!(
        plane.finish_arena_scoring(&scored.0, scored.1),
        Err(ControlError::Projection(message))
            if message == "Arena commit phase could not be recorded"
    ));
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after committing phase rejection");
    assert!(!history.iter().any(|event| {
        event.event_id == format!("arena-job:{committing_id}:committing")
            || event.event_id == format!("arena:evaluation:{committing_id}:recorded")
    }));
    database
        .execute_batch("DROP TRIGGER reject_fixture_committing_phase;")
        .expect("restore committing phase writes");
    drop(plane);
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("restart after committing phase write failure");
    let interrupted = &plane.state.arena_jobs[committing_id];
    assert_eq!(interrupted.state, JobState::Interrupted);
    assert_eq!(interrupted.terminal, Some(JobTerminal::Interrupted));
    assert!(interrupted.evaluation.is_none());

    let recovery_id = "scored-terminal-write-failure";
    assert!(matches!(
        plane
            .submit_arena_job(recovery_id, &parent.genome_id, &candidate.genome_id, false)
            .expect("admit scored terminal write fixture"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(20);
    while plane
        .active_arena_job
        .as_ref()
        .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
    {
        plane
            .service_async_messages()
            .expect("persist signed trials before scoring");
        assert!(
            Instant::now() < deadline,
            "Arena trials did not reach scoring"
        );
        if plane
            .active_arena_job
            .as_ref()
            .is_some_and(|active| active.record.phase != ArenaJobPhase::Scoring)
        {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let scored = loop {
        match plane
            .arena_message_receiver
            .as_ref()
            .expect("Arena scorer channel")
            .try_recv()
        {
            Ok(ArenaWorkerMessage::Scoring { job_id, result }) => break (job_id, result),
            Ok(_) => panic!("unexpected message after scoring began"),
            Err(mpsc::TryRecvError::Disconnected) => panic!("Arena scorer disconnected"),
            Err(mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "Arena scoring did not finish");
                thread::sleep(Duration::from_millis(2));
            }
        }
    };
    assert_eq!(scored.0, recovery_id);
    assert!(
        scored.1.is_ok(),
        "fixture evaluator must produce valid scores"
    );
    database
        .execute_batch(
            "CREATE TRIGGER reject_fixture_scored_terminal BEFORE INSERT ON events
             WHEN NEW.event_id = 'arena-job:scored-terminal-write-failure:terminal'
             BEGIN SELECT RAISE(ABORT, 'fixture scored terminal write failure'); END;",
        )
        .expect("reject successful terminal append");
    assert!(
        plane.finish_arena_scoring(&scored.0, scored.1).is_err(),
        "committed evaluation must not imply an acknowledged terminal"
    );
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after terminal rejection");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == format!("arena:evaluation:{recovery_id}:recorded"))
            .count(),
        1,
        "the evaluation receipt is durable before terminal acknowledgement"
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == format!("arena-job:{recovery_id}:terminal"))
            .count(),
        0,
        "the failed terminal append leaves no terminal event"
    );
    let recorded = load_recorded_evaluation(
        plane
            .open_arena_stores()
            .expect("open committed evaluation stores"),
        recovery_id,
    )
    .expect("verify committed evaluation receipt");
    let expected_evaluation = evaluation_record_from_recorded(&recorded);
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify committed evaluation event");
    let recorded_event = history
        .iter()
        .find(|event| event.event_id == format!("arena:evaluation:{recovery_id}:recorded"))
        .expect("canonical evaluation event");
    let receipt: serde_json::Value =
        serde_json::from_slice(&recorded_event.payload).expect("parse private receipt fixture");
    let artifact_store =
        ArtifactStore::open(data_dir.join("blobs")).expect("open committed evaluation CAS");
    let candidate_submission_id = ArtifactId::parse(
        receipt["candidate_submission_artifact_id"]
            .as_str()
            .expect("candidate submission CAS identity")
            .to_owned(),
    )
    .expect("parse candidate submission CAS identity");
    let candidate_submission_path = artifact_store.path_for(&candidate_submission_id);
    let candidate_submission_bytes =
        fs::read(&candidate_submission_path).expect("read candidate submission CAS blob");
    assert_eq!(
        ArtifactId::for_bytes(&candidate_submission_bytes),
        candidate_submission_id,
        "the removed blob is the exact receipt-bound candidate submission"
    );
    for field in [
        "visible_manifest_artifact_id",
        "sealed_manifest_artifact_id",
        "parent_submission_artifact_id",
    ] {
        let id = ArtifactId::parse(
            receipt[field]
                .as_str()
                .expect("receipt-bound preserved artifact identity")
                .to_owned(),
        )
        .expect("parse preserved artifact identity");
        artifact_store
            .get(&id)
            .expect("other receipt-bound evaluation evidence remains present");
    }
    let world_artifact_id = ArtifactId::parse(world.artifact_id.clone())
        .expect("parse registered World artifact identity");
    artifact_store
        .get(&world_artifact_id)
        .expect("registered World artifact remains present");
    ControlState::verify_artifacts(&history, &artifact_store, &plane.run_result_verifier)
        .expect("signed trial outputs and traces remain intact");
    let prior_event_hashes = history.iter().map(|event| event.hash).collect::<Vec<_>>();
    let run_result_verifier = plane.run_result_verifier.clone();
    database
        .execute_batch("DROP TRIGGER reject_fixture_scored_terminal;")
        .expect("restore successful terminal writes");
    drop(database);
    drop(plane);

    fs::remove_file(&candidate_submission_path).expect("remove only candidate submission blob");
    assert!(!candidate_submission_path.exists());
    for field in [
        "visible_manifest_artifact_id",
        "sealed_manifest_artifact_id",
        "parent_submission_artifact_id",
    ] {
        let id = ArtifactId::parse(
            receipt[field]
                .as_str()
                .expect("preserved artifact identity")
                .to_owned(),
        )
        .expect("parse preserved artifact identity");
        artifact_store
            .get(&id)
            .expect("preserved evaluation artifact must survive candidate removal");
    }
    assert!(artifact_store.get(&world_artifact_id).is_ok());
    ControlState::verify_artifacts(&history, &artifact_store, &run_result_verifier)
        .expect("signed trial outputs and traces survive candidate removal");
    let failed_recovery = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    );
    assert!(matches!(
        failed_recovery,
        Err(ControlError::Projection(message))
            if message == "Arena recovery receipt failed verification"
    ));
    let history_after_failed_recovery = EventStore::open(data_dir.join("events.sqlite3"))
        .expect("open history after missing-blob rejection")
        .replay_verified()
        .expect("verify unchanged history");
    assert_eq!(
        history_after_failed_recovery
            .iter()
            .map(|event| event.hash)
            .collect::<Vec<_>>(),
        prior_event_hashes,
        "failed recovery must not append or rewrite canonical events"
    );
    assert!(
        !history_after_failed_recovery
            .iter()
            .any(|event| { event.event_id == format!("arena-job:{recovery_id}:terminal") })
    );

    fs::write(&candidate_submission_path, &candidate_submission_bytes)
        .expect("restore exact candidate submission CAS blob");
    assert_eq!(
        artifact_store
            .get(&candidate_submission_id)
            .expect("verify restored candidate submission")
            .as_slice(),
        candidate_submission_bytes.as_slice()
    );

    for restart in 0..2 {
        let recovered = ControlPlane::open_with_repository_evaluator_and_reference_worker(
            &data_dir,
            &repository,
            &evaluator,
            &worker,
        )
        .expect("reopen and reconcile committed Arena evaluation");
        let terminal = &recovered.state.arena_jobs[recovery_id];
        assert_eq!(terminal.state, JobState::Succeeded);
        assert_eq!(terminal.terminal, Some(JobTerminal::Succeeded));
        assert_eq!(terminal.completed_trials, terminal.total_trials);
        assert_eq!(terminal.evaluation.as_ref(), Some(&expected_evaluation));
        let launch_failed = &recovered.state.arena_jobs["scorer-launch-failure"];
        assert_eq!(launch_failed.state, JobState::Interrupted);
        assert_eq!(launch_failed.terminal, Some(JobTerminal::Interrupted));
        assert!(launch_failed.evaluation.is_none());
        assert!(recovered.active_arena_job.is_none());
        let history = recovered
            .storage
            .as_ref()
            .expect("recovered canonical storage")
            .ledger
            .replay_verified()
            .expect("verify recovered history");
        assert_eq!(
            history
                .iter()
                .filter(|event| event.event_id == format!("arena-job:{recovery_id}:terminal"))
                .count(),
            1,
            "restart {restart} must leave exactly one terminal event"
        );
        assert_eq!(
            history
                .iter()
                .filter(|event| event.event_id == "arena-job:scorer-launch-failure:terminal")
                .count(),
            1,
            "restart {restart} must preserve one scorer launch terminal"
        );
    }

    fs::remove_file(&candidate_submission_path)
        .expect("remove receipt-bound candidate submission after terminal commit");
    assert!(matches!(
        ControlPlane::open_with_repository_evaluator_and_reference_worker(
            &data_dir,
            &repository,
            &evaluator,
            &worker,
        ),
        Err(ControlError::Projection(message))
            if message == "Arena terminal lacks trusted evaluation evidence"
    ));
    fs::write(&candidate_submission_path, &candidate_submission_bytes)
        .expect("restore exact candidate submission for completed terminal verification");
    assert_eq!(
        artifact_store
            .get(&candidate_submission_id)
            .expect("verify restored candidate submission")
            .as_slice(),
        candidate_submission_bytes.as_slice()
    );
    let mut recovered = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("reopen after restoring terminal-bound evidence");
    assert_eq!(
        recovered.state.arena_jobs[recovery_id].terminal,
        Some(JobTerminal::Succeeded)
    );

    let token = recovered.token_hex.clone();
    assert!(matches!(
        dispatch_call(
            &mut recovered,
            &token,
            "select-recovered-evaluation",
            Command::ArenaSelect {
                evaluation_id: recovery_id.to_owned(),
            },
        )
        .data,
        Some(ResponseData::Selection { .. })
    ));
    recovered
        .storage
        .as_mut()
        .expect("canonical selection storage")
        .ledger
        .append(EventInput::new(
            "selection:malformed-envelope-fixture",
            "arena:selection:malformed-envelope-fixture",
            "selection.recorded",
            "arena-plane",
            timestamp_millis().expect("selection fixture timestamp"),
            b"not-json",
        ))
        .expect("append malformed selection envelope to valid hash chain");
    drop(recovered);
    assert!(matches!(
        ControlPlane::open_with_repository_evaluator_and_reference_worker(
            &data_dir,
            &repository,
            &evaluator,
            &worker,
        ),
        Err(ControlError::Projection(message))
            if message == "canonical selection event is invalid"
    ));
}

fn register_dispatch_arena_objects(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
) -> (WorldRecord, GenomeRecord, GenomeRecord) {
    register_dispatch_arena_objects_with_invariants(plane, token, directory, None)
}

#[allow(clippy::too_many_lines)]
fn register_dispatch_arena_objects_with_invariants(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    invariant_manifest: Option<&[u8]>,
) -> (WorldRecord, GenomeRecord, GenomeRecord) {
    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    let visible = TrustedManifest::new(
        "dispatch-visible",
        Visibility::Visible,
        vec![
            hephaestus_arena::TrustedTask::new("visible-task", "visible", "VISIBLE")
                .expect("visible task"),
        ],
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        "dispatch-sealed",
        Visibility::Sealed,
        vec![
            hephaestus_arena::TrustedTask::new("sealed-task", "sealed", "SEALED")
                .expect("sealed task"),
        ],
    )
    .expect("sealed manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator = env::current_exe()
        .expect("locate test executable")
        .parent()
        .and_then(Path::parent)
        .expect("locate Cargo binary directory")
        .join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
    let evaluator_id = artifacts
        .put(&fs::read(evaluator).expect("read reference evaluator"))
        .expect("store evaluator identity");
    let verifier_id = artifacts
        .put(&plane.run_result_verifier.public_key_bytes())
        .expect("store result verifier");
    let invariant_entry = invariant_manifest.map_or_else(String::new, |manifest| {
        format!(
            r#","arena.invariant_manifest":"{}""#,
            artifacts
                .put(manifest)
                .expect("store invariant manifest")
                .as_str()
        )
    });
    drop(artifacts);
    let world_path = directory.path().join("arena-world.json");
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"dispatch-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":["harness"],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"{}}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
            invariant_entry,
        ),
    )
    .expect("write Arena World");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        "arena-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("Arena World registration should succeed");
    };
    let register_genome = |plane: &mut ControlPlane, token: &str, name: &str, parents: &str| {
        let path = directory.path().join(format!("{name}.md"));
        fs::write(
            &path,
            format!(
                "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
            ),
        )
        .expect("write Genome source");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            token,
            name,
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("Arena Genome registration should succeed");
        };
        genome
    };
    let parent = register_genome(plane, token, "arena-parent", "[]");
    let candidate = register_genome(
        plane,
        token,
        "arena-candidate",
        &format!("[\"{}\"]", parent.genome_id),
    );
    (world, parent, candidate)
}

#[test]
fn terminal_job_kill_is_idempotent_and_selection_errors_map_to_safe_api_states() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let job = JobRecord {
        job_id: "completed".to_owned(),
        genome_id: format!("hephaestus:genome:{}", "1".repeat(64)),
        run_id: "async-completed".to_owned(),
        source_revision: "2".repeat(40),
        world_id: format!("hephaestus:world:{}", "3".repeat(64)),
        task_id: "repository-inventory-v1".to_owned(),
        input_commitment: "4".repeat(64),
        seed: 0,
        environment_id: format!("reference-v1.{}", "5".repeat(64)),
        budget: RunBudgetReceipt {
            wall_millis: 10_000,
            maximum_output_bytes: 1_048_576,
            maximum_cost_microusd: 0,
        },
        state: JobState::Succeeded,
        terminal: Some(JobTerminal::Succeeded),
    };
    plane.state.jobs.insert(job.job_id.clone(), job);
    assert!(matches!(
        plane.job_status("missing"),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.kill_job("missing"),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.kill_job("completed").expect("idempotent terminal kill"),
        ResponseData::Job { job, .. } if job.terminal == Some(JobTerminal::Succeeded)
    ));
    assert!(matches!(
        map_selection_error(&ArenaError::UnknownEvaluation("missing".to_owned())),
        ExecuteError::NotFound
    ));
    assert!(matches!(
        map_selection_error(&ArenaError::UnsupportedSelectionConfidence(10_000)),
        ExecuteError::Rejected(_)
    ));
    assert!(matches!(
        map_selection_error(&ArenaError::BootstrapWorkExceeded),
        ExecuteError::Rejected(_)
    ));
    assert!(matches!(
        map_selection_error(&ArenaError::UnsupportedEvaluator),
        ExecuteError::Internal
    ));
    assert!(matches!(
        map_invariant_error(ArenaError::UnknownEvaluation("missing".to_owned())),
        ExecuteError::NotFound
    ));
    assert!(matches!(
        map_invariant_error(ArenaError::UnknownInvariantCheck("missing".to_owned())),
        ExecuteError::NotFound
    ));
    assert!(matches!(
        map_invariant_error(ArenaError::MissingWorldArtifact("arena.invariant_manifest")),
        ExecuteError::Rejected(message)
            if message == "registered World has no reference-output invariant profile"
    ));
    assert!(matches!(
        map_invariant_error(ArenaError::InvariantConflict("evaluation-001".to_owned())),
        ExecuteError::Rejected(message) if message == "evaluation-001"
    ));
    assert!(matches!(
        map_invariant_error(ArenaError::UnsupportedEvaluator),
        ExecuteError::Internal
    ));
}

#[test]
fn arena_selection_and_invariant_commands_validate_and_map_real_errors() {
    let directory = tempdir().expect("Arena selection fixture");
    let (mut plane, initial_parent, initial_candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();

    assert!(matches!(
        plane.select_arena_evaluation(""),
        Err(ExecuteError::Invalid("evaluation_id is required"))
    ));
    assert!(matches!(
        plane.check_arena_invariants(""),
        Err(ExecuteError::Invalid("evaluation_id is required"))
    ));
    assert!(matches!(
        plane.select_arena_evaluation("missing-evaluation"),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.check_arena_invariants("missing-evaluation"),
        Err(ExecuteError::NotFound)
    ));
    // require_command_fields already rejects a blank selection_event_id before
    // dispatch reaches this method; only a direct call exercises its own guard.
    assert!(matches!(
        plane.assess_genome("assessment", "proposal", ""),
        Err(ExecuteError::Invalid("selection_event_id is required"))
    ));

    for (request_id, command) in [
        (
            "arena-select-blank",
            Command::ArenaSelect {
                evaluation_id: String::new(),
            },
        ),
        (
            "arena-invariants-blank",
            Command::ArenaInvariants {
                evaluation_id: String::new(),
            },
        ),
    ] {
        let response = dispatch_call(&mut plane, &token, request_id, command);
        assert!(matches!(
            response.error,
            Some(error) if error.code == ApiErrorCode::InvalidRequest
                && error.message == "evaluation_id is required"
        ));
    }

    // This fixture's registered World has no invariant manifest, so a
    // completed evaluation is a real, otherwise-valid target that still
    // cannot be checked.
    complete_arena_test_job(
        &mut plane,
        "no-invariant-profile",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
    );
    assert!(matches!(
        plane.check_arena_invariants("no-invariant-profile"),
        Err(ExecuteError::Rejected(message))
            if message == "registered World has no reference-output invariant profile"
    ));

    let invariant_directory = tempdir().expect("Arena invariant dispatch fixture");
    let (mut invariant_plane, invariant_parent, invariant_candidate) =
        real_worker_arena_fixture_with_invariants(&invariant_directory, Some(CLEAN_INVARIANTS));
    let invariant_token = invariant_plane.token_hex.clone();
    complete_arena_test_job(
        &mut invariant_plane,
        "dispatched-invariants",
        &invariant_parent.genome_id,
        &invariant_candidate.genome_id,
    );
    let response = dispatch_call(
        &mut invariant_plane,
        &invariant_token,
        "arena-invariants-dispatch",
        Command::ArenaInvariants {
            evaluation_id: "dispatched-invariants".to_owned(),
        },
    );
    assert!(matches!(
        response.data,
        Some(ResponseData::ArenaInvariants { .. })
    ));
}

fn send_test_api_request(
    socket_path: &Path,
    token: &str,
    request_id: &str,
    command: Command,
) -> ApiResponse {
    let mut stream = UnixStream::connect(socket_path).expect("connect to control socket");
    let request = ApiRequest {
        version: API_VERSION,
        request_id: request_id.to_owned(),
        token: token.to_owned(),
        command,
    };
    stream
        .write_all(&serde_json::to_vec(&request).expect("encode request"))
        .expect("write request");
    stream
        .shutdown(std::net::Shutdown::Write)
        .expect("finish request");
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read response");
    serde_json::from_slice(&bytes).expect("decode response")
}

#[test]
fn projection_identifiers_text_and_trace_artifact_selection_fail_closed() {
    let hash = "a".repeat(64);
    assert_eq!(
        validate_content_id(&format!("hephaestus:genome:{hash}"), "genome").unwrap(),
        hash
    );
    assert!(validate_content_id("foreign:genome:abc", "genome").is_err());
    assert!(validate_content_id("hephaestus:genome:abc", "genome").is_err());
    assert!(require_projection_text("run-1", "run_id").is_ok());
    assert!(require_projection_text(" \t", "run_id").is_err());

    let make_trace = |run_id: &str, event_id: &str, artifact_id: &str| TraceReceipt {
        schema_version: 1,
        event_id: event_id.to_owned(),
        provenance: Provenance::new(
            run_id,
            format!("hephaestus:genome:{}", "1".repeat(64)),
            format!("hephaestus:world:{}", "2".repeat(64)),
        )
        .unwrap(),
        kind: TraceKind::LifecycleStarted,
        artifact_id: artifact_id.to_owned(),
        redacted_fields: 0,
    };
    let selected = make_trace("run-1", "trace-1", &"3".repeat(64));
    let unrelated = make_trace("run-2", "trace-2", &"4".repeat(64));
    let history = [
        stored_event(
            1,
            "trace.recorded",
            "run:run-1",
            "experience-plane",
            &serde_json::to_vec(&selected).unwrap(),
        ),
        stored_event(
            2,
            "trace.recorded",
            "run:run-2",
            "experience-plane",
            &serde_json::to_vec(&unrelated).unwrap(),
        ),
        stored_event(
            3,
            "trace.recorded",
            "run:run-1",
            "experience-plane",
            b"invalid",
        ),
        stored_event(4, "other.event", "run:run-1", "test", b"{}"),
    ];
    assert_eq!(
        trace_artifacts_for_run(&history[..2], "run-1").unwrap(),
        vec!["3".repeat(64)]
    );
    assert!(trace_artifacts_for_run(&history, "run-1").is_err());

    let receipt = TraceReceipt {
        schema_version: 1,
        event_id: "trace-1".to_owned(),
        provenance: Provenance::new(
            "run-1",
            format!("hephaestus:genome:{}", "1".repeat(64)),
            format!("hephaestus:world:{}", "2".repeat(64)),
        )
        .unwrap(),
        kind: TraceKind::LifecycleStarted,
        artifact_id: "6".repeat(64),
        redacted_fields: 0,
    };
    let mut trace_event = stored_event(1, "trace.recorded", "run:run-1", "experience-plane", b"{}");
    trace_event.event_id = receipt.event_id.clone();
    assert!(validate_trace_receipt(&trace_event, &receipt).is_ok());
    trace_event.actor = "runtime-plane".to_owned();
    assert!(validate_trace_receipt(&trace_event, &receipt).is_err());
    trace_event.actor = "experience-plane".to_owned();
    let mut malformed_receipt = receipt;
    malformed_receipt.artifact_id = "not-a-content-address".to_owned();
    assert!(validate_trace_receipt(&trace_event, &malformed_receipt).is_err());
}

#[test]
#[allow(clippy::too_many_lines)]
fn job_transition_rules_cover_admission_running_cancellation_and_terminal_edges() {
    fn record(state: JobState, terminal: Option<JobTerminal>) -> JobRecord {
        JobRecord {
            job_id: "job-1".to_owned(),
            genome_id: format!("hephaestus:genome:{}", "1".repeat(64)),
            run_id: "async-1".to_owned(),
            source_revision: "2".repeat(40),
            world_id: format!("hephaestus:world:{}", "3".repeat(64)),
            task_id: "repository-inventory-v1".to_owned(),
            input_commitment: "4".repeat(64),
            seed: 0,
            environment_id: format!("reference-v1.{}", "5".repeat(64)),
            budget: RunBudgetReceipt {
                wall_millis: 10_000,
                maximum_output_bytes: 1_048_576,
                maximum_cost_microusd: 0,
            },
            state,
            terminal,
        }
    }
    fn state_with(previous: Option<JobRecord>) -> ControlState {
        let mut jobs = BTreeMap::new();
        if let Some(previous) = previous {
            jobs.insert(previous.job_id.clone(), previous);
        }
        ControlState {
            freeze: FreezeState::frozen(&OperatorToken::from_bytes([1; 32])),
            active_runs: BTreeSet::new(),
            jobs,
            arena_jobs: BTreeMap::new(),
            job_progress: BTreeMap::new(),
            evaluation_events: BTreeMap::new(),
            run_results: BTreeMap::new(),
            completed_runs: BTreeSet::new(),
            registered: RegisteredObjects::default(),
            event_count: 0,
            worker_credentials: BTreeMap::new(),
            remote_jobs: BTreeMap::new(),
        }
    }
    let event = |event_type: &str| stored_event(1, event_type, "job:job-1", RUNTIME_ACTOR, b"{}");

    assert!(
        state_with(None)
            .job_transition_is_valid(&event("job.admitted"), &record(JobState::Admitted, None))
    );
    assert!(
        state_with(Some(record(JobState::Admitted, None)))
            .job_transition_is_valid(&event("job.running"), &record(JobState::Running, None))
    );
    assert!(
        state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
            &event("job.cancellation_requested"),
            &record(JobState::CancellationRequested, None)
        )
    );
    assert!(
        state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
            &event("job.terminal"),
            &record(JobState::Failed, Some(JobTerminal::Failed))
        )
    );
    assert!(
        state_with(Some(record(JobState::CancellationRequested, None))).job_transition_is_valid(
            &event("job.terminal"),
            &record(JobState::Interrupted, Some(JobTerminal::Cancelled))
        )
    );
    assert!(
        state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
            &event("job.terminal"),
            &record(JobState::Interrupted, Some(JobTerminal::Interrupted))
        )
    );
    assert!(
        !state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
            &event("job.terminal"),
            &record(JobState::Succeeded, Some(JobTerminal::Succeeded))
        )
    );
    assert!(
        !state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
            &event("job.terminal"),
            &record(JobState::Interrupted, Some(JobTerminal::Cancelled))
        )
    );
    assert!(
        state_with(Some(record(JobState::Running, None))).job_transition_is_valid(
            &event("job.terminal"),
            &record(JobState::Interrupted, Some(JobTerminal::Interrupted))
        )
    );
    assert!(
        !state_with(Some(record(JobState::Running, None)))
            .job_transition_is_valid(&event("job.unknown"), &record(JobState::Running, None))
    );
    let mut conflicting = record(JobState::Running, None);
    conflicting.world_id = format!("hephaestus:world:{}", "6".repeat(64));
    assert!(
        !state_with(Some(record(JobState::Running, None)))
            .job_transition_is_valid(&event("job.terminal"), &conflicting)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn command_field_validation_rejects_empty_ids_and_paths() {
    let invalid = [
        Command::GenomeShow {
            genome_id: " ".to_owned(),
        },
        Command::GenomePrompt {
            genome_id: String::new(),
        },
        Command::WorldShow {
            world_id: String::new(),
        },
        Command::WorldRegister {
            path: " ".to_owned(),
        },
        Command::GenomeRegister {
            path: "genome.md".to_owned(),
            world_id: " ".to_owned(),
        },
        Command::GenomeRegister {
            path: String::new(),
            world_id: "world".to_owned(),
        },
        Command::ArtifactPut {
            path: String::new(),
        },
        Command::RunSubmit {
            job_id: String::new(),
            genome_id: "genome".to_owned(),
        },
        Command::RunSubmit {
            job_id: "job".to_owned(),
            genome_id: String::new(),
        },
        Command::JobStatus {
            job_id: String::new(),
        },
        Command::JobKill {
            job_id: " ".to_owned(),
        },
        Command::GenomePropose {
            proposal_id: String::new(),
            selection_event_id: "selection".to_owned(),
            parent_genome_id: "candidate".to_owned(),
            hypothesis: Some("valid hypothesis".to_owned()),
            analysis_id: None,
            cluster_index: None,
        },
        Command::GenomePropose {
            proposal_id: "proposal".to_owned(),
            selection_event_id: String::new(),
            parent_genome_id: "candidate".to_owned(),
            hypothesis: Some("valid hypothesis".to_owned()),
            analysis_id: None,
            cluster_index: None,
        },
        Command::GenomePropose {
            proposal_id: "proposal".to_owned(),
            selection_event_id: "selection".to_owned(),
            parent_genome_id: "candidate".to_owned(),
            hypothesis: Some("\n".to_owned()),
            analysis_id: None,
            cluster_index: None,
        },
        Command::GenomePropose {
            proposal_id: "proposal".to_owned(),
            selection_event_id: "selection".to_owned(),
            parent_genome_id: "candidate".to_owned(),
            hypothesis: Some("x".repeat(513)),
            analysis_id: None,
            cluster_index: None,
        },
        Command::RunReference {
            genome_id: String::new(),
        },
        Command::RunEvaluation {
            genome_id: " ".to_owned(),
            task_id: "task".to_owned(),
            input: "input".to_owned(),
            seed: 0,
            wall_millis: 1,
            maximum_output_bytes: 1,
            maximum_cost_microusd: 0,
        },
        Command::EvaluatePair {
            evaluation_id: String::new(),
            parent_genome_id: "parent".to_owned(),
            candidate_genome_id: "candidate".to_owned(),
            remote: false,
        },
        Command::EvaluatePair {
            evaluation_id: "evaluation".to_owned(),
            parent_genome_id: " ".to_owned(),
            candidate_genome_id: "candidate".to_owned(),
            remote: false,
        },
        Command::EvaluatePair {
            evaluation_id: "evaluation".to_owned(),
            parent_genome_id: "parent".to_owned(),
            candidate_genome_id: String::new(),
            remote: false,
        },
        Command::ArenaSelect {
            evaluation_id: String::new(),
        },
    ];
    assert!(invalid.into_iter().all(|command| {
        matches!(
            require_command_fields(&command),
            Err(ExecuteError::Invalid(_))
        )
    }));
    assert!(require_command_fields(&Command::Status).is_ok());
}

#[test]
fn reference_worker_identity_requires_an_executable_regular_file() {
    let directory = tempdir().expect("temporary directory");
    let worker = directory.path().join("worker");
    fs::write(&worker, b"worker bytes").expect("write worker");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).expect("make executable");
    assert_eq!(
        executable_digest(&worker).expect("hash executable"),
        blake3::hash(b"worker bytes").to_hex().to_string()
    );
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o600))
        .expect("remove execute permission");
    assert!(executable_digest(&worker).is_err());
    assert!(executable_digest(directory.path()).is_err());
}

#[test]
fn pinned_reference_worker_rejects_mutated_snapshot_bytes() {
    let directory = tempdir().expect("temporary directory");
    let snapshot = tempfile::Builder::new()
        .prefix("pinned-worker-")
        .tempdir_in(directory.path())
        .expect("private worker directory");
    let executable = snapshot.path().join("worker");
    fs::write(&executable, b"pinned worker").expect("write worker");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let digest = executable_digest(&executable).expect("hash worker");
    let worker = PinnedReferenceWorker {
        directory: snapshot,
        executable: executable.clone(),
        digest,
    };
    worker.verify().expect("initial pinned worker is valid");

    fs::write(&executable, b"replaced worker").expect("replace snapshot bytes");
    assert!(matches!(
        worker.verify(),
        Err(ExecuteError::Rejected(message))
            if message == "pinned reference worker identity changed during execution"
    ));

    let private_directory = tempfile::Builder::new()
        .prefix("private-worker-")
        .tempdir_in(directory.path())
        .expect("private worker directory");
    let external_worker = directory.path().join("external-worker");
    fs::write(&external_worker, b"external worker").expect("write external worker");
    fs::set_permissions(&external_worker, fs::Permissions::from_mode(0o700))
        .expect("make external worker executable");
    let escaped = PinnedReferenceWorker {
        directory: private_directory,
        digest: executable_digest(&external_worker).expect("hash external worker"),
        executable: external_worker,
    };
    assert!(matches!(
        escaped.verify(),
        Err(ExecuteError::Rejected(message))
            if message == "pinned reference worker escaped its private directory"
    ));
}

#[test]
fn admitted_job_projection_binds_the_runtime_actor_and_immutable_spec() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (world, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let input = "Inventory the isolated repository without modifying it or using the network.";
    let job = JobRecord {
        job_id: "validated-job".to_owned(),
        genome_id: genome.genome_id,
        run_id: job_run_id("validated-job"),
        source_revision: "2".repeat(40),
        world_id: world.world_id,
        task_id: "repository-inventory-v1".to_owned(),
        input_commitment: blake3::hash(input.as_bytes()).to_hex().to_string(),
        seed: 0,
        environment_id: format!("reference-v1.{}", "5".repeat(64)),
        budget: RunBudgetReceipt {
            wall_millis: 10_000,
            maximum_output_bytes: 1_048_576,
            maximum_cost_microusd: 0,
        },
        state: JobState::Admitted,
        terminal: None,
    };
    let mut event = stored_event(1, "job.admitted", "job:validated-job", RUNTIME_ACTOR, b"{}");
    event.event_id = "job:validated-job:admitted".to_owned();
    assert!(plane.state.validate_job_record(&event, &job).is_ok());

    event.payload = serde_json::to_vec(&job).expect("serialize admitted job");
    plane
        .state
        .apply_job_record(&event)
        .expect("valid admitted job projects");
    let before = plane.state.snapshot();
    let mut forged_running = job.clone();
    forged_running.state = JobState::Running;
    forged_running.budget.maximum_output_bytes += 1;
    let mut running_event = event.clone();
    running_event.sequence += 1;
    running_event.event_id = "job:validated-job:running".to_owned();
    running_event.event_type = "job.running".to_owned();
    running_event.payload =
        serde_json::to_vec(&forged_running).expect("serialize forged running job");
    assert!(matches!(
        plane.state.apply_job_record(&running_event),
        Err(ControlError::Projection(message)) if message == "job spec binding is invalid"
    ));
    assert!(plane.state.snapshot() == before);

    event.actor = "untrusted-actor".to_owned();
    assert!(plane.state.validate_job_record(&event, &job).is_err());
}

#[test]
fn async_reference_spec_uses_read_only_offline_authority() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let worker = plane.pin_reference_worker().expect("pin reference worker");
    let spec = plane
        .async_reference_spec("authority-check", &genome, &worker)
        .expect("build direct reference spec");
    assert_eq!(spec.capabilities(), CapabilitySet::new(false, false));
}

#[test]
fn daemon_stop_cancels_active_job_before_acknowledging_shutdown() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    plane
        .submit_job("stop-active", &genome.genome_id)
        .expect("submit active job");
    assert!(matches!(
        plane.request_daemon_stop(),
        Err(ExecuteError::Busy)
    ));
    assert!(!plane.shutdown_requested);
    assert_eq!(
        plane.state.jobs["stop-active"].state,
        JobState::CancellationRequested
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist cancellation");
        assert!(Instant::now() < deadline, "job cancellation stalled");
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(plane.state.jobs["stop-active"].state, JobState::Interrupted);
    assert!(plane.request_daemon_stop().is_ok());
    assert!(plane.shutdown_requested);
}

fn wait_for_recorded_run_start(plane: &mut ControlPlane, run_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !plane.state.active_runs.contains(run_id) {
        plane
            .service_async_messages()
            .expect("persist run start evidence");
        assert!(Instant::now() < deadline, "runtime did not start");
        if !plane.state.active_runs.contains(run_id) {
            thread::sleep(Duration::from_millis(2));
        }
    }
}

fn assert_cancellation_terminal_replays_once(
    plane: ControlPlane,
    directory: &TempDir,
    repository: PathBuf,
    executable: &Path,
    worker: &Path,
    job_id: &str,
) {
    assert_eq!(plane.state.jobs[job_id].state, JobState::Interrupted);
    assert_eq!(
        plane.state.jobs[job_id].terminal,
        Some(JobTerminal::Cancelled)
    );
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify cancellation history");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == format!("job:{job_id}:cancellation_requested"))
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_id == format!("job:{job_id}:terminal"))
            .count(),
        1
    );
    drop(plane);

    let reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        directory.path(),
        repository,
        executable,
        worker,
    )
    .expect("replay confirmed cancellation");
    assert_eq!(reopened.state.jobs[job_id].state, JobState::Interrupted);
    assert_eq!(
        reopened.state.jobs[job_id].terminal,
        Some(JobTerminal::Cancelled)
    );
    let replayed_history = reopened
        .storage
        .as_ref()
        .expect("reopened canonical storage")
        .ledger
        .replay_verified()
        .expect("verify replayed cancellation history");
    assert_eq!(
        replayed_history
            .iter()
            .filter(|event| event.event_id == format!("job:{job_id}:terminal"))
            .count(),
        1
    );
}

#[test]
fn kill_all_signals_active_runtime_and_replays_confirmed_cancellation() {
    let directory = tempdir().expect("daemon directory");
    let slow_worker = directory.path().join("kill-all-worker");
    fs::write(&slow_worker, "#!/bin/sh\nexec /bin/sleep 60\n").expect("write slow worker");
    fs::set_permissions(&slow_worker, fs::Permissions::from_mode(0o700))
        .expect("make slow worker executable");
    let executable = env::current_exe().expect("test executable");
    let repository = env::current_dir().expect("repository working directory");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        directory.path(),
        repository.clone(),
        &executable,
        &slow_worker,
    )
    .expect("open control plane with slow worker");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    let job_id = "kill-all-runtime-cancel";
    plane
        .submit_job(job_id, &genome.genome_id)
        .expect("admit slow direct job");
    wait_for_recorded_run_start(&mut plane, &job_run_id(job_id));
    let runtime_cancel = Arc::clone(
        &plane
            .active_job
            .as_ref()
            .expect("active direct runtime")
            .cancel,
    );
    assert!(!runtime_cancel.load(Ordering::Acquire));

    assert!(matches!(
        plane.execute("kill-all-runtime-cancel-request", Command::KillAll),
        Ok(ResponseData::Acknowledged { killed_runs: 0, .. })
    ));
    assert!(runtime_cancel.load(Ordering::Acquire));
    assert_eq!(
        plane.state.jobs[job_id].state,
        JobState::CancellationRequested
    );
    assert_eq!(plane.state.jobs[job_id].terminal, None);
    assert!(
        plane.active_job.is_some(),
        "signal is not terminal confirmation"
    );

    let terminal_deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist confirmed runtime cancellation");
        assert!(
            Instant::now() < terminal_deadline,
            "worker cancellation stalled"
        );
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    assert_cancellation_terminal_replays_once(
        plane,
        &directory,
        repository,
        &executable,
        &slow_worker,
        job_id,
    );
}

#[test]
fn explicit_reference_worker_open_uses_the_supplied_executable_identity() {
    let directory = tempdir().expect("temporary directory");
    let worker = std::env::current_exe().expect("test executable");
    let plane = ControlPlane::open_with_repository_and_reference_worker(
        directory.path(),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        worker,
    )
    .expect("open with explicit worker");
    assert_eq!(plane.reference_worker_digest.len(), 64);
}

#[test]
fn evaluation_worker_snapshot_survives_deployment_path_replacement() {
    let directory = tempdir().expect("temporary directory");
    let worker = directory.path().join("deployed-worker");
    fs::copy(std::env::current_exe().expect("test executable"), &worker)
        .expect("copy worker executable");
    fs::set_permissions(&worker, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let plane = ControlPlane::open_with_repository_and_reference_worker(
        directory.path().join("daemon-data"),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &worker,
    )
    .expect("open control plane");
    let pinned = plane.pin_reference_worker().expect("pin worker");
    let environment = ControlPlane::reference_execution_environment(&pinned);
    let pinned_bytes = fs::read(&pinned.executable).expect("read pinned worker");
    assert_eq!(
        fs::metadata(pinned.directory.path())
            .expect("read snapshot directory metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&pinned.executable)
            .expect("read snapshot executable metadata")
            .permissions()
            .mode()
            & 0o777,
        0o500
    );

    fs::write(&worker, b"replacement at deployment path").expect("replace worker path");
    assert_ne!(
        executable_digest(&worker).expect("replacement stays executable"),
        plane.reference_worker_digest
    );
    pinned.verify().expect("pinned executable remains valid");
    assert_eq!(
        fs::read(&pinned.executable).expect("read pinned worker after replacement"),
        pinned_bytes
    );
    assert_eq!(
        ControlPlane::reference_execution_environment(&pinned),
        environment
    );
    fs::set_permissions(&pinned.executable, fs::Permissions::from_mode(0o700))
        .expect("make snapshot writable for tamper test");
    fs::write(&pinned.executable, b"tampered pinned worker").expect("tamper pinned snapshot");
    assert!(pinned.verify().is_err());
}

/// Counts the private `reference-worker-*` snapshot directories currently
/// under a daemon data directory: exactly the on-disk footprint of
/// `pin_reference_worker`'s cache.
fn count_reference_worker_snapshots(data_dir: &Path) -> usize {
    fs::read_dir(data_dir)
        .expect("read daemon data directory")
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("reference-worker-")
        })
        .count()
}

#[test]
fn pin_reference_worker_is_cached_and_reused_across_calls() {
    let directory = tempdir().expect("temporary directory");
    let plane = ControlPlane::open(directory.path()).expect("open control plane");
    let first = plane.pin_reference_worker().expect("pin reference worker");
    let second = plane
        .pin_reference_worker()
        .expect("reuse pinned reference worker");
    assert!(
        Arc::ptr_eq(&first, &second),
        "a second pin call must reuse the daemon-lifetime cached snapshot"
    );
    assert_eq!(first.executable, second.executable);
    assert_eq!(
        count_reference_worker_snapshots(&plane.data_dir),
        1,
        "one pin call, then a cached reuse, must write exactly one private snapshot"
    );
}

#[test]
fn submitted_job_and_paired_arena_evaluation_reuse_the_same_pinned_reference_worker() {
    let directory = tempdir().expect("Arena reuse fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);

    // The direct async job admission is the first use in this daemon's
    // lifetime: it creates the one private snapshot every later use shares.
    exercise_dispatch_job(&mut plane, &parent.genome_id);
    let job_environment_id = plane.state.jobs["dispatch-run"].environment_id.clone();
    let pinned_after_job = plane
        .pin_reference_worker()
        .expect("reuse pinned worker after direct job admission");
    assert_eq!(
        job_environment_id,
        ControlPlane::reference_execution_environment(&pinned_after_job)
    );

    // A paired Arena evaluation admitted afterward reuses the very same
    // cached snapshot rather than pinning a fresh private copy.
    complete_arena_test_job(
        &mut plane,
        "reuse-arena-evaluation",
        &parent.genome_id,
        &candidate.genome_id,
    );
    let arena_record = &plane.state.arena_jobs["reuse-arena-evaluation"];
    assert_eq!(arena_record.worker_digest, pinned_after_job.digest);
    assert_eq!(
        arena_record.environment_id,
        ControlPlane::reference_execution_environment(&pinned_after_job)
    );

    let pinned_after_arena = plane
        .pin_reference_worker()
        .expect("reuse pinned worker after paired Arena evaluation");
    assert!(
        Arc::ptr_eq(&pinned_after_job, &pinned_after_arena),
        "the direct job and the paired Arena evaluation must share one pinned Arc"
    );
    assert_eq!(
        count_reference_worker_snapshots(&plane.data_dir),
        1,
        "a submitted job followed by a paired Arena evaluation must still only ever \
         have written one private reference-worker snapshot"
    );
}

#[test]
fn pinned_reference_worker_cache_fails_closed_on_corruption_and_recovers_on_next_use() {
    let directory = tempdir().expect("Arena reuse fixture");
    let (mut plane, parent, _candidate) = real_worker_arena_fixture(&directory);
    let original = plane
        .pin_reference_worker()
        .expect("pin reference worker for the first time");

    // Corrupt the private snapshot bytes directly, as an attacker (or disk
    // corruption) would, without touching the public reference worker path.
    fs::set_permissions(&original.executable, fs::Permissions::from_mode(0o700))
        .expect("make cached snapshot writable for the corruption test");
    fs::write(&original.executable, b"corrupted pinned worker bytes")
        .expect("corrupt the cached private snapshot");

    // A submission that would use the corrupted cache is rejected before it
    // ever admits a job or spawns an executing thread.
    let rejection = plane
        .submit_job("corrupted-pin-job", &parent.genome_id)
        .expect_err("submission must fail closed on a corrupted pinned snapshot");
    assert!(
        matches!(
            &rejection,
            ExecuteError::Rejected(message)
                if message == "pinned reference worker identity changed during execution"
        ),
        "unexpected rejection: {rejection:?}"
    );
    assert!(
        plane.active_job.is_none(),
        "a rejected pin must never launch an execution thread"
    );
    assert!(
        !plane.state.jobs.contains_key("corrupted-pin-job"),
        "a rejected pin must never admit a job record"
    );

    // The next call drops the poisoned cache entry and pins a fresh snapshot
    // instead of being stuck reusing (or forever refusing) the tampered copy.
    let recovered = plane
        .pin_reference_worker()
        .expect("a fresh pin recovers after the corrupted cache entry is dropped");
    assert!(
        !Arc::ptr_eq(&original, &recovered),
        "recovery must pin a brand-new snapshot, not the corrupted one"
    );
    assert_eq!(recovered.digest, plane.reference_worker_digest);
    assert!(
        exercise_dispatch_job_succeeds(&mut plane, "recovered-pin-job", &parent.genome_id),
        "a submission after recovery must run to completion on the fresh snapshot"
    );
}

fn exercise_dispatch_job_succeeds(plane: &mut ControlPlane, job_id: &str, genome_id: &str) -> bool {
    let admitted = match plane.submit_job(job_id, genome_id) {
        Ok(ResponseData::Job { job, .. }) => job.state == JobState::Running,
        _ => return false,
    };
    if !admitted {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        if plane.service_async_messages().is_err() {
            return false;
        }
        if Instant::now() >= deadline {
            return false;
        }
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    plane.state.jobs[job_id].state == JobState::Succeeded
}

#[test]
fn pin_reference_worker_rejects_a_public_executable_replaced_before_first_pin() {
    let directory = tempdir().expect("temporary directory");
    let worker_path = directory.path().join("public-reference-worker");
    fs::copy(
        std::env::current_exe().expect("test executable"),
        &worker_path,
    )
    .expect("copy worker executable");
    fs::set_permissions(&worker_path, fs::Permissions::from_mode(0o700))
        .expect("make worker executable");
    let plane = ControlPlane::open_with_repository_and_reference_worker(
        directory.path().join("daemon-data"),
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &worker_path,
    )
    .expect("open control plane with an explicit reference worker");

    // The public executable changes before the daemon ever pins it: the
    // digest captured at startup no longer matches, so the very first pin
    // attempt must still fail closed.
    fs::write(&worker_path, b"replaced before first pin").expect("replace public worker path");
    fs::set_permissions(&worker_path, fs::Permissions::from_mode(0o700))
        .expect("keep replacement executable");
    assert!(matches!(
        plane.pin_reference_worker(),
        Err(ExecuteError::Rejected(message))
            if message == "reference worker identity changed after daemon startup"
    ));
    assert_eq!(
        count_reference_worker_snapshots(&plane.data_dir),
        0,
        "a rejected first pin must never leave a private snapshot behind"
    );
}

#[test]
fn authenticated_failures_are_safe_and_replay_divergence_is_detected() {
    let directory = tempdir().expect("temporary directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let blank = plane.handle(ApiRequest {
        version: API_VERSION,
        request_id: String::new(),
        token: plane.token_hex.clone(),
        command: Command::Status,
    });
    assert_eq!(
        blank.error.expect("blank request error").code,
        ApiErrorCode::InvalidRequest
    );
    let missing = plane.handle(ApiRequest {
        version: API_VERSION,
        request_id: "missing".to_owned(),
        token: plane.token_hex.clone(),
        command: Command::GenomeShow {
            genome_id: format!("hephaestus:genome:{}", "1".repeat(64)),
        },
    });
    assert_eq!(
        missing.error.expect("missing Genome error").code,
        ApiErrorCode::NotFound
    );

    let mut competing =
        EventStore::open(directory.path().join("events.sqlite3")).expect("open competing store");
    competing
        .append(EventInput::new(
            "competing-event",
            "other",
            "other.event",
            "test",
            1,
            b"{}",
        ))
        .expect("advance canonical tail");
    let internal = plane.handle(ApiRequest {
        version: API_VERSION,
        request_id: "stale-head".to_owned(),
        token: plane.token_hex.clone(),
        command: Command::Status,
    });
    assert_eq!(
        internal.error.expect("internal error").code,
        ApiErrorCode::Internal
    );

    let clean_directory = tempdir().expect("temporary directory");
    let mut clean = ControlPlane::open(clean_directory.path()).expect("open clean plane");
    clean.state.event_count = 1;
    assert!(matches!(
        clean.replay_response(),
        Err(ExecuteError::Internal)
    ));
}

#[test]
fn rejected_request_audit_failure_returns_safe_internal_and_recovers() {
    let directory = tempdir().expect("temporary directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let database = rusqlite::Connection::open(directory.path().join("events.sqlite3"))
        .expect("open fixture ledger trigger connection");
    database
        .execute_batch(
            "CREATE TRIGGER reject_control_request BEFORE INSERT ON events
             WHEN NEW.event_type = 'control.request_rejected'
             BEGIN SELECT RAISE(ABORT, 'fixture rejection append failure'); END;",
        )
        .expect("reject request rejection append");

    let failed = plane.handle(ApiRequest {
        version: API_VERSION,
        request_id: String::new(),
        token: plane.token_hex.clone(),
        command: Command::Status,
    });
    let error = failed
        .error
        .expect("safe response to rejected audit failure");
    assert_eq!(error.code, ApiErrorCode::Internal);
    assert_eq!(error.message, "canonical operation failed");
    assert!(
        plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify ledger after rejected request append")
            .is_empty()
    );

    database
        .execute_batch("DROP TRIGGER reject_control_request;")
        .expect("restore request audit writes");
    let token = plane.token_hex.clone();
    let recovered = dispatch_call(&mut plane, &token, "status-after-reject", Command::Status);
    assert!(recovered.error.is_none());
    assert!(matches!(recovered.data, Some(ResponseData::Status { .. })));
    assert_eq!(
        plane
            .storage
            .as_ref()
            .expect("canonical storage")
            .ledger
            .replay_verified()
            .expect("verify valid request after trigger removal")
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["control.status"]
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn projection_rejects_mismatched_commands_and_mutable_genome_metadata() {
    let token = OperatorToken::from_bytes([7; 32]);
    let run_result_verifier = RunResultSigner::from_seed([8; 32]).verifier();
    let mismatched = stored_event(
        1,
        "control.freeze",
        CONTROL_AGGREGATE,
        OPERATOR_ACTOR,
        br#"{"request_id":"mismatch","command":{"command":"unfreeze"}}"#,
    );
    assert!(matches!(
        ControlState::from_events(
            &[mismatched],
            RegisteredObjects::default(),
            &token,
            &run_result_verifier,
        ),
        Err(ControlError::Projection(_))
    ));

    let lifecycle = [
        stored_event(1, "run.started", "run", "runtime", br#"{"run_id":"r1"}"#),
        stored_event(2, "run.completed", "run", "runtime", br#"{"run_id":"r1"}"#),
    ];
    let state = ControlState::from_events(
        &lifecycle,
        RegisteredObjects::default(),
        &token,
        &run_result_verifier,
    )
    .expect("replay run lifecycle");
    assert!(state.active_runs.is_empty());

    let provenance = Provenance::new(
        "r2",
        format!("hephaestus:genome:{}", "7".repeat(64)),
        format!("hephaestus:world:{}", "8".repeat(64)),
    )
    .expect("valid provenance");
    let started = TraceReceipt {
        schema_version: 1,
        event_id: "fixture-1".to_owned(),
        provenance: provenance.clone(),
        kind: TraceKind::LifecycleStarted,
        artifact_id: "9".repeat(64),
        redacted_fields: 0,
    };
    let completed = TraceReceipt {
        schema_version: 1,
        event_id: "fixture-2".to_owned(),
        provenance,
        kind: TraceKind::LifecycleCompleted,
        artifact_id: "a".repeat(64),
        redacted_fields: 0,
    };
    let traces = [
        stored_event(
            1,
            "trace.recorded",
            "run:r2",
            "experience-plane",
            &serde_json::to_vec(&started).expect("encode started trace"),
        ),
        stored_event(
            2,
            "trace.recorded",
            "run:r2",
            "experience-plane",
            &serde_json::to_vec(&completed).expect("encode completed trace"),
        ),
    ];
    let replayed_trace_state = ControlState::from_events(
        &traces,
        RegisteredObjects::default(),
        &token,
        &run_result_verifier,
    )
    .expect("replay trace lifecycle");
    assert!(replayed_trace_state.active_runs.is_empty());
}

#[test]
#[allow(clippy::too_many_lines)]
fn canonical_path_token_and_identity_helpers_fail_closed() {
    assert!(data_dir_from_environment().is_ok());
    assert!(!constant_time_equal(b"short", b"different"));
    assert!(matches!(
        hex_decode("short"),
        Err(ControlError::Protocol(_))
    ));
    assert!(matches!(
        hex_decode(&"g".repeat(64)),
        Err(ControlError::Protocol(_))
    ));

    let key_directory = tempdir().expect("producer key directory");
    let missing_producer_key = key_directory.path().join("missing-producer.key");
    assert!(matches!(
        load_or_create_run_result_signer(&missing_producer_key, false),
        Err(ControlError::Protocol(_))
    ));
    let producer_key = key_directory.path().join("producer.key");
    let first_verifier = load_or_create_run_result_signer(&producer_key, true)
        .expect("create producer key")
        .verifier();
    let reopened_verifier = load_or_create_run_result_signer(&producer_key, false)
        .expect("reload producer key")
        .verifier();
    assert_eq!(first_verifier, reopened_verifier);
    fs::set_permissions(&producer_key, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        load_or_create_run_result_signer(&producer_key, false),
        Err(ControlError::Protocol(_))
    ));

    let directory = tempdir().expect("temporary directory");
    let data = directory.path().join("data");
    fs::create_dir(&data).expect("create data directory");
    assert!(matches!(
        ControlPlane::open_with_repository(&data, directory.path()),
        Err(ControlError::Protocol(_))
    ));
    let ordinary_file = directory.path().join("ordinary");
    fs::write(&ordinary_file, b"file").expect("write ordinary file");
    assert!(matches!(
        validate_source_repository(&ordinary_file),
        Err(ControlError::Protocol(_))
    ));
    assert!(matches!(
        prepare_private_directory(&ordinary_file),
        Err(ControlError::Protocol(_))
    ));
    let link = directory.path().join("link");
    symlink(directory.path(), &link).expect("create symlink");
    assert!(matches!(
        prepare_private_directory(&link),
        Err(ControlError::Protocol(_))
    ));
    assert!(matches!(
        prepare_private_file(directory.path()),
        Err(ControlError::Protocol(_))
    ));
    assert!(matches!(
        load_or_create_token(directory.path()),
        Err(ControlError::Protocol(_))
    ));
    assert!(matches!(
        remove_stale_socket(&ordinary_file),
        Err(ControlError::Protocol(_))
    ));

    let provenance = Provenance::new(
        "run",
        format!("hephaestus:genome:{}", "8".repeat(64)),
        format!("hephaestus:world:{}", "7".repeat(64)),
    )
    .expect("valid provenance");
    let receipt = TraceReceipt {
        schema_version: 1,
        event_id: "fixture-1".to_owned(),
        provenance,
        kind: TraceKind::LifecycleStarted,
        artifact_id: "9".repeat(64),
        redacted_fields: 0,
    };
    let forged_trace = stored_event(
        1,
        "trace.recorded",
        "run:run",
        "forged",
        &serde_json::to_vec(&receipt).expect("encode receipt"),
    );
    assert!(matches!(
        validate_trace_receipt(&forged_trace, &receipt),
        Err(ControlError::Projection(_))
    ));

    let result_payload = serde_json::json!({
        "schema_version": 2,
        "run_id": "run",
        "genome_id": format!("hephaestus:genome:{}", "8".repeat(64)),
        "world_id": format!("hephaestus:world:{}", "7".repeat(64)),
        "source_revision": "6".repeat(40),
        "task_id": "task",
        "input_commitment": "5".repeat(64),
        "seed": 1,
        "environment_id": "environment-v1",
        "budget": {
            "wall_millis": 10_000,
            "maximum_output_bytes": 1_048_576,
            "maximum_cost_microusd": 0
        },
        "completion_reason": "success",
        "latency_millis": 1,
        "actual_cost_microusd": 0,
        "stdout_artifact_id": "a".repeat(64),
        "stderr_artifact_id": "b".repeat(64),
        "trace_artifact_ids": ["c".repeat(64)]
    });
    let forged_result = stored_event(
        1,
        "run.result_recorded",
        "run:run",
        "forged",
        &serde_json::to_vec(&result_payload).expect("encode result"),
    );
    let run_result_verifier = RunResultSigner::from_seed([8; 32]).verifier();
    assert!(matches!(
        validate_run_result(&forged_result, &run_result_verifier),
        Err(ControlError::Projection(_))
    ));
    for reason in [
        CompletionReason::ProviderFailure,
        CompletionReason::OperatorInterrupt,
        CompletionReason::WallBudgetExceeded,
        CompletionReason::OutputBudgetExceeded,
        CompletionReason::IoFailure,
    ] {
        assert_ne!(run_completion_reason(reason), RunCompletionReason::Success);
    }
}

#[test]
fn legacy_unsigned_run_results_fail_with_actionable_incompatibility() {
    let directory = tempdir().expect("legacy directory");
    let database = directory.path().join("events.sqlite3");
    let mut ledger = EventStore::open(&database).expect("open legacy ledger");
    ledger
        .append(EventInput::new(
            "result:legacy",
            "run:legacy",
            "run.result_recorded",
            "runtime-plane",
            1,
            br#"{"schema_version":1,"run_id":"legacy"}"#,
        ))
        .expect("append legacy result");
    drop(ledger);
    let Err(error) = ControlPlane::open(directory.path()) else {
        panic!("legacy startup must fail");
    };
    assert!(format!("{error}").contains("back up the data directory and reinitialize"));
    assert!(!directory.path().join("runtime-producer.key").exists());
}

#[test]
fn evaluation_budget_boundaries_are_validated_before_execution() {
    assert!(validated_evaluation_budget(10_001, 1_048_576, 0).is_ok());
    assert!(
        validated_evaluation_budget(
            MAX_EVALUATION_WALL_MILLIS,
            MAX_EVALUATION_OUTPUT_BYTES,
            MAX_EVALUATION_COST_MICROUSD,
        )
        .is_ok()
    );
    assert!(validated_evaluation_budget(MAX_EVALUATION_WALL_MILLIS + 1, 1_048_576, 0,).is_err());
    assert!(
        validated_evaluation_budget(10_000, 1_048_576, MAX_EVALUATION_COST_MICROUSD + 1,).is_err()
    );
}

#[test]
fn world_verifier_and_producer_key_failures_are_covered() {
    let directory = tempdir().expect("fixture directory");
    let artifacts = ArtifactStore::open(directory.path().join("blobs")).expect("artifacts");

    let non_utf8 = artifacts.put(&[0xff]).expect("non-UTF-8 artifact");
    let non_utf8_world = world_registration_event(1, "non-utf8", non_utf8.as_str());
    assert!(RegisteredObjects::replay(&[non_utf8_world], &artifacts).is_err());

    let invalid_json = artifacts.put(b"{").expect("invalid World artifact");
    let invalid_world = world_registration_event(1, "invalid", invalid_json.as_str());
    assert!(RegisteredObjects::replay(&[invalid_world], &artifacts).is_err());

    let short_key = artifacts.put(b"short verifier").expect("short verifier");
    let (short_event, _) = compiled_world_registration(&artifacts, "short", short_key.as_str());
    let short_registered =
        RegisteredObjects::replay(&[short_event], &artifacts).expect("registered short key");
    assert!(anchored_world_verifier(&short_registered, &artifacts).is_err());

    let invalid_encoding = (0_u8..=u8::MAX)
        .map(|byte| [byte; 32])
        .find(|bytes| RunResultVerifier::from_public_key_bytes(*bytes).is_err())
        .expect("find a repeated-byte string that does not decompress as Ed25519");
    assert!(RunResultVerifier::from_public_key_bytes(invalid_encoding).is_err());
    let invalid_key = artifacts
        .put(&invalid_encoding)
        .expect("store invalid Ed25519 encoding");
    let (invalid_event, _) =
        compiled_world_registration(&artifacts, "invalid-encoding", invalid_key.as_str());
    let invalid_registered = RegisteredObjects::replay(&[invalid_event], &artifacts)
        .expect("project World registration before verifier validation");
    assert!(anchored_world_verifier(&invalid_registered, &artifacts).is_err());

    let first_signer = RunResultSigner::from_seed([21; 32]);
    let second_signer = RunResultSigner::from_seed([22; 32]);
    let first_key = artifacts
        .put(&first_signer.verifier().public_key_bytes())
        .expect("first verifier");
    let second_key = artifacts
        .put(&second_signer.verifier().public_key_bytes())
        .expect("second verifier");
    let (first_event, first_world) =
        compiled_world_registration(&artifacts, "first", first_key.as_str());
    let (second_event, _) = compiled_world_registration(&artifacts, "second", second_key.as_str());
    let different_registered =
        RegisteredObjects::replay(&[first_event.clone(), second_event], &artifacts)
            .expect("register Worlds with different verifier keys");
    assert!(anchored_world_verifier(&different_registered, &artifacts).is_err());

    let mut mismatched = first_world;
    mismatched.name = "forged-name".to_owned();
    let mismatched_event = stored_event(
        1,
        "world.registered",
        &mismatched.world_id,
        "test-fixture",
        &serde_json::to_vec(&mismatched).expect("mismatched World"),
    );
    assert!(RegisteredObjects::replay(&[mismatched_event], &artifacts).is_err());

    let producer_key = directory.path().join("producer.key");
    fs::write(&producer_key, [24; 32]).expect("producer key");
    fs::set_permissions(&producer_key, fs::Permissions::from_mode(0o600)).unwrap();
    let producer_link = directory.path().join("producer-link.key");
    symlink(&producer_key, &producer_link).expect("producer key link");
    assert!(load_or_create_run_result_signer(&producer_link, false).is_err());
}

fn compiled_world_registration(
    artifacts: &ArtifactStore,
    name: &str,
    verifier_id: &str,
) -> (StoredEvent, WorldRecord) {
    let source = format!(
        r#"{{"schema_version":1,"name":"{name}","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["coverage"],"evaluator_artifacts":{{"arena.runtime_verifier":"{verifier_id}"}}}}"#
    );
    let compiled = compile_world(&source, SourceFormat::Json, artifacts).expect("compile World");
    let artifact = artifacts
        .put(compiled.canonical_json())
        .expect("canonical World artifact");
    let record = WorldRecord {
        world_id: compiled.id().to_owned(),
        name: compiled.name().to_owned(),
        artifact_id: artifact.as_str().to_owned(),
    };
    let event = stored_event(
        1,
        "world.registered",
        &record.world_id,
        "test-fixture",
        &serde_json::to_vec(&record).expect("World record"),
    );
    (event, record)
}

fn world_registration_event(sequence: u64, name: &str, artifact_id: &str) -> StoredEvent {
    let record = WorldRecord {
        world_id: format!("hephaestus:world:{}", "d".repeat(64)),
        name: name.to_owned(),
        artifact_id: artifact_id.to_owned(),
    };
    stored_event(
        sequence,
        "world.registered",
        &record.world_id,
        "test-fixture",
        &serde_json::to_vec(&record).expect("World record"),
    )
}

fn stored_event(
    sequence: u64,
    event_type: &str,
    aggregate_id: &str,
    actor: &str,
    payload: &[u8],
) -> StoredEvent {
    StoredEvent {
        sequence,
        event_id: format!("fixture-{sequence}"),
        aggregate_id: aggregate_id.to_owned(),
        event_type: event_type.to_owned(),
        actor: actor.to_owned(),
        timestamp_millis: 1,
        payload: payload.to_vec(),
        previous_hash: [0; 32],
        hash: [0; 32],
    }
}

fn fixture_git(repository: &Path, arguments: &[&str]) -> std::process::Output {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .expect("run fixture Git command");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn committed_reference_fixture(directory: &Path) -> (PathBuf, String) {
    let repository = directory.join("reference-source");
    fs::create_dir_all(&repository).expect("create reference source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"deterministic fixture\n")
        .expect("write tracked fixture");
    fixture_git(&repository, &["add", "fixture.txt"]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);
    let revision = String::from_utf8(fixture_git(&repository, &["rev-parse", "HEAD"]).stdout)
        .expect("Git revision is UTF-8")
        .trim()
        .to_owned();
    (repository, revision)
}

fn corrupted_runtime_recorder_fixture(
    directory: &Path,
    run_id: &str,
) -> (
    RunSpec,
    Sandbox,
    CapabilityToken,
    EvidenceRecorder,
    PathBuf,
    PathBuf,
) {
    let (repository, _) = committed_reference_fixture(directory);
    let spec = RunSpec::new(
        run_id,
        "genome-fixture",
        "world-fixture",
        repository,
        "exercise recoverable evidence initialization",
        CapabilitySet::new(false, false),
        Budget::new(Duration::from_secs(5), 1024, 0).expect("valid run budget"),
    )
    .expect("valid run spec");
    let sandbox_root = directory.join("sandboxes");
    let manager =
        SandboxManager::open(&sandbox_root, Duration::from_secs(30)).expect("open sandbox manager");
    let (sandbox, token) = manager.create(&spec).expect("materialize sandbox");
    let sandbox_run = sandbox
        .worktree()
        .parent()
        .expect("sandbox run directory")
        .to_path_buf();

    let database = directory.join("events.sqlite3");
    let mut events = EventStore::open(&database).expect("open canonical events");
    events
        .append(EventInput::new(
            "fixture-valid-event",
            "run:fixture",
            "fixture.valid",
            "test",
            1,
            b"valid canonical payload",
        ))
        .expect("append valid canonical event");
    let artifacts = ArtifactStore::open(directory.join("artifacts")).expect("open artifact store");
    let recorder = EvidenceRecorder::from_stores(
        Box::new(events),
        Box::new(artifacts),
        RedactionPolicy::new([]),
        RetentionLimits::new(8, 1024).expect("valid retention limits"),
    );

    // Corrupt the already-open recorder's ledger through an independent SQLite connection.
    let connection = rusqlite::Connection::open(&database).expect("open tamper connection");
    let changed = connection
        .execute(
            "UPDATE events SET payload = ?1 WHERE event_id = ?2",
            rusqlite::params![
                b"tampered canonical payload".as_slice(),
                "fixture-valid-event"
            ],
        )
        .expect("tamper with persisted event");
    assert_eq!(changed, 1);
    drop(connection);

    (spec, sandbox, token, recorder, database, sandbox_run)
}

fn assert_recovery_failure_keeps_recorder_and_cleans_sandbox(
    result: &Result<ReferenceExecution, ExecuteError>,
    recorder: EvidenceRecorder,
    database: &Path,
    sandbox: Sandbox,
    sandbox_run: &Path,
) {
    assert!(matches!(result, Err(ExecuteError::Internal)));

    let connection = rusqlite::Connection::open(database).expect("open result check");
    let run_results: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM events WHERE event_type = 'run.result_recorded'",
            [],
            |row| row.get(0),
        )
        .expect("count recorded results");
    assert_eq!(run_results, 0, "failed recovery cannot record a run result");
    drop(connection);

    let (events, artifacts) = recorder.into_stores();
    assert!(
        events.replay_verified().is_err(),
        "returned recorder retains the corrupt canonical ledger"
    );
    let retained_id = artifacts
        .put(b"recorder ownership retained")
        .expect("returned recorder retains its artifact store");
    assert_eq!(
        artifacts.get(&retained_id).expect("read retained artifact"),
        b"recorder ownership retained"
    );
    drop((events, artifacts));

    sandbox
        .cleanup()
        .expect("cleanup sandbox after failed recovery");
    assert!(
        !sandbox_run.exists(),
        "failed recovery leaves no sandbox run"
    );
}

#[test]
fn reference_runtime_returns_recorder_when_recovery_detects_tampered_history() {
    let directory = tempdir().expect("runtime recovery fixture");
    let (spec, sandbox, token, recorder, database, sandbox_run) =
        corrupted_runtime_recorder_fixture(directory.path(), "reference-recovery");

    let (result, recorder) =
        execute_reference_runtime(recorder, &spec, &sandbox, &token, spec.run_id());

    assert_recovery_failure_keeps_recorder_and_cleans_sandbox(
        &result,
        recorder,
        &database,
        sandbox,
        &sandbox_run,
    );
}

#[test]
fn candidate_runtime_returns_recorder_when_recovery_detects_tampered_history() {
    let directory = tempdir().expect("runtime recovery fixture");
    let (spec, sandbox, token, recorder, database, sandbox_run) =
        corrupted_runtime_recorder_fixture(directory.path(), "candidate-recovery");
    let executable = std::env::current_exe().expect("test executable");
    let runtime = SupervisedRuntime::deterministic_guarded(
        IsolationPolicy::detect([]),
        &executable,
        [],
        &executable,
    )
    .expect("construct candidate runtime without launching it");

    let (result, recorder) =
        execute_candidate_runtime(runtime, recorder, &spec, &sandbox, &token, spec.run_id());

    assert_recovery_failure_keeps_recorder_and_cleans_sandbox(
        &result,
        recorder,
        &database,
        sandbox,
        &sandbox_run,
    );
}

#[test]
fn private_path_helpers_reject_children_beneath_regular_files_without_mutation() {
    let directory = tempdir().expect("private path fixture");
    let regular_file = directory.path().join("regular-file");
    let original = b"preserve this file";
    fs::write(&regular_file, original).expect("create regular file");
    let child = regular_file.join("child");

    assert!(prepare_private_directory(&child).is_err());
    assert!(prepare_private_file(&child).is_err());
    assert!(load_or_create_token(&child).is_err());
    assert!(load_or_create_run_result_signer(&child, true).is_err());
    assert!(remove_stale_socket(&child).is_err());

    assert_eq!(
        fs::read(&regular_file).expect("read unchanged regular file"),
        original
    );
    assert!(!child.exists());
}

fn run_result_receipt(plane: &ControlPlane, run_id: &str) -> RunResultReceipt {
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify runtime history");
    let event = history
        .iter()
        .find(|event| {
            event.event_type == "run.result_recorded"
                && event.event_id == format!("result:{run_id}")
        })
        .expect("signed run result event");
    assert_eq!(event.actor, "runtime-plane");
    RunResultReceipt::parse_from_event(event, &plane.run_result_verifier)
        .expect("authenticate run result receipt")
}

/// Asserts a synchronous run leaves no per-run sandbox state behind.
///
/// The reference worker's private snapshot (`reference-worker-*`) is now a
/// daemon-lifetime pin, reused by every later call instead of being written
/// and torn down on each run (see `pin_reference_worker`), so up to one such
/// directory is expected to remain after the first reference run; more than
/// one would mean a duplicate pin leaked.
fn assert_runtime_directories_clean(data_dir: &Path) {
    let mut reference_worker_snapshots = 0usize;
    for entry in fs::read_dir(data_dir).expect("read runtime data directory") {
        let entry = entry.expect("read runtime directory");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "sandboxes" {
            assert!(
                entry
                    .path()
                    .read_dir()
                    .expect("read sandbox root")
                    .next()
                    .is_none(),
                "synchronous run left a sandbox behind"
            );
        } else if name.starts_with("reference-worker-") {
            reference_worker_snapshots += 1;
        } else {
            assert!(
                !name.starts_with("sandbox-"),
                "synchronous run left runtime state behind: {name}"
            );
        }
    }
    assert!(
        reference_worker_snapshots <= 1,
        "synchronous run must reuse one daemon-lifetime pinned reference worker snapshot, found {reference_worker_snapshots}"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn synchronous_markdown_run_evaluation_persists_signed_provenance_and_replays() {
    let directory = tempdir().expect("fixture directory");
    let (repository, source_revision) = committed_reference_fixture(directory.path());
    let data_dir = directory.path().join("data");
    let current_executable = env::current_exe().expect("test executable");
    let cargo_bin = current_executable
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory");
    let worker = cargo_bin.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        worker.is_file(),
        "reference worker binary missing: {worker:?}"
    );
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &current_executable,
        &worker,
    )
    .expect("open control plane for synchronous reference evaluation");
    let token = plane.token_hex.clone();
    let (world, genome, prompt) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(prompt.contains("\"operation\":\"identity\""));
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    let task_id = "sync-evaluation-task";
    let input = "signed evaluation fixture input";
    let seed = 0x1234;
    let response = dispatch_call(
        &mut plane,
        &token,
        "sync-evaluation",
        Command::RunEvaluation {
            genome_id: genome.genome_id.clone(),
            task_id: task_id.to_owned(),
            input: input.to_owned(),
            seed,
            wall_millis: 10_000,
            maximum_output_bytes: 1_048_576,
            maximum_cost_microusd: 0,
        },
    );
    assert!(
        response.error.is_none(),
        "evaluation failed: {:?}",
        response.error
    );
    let (
        run_id,
        response_genome,
        response_world,
        response_revision,
        completion_reason,
        stdout_artifact_id,
        stderr_artifact_id,
        trace_artifact_ids,
    ) = match response.data.expect("evaluation response") {
        ResponseData::Run {
            run_id,
            genome_id,
            world_id,
            source_revision,
            completion_reason,
            stdout_artifact_id,
            stderr_artifact_id,
            trace_artifact_ids,
            ..
        } => (
            run_id,
            genome_id,
            world_id,
            source_revision,
            completion_reason,
            stdout_artifact_id,
            stderr_artifact_id,
            trace_artifact_ids,
        ),
        other => panic!("unexpected synchronous evaluation response: {other:?}"),
    };
    assert_eq!(response_genome, genome.genome_id);
    assert_eq!(response_world, world.world_id);
    assert_eq!(response_revision, source_revision);
    assert_eq!(completion_reason, RunCompletionReason::Success);

    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open output CAS");
    assert_eq!(
        artifacts
            .get(&ArtifactId::parse(stdout_artifact_id.clone()).expect("stdout artifact ID"))
            .expect("load output from CAS"),
        input.as_bytes()
    );
    assert!(
        artifacts
            .get(&ArtifactId::parse(stderr_artifact_id).expect("stderr artifact ID"))
            .expect("load diagnostics from CAS")
            .is_empty()
    );
    drop(artifacts);

    let receipt = run_result_receipt(&plane, &run_id);
    assert_eq!(receipt.run_id, run_id);
    assert_eq!(receipt.genome_id, genome.genome_id);
    assert_eq!(receipt.world_id, world.world_id);
    assert_eq!(receipt.source_revision, source_revision);
    assert_eq!(receipt.task_id, task_id);
    assert_eq!(
        receipt.input_commitment,
        blake3::hash(input.as_bytes()).to_hex().to_string()
    );
    assert_eq!(receipt.seed, seed);
    assert_eq!(receipt.completion_reason, RunCompletionReason::Success);
    assert_eq!(receipt.stdout_artifact_id, stdout_artifact_id);
    assert_eq!(receipt.trace_artifact_ids, trace_artifact_ids);
    assert_eq!(receipt.budget.wall_millis, 10_000);
    assert_eq!(receipt.budget.maximum_output_bytes, 1_048_576);
    assert_eq!(receipt.budget.maximum_cost_microusd, 0);
    let worker_digest = blake3::hash(&fs::read(&worker).expect("read worker identity"))
        .to_hex()
        .to_string();
    let worker_environment = format!(
        "{}|reference-instruction-language-v1|{worker_digest}",
        reference_environment_id()
    );
    assert_eq!(
        receipt.environment_id,
        format!(
            "reference-v1.{}",
            blake3::hash(worker_environment.as_bytes()).to_hex()
        )
    );
    assert!(plane.state.completed_runs.contains(&run_id));
    assert!(!plane.state.active_runs.contains(&run_id));
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify completed lifecycle");
    assert!(history.iter().any(|event| {
        event.event_type == "trace.recorded"
            && serde_json::from_slice::<TraceReceipt>(&event.payload).is_ok_and(|trace| {
                trace.provenance.run_id() == run_id
                    && trace.provenance.genome_id() == genome.genome_id
                    && trace.provenance.world_id() == world.world_id
                    && trace.kind == TraceKind::LifecycleCompleted
            })
    }));
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay synchronous evaluation"),
        ResponseData::Replay { .. }
    ));
    assert_runtime_directories_clean(&data_dir);
    drop(plane);

    let reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &current_executable,
        &worker,
    )
    .expect("reopen verified synchronous evaluation");
    assert!(reopened.state.completed_runs.contains(&run_id));
    assert!(!reopened.state.active_runs.contains(&run_id));
    assert_eq!(
        reopened.state.run_results.get(&run_id),
        Some(&receipt),
        "signed result provenance survives replay and reopen"
    );
    assert_runtime_directories_clean(&data_dir);
}

#[test]
#[allow(clippy::too_many_lines)]
fn synchronous_json_run_reference_inventories_committed_repository_and_replays() {
    let directory = tempdir().expect("fixture directory");
    let (repository, source_revision) = committed_reference_fixture(directory.path());
    let data_dir = directory.path().join("data");
    let current_executable = env::current_exe().expect("test executable");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &current_executable,
        &current_executable,
    )
    .expect("open control plane for no-prompt reference run");
    let token = plane.token_hex.clone();
    let (world, _, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let genome_path = directory.path().join("json-no-prompt.json");
    fs::write(
        &genome_path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "name": "json-no-prompt",
            "parents": [],
            "model": {"provider": "deterministic", "family": "reference"},
            "authority": {"workspace_write": false, "network": false},
            "artifacts": {}
        }))
        .expect("encode JSON Genome"),
    )
    .expect("write JSON Genome source");
    let Some(ResponseData::Genome { genome }) = dispatch_call(
        &mut plane,
        &token,
        "register-json-genome",
        Command::GenomeRegister {
            path: genome_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("JSON Genome registration should succeed");
    };
    assert!(
        plane
            .state
            .registered
            .genome(&genome.genome_id)
            .expect("registered JSON Genome")
            .compiled()
            .artifact_id("agent.prompt")
            .is_none(),
        "JSON Genome has no registered prompt"
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    let async_response = dispatch_call(
        &mut plane,
        &token,
        "submit-json-no-prompt",
        Command::RunSubmit {
            job_id: "json-no-prompt-async".to_owned(),
            genome_id: genome.genome_id.clone(),
        },
    );
    let error = async_response
        .error
        .expect("prompt-free JSON Genome cannot be submitted asynchronously");
    assert_eq!(error.code, ApiErrorCode::InvalidRequest);
    assert_eq!(
        error.message,
        "async reference jobs require a supported reference instruction"
    );
    assert!(
        !plane.state.jobs.contains_key("json-no-prompt-async"),
        "unsupported admission must not persist a job"
    );

    let response = dispatch_call(
        &mut plane,
        &token,
        "reference-inventory",
        Command::RunReference {
            genome_id: genome.genome_id.clone(),
        },
    );
    assert!(
        response.error.is_none(),
        "reference run failed: {:?}",
        response.error
    );
    let (run_id, stdout_artifact_id, revision) = match response.data.expect("reference response") {
        ResponseData::Run {
            run_id,
            stdout_artifact_id,
            source_revision,
            completion_reason,
            genome_id,
            world_id,
            ..
        } => {
            assert_eq!(completion_reason, RunCompletionReason::Success);
            assert_eq!(genome_id, genome.genome_id);
            assert_eq!(world_id, world.world_id);
            (run_id, stdout_artifact_id, source_revision)
        }
        other => panic!("unexpected reference response: {other:?}"),
    };
    assert_eq!(revision, source_revision);
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open inventory CAS");
    let stdout = artifacts
        .get(&ArtifactId::parse(stdout_artifact_id).expect("inventory artifact ID"))
        .expect("load inventory from CAS");
    let inventory: serde_json::Value = serde_json::from_slice(&stdout).expect("inventory JSON");
    assert_eq!(inventory["schema_version"], 1);
    assert_eq!(inventory["genome_id"], genome.genome_id);
    assert_eq!(inventory["world_id"], world.world_id);
    assert_eq!(inventory["source_revision"], source_revision);
    assert_eq!(inventory["files"].as_array().unwrap().len(), 1);
    assert_eq!(inventory["files"][0]["path"], "fixture.txt");
    assert_eq!(
        inventory["files"][0]["bytes"],
        b"deterministic fixture\n".len()
    );
    assert_eq!(
        inventory["files"][0]["blake3"],
        blake3::hash(b"deterministic fixture\n")
            .to_hex()
            .to_string()
    );
    drop(artifacts);

    let receipt = run_result_receipt(&plane, &run_id);
    let reference_input =
        "Inventory the isolated repository without modifying it or using the network.";
    assert_eq!(receipt.run_id, run_id);
    assert_eq!(receipt.genome_id, genome.genome_id);
    assert_eq!(receipt.world_id, world.world_id);
    assert_eq!(receipt.source_revision, source_revision);
    assert_eq!(receipt.task_id, "repository-inventory-v1");
    assert_eq!(
        receipt.input_commitment,
        blake3::hash(reference_input.as_bytes())
            .to_hex()
            .to_string()
    );
    assert_eq!(receipt.seed, 0);
    assert_eq!(receipt.environment_id, reference_environment_id());
    assert_eq!(receipt.completion_reason, RunCompletionReason::Success);
    assert!(plane.state.completed_runs.contains(&run_id));
    assert!(!plane.state.active_runs.contains(&run_id));
    assert!(matches!(
        plane.replay_response().expect("replay reference inventory"),
        ResponseData::Replay { .. }
    ));
    assert_runtime_directories_clean(&data_dir);
    drop(plane);

    let reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &current_executable,
        &current_executable,
    )
    .expect("reopen verified reference inventory");
    assert_eq!(reopened.state.run_results.get(&run_id), Some(&receipt));
    assert!(reopened.state.completed_runs.contains(&run_id));
    assert!(!reopened.state.active_runs.contains(&run_id));
}

const CLEAN_INVARIANTS: &[u8] = br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4096,"forbidden_ascii_bytes":[0]}"#;
// Forbids `V`, which only the uppercase child emits for the visible task.
const UPPERCASE_V_FORBIDDEN_INVARIANTS: &[u8] = br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":4096,"forbidden_ascii_bytes":[86]}"#;
// Every fixture output is longer than five bytes, so parent and child both
// violate the cap: no paired regression, yet the candidate contract fails.
const SHARED_OUTPUT_CAP_INVARIANTS: &[u8] = br#"{"schema_version":1,"algorithm":"reference-output-invariants-v1","maximum_output_bytes":5,"forbidden_ascii_bytes":[0]}"#;

struct AssessedChild {
    world: String,
    child: String,
    evaluation: String,
    selection_event: String,
}

/// Evaluates `parent` against `candidate`, proposes a one-flip child of
/// `candidate`, evaluates and assesses that child against `candidate`.
///
/// Selection's Pareto gate compares measured wall-clock latency, so an
/// improving child can measure as rejected on a slow scheduler tick. When
/// `require_metrics_pass` is set, the paired child evaluation is repeated
/// (bounded) until its verified receipt passes; every attempt stays in history.
fn assessed_forge_child(
    plane: &mut ControlPlane,
    prefix: &str,
    parent: &str,
    candidate: &str,
    require_metrics_pass: bool,
) -> AssessedChild {
    let source_evaluation = format!("{prefix}-source");
    complete_arena_test_job(plane, &source_evaluation, parent, candidate);
    let ResponseData::Selection { selection: source } = plane
        .select_arena_evaluation(&source_evaluation)
        .expect("select Forge source")
    else {
        panic!("source selection should produce a receipt");
    };
    let ResponseData::ForgeProposal { proposal } = plane
        .propose_genome(
            &format!("{prefix}-proposal"),
            &source.event.event_id,
            candidate,
            "Flip the single reference operation.",
        )
        .expect("record Forge proposal")
    else {
        panic!("proposal should return its durable record");
    };
    let mut attempt = 0;
    let (evaluation, child) = loop {
        let evaluation = format!("{prefix}-child-{attempt}");
        complete_arena_test_job(
            plane,
            &evaluation,
            candidate,
            &proposal.payload.child.genome_id,
        );
        let ResponseData::Selection { selection: child } = plane
            .select_arena_evaluation(&evaluation)
            .expect("select child evaluation")
        else {
            panic!("child selection should produce a receipt");
        };
        if !require_metrics_pass || child.receipt.metrics_eligible() {
            break (evaluation, child);
        }
        // Only measurement noise justifies another paired run; a child that did
        // not improve correctness fails immediately.
        assert!(
            child.receipt.correctness_improvements() > 0
                && child.receipt.correctness_regressions() == 0,
            "the proposed child did not improve correctness"
        );
        attempt += 1;
        assert!(
            attempt < 4,
            "an improving child never passed the measured gate"
        );
    };
    plane
        .assess_genome(
            &format!("{prefix}-assessment"),
            &format!("{prefix}-proposal"),
            &child.event.event_id,
        )
        .expect("record Forge assessment");
    AssessedChild {
        world: proposal.payload.world_id.clone(),
        child: proposal.payload.child.genome_id.clone(),
        evaluation,
        selection_event: child.event.event_id.clone(),
    }
}

fn champion_transition(
    plane: &mut ControlPlane,
    token: &str,
    request_id: &str,
    command: Command,
) -> Result<ChampionTransitionRecord, ApiError> {
    let response = dispatch_call(plane, token, request_id, command);
    match (response.data, response.error) {
        (Some(ResponseData::ChampionTransition { transition }), None) => Ok(*transition),
        (None, Some(error)) => Err(error),
        other => panic!("unexpected Champion response: {other:?}"),
    }
}

fn champion_show(plane: &mut ControlPlane, token: &str, world_id: &str) -> ChampionRecord {
    match dispatch_call(
        plane,
        token,
        "champion-show",
        Command::ChampionShow {
            world_id: world_id.to_owned(),
        },
    )
    .data
    {
        Some(ResponseData::Champion { champion }) => *champion,
        other => panic!("unexpected Champion projection: {other:?}"),
    }
}

fn assert_champion_error(
    result: Result<ChampionTransitionRecord, ApiError>,
    code: ApiErrorCode,
    message: &str,
) {
    let error = result.expect_err("Champion transition should be refused");
    assert_eq!((error.code, error.message.as_str()), (code, message));
}

fn champion_history(plane: &ControlPlane) -> Vec<StoredEvent> {
    plane
        .storage
        .as_ref()
        .expect("canonical Champion ledger")
        .ledger
        .replay_verified()
        .expect("verify Champion history")
}

fn champion_history_with_payload_edit(
    history: &[StoredEvent],
    event_id: &str,
    edit: impl FnOnce(&mut ChampionTransitionPayload),
) -> Vec<StoredEvent> {
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == event_id)
        .expect("Champion event exists");
    let mut payload: ChampionTransitionPayload =
        serde_json::from_slice(&event.payload).expect("decode Champion payload");
    edit(&mut payload);
    let canonical = serde_json::to_value(&payload).expect("canonicalize Champion payload");
    event.payload = serde_json::to_vec(&canonical).expect("encode Champion payload");
    tampered
}

#[test]
#[allow(clippy::too_many_lines)]
fn champion_seed_promote_and_rollback_join_verified_evidence_and_replay() {
    let directory = tempdir().expect("Champion fixture");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();

    for (request_id, command, expected) in [
        (
            "seed-invalid-id",
            Command::ChampionSeed {
                transition_id: "bad id".to_owned(),
                world_id: "world".to_owned(),
                genome_id: "genome".to_owned(),
                reason: "bootstrap".to_owned(),
            },
            "transition_id is invalid",
        ),
        (
            "seed-empty-world",
            Command::ChampionSeed {
                transition_id: "seed".to_owned(),
                world_id: " ".to_owned(),
                genome_id: "genome".to_owned(),
                reason: "bootstrap".to_owned(),
            },
            "world_id and genome_id are required",
        ),
        (
            "seed-control-reason",
            Command::ChampionSeed {
                transition_id: "seed".to_owned(),
                world_id: "world".to_owned(),
                genome_id: "genome".to_owned(),
                reason: "line\nbreak".to_owned(),
            },
            "reason must be 1 to 512 printable UTF-8 bytes",
        ),
        (
            "promote-invalid-assessment",
            Command::ChampionPromote {
                transition_id: "promote".to_owned(),
                assessment_id: "bad assessment".to_owned(),
            },
            "assessment_id is invalid",
        ),
        (
            "rollback-empty-world",
            Command::ChampionRollback {
                transition_id: "rollback".to_owned(),
                world_id: String::new(),
                reason: "regressed".to_owned(),
            },
            "world_id is required",
        ),
        (
            "rollback-empty-reason",
            Command::ChampionRollback {
                transition_id: "rollback".to_owned(),
                world_id: "world".to_owned(),
                reason: " ".to_owned(),
            },
            "reason must be 1 to 512 printable UTF-8 bytes",
        ),
        (
            "show-empty-world",
            Command::ChampionShow {
                world_id: String::new(),
            },
            "world_id is required",
        ),
    ] {
        let error = dispatch_call(&mut plane, &token, request_id, command)
            .error
            .expect("malformed Champion request is refused");
        assert_eq!(
            (error.code, error.message.as_str()),
            (ApiErrorCode::InvalidRequest, expected)
        );
    }

    let assessed = assessed_forge_child(
        &mut plane,
        "improve",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    let world_id = assessed.world.clone();
    let seed = |transition_id: &str, genome_id: &str| Command::ChampionSeed {
        transition_id: transition_id.to_owned(),
        world_id: world_id.clone(),
        genome_id: genome_id.to_owned(),
        reason: "Bootstrap the reference lineage.".to_owned(),
    };
    let promote = |transition_id: &str, assessment_id: &str| Command::ChampionPromote {
        transition_id: transition_id.to_owned(),
        assessment_id: assessment_id.to_owned(),
    };
    let rollback = |transition_id: &str| Command::ChampionRollback {
        transition_id: transition_id.to_owned(),
        world_id: world_id.clone(),
        reason: "Injected live regression.".to_owned(),
    };

    assert_eq!(
        champion_show(&mut plane, &token, &world_id).champion_genome_id,
        None
    );
    assert_eq!(
        dispatch_call(
            &mut plane,
            &token,
            "show-unknown-world",
            Command::ChampionShow {
                world_id: "hephaestus:world:missing".to_owned()
            }
        )
        .error
        .expect("unknown World")
        .code,
        ApiErrorCode::NotFound
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "promote-unseeded",
            promote("promote-early", "improve-assessment"),
        ),
        ApiErrorCode::InvalidRequest,
        "World has no Champion; seed one before promotion",
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "rollback-unseeded",
            rollback("rollback-early"),
        ),
        ApiErrorCode::InvalidRequest,
        "World has no previous Champion to restore",
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "seed-unknown-genome",
            seed("seed-unknown", "hephaestus:genome:missing"),
        ),
        ApiErrorCode::NotFound,
        "canonical record not found",
    );

    assert!(
        dispatch_call(&mut plane, &token, "freeze-seed", Command::Freeze)
            .error
            .is_none()
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "seed-frozen",
            seed("seed-frozen", &initial_candidate.genome_id),
        ),
        ApiErrorCode::InvalidRequest,
        "evolution is frozen",
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-seed", Command::Unfreeze)
            .error
            .is_none()
    );

    let seeded = champion_transition(
        &mut plane,
        &token,
        "seed",
        seed("seed-reference", &initial_candidate.genome_id),
    )
    .expect("seed Champion");
    assert_eq!(seeded.payload.kind, ChampionTransitionKind::Seeded);
    assert_eq!(
        seeded.payload.champion_genome_id,
        initial_candidate.genome_id
    );
    assert_eq!(seeded.payload.previous_champion_genome_id, None);
    assert_eq!(seeded.payload.previous_transition_event_id, None);
    assert_eq!(seeded.event.event_id, "champion:seed-reference:recorded");
    assert_eq!(seeded.event.aggregate_id, format!("champion:{world_id}"));
    assert_eq!(
        champion_transition(
            &mut plane,
            &token,
            "seed-retry",
            seed("seed-reference", &initial_candidate.genome_id)
        ),
        Ok(seeded.clone()),
        "an identical retry returns the recorded transition"
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "seed-conflict",
            seed("seed-reference", &initial_parent.genome_id),
        ),
        ApiErrorCode::InvalidRequest,
        "transition_id is already bound to different Champion content",
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "seed-twice",
            seed("seed-again", &initial_parent.genome_id),
        ),
        ApiErrorCode::InvalidRequest,
        "World already has Champion history; only promotion or rollback may change it",
    );

    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "promote-missing",
            promote("promote-missing", "missing-assessment"),
        ),
        ApiErrorCode::NotFound,
        "canonical record not found",
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "promote-no-invariants",
            promote("promote-child", "improve-assessment"),
        ),
        ApiErrorCode::InvalidRequest,
        "invariant evidence for the assessed evaluation is required",
    );
    let ResponseData::ArenaInvariants { invariants } = plane
        .check_arena_invariants(&assessed.evaluation)
        .expect("record child invariant evidence")
    else {
        panic!("invariant check should return its receipt");
    };
    assert!(invariants.receipt.regressions_within_budget);
    assert!(invariants.receipt.candidate_contract_satisfied);

    assert!(
        dispatch_call(&mut plane, &token, "freeze-promote", Command::Freeze)
            .error
            .is_none()
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "promote-frozen",
            promote("promote-child", "improve-assessment"),
        ),
        ApiErrorCode::InvalidRequest,
        "evolution is frozen",
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-promote", Command::Unfreeze)
            .error
            .is_none()
    );

    let promoted = champion_transition(
        &mut plane,
        &token,
        "promote",
        promote("promote-child", "improve-assessment"),
    )
    .expect("promote measured child");
    assert_eq!(promoted.payload.kind, ChampionTransitionKind::Promoted);
    assert_eq!(promoted.payload.champion_genome_id, assessed.child);
    assert_eq!(
        promoted.payload.previous_champion_genome_id.as_deref(),
        Some(initial_candidate.genome_id.as_str())
    );
    assert_eq!(
        promoted.payload.previous_transition_event_id.as_deref(),
        Some(seeded.event.event_id.as_str())
    );
    assert_eq!(
        promoted.payload.previous_transition_event_hash.as_deref(),
        Some(seeded.event.event_hash.as_str())
    );
    let evidence = promoted
        .payload
        .promotion
        .clone()
        .expect("promotion evidence");
    assert_eq!(evidence.assessment_id, "improve-assessment");
    assert_eq!(
        evidence.assessment_event_id,
        "forge-assessment:improve-assessment:recorded"
    );
    assert_eq!(evidence.evaluation_id, assessed.evaluation);
    assert_eq!(evidence.invariant_event_id, invariants.event.event_id);
    assert_eq!(evidence.invariant_event_hash, invariants.event.event_hash);
    assert_eq!(
        evidence.invariant_receipt_artifact_id,
        invariants.event.receipt_artifact_id
    );
    assert_eq!(promoted.payload.reason, None);
    let Some(ResponseData::EvaluationList { evaluations }) = dispatch_call(
        &mut plane,
        &token,
        "evaluations-after-promotion",
        Command::EvaluationList { limit: 50 },
    )
    .data
    else {
        panic!("evaluation list should succeed after promotion");
    };
    for entry in &evaluations {
        let expected: Vec<String> = if entry.evaluation.evaluation_id == assessed.evaluation {
            vec!["promote-child".to_owned()]
        } else {
            Vec::new()
        };
        assert_eq!(
            entry.champion_transition_ids, expected,
            "only the promoting evaluation references the Champion transition"
        );
    }
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "promote-stale",
            promote("promote-stale", "improve-assessment"),
        ),
        ApiErrorCode::InvalidRequest,
        "assessment parent is not the current Champion",
    );

    let projection = champion_show(&mut plane, &token, &world_id);
    assert_eq!(
        projection.champion_genome_id.as_deref(),
        Some(assessed.child.as_str())
    );
    assert_eq!(
        projection.standby_genome_ids,
        vec![initial_candidate.genome_id.clone()]
    );
    assert!(projection.quarantined_genome_ids.is_empty());
    assert_eq!(
        projection.transitions,
        vec![seeded.clone(), promoted.clone()]
    );

    // A worse child of the Champion measures as rejected and cannot be promoted.
    let regressed = assessed_forge_child(
        &mut plane,
        "regress",
        &initial_candidate.genome_id,
        &assessed.child,
        false,
    );
    assert_ne!(regressed.selection_event, assessed.selection_event);
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "promote-rejected",
            promote("promote-regressed", "regress-assessment"),
        ),
        ApiErrorCode::InvalidRequest,
        "Forge assessment did not pass the World metrics policy",
    );

    // Rollback is a safety action and remains available while frozen.
    assert!(
        dispatch_call(&mut plane, &token, "freeze-rollback", Command::Freeze)
            .error
            .is_none()
    );
    let rolled_back =
        champion_transition(&mut plane, &token, "rollback", rollback("rollback-child"))
            .expect("roll back to previous Champion");
    assert_eq!(rolled_back.payload.kind, ChampionTransitionKind::RolledBack);
    assert_eq!(
        rolled_back.payload.champion_genome_id,
        initial_candidate.genome_id
    );
    assert_eq!(
        rolled_back.payload.previous_champion_genome_id.as_deref(),
        Some(assessed.child.as_str())
    );
    assert_eq!(
        rolled_back.payload.previous_transition_event_id.as_deref(),
        Some(promoted.event.event_id.as_str())
    );
    assert_eq!(
        rolled_back.payload.reason.as_deref(),
        Some("Injected live regression.")
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "rollback-seed",
            rollback("rollback-seed"),
        ),
        ApiErrorCode::InvalidRequest,
        "World has no previous Champion to restore",
    );
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "unfreeze-after-rollback",
            Command::Unfreeze
        )
        .error
        .is_none()
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "repromote-quarantined",
            promote("promote-again", "improve-assessment"),
        ),
        ApiErrorCode::InvalidRequest,
        "assessment child already held or lost the Champion role",
    );

    let projection = champion_show(&mut plane, &token, &world_id);
    assert_eq!(
        projection.champion_genome_id.as_deref(),
        Some(initial_candidate.genome_id.as_str())
    );
    assert!(projection.standby_genome_ids.is_empty());
    assert_eq!(
        projection.quarantined_genome_ids,
        vec![assessed.child.clone()]
    );
    assert_eq!(projection.transitions.len(), 3);
    assert!(
        plane.state.registered.genome(&assessed.child).is_some(),
        "a quarantined Champion stays registered and reconstructable"
    );
    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));

    let history = champion_history(&plane);
    verify_champion_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        &plane.state.registered,
    )
    .expect("canonical Champion history verifies");
    for (event_id, edit) in [
        (
            promoted.event.event_id.clone(),
            Box::new(|payload: &mut ChampionTransitionPayload| {
                payload
                    .champion_genome_id
                    .clone_from(&initial_parent.genome_id);
            }) as Box<dyn FnOnce(&mut ChampionTransitionPayload)>,
        ),
        (
            promoted.event.event_id.clone(),
            Box::new(|payload: &mut ChampionTransitionPayload| {
                payload.previous_transition_event_hash = Some("0".repeat(64));
            }),
        ),
        (
            rolled_back.event.event_id.clone(),
            Box::new(|payload: &mut ChampionTransitionPayload| {
                payload.reason = None;
            }),
        ),
        (
            seeded.event.event_id.clone(),
            Box::new(|payload: &mut ChampionTransitionPayload| {
                payload.schema_version = 2;
            }),
        ),
    ] {
        let tampered = champion_history_with_payload_edit(&history, &event_id, edit);
        assert!(
            verify_champion_history(
                &plane.storage.as_ref().unwrap().artifacts,
                &tampered,
                &plane.state.registered
            )
            .is_err(),
            "tampered Champion transition {event_id} must fail replay"
        );
    }
    let mut noncanonical = history.clone();
    noncanonical
        .iter_mut()
        .find(|event| event.event_id == rolled_back.event.event_id)
        .expect("rollback event")
        .payload
        .push(b' ');
    assert!(
        verify_champion_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &noncanonical,
            &plane.state.registered
        )
        .is_err(),
        "a noncanonical Champion payload must fail replay"
    );
    // Nothing later depends on the final rollback, so only identity-based
    // detection can reject its rewritten event type.
    let mut retyped = history.clone();
    retyped
        .iter_mut()
        .find(|event| event.event_id == rolled_back.event.event_id)
        .expect("rollback event")
        .event_type = "champion.rewritten".to_owned();
    assert!(
        verify_champion_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &retyped,
            &plane.state.registered
        )
        .is_err()
    );
    let mut reordered = history;
    let seed_index = reordered
        .iter()
        .position(|event| event.event_id == seeded.event.event_id)
        .expect("seed event");
    let seed_event = reordered.remove(seed_index);
    reordered.push(seed_event);
    assert!(
        verify_champion_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &reordered,
            &plane.state.registered
        )
        .is_err(),
        "a promotion cannot precede the seed it depends on"
    );
}

#[test]
fn champion_promotion_requires_satisfied_invariant_contract() {
    for (manifest, regressions_within_budget) in [
        (UPPERCASE_V_FORBIDDEN_INVARIANTS, false),
        (SHARED_OUTPUT_CAP_INVARIANTS, true),
    ] {
        assert_promotion_refused_by_invariants(manifest, regressions_within_budget);
    }
}

fn assert_promotion_refused_by_invariants(manifest: &[u8], regressions_within_budget: bool) {
    let directory = tempdir().expect("Champion invariant fixture");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(manifest));
    let token = plane.token_hex.clone();
    let assessed = assessed_forge_child(
        &mut plane,
        "violating",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: assessed.world.clone(),
            genome_id: initial_candidate.genome_id.clone(),
            reason: "Bootstrap.".to_owned(),
        },
    )
    .expect("seed Champion");
    let ResponseData::ArenaInvariants { invariants } = plane
        .check_arena_invariants(&assessed.evaluation)
        .expect("record violating invariant evidence")
    else {
        panic!("invariant check should return its receipt");
    };
    assert!(!invariants.receipt.candidate_contract_satisfied);
    assert_eq!(
        invariants.receipt.regressions_within_budget,
        regressions_within_budget
    );
    assert_champion_error(
        champion_transition(
            &mut plane,
            &token,
            "promote-violating",
            Command::ChampionPromote {
                transition_id: "promote-violating".to_owned(),
                assessment_id: "violating-assessment".to_owned(),
            },
        ),
        ApiErrorCode::InvalidRequest,
        "invariant evidence does not satisfy the World contract",
    );
    assert_eq!(
        champion_show(&mut plane, &token, &assessed.world)
            .champion_genome_id
            .as_deref(),
        Some(initial_candidate.genome_id.as_str())
    );
}

// --- Failure-cluster analysis: `forge.clustered` ledger events and their use
// in an analysis-derived `genome propose`. Every default-identity candidate
// in this fixture already fails `visible-task` ("visible" != "VISIBLE") with
// a pure letter-case mismatch, so no extra fixture wiring is needed to
// produce a `shape_case_mismatch` cluster with a supported mutation.

fn cluster_analyze(
    plane: &mut ControlPlane,
    token: &str,
    analysis_id: &str,
    evaluation_id: &str,
) -> Box<ForgeAnalysisRecord> {
    let response = dispatch_call(
        plane,
        token,
        analysis_id,
        Command::ForgeAnalyze {
            analysis_id: analysis_id.to_owned(),
            evaluation_id: evaluation_id.to_owned(),
        },
    );
    match (response.data, response.error) {
        (Some(ResponseData::ForgeAnalysis { analysis }), None) => analysis,
        (data, error) => panic!("forge analyze should return its record: {data:?} {error:?}"),
    }
}

#[test]
fn cluster_analyze_records_deterministic_clusters_and_idempotent_replay() {
    let directory = tempdir().expect("cluster analysis fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();

    complete_arena_test_job(
        &mut plane,
        "cluster-evaluation",
        &parent.genome_id,
        &candidate.genome_id,
    );

    let analysis = cluster_analyze(&mut plane, &token, "cluster-analysis", "cluster-evaluation");
    assert_eq!(analysis.analysis.analysis_id, "cluster-analysis");
    assert_eq!(analysis.analysis.evaluation_id, "cluster-evaluation");
    assert_eq!(analysis.analysis.candidate_genome_id, candidate.genome_id);
    assert_eq!(
        analysis.event.event_id,
        "forge:analysis:cluster-analysis:clustered"
    );
    assert_eq!(analysis.event.event_type, "forge.clustered");
    let case_mismatch = analysis
        .analysis
        .clusters
        .iter()
        .find(|cluster| cluster.signature == "shape_case_mismatch")
        .expect("candidate's identity output should mismatch the uppercase-expected task");
    assert_eq!(case_mismatch.visible_count, 1);
    assert_eq!(analysis.analysis.algorithm, "failure-cluster-v2");
    assert_eq!(
        analysis.analysis.candidate_operation.as_deref(),
        Some("identity")
    );
    assert_eq!(
        case_mismatch.suggested_mutation,
        Some(SuggestedMutation::ReferenceOperation {
            operation_after: "ascii_uppercase".to_owned()
        })
    );

    // The identity candidate also answers the sealed task wrong; it is counted
    // without exposing any sealed content or suggesting a mutation.
    let sealed = analysis
        .analysis
        .clusters
        .iter()
        .find(|cluster| cluster.signature == "sealed_incorrect_output")
        .expect("sealed correctness failures are counted");
    assert_eq!((sealed.visible_count, sealed.sealed_count), (0, 1));
    assert_eq!(sealed.suggested_mutation, None);
    assert_eq!(analysis.analysis.total_sealed_failed_trials, 1);

    // Sealed task content never appears in the operator-visible analysis.
    let analysis_json = serde_json::to_string(&analysis.analysis).expect("encode analysis");
    assert!(!analysis_json.contains("SEALED"));
    assert!(!analysis_json.contains("sealed-task"));

    // An exact retry recomputes and returns the identical record.
    let retry = cluster_analyze(&mut plane, &token, "cluster-analysis", "cluster-evaluation");
    assert_eq!(retry.analysis, analysis.analysis);
    assert_eq!(retry.event, analysis.event);

    // Replay independently re-verifies the recorded `forge.clustered` event.
    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
}

#[test]
fn cluster_analyze_rejects_empty_identifiers() {
    let directory = tempdir().expect("cluster validation fixture");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();

    for (analysis_id, evaluation_id) in [("", "evaluation"), ("analysis", "")] {
        let response = dispatch_call(
            &mut plane,
            &token,
            "cluster-invalid",
            Command::ForgeAnalyze {
                analysis_id: analysis_id.to_owned(),
                evaluation_id: evaluation_id.to_owned(),
            },
        );
        assert!(matches!(
            response.error,
            Some(error) if error.code == ApiErrorCode::InvalidRequest
        ));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn cluster_propose_from_analysis_binds_hash_and_derives_hypothesis() {
    let directory = tempdir().expect("cluster proposal fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();

    complete_arena_test_job(
        &mut plane,
        "cluster-proposal-source",
        &parent.genome_id,
        &candidate.genome_id,
    );
    let ResponseData::Selection { selection } = plane
        .select_arena_evaluation("cluster-proposal-source")
        .expect("select Forge source")
    else {
        panic!("source selection should produce a receipt");
    };
    let analysis = cluster_analyze(
        &mut plane,
        &token,
        "cluster-proposal-analysis",
        "cluster-proposal-source",
    );
    let (cluster_index, cluster) = analysis
        .analysis
        .clusters
        .iter()
        .enumerate()
        .find(|(_, cluster)| cluster.suggested_mutation.is_some())
        .expect("a supported-mutation cluster exists");
    let cluster_index = u32::try_from(cluster_index).expect("small cluster index");
    let expected_hypothesis = cluster.hypothesis.clone();
    let expected_signature = cluster.signature.clone();

    let response = dispatch_call(
        &mut plane,
        &token,
        "cluster-proposal",
        Command::GenomePropose {
            proposal_id: "cluster-proposal".to_owned(),
            selection_event_id: selection.event.event_id.clone(),
            parent_genome_id: candidate.genome_id.clone(),
            hypothesis: None,
            analysis_id: Some("cluster-proposal-analysis".to_owned()),
            cluster_index: Some(cluster_index),
        },
    );
    let Some(ResponseData::ForgeProposal { proposal }) = response.data else {
        panic!(
            "analysis-derived proposal should succeed: {:?}",
            response.error
        );
    };
    assert_eq!(proposal.payload.hypothesis, expected_hypothesis);
    let binding = proposal
        .payload
        .analysis_binding
        .as_ref()
        .expect("proposal should bind the source analysis");
    assert_eq!(binding.analysis_id, "cluster-proposal-analysis");
    assert_eq!(binding.analysis_event_id, analysis.event.event_id);
    assert_eq!(binding.analysis_event_hash, analysis.event.event_hash);
    assert_eq!(binding.cluster_index, cluster_index);
    assert_eq!(binding.cluster_signature, expected_signature);

    // The unchanged operator-hypothesis path never sets a binding.
    let operator_response = dispatch_call(
        &mut plane,
        &token,
        "cluster-proposal-operator",
        Command::GenomePropose {
            proposal_id: "cluster-proposal-operator".to_owned(),
            selection_event_id: selection.event.event_id.clone(),
            parent_genome_id: candidate.genome_id.clone(),
            hypothesis: Some("Operator-authored hypothesis.".to_owned()),
            analysis_id: None,
            cluster_index: None,
        },
    );
    let Some(ResponseData::ForgeProposal {
        proposal: operator_proposal,
    }) = operator_response.data
    else {
        panic!(
            "operator-hypothesis proposal should still succeed: {:?}",
            operator_response.error
        );
    };
    assert!(operator_proposal.payload.analysis_binding.is_none());

    // Supplying both or neither is rejected before any evidence is touched.
    for command in [
        Command::GenomePropose {
            proposal_id: "cluster-proposal-both".to_owned(),
            selection_event_id: selection.event.event_id.clone(),
            parent_genome_id: candidate.genome_id.clone(),
            hypothesis: Some("Operator-authored hypothesis.".to_owned()),
            analysis_id: Some("cluster-proposal-analysis".to_owned()),
            cluster_index: Some(cluster_index),
        },
        Command::GenomePropose {
            proposal_id: "cluster-proposal-neither".to_owned(),
            selection_event_id: selection.event.event_id.clone(),
            parent_genome_id: candidate.genome_id.clone(),
            hypothesis: None,
            analysis_id: None,
            cluster_index: None,
        },
    ] {
        let response = dispatch_call(&mut plane, &token, "cluster-proposal-invalid", command);
        assert!(matches!(
            response.error,
            Some(error) if error.code == ApiErrorCode::InvalidRequest
        ));
    }

    // A cluster without a supported mutation cannot become a proposal.
    let unsupported_index = analysis
        .analysis
        .clusters
        .iter()
        .position(|cluster| cluster.suggested_mutation.is_none())
        .expect("the sealed correctness cluster has no supported mutation");
    let unsupported = dispatch_call(
        &mut plane,
        &token,
        "cluster-proposal-unsupported",
        Command::GenomePropose {
            proposal_id: "cluster-proposal-unsupported".to_owned(),
            selection_event_id: selection.event.event_id.clone(),
            parent_genome_id: candidate.genome_id.clone(),
            hypothesis: None,
            analysis_id: Some("cluster-proposal-analysis".to_owned()),
            cluster_index: Some(u32::try_from(unsupported_index).expect("small index")),
        },
    );
    assert!(matches!(
        unsupported.error,
        Some(error) if error.code == ApiErrorCode::InvalidRequest
            && error.message == "the selected cluster has no supported mutation"
    ));

    // An analysis of a different evaluation cannot be bound to this selection.
    complete_arena_test_job(
        &mut plane,
        "cluster-proposal-other",
        &parent.genome_id,
        &candidate.genome_id,
    );
    cluster_analyze(
        &mut plane,
        &token,
        "cluster-other-analysis",
        "cluster-proposal-other",
    );
    let mismatched = dispatch_call(
        &mut plane,
        &token,
        "cluster-proposal-mismatch",
        Command::GenomePropose {
            proposal_id: "cluster-proposal-mismatch".to_owned(),
            selection_event_id: selection.event.event_id.clone(),
            parent_genome_id: candidate.genome_id.clone(),
            hypothesis: None,
            analysis_id: Some("cluster-other-analysis".to_owned()),
            cluster_index: Some(cluster_index),
        },
    );
    assert!(matches!(
        mismatched.error,
        Some(error) if error.code == ApiErrorCode::InvalidRequest
            && error.message == "the bound analysis must have clustered the exact same selected candidate"
    ));
    // An analysis ID stays bound to the evaluation it first clustered.
    let reused = dispatch_call(
        &mut plane,
        &token,
        "cluster-analysis-reuse",
        Command::ForgeAnalyze {
            analysis_id: "cluster-proposal-analysis".to_owned(),
            evaluation_id: "cluster-proposal-other".to_owned(),
        },
    );
    assert!(
        matches!(
            reused.error,
            Some(error) if error.code == ApiErrorCode::InvalidRequest
        ),
        "reusing an analysis ID for another evaluation must be refused"
    );
}

// --- Evidence API: RunList, EvaluationList, DenialList (roadmap item 9 prerequisite) ---

#[test]
fn evidence_run_list_limit_is_bounded() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    for (limit, expect_error) in [(0u32, true), (201, true), (1, false), (200, false)] {
        let response = dispatch_call(
            &mut plane,
            &token,
            &format!("run-list-{limit}"),
            Command::RunList { limit },
        );
        assert_eq!(
            response.error.map(|error| error.code),
            expect_error.then_some(ApiErrorCode::InvalidRequest),
            "limit={limit}"
        );
    }
}

#[test]
fn evidence_run_list_is_newest_first_and_merges_jobs_with_verified_results() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    let reference_response = dispatch_call(
        &mut plane,
        &token,
        "reference-run",
        Command::RunReference {
            genome_id: genome.genome_id.clone(),
        },
    );
    let Some(ResponseData::Run {
        run_id: reference_run_id,
        ..
    }) = reference_response.data
    else {
        panic!(
            "reference run should succeed: {:?}",
            reference_response.error
        );
    };

    exercise_dispatch_job(&mut plane, &genome.genome_id);

    let Some(ResponseData::RunList { runs }) = dispatch_call(
        &mut plane,
        &token,
        "run-list",
        Command::RunList { limit: 10 },
    )
    .data
    else {
        panic!("run list should succeed");
    };
    assert_eq!(runs.len(), 2, "one job and one reference run");
    assert_eq!(
        runs[0].job_id.as_deref(),
        Some("dispatch-run"),
        "the async job committed last must be newest-first"
    );
    assert_eq!(runs[0].state, JobState::Succeeded);
    assert_eq!(
        runs[0].completion_reason,
        Some(RunCompletionReason::Success)
    );
    assert!(runs[0].latency_millis.is_some());
    assert!(runs[0].actual_cost_microusd.is_some());
    assert_eq!(runs[1].run_id, reference_run_id);
    assert_eq!(runs[1].job_id, None, "a synchronous run has no job_id");
    assert_eq!(
        runs[1].completion_reason,
        Some(RunCompletionReason::Success)
    );

    let bounded = dispatch_call(
        &mut plane,
        &token,
        "run-list-bounded",
        Command::RunList { limit: 1 },
    );
    let Some(ResponseData::RunList { runs: bounded_runs }) = bounded.data else {
        panic!("bounded run list should succeed");
    };
    assert_eq!(bounded_runs.len(), 1);
    assert_eq!(bounded_runs[0].job_id.as_deref(), Some("dispatch-run"));

    // Consistency with replay: a fresh verified replay does not change what is listed.
    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
    let Some(ResponseData::RunList { runs: replayed }) = dispatch_call(
        &mut plane,
        &token,
        "run-list-again",
        Command::RunList { limit: 10 },
    )
    .data
    else {
        panic!("second run list should succeed");
    };
    assert_eq!(runs, replayed);
}

#[test]
fn evidence_run_list_is_available_while_a_job_is_active() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    assert!(
        plane
            .submit_job("busy-run", &genome.genome_id)
            .is_ok_and(|data| matches!(data, ResponseData::Job { .. }))
    );
    assert!(plane.active_job.is_some(), "job admission stays active");
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "run-list-busy",
            Command::RunList { limit: 10 }
        )
        .error
        .is_none(),
        "read-only evidence lists must stay available while a job is active"
    );
}

#[test]
fn evidence_evaluation_list_includes_selection_and_invariant_summaries_and_hides_sealed_data() {
    let directory = tempdir().expect("Arena evidence fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture_with_invariants(
        &directory,
        Some(UPPERCASE_V_FORBIDDEN_INVARIANTS),
    );
    let token = plane.token_hex.clone();
    complete_arena_test_job(
        &mut plane,
        "evidence-evaluation-1",
        &parent.genome_id,
        &candidate.genome_id,
    );
    plane
        .select_arena_evaluation("evidence-evaluation-1")
        .expect("select evaluation");
    plane
        .check_arena_invariants("evidence-evaluation-1")
        .expect("check invariants");
    // A later evaluation with no selection or invariant evidence yet.
    complete_arena_test_job(
        &mut plane,
        "evidence-evaluation-2",
        &parent.genome_id,
        &candidate.genome_id,
    );

    let response = dispatch_call(
        &mut plane,
        &token,
        "evaluation-list",
        Command::EvaluationList { limit: 10 },
    );
    let Some(ResponseData::EvaluationList { evaluations }) = response.data.clone() else {
        panic!("evaluation list should succeed");
    };
    assert_eq!(evaluations.len(), 2);
    let pending = &evaluations[0];
    assert_eq!(pending.evaluation.evaluation_id, "evidence-evaluation-2");
    assert!(
        pending.selection.is_none() && pending.invariants.is_none(),
        "evidence recorded for another evaluation must not be attributed to this one"
    );
    let entry = &evaluations[1];
    assert_eq!(entry.evaluation.evaluation_id, "evidence-evaluation-1");
    assert!(
        entry.selection.is_some(),
        "a recorded selection must be summarized"
    );
    assert!(
        entry.invariants.is_some(),
        "recorded invariant evidence must be summarized"
    );
    assert!(entry.forge_assessment.is_none());
    assert!(entry.champion_transition_ids.is_empty());

    let encoded = serde_json::to_string(&response).expect("response serializes");
    for forbidden in ["sealed", "expected_output", "task_input", "raw_output"] {
        assert!(
            !encoded.contains(forbidden),
            "evaluation list must never expose {forbidden}"
        );
    }
}

#[test]
fn evidence_evaluation_list_references_forge_assessment_and_orders_newest_first() {
    let directory = tempdir().expect("Forge evidence fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let assessed = assessed_forge_child(
        &mut plane,
        "evidence-forge",
        &parent.genome_id,
        &candidate.genome_id,
        false,
    );

    let Some(ResponseData::EvaluationList { evaluations }) = dispatch_call(
        &mut plane,
        &token,
        "evaluation-list-forge",
        Command::EvaluationList { limit: 10 },
    )
    .data
    else {
        panic!("evaluation list should succeed");
    };
    assert_eq!(evaluations.len(), 2, "source and child evaluations");
    assert_eq!(
        evaluations[0].evaluation.evaluation_id, assessed.evaluation,
        "the child evaluation is newest and must sort first"
    );
    let forge = evaluations[0]
        .forge_assessment
        .as_ref()
        .expect("child evaluation carries its Forge assessment reference");
    assert_eq!(forge.assessment_id, "evidence-forge-assessment");
    assert_eq!(
        evaluations[1].evaluation.evaluation_id, "evidence-forge-source",
        "the source evaluation is oldest and must sort last"
    );
    assert!(evaluations[1].forge_assessment.is_none());

    let Some(ResponseData::EvaluationList {
        evaluations: bounded,
    }) = dispatch_call(
        &mut plane,
        &token,
        "evaluation-list-forge-bounded",
        Command::EvaluationList { limit: 1 },
    )
    .data
    else {
        panic!("bounded evaluation list should succeed");
    };
    assert_eq!(
        bounded.len(),
        1,
        "the limit must bound the returned entries"
    );
    assert_eq!(bounded[0].evaluation.evaluation_id, assessed.evaluation);
}

#[test]
fn evidence_evaluation_list_limit_is_bounded() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    for (limit, expect_error) in [(0u32, true), (201, true), (1, false)] {
        let response = dispatch_call(
            &mut plane,
            &token,
            &format!("evaluation-list-{limit}"),
            Command::EvaluationList { limit },
        );
        assert_eq!(
            response.error.map(|error| error.code),
            expect_error.then_some(ApiErrorCode::InvalidRequest),
            "limit={limit}"
        );
    }
}

#[test]
fn evidence_denial_list_limit_is_bounded() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    for (limit, expect_error) in [(0u32, true), (201, true), (1, false)] {
        let response = dispatch_call(
            &mut plane,
            &token,
            &format!("denial-list-{limit}"),
            Command::DenialList { limit },
        );
        assert_eq!(
            response.error.map(|error| error.code),
            expect_error.then_some(ApiErrorCode::InvalidRequest),
            "limit={limit}"
        );
    }
}

#[test]
fn evidence_denial_list_records_request_rejected_and_runtime_capability_denials_newest_first() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();

    let rejected = plane.handle(ApiRequest {
        version: API_VERSION,
        request_id: String::new(),
        token: token.clone(),
        command: Command::Status,
    });
    assert_eq!(
        rejected.error.expect("empty request_id is refused").code,
        ApiErrorCode::InvalidRequest
    );

    let denied_genome_id = format!("hephaestus:genome:{}", "a".repeat(64));
    let denied_world_id = format!("hephaestus:world:{}", "b".repeat(64));
    {
        let storage = plane.storage.as_mut().expect("canonical storage");
        let receipt = TraceReceipt {
            schema_version: 1,
            event_id: "trace:denial-run:capability_denied".to_owned(),
            provenance: Provenance::new(
                "denial-run",
                denied_genome_id.clone(),
                denied_world_id.clone(),
            )
            .expect("valid provenance"),
            kind: TraceKind::CapabilityDenied,
            artifact_id: "c".repeat(64),
            redacted_fields: 0,
        };
        storage
            .ledger
            .append(EventInput::new(
                receipt.event_id.clone(),
                "run:denial-run".to_owned(),
                "trace.recorded",
                "experience-plane",
                timestamp_millis().expect("clock reads"),
                serde_json::to_vec(&receipt).expect("encode trace receipt"),
            ))
            .expect("append runtime denial trace");
    }

    let Some(ResponseData::DenialList { denials }) = dispatch_call(
        &mut plane,
        &token,
        "denial-list",
        Command::DenialList { limit: 10 },
    )
    .data
    else {
        panic!("denial list should succeed");
    };
    assert_eq!(denials.len(), 2);
    assert_eq!(denials[0].kind, DenialKind::RuntimeCapabilityDenied);
    assert_eq!(denials[0].run_id.as_deref(), Some("denial-run"));
    assert_eq!(
        denials[0].genome_id.as_deref(),
        Some(denied_genome_id.as_str())
    );
    assert_eq!(
        denials[0].world_id.as_deref(),
        Some(denied_world_id.as_str())
    );
    assert_eq!(denials[1].kind, DenialKind::RequestRejected);
    assert_eq!(denials[1].request_id.as_deref(), Some(""));
    assert_eq!(denials[1].command.as_deref(), Some("status"));

    let bounded = dispatch_call(
        &mut plane,
        &token,
        "denial-list-bounded",
        Command::DenialList { limit: 1 },
    );
    let Some(ResponseData::DenialList {
        denials: bounded_denials,
    }) = bounded.data
    else {
        panic!("bounded denial list should succeed");
    };
    assert_eq!(bounded_denials.len(), 1);
    assert_eq!(bounded_denials[0].kind, DenialKind::RuntimeCapabilityDenied);
}

fn history_with_payload_replaced(
    history: &[StoredEvent],
    event_id: &str,
    from: &str,
    to: &str,
) -> Vec<StoredEvent> {
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == event_id)
        .expect("tampered event exists");
    let payload = String::from_utf8(event.payload.clone()).expect("UTF-8 payload");
    assert!(payload.contains(from), "payload must contain {from}");
    event.payload = payload.replace(from, to).into_bytes();
    tampered
}

#[test]
#[allow(clippy::too_many_lines)]
fn cluster_and_invariant_histories_reject_tampered_events() {
    let directory = tempdir().expect("tamper fixture");
    let (mut plane, parent, candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    complete_arena_test_job(
        &mut plane,
        "tamper-evaluation",
        &parent.genome_id,
        &candidate.genome_id,
    );
    let ResponseData::ArenaInvariants { invariants } = plane
        .check_arena_invariants("tamper-evaluation")
        .expect("record invariant evidence")
    else {
        panic!("invariant check should return its receipt");
    };
    let analysis = cluster_analyze(&mut plane, &token, "tamper-analysis", "tamper-evaluation");

    assert!(matches!(
        plane.analyze_forge_clusters(" ", "tamper-evaluation"),
        Err(ExecuteError::Invalid(
            "analysis_id and evaluation_id are required"
        ))
    ));
    assert!(matches!(
        plane.analyze_forge_clusters("unknown-analysis", "missing-evaluation"),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.analyze_forge_clusters("bad analysis id", "tamper-evaluation"),
        Err(ExecuteError::Invalid("analysis_id is invalid"))
    ));

    let history = plane
        .storage
        .as_ref()
        .expect("canonical ledger")
        .ledger
        .replay_verified()
        .expect("verified history");
    let registered = &plane.state.registered;
    verify_cluster_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        registered,
    )
    .expect("canonical clusters");
    verify_invariant_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        registered,
    )
    .expect("canonical invariants");

    let world_id = analysis.analysis.world_id.clone();
    let foreign_world = format!("hephaestus:world:{}", "0".repeat(64));
    let cluster_event = analysis.event.event_id.clone();
    let invariant_event = invariants.event.event_id.clone();
    let cluster_artifact = analysis.event.analysis_artifact_id.clone();
    let invariant_artifact = invariants.event.receipt_artifact_id.clone();
    assert_ne!(cluster_artifact, invariant_artifact);

    for tampered in [
        history_with_payload_replaced(&history, &cluster_event, &world_id, &foreign_world),
        history_with_payload_replaced(
            &history,
            &cluster_event,
            &cluster_artifact,
            &invariant_artifact,
        ),
        history_with_payload_replaced(
            &history,
            &cluster_event,
            "\"schema_version\":1",
            "\"schema_version\":2",
        ),
    ] {
        assert!(
            verify_cluster_history(
                &plane.storage.as_ref().unwrap().artifacts,
                &tampered,
                registered
            )
            .is_err(),
            "a tampered forge.clustered event must fail replay"
        );
    }
    for tampered in [
        history_with_payload_replaced(&history, &invariant_event, &world_id, &foreign_world),
        history_with_payload_replaced(
            &history,
            &invariant_event,
            &invariant_artifact,
            &cluster_artifact,
        ),
        history_with_payload_replaced(
            &history,
            &invariant_event,
            "\"schema_version\":1",
            "\"schema_version\":2",
        ),
    ] {
        assert!(
            verify_invariant_history(
                &plane.storage.as_ref().unwrap().artifacts,
                &tampered,
                registered
            )
            .is_err(),
            "a tampered invariants.recorded event must fail replay"
        );
    }
}

#[test]
fn evidence_run_list_marks_a_budget_exceeded_direct_run_failed() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    let Some(ResponseData::Run {
        run_id,
        completion_reason,
        ..
    }) = dispatch_call(
        &mut plane,
        &token,
        "tiny-output-run",
        Command::RunEvaluation {
            genome_id: genome.genome_id.clone(),
            task_id: "tiny-output".to_owned(),
            input: "inventory".to_owned(),
            seed: 1,
            wall_millis: 10_000,
            maximum_output_bytes: 1,
            maximum_cost_microusd: 0,
        },
    )
    .data
    else {
        panic!("budget-limited evaluation run should record a signed result");
    };
    assert_eq!(completion_reason, RunCompletionReason::OutputBudgetExceeded);
    let Some(ResponseData::RunList { runs }) = dispatch_call(
        &mut plane,
        &token,
        "run-list",
        Command::RunList { limit: 10 },
    )
    .data
    else {
        panic!("run list should succeed");
    };
    let entry = runs
        .iter()
        .find(|entry| entry.run_id == run_id)
        .expect("the direct run is listed");
    assert_eq!(entry.job_id, None);
    assert_eq!(entry.state, JobState::Failed);
    assert_eq!(
        entry.completion_reason,
        Some(RunCompletionReason::OutputBudgetExceeded)
    );
}

// ---------------------------------------------------------------------------
// Gauntlet failure modes (roadmap item 10). Each of the seven named modes
// (context loss, premature completion, schema drift, bad routing, duplicate
// subagents, poisoned memory, hallucinated verification) is a deterministic
// reference-worker operation pair defined in
// `hephaestus_runtime::reference_instruction`: a "bad" operation that
// exhibits the pathology on a crafted scenario, and a paired "fix" operation
// that avoids it on the exact same input. `examples/gauntlet/<mode>/` holds
// the matching example World/Genomes/tasks for a manual walkthrough; this
// registers structurally identical objects in-process and proves, at the
// Arena level, that the Genome carrying the bad operation is measurably
// rejected (zero correctness) while the Genome carrying the fix passes.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn register_gauntlet_objects(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    mode: &str,
    task_input: &str,
    expected_output: &str,
    bad_operation: &str,
    good_operation: &str,
) -> (WorldRecord, GenomeRecord, GenomeRecord) {
    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    let visible = TrustedManifest::new(
        format!("{mode}-visible"),
        Visibility::Visible,
        vec![
            TrustedTask::new(format!("{mode}-visible-task"), task_input, expected_output)
                .expect("visible task"),
        ],
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        format!("{mode}-sealed"),
        Visibility::Sealed,
        vec![
            TrustedTask::new(format!("{mode}-sealed-task"), task_input, expected_output)
                .expect("sealed task"),
        ],
    )
    .expect("sealed manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator = env::current_exe()
        .expect("locate test executable")
        .parent()
        .and_then(Path::parent)
        .expect("locate Cargo binary directory")
        .join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
    let evaluator_id = artifacts
        .put(&fs::read(evaluator).expect("read reference evaluator"))
        .expect("store evaluator identity");
    let verifier_id = artifacts
        .put(&plane.run_result_verifier.public_key_bytes())
        .expect("store result verifier");
    let invariant_id = artifacts
        .put(CLEAN_INVARIANTS)
        .expect("store invariant manifest");
    drop(artifacts);
    let world_path = directory.path().join(format!("gauntlet-{mode}-world.json"));
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"gauntlet-{mode}","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":["harness"],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}","arena.invariant_manifest":"{}"}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
            invariant_id.as_str(),
        ),
    )
    .expect("write Gauntlet World");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        &format!("gauntlet-{mode}-world"),
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("Gauntlet World registration should succeed");
    };
    let register_genome = |plane: &mut ControlPlane,
                           token: &str,
                           name: &str,
                           parents: &str,
                           operation: &str| {
        let path = directory.path().join(format!("gauntlet-{mode}-{name}.md"));
        fs::write(
            &path,
            format!(
                "---\nschema_version: 1\nname: gauntlet-{mode}-{name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"{operation}\"}}\n```\n"
            ),
        )
        .expect("write Gauntlet Genome source");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            token,
            &format!("gauntlet-{mode}-{name}"),
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("Gauntlet Genome registration should succeed");
        };
        genome
    };
    let parent = register_genome(plane, token, "parent", "[]", bad_operation);
    let candidate = register_genome(
        plane,
        token,
        "candidate",
        &format!("[\"{}\"]", parent.genome_id),
        good_operation,
    );
    (world, parent, candidate)
}

fn real_worker_gauntlet_fixture(
    directory: &TempDir,
    mode: &str,
    task_input: &str,
    expected_output: &str,
    bad_operation: &str,
    good_operation: &str,
) -> (ControlPlane, GenomeRecord, GenomeRecord) {
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"Gauntlet fixture\n").expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);

    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    let evaluator = directory.path().join("fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open Gauntlet fixture");
    let token = plane.token_hex.clone();
    let (_, parent, candidate) = register_gauntlet_objects(
        &mut plane,
        &token,
        directory,
        mode,
        task_input,
        expected_output,
        bad_operation,
        good_operation,
    );
    assert!(
        dispatch_call(&mut plane, &token, "gauntlet-unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    (plane, parent, candidate)
}

/// One case per named Gauntlet failure mode: the bad operation's Genome is
/// measurably rejected (zero correctness against the crafted task) and the
/// paired fix's Genome passes (full correctness), on the exact same input.
#[test]
fn gauntlet_failure_modes_reject_the_bad_operation_and_pass_the_fix() {
    for (mode, task_input, expected_output, bad_operation, good_operation) in [
        (
            "context-loss",
            r#"{"turns":["FACT: the deploy key is banana","small talk","more small talk","what is the deploy key?"]}"#,
            " the deploy key is banana",
            "context_loss_naive",
            "context_loss_aware",
        ),
        (
            "premature-completion",
            r#"{"steps":["step1:DONE_A","step2:DONE_B","step3:DONE_C"]}"#,
            "DONE_A,DONE_B,DONE_C",
            "premature_completion",
            "verified_completion",
        ),
        (
            "schema-drift",
            r#"{"schema_version":2,"field_v1":null,"field_v2":"correct-value"}"#,
            "correct-value",
            "schema_drift_brittle",
            "schema_drift_adaptive",
        ),
        (
            "bad-routing",
            r#"{"requires_capability":"large_context","routes":[{"name":"cheap","capability":"small","cost":1},{"name":"expensive","capability":"large_context","cost":9}]}"#,
            "expensive",
            "bad_routing_cheapest",
            "capability_aware_routing",
        ),
        (
            "duplicate-subagents",
            r#"{"requests":["task-a","task-a","task-b"]}"#,
            "task-a,task-b",
            "duplicate_subagents_wasteful",
            "deduplicated_subagents",
        ),
        (
            "poisoned-memory",
            r#"{"memory":[{"text":"correct-fact","trusted":true},{"text":"malicious-fact","trusted":false}]}"#,
            "correct-fact",
            "poisoned_memory_trusting",
            "provenance_checked_memory",
        ),
        (
            "hallucinated-verification",
            r#"{"claimed_output":"success-value","claimed_status":"success","actual_state":"actual-value"}"#,
            "actual-value",
            "hallucinated_verification_trusting",
            "ground_truth_verification",
        ),
    ] {
        let directory = tempdir().expect("Gauntlet fixture directory");
        let (mut plane, parent, candidate) = real_worker_gauntlet_fixture(
            &directory,
            mode,
            task_input,
            expected_output,
            bad_operation,
            good_operation,
        );
        let evaluation_id = format!("gauntlet-{mode}-evaluation");
        complete_arena_test_job(
            &mut plane,
            &evaluation_id,
            &parent.genome_id,
            &candidate.genome_id,
        );
        let ResponseData::Selection { selection } = plane
            .select_arena_evaluation(&evaluation_id)
            .expect("select Gauntlet evidence")
        else {
            panic!("selection should succeed for {mode}");
        };
        assert_eq!(
            selection.receipt.parent_correctness_bps(),
            0,
            "the bad operation should fail {mode}'s task entirely"
        );
        assert_eq!(
            selection.receipt.candidate_correctness_bps(),
            10_000,
            "the fix should pass {mode}'s task entirely"
        );
        assert!(
            selection.receipt.correctness_improvements() > 0,
            "the fix should register as a correctness improvement for {mode}"
        );
    }
}

/// Registers a minimal Fifo/`HighestTransferEffect`-free Evolver strategy
/// (roadmap item 13) and returns its content-derived strategy id.
fn register_test_strategy(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    name: &str,
    mutation_prioritization: &str,
    gene_selection: &str,
) -> String {
    register_test_strategy_with_candidate_count(
        plane,
        token,
        directory,
        name,
        mutation_prioritization,
        gene_selection,
        1,
    )
}

/// TD-17 (roadmap items 10, 13): like [`register_test_strategy`], but with an
/// explicit `candidate_count` so a test can exercise multi-candidate
/// generations.
fn register_test_strategy_with_candidate_count(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    name: &str,
    mutation_prioritization: &str,
    gene_selection: &str,
    candidate_count: u32,
) -> String {
    let path = directory.path().join(format!("{name}-strategy.json"));
    fs::write(
        &path,
        format!(
            r#"{{"schema_version":1,"name":"{name}","mutation_prioritization":"{mutation_prioritization}","generation_count":1,"experiment_allocation":{},"candidate_count":{candidate_count},"gene_selection":"{gene_selection}"}}"#,
            TRIALS_PER_GENERATION + u64::from(candidate_count.saturating_sub(1))
        ),
    )
    .expect("write Evolver strategy");
    let Some(ResponseData::MetaStrategy { strategy }) = dispatch_call(
        plane,
        token,
        &format!("{name}-register"),
        Command::MetaStrategyRegister {
            path: path.display().to_string(),
        },
    )
    .data
    else {
        panic!("Evolver strategy registration should succeed");
    };
    strategy.strategy_id
}

/// Roadmap items 8, 10, 13: one strategy-driven `evolve` run per named
/// Gauntlet mode, seeding the mode's bad Genome as Champion and its fix
/// Genome as the fixed diagnostic baseline (exactly like
/// `gauntlet_failure_modes_reject_the_bad_operation_and_pass_the_fix`'s
/// fixture). Generation zero's diagnostic evaluation surfaces the bad
/// operation's failures; `failure-cluster-v2` recognizes the Champion's
/// current operation as that mode's known-bad operation and every cluster
/// suggests the paired fix regardless of shape signature; the strategy's
/// Fifo prioritization picks it; Forge proposes it as an analysis-bound
/// catalog edge (`World mutation scope` now authorizes it); the child scores
/// full correctness against the bad Champion and is promoted through the
/// exact unmodified Arena/selection/invariant/Champion policy. This is the
/// first proof that `evolve` can discover a Gauntlet fix on its own, not
/// just replay an operator-supplied one (see `examples/gauntlet/README.md`).
#[test]
#[allow(clippy::too_many_lines)]
fn evolve_promotes_the_fix_for_every_gauntlet_mode_from_the_bundled_fixtures() {
    for (mode, task_input, expected_output, bad_operation, good_operation) in [
        (
            "context-loss",
            r#"{"turns":["FACT: the deploy key is banana","small talk","more small talk","what is the deploy key?"]}"#,
            " the deploy key is banana",
            "context_loss_naive",
            "context_loss_aware",
        ),
        (
            "premature-completion",
            r#"{"steps":["step1:DONE_A","step2:DONE_B","step3:DONE_C"]}"#,
            "DONE_A,DONE_B,DONE_C",
            "premature_completion",
            "verified_completion",
        ),
        (
            "schema-drift",
            r#"{"schema_version":2,"field_v1":null,"field_v2":"correct-value"}"#,
            "correct-value",
            "schema_drift_brittle",
            "schema_drift_adaptive",
        ),
        (
            "bad-routing",
            r#"{"requires_capability":"large_context","routes":[{"name":"cheap","capability":"small","cost":1},{"name":"expensive","capability":"large_context","cost":9}]}"#,
            "expensive",
            "bad_routing_cheapest",
            "capability_aware_routing",
        ),
        (
            "duplicate-subagents",
            r#"{"requests":["task-a","task-a","task-b"]}"#,
            "task-a,task-b",
            "duplicate_subagents_wasteful",
            "deduplicated_subagents",
        ),
        (
            "poisoned-memory",
            r#"{"memory":[{"text":"correct-fact","trusted":true},{"text":"malicious-fact","trusted":false}]}"#,
            "correct-fact",
            "poisoned_memory_trusting",
            "provenance_checked_memory",
        ),
        (
            "hallucinated-verification",
            r#"{"claimed_output":"success-value","claimed_status":"success","actual_state":"actual-value"}"#,
            "actual-value",
            "hallucinated_verification_trusting",
            "ground_truth_verification",
        ),
    ] {
        let directory = tempdir().expect("Gauntlet evolve fixture directory");
        let (mut plane, parent, _candidate) = real_worker_gauntlet_fixture(
            &directory,
            mode,
            task_input,
            expected_output,
            bad_operation,
            good_operation,
        );
        let token = plane.token_hex.clone();
        let strategy_id =
            register_test_strategy(&mut plane, &token, &directory, mode, "fifo", "none");

        let run_id = format!("evolve-gauntlet-{mode}");
        let start = dispatch_call(
            &mut plane,
            &token,
            &format!("{run_id}-start"),
            evolve_start_command_with_strategy(
                &run_id,
                &parent.world_id,
                &parent.genome_id,
                1,
                TRIALS_PER_GENERATION,
                Some(&strategy_id),
            ),
        );
        assert!(
            start.error.is_none(),
            "evolve start should succeed for {mode}: {:?}",
            start.error
        );

        let run = evolve_drain_active_run(&mut plane, &run_id);
        assert_eq!(
            run.finish_reason,
            Some(EvolutionFinishReason::GenerationsExhausted),
            "evolve run for {mode} should complete its one generation"
        );
        assert_eq!(
            run.generations.len(),
            1,
            "evolve run for {mode} should record exactly one generation"
        );
        let generation = &run.generations[0].payload;
        assert!(
            generation.promoted,
            "evolve should have discovered and promoted {mode}'s fix on its own"
        );
        assert_eq!(generation.champion_after, generation.child_genome_id);
        let promoted_operation = plane
            .reference_instruction(&generation.child_genome_id)
            .expect("promoted child should have a readable reference operation");
        assert_eq!(
            promoted_operation.map(ReferenceInstruction::operation_name),
            Some(good_operation),
            "the promoted child for {mode} should carry the paired fix operation"
        );

        assert!(matches!(
            plane.replay_response(),
            Ok(ResponseData::Replay { .. })
        ));
    }
}

/// A strategy-bound run whose Champion runs an operation with no supported
/// mutation (a Gauntlet "fix" operation, which never suggests a regression)
/// and no failing clusters to derive one from finishes with
/// `NoCandidateMutation` instead of proposing anything.
#[test]
fn evolve_strategy_run_with_no_candidate_mutation_finishes_without_a_generation() {
    let directory = tempdir().expect("Gauntlet evolve fixture directory");
    let (mut plane, _parent, candidate) = real_worker_gauntlet_fixture(
        &directory,
        "context-loss",
        r#"{"turns":["FACT: the deploy key is banana","small talk","more small talk","what is the deploy key?"]}"#,
        " the deploy key is banana",
        "context_loss_naive",
        "context_loss_aware",
    );
    let token = plane.token_hex.clone();
    let strategy_id =
        register_test_strategy(&mut plane, &token, &directory, "no-op", "fifo", "none");

    // Seed the *fix* Genome as Champion (instead of the bad one): it answers
    // the diagnostic task correctly, so its evaluation has no failed trials
    // at all (an empty cluster list) — and even if it had failed some,
    // `failure-cluster-v2` never suggests a regression for a Gauntlet fix
    // operation. Either way no cluster suggests anything, and the Champion
    // isn't the casing pair either.
    let run_id = "evolve-gauntlet-no-candidate";
    let start = dispatch_call(
        &mut plane,
        &token,
        "no-candidate-start",
        evolve_start_command_with_strategy(
            run_id,
            &candidate.world_id,
            &candidate.genome_id,
            1,
            TRIALS_PER_GENERATION,
            Some(&strategy_id),
        ),
    );
    assert!(
        start.error.is_none(),
        "evolve start should succeed: {:?}",
        start.error
    );

    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(
        run.finish_reason,
        Some(EvolutionFinishReason::NoCandidateMutation)
    );
    assert!(run.generations.is_empty());
    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
}

/// TD-17 (roadmap items 10, 13): a `candidate_count == 2` strategy proposes
/// two distinct, ranked candidates from the Champion's failure clusters --
/// the context-loss Gauntlet mode's family fix (`context_loss_aware`, rank
/// `0`, from every cluster's shape-independent primary suggestion) and the
/// `sealed_incorrect_output` cluster's secondary exploratory suggestion
/// (`ascii_uppercase`, rank `1`) -- consumes `1 + 2 = 3` trials, and promotes
/// the highest-ranked `metrics_passed` candidate (the genuine fix; the
/// casing flip does not address context loss and never passes correctness).
#[test]
#[allow(clippy::too_many_lines)]
fn evolve_candidate_count_two_evaluates_both_ranked_candidates_and_promotes_the_better_one() {
    let directory = tempdir().expect("Gauntlet evolve fixture directory");
    let (mut plane, parent, _candidate) = real_worker_gauntlet_fixture(
        &directory,
        "context-loss",
        r#"{"turns":["FACT: the deploy key is banana","small talk","more small talk","what is the deploy key?"]}"#,
        " the deploy key is banana",
        "context_loss_naive",
        "context_loss_aware",
    );
    let token = plane.token_hex.clone();
    let strategy_id = register_test_strategy_with_candidate_count(
        &mut plane,
        &token,
        &directory,
        "two-candidates",
        "fifo",
        "none",
        2,
    );

    let run_id = "evolve-two-candidates";
    let start = dispatch_call(
        &mut plane,
        &token,
        "evolve-two-candidates-start",
        evolve_start_command_with_strategy(
            run_id,
            &parent.world_id,
            &parent.genome_id,
            1,
            TRIALS_PER_GENERATION + 1,
            Some(&strategy_id),
        ),
    );
    assert!(
        start.error.is_none(),
        "evolve start should succeed: {:?}",
        start.error
    );

    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(
        run.finish_reason,
        Some(EvolutionFinishReason::GenerationsExhausted)
    );
    assert_eq!(run.generations.len(), 1);
    assert_eq!(
        run.trials_consumed,
        TRIALS_PER_GENERATION + 1,
        "one diagnostic trial plus one trial per proposed candidate"
    );

    let generation = &run.generations[0].payload;
    assert_eq!(
        generation.candidates.len(),
        2,
        "both ranked candidates should have been proposed and assessed"
    );
    assert_eq!(generation.candidates[0].rank, 0);
    assert_eq!(generation.candidates[1].rank, 1);

    let operation_of = |genome_id: &str| {
        plane
            .reference_instruction(genome_id)
            .expect("readable reference operation")
            .map(ReferenceInstruction::operation_name)
    };
    assert_eq!(
        operation_of(&generation.candidates[0].child_genome_id),
        Some("context_loss_aware"),
        "rank 0 is the family fix every cluster suggests"
    );
    assert_eq!(
        operation_of(&generation.candidates[1].child_genome_id),
        Some("ascii_uppercase"),
        "rank 1 is the sealed cluster's distinct secondary suggestion"
    );
    assert_eq!(
        generation.candidates[0].outcome,
        ForgeAssessmentOutcome::MetricsPassed,
        "the genuine fix should pass correctness"
    );
    assert_ne!(
        generation.candidates[1].outcome,
        ForgeAssessmentOutcome::MetricsPassed,
        "uppercasing the wrong answer never fixes context loss"
    );

    assert!(
        generation.promoted,
        "the highest-ranked metrics_passed candidate should be promoted"
    );
    assert_eq!(
        generation.champion_after,
        generation.candidates[0].child_genome_id
    );
    assert_eq!(generation.proposal_id, generation.candidates[0].proposal_id);
    assert_eq!(
        generation.child_genome_id,
        generation.candidates[0].child_genome_id
    );
    assert_eq!(
        generation.assessment_id,
        generation.candidates[0].assessment_id
    );

    let history = plane
        .storage
        .as_ref()
        .expect("canonical ledger")
        .ledger
        .replay_verified()
        .expect("verified history");
    verify_evolution_history(&history, &plane.state.registered)
        .expect("canonical multi-candidate evolution history");
    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
}

/// TD-17 (roadmap items 10, 13): a `candidates` list that claims a
/// non-highest-ranked (or non-passing) candidate was promoted, without
/// actually promoting it through the Champion, must fail replay -- this is
/// exactly the tamper `verify_evolution_history`'s new candidate
/// cross-check exists to catch.
#[test]
fn evolve_history_replay_rejects_a_forged_candidates_list() {
    let directory = tempdir().expect("Gauntlet evolve fixture directory");
    let (mut plane, parent, _candidate) = real_worker_gauntlet_fixture(
        &directory,
        "context-loss",
        r#"{"turns":["FACT: the deploy key is banana","small talk","more small talk","what is the deploy key?"]}"#,
        " the deploy key is banana",
        "context_loss_naive",
        "context_loss_aware",
    );
    let token = plane.token_hex.clone();
    let strategy_id = register_test_strategy_with_candidate_count(
        &mut plane,
        &token,
        &directory,
        "two-candidates-forged",
        "fifo",
        "none",
        2,
    );

    let run_id = "evolve-forged-candidates";
    let start = dispatch_call(
        &mut plane,
        &token,
        "evolve-forged-candidates-start",
        evolve_start_command_with_strategy(
            run_id,
            &parent.world_id,
            &parent.genome_id,
            1,
            TRIALS_PER_GENERATION + 1,
            Some(&strategy_id),
        ),
    );
    assert!(start.error.is_none());
    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(run.generations.len(), 1);
    let genuine = run.generations[0].clone();
    assert!(genuine.payload.promoted);
    assert_eq!(genuine.payload.candidates.len(), 2);
    assert_eq!(
        genuine.payload.candidates[1].outcome,
        ForgeAssessmentOutcome::MetricsRejected,
        "the second candidate must genuinely have failed for this forgery to be meaningful"
    );

    // Rewrite the recorded generation event's payload so its top-level
    // fields describe the second (non-passing, non-highest-ranked)
    // candidate while keeping `promoted: true` and the real, unmodified
    // candidates list -- an attacker cannot simply claim a different
    // candidate won without also editing the evidence it names.
    let history = plane
        .storage
        .as_ref()
        .expect("canonical ledger")
        .ledger
        .replay_verified()
        .expect("verified history");
    let generation_event_id = evolution_generation_event_id(run_id, 0);
    let forged = EvolutionGenerationPayload {
        proposal_id: genuine.payload.candidates[1].proposal_id.clone(),
        child_genome_id: genuine.payload.candidates[1].child_genome_id.clone(),
        child_evaluation_id: genuine.payload.candidates[1].child_evaluation_id.clone(),
        assessment_id: genuine.payload.candidates[1].assessment_id.clone(),
        champion_after: genuine.payload.candidates[1].child_genome_id.clone(),
        ..genuine.payload.clone()
    };
    assert_ne!(forged, genuine.payload);
    let payload_value = serde_json::to_value(&forged).expect("encode forged payload");
    let payload_bytes = serde_json::to_vec(&payload_value).expect("serialize forged payload");
    let forged_history: Vec<StoredEvent> = history
        .iter()
        .cloned()
        .map(|mut event| {
            if event.event_id == generation_event_id {
                event.payload = payload_bytes.clone();
            }
            event
        })
        .collect();

    assert!(
        verify_evolution_history(&forged_history, &plane.state.registered).is_err(),
        "a candidates list must not let the top-level fields describe a non-highest-ranked, \
         non-promoted candidate"
    );
}

// ---------------------------------------------------------------------------
// Autonomous evolution (roadmap item 10). `real_worker_arena_fixture` already
// registers a World plus two Genomes ("arena-parent" and its child
// "arena-candidate", both the identity operation): `evolve_start_command`
// seeds the parent as generation zero's Champion and the daemon picks the
// other Genome as the fixed diagnostic baseline.
// ---------------------------------------------------------------------------

fn evolve_start_command(
    run_id: &str,
    world_id: &str,
    from_genome_id: &str,
    generations: u32,
    budget: u64,
) -> Command {
    evolve_start_command_with_strategy(run_id, world_id, from_genome_id, generations, budget, None)
}

fn evolve_start_command_with_strategy(
    run_id: &str,
    world_id: &str,
    from_genome_id: &str,
    generations: u32,
    budget: u64,
    strategy_id: Option<&str>,
) -> Command {
    Command::EvolveStart {
        run_id: run_id.to_owned(),
        world_id: world_id.to_owned(),
        from_genome_id: from_genome_id.to_owned(),
        generations,
        budget,
        strategy_id: strategy_id.map(str::to_owned),
    }
}

/// Polls the reconciliation loop the same way `drain_active_arena_test_job`
/// polls one Arena job, until the named run reaches a terminal state.
fn evolve_drain_active_run(plane: &mut ControlPlane, run_id: &str) -> EvolutionRunRecord {
    // Each generation drives two full paired Arena evaluations through the
    // real supervised worker/evaluator subprocesses; a three-generation run
    // needs meaningfully more wall-clock time than one Arena job alone.
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        plane
            .service_async_messages()
            .expect("advance the evolution reconciliation loop");
        let history = plane
            .storage
            .as_ref()
            .expect("open canonical storage")
            .ledger
            .replay_verified()
            .expect("replay verified history");
        if let Some(run) = evolution_projection(&history, run_id).expect("evolution projection")
            && run.state == EvolutionRunState::Finished
        {
            return run;
        }
        assert!(Instant::now() < deadline, "evolution run did not finish");
        thread::sleep(Duration::from_millis(2));
    }
}

fn evolve_run_projection(plane: &ControlPlane, run_id: &str) -> EvolutionRunRecord {
    let history = plane
        .storage
        .as_ref()
        .expect("open canonical storage")
        .ledger
        .replay_verified()
        .expect("replay verified history");
    evolution_projection(&history, run_id)
        .expect("evolution projection")
        .expect("evolution run exists")
}

fn evolve_reopen_real_worker_fixture(directory: &TempDir) -> Result<ControlPlane, ControlError> {
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    let evaluator = directory.path().join("fixture-evaluator");
    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
}

#[test]
#[allow(clippy::too_many_lines)]
fn evolve_start_completes_three_generations_with_one_promotion_and_replays() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent, _candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();
    let from_genome_id = parent.genome_id.clone();
    let run_id = "evolve-three-generations";

    let start = dispatch_call(
        &mut plane,
        &token,
        "evolve-start",
        evolve_start_command(run_id, &world_id, &from_genome_id, 3, 6),
    );
    assert!(
        start.error.is_none(),
        "evolve start failed: {:?}",
        start.error
    );
    let Some(ResponseData::Evolution { run }) = start.data else {
        panic!("evolve start should return the admitted run");
    };
    assert_eq!(run.state, EvolutionRunState::Running);
    assert_eq!(run.max_generations, 3);
    assert_eq!(run.max_paired_trials, 6);
    assert_eq!(run.from_genome_id, from_genome_id);
    assert_ne!(run.baseline_genome_id, from_genome_id);

    // Re-admitting the exact same request is idempotent.
    let repeat = dispatch_call(
        &mut plane,
        &token,
        "evolve-start-repeat",
        evolve_start_command(run_id, &world_id, &from_genome_id, 3, 6),
    );
    assert!(repeat.error.is_none());

    // While one run is active, an operator cannot race its own internal
    // Arena/Forge/Champion calls through the ordinary command surface.
    let busy = dispatch_call(
        &mut plane,
        &token,
        "evolve-busy-probe",
        Command::ChampionSeed {
            transition_id: "should-not-seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: from_genome_id.clone(),
            reason: "operator race attempt".to_owned(),
        },
    );
    assert_eq!(busy.error.expect("busy response").code, ApiErrorCode::Busy);

    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(run.state, EvolutionRunState::Finished);
    assert_eq!(
        run.finish_reason,
        Some(EvolutionFinishReason::GenerationsExhausted)
    );
    assert_eq!(run.generations.len(), 3);
    assert_eq!(run.trials_consumed, 6);
    assert!(
        run.generations[0].payload.promoted,
        "the first generation's mutation genuinely improves on the identity Champion"
    );
    assert!(
        !run.generations[1].payload.promoted,
        "flipping the same operation back is never an improvement"
    );
    assert!(!run.generations[2].payload.promoted);
    // TD-17 (roadmap items 10, 13): a strategy-less run always proposes
    // exactly one candidate per generation, exactly like before
    // multi-candidate generations existed; `candidates` stays empty and is
    // omitted from the canonical payload bytes so every previously recorded
    // generation still replays byte-for-byte.
    for generation in &run.generations {
        assert!(
            generation.payload.candidates.is_empty(),
            "a strategy-less run's generation never records more than one candidate"
        );
        let canonical = serde_json::to_value(&generation.payload)
            .expect("encode generation payload")
            .as_object()
            .expect("generation payload is a JSON object")
            .clone();
        assert!(
            !canonical.contains_key("candidates"),
            "single-candidate generation bytes must stay identical to before candidates existed"
        );
    }
    assert_eq!(
        run.generations[1].payload.champion_before,
        run.generations[0].payload.champion_after
    );
    assert_eq!(
        run.generations[2].payload.champion_before,
        run.generations[1].payload.champion_after
    );

    let Some(ResponseData::Champion { champion }) = dispatch_call(
        &mut plane,
        &token,
        "evolve-champion-show",
        Command::ChampionShow {
            world_id: world_id.clone(),
        },
    )
    .data
    else {
        panic!("champion show should succeed");
    };
    assert_eq!(
        champion.champion_genome_id.as_deref(),
        Some(run.generations[0].payload.child_genome_id.as_str())
    );

    // Once active work has drained, the run no longer blocks ordinary commands.
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "evolve-after-finish-status",
            Command::Status,
        )
        .error
        .is_none()
    );

    let replay = dispatch_call(&mut plane, &token, "evolve-replay", Command::Replay);
    assert!(replay.error.is_none(), "replay failed: {:?}", replay.error);

    // A generation that claims a promotion must be backed by the matching
    // Champion promotion; without it, evolution history no longer verifies.
    let history = plane
        .storage
        .as_ref()
        .expect("canonical ledger")
        .ledger
        .replay_verified()
        .expect("verified history");
    verify_evolution_history(&history, &plane.state.registered)
        .expect("canonical evolution history");
    let promoted_child = run.generations[0].payload.child_genome_id.clone();
    let without_promotion: Vec<StoredEvent> = history
        .iter()
        .filter(|event| {
            super::champion::decode_champion_transition(event).map_or(true, |transition| {
                transition.kind != ChampionTransitionKind::Promoted
                    || transition.champion_genome_id != promoted_child
            })
        })
        .cloned()
        .collect();
    assert_eq!(without_promotion.len(), history.len() - 1);
    assert!(
        verify_evolution_history(&without_promotion, &plane.state.registered).is_err(),
        "a promotion claim without its Champion promotion must fail replay"
    );
}

/// Proves `hephaestus evolve coding`'s exact prerequisite sequence (the CLI
/// convenience runs these same commands against a live daemon over its
/// socket; this drives the identical Commands in-process against the real
/// bundled `examples/gauntlet/coding` fixture files on disk, per SPEED MODE
/// guidance to skip a daemon-process E2E). It registers the bundled World
/// and its two reference Genomes exactly as the CLI does, starts a
/// 3-generation run, and drains it to completion.
#[test]
#[allow(clippy::too_many_lines)]
fn evolve_coding_bundled_world_completes_three_generations_from_the_example_fixture() {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gauntlet/coding");
    assert!(
        examples_dir.is_dir(),
        "bundled examples/gauntlet/coding fixture is missing: {examples_dir:?}"
    );

    let directory = tempdir().expect("daemon directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"Gauntlet coding fixture\n")
        .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);
    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    let evaluator_path = directory.path().join("fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator_path).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator_path,
        &worker,
    )
    .expect("open coding fixture");
    let token = plane.token_hex.clone();
    assert!(
        dispatch_call(&mut plane, &token, "coding-unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    let artifact_id_of = |response: ApiResponse| match response.data {
        Some(ResponseData::Artifact { artifact_id, .. }) => artifact_id,
        other => panic!("expected an Artifact response, got {other:?}"),
    };
    let visible_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "coding-visible-manifest",
        Command::ManifestPut {
            path: examples_dir
                .join("tasks/visible.json")
                .display()
                .to_string(),
        },
    ));
    let sealed_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "coding-sealed-manifest",
        Command::ManifestPut {
            path: examples_dir.join("tasks/sealed.json").display().to_string(),
        },
    ));
    let evaluator_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "coding-evaluator",
        Command::ArtifactPut {
            path: evaluator_path.display().to_string(),
        },
    ));
    let Some(ResponseData::Verifier {
        artifact_id: verifier_id,
        ..
    }) = dispatch_call(&mut plane, &token, "coding-verifier", Command::VerifierShow).data
    else {
        panic!("verifier show should succeed");
    };
    let invariants_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "coding-invariants",
        Command::ArtifactPut {
            path: examples_dir.join("invariants.json").display().to_string(),
        },
    ));

    let world_template =
        fs::read_to_string(examples_dir.join("world.template.json")).expect("read world template");
    let world_json = world_template
        .replace("__VISIBLE_MANIFEST__", &visible_id)
        .replace("__SEALED_MANIFEST__", &sealed_id)
        .replace("__EVALUATOR__", &evaluator_id)
        .replace("__VERIFIER__", &verifier_id)
        .replace("__INVARIANTS__", &invariants_id);
    let world_path = directory.path().join("gauntlet-coding-world.json");
    fs::write(&world_path, world_json).expect("write coding World");
    let Some(ResponseData::World { world }) = dispatch_call(
        &mut plane,
        &token,
        "coding-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("World registration should succeed");
    };

    let Some(ResponseData::Genome { genome: parent }) = dispatch_call(
        &mut plane,
        &token,
        "coding-parent",
        Command::GenomeRegister {
            path: examples_dir.join("parent.md").display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("parent Genome registration should succeed");
    };
    let candidate_template =
        fs::read_to_string(examples_dir.join("candidate.md")).expect("read candidate Genome");
    let candidate_path = directory.path().join("gauntlet-coding-candidate.md");
    fs::write(
        &candidate_path,
        candidate_template.replace("__PARENT_ID__", &parent.genome_id),
    )
    .expect("write coding candidate Genome");
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "coding-candidate",
            Command::GenomeRegister {
                path: candidate_path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .error
        .is_none()
    );

    let run_id = "gauntlet-coding";
    let start = dispatch_call(
        &mut plane,
        &token,
        "coding-evolve-start",
        evolve_start_command(run_id, &world.world_id, &parent.genome_id, 3, 6),
    );
    assert!(
        start.error.is_none(),
        "evolve start failed: {:?}",
        start.error
    );

    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(run.state, EvolutionRunState::Finished);
    assert_eq!(
        run.finish_reason,
        Some(EvolutionFinishReason::GenerationsExhausted)
    );
    assert_eq!(
        run.generations.len(),
        3,
        "evolve coding must complete at least three unattended generations"
    );
    assert_eq!(run.trials_consumed, 6);
}

/// Roadmap item 10: "a sealed holdout shows statistically supported
/// improvement within enforced budget." Reads the bundled
/// `examples/gauntlet/sealed-holdout-improvement` fixture files exactly like
/// `evolve_coding_bundled_world_completes_three_generations_from_the_example_fixture`
/// does, registers the World and both Genomes, seeds the bad
/// `poisoned_memory_trusting` Genome as generation zero's Champion, binds a
/// minimal Evolver strategy, and starts a one-generation, budget-2
/// strategy-bound `evolve` run. The candidate's paired evidence spans all 3
/// visible plus 8 sealed poisoned-memory scenarios (distinct content per
/// scenario): the bad operation fails every one and the fix
/// (`provenance_checked_memory`) passes every one, so the run promotes the
/// fix within budget, the child's selection receipt reports a
/// zero-regression, 11-improvement histogram bootstrap whose lower
/// confidence bound clears zero at the World's 95% confidence with
/// `metrics_eligible=true`, the sealed subset alone (never shown to the
/// candidate) moved from 0/8 to 8/8 correct, replay verifies, and an
/// independent Python recompute of the exact same receipt agrees.
#[test]
#[allow(clippy::too_many_lines)]
fn sealed_holdout_improvement_is_statistically_supported_and_promoted_within_budget() {
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/gauntlet/sealed-holdout-improvement");
    assert!(
        examples_dir.is_dir(),
        "bundled examples/gauntlet/sealed-holdout-improvement fixture is missing: {examples_dir:?}"
    );

    let directory = tempdir().expect("daemon directory");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(
        repository.join("fixture.txt"),
        b"Sealed holdout improvement fixture\n",
    )
    .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);
    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    let evaluator_path = directory.path().join("fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator_path).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator_path, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator_path,
        &worker,
    )
    .expect("open sealed-holdout-improvement fixture");
    let token = plane.token_hex.clone();
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "sealed-holdout-unfreeze",
            Command::Unfreeze
        )
        .error
        .is_none()
    );

    let artifact_id_of = |response: ApiResponse| match response.data {
        Some(ResponseData::Artifact { artifact_id, .. }) => artifact_id,
        other => panic!("expected an Artifact response, got {other:?}"),
    };
    let visible_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-visible-manifest",
        Command::ManifestPut {
            path: examples_dir
                .join("tasks/visible.json")
                .display()
                .to_string(),
        },
    ));
    let sealed_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-sealed-manifest",
        Command::ManifestPut {
            path: examples_dir.join("tasks/sealed.json").display().to_string(),
        },
    ));
    let evaluator_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-evaluator",
        Command::ArtifactPut {
            path: evaluator_path.display().to_string(),
        },
    ));
    let Some(ResponseData::Verifier {
        artifact_id: verifier_id,
        ..
    }) = dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-verifier",
        Command::VerifierShow,
    )
    .data
    else {
        panic!("verifier show should succeed");
    };
    let invariants_id = artifact_id_of(dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-invariants",
        Command::ArtifactPut {
            path: examples_dir.join("invariants.json").display().to_string(),
        },
    ));

    let world_template =
        fs::read_to_string(examples_dir.join("world.template.json")).expect("read world template");
    let world_json = world_template
        .replace("__VISIBLE_MANIFEST__", &visible_id)
        .replace("__SEALED_MANIFEST__", &sealed_id)
        .replace("__EVALUATOR__", &evaluator_id)
        .replace("__VERIFIER__", &verifier_id)
        .replace("__INVARIANTS__", &invariants_id);
    let world_path = directory
        .path()
        .join("sealed-holdout-improvement-world.json");
    fs::write(&world_path, world_json).expect("write sealed-holdout-improvement World");
    let Some(ResponseData::World { world }) = dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("World registration should succeed");
    };

    let Some(ResponseData::Genome { genome: parent }) = dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-parent",
        Command::GenomeRegister {
            path: examples_dir.join("parent.md").display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("parent Genome registration should succeed");
    };
    let candidate_template =
        fs::read_to_string(examples_dir.join("candidate.md")).expect("read candidate Genome");
    let candidate_path = directory
        .path()
        .join("sealed-holdout-improvement-candidate.md");
    fs::write(
        &candidate_path,
        candidate_template.replace("__PARENT_ID__", &parent.genome_id),
    )
    .expect("write sealed-holdout-improvement candidate Genome");
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "sealed-holdout-candidate",
            Command::GenomeRegister {
                path: candidate_path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .error
        .is_none()
    );

    let strategy_id = register_test_strategy(
        &mut plane,
        &token,
        &directory,
        "sealed-holdout-improvement",
        "fifo",
        "none",
    );

    let run_id = "sealed-holdout-improvement";
    let start = dispatch_call(
        &mut plane,
        &token,
        "sealed-holdout-evolve-start",
        evolve_start_command_with_strategy(
            run_id,
            &world.world_id,
            &parent.genome_id,
            1,
            TRIALS_PER_GENERATION,
            Some(&strategy_id),
        ),
    );
    assert!(
        start.error.is_none(),
        "evolve start failed: {:?}",
        start.error
    );

    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(
        run.finish_reason,
        Some(EvolutionFinishReason::GenerationsExhausted)
    );
    assert_eq!(run.generations.len(), 1);
    assert!(
        run.trials_consumed <= TRIALS_PER_GENERATION,
        "the run must finish within its enforced budget: consumed {} of {}",
        run.trials_consumed,
        TRIALS_PER_GENERATION
    );

    let generation = &run.generations[0].payload;
    assert!(
        generation.promoted,
        "the fix should be discovered and promoted from its own visible+sealed evidence"
    );
    assert_eq!(generation.champion_after, generation.child_genome_id);
    let promoted_operation = plane
        .reference_instruction(&generation.child_genome_id)
        .expect("promoted child should have a readable reference operation");
    assert_eq!(
        promoted_operation.map(ReferenceInstruction::operation_name),
        Some("provenance_checked_memory"),
        "the promoted child should carry the poisoned-memory fix"
    );

    // The child's selection receipt: paired evidence over all 3 visible plus
    // 8 sealed tasks is a clean, zero-regression, 11-improvement histogram
    // whose bootstrap lower bound clears the World's zero-delta policy at
    // 95% confidence.
    let ResponseData::Selection { selection } = plane
        .select_arena_evaluation(&generation.child_evaluation_id)
        .expect("selection over the promoted child's evidence should succeed")
    else {
        panic!("select_arena_evaluation should return a Selection response");
    };
    assert_eq!(selection.receipt.correctness_regressions(), 0);
    assert_eq!(selection.receipt.correctness_unchanged(), 0);
    assert_eq!(
        selection.receipt.correctness_improvements(),
        11,
        "3 visible + 8 sealed poisoned-memory scenarios should all register as improvements"
    );
    assert!(
        selection.receipt.lower_bps() > 0,
        "the bootstrap lower confidence bound should clear zero: {}",
        selection.receipt.lower_bps()
    );
    assert!(selection.receipt.metrics_eligible());

    // The sealed subset alone (never shown to the candidate during
    // development) moved from 0 correct to all correct, without leaking any
    // sealed task content into this assertion.
    let arena_stores = plane
        .open_arena_stores()
        .expect("open Arena stores for a read-only operator scores check");
    let operator = load_operator_evaluation(arena_stores, &generation.child_evaluation_id)
        .expect("load the promoted child's operator evaluation");
    let scores = operator.operator_scores();
    assert_eq!(scores.visible_total, 3);
    assert_eq!(scores.sealed_total, 8);
    assert_eq!(scores.parent_visible_correct, 0);
    assert_eq!(scores.candidate_visible_correct, 3);
    assert_eq!(
        scores.parent_sealed_correct, 0,
        "the bad operation should fail every sealed task"
    );
    assert_eq!(
        scores.candidate_sealed_correct, 8,
        "the fix should pass every held-out sealed task"
    );

    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));

    // Independent recompute: write the exact receipt to disk and check that
    // hephaestus_lab.crosscheck agrees, exactly as a human operator's own
    // audit would. This is part of the "statistically supported" claim, not
    // decoration: the Rust bootstrap and Python's independent reimplementation
    // must agree on the same recorded evidence.
    let receipt_path = directory
        .path()
        .join("sealed-holdout-improvement-receipt.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&selection.receipt).expect("receipt serializes"),
    )
    .expect("write selection receipt for cross-check");
    let python_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python");
    match ProcessCommand::new("python3")
        .arg("-m")
        .arg("hephaestus_lab.crosscheck")
        .arg(&receipt_path)
        .arg("--kind")
        .arg("selection")
        .env("PYTHONPATH", &python_dir)
        .output()
    {
        Ok(output) => {
            assert!(
                output.status.success(),
                "independent Python cross-check of the receipt should agree: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping independent Python cross-check: python3 is not available on this \
                 machine ({error})"
            );
        }
        Err(error) => panic!("failed to run python3 cross-check: {error}"),
    }
}

#[test]
fn evolve_respects_freeze_and_resumes_only_after_explicit_unfreeze() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent, _candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();
    let from_genome_id = parent.genome_id.clone();
    let run_id = "evolve-freeze";

    let start = dispatch_call(
        &mut plane,
        &token,
        "evolve-start",
        evolve_start_command(run_id, &world_id, &from_genome_id, 2, 4),
    );
    assert!(start.error.is_none());

    assert!(
        dispatch_call(&mut plane, &token, "evolve-freeze", Command::Freeze)
            .error
            .is_none()
    );
    for _ in 0..25 {
        plane
            .service_async_messages()
            .expect("tick the reconciliation loop while frozen");
    }
    let frozen_run = evolve_run_projection(&plane, run_id);
    assert_eq!(
        frozen_run.generations.len(),
        0,
        "a frozen daemon never advances an evolution run"
    );
    assert_eq!(frozen_run.state, EvolutionRunState::Running);

    assert!(
        dispatch_call(&mut plane, &token, "evolve-unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(run.state, EvolutionRunState::Finished);
    assert_eq!(
        run.finish_reason,
        Some(EvolutionFinishReason::GenerationsExhausted)
    );
    assert_eq!(run.generations.len(), 2);
}

#[test]
fn evolve_cancel_stops_the_run_before_any_generation_is_admitted() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent, _candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();
    let from_genome_id = parent.genome_id.clone();
    let run_id = "evolve-cancel";

    let start = dispatch_call(
        &mut plane,
        &token,
        "evolve-start",
        evolve_start_command(run_id, &world_id, &from_genome_id, 5, 10),
    );
    assert!(start.error.is_none());

    let cancel = dispatch_call(
        &mut plane,
        &token,
        "evolve-cancel",
        Command::EvolveCancel {
            run_id: run_id.to_owned(),
        },
    );
    assert!(cancel.error.is_none());
    let Some(ResponseData::Evolution { run }) = cancel.data else {
        panic!("evolve cancel should return the run");
    };
    assert!(run.cancel_requested);

    // Cancelling an already-cancelled run is a harmless idempotent no-op.
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "evolve-cancel-repeat",
            Command::EvolveCancel {
                run_id: run_id.to_owned(),
            },
        )
        .error
        .is_none()
    );

    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(run.state, EvolutionRunState::Finished);
    assert_eq!(run.finish_reason, Some(EvolutionFinishReason::Cancelled));
    assert_eq!(run.generations.len(), 0);
}

#[test]
fn evolve_budget_exhaustion_stops_a_run_before_its_generation_limit() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent, _candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();
    let from_genome_id = parent.genome_id.clone();
    let run_id = "evolve-budget";

    let start = dispatch_call(
        &mut plane,
        &token,
        "evolve-start",
        evolve_start_command(run_id, &world_id, &from_genome_id, 5, TRIALS_PER_GENERATION),
    );
    assert!(start.error.is_none());

    let run = evolve_drain_active_run(&mut plane, run_id);
    assert_eq!(run.state, EvolutionRunState::Finished);
    assert_eq!(
        run.finish_reason,
        Some(EvolutionFinishReason::BudgetExhausted)
    );
    assert_eq!(run.generations.len(), 1);
    assert_eq!(run.trials_consumed, TRIALS_PER_GENERATION);
}

#[test]
fn evolve_history_rejects_a_generation_forged_without_matching_evidence() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent, candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();
    let from_genome_id = parent.genome_id.clone();
    let run_id = "evolve-forged";

    let start = dispatch_call(
        &mut plane,
        &token,
        "evolve-start",
        evolve_start_command(run_id, &world_id, &from_genome_id, 3, 6),
    );
    assert!(start.error.is_none());

    // No proposal, child evaluation, or assessment for this generation was
    // ever recorded; this event claims a promotion the ledger cannot support.
    let forged = EvolutionGenerationPayload {
        schema_version: 1,
        run_id: run_id.to_owned(),
        generation_index: 0,
        champion_before: from_genome_id.clone(),
        diagnostic_evaluation_id: "forged-diagnostic".to_owned(),
        proposal_id: "forged-proposal".to_owned(),
        child_genome_id: candidate.genome_id.clone(),
        child_evaluation_id: "forged-child".to_owned(),
        assessment_id: "forged-assessment".to_owned(),
        promoted: true,
        champion_after: candidate.genome_id.clone(),
        candidates: Vec::new(),
    };
    let payload_value = serde_json::to_value(&forged).expect("encode forged payload");
    let payload_bytes = serde_json::to_vec(&payload_value).expect("serialize forged payload");
    plane
        .storage
        .as_mut()
        .expect("open canonical storage")
        .ledger
        .append(EventInput::new(
            evolution_generation_event_id(run_id, 0),
            evolution_aggregate_id(run_id),
            EVOLUTION_GENERATION_TYPE,
            OPERATOR_ACTOR,
            timestamp_millis().expect("clock reads"),
            payload_bytes,
        ))
        .expect("append forged evolution generation event");
    drop(plane);

    let reopened = evolve_reopen_real_worker_fixture(&directory);
    assert!(
        reopened.is_err(),
        "a daemon must never trust a promotion claim without matching Forge and Champion evidence"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn evolve_start_rejects_invalid_conflicting_and_concurrent_requests() {
    let directory = tempdir().expect("evolve validation fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();
    let expect_error = |plane: &mut ControlPlane, request: &str, command: Command| {
        dispatch_call(plane, &token, request, command)
            .error
            .expect("evolve request must be refused")
    };

    for (request, command, message) in [
        (
            "bad-run-id",
            evolve_start_command("bad run", &world_id, &parent.genome_id, 1, 2),
            "run_id is invalid",
        ),
        (
            "long-run-id",
            evolve_start_command(&"r".repeat(101), &world_id, &parent.genome_id, 1, 2),
            "run_id must leave room for the run's derived identifiers",
        ),
        (
            "blank-world",
            evolve_start_command("run", " ", &parent.genome_id, 1, 2),
            "world_id and from_genome_id are required",
        ),
        (
            "zero-generations",
            evolve_start_command("run", &world_id, &parent.genome_id, 0, 2),
            "generations must be positive",
        ),
        (
            "tiny-budget",
            evolve_start_command("run", &world_id, &parent.genome_id, 1, 1),
            "budget must allow at least one generation",
        ),
        (
            "bad-status-id",
            Command::EvolveStatus {
                run_id: "bad run".to_owned(),
            },
            "run_id is invalid",
        ),
    ] {
        let error = expect_error(&mut plane, request, command);
        assert_eq!(
            (error.code, error.message.as_str()),
            (ApiErrorCode::InvalidRequest, message),
            "{request}"
        );
    }
    for (request, command) in [
        (
            "unknown-world",
            evolve_start_command("run", "hephaestus:world:missing", &parent.genome_id, 1, 2),
        ),
        (
            "unknown-genome",
            evolve_start_command("run", &world_id, "hephaestus:genome:missing", 1, 2),
        ),
        (
            "unknown-status",
            Command::EvolveStatus {
                run_id: "missing-run".to_owned(),
            },
        ),
    ] {
        assert_eq!(
            expect_error(&mut plane, request, command).code,
            ApiErrorCode::NotFound,
            "{request}"
        );
    }

    // A World whose Champion is another Genome cannot start from this one.
    champion_transition(
        &mut plane,
        &token,
        "seed-candidate",
        Command::ChampionSeed {
            transition_id: "seed-candidate".to_owned(),
            world_id: world_id.clone(),
            genome_id: candidate.genome_id.clone(),
            reason: "Candidate already serves.".to_owned(),
        },
    )
    .expect("seed a different Champion");
    let mismatch = expect_error(
        &mut plane,
        "champion-mismatch",
        evolve_start_command("run", &world_id, &parent.genome_id, 1, 2),
    );
    assert_eq!(
        (mismatch.code, mismatch.message.as_str()),
        (
            ApiErrorCode::InvalidRequest,
            "World Champion does not match from_genome_id"
        )
    );

    // A run ID is bound to its configuration, and only one run may be active.
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "start-run",
            evolve_start_command("bound-run", &world_id, &candidate.genome_id, 1, 2),
        )
        .error
        .is_none()
    );
    let conflicting = expect_error(
        &mut plane,
        "conflicting-run",
        evolve_start_command("bound-run", &world_id, &candidate.genome_id, 2, 4),
    );
    assert!(matches!(
        conflicting.code,
        ApiErrorCode::InvalidRequest | ApiErrorCode::Busy
    ));
    assert_eq!(
        expect_error(
            &mut plane,
            "second-run",
            evolve_start_command("second-run", &world_id, &candidate.genome_id, 1, 2),
        )
        .code,
        ApiErrorCode::Busy
    );

    // Cancellation is idempotent and the reconciliation loop finishes the run.
    assert_eq!(
        expect_error(
            &mut plane,
            "cancel-missing",
            Command::EvolveCancel {
                run_id: "missing-run".to_owned(),
            },
        )
        .code,
        ApiErrorCode::NotFound
    );
    let cancel = || Command::EvolveCancel {
        run_id: "bound-run".to_owned(),
    };
    for request in ["cancel-run", "cancel-run-again"] {
        let response = dispatch_call(&mut plane, &token, request, cancel());
        assert!(
            matches!(
                response.data,
                Some(ResponseData::Evolution { ref run }) if run.cancel_requested
            ),
            "{request}: {:?}",
            response.error
        );
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let finished = loop {
        plane
            .service_async_messages()
            .expect("service evolution after cancellation");
        let Some(ResponseData::Evolution { run }) = dispatch_call(
            &mut plane,
            &token,
            "status-after-cancel",
            Command::EvolveStatus {
                run_id: "bound-run".to_owned(),
            },
        )
        .data
        else {
            panic!("evolution status should succeed");
        };
        if run.state == EvolutionRunState::Finished {
            break run;
        }
        assert!(Instant::now() < deadline, "cancelled run never finished");
        thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        finished.finish_reason,
        Some(EvolutionFinishReason::Cancelled)
    );
    // A finished run is returned unchanged by another cancel.
    assert!(matches!(
        dispatch_call(&mut plane, &token, "cancel-finished", cancel()).data,
        Some(ResponseData::Evolution { run }) if run.state == EvolutionRunState::Finished
    ));
    plane
        .service_async_messages()
        .expect("a finished run is left alone");
}

fn canary_transition(
    plane: &mut ControlPlane,
    token: &str,
    request_id: &str,
    command: Command,
) -> Result<CanaryTransitionRecord, ApiError> {
    let response = dispatch_call(plane, token, request_id, command);
    match (response.data, response.error) {
        (Some(ResponseData::CanaryTransition { transition }), None) => Ok(*transition),
        (None, Some(error)) => Err(error),
        other => panic!("unexpected canary response: {other:?}"),
    }
}

fn canary_show(plane: &mut ControlPlane, token: &str, canary_id: &str) -> CanaryRecord {
    match dispatch_call(
        plane,
        token,
        "canary-show",
        Command::CanaryShow {
            canary_id: canary_id.to_owned(),
        },
    )
    .data
    {
        Some(ResponseData::Canary { canary }) => *canary,
        other => panic!("unexpected canary projection: {other:?}"),
    }
}

fn assert_canary_error(
    result: Result<CanaryTransitionRecord, ApiError>,
    code: ApiErrorCode,
    message: &str,
) {
    let error = result.expect_err("canary transition should be refused");
    assert_eq!((error.code, error.message.as_str()), (code, message));
}

/// Evaluates `parent` against `candidate` and returns the evaluation ID of a
/// fresh, verified selection receipt pairing them: directly usable as
/// canary staged or live-check evidence.
fn canary_evidence_evaluation(
    plane: &mut ControlPlane,
    evaluation_id: &str,
    parent: &str,
    candidate: &str,
) -> String {
    complete_arena_test_job(plane, evaluation_id, parent, candidate);
    plane
        .select_arena_evaluation(evaluation_id)
        .expect("select canary evidence");
    evaluation_id.to_owned()
}

/// Like [`canary_evidence_evaluation`], but for a pairing expected to be
/// healthy. Real wall-clock latency measurement is genuinely noisy (the
/// codebase's own comment on `assessed_forge_child` documents this), so a
/// slow scheduler tick can occasionally cross the fixed latency threshold on
/// one attempt; this retries a bounded number of times on a fresh
/// evaluation ID, exactly as `assessed_forge_child` retries a measured-gate
/// failure, rather than accepting real timing noise as a regression.
fn canary_healthy_evidence_evaluation(
    plane: &mut ControlPlane,
    evaluation_id_prefix: &str,
    parent: &str,
    candidate: &str,
) -> String {
    for attempt in 0..4 {
        let evaluation_id = format!("{evaluation_id_prefix}-{attempt}");
        complete_arena_test_job(plane, &evaluation_id, parent, candidate);
        let ResponseData::Selection { selection } = plane
            .select_arena_evaluation(&evaluation_id)
            .expect("select canary evidence")
        else {
            panic!("selection should return its receipt");
        };
        let deltas = super::canary::regression_deltas(&selection.receipt);
        if !super::canary::is_regression(&deltas) {
            return evaluation_id;
        }
    }
    panic!("healthy evidence never measured as healthy after retries");
}

fn canary_history(plane: &ControlPlane) -> Vec<StoredEvent> {
    plane
        .storage
        .as_ref()
        .expect("canonical canary ledger")
        .ledger
        .replay_verified()
        .expect("verify canary history")
}

fn canary_history_with_payload_edit(
    history: &[StoredEvent],
    event_id: &str,
    edit: impl FnOnce(&mut CanaryTransitionPayload),
) -> Vec<StoredEvent> {
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == event_id)
        .expect("canary event exists");
    let mut payload: CanaryTransitionPayload =
        serde_json::from_slice(&event.payload).expect("decode canary payload");
    edit(&mut payload);
    let canonical = serde_json::to_value(&payload).expect("canonicalize canary payload");
    event.payload = serde_json::to_vec(&canonical).expect("encode canary payload");
    tampered
}

fn drift_record_cmd(
    plane: &mut ControlPlane,
    token: &str,
    request_id: &str,
    command: Command,
) -> Result<DriftRecord, ApiError> {
    let response = dispatch_call(plane, token, request_id, command);
    match (response.data, response.error) {
        (Some(ResponseData::Drift { drift }), None) => Ok(*drift),
        (None, Some(error)) => Err(error),
        other => panic!("unexpected drift response: {other:?}"),
    }
}

fn drift_history(plane: &ControlPlane) -> Vec<StoredEvent> {
    plane
        .storage
        .as_ref()
        .expect("canonical drift ledger")
        .ledger
        .replay_verified()
        .expect("verify drift history")
}

fn drift_history_with_payload_edit(
    history: &[StoredEvent],
    event_id: &str,
    edit: impl FnOnce(&mut DriftRecordPayload),
) -> Vec<StoredEvent> {
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == event_id)
        .expect("drift event exists");
    let mut payload: DriftRecordPayload =
        serde_json::from_slice(&event.payload).expect("decode drift payload");
    edit(&mut payload);
    let canonical = serde_json::to_value(&payload).expect("canonicalize drift payload");
    event.payload = serde_json::to_vec(&canonical).expect("encode drift payload");
    tampered
}

#[test]
#[allow(clippy::too_many_lines)]
fn canary_staged_rollout_promotes_through_champion_path_and_replays() {
    let directory = tempdir().expect("canary fixture");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();

    let assessed = assessed_forge_child(
        &mut plane,
        "canary-improve",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    let world_id = assessed.world.clone();
    plane
        .check_arena_invariants(&assessed.evaluation)
        .expect("record child invariant evidence");

    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: initial_candidate.genome_id.clone(),
            reason: "Bootstrap the reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");

    let start = |canary_id: &str| Command::CanaryStart {
        canary_id: canary_id.to_owned(),
        world_id: world_id.clone(),
        candidate_genome_id: assessed.child.clone(),
        assessment_id: "canary-improve-assessment".to_owned(),
    };

    assert_canary_error(
        canary_transition(
            &mut plane,
            &token,
            "start-missing-world",
            Command::CanaryStart {
                canary_id: "canary".to_owned(),
                world_id: " ".to_owned(),
                candidate_genome_id: assessed.child.clone(),
                assessment_id: "canary-improve-assessment".to_owned(),
            },
        ),
        ApiErrorCode::InvalidRequest,
        "world_id and candidate_genome_id are required",
    );

    let started =
        canary_transition(&mut plane, &token, "start", start("canary")).expect("start canary");
    assert_eq!(started.payload.kind, CanaryTransitionKind::Started);
    assert_eq!(started.payload.stage, CanaryStage::Pending);
    assert_eq!(started.payload.candidate_genome_id, assessed.child);
    assert_eq!(
        started.payload.previous_champion_genome_id,
        initial_candidate.genome_id
    );
    assert_eq!(started.event.event_id, "canary:canary:started");
    assert_eq!(
        canary_transition(&mut plane, &token, "start-retry", start("canary")),
        Ok(started.clone()),
        "an identical retry returns the recorded transition"
    );
    assert_canary_error(
        canary_transition(
            &mut plane,
            &token,
            "start-conflict",
            Command::CanaryStart {
                canary_id: "canary".to_owned(),
                world_id: world_id.clone(),
                candidate_genome_id: initial_parent.genome_id.clone(),
                assessment_id: "canary-improve-assessment".to_owned(),
            },
        ),
        ApiErrorCode::InvalidRequest,
        "canary_id / evidence is already bound to different canary content",
    );

    // Freeze blocks a would-be-healthy advance. Evidence generation itself
    // is new Arena work, so it is captured before freezing.
    let evidence_1 = canary_healthy_evidence_evaluation(
        &mut plane,
        "canary-evidence-1",
        &initial_candidate.genome_id,
        &assessed.child,
    );
    assert!(
        dispatch_call(&mut plane, &token, "freeze-advance", Command::Freeze)
            .error
            .is_none()
    );
    assert_canary_error(
        canary_transition(
            &mut plane,
            &token,
            "advance-frozen",
            Command::CanaryAdvance {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: evidence_1.clone(),
            },
        ),
        ApiErrorCode::InvalidRequest,
        "evolution is frozen",
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-advance", Command::Unfreeze)
            .error
            .is_none()
    );

    let advance1 = canary_transition(
        &mut plane,
        &token,
        "advance-1",
        Command::CanaryAdvance {
            canary_id: "canary".to_owned(),
            evidence_evaluation_id: evidence_1.clone(),
        },
    )
    .expect("advance to 5%");
    assert_eq!(advance1.payload.kind, CanaryTransitionKind::Advanced);
    assert_eq!(advance1.payload.stage, CanaryStage::Stage5);
    assert!(
        !advance1
            .payload
            .evidence
            .as_ref()
            .expect("evidence")
            .regressed
    );
    assert_eq!(
        canary_transition(
            &mut plane,
            &token,
            "advance-1-retry",
            Command::CanaryAdvance {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: evidence_1.clone(),
            },
        ),
        Ok(advance1.clone()),
        "an identical retry returns the recorded transition"
    );

    let evidence_2 = canary_healthy_evidence_evaluation(
        &mut plane,
        "canary-evidence-2",
        &initial_candidate.genome_id,
        &assessed.child,
    );
    let advance2 = canary_transition(
        &mut plane,
        &token,
        "advance-2",
        Command::CanaryAdvance {
            canary_id: "canary".to_owned(),
            evidence_evaluation_id: evidence_2.clone(),
        },
    )
    .expect("advance to 25%");
    assert_eq!(advance2.payload.stage, CanaryStage::Stage25);

    let evidence_3 = canary_healthy_evidence_evaluation(
        &mut plane,
        "canary-evidence-3",
        &initial_candidate.genome_id,
        &assessed.child,
    );
    let advance3 = canary_transition(
        &mut plane,
        &token,
        "advance-3",
        Command::CanaryAdvance {
            canary_id: "canary".to_owned(),
            evidence_evaluation_id: evidence_3.clone(),
        },
    )
    .expect("advance to 50%");
    assert_eq!(advance3.payload.stage, CanaryStage::Stage50);

    // The prior Champion stays Champion until 100%.
    assert_eq!(
        champion_show(&mut plane, &token, &world_id)
            .champion_genome_id
            .as_deref(),
        Some(initial_candidate.genome_id.as_str())
    );

    let evidence_4 = canary_healthy_evidence_evaluation(
        &mut plane,
        "canary-evidence-4",
        &initial_candidate.genome_id,
        &assessed.child,
    );
    let advance4 = canary_transition(
        &mut plane,
        &token,
        "advance-4",
        Command::CanaryAdvance {
            canary_id: "canary".to_owned(),
            evidence_evaluation_id: evidence_4.clone(),
        },
    )
    .expect("complete the canary");
    assert_eq!(advance4.payload.stage, CanaryStage::Completed);
    let promotion = advance4
        .payload
        .champion_promotion
        .clone()
        .expect("promotion evidence");
    assert_eq!(promotion.assessment_id, "canary-improve-assessment");

    // Completion promoted through the existing, unchanged Champion path.
    let champion = champion_show(&mut plane, &token, &world_id);
    assert_eq!(
        champion.champion_genome_id.as_deref(),
        Some(assessed.child.as_str())
    );
    assert_eq!(champion.transitions.len(), 2, "seed then promote");
    assert_eq!(
        champion.transitions[1].payload.kind,
        ChampionTransitionKind::Promoted
    );

    let canary = canary_show(&mut plane, &token, "canary");
    assert_eq!(canary.stage, CanaryStage::Completed);
    assert_eq!(canary.transitions.len(), 5, "started plus four advances");

    let listed = match dispatch_call(
        &mut plane,
        &token,
        "canary-list",
        Command::CanaryList { limit: 10 },
    )
    .data
    {
        Some(ResponseData::CanaryList { canaries }) => canaries,
        other => panic!("unexpected canary list response: {other:?}"),
    };
    assert_eq!(
        listed,
        vec![canary.clone()],
        "the one canary is listed newest first"
    );

    // A terminal canary refuses further advancement.
    let evidence_5 = canary_healthy_evidence_evaluation(
        &mut plane,
        "canary-evidence-5",
        &initial_candidate.genome_id,
        &assessed.child,
    );
    assert_canary_error(
        canary_transition(
            &mut plane,
            &token,
            "advance-terminal",
            Command::CanaryAdvance {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: evidence_5.clone(),
            },
        ),
        ApiErrorCode::InvalidRequest,
        "canary has already reached a terminal stage",
    );

    // Live-check requires the completion pairing exactly; a reversed pairing
    // is refused, and healthy (non-regressed) evidence is correctly refused
    // rather than triggering an unwarranted rollback.
    let reversed_evidence = canary_evidence_evaluation(
        &mut plane,
        "canary-evidence-reversed",
        &assessed.child,
        &initial_parent.genome_id,
    );
    assert_canary_error(
        canary_transition(
            &mut plane,
            &token,
            "live-check-wrong-pair",
            Command::CanaryLiveCheck {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: reversed_evidence,
            },
        ),
        ApiErrorCode::InvalidRequest,
        "evidence does not pair the previous Champion against the live Champion",
    );
    assert_canary_error(
        canary_transition(
            &mut plane,
            &token,
            "live-check-healthy",
            Command::CanaryLiveCheck {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: evidence_4,
            },
        ),
        ApiErrorCode::InvalidRequest,
        "evidence does not show a live regression",
    );

    // A candidate not evidenced against the current Champion cannot start.
    let pending_started = canary_transition(
        &mut plane,
        &token,
        "start-pending",
        Command::CanaryStart {
            canary_id: "canary-pending".to_owned(),
            world_id: world_id.clone(),
            candidate_genome_id: initial_parent.genome_id.clone(),
            assessment_id: "canary-improve-assessment".to_owned(),
        },
    );
    assert_canary_error(
        pending_started,
        ApiErrorCode::InvalidRequest,
        "assessment does not evidence the current Champion against this candidate",
    );

    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));

    let history = canary_history(&plane);
    verify_canary_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        &plane.state.registered,
    )
    .expect("canonical canary history verifies");
    for (event_id, edit) in [
        (
            started.event.event_id.clone(),
            Box::new(|payload: &mut CanaryTransitionPayload| {
                payload
                    .candidate_genome_id
                    .clone_from(&initial_parent.genome_id);
            }) as Box<dyn FnOnce(&mut CanaryTransitionPayload)>,
        ),
        (
            advance4.event.event_id.clone(),
            Box::new(|payload: &mut CanaryTransitionPayload| {
                payload.champion_promotion = None;
            }),
        ),
        (
            advance1.event.event_id.clone(),
            Box::new(|payload: &mut CanaryTransitionPayload| {
                payload.stage = CanaryStage::Stage25;
            }),
        ),
    ] {
        let tampered = canary_history_with_payload_edit(&history, &event_id, edit);
        assert!(
            verify_canary_history(
                &plane.storage.as_ref().unwrap().artifacts,
                &tampered,
                &plane.state.registered
            )
            .is_err(),
            "tampered canary transition {event_id} must fail replay"
        );
    }
    let mut retyped = history.clone();
    retyped
        .iter_mut()
        .find(|event| event.event_id == started.event.event_id)
        .expect("start event")
        .event_type = "canary.rewritten".to_owned();
    assert!(
        verify_canary_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &retyped,
            &plane.state.registered
        )
        .is_err()
    );
    let mut reordered = history;
    let start_index = reordered
        .iter()
        .position(|event| event.event_id == started.event.event_id)
        .expect("start event");
    let start_event = reordered.remove(start_index);
    reordered.push(start_event);
    assert!(
        verify_canary_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &reordered,
            &plane.state.registered
        )
        .is_err(),
        "an advance cannot precede the start it depends on"
    );
}

/// Completes a canary to 100% (the real Champion path promotes it), then
/// genuinely slows the now-live Champion's reference worker via the
/// test-only `HEPHAESTUS_TEST_REFERENCE_DELAY_MS` injection (see
/// `hephaestus_runtime::supervisor::test_reference_delay_millis_for_genome`
/// and `ReferenceInstruction::frame_with_test_delay`), runs a fresh paired
/// evaluation of the previous Champion against the now-slow live Champion
/// through the real worker binary, and confirms `canary live-check` reads
/// that real latency regression and automatically rolls the Champion back:
/// the previous Champion is restored, the regressed one is quarantined, one
/// `LiveRegressionDetected` transition is recorded, and replay verifies.
#[test]
#[allow(clippy::too_many_lines)]
fn canary_live_check_detects_a_genuine_latency_regression_and_rolls_back_the_champion() {
    let _reference_delay_slot = hold_reference_delay_slot();
    let directory = tempdir().expect("canary live-check fixture");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();

    let assessed = assessed_forge_child(
        &mut plane,
        "livecheck-improve",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    let world_id = assessed.world.clone();
    plane
        .check_arena_invariants(&assessed.evaluation)
        .expect("record child invariant evidence");

    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: initial_candidate.genome_id.clone(),
            reason: "Bootstrap the reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");

    canary_transition(
        &mut plane,
        &token,
        "start",
        Command::CanaryStart {
            canary_id: "canary".to_owned(),
            world_id: world_id.clone(),
            candidate_genome_id: assessed.child.clone(),
            assessment_id: "livecheck-improve-assessment".to_owned(),
        },
    )
    .expect("start canary");

    for (index, stage_label) in ["5", "25", "50", "100"].into_iter().enumerate() {
        let evidence = canary_healthy_evidence_evaluation(
            &mut plane,
            &format!("canary-evidence-{index}"),
            &initial_candidate.genome_id,
            &assessed.child,
        );
        canary_transition(
            &mut plane,
            &token,
            &format!("advance-{stage_label}"),
            Command::CanaryAdvance {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: evidence,
            },
        )
        .unwrap_or_else(|error| panic!("advance to {stage_label}%: {error:?}"));
    }

    let champion = champion_show(&mut plane, &token, &world_id);
    assert_eq!(
        champion.champion_genome_id.as_deref(),
        Some(assessed.child.as_str()),
        "the canary should have promoted its candidate to Champion"
    );

    // Genuinely slow the now-live Champion's reference worker so a fresh
    // paired evaluation measures a real latency regression rather than
    // fabricating one. This targets a content-addressed Genome id that
    // cannot collide with any other test's Genome, so it is harmless even
    // if another test happens to run concurrently in this process.
    hephaestus_runtime::set_test_reference_delay(assessed.child.clone(), 750);
    let live_evidence = "canary-live-regression-eval";
    complete_arena_test_job(
        &mut plane,
        live_evidence,
        &initial_candidate.genome_id,
        &assessed.child,
    );
    hephaestus_runtime::clear_test_reference_delay();
    let ResponseData::Selection { selection } = plane
        .select_arena_evaluation(live_evidence)
        .expect("select live regression evidence")
    else {
        panic!("selection should return its receipt");
    };
    assert!(
        selection.receipt.candidate_latency_millis() > selection.receipt.parent_latency_millis(),
        "the injected delay should make the live Champion measurably slower: parent={} candidate={}",
        selection.receipt.parent_latency_millis(),
        selection.receipt.candidate_latency_millis(),
    );
    let deltas = super::canary::regression_deltas(&selection.receipt);
    assert!(
        super::canary::is_regression(&deltas),
        "the injected delay should read as a genuine regression: {deltas:?}"
    );

    let rollback = canary_transition(
        &mut plane,
        &token,
        "live-check",
        Command::CanaryLiveCheck {
            canary_id: "canary".to_owned(),
            evidence_evaluation_id: live_evidence.to_owned(),
        },
    )
    .expect("live-check should detect the regression and roll back");
    assert_eq!(
        rollback.payload.kind,
        CanaryTransitionKind::LiveRegressionDetected
    );
    assert!(
        rollback
            .payload
            .evidence
            .as_ref()
            .expect("rollback evidence")
            .regressed
    );
    assert!(rollback.payload.champion_rollback_event_id.is_some());

    let champion_after = champion_show(&mut plane, &token, &world_id);
    assert_eq!(
        champion_after.champion_genome_id.as_deref(),
        Some(initial_candidate.genome_id.as_str()),
        "rollback should restore the previous Champion"
    );
    assert_eq!(
        champion_after.quarantined_genome_ids,
        vec![assessed.child.clone()],
        "the regressed live Champion should be quarantined"
    );

    let canary = canary_show(&mut plane, &token, "canary");
    assert_eq!(
        canary
            .transitions
            .iter()
            .filter(|transition| transition.payload.kind
                == CanaryTransitionKind::LiveRegressionDetected)
            .count(),
        1,
        "exactly one live regression transition should be recorded"
    );

    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
    let history = canary_history(&plane);
    verify_canary_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        &plane.state.registered,
    )
    .expect("canonical canary history including the live rollback verifies");
}

#[test]
#[allow(clippy::too_many_lines)]
fn canary_injected_regression_during_staged_advance_automatically_aborts_and_replays() {
    let directory = tempdir().expect("canary regression fixture");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();

    let improved = assessed_forge_child(
        &mut plane,
        "base-improve",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    let world_id = improved.world.clone();

    // Seed the Champion directly as the already-improved Genome, so a second
    // flip of the reference operation is a genuine regression against it.
    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: improved.child.clone(),
            reason: "Bootstrap the reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");

    // The candidate flipped to the worse reference operation: a real,
    // verified regression against the seeded Champion.
    let regressed = assessed_forge_child(
        &mut plane,
        "canary-regress",
        &initial_parent.genome_id,
        &improved.child,
        false,
    );
    assert!(
        regressed.selection_event != improved.selection_event,
        "regression uses fresh, distinct evidence"
    );

    canary_transition(
        &mut plane,
        &token,
        "start",
        Command::CanaryStart {
            canary_id: "canary".to_owned(),
            world_id: world_id.clone(),
            candidate_genome_id: regressed.child.clone(),
            assessment_id: "canary-regress-assessment".to_owned(),
        },
    )
    .expect("start canary on a candidate not yet known to be safe");

    // Abort is a safety action and remains available while frozen.
    assert!(
        dispatch_call(&mut plane, &token, "freeze-advance", Command::Freeze)
            .error
            .is_none()
    );
    let aborted = canary_transition(
        &mut plane,
        &token,
        "advance-regressed",
        Command::CanaryAdvance {
            canary_id: "canary".to_owned(),
            evidence_evaluation_id: regressed.evaluation.clone(),
        },
    )
    .expect("regression evidence automatically aborts rather than erroring");
    assert_eq!(aborted.payload.kind, CanaryTransitionKind::Aborted);
    assert_eq!(aborted.payload.stage, CanaryStage::Aborted);
    let evidence = aborted.payload.evidence.clone().expect("abort evidence");
    assert!(evidence.regressed);
    assert!(
        evidence.correctness_delta_bps <= -super::canary::CORRECTNESS_REGRESSION_BPS,
        "the regression must cross the documented correctness threshold"
    );
    assert!(aborted.payload.reason.is_some());

    // The prior Champion was never at risk.
    assert_eq!(
        champion_show(&mut plane, &token, &world_id)
            .champion_genome_id
            .as_deref(),
        Some(improved.child.as_str())
    );
    assert_eq!(
        canary_show(&mut plane, &token, "canary").stage,
        CanaryStage::Aborted
    );

    // An identical retry with the same evidence is idempotent.
    assert_eq!(
        canary_transition(
            &mut plane,
            &token,
            "advance-regressed-retry",
            Command::CanaryAdvance {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: regressed.evaluation.clone(),
            },
        ),
        Ok(aborted.clone())
    );
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    // A terminal (aborted) canary refuses any further advancement.
    let fresh_evidence = canary_evidence_evaluation(
        &mut plane,
        "canary-post-abort-evidence",
        &improved.child,
        &initial_parent.genome_id,
    );
    assert_canary_error(
        canary_transition(
            &mut plane,
            &token,
            "advance-after-abort",
            Command::CanaryAdvance {
                canary_id: "canary".to_owned(),
                evidence_evaluation_id: fresh_evidence,
            },
        ),
        ApiErrorCode::InvalidRequest,
        "canary has already reached a terminal stage",
    );
    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
    let history = canary_history(&plane);
    verify_canary_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        &plane.state.registered,
    )
    .expect("canonical canary history verifies");
    let tampered = canary_history_with_payload_edit(
        &history,
        &aborted.event.event_id,
        |payload: &mut CanaryTransitionPayload| {
            payload.kind = CanaryTransitionKind::Advanced;
            payload.stage = CanaryStage::Stage5;
        },
    );
    assert!(
        verify_canary_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &tampered,
            &plane.state.registered
        )
        .is_err(),
        "a rewritten abort must fail replay"
    );
}

fn assert_drift_error(result: Result<DriftRecord, ApiError>, message: &str) {
    let error = result.expect_err("drift record should be refused");
    assert_eq!(
        (error.code, error.message.as_str()),
        (ApiErrorCode::InvalidRequest, message)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn drift_record_derives_from_verified_evidence_and_replays() {
    let _reference_delay_slot = hold_reference_delay_slot();
    let directory = tempdir().expect("drift fixture");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();

    let improved = assessed_forge_child(
        &mut plane,
        "base-improve",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    let world_id = improved.world.clone();

    assert_drift_error(
        drift_record_cmd(
            &mut plane,
            &token,
            "drift-no-champion",
            Command::DriftRecord {
                drift_id: "drift".to_owned(),
                world_id: world_id.clone(),
                kind: DriftKind::Correctness,
                evidence_evaluation_id: improved.evaluation.clone(),
            },
        ),
        "World has no Champion; seed one before recording drift",
    );

    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: improved.child.clone(),
            reason: "Bootstrap the reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");

    let regressed = assessed_forge_child(
        &mut plane,
        "drift-regress",
        &initial_parent.genome_id,
        &improved.child,
        false,
    );

    // The wrong kind for this evidence is refused rather than silently
    // recorded under a threshold it did not actually cross.
    assert_drift_error(
        drift_record_cmd(
            &mut plane,
            &token,
            "drift-wrong-kind",
            Command::DriftRecord {
                drift_id: "drift".to_owned(),
                world_id: world_id.clone(),
                kind: DriftKind::Latency,
                evidence_evaluation_id: regressed.evaluation.clone(),
            },
        ),
        "evidence does not show a shift beyond the documented threshold for this kind",
    );

    let recorded = drift_record_cmd(
        &mut plane,
        &token,
        "drift-record",
        Command::DriftRecord {
            drift_id: "drift".to_owned(),
            world_id: world_id.clone(),
            kind: DriftKind::Correctness,
            evidence_evaluation_id: regressed.evaluation.clone(),
        },
    )
    .expect("record a genuine correctness drift");
    assert_eq!(recorded.payload.kind, DriftKind::Correctness);
    assert_eq!(recorded.payload.baseline_genome_id, improved.child);
    assert_eq!(recorded.payload.shifted_genome_id, regressed.child);
    assert_eq!(recorded.event.event_id, "drift:drift:recorded");
    assert!(recorded.payload.observed_delta_bps <= -i64::from(recorded.payload.threshold_bps));

    assert_eq!(
        drift_record_cmd(
            &mut plane,
            &token,
            "drift-retry",
            Command::DriftRecord {
                drift_id: "drift".to_owned(),
                world_id: world_id.clone(),
                kind: DriftKind::Correctness,
                evidence_evaluation_id: regressed.evaluation.clone(),
            },
        ),
        Ok(recorded.clone()),
        "an identical retry returns the recorded drift"
    );
    assert_drift_error(
        drift_record_cmd(
            &mut plane,
            &token,
            "drift-conflict",
            Command::DriftRecord {
                drift_id: "drift".to_owned(),
                world_id: world_id.clone(),
                kind: DriftKind::Workload,
                evidence_evaluation_id: regressed.evaluation.clone(),
            },
        ),
        "drift_id is already bound to different drift content",
    );

    let shown = match dispatch_call(
        &mut plane,
        &token,
        "drift-show",
        Command::DriftShow {
            drift_id: "drift".to_owned(),
        },
    )
    .data
    {
        Some(ResponseData::Drift { drift }) => *drift,
        other => panic!("unexpected drift show response: {other:?}"),
    };
    assert_eq!(shown, recorded);

    let listed = match dispatch_call(
        &mut plane,
        &token,
        "drift-list",
        Command::DriftList { limit: 10 },
    )
    .data
    {
        Some(ResponseData::DriftList { drifts }) => drifts,
        other => panic!("unexpected drift list response: {other:?}"),
    };
    assert_eq!(
        listed,
        vec![recorded.clone()],
        "the one recorded drift is listed newest first"
    );
    let Some(capped) = dispatch_call(
        &mut plane,
        &token,
        "drift-list-capped",
        Command::DriftList { limit: 0 },
    )
    .error
    else {
        panic!("a zero limit must be rejected")
    };
    assert_eq!(capped.message, "limit must be between 1 and 200");

    // Drift never directly replaces a Champion.
    assert_eq!(
        champion_show(&mut plane, &token, &world_id)
            .champion_genome_id
            .as_deref(),
        Some(improved.child.as_str())
    );

    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
    let history = drift_history(&plane);
    verify_drift_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        &plane.state.registered,
    )
    .expect("canonical drift history verifies");
    let tampered = drift_history_with_payload_edit(
        &history,
        &recorded.event.event_id,
        |payload: &mut DriftRecordPayload| {
            payload.observed_delta_bps = 0;
        },
    );
    assert!(
        verify_drift_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &tampered,
            &plane.state.registered
        )
        .is_err(),
        "a rewritten drift observation must fail replay"
    );
    let mut retyped = history;
    retyped
        .iter_mut()
        .find(|event| event.event_id == recorded.event.event_id)
        .expect("drift event")
        .event_type = "drift.rewritten".to_owned();
    assert!(
        verify_drift_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &retyped,
            &plane.state.registered
        )
        .is_err()
    );
}

fn write_fake_claude_binary(path: &Path) {
    fs::write(
        path,
        "#!/bin/sh\n\
cat >/dev/null\n\
echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"fake-session\"}'\n\
echo '{\"type\":\"assistant\",\"parent_tool_use_id\":null,\"message\":{\"content\":[{\"type\":\"tool_use\",\"name\":\"Read\",\"input\":{\"path\":\"fixture.txt\"}}]}}'\n\
echo '{\"type\":\"user\",\"parent_tool_use_id\":null,\"message\":{\"content\":[{\"type\":\"tool_result\",\"content\":\"deterministic fixture\",\"is_error\":false}]}}'\n\
echo '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"Inventory complete: fixture.txt\",\"total_cost_usd\":0.0042,\"session_id\":\"fake-session\"}'\n",
    )
    .expect("write fake claude binary");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("mark fake claude executable");
}

fn register_claude_provider_genome(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    world_id: &str,
) -> GenomeRecord {
    let genome_path = directory.path().join("claude-agent.json");
    fs::write(
        &genome_path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "name": "claude-agent",
            "parents": [],
            "model": {"provider": "claude", "family": "sonnet"},
            "authority": {"workspace_write": false, "network": false},
            "artifacts": {}
        }))
        .expect("encode claude Genome"),
    )
    .expect("write claude Genome source");
    let Some(ResponseData::Genome { genome }) = dispatch_call(
        plane,
        token,
        "register-claude-genome",
        Command::GenomeRegister {
            path: genome_path.display().to_string(),
            world_id: world_id.to_owned(),
        },
    )
    .data
    else {
        panic!("claude Genome registration should succeed");
    };
    genome
}

#[test]
#[allow(clippy::too_many_lines)]
fn provider_claude_genome_runs_end_to_end_through_run_with_signed_result_and_traces() {
    let directory = tempdir().expect("fixture directory");
    let (repository, source_revision) = committed_reference_fixture(directory.path());
    let data_dir = directory.path().join("data");
    let current_executable = env::current_exe().expect("test executable");
    let fake_claude = directory.path().join("fake-claude");
    write_fake_claude_binary(&fake_claude);
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &current_executable,
        &current_executable,
    )
    .expect("open control plane for provider adapter run")
    .with_provider_executables_for_testing(
        "/nonexistent/codex-should-not-be-invoked",
        &fake_claude,
        Vec::new(),
    );
    let token = plane.token_hex.clone();
    // A provider run's cost is bounded by its World's approved Law, not by
    // the fixed zero ceiling the reference-worker smoke test uses; give this
    // World enough headroom for the fake binary's reported $0.0042.
    let world_path = directory.path().join("provider-world.json");
    fs::write(
        &world_path,
        r#"{"schema_version":1,"name":"provider-world","laws":{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":1000000},"authority_ceiling":{"workspace_write":false,"network":false},"mutation_scope":[],"promotion":{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500},"objectives":["correctness"],"evaluator_artifacts":{}}"#,
    )
    .expect("write provider World source");
    let Some(ResponseData::World { world }) = dispatch_call(
        &mut plane,
        &token,
        "register-provider-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("provider World registration should succeed");
    };
    let genome = register_claude_provider_genome(&mut plane, &token, &directory, &world.world_id);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    // `run`: synchronous path through `execute_provider_runtime`.
    let response = dispatch_call(
        &mut plane,
        &token,
        "claude-run",
        Command::RunReference {
            genome_id: genome.genome_id.clone(),
        },
    );
    assert!(
        response.error.is_none(),
        "claude adapter run failed: {:?}",
        response.error
    );
    let (run_id, stdout_artifact_id, revision, run_completion, run_cost) =
        match response.data.expect("claude run response") {
            ResponseData::Run {
                run_id,
                stdout_artifact_id,
                source_revision,
                completion_reason,
                actual_cost_microusd,
                genome_id,
                world_id,
                trace_artifact_ids,
                ..
            } => {
                assert_eq!(genome_id, genome.genome_id);
                assert_eq!(world_id, world.world_id);
                assert!(
                    !trace_artifact_ids.is_empty(),
                    "provider run must record trace evidence"
                );
                (
                    run_id,
                    stdout_artifact_id,
                    source_revision,
                    completion_reason,
                    actual_cost_microusd,
                )
            }
            other => panic!("unexpected claude run response: {other:?}"),
        };
    assert_eq!(run_completion, RunCompletionReason::Success);
    assert_eq!(run_cost, 4_200, "claude's reported $0.0042 must round-trip");
    assert_eq!(revision, source_revision);
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open provider CAS");
    let stdout = artifacts
        .get(&ArtifactId::parse(stdout_artifact_id).expect("stdout artifact ID"))
        .expect("load final answer from CAS");
    assert_eq!(
        String::from_utf8(stdout).expect("final answer is UTF-8"),
        "Inventory complete: fixture.txt",
        "stdout must be the extracted final answer, not the raw NDJSON stream"
    );

    // `submit` (the async job path) now supports a provider Genome too: its
    // job-record projection accepts a `provider-v1.<digest>` environment
    // identity pinned to the configured executable and a cost budget bounded
    // by the Genome's own registered World Law.
    let submit_response = dispatch_call(
        &mut plane,
        &token,
        "claude-submit",
        Command::RunSubmit {
            job_id: "claude-submit".to_owned(),
            genome_id: genome.genome_id.clone(),
        },
    );
    assert!(
        submit_response.error.is_none(),
        "submit must admit a provider Genome job: {:?}",
        submit_response.error
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist provider job evidence");
        assert!(Instant::now() < deadline, "provider job stalled");
        if plane.active_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let submitted_job = plane
        .state
        .jobs
        .get("claude-submit")
        .expect("submitted provider job is recorded");
    assert_eq!(submitted_job.state, JobState::Succeeded);
    assert!(
        submitted_job.environment_id.starts_with("provider-v1."),
        "provider job must bind a provider-shaped environment identity"
    );
    assert_eq!(submitted_job.budget.maximum_cost_microusd, 1_000_000);

    // Replay proves the signed run result verifies from canonical history.
    let history = plane
        .storage
        .as_ref()
        .expect("storage open")
        .ledger
        .replay_verified()
        .expect("replay canonical ledger");
    let recorded_runs: Vec<RunResultReceipt> = history
        .iter()
        .filter(|event| event.event_type == "run.result_recorded")
        .map(|event| {
            RunResultReceipt::parse_from_event(event, &plane.run_result_verifier)
                .expect("provider run result verifies")
        })
        .collect();
    assert!(
        recorded_runs.iter().any(|receipt| receipt.run_id == run_id
            && receipt.completion_reason == RunCompletionReason::Success
            && receipt.actual_cost_microusd == 4_200),
        "synchronous claude run must be in signed history with its reported cost"
    );
}

#[test]
fn provider_run_redacts_secret_looking_text_before_it_reaches_the_artifact_store() {
    let directory = tempdir().expect("fixture directory");
    let (repository, _source_revision) = committed_reference_fixture(directory.path());
    let data_dir = directory.path().join("data");
    let current_executable = env::current_exe().expect("test executable");
    let fake_claude = directory.path().join("fake-claude-secret");
    fs::write(
        &fake_claude,
        "#!/bin/sh\n\
cat >/dev/null\n\
echo '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"token=sk-verysecrettoken1234 and operator-secret\",\"total_cost_usd\":0}'\n",
    )
    .expect("write fake claude binary");
    fs::set_permissions(&fake_claude, fs::Permissions::from_mode(0o700))
        .expect("mark fake claude executable");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &current_executable,
        &current_executable,
    )
    .expect("open control plane for redaction test")
    .with_provider_executables_for_testing(
        "/nonexistent/codex-should-not-be-invoked",
        &fake_claude,
        Vec::new(),
    );
    let token = plane.token_hex.clone();
    let (world, _genome, _prompt) = register_dispatch_objects(&mut plane, &token, &directory);
    let genome_path = directory.path().join("claude-secret-agent.json");
    fs::write(
        &genome_path,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "name": "claude-secret-agent",
            "parents": [],
            "model": {"provider": "claude", "family": "sonnet"},
            "authority": {"workspace_write": false, "network": false},
            "artifacts": {}
        }))
        .expect("encode claude Genome"),
    )
    .expect("write claude Genome source");
    let Some(ResponseData::Genome { genome }) = dispatch_call(
        &mut plane,
        &token,
        "register-claude-secret-genome",
        Command::GenomeRegister {
            path: genome_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("claude Genome registration should succeed");
    };
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    let response = dispatch_call(
        &mut plane,
        &token,
        "claude-secret-run",
        Command::RunReference {
            genome_id: genome.genome_id.clone(),
        },
    );
    assert!(response.error.is_none(), "run failed: {:?}", response.error);
    let ResponseData::Run {
        stdout_artifact_id,
        completion_reason,
        ..
    } = response.data.expect("run response")
    else {
        panic!("unexpected response shape");
    };
    assert_eq!(completion_reason, RunCompletionReason::Success);
    let artifacts = ArtifactStore::open(data_dir.join("blobs")).expect("open CAS");
    let stdout = artifacts
        .get(&ArtifactId::parse(stdout_artifact_id).expect("stdout artifact ID"))
        .expect("load redacted final answer from CAS");
    let final_answer = String::from_utf8(stdout).expect("final answer is UTF-8");
    assert!(
        !final_answer.contains("sk-verysecrettoken1234"),
        "the sk- prefixed token must never reach the artifact store: {final_answer}"
    );
    assert!(
        final_answer.contains("[REDACTED]"),
        "redaction must replace the secret rather than silently drop the whole message: {final_answer}"
    );
}

// ---------------------------------------------------------------------------
// Recursive evolution of the Evolver (roadmap item 13). `real_worker_arena_fixture`
// gives the first held-out lineage; `register_second_meta_lineage` registers a
// second, distinctly named World and Genome pair on the same plane so the
// meta-evaluation has two held-out lineages to bootstrap over.
// ---------------------------------------------------------------------------

fn register_second_meta_lineage(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
) -> (WorldRecord, GenomeRecord, GenomeRecord) {
    register_meta_lineage(plane, token, directory, "second-lineage", None)
}

/// Registers one held-out lineage (a distinctly named World plus a parent
/// and candidate Genome pair, the parent Genome misconfigured with the
/// `identity` reference operation against uppercase-expecting tasks so the
/// unmodified evolve engine's one available mutation, the reference
/// operation flip, corrects it in generation zero and can never improve it
/// further) under `label`, so repeated calls with distinct labels build as
/// many independent held-out lineages as a meta-evaluation needs.
/// `invariant_manifest`, when given, is registered as the World's
/// `arena.invariant_manifest`; evolve's per-generation invariant check
/// otherwise has nothing to check against and the run finishes interrupted
/// after zero generations, exactly like `evolve_start` without one (see
/// `real_worker_arena_fixture_with_invariants`).
#[allow(clippy::too_many_lines)]
fn register_meta_lineage(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    label: &str,
    invariant_manifest: Option<&[u8]>,
) -> (WorldRecord, GenomeRecord, GenomeRecord) {
    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    let visible = TrustedManifest::new(
        format!("{label}-visible"),
        Visibility::Visible,
        vec![
            hephaestus_arena::TrustedTask::new("visible-task", "visible", "VISIBLE")
                .expect("visible task"),
        ],
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        format!("{label}-sealed"),
        Visibility::Sealed,
        vec![
            hephaestus_arena::TrustedTask::new("sealed-task", "sealed", "SEALED")
                .expect("sealed task"),
        ],
    )
    .expect("sealed manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator = env::current_exe()
        .expect("locate test executable")
        .parent()
        .and_then(Path::parent)
        .expect("locate Cargo binary directory")
        .join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
    let evaluator_id = artifacts
        .put(&fs::read(evaluator).expect("read reference evaluator"))
        .expect("store evaluator identity");
    let verifier_id = artifacts
        .put(&plane.run_result_verifier.public_key_bytes())
        .expect("store result verifier");
    let invariant_entry = invariant_manifest.map_or_else(String::new, |manifest| {
        format!(
            r#","arena.invariant_manifest":"{}""#,
            artifacts
                .put(manifest)
                .expect("store invariant manifest")
                .as_str()
        )
    });
    drop(artifacts);
    let world_path = directory.path().join(format!("{label}-world.json"));
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"{label}","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":["harness"],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"{}}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
            invariant_entry,
        ),
    )
    .expect("write held-out lineage World");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        &format!("{label}-world"),
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("held-out lineage World registration should succeed");
    };
    let register_genome = |plane: &mut ControlPlane, token: &str, name: &str, parents: &str| {
        let path = directory.path().join(format!("{name}.md"));
        fs::write(
            &path,
            format!(
                "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
            ),
        )
        .expect("write Genome source");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            token,
            name,
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("held-out lineage Genome registration should succeed");
        };
        genome
    };
    let parent = register_genome(plane, token, &format!("{label}-parent"), "[]");
    let candidate = register_genome(
        plane,
        token,
        &format!("{label}-candidate"),
        &format!("[\"{}\"]", parent.genome_id),
    );
    (world, parent, candidate)
}

fn register_meta_strategy(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    label: &str,
) -> String {
    register_meta_strategy_ex(plane, token, directory, label, 1, 2, None)
}

/// Registers an Evolver strategy Genome with caller-chosen
/// `generation_count`/`experiment_allocation` knobs and an optional declared
/// `parent_strategy_id`, so a test can build a descendant strategy alongside
/// its ancestor.
fn register_meta_strategy_ex(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    label: &str,
    generation_count: u32,
    experiment_allocation: u64,
    parent_strategy_id: Option<&str>,
) -> String {
    let parent_entry = parent_strategy_id.map_or_else(String::new, |parent_id| {
        format!(r#","parent_strategy_id":"{parent_id}""#)
    });
    let path = directory.path().join(format!("strategy-{label}.json"));
    fs::write(
        &path,
        format!(
            r#"{{"schema_version":1,"name":"strategy-{label}","mutation_prioritization":"fifo","generation_count":{generation_count},"experiment_allocation":{experiment_allocation},"candidate_count":1,"gene_selection":"none"{parent_entry}}}"#
        ),
    )
    .expect("write strategy source");
    let Some(ResponseData::MetaStrategy { strategy }) = dispatch_call(
        plane,
        token,
        &format!("meta-strategy-{label}"),
        Command::MetaStrategyRegister {
            path: path.display().to_string(),
        },
    )
    .data
    else {
        panic!("strategy registration should succeed");
    };
    strategy.strategy_id
}

#[test]
#[allow(clippy::too_many_lines, clippy::similar_names)]
fn meta_evaluate_runs_two_lineages_and_records_a_replay_verified_receipt() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent_a, _candidate_a) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let world_a = parent_a.world_id.clone();
    let (world_b_record, parent_b, _candidate_b) =
        register_second_meta_lineage(&mut plane, &token, &directory);
    let world_b = world_b_record.world_id.clone();

    let strategy_a_id = register_meta_strategy(&mut plane, &token, &directory, "a");
    let strategy_b_id = register_meta_strategy(&mut plane, &token, &directory, "b");
    assert_ne!(
        strategy_a_id, strategy_b_id,
        "distinct content, distinct identity"
    );

    let evaluate = dispatch_call(
        &mut plane,
        &token,
        "meta-evaluate",
        Command::MetaEvaluate {
            meta_run_id: "meta-1".to_owned(),
            strategy_a_id: strategy_a_id.clone(),
            strategy_b_id: strategy_b_id.clone(),
            lineages: vec![
                MetaLineageSpec {
                    world_id: world_a.clone(),
                    from_genome_id: parent_a.genome_id.clone(),
                },
                MetaLineageSpec {
                    world_id: world_b.clone(),
                    from_genome_id: parent_b.genome_id.clone(),
                },
            ],
            confidence_bps: 9_500,
            bootstrap_seed: 1,
        },
    );
    assert!(
        evaluate.error.is_none(),
        "meta evaluate failed: {:?}",
        evaluate.error
    );
    let Some(ResponseData::MetaEvaluation { receipt }) = evaluate.data else {
        panic!("meta evaluate should return the recorded receipt");
    };
    assert_eq!(receipt.payload.meta_run_id, "meta-1");
    assert_eq!(receipt.payload.lineages.len(), 2);
    // Both strategies declare the identical generation/budget knobs and only
    // the reference-operation-flip mutation exists, so this pair cannot show
    // a real efficiency difference; the interval should center on zero.
    assert_eq!(receipt.payload.quality_delta.estimate_x10000, 0);
    assert_eq!(receipt.payload.cost_delta.estimate_x10000, 0);

    // Re-running the same meta_run_id is idempotent and returns the exact
    // recorded receipt without redoing any lineage work.
    let repeat = dispatch_call(
        &mut plane,
        &token,
        "meta-evaluate-repeat",
        Command::MetaEvaluate {
            meta_run_id: "meta-1".to_owned(),
            strategy_a_id,
            strategy_b_id,
            lineages: vec![
                MetaLineageSpec {
                    world_id: world_a.clone(),
                    from_genome_id: parent_a.genome_id.clone(),
                },
                MetaLineageSpec {
                    world_id: world_b.clone(),
                    from_genome_id: parent_b.genome_id.clone(),
                },
            ],
            confidence_bps: 9_500,
            bootstrap_seed: 1,
        },
    );
    assert!(repeat.error.is_none());
    assert_eq!(repeat.data, Some(ResponseData::MetaEvaluation { receipt }));

    // The meta-evaluation left each lineage's Champion exactly where it
    // found it.
    for (world_id, from_genome_id) in [
        (world_a.clone(), parent_a.genome_id.clone()),
        (world_b.clone(), parent_b.genome_id.clone()),
    ] {
        let Some(ResponseData::Champion { champion }) = dispatch_call(
            &mut plane,
            &token,
            &format!("champion-after-{world_id}"),
            Command::ChampionShow { world_id },
        )
        .data
        else {
            panic!("champion show should succeed");
        };
        assert_eq!(champion.champion_genome_id, Some(from_genome_id));
    }

    // A full verified replay accepts the meta-evolution events.
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("replay accepts meta-evolution history");
    assert!(
        history
            .iter()
            .any(|event| event.event_type == META_EVALUATION_EVENT_TYPE)
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_type == META_STRATEGY_EVENT_TYPE)
            .count(),
        2
    );
}

#[test]
fn meta_strategy_register_is_idempotent_and_content_addressed() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, _parent, _candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();

    let first_id = register_meta_strategy(&mut plane, &token, &directory, "idempotent");
    let second_id = register_meta_strategy(&mut plane, &token, &directory, "idempotent");
    assert_eq!(first_id, second_id, "identical content registers once");

    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("replay verified history");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_type == META_STRATEGY_EVENT_TYPE)
            .count(),
        1,
        "re-registering identical content appends no second event"
    );
}

#[test]
#[allow(clippy::similar_names)]
fn meta_evaluate_rejects_unregistered_strategies_and_duplicate_lineage_worlds() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent, _candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let (world_b_record, parent_b, _candidate_b) =
        register_second_meta_lineage(&mut plane, &token, &directory);
    let strategy_a_id = register_meta_strategy(&mut plane, &token, &directory, "solo-a");
    let strategy_b_id = register_meta_strategy(&mut plane, &token, &directory, "solo-b");

    // Two distinct-World lineages pass field validation, so this fails
    // inside the handler once it resolves `strategy_b_id`.
    let missing_strategy = dispatch_call(
        &mut plane,
        &token,
        "meta-missing-strategy",
        Command::MetaEvaluate {
            meta_run_id: "meta-missing".to_owned(),
            strategy_a_id: strategy_a_id.clone(),
            strategy_b_id: "hephaestus:meta-strategy:missing".to_owned(),
            lineages: vec![
                MetaLineageSpec {
                    world_id: parent.world_id.clone(),
                    from_genome_id: parent.genome_id.clone(),
                },
                MetaLineageSpec {
                    world_id: world_b_record.world_id.clone(),
                    from_genome_id: parent_b.genome_id.clone(),
                },
            ],
            confidence_bps: 9_500,
            bootstrap_seed: 0,
        },
    );
    assert_eq!(
        missing_strategy.error.expect("not found").code,
        ApiErrorCode::NotFound
    );

    // Two distinct, registered strategies but the same World twice fails
    // request-field validation before any lineage is ever run.
    let duplicate_world = dispatch_call(
        &mut plane,
        &token,
        "meta-duplicate-world",
        Command::MetaEvaluate {
            meta_run_id: "meta-duplicate".to_owned(),
            strategy_a_id,
            strategy_b_id,
            lineages: vec![
                MetaLineageSpec {
                    world_id: parent.world_id.clone(),
                    from_genome_id: parent.genome_id.clone(),
                },
                MetaLineageSpec {
                    world_id: parent.world_id.clone(),
                    from_genome_id: parent.genome_id.clone(),
                },
            ],
            confidence_bps: 9_500,
            bootstrap_seed: 0,
        },
    );
    assert_eq!(
        duplicate_world.error.expect("invalid").code,
        ApiErrorCode::InvalidRequest
    );
}

#[test]
fn meta_strategy_lineage_is_recorded_and_replay_verified() {
    let directory = tempdir().expect("daemon directory");
    let (mut plane, _parent, _candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();

    let ancestor_id =
        register_meta_strategy_ex(&mut plane, &token, &directory, "ancestor", 3, 6, None);
    let descendant_id = register_meta_strategy_ex(
        &mut plane,
        &token,
        &directory,
        "descendant",
        1,
        2,
        Some(&ancestor_id),
    );
    assert_ne!(
        ancestor_id, descendant_id,
        "a declared parent changes content identity"
    );

    let Some(ResponseData::MetaStrategy { strategy }) = dispatch_call(
        &mut plane,
        &token,
        "meta-strategy-show-descendant",
        Command::MetaStrategyShow {
            strategy_id: descendant_id.clone(),
        },
    )
    .data
    else {
        panic!("descendant strategy show should succeed");
    };
    assert_eq!(
        strategy.config.parent_strategy_id.as_deref(),
        Some(ancestor_id.as_str())
    );

    // An unregistered parent is rejected before any event is appended.
    let path = directory.path().join("strategy-orphan.json");
    fs::write(
        &path,
        r#"{"schema_version":1,"name":"strategy-orphan","mutation_prioritization":"fifo","generation_count":1,"experiment_allocation":2,"candidate_count":1,"gene_selection":"none","parent_strategy_id":"hephaestus:meta-strategy:missing"}"#,
    )
    .expect("write orphan strategy source");
    let orphan = dispatch_call(
        &mut plane,
        &token,
        "meta-strategy-orphan",
        Command::MetaStrategyRegister {
            path: path.display().to_string(),
        },
    );
    assert_eq!(
        orphan.error.expect("not found").code,
        ApiErrorCode::NotFound
    );

    // A full verified replay accepts the recorded lineage edge.
    plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("replay accepts a declared, registered parent strategy");
}

#[test]
#[allow(clippy::too_many_lines, clippy::similar_names)]
fn meta_evaluate_shows_a_descendant_strategy_reaching_equal_champions_at_lower_cost() {
    // The ancestor strategy runs two generations at four paired trials of
    // budget; the descendant declares the ancestor as its parent and runs
    // exactly the one generation the deterministic reference-operation-flip
    // mutation can ever usefully spend on these lineages (their parent
    // Genome's `identity` operation is wrong against uppercase-expecting
    // tasks, so generation zero's flip to `ascii_uppercase` is both the
    // first and the only ever-promoted mutation; every lineage's Champion
    // is already optimal afterward, so further generations can only churn
    // without promoting). Both strategies therefore reach an equally
    // already-optimal Champion on every lineage (one promotion each; the
    // two final Genome identities differ only because each strategy's own
    // evolve run content-addresses its proposals by that run's own run ID):
    // equal quality at a strictly lower, deterministic cost for the
    // descendant.
    let directory = tempdir().expect("daemon directory");
    let (mut plane, parent_1, _candidate_1) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    let world_1 = parent_1.world_id.clone();
    let (world_2_record, parent_2, _candidate_2) = register_meta_lineage(
        &mut plane,
        &token,
        &directory,
        "descendant-lineage-2",
        Some(CLEAN_INVARIANTS),
    );
    let (world_3_record, parent_3, _candidate_3) = register_meta_lineage(
        &mut plane,
        &token,
        &directory,
        "descendant-lineage-3",
        Some(CLEAN_INVARIANTS),
    );
    let world_2 = world_2_record.world_id.clone();
    let world_3 = world_3_record.world_id.clone();

    let ancestor_id =
        register_meta_strategy_ex(&mut plane, &token, &directory, "cost-ancestor", 2, 4, None);
    let descendant_id = register_meta_strategy_ex(
        &mut plane,
        &token,
        &directory,
        "cost-descendant",
        1,
        2,
        Some(&ancestor_id),
    );

    let lineages = vec![
        MetaLineageSpec {
            world_id: world_1.clone(),
            from_genome_id: parent_1.genome_id.clone(),
        },
        MetaLineageSpec {
            world_id: world_2.clone(),
            from_genome_id: parent_2.genome_id.clone(),
        },
        MetaLineageSpec {
            world_id: world_3.clone(),
            from_genome_id: parent_3.genome_id.clone(),
        },
    ];

    let evaluate = dispatch_call(
        &mut plane,
        &token,
        "meta-evaluate-descendant",
        Command::MetaEvaluate {
            meta_run_id: "meta-descendant-1".to_owned(),
            strategy_a_id: ancestor_id.clone(),
            strategy_b_id: descendant_id.clone(),
            lineages,
            confidence_bps: 9_500,
            bootstrap_seed: 3,
        },
    );
    assert!(
        evaluate.error.is_none(),
        "meta evaluate failed: {:?}",
        evaluate.error
    );
    let Some(ResponseData::MetaEvaluation { receipt }) = evaluate.data else {
        panic!("meta evaluate should return the recorded receipt");
    };
    assert_eq!(
        receipt.payload.lineages.len(),
        3,
        "at least 3 held-out lineages"
    );

    // Equal Champion quality: both strategies promote exactly once per
    // lineage (the one beneficial mutation; promoted-generation count is
    // this receipt's documented quality proxy). The two final Champion
    // Genome identities differ, because each strategy's own evolve run
    // content-addresses its proposals by that run's own run ID, but both
    // are the same corrected reference operation reached in generation
    // zero and neither ever regresses from it.
    for lineage in &receipt.payload.lineages {
        assert_eq!(lineage.strategy_a_promotions, 1);
        assert_eq!(lineage.strategy_b_promotions, 1);
        assert_ne!(
            lineage.strategy_a_champion_genome_id, lineage.from_genome_id,
            "the ancestor's Champion actually promoted away from the starting Genome"
        );
        assert_ne!(
            lineage.strategy_b_champion_genome_id, lineage.from_genome_id,
            "the descendant's Champion actually promoted away from the starting Genome"
        );
        // Ancestor spends its full two-generation budget; descendant
        // spends exactly the one generation that ever promotes.
        assert_eq!(lineage.strategy_a_trials_consumed, 4);
        assert_eq!(lineage.strategy_b_trials_consumed, 2);
    }
    assert_eq!(receipt.payload.quality_delta.estimate_x10000, 0);
    assert_eq!(receipt.payload.quality_delta.lower_x10000, 0);
    assert_eq!(receipt.payload.quality_delta.upper_x10000, 0);

    // Statistically lower experiment cost: the upper bound of the
    // (descendant-minus-ancestor) cost-delta confidence interval is below
    // zero. The receipt orients cost_delta as b-a = descendant-ancestor
    // here since the descendant is strategy B.
    assert!(
        receipt.payload.cost_delta.upper_x10000 < 0,
        "cost delta upper bound should be below zero: {:?}",
        receipt.payload.cost_delta
    );

    assert_eq!(
        receipt.payload.descendant_cheaper_at_equal_quality,
        Some(true),
        "descendant should be verdicted cheaper at equal quality: {receipt:?}"
    );

    // A full verified replay recomputes the same verdict from the recorded
    // strategies and lineage outcomes.
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("replay accepts the descendant meta-evaluation");
    assert!(
        history
            .iter()
            .any(|event| event.event_type == META_EVALUATION_EVENT_TYPE)
    );
}

// ---------------------------------------------------------------------
// Gene Bank
// ---------------------------------------------------------------------

/// Opens a fresh real-worker control plane with no Worlds or Genomes
/// registered, mirroring the boilerplate in
/// `real_worker_arena_fixture_with_invariants` without its fixed
/// single-task World.
fn gene_bank_plane_fixture(directory: &TempDir) -> ControlPlane {
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"Gene Bank fixture\n")
        .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);

    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(
        cargo_evaluator.is_file(),
        "missing evaluator {cargo_evaluator:?}"
    );
    let evaluator = directory.path().join("gene-bank-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(worker.is_file(), "missing worker {worker:?}");
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open Gene Bank fixture");
    let token = plane.token_hex.clone();
    assert!(
        dispatch_call(&mut plane, &token, "gene-bank-unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );
    plane
}

/// Registers one Arena World whose visible tasks are exactly
/// `visible_tasks` (id, input, expected). Every World gets its own sealed
/// task and its own evaluator-artifact identities.
fn gene_bank_world(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    request_id: &str,
    world_name: &str,
    visible_tasks: &[(&str, &str, &str)],
) -> WorldRecord {
    // The sealed task must agree with the visible tasks about whether
    // uppercase helps or hurts: mixing the two would dilute a domain's
    // correctness signal toward neutral regardless of the visible outcome.
    let sealed_task = visible_tasks
        .first()
        .copied()
        .unwrap_or(("sealed-task", "sealed", "SEALED"));
    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    let visible = TrustedManifest::new(
        format!("{world_name}-visible"),
        Visibility::Visible,
        visible_tasks
            .iter()
            .map(|(id, input, expected)| {
                TrustedTask::new(*id, *input, *expected).expect("visible task")
            })
            .collect::<Vec<_>>(),
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        format!("{world_name}-sealed"),
        Visibility::Sealed,
        vec![TrustedTask::new("sealed-task", sealed_task.1, sealed_task.2).expect("sealed task")],
    )
    .expect("sealed manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator = env::current_exe()
        .expect("locate test executable")
        .parent()
        .and_then(Path::parent)
        .expect("locate Cargo binary directory")
        .join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
    let evaluator_id = artifacts
        .put(&fs::read(evaluator).expect("read reference evaluator"))
        .expect("store evaluator identity");
    let verifier_id = artifacts
        .put(&plane.run_result_verifier.public_key_bytes())
        .expect("store result verifier");
    let invariant_id = artifacts
        .put(CLEAN_INVARIANTS)
        .expect("store invariant manifest");
    drop(artifacts);
    let world_path = directory.path().join(format!("{world_name}-world.json"));
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"{world_name}","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":["harness"],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}","arena.invariant_manifest":"{}"}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
            invariant_id.as_str(),
        ),
    )
    .expect("write Gene Bank World");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        request_id,
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("Gene Bank World registration should succeed");
    };
    world
}

/// Registers one identity-operation Genome under `world_id`.
fn gene_bank_genome(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    world_id: &str,
    name: &str,
    parents: &str,
) -> GenomeRecord {
    let path = directory.path().join(format!("{name}.md"));
    fs::write(
        &path,
        format!(
            "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
        ),
    )
    .expect("write Genome source");
    let Some(ResponseData::Genome { genome }) = dispatch_call(
        plane,
        token,
        &format!("register-{name}"),
        Command::GenomeRegister {
            path: path.display().to_string(),
            world_id: world_id.to_owned(),
        },
    )
    .data
    else {
        panic!("Gene Bank Genome registration should succeed");
    };
    genome
}

/// Transfers `gene_id` onto `to_genome_id`, evaluates the recipient against
/// the transfer child, and records the effect. Returns the recorded outcome.
fn gene_bank_transfer_and_record(
    plane: &mut ControlPlane,
    trial_id: &str,
    gene_id: &str,
    to_genome_id: &str,
) -> GeneTransferOutcome {
    let ResponseData::GeneTransfer { trial } = plane
        .gene_transfer_apply(trial_id, gene_id, to_genome_id)
        .expect("apply Gene transfer")
    else {
        panic!("transfer apply should return its durable record");
    };
    let evaluation_id = format!("{trial_id}-eval");
    complete_arena_test_job(
        plane,
        &evaluation_id,
        to_genome_id,
        &trial.applied.child.genome_id,
    );
    plane
        .select_arena_evaluation(&evaluation_id)
        .expect("select transfer trial evaluation");
    let ResponseData::GeneTransfer { trial: recorded } = plane
        .gene_transfer_record(trial_id, &evaluation_id)
        .expect("record Gene transfer effect")
    else {
        panic!("transfer record should return its durable record");
    };
    recorded
        .recorded
        .expect("a recorded transfer trial carries its outcome")
        .outcome
}

/// Builds the origin World A, promotes an `identity` -> `ascii_uppercase`
/// Champion transition with `visible_task_count` measured paired trials,
/// and extracts a Gene from it.
fn gene_bank_origin(
    plane: &mut ControlPlane,
    token: &str,
    directory: &TempDir,
    gene_id: &str,
    visible_task_count: usize,
) -> GeneRecord {
    let tasks: Vec<(String, String, String)> = (0..visible_task_count)
        .map(|index| {
            let word = format!("word{index}");
            (
                format!("origin-task-{index}"),
                word.clone(),
                word.to_uppercase(),
            )
        })
        .collect();
    let task_refs: Vec<(&str, &str, &str)> = tasks
        .iter()
        .map(|(id, input, expected)| (id.as_str(), input.as_str(), expected.as_str()))
        .collect();
    let world = gene_bank_world(
        plane,
        token,
        directory,
        "origin-world",
        "origin",
        &task_refs,
    );
    let origin_parent = gene_bank_genome(
        plane,
        token,
        directory,
        &world.world_id,
        "origin-parent",
        "[]",
    );
    let origin_candidate = gene_bank_genome(
        plane,
        token,
        directory,
        &world.world_id,
        "origin-candidate",
        &format!("[\"{}\"]", origin_parent.genome_id),
    );
    let assessed = assessed_forge_child(
        plane,
        "origin",
        &origin_parent.genome_id,
        &origin_candidate.genome_id,
        true,
    );
    plane
        .check_arena_invariants(&assessed.evaluation)
        .expect("check origin invariant evidence");
    champion_transition(
        plane,
        token,
        "origin-seed",
        Command::ChampionSeed {
            transition_id: "origin-seed".to_owned(),
            world_id: assessed.world.clone(),
            genome_id: origin_candidate.genome_id.clone(),
            reason: "bootstrap origin Champion".to_owned(),
        },
    )
    .expect("seed origin Champion");
    champion_transition(
        plane,
        token,
        "origin-promote",
        Command::ChampionPromote {
            transition_id: "origin-promote".to_owned(),
            assessment_id: "origin-assessment".to_owned(),
        },
    )
    .expect("promote origin Champion");
    let ResponseData::Gene { gene } = plane
        .gene_extract(gene_id, "origin-promote")
        .expect("extract Gene from a promoted, evidence-bound transition")
    else {
        panic!("gene extraction should return its durable record");
    };
    assert_eq!(gene.payload.operation_before, "identity");
    assert_eq!(gene.payload.operation_after, "ascii_uppercase");
    assert_eq!(gene.payload.world_id, world.world_id);
    assert!(gene.payload.evidence_trials >= GENE_MIN_EVIDENCE_TRIALS);
    *gene
}

#[test]
fn gene_extraction_refuses_below_the_evidence_threshold_and_bad_input() {
    let directory = tempdir().expect("Gene extraction refusal fixture");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();

    for (request_id, gene_id, promotion_transition_id, expected) in [
        (
            "extract-bad-gene-id",
            "bad id",
            "promotion",
            "gene_id is invalid",
        ),
        (
            "extract-bad-promotion-id",
            "gene",
            "bad id",
            "promotion_transition_id is invalid",
        ),
    ] {
        let response = dispatch_call(
            &mut plane,
            &token,
            request_id,
            Command::GeneExtract {
                gene_id: gene_id.to_owned(),
                promotion_transition_id: promotion_transition_id.to_owned(),
            },
        );
        assert!(matches!(
            response.error,
            Some(error) if error.code == ApiErrorCode::InvalidRequest && error.message == expected
        ));
    }

    assert!(matches!(
        plane.gene_extract("missing-gene", "missing-promotion"),
        Err(ExecuteError::NotFound)
    ));

    // The default fixture's single visible task always yields exactly one
    // measured paired trial, below `GENE_MIN_EVIDENCE_TRIALS`.
    let assessed = assessed_forge_child(
        &mut plane,
        "thin",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    plane
        .check_arena_invariants(&assessed.evaluation)
        .expect("check thin invariant evidence");
    champion_transition(
        &mut plane,
        &token,
        "thin-seed",
        Command::ChampionSeed {
            transition_id: "thin-seed".to_owned(),
            world_id: assessed.world.clone(),
            genome_id: initial_candidate.genome_id.clone(),
            reason: "bootstrap thin Champion".to_owned(),
        },
    )
    .expect("seed thin Champion");
    champion_transition(
        &mut plane,
        &token,
        "thin-promote",
        Command::ChampionPromote {
            transition_id: "thin-promote".to_owned(),
            assessment_id: "thin-assessment".to_owned(),
        },
    )
    .expect("promote thin Champion");

    match plane.gene_extract("thin-gene", "thin-promote") {
        Err(ExecuteError::Rejected(message)) => {
            assert!(
                message.contains("measured paired trials"),
                "unexpected message: {message}"
            );
        }
        other => panic!("thin evidence must refuse extraction: {other:?}"),
    }
    assert!(
        plane.state.registered.genome(&assessed.child).is_some(),
        "a refused extraction must not roll back the promoted Champion"
    );

    // Extracting from a seed (not a promotion) is refused too.
    match plane.gene_extract("seed-gene", "thin-seed") {
        Err(ExecuteError::Rejected(message)) => {
            assert!(
                message.contains("promotion"),
                "unexpected message: {message}"
            );
        }
        other => panic!("a seed transition must refuse Gene extraction: {other:?}"),
    }

    let history = gene_bank_history(&plane);
    assert!(
        !history
            .iter()
            .any(|event| event.event_type == GENE_EVENT_TYPE),
        "a refused extraction must not record a Gene"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn gene_transfer_trials_record_contradiction_and_speciation() {
    let directory = tempdir().expect("Gene Bank transfer fixture");
    let mut plane = gene_bank_plane_fixture(&directory);
    let token = plane.token_hex.clone();

    let gene = gene_bank_origin(&mut plane, &token, &directory, "uppercase-gene", 3);

    // Re-extracting the same gene_id from a different promotion fails closed.
    assert!(matches!(
        plane.gene_extract(&gene.payload.gene_id, "a-different-promotion"),
        Err(ExecuteError::Rejected(_))
    ));

    // World B: uppercase helps. Three distinct lineages (b1, b2, b3), all positive.
    let helps_world = gene_bank_world(
        &mut plane,
        &token,
        &directory,
        "helps-world",
        "helps",
        &[("helps-task", "delta", "DELTA")],
    );
    let b1 = gene_bank_genome(
        &mut plane,
        &token,
        &directory,
        &helps_world.world_id,
        "b1",
        "[]",
    );
    let b2 = gene_bank_genome(
        &mut plane,
        &token,
        &directory,
        &helps_world.world_id,
        "b2",
        "[]",
    );
    let b3 = gene_bank_genome(
        &mut plane,
        &token,
        &directory,
        &helps_world.world_id,
        "b3",
        "[]",
    );

    // World C: uppercase hurts (an exact-case match task).
    let hurts_world = gene_bank_world(
        &mut plane,
        &token,
        &directory,
        "hurts-world",
        "hurts",
        &[("hurts-task", "MixedCase", "MixedCase")],
    );
    let c1 = gene_bank_genome(
        &mut plane,
        &token,
        &directory,
        &hurts_world.world_id,
        "c1",
        "[]",
    );

    // Transfer trials: an unknown Gene or recipient Genome is refused.
    assert!(matches!(
        plane.gene_transfer_apply("transfer-missing-gene", "missing-gene", &b1.genome_id),
        Err(ExecuteError::NotFound)
    ));
    assert!(matches!(
        plane.gene_transfer_apply(
            "transfer-missing-genome",
            &gene.payload.gene_id,
            "missing-genome"
        ),
        Err(ExecuteError::NotFound)
    ));

    let outcome_b1 = gene_bank_transfer_and_record(
        &mut plane,
        "transfer-b1",
        &gene.payload.gene_id,
        &b1.genome_id,
    );
    assert_eq!(outcome_b1, GeneTransferOutcome::Positive);
    let outcome_b2 = gene_bank_transfer_and_record(
        &mut plane,
        "transfer-b2",
        &gene.payload.gene_id,
        &b2.genome_id,
    );
    assert_eq!(outcome_b2, GeneTransferOutcome::Positive);

    // Two distinct positive lineages are not enough for speciation.
    match plane.gene_speciate(
        "species-too-few",
        &gene.payload.gene_id,
        &helps_world.world_id,
    ) {
        Err(ExecuteError::Rejected(message)) => {
            assert!(
                message.contains("distinct positive lineage"),
                "unexpected message: {message}"
            );
        }
        other => panic!("two lineages must refuse speciation: {other:?}"),
    }

    let outcome_hurts = gene_bank_transfer_and_record(
        &mut plane,
        "transfer-c1",
        &gene.payload.gene_id,
        &c1.genome_id,
    );
    assert_eq!(
        outcome_hurts,
        GeneTransferOutcome::Negative,
        "negative transfer is retained, never dropped"
    );

    // Reusing a trial id with a different recipient fails closed.
    assert!(matches!(
        plane.gene_transfer_apply("transfer-b1", &gene.payload.gene_id, &c1.genome_id),
        Err(ExecuteError::Rejected(_))
    ));

    // A recipient that does not currently carry the Gene's origin operation
    // cannot receive the transfer.
    let already_uppercase_path = directory.path().join("already-uppercase.md");
    fs::write(
        &already_uppercase_path,
        "---\nschema_version: 1\nname: already-uppercase\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"ascii_uppercase\"}\n```\n",
    )
    .expect("write already-uppercase Genome source");
    let Some(ResponseData::Genome {
        genome: already_uppercase,
    }) = dispatch_call(
        &mut plane,
        &token,
        "register-already-uppercase",
        Command::GenomeRegister {
            path: already_uppercase_path.display().to_string(),
            world_id: helps_world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("already-uppercase Genome registration should succeed");
    };
    assert!(matches!(
        plane.gene_transfer_apply(
            "transfer-already-uppercase",
            &gene.payload.gene_id,
            &already_uppercase.genome_id
        ),
        Err(ExecuteError::Rejected(_))
    ));

    // The three-lineage transfer trials above (two positive, one negative)
    // are enough for the Gene's contradiction to be recorded automatically.
    let aggregate = gene_aggregate(&gene_bank_history(&plane), &gene.payload.gene_id)
        .expect("aggregate the Gene's transfer trials");
    let contradiction = aggregate
        .contradiction
        .expect("a positive and a negative lineage must produce a contradiction record");
    assert_eq!(
        contradiction.payload.positive_world_id,
        helps_world.world_id
    );
    assert_eq!(
        contradiction.payload.negative_world_id,
        hurts_world.world_id
    );

    // Recording another trial never overwrites the existing contradiction.
    let outcome_b3 = gene_bank_transfer_and_record(
        &mut plane,
        "transfer-b3",
        &gene.payload.gene_id,
        &b3.genome_id,
    );
    assert_eq!(outcome_b3, GeneTransferOutcome::Positive);
    let aggregate_again = gene_aggregate(&gene_bank_history(&plane), &gene.payload.gene_id)
        .expect("re-aggregate the Gene's transfer trials");
    assert_eq!(
        aggregate_again
            .contradiction
            .expect("contradiction persists"),
        contradiction
    );

    // A domain with a recorded negative can never admit a species.
    match plane.gene_speciate(
        "species-hurts",
        &gene.payload.gene_id,
        &hurts_world.world_id,
    ) {
        Err(ExecuteError::Rejected(message)) => {
            assert!(
                message.contains("negative"),
                "unexpected message: {message}"
            );
        }
        other => panic!("a domain with a recorded negative must refuse speciation: {other:?}"),
    }

    // The helps domain now has three distinct positive lineages: admitted.
    let ResponseData::GeneSpecies { species } = plane
        .gene_speciate(
            "species-helps",
            &gene.payload.gene_id,
            &helps_world.world_id,
        )
        .expect("admit a species from persistent, significant domain advantage")
    else {
        panic!("speciation should return its durable record");
    };
    assert_eq!(species.payload.gene_id, gene.payload.gene_id);
    assert_eq!(species.payload.domain_world_id, helps_world.world_id);
    assert_eq!(species.payload.lineage_genome_ids.len(), 3);
    assert!(species.payload.average_estimate_bps >= SPECIATION_MIN_EFFECT_BPS);

    // Idempotent retry returns the identical recorded species.
    let retry = plane
        .gene_speciate(
            "species-helps",
            &gene.payload.gene_id,
            &helps_world.world_id,
        )
        .expect("idempotent speciation retry");
    assert!(matches!(
        retry,
        ResponseData::GeneSpecies { species: retried } if *retried == *species
    ));
    assert!(matches!(
        plane.gene_speciate(
            "species-helps",
            &gene.payload.gene_id,
            &hurts_world.world_id
        ),
        Err(ExecuteError::Rejected(_))
    ));

    // `gene show` and `gene list` report aggregates across the four lineages.
    let ResponseData::GeneAggregate { aggregate } = plane
        .gene_show(&gene.payload.gene_id)
        .expect("show the full Gene aggregate")
    else {
        panic!("gene show should return the full aggregate");
    };
    assert_eq!(aggregate.transfers.len(), 4);
    assert_eq!(aggregate.species.len(), 1);
    assert!(aggregate.contradiction.is_some());

    let ResponseData::Genes { genes } = plane.gene_list().expect("list every extracted Gene")
    else {
        panic!("gene list should return every Gene summary");
    };
    let summary = genes
        .into_iter()
        .find(|summary| summary.payload.gene_id == gene.payload.gene_id)
        .expect("the extracted Gene is listed");
    assert_eq!(summary.lineages, 4);
    assert_eq!(summary.positive, 3);
    assert_eq!(summary.negative, 1);
    assert_eq!(summary.neutral, 0);
    assert!(summary.contradiction);
    assert_eq!(summary.species_ids, vec!["species-helps".to_owned()]);

    assert!(matches!(
        plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
    verify_gene_bank_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &gene_bank_history(&plane),
        &plane.state.registered,
    )
    .expect("canonical Gene Bank history verifies");
}

fn gene_bank_history(plane: &ControlPlane) -> Vec<StoredEvent> {
    plane
        .storage
        .as_ref()
        .expect("canonical Gene Bank ledger")
        .ledger
        .replay_verified()
        .expect("verify Gene Bank history")
}

fn gene_bank_history_with_payload_edit<P, F>(
    history: &[StoredEvent],
    event_id: &str,
    edit: F,
) -> Vec<StoredEvent>
where
    P: serde::Serialize + serde::de::DeserializeOwned,
    F: FnOnce(&mut P),
{
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == event_id)
        .expect("Gene Bank event exists");
    let mut payload: P = serde_json::from_slice(&event.payload).expect("decode Gene Bank payload");
    edit(&mut payload);
    let canonical = serde_json::to_value(&payload).expect("canonicalize Gene Bank payload");
    event.payload = serde_json::to_vec(&canonical).expect("encode Gene Bank payload");
    tampered
}

#[test]
#[allow(clippy::too_many_lines)]
fn gene_bank_history_rejects_tampering_retyping_and_reordering() {
    let directory = tempdir().expect("Gene Bank tamper fixture");
    let mut plane = gene_bank_plane_fixture(&directory);
    let token = plane.token_hex.clone();

    let gene = gene_bank_origin(&mut plane, &token, &directory, "tamper-gene", 3);
    let helps_world = gene_bank_world(
        &mut plane,
        &token,
        &directory,
        "tamper-helps-world",
        "tamper-helps",
        &[("tamper-task", "delta", "DELTA")],
    );
    let recipient = gene_bank_genome(
        &mut plane,
        &token,
        &directory,
        &helps_world.world_id,
        "tamper-recipient",
        "[]",
    );
    gene_bank_transfer_and_record(
        &mut plane,
        "tamper-transfer",
        &gene.payload.gene_id,
        &recipient.genome_id,
    );

    let history = gene_bank_history(&plane);
    verify_gene_bank_history(
        &plane.storage.as_ref().unwrap().artifacts,
        &history,
        &plane.state.registered,
    )
    .expect("canonical Gene Bank history verifies before tampering");

    let tampered_gene_event_id = gene_event_id(&gene.payload.gene_id);
    let tampered_gene = gene_bank_history_with_payload_edit::<GeneExtractedPayload, _>(
        &history,
        &tampered_gene_event_id,
        |payload| payload.evidence_trials = 999,
    );
    assert!(
        verify_gene_bank_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &tampered_gene,
            &plane.state.registered
        )
        .is_err(),
        "a tampered Gene payload must fail replay"
    );

    let applied_event_id = transfer_applied_event_id("tamper-transfer");
    let tampered_applied = gene_bank_history_with_payload_edit::<GeneTransferAppliedPayload, _>(
        &history,
        &applied_event_id,
        |payload| payload.to_genome_id = gene.payload.origin_parent_genome_id.clone(),
    );
    assert!(
        verify_gene_bank_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &tampered_applied,
            &plane.state.registered
        )
        .is_err(),
        "a tampered transfer applied payload must fail replay"
    );

    let recorded_event_id = transfer_recorded_event_id("tamper-transfer");
    let tampered_recorded = gene_bank_history_with_payload_edit::<GeneTransferRecordedPayload, _>(
        &history,
        &recorded_event_id,
        |payload| payload.outcome = GeneTransferOutcome::Negative,
    );
    assert!(
        verify_gene_bank_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &tampered_recorded,
            &plane.state.registered
        )
        .is_err(),
        "a tampered transfer outcome must fail replay"
    );

    let mut noncanonical = history.clone();
    noncanonical
        .iter_mut()
        .find(|event| event.event_id == tampered_gene_event_id)
        .expect("Gene event")
        .payload
        .push(b' ');
    assert!(
        verify_gene_bank_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &noncanonical,
            &plane.state.registered
        )
        .is_err(),
        "a noncanonical Gene payload must fail replay"
    );

    // ID-prefix detection of a retyped event: nothing later depends on the
    // transfer-recorded event, so only identity-based detection can reject
    // its rewritten event type.
    let mut retyped = history.clone();
    retyped
        .iter_mut()
        .find(|event| event.event_id == recorded_event_id)
        .expect("transfer recorded event")
        .event_type = "gene.rewritten".to_owned();
    assert!(
        verify_gene_bank_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &retyped,
            &plane.state.registered
        )
        .is_err(),
        "a retyped Gene Bank event must still be rejected by ID-prefix detection"
    );

    // Reordering: the applied trial cannot follow the recorded effect it
    // produced.
    let mut reordered = history;
    let applied_index = reordered
        .iter()
        .position(|event| event.event_id == applied_event_id)
        .expect("applied event");
    let applied_event = reordered.remove(applied_index);
    reordered.push(applied_event);
    assert!(
        verify_gene_bank_history(
            &plane.storage.as_ref().unwrap().artifacts,
            &reordered,
            &plane.state.registered
        )
        .is_err(),
        "a transfer record cannot precede the trial it applies to"
    );
}

// ---------------------------------------------------------------------
// MCP gateway: capability-policy denial and allowed dispatch are ledgered
// through the ordinary authenticated command path (roadmap item 14).
// ---------------------------------------------------------------------

#[test]
fn gateway_mcp_call_denied_is_ledgered_without_dispatch() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();

    let response = dispatch_call(
        &mut plane,
        &token,
        "mcp-denied",
        Command::McpCall {
            client_id: "agent-1".to_owned(),
            tool: "arena_evaluate".to_owned(),
            tool_version: 1,
            decision: McpDecision::Denied {
                reason: "client is not granted this mutating tool".to_owned(),
            },
        },
    );
    assert_eq!(
        response.data,
        Some(ResponseData::McpDenied {
            reason: "client is not granted this mutating tool".to_owned()
        })
    );

    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after denied mcp call");
    let mcp_events: Vec<_> = history
        .iter()
        .filter(|event| event.event_type == "mcp.call")
        .collect();
    assert_eq!(
        mcp_events.len(),
        1,
        "the denial must be ledgered exactly once"
    );
    assert!(
        !history
            .iter()
            .any(|event| event.event_type == "control.evaluate_pair"),
        "a denied tool call must never dispatch its wrapped command"
    );

    let Some(ResponseData::DenialList { denials }) = dispatch_call(
        &mut plane,
        &token,
        "denials-after-mcp-denial",
        Command::DenialList { limit: 20 },
    )
    .data
    else {
        panic!("denial_list should succeed");
    };
    let mcp_denial = denials
        .iter()
        .find(|entry| entry.kind == DenialKind::McpCallDenied)
        .expect("mcp denial appears in the denial list");
    assert_eq!(mcp_denial.client_id.as_deref(), Some("agent-1"));
    assert_eq!(mcp_denial.command.as_deref(), Some("arena_evaluate"));
}

#[test]
fn gateway_mcp_call_allowed_routes_through_ordinary_command() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();

    let response = dispatch_call(
        &mut plane,
        &token,
        "mcp-allowed-status",
        Command::McpCall {
            client_id: "agent-1".to_owned(),
            tool: "status".to_owned(),
            tool_version: 1,
            decision: McpDecision::Allowed {
                command: Box::new(Command::Status),
            },
        },
    );
    assert!(matches!(response.data, Some(ResponseData::Status { .. })));

    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after allowed mcp call");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_type == "mcp.call")
            .count(),
        1,
        "the allowed call itself is ledgered once"
    );
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_type == "control.status")
            .count(),
        1,
        "the wrapped command is dispatched through its ordinary authenticated path"
    );
}

#[test]
fn gateway_mcp_call_rejects_nested_mcp_call() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();

    let response = dispatch_call(
        &mut plane,
        &token,
        "mcp-nested",
        Command::McpCall {
            client_id: "agent-1".to_owned(),
            tool: "status".to_owned(),
            tool_version: 1,
            decision: McpDecision::Allowed {
                command: Box::new(Command::McpCall {
                    client_id: "agent-1".to_owned(),
                    tool: "status".to_owned(),
                    tool_version: 1,
                    decision: McpDecision::Allowed {
                        command: Box::new(Command::Status),
                    },
                }),
            },
        },
    );
    assert_eq!(
        response.error.expect("nested mcp_call is rejected").code,
        ApiErrorCode::InvalidRequest
    );
}

// ---------------------------------------------------------------------
// Remote workers: scoped expiring credentials and idempotent leased
// execution of the existing isolated reference-worker transform
// (roadmap item 14).
// ---------------------------------------------------------------------

fn mint_worker_credential(
    plane: &mut ControlPlane,
    token: &str,
    worker_id: &str,
    ttl_seconds: u64,
) -> (String, String) {
    let Some(ResponseData::WorkerCredential {
        credential_id,
        token: worker_token,
        ..
    }) = dispatch_call(
        plane,
        token,
        &format!("mint-{worker_id}"),
        Command::WorkerCredentialMint {
            worker_id: worker_id.to_owned(),
            ttl_seconds,
        },
    )
    .data
    else {
        panic!("credential mint should succeed");
    };
    (credential_id, worker_token)
}

#[test]
fn worker_lease_and_result_round_trip_signs_and_records_the_output() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _prompt) = register_dispatch_objects(&mut plane, &token, &directory);
    let (_, worker_token) = mint_worker_credential(&mut plane, &token, "worker-1", 3_600);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-remote", Command::Unfreeze)
            .error
            .is_none()
    );

    let Some(ResponseData::RemoteJob { state, .. }) = dispatch_call(
        &mut plane,
        &token,
        "remote-submit",
        Command::RemoteRunSubmit {
            job_id: "remote-job-1".to_owned(),
            genome_id: genome.genome_id.clone(),
        },
    )
    .data
    else {
        panic!("remote run submit should succeed");
    };
    assert_eq!(state, RemoteJobState::Pending);

    let WorkerReply::Leased {
        job_id, frame_hex, ..
    } = plane.handle_worker_request(WorkerRequest::Lease {
        worker_id: "worker-1".to_owned(),
        token: worker_token.clone(),
    })
    else {
        panic!("a pending job should be leased");
    };
    assert_eq!(job_id, "remote-job-1");

    let frame = (0..frame_hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&frame_hex[index..index + 2], 16).expect("hex byte"))
        .collect::<Vec<u8>>();
    let output = hephaestus_runtime::execute_reference_worker_request(&frame)
        .expect("identity transform succeeds");
    assert_eq!(output, REMOTE_REFERENCE_PROMPT.as_bytes());
    let output_hex = hex_encode_bytes(&output);

    let WorkerReply::ResultAccepted {
        job_id: accepted_job_id,
    } = plane.handle_worker_request(WorkerRequest::SubmitResult {
        worker_id: "worker-1".to_owned(),
        token: worker_token.clone(),
        job_id: job_id.clone(),
        output_hex: output_hex.clone(),
        completion: RemoteCompletion::Success,
    })
    else {
        panic!("the signed result should be accepted");
    };
    assert_eq!(accepted_job_id, "remote-job-1");

    let Some(ResponseData::RemoteJob {
        state,
        completion_reason,
        ..
    }) = dispatch_call(
        &mut plane,
        &token,
        "remote-status",
        Command::RemoteJobStatus {
            job_id: "remote-job-1".to_owned(),
        },
    )
    .data
    else {
        panic!("remote job status should succeed");
    };
    assert_eq!(state, RemoteJobState::Succeeded);
    assert_eq!(completion_reason, Some(RunCompletionReason::Success));

    // Duplicate delivery is idempotent: the second submission must not
    // append a second signed result.
    let repeat = plane.handle_worker_request(WorkerRequest::SubmitResult {
        worker_id: "worker-1".to_owned(),
        token: worker_token,
        job_id,
        output_hex,
        completion: RemoteCompletion::Success,
    });
    assert!(matches!(repeat, WorkerReply::ResultAccepted { .. }));
    let history = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after duplicate delivery");
    assert_eq!(
        history
            .iter()
            .filter(|event| event.event_type == "run.result_recorded")
            .count(),
        1,
        "duplicate delivery must not record a second signed result"
    );
}

#[test]
fn worker_expired_credential_fails_closed() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (_, genome, _) = register_dispatch_objects(&mut plane, &token, &directory);
    let (_, worker_token) = mint_worker_credential(&mut plane, &token, "worker-expiring", 1);
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-expiring", Command::Unfreeze)
            .error
            .is_none()
    );
    dispatch_call(
        &mut plane,
        &token,
        "remote-submit-expiring",
        Command::RemoteRunSubmit {
            job_id: "remote-job-expiring".to_owned(),
            genome_id: genome.genome_id,
        },
    );
    std::thread::sleep(Duration::from_millis(1_100));

    let reply = plane.handle_worker_request(WorkerRequest::Lease {
        worker_id: "worker-expiring".to_owned(),
        token: worker_token,
    });
    assert!(
        matches!(reply, WorkerReply::Error { .. }),
        "an expired credential must fail closed"
    );
}

#[test]
fn worker_revoked_credential_fails_closed() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let token = plane.token_hex.clone();
    let (credential_id, worker_token) =
        mint_worker_credential(&mut plane, &token, "worker-revoked", 3_600);

    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "revoke",
            Command::WorkerCredentialRevoke { credential_id }
        )
        .error
        .is_none()
    );

    let reply = plane.handle_worker_request(WorkerRequest::Lease {
        worker_id: "worker-revoked".to_owned(),
        token: worker_token,
    });
    assert!(
        matches!(reply, WorkerReply::Error { .. }),
        "a revoked credential must fail closed"
    );
}

#[test]
fn worker_unknown_credential_fails_closed() {
    let directory = tempdir().expect("daemon directory");
    let mut plane = ControlPlane::open(directory.path()).expect("open control plane");
    let reply = plane.handle_worker_request(WorkerRequest::Lease {
        worker_id: "ghost".to_owned(),
        token: "ab".repeat(32),
    });
    assert!(matches!(reply, WorkerReply::Error { .. }));
}

#[test]
#[allow(clippy::too_many_lines)]
fn arena_paired_evaluation_admits_a_mixed_reference_parent_and_provider_candidate_selects_and_replays()
 {
    let directory = tempdir().expect("mixed Arena fixture");
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"Mixed Arena fixture\n")
        .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);

    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    let evaluator = directory.path().join("mixed-fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(worker.is_file(), "missing worker {worker:?}");
    let fake_claude = directory.path().join("mixed-fake-claude");
    write_fake_claude_binary(&fake_claude);

    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open mixed Arena fixture")
    .with_provider_executables_for_testing(
        "/nonexistent/codex-should-not-be-invoked",
        &fake_claude,
        Vec::new(),
    );
    let token = plane.token_hex.clone();

    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    let visible = TrustedManifest::new(
        "mixed-visible",
        Visibility::Visible,
        vec![
            hephaestus_arena::TrustedTask::new("visible-task", "visible", "VISIBLE")
                .expect("visible task"),
        ],
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        "mixed-sealed",
        Visibility::Sealed,
        vec![
            hephaestus_arena::TrustedTask::new("sealed-task", "sealed", "SEALED")
                .expect("sealed task"),
        ],
    )
    .expect("sealed manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator_id = artifacts
        .put(&fs::read(&evaluator).expect("read reference evaluator"))
        .expect("store evaluator identity");
    let verifier_id = artifacts
        .put(&plane.run_result_verifier.public_key_bytes())
        .expect("store result verifier");
    drop(artifacts);

    let world_path = directory.path().join("mixed-arena-world.json");
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"mixed-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":1000000,"allow_mixed_environments":true}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
        ),
    )
    .expect("write mixed Arena World");
    let Some(ResponseData::World { world }) = dispatch_call(
        &mut plane,
        &token,
        "mixed-arena-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("mixed Arena World registration should succeed");
    };

    let parent_path = directory.path().join("mixed-parent.md");
    fs::write(
        &parent_path,
        "---\nschema_version: 1\nname: mixed-parent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n```hephaestus-reference-v1\n{\"schema_version\":1,\"operation\":\"identity\"}\n```\n",
    )
    .expect("write parent Genome");
    let Some(ResponseData::Genome { genome: parent }) = dispatch_call(
        &mut plane,
        &token,
        "mixed-parent",
        Command::GenomeRegister {
            path: parent_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .data
    else {
        panic!("parent Genome registration should succeed");
    };
    let candidate =
        register_claude_provider_genome(&mut plane, &token, &directory, &world.world_id);

    assert!(
        dispatch_call(&mut plane, &token, "mixed-unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    complete_arena_test_job(
        &mut plane,
        "mixed-eval",
        &parent.genome_id,
        &candidate.genome_id,
    );

    let job = plane
        .state
        .arena_jobs
        .get("mixed-eval")
        .expect("mixed Arena job recorded");
    assert!(
        job.environment_id.starts_with("reference-v1."),
        "parent trial must keep the reference-worker environment identity"
    );
    assert_eq!(
        job.candidate_environment_id
            .as_deref()
            .map(|id| id.starts_with("provider-v1.")),
        Some(true),
        "mixed pair must record both a reference parent and a provider candidate environment"
    );
    assert!(job.evaluation.is_some());

    // Selection runs over the recorded mixed-environment evaluation exactly
    // like a homogeneous one.
    assert!(matches!(
        dispatch_call(
            &mut plane,
            &token,
            "mixed-select",
            Command::ArenaSelect {
                evaluation_id: "mixed-eval".to_owned(),
            },
        )
        .data,
        Some(ResponseData::Selection { .. })
    ));

    // Replay proves the mixed-environment job and its evaluation verify from
    // canonical history, in-process and from a fresh reopen.
    assert!(matches!(
        plane.replay_response().expect("replay mixed Arena history"),
        ResponseData::Replay { .. }
    ));
    drop(plane);
    let reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("reopen mixed Arena fixture from canonical history");
    assert_eq!(
        reopened.state.arena_jobs["mixed-eval"].terminal,
        Some(JobTerminal::Succeeded)
    );
}

#[test]
fn submit_admits_and_cancels_a_provider_job_through_daemon_stop() {
    let directory = tempdir().expect("fixture directory");
    let (repository, _source_revision) = committed_reference_fixture(directory.path());
    let data_dir = directory.path().join("data");
    let current_executable = env::current_exe().expect("test executable");
    let fake_claude = directory.path().join("cancel-fake-claude");
    write_fake_claude_binary(&fake_claude);
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &current_executable,
        &current_executable,
    )
    .expect("open control plane for provider job cancellation")
    .with_provider_executables_for_testing(
        "/nonexistent/codex-should-not-be-invoked",
        &fake_claude,
        Vec::new(),
    );
    let token = plane.token_hex.clone();
    let world_path = directory.path().join("cancel-provider-world.json");
    fs::write(
        &world_path,
        r#"{"schema_version":1,"name":"cancel-provider-world","laws":{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":1000000},"authority_ceiling":{"workspace_write":false,"network":false},"mutation_scope":[],"promotion":{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500},"objectives":["correctness"],"evaluator_artifacts":{}}"#,
    )
    .expect("write provider World source");
    let Some(ResponseData::World { world }) = dispatch_call(
        &mut plane,
        &token,
        "register-cancel-provider-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("provider World registration should succeed");
    };
    let genome = register_claude_provider_genome(&mut plane, &token, &directory, &world.world_id);
    assert!(
        dispatch_call(&mut plane, &token, "cancel-unfreeze", Command::Unfreeze)
            .error
            .is_none()
    );

    let submit_response = dispatch_call(
        &mut plane,
        &token,
        "provider-submit-for-cancel",
        Command::RunSubmit {
            job_id: "provider-cancel-job".to_owned(),
            genome_id: genome.genome_id.clone(),
        },
    );
    assert!(
        submit_response.error.is_none(),
        "submit must admit a provider Genome job: {:?}",
        submit_response.error
    );

    // Requesting daemon stop while the job is active cancels it instead of
    // shutting down immediately, exactly like the reference-worker path.
    assert!(matches!(
        plane.request_daemon_stop(),
        Err(ExecuteError::Busy)
    ));
    assert!(!plane.shutdown_requested);
    assert_eq!(
        plane.state.jobs["provider-cancel-job"].state,
        JobState::CancellationRequested
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist provider cancellation");
        assert!(
            Instant::now() < deadline,
            "provider job cancellation stalled"
        );
        thread::sleep(Duration::from_millis(2));
    }
    let terminal_state = plane.state.jobs["provider-cancel-job"].state;
    assert!(
        matches!(terminal_state, JobState::Interrupted | JobState::Succeeded),
        "cancellation must race safely to either Interrupted or a completed Succeeded job, got {terminal_state:?}"
    );
    assert!(plane.request_daemon_stop().is_ok());
    assert!(plane.shutdown_requested);

    // Replay proves the cancelled (or completed) provider job verifies from
    // canonical history.
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay provider job cancellation"),
        ResponseData::Replay { .. }
    ));
}

#[test]
fn projection_refresh_rejects_a_ledger_row_tampered_after_a_prior_successful_refresh() {
    // TD-16: refresh no longer caches verified evidence across calls, so this
    // also proves a full refresh is not merely fast because it skips already
    // verified events; the very next refresh re-verifies every event against
    // a freshly hash-chain-verified history and still catches a row tampered
    // in between.
    let directory = tempdir().expect("projection refresh fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let evaluation_id = "refresh-retamper-evaluation";
    plane
        .submit_arena_job(
            evaluation_id,
            &parent.genome_id,
            &candidate.genome_id,
            false,
        )
        .expect("admit genuine Arena evaluation");
    let deadline = Instant::now() + Duration::from_secs(30);
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist trials for the projection refresh fixture");
        assert!(
            Instant::now() < deadline,
            "genuine Arena evaluation did not finish"
        );
        thread::sleep(Duration::from_millis(2));
    }
    plane
        .select_arena_evaluation(evaluation_id)
        .expect("select completed Arena evidence");
    plane
        .refresh_projection()
        .expect("refresh verifies the genuine selection");

    let connection =
        rusqlite::Connection::open(plane.data_dir.join("events.sqlite3")).expect("open ledger");
    let changed = connection
        .execute(
            "UPDATE events SET payload = ?1 WHERE event_type = 'selection.recorded'",
            rusqlite::params![b"{}".as_slice()],
        )
        .expect("tamper with the previously verified selection event");
    assert_eq!(changed, 1);
    drop(connection);

    assert!(
        plane.refresh_projection().is_err(),
        "an event whose ledger bytes changed after a prior successful refresh must fail the next refresh"
    );
}

/// Stores bytes through whichever `ArtifactBackend` the given plane is
/// currently open on (SQLite/CAS, or the JSONL/memory pair from
/// [`ControlPlane::open_with_backends`]), instead of assuming the
/// filesystem CAS the way `register_dispatch_arena_objects_with_invariants`
/// does. Needed so the same registration recipe can run unmodified against
/// either backend pair (TD-13).
fn put_plane_artifact(plane: &mut ControlPlane, bytes: &[u8]) -> ArtifactId {
    plane
        .storage
        .as_mut()
        .expect("canonical storage")
        .artifacts
        .put(bytes)
        .expect("store artifact through the plane's own backend")
}

/// Backend-agnostic counterpart to `register_dispatch_arena_objects_with_invariants`:
/// registers one World and a parent/candidate reference Genome pair, storing
/// every supporting artifact through `plane.storage.artifacts` (whatever
/// backend that is) rather than opening a second, backend-specific handle.
fn register_backend_arena_objects(
    plane: &mut ControlPlane,
    token: &str,
    directory: &Path,
) -> (WorldRecord, GenomeRecord, GenomeRecord) {
    let visible = TrustedManifest::new(
        "backend-visible",
        Visibility::Visible,
        vec![
            hephaestus_arena::TrustedTask::new("visible-task", "visible", "VISIBLE")
                .expect("visible task"),
        ],
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        "backend-sealed",
        Visibility::Sealed,
        vec![
            hephaestus_arena::TrustedTask::new("sealed-task", "sealed", "SEALED")
                .expect("sealed task"),
        ],
    )
    .expect("sealed manifest");
    let visible_id = put_plane_artifact(
        plane,
        &serde_json::to_vec(&visible).expect("encode visible manifest"),
    );
    let sealed_id = put_plane_artifact(
        plane,
        &serde_json::to_vec(&sealed).expect("encode sealed manifest"),
    );
    let evaluator = env::current_exe()
        .expect("locate test executable")
        .parent()
        .and_then(Path::parent)
        .expect("locate Cargo binary directory")
        .join(format!(
            "hephaestus-reference-evaluator{}",
            std::env::consts::EXE_SUFFIX
        ));
    let evaluator_id = put_plane_artifact(
        plane,
        &fs::read(evaluator).expect("read reference evaluator"),
    );
    let verifier_bytes = plane.run_result_verifier.public_key_bytes();
    let verifier_id = put_plane_artifact(plane, &verifier_bytes);
    let world_path = directory.join("backend-arena-world.json");
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"backend-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":["harness"],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
        ),
    )
    .expect("write Arena World");
    let Some(ResponseData::World { world }) = dispatch_call(
        plane,
        token,
        "backend-arena-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("Arena World registration should succeed");
    };
    let register_genome = |plane: &mut ControlPlane, token: &str, name: &str, parents: &str| {
        let path = directory.join(format!("{name}.md"));
        fs::write(
            &path,
            format!(
                "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
            ),
        )
        .expect("write Genome source");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            token,
            name,
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("Arena Genome registration should succeed");
        };
        genome
    };
    let parent = register_genome(plane, token, "backend-arena-parent", "[]");
    let candidate = register_genome(
        plane,
        token,
        "backend-arena-candidate",
        &format!("[\"{}\"]", parent.genome_id),
    );
    (world, parent, candidate)
}

/// Opens a fresh `ControlPlane` over the JSONL event ledger and in-memory
/// artifact backend (TD-13) instead of the default SQLite/CAS pair, using
/// the same real reference-worker/evaluator binaries every other Arena
/// fixture in this file uses.
fn open_jsonl_memory_arena_fixture(
    data_dir: &Path,
    repository: &Path,
    evaluator: &Path,
    worker: &Path,
) -> ControlPlane {
    let ledger_path = data_dir.join("events.jsonl");
    let open_ledger = move || -> Result<Box<dyn EventLedger + Send>, ControlError> {
        Ok(Box::new(FileEventLedger::open(&ledger_path)?))
    };
    let backend = Arc::new(MemoryArtifactBackend::new());
    let open_artifacts = move || -> Result<Box<dyn ArtifactBackend + Send + Sync>, ControlError> {
        Ok(Box::new(Arc::clone(&backend)))
    };
    ControlPlane::open_with_backends(
        data_dir,
        repository,
        evaluator,
        worker,
        open_ledger,
        open_artifacts,
    )
    .expect("open JSONL/memory Arena fixture")
}

/// Every field of a [`hephaestus_arena::SelectionReceipt`] that is a pure
/// function of the trial inputs (the pinned reference-worker transform, the
/// bootstrap algorithm, and the World's promotion policy), used to compare
/// two receipts computed by two different storage backends for otherwise
/// identical inputs.
///
/// Deliberately excludes `world_id`/`parent_genome_id`/`candidate_genome_id`
/// (each backend's `ControlPlane` mints its own runtime-producer keypair on
/// first open, which is folded into the World's `arena.runtime_verifier`
/// artifact and therefore into the content-addressed World/Genome
/// identities — expected to differ across independently opened daemons, not
/// a storage-backend effect), `evaluation_event_hash` (hash-chained, so it
/// depends on wall-clock timestamps of every prior event), the two measured
/// `*_latency_millis` fields (wall-clock, not decision inputs), and
/// `candidate_pareto_dominates` (folds those same measured latencies into a
/// cost/latency dominance check, so it can legitimately flip between two
/// runs of the identical deterministic reference genomes whose measured
/// wall-clock latencies differ, e.g. local execution versus a remote-leased
/// trial's extra round trip).
#[derive(Debug, PartialEq)]
struct SelectionDecision {
    schema_version: u16,
    algorithm: String,
    resamples: u32,
    seed: u64,
    maximum_cost_microusd: u64,
    minimum_delta_bps: i64,
    maximum_regressions: u32,
    confidence_bps: u16,
    correctness_regressions: u32,
    correctness_unchanged: u32,
    correctness_improvements: u32,
    estimate_bps: i64,
    lower_bps: i64,
    upper_bps: i64,
    parent_correctness_bps: u32,
    candidate_correctness_bps: u32,
    parent_reliability_bps: u32,
    candidate_reliability_bps: u32,
    parent_cost_microusd: u64,
    candidate_cost_microusd: u64,
    metrics_eligible: bool,
    invariant_gate_verified: bool,
    promotion_eligible: bool,
}

impl From<&hephaestus_arena::SelectionReceipt> for SelectionDecision {
    fn from(receipt: &hephaestus_arena::SelectionReceipt) -> Self {
        Self {
            schema_version: receipt.schema_version(),
            algorithm: receipt.algorithm().to_owned(),
            resamples: receipt.resamples(),
            seed: receipt.seed(),
            maximum_cost_microusd: receipt.maximum_cost_microusd(),
            minimum_delta_bps: receipt.minimum_delta_bps(),
            maximum_regressions: receipt.maximum_regressions(),
            confidence_bps: receipt.confidence_bps(),
            correctness_regressions: receipt.correctness_regressions(),
            correctness_unchanged: receipt.correctness_unchanged(),
            correctness_improvements: receipt.correctness_improvements(),
            estimate_bps: receipt.estimate_bps(),
            lower_bps: receipt.lower_bps(),
            upper_bps: receipt.upper_bps(),
            parent_correctness_bps: receipt.parent_correctness_bps(),
            candidate_correctness_bps: receipt.candidate_correctness_bps(),
            parent_reliability_bps: receipt.parent_reliability_bps(),
            candidate_reliability_bps: receipt.candidate_reliability_bps(),
            parent_cost_microusd: receipt.parent_cost_microusd(),
            candidate_cost_microusd: receipt.candidate_cost_microusd(),
            metrics_eligible: receipt.metrics_eligible(),
            invariant_gate_verified: receipt.invariant_gate_verified(),
            promotion_eligible: receipt.promotion_eligible(),
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn open_with_backends_on_jsonl_ledger_and_memory_artifacts_matches_sqlite_cas_decision() {
    // TD-13: proves the JSONL ledger + in-memory artifact backend pair
    // lands a real World/Genome registration, a paired Arena evaluation,
    // `arena select`, and `replay` end to end through `ControlPlane`, and
    // that the resulting selection decision is identical to the default
    // SQLite/CAS backend for the same reference-genome inputs.
    let directory = tempdir().expect("storage backend parity fixture");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(
        repository.join("fixture.txt"),
        b"Storage backend parity fixture\n",
    )
    .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);

    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    let evaluator = directory.path().join("parity-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    assert!(worker.is_file(), "missing worker {worker:?}");

    // SQLite/CAS backend (the default daemon path).
    let sqlite_data_dir = directory.path().join("sqlite-data");
    let mut sqlite_plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &sqlite_data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open SQLite/CAS Arena fixture");
    let sqlite_token = sqlite_plane.token_hex.clone();
    let (_, sqlite_parent, sqlite_candidate) =
        register_backend_arena_objects(&mut sqlite_plane, &sqlite_token, directory.path());
    assert!(
        dispatch_call(
            &mut sqlite_plane,
            &sqlite_token,
            "sqlite-unfreeze",
            Command::Unfreeze
        )
        .error
        .is_none()
    );
    complete_arena_test_job(
        &mut sqlite_plane,
        "parity-eval",
        &sqlite_parent.genome_id,
        &sqlite_candidate.genome_id,
    );
    let ResponseData::Selection {
        selection: sqlite_selection,
    } = sqlite_plane
        .select_arena_evaluation("parity-eval")
        .expect("select on SQLite/CAS backend")
    else {
        panic!("SQLite/CAS selection should produce a receipt");
    };
    assert!(matches!(
        sqlite_plane
            .replay_response()
            .expect("replay SQLite/CAS history"),
        ResponseData::Replay { .. }
    ));

    // JSONL ledger + in-memory artifact backend pair (TD-13's second
    // backend), driven through the identical registration/run/select/replay
    // recipe.
    let jsonl_data_dir = directory.path().join("jsonl-data");
    let mut jsonl_plane =
        open_jsonl_memory_arena_fixture(&jsonl_data_dir, &repository, &evaluator, &worker);
    let jsonl_token = jsonl_plane.token_hex.clone();
    let (_, jsonl_parent, jsonl_candidate) =
        register_backend_arena_objects(&mut jsonl_plane, &jsonl_token, directory.path());
    assert!(
        dispatch_call(
            &mut jsonl_plane,
            &jsonl_token,
            "jsonl-unfreeze",
            Command::Unfreeze
        )
        .error
        .is_none()
    );
    complete_arena_test_job(
        &mut jsonl_plane,
        "parity-eval",
        &jsonl_parent.genome_id,
        &jsonl_candidate.genome_id,
    );
    let ResponseData::Selection {
        selection: jsonl_selection,
    } = jsonl_plane
        .select_arena_evaluation("parity-eval")
        .expect("select on JSONL/memory backend")
    else {
        panic!("JSONL/memory selection should produce a receipt");
    };
    assert!(matches!(
        jsonl_plane
            .replay_response()
            .expect("replay JSONL/memory history"),
        ResponseData::Replay { .. }
    ));

    assert_eq!(
        SelectionDecision::from(&sqlite_selection.receipt),
        SelectionDecision::from(&jsonl_selection.receipt),
        "the same reference-genome inputs must reach the same Arena decision \
         regardless of which EventLedger/ArtifactBackend pair the control plane \
         is routed through"
    );

    // Independently proves both hash chains verify byte for byte from a
    // fresh reader, not merely from the already-open handle above.
    assert!(
        sqlite_plane
            .storage
            .as_ref()
            .expect("SQLite/CAS storage")
            .ledger
            .replay_verified()
            .is_ok()
    );
    assert!(
        jsonl_plane
            .storage
            .as_ref()
            .expect("JSONL storage")
            .ledger
            .replay_verified()
            .is_ok()
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn remote_leased_arena_trial_matches_local_execution_and_is_idempotent_under_duplicate_delivery() {
    let _reference_delay_slot = hold_reference_delay_slot();
    // TD-12's last item: a paired Arena evaluation admitted with the remote
    // opt-in leases every reference-role trial to a remote worker exactly
    // like the direct-run `worker.sock` path, and the daemon (never the
    // worker) remains the sole signer of the resulting canonical result.
    let directory = tempdir().expect("remote Arena lease fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let (_, worker_token) =
        mint_worker_credential(&mut plane, &token, "arena-remote-worker", 3_600);

    // Baseline: the same Genome pair evaluated locally.
    complete_arena_test_job(
        &mut plane,
        "local-eval",
        &parent.genome_id,
        &candidate.genome_id,
    );
    let ResponseData::Selection {
        selection: local_selection,
    } = plane
        .select_arena_evaluation("local-eval")
        .expect("select local Arena evidence")
    else {
        panic!("local selection should produce a receipt");
    };

    // The same pair, admitted with the remote opt-in: every reference-role
    // trial must be leased and completed through `handle_worker_request`
    // instead of a local sandbox.
    assert!(matches!(
        plane
            .submit_arena_job("remote-eval", &parent.genome_id, &candidate.genome_id, true)
            .expect("admit remote-leased Arena evaluation"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_leased: Option<(String, String)> = None;
    let mut leased_trials = 0_u32;
    while plane.active_arena_job.is_some() {
        match plane.handle_worker_request(WorkerRequest::Lease {
            worker_id: "arena-remote-worker".to_owned(),
            token: worker_token.clone(),
        }) {
            WorkerReply::Leased {
                job_id, frame_hex, ..
            } => {
                let frame = hex_decode_bytes(&frame_hex).expect("leased frame is valid hex");
                let output = hephaestus_runtime::execute_reference_worker_request(&frame)
                    .expect("remote reference transform succeeds");
                let output_hex = hex_encode_bytes(&output);
                let reply = plane.handle_worker_request(WorkerRequest::SubmitResult {
                    worker_id: "arena-remote-worker".to_owned(),
                    token: worker_token.clone(),
                    job_id: job_id.clone(),
                    output_hex: output_hex.clone(),
                    completion: RemoteCompletion::Success,
                });
                assert!(
                    matches!(reply, WorkerReply::ResultAccepted { .. }),
                    "a genuine leased trial result must be accepted"
                );
                leased_trials += 1;
                last_leased = Some((job_id, output_hex));
            }
            WorkerReply::NoWork => {}
            WorkerReply::Error { reason } => panic!("lease unexpectedly failed: {reason}"),
            WorkerReply::ResultAccepted { .. } => {
                panic!("Lease request must not reply ResultAccepted")
            }
        }
        plane
            .service_async_messages()
            .expect("persist remote-leased Arena trials");
        assert!(
            Instant::now() < deadline,
            "remote-leased Arena evaluation did not finish"
        );
        if plane.active_arena_job.is_some() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(
        plane.state.arena_jobs["remote-eval"].terminal,
        Some(JobTerminal::Succeeded)
    );
    assert!(
        leased_trials > 0,
        "the remote-leased evaluation must have leased at least one trial"
    );

    let ResponseData::Selection {
        selection: remote_selection,
    } = plane
        .select_arena_evaluation("remote-eval")
        .expect("select remote-leased Arena evidence")
    else {
        panic!("remote-leased selection should produce a receipt");
    };
    assert_eq!(
        SelectionDecision::from(&local_selection.receipt),
        SelectionDecision::from(&remote_selection.receipt),
        "a remote-leased evaluation must reach the same decision as the identical \
         evaluation executed locally: its recorded run.result_recorded events must \
         be indistinguishable from local execution"
    );

    // Duplicate delivery of the last trial's result, after the whole
    // evaluation already completed, must stay idempotent: no new canonical
    // event is appended, and the reply still reports acceptance.
    let (duplicate_job_id, duplicate_output_hex) =
        last_leased.expect("at least one trial was leased");
    let history_len_before = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history before duplicate delivery")
        .len();
    let repeat = plane.handle_worker_request(WorkerRequest::SubmitResult {
        worker_id: "arena-remote-worker".to_owned(),
        token: worker_token,
        job_id: duplicate_job_id,
        output_hex: duplicate_output_hex,
        completion: RemoteCompletion::Success,
    });
    assert!(
        matches!(repeat, WorkerReply::ResultAccepted { .. }),
        "duplicate delivery of an already-completed Arena trial must still be accepted"
    );
    let history_len_after = plane
        .storage
        .as_ref()
        .expect("canonical storage")
        .ledger
        .replay_verified()
        .expect("verify history after duplicate delivery")
        .len();
    assert_eq!(
        history_len_before, history_len_after,
        "duplicate delivery of a completed Arena trial result must not append a new event"
    );
}

#[test]
fn remote_arena_trial_credential_expiry_fails_closed_mid_evaluation_and_leaves_local_path_unaffected()
 {
    // TD-12: an expired worker credential must fail closed before any lease
    // is handed out, leaving the Arena job pending rather than silently
    // executing the trial some other way; operator cancellation still
    // terminalizes it, and the daemon's local (non-remote) execution path is
    // unaffected by the failed remote attempt.
    let directory = tempdir().expect("remote Arena credential expiry fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let (_, worker_token) = mint_worker_credential(&mut plane, &token, "expiring-arena-worker", 1);

    assert!(matches!(
        plane
            .submit_arena_job(
                "remote-expiry-eval",
                &parent.genome_id,
                &candidate.genome_id,
                true
            )
            .expect("admit remote-leased Arena evaluation"),
        ResponseData::ArenaJob { job } if job.state == JobState::Running
    ));
    std::thread::sleep(Duration::from_millis(1_100));

    let reply = plane.handle_worker_request(WorkerRequest::Lease {
        worker_id: "expiring-arena-worker".to_owned(),
        token: worker_token.clone(),
    });
    assert!(
        matches!(reply, WorkerReply::Error { .. }),
        "an expired credential must fail closed before a trial is leased"
    );
    assert!(
        plane.active_arena_job.is_some(),
        "the Arena job must stay pending, not silently fail or complete, \
         when its only remote worker's credential has expired"
    );
    assert!(
        plane.state.arena_jobs["remote-expiry-eval"]
            .terminal
            .is_none()
    );

    // The same expired credential must also fail closed on `SubmitResult`,
    // not only on `Lease`: a result must never be recorded for any job
    // (direct-run or a leased Arena trial) without a fresh, valid
    // credential re-check.
    let submit_reply = plane.handle_worker_request(WorkerRequest::SubmitResult {
        worker_id: "expiring-arena-worker".to_owned(),
        token: worker_token,
        job_id: "arena:remote-expiry-eval:trial:0".to_owned(),
        output_hex: hex_encode_bytes(b"forged output"),
        completion: RemoteCompletion::Success,
    });
    assert!(
        matches!(submit_reply, WorkerReply::Error { .. }),
        "an expired credential must fail closed before a result is recorded"
    );

    // Operator cancellation still terminalizes a job stuck on an
    // unavailable remote worker.
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "cancel-remote-expiry",
            Command::JobKill {
                job_id: "remote-expiry-eval".to_owned(),
            },
        )
        .error
        .is_none()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while plane.active_arena_job.is_some() {
        plane
            .service_async_messages()
            .expect("drain cancelled remote-leased Arena job");
        assert!(
            Instant::now() < deadline,
            "cancellation of the remote-leased Arena job did not terminalize it"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        plane.state.arena_jobs["remote-expiry-eval"].terminal,
        Some(JobTerminal::Cancelled)
    );

    // The local (non-remote) execution path is unaffected by the failed
    // remote attempt above.
    complete_arena_test_job(
        &mut plane,
        "local-after-expiry",
        &parent.genome_id,
        &candidate.genome_id,
    );
}

// ---------------------------------------------------------------------------
// Automatic drift-to-canary adaptation (roadmap item 12).
// ---------------------------------------------------------------------------

/// Per-trial delay injected to make a Genome genuinely slower in the
/// 16-task auto-canary fixture. The single-task canary tests use 750 ms; here
/// that would add 12 s to every paired evaluation. 250 ms still adds about
/// 4 s per evaluation, far past the 20% latency regression threshold.
const AUTO_CANARY_REGRESSION_DELAY_MILLIS: u64 = 250;

/// Like `real_worker_arena_fixture_with_invariants`, but the registered World
/// opts in to `laws.auto_canary_on_drift`, and both registered Genomes start
/// on the `identity` reference operation against these uppercase-expecting
/// tasks: exactly the misconfiguration `register_meta_lineage` uses for the
/// Evolver's own recursive tests, so a Champion seeded on either one is
/// genuinely fixed (not fabricated) by the Forge catalog's one supported
/// mutation, the flip to `ascii_uppercase`.
#[allow(clippy::too_many_lines)]
fn auto_canary_arena_fixture(directory: &TempDir) -> (ControlPlane, GenomeRecord, GenomeRecord) {
    let data_dir = directory.path().join("data");
    let repository = directory.path().join("repository");
    fs::create_dir_all(&repository).expect("create source repository");
    fixture_git(&repository, &["init", "-q"]);
    fixture_git(&repository, &["config", "user.name", "Hephaestus Test"]);
    fixture_git(
        &repository,
        &["config", "user.email", "hephaestus@example.invalid"],
    );
    fs::write(repository.join("fixture.txt"), b"Arena evidence fixture\n")
        .expect("write source fixture");
    fixture_git(&repository, &["add", "."]);
    fixture_git(&repository, &["commit", "-m", "fixture", "-q"]);

    let bin_directory = env::current_exe()
        .expect("test executable")
        .parent()
        .and_then(Path::parent)
        .expect("Cargo binary directory")
        .to_owned();
    let cargo_evaluator = bin_directory.join(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    let evaluator = directory.path().join("fixture-evaluator");
    fs::copy(&cargo_evaluator, &evaluator).expect("copy evaluator into private inode");
    fs::set_permissions(&evaluator, fs::Permissions::from_mode(0o700))
        .expect("make evaluator executable");
    let worker = bin_directory.join(format!(
        "hephaestus-reference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let mut plane = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .expect("open auto-canary fixture");
    let token = plane.token_hex.clone();

    let artifacts =
        ArtifactStore::open(plane.data_dir.join("blobs")).expect("open canonical artifacts");
    // Many small tasks, not one: a single-task pairing's wall-clock latency
    // is dominated by per-trial process-spawn scheduling noise (the shared,
    // multi-agent machine this suite runs on), which can swing well past
    // the fixed 20% regression threshold in either direction on one trial.
    // Aggregating over enough trials averages that noise out so a genuinely
    // unregressed pairing reads as healthy deterministically enough for an
    // unattended, non-retrying reconciliation loop to act on, exactly as it
    // would in production.
    let auto_canary_tasks = |visibility_word: &str| -> Vec<hephaestus_arena::TrustedTask> {
        (0..8)
            .map(|index| {
                hephaestus_arena::TrustedTask::new(
                    format!("{visibility_word}-task-{index}"),
                    visibility_word,
                    visibility_word.to_uppercase(),
                )
                .expect("auto-canary task")
            })
            .collect()
    };
    let visible = TrustedManifest::new(
        "auto-canary-visible",
        Visibility::Visible,
        auto_canary_tasks("visible"),
    )
    .expect("visible manifest");
    let sealed = TrustedManifest::new(
        "auto-canary-sealed",
        Visibility::Sealed,
        auto_canary_tasks("sealed"),
    )
    .expect("sealed manifest");
    let visible_id = artifacts
        .put(&serde_json::to_vec(&visible).expect("encode visible manifest"))
        .expect("store visible manifest");
    let sealed_id = artifacts
        .put(&serde_json::to_vec(&sealed).expect("encode sealed manifest"))
        .expect("store sealed manifest");
    let evaluator_id = artifacts
        .put(&fs::read(&evaluator).expect("read reference evaluator"))
        .expect("store evaluator identity");
    let verifier_id = artifacts
        .put(&plane.run_result_verifier.public_key_bytes())
        .expect("store result verifier");
    let invariant_id = artifacts
        .put(CLEAN_INVARIANTS)
        .expect("store invariant manifest");
    drop(artifacts);

    let world_path = directory.path().join("auto-canary-world.json");
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"auto-canary","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0,"auto_canary_on_drift":true}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":["harness"],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}","arena.invariant_manifest":"{}"}}}}"#,
            visible_id.as_str(),
            sealed_id.as_str(),
            evaluator_id.as_str(),
            verifier_id.as_str(),
            invariant_id.as_str(),
        ),
    )
    .expect("write auto-canary World");
    let Some(ResponseData::World { world }) = dispatch_call(
        &mut plane,
        &token,
        "auto-canary-world",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    )
    .data
    else {
        panic!("auto-canary World registration should succeed");
    };
    assert!(
        plane
            .state
            .registered
            .world(&world.world_id)
            .expect("registered auto-canary World")
            .compiled()
            .evaluation_policy()
            .auto_canary_on_drift(),
        "the registered World must carry the opted-in Law"
    );
    let register_genome = |plane: &mut ControlPlane, token: &str, name: &str, parents: &str| {
        let path = directory.path().join(format!("{name}.md"));
        fs::write(
            &path,
            format!(
                "---\nschema_version: 1\nname: {name}\nparents: {parents}\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {{}}\n---\n```hephaestus-reference-v1\n{{\"schema_version\":1,\"operation\":\"identity\"}}\n```\n"
            ),
        )
        .expect("write Genome source");
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            plane,
            token,
            name,
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("auto-canary Genome registration should succeed");
        };
        genome
    };
    let parent = register_genome(&mut plane, &token, "auto-canary-parent", "[]");
    let candidate = register_genome(
        &mut plane,
        &token,
        "auto-canary-candidate",
        &format!("[\"{}\"]", parent.genome_id),
    );
    assert!(
        dispatch_call(
            &mut plane,
            &token,
            "auto-canary-unfreeze",
            Command::Unfreeze
        )
        .error
        .is_none()
    );
    (plane, parent, candidate)
}

/// Polls the reconciliation loop, the same way `evolve_drain_active_run`
/// polls an evolution run, until the named drift's automatic adaptation
/// reaches a terminal (`drift.adaptation_finished`) state.
fn drain_drift_adaptation(plane: &mut ControlPlane, drift_id: &str) -> DriftRecord {
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        plane
            .service_async_messages()
            .expect("advance the drift adaptation reconciliation loop");
        let history = plane
            .storage
            .as_ref()
            .expect("open canonical storage")
            .ledger
            .replay_verified()
            .expect("replay verified history");
        let record = super::drift::drift_projection(&history, drift_id)
            .expect("drift adaptation projection");
        if let Some(record) = &record
            && record.adaptation.finished
        {
            return record.clone();
        }
        assert!(
            Instant::now() < deadline,
            "drift adaptation did not finish: {:?}; active Arena job: {:?}",
            record.map(|record| record.adaptation),
            plane
                .active_arena_job
                .as_ref()
                .map(|job| job.record.evaluation_id.clone())
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn drift_adaptation_show(plane: &ControlPlane, drift_id: &str) -> DriftRecord {
    let history = plane
        .storage
        .as_ref()
        .expect("open canonical storage")
        .ledger
        .replay_verified()
        .expect("replay verified history");
    super::drift::drift_projection(&history, drift_id)
        .expect("drift adaptation projection")
        .expect("drift record exists")
}

fn adaptation_history_with_payload_edit(
    history: &[StoredEvent],
    event_id: &str,
    edit: impl FnOnce(&mut crate::DriftAdaptationFinishedPayload),
) -> Vec<StoredEvent> {
    let mut tampered = history.to_vec();
    let event = tampered
        .iter_mut()
        .find(|event| event.event_id == event_id)
        .expect("drift adaptation finished event exists");
    let mut payload: crate::DriftAdaptationFinishedPayload =
        serde_json::from_slice(&event.payload).expect("decode drift adaptation finished payload");
    edit(&mut payload);
    let canonical = serde_json::to_value(&payload).expect("canonicalize adaptation payload");
    event.payload = serde_json::to_vec(&canonical).expect("encode adaptation payload");
    tampered
}

#[test]
#[allow(clippy::too_many_lines)]
fn auto_canary_on_drift_promotes_a_genuine_champion_correction_and_replays() {
    let _reference_delay_slot = hold_reference_delay_slot();
    let directory = tempdir().expect("auto-canary fixture");
    let (mut plane, parent, candidate) = auto_canary_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();

    // Seed the Champion misconfigured: `identity` against these
    // uppercase-expecting tasks, exactly the World the Forge catalog's one
    // supported mutation (flip to `ascii_uppercase`) genuinely fixes.
    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: candidate.genome_id.clone(),
            reason: "Bootstrap the misconfigured reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");

    // A sibling derived from `parent` (not from the Champion, so it cannot
    // collide with the child the automatic adaptation itself will later
    // derive from the Champion): genuinely improved to `ascii_uppercase`,
    // then genuinely, deterministically slowed (see
    // `canary_live_check_detects_a_genuine_latency_regression_and_rolls_back_the_champion`)
    // so a fresh paired evaluation against the Champion measures a real
    // latency drift without depending on the tasks' correctness at all
    // (the misconfigured Champion is already at the correctness floor, so
    // no sibling can measure as further regressed on that axis).
    let sibling = assessed_forge_child(
        &mut plane,
        "trigger-improve",
        &candidate.genome_id,
        &parent.genome_id,
        true,
    );
    hephaestus_runtime::set_test_reference_delay(
        sibling.child.clone(),
        AUTO_CANARY_REGRESSION_DELAY_MILLIS,
    );
    let drift_evidence_id = "trigger-evidence";
    complete_arena_test_job(
        &mut plane,
        drift_evidence_id,
        &candidate.genome_id,
        &sibling.child,
    );
    hephaestus_runtime::clear_test_reference_delay();
    let ResponseData::Selection { selection } = plane
        .select_arena_evaluation(drift_evidence_id)
        .expect("select genuine latency drift evidence")
    else {
        panic!("selection should return its receipt");
    };
    assert!(
        selection.receipt.candidate_latency_millis() > selection.receipt.parent_latency_millis(),
        "the injected delay should make the sibling measurably slower"
    );

    let recorded = drift_record_cmd(
        &mut plane,
        &token,
        "trigger-drift",
        Command::DriftRecord {
            drift_id: "trigger".to_owned(),
            world_id: world_id.clone(),
            kind: DriftKind::Latency,
            evidence_evaluation_id: drift_evidence_id.to_owned(),
        },
    )
    .expect("record a genuine latency drift against the misconfigured Champion");
    assert!(!recorded.adaptation.started);

    let finished = drain_drift_adaptation(&mut plane, "trigger");
    assert!(finished.adaptation.finished);
    assert_eq!(
        finished.adaptation.finish_reason,
        Some(DriftAdaptationFinishReason::Promoted)
    );
    assert_eq!(
        finished.adaptation.canary_stage,
        Some(CanaryStage::Completed)
    );
    let promoted_child = finished
        .adaptation
        .child_genome_id
        .clone()
        .expect("a promoted adaptation records its child Genome");
    assert_ne!(
        promoted_child, sibling.child,
        "the adaptation must derive its own child from the Champion, not reuse the sibling"
    );

    let champion = champion_show(&mut plane, &token, &world_id);
    assert_eq!(
        champion.champion_genome_id.as_deref(),
        Some(promoted_child.as_str()),
        "the automatic adaptation should have promoted its own child to Champion"
    );
    assert_ne!(
        champion.champion_genome_id.as_deref(),
        Some(candidate.genome_id.as_str()),
        "the promoted child must differ from the misconfigured Champion it corrected"
    );

    assert!(matches!(
        plane
            .replay_response()
            .expect("replay after automatic promotion"),
        ResponseData::Replay { .. }
    ));
}

#[test]
fn auto_canary_on_drift_never_fires_when_the_law_is_off() {
    let directory = tempdir().expect("drift fixture without the Law");
    let (mut plane, initial_parent, initial_candidate) =
        real_worker_arena_fixture_with_invariants(&directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();

    // The exact recipe `drift_record_derives_from_verified_evidence_and_replays`
    // uses for a genuine correctness drift, under a World that never opted
    // in to `laws.auto_canary_on_drift`.
    let improved = assessed_forge_child(
        &mut plane,
        "no-law-improve",
        &initial_parent.genome_id,
        &initial_candidate.genome_id,
        true,
    );
    let world_id = improved.world.clone();
    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: improved.child.clone(),
            reason: "Bootstrap the reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");
    let regressed = assessed_forge_child(
        &mut plane,
        "no-law-regress",
        &initial_parent.genome_id,
        &improved.child,
        false,
    );
    let recorded = drift_record_cmd(
        &mut plane,
        &token,
        "no-law-drift",
        Command::DriftRecord {
            drift_id: "no-law-drift".to_owned(),
            world_id: world_id.clone(),
            kind: DriftKind::Correctness,
            evidence_evaluation_id: regressed.evaluation.clone(),
        },
    )
    .expect("record a genuine correctness drift");
    assert!(!recorded.adaptation.started);

    for tick in 0..20 {
        plane
            .service_async_messages()
            .expect("service ticks with no opted-in World must stay quiet");
        let shown = drift_adaptation_show(&plane, "no-law-drift");
        assert!(
            !shown.adaptation.started,
            "tick {tick}: an adaptation must never start without the Law"
        );
    }
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay with an unadopted drift"),
        ResponseData::Replay { .. }
    ));
}

#[test]
#[allow(clippy::too_many_lines)]
fn auto_canary_on_drift_aborts_the_canary_on_a_genuine_regression_leaving_the_champion_untouched() {
    let _reference_delay_slot = hold_reference_delay_slot();
    let directory = tempdir().expect("auto-canary abort fixture");
    let (mut plane, parent, candidate) = auto_canary_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();

    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: candidate.genome_id.clone(),
            reason: "Bootstrap the misconfigured reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");

    let sibling = assessed_forge_child(
        &mut plane,
        "abort-improve",
        &candidate.genome_id,
        &parent.genome_id,
        true,
    );
    hephaestus_runtime::set_test_reference_delay(
        sibling.child.clone(),
        AUTO_CANARY_REGRESSION_DELAY_MILLIS,
    );
    let drift_evidence_id = "abort-evidence";
    complete_arena_test_job(
        &mut plane,
        drift_evidence_id,
        &candidate.genome_id,
        &sibling.child,
    );
    hephaestus_runtime::clear_test_reference_delay();
    plane
        .select_arena_evaluation(drift_evidence_id)
        .expect("select the latency drift evidence");
    drift_record_cmd(
        &mut plane,
        &token,
        "abort-drift",
        Command::DriftRecord {
            drift_id: "abort".to_owned(),
            world_id: world_id.clone(),
            kind: DriftKind::Latency,
            evidence_evaluation_id: drift_evidence_id.to_owned(),
        },
    )
    .expect("record a genuine latency drift against the misconfigured Champion");

    // Drain until the automatic adaptation has proposed and started its own
    // canary (the child Genome now exists), then genuinely, deterministically
    // slow that exact child so every staged evaluation the pipeline submits
    // against it measures a real regression: the very first stage aborts.
    let deadline = Instant::now() + Duration::from_secs(60);
    let child = loop {
        plane
            .service_async_messages()
            .expect("advance the drift adaptation reconciliation loop");
        let shown = drift_adaptation_show(&plane, "abort");
        if let Some(child) = shown.adaptation.child_genome_id.clone() {
            break child;
        }
        assert!(
            Instant::now() < deadline,
            "the automatic adaptation never proposed a child"
        );
        thread::sleep(Duration::from_millis(2));
    };
    hephaestus_runtime::set_test_reference_delay(
        child.clone(),
        AUTO_CANARY_REGRESSION_DELAY_MILLIS,
    );

    let finished = drain_drift_adaptation(&mut plane, "abort");
    hephaestus_runtime::clear_test_reference_delay();
    assert_eq!(
        finished.adaptation.finish_reason,
        Some(DriftAdaptationFinishReason::CanaryAborted)
    );
    assert_eq!(finished.adaptation.canary_stage, Some(CanaryStage::Aborted));

    let champion = champion_show(&mut plane, &token, &world_id);
    assert_eq!(
        champion.champion_genome_id.as_deref(),
        Some(candidate.genome_id.as_str()),
        "an aborted adaptation must never touch the Champion"
    );
    assert!(matches!(
        plane
            .replay_response()
            .expect("replay after an aborted adaptation"),
        ResponseData::Replay { .. }
    ));
}

#[test]
fn auto_canary_on_drift_pauses_under_freeze_and_resumes_after_unfreeze() {
    let _reference_delay_slot = hold_reference_delay_slot();
    let directory = tempdir().expect("auto-canary freeze fixture");
    let (mut plane, parent, candidate) = auto_canary_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();

    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: candidate.genome_id.clone(),
            reason: "Bootstrap the misconfigured reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");
    let sibling = assessed_forge_child(
        &mut plane,
        "freeze-improve",
        &candidate.genome_id,
        &parent.genome_id,
        true,
    );
    hephaestus_runtime::set_test_reference_delay(
        sibling.child.clone(),
        AUTO_CANARY_REGRESSION_DELAY_MILLIS,
    );
    let drift_evidence_id = "freeze-evidence";
    complete_arena_test_job(
        &mut plane,
        drift_evidence_id,
        &candidate.genome_id,
        &sibling.child,
    );
    hephaestus_runtime::clear_test_reference_delay();
    plane
        .select_arena_evaluation(drift_evidence_id)
        .expect("select the latency drift evidence");
    drift_record_cmd(
        &mut plane,
        &token,
        "freeze-drift",
        Command::DriftRecord {
            drift_id: "freeze".to_owned(),
            world_id: world_id.clone(),
            kind: DriftKind::Latency,
            evidence_evaluation_id: drift_evidence_id.to_owned(),
        },
    )
    .expect("record a genuine latency drift against the misconfigured Champion");

    assert!(
        dispatch_call(&mut plane, &token, "freeze-adaptation", Command::Freeze)
            .error
            .is_none()
    );
    for tick in 0..10 {
        plane
            .service_async_messages()
            .expect("service ticks while frozen must stay quiet");
        let shown = drift_adaptation_show(&plane, "freeze");
        assert!(
            !shown.adaptation.started,
            "tick {tick}: freeze must halt advancement before it ever starts"
        );
    }
    assert!(
        dispatch_call(&mut plane, &token, "unfreeze-adaptation", Command::Unfreeze)
            .error
            .is_none()
    );

    let finished = drain_drift_adaptation(&mut plane, "freeze");
    assert_eq!(
        finished.adaptation.finish_reason,
        Some(DriftAdaptationFinishReason::Promoted)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn auto_canary_on_drift_replay_rejects_a_forged_promotion_claim() {
    let _reference_delay_slot = hold_reference_delay_slot();
    let directory = tempdir().expect("auto-canary forged fixture");
    let (mut plane, parent, candidate) = auto_canary_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();

    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: candidate.genome_id.clone(),
            reason: "Bootstrap the misconfigured reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");
    let sibling = assessed_forge_child(
        &mut plane,
        "forge-improve",
        &candidate.genome_id,
        &parent.genome_id,
        true,
    );
    hephaestus_runtime::set_test_reference_delay(
        sibling.child.clone(),
        AUTO_CANARY_REGRESSION_DELAY_MILLIS,
    );
    let drift_evidence_id = "forge-evidence";
    complete_arena_test_job(
        &mut plane,
        drift_evidence_id,
        &candidate.genome_id,
        &sibling.child,
    );
    hephaestus_runtime::clear_test_reference_delay();
    plane
        .select_arena_evaluation(drift_evidence_id)
        .expect("select the latency drift evidence");
    drift_record_cmd(
        &mut plane,
        &token,
        "forge-drift",
        Command::DriftRecord {
            drift_id: "forge".to_owned(),
            world_id: world_id.clone(),
            kind: DriftKind::Latency,
            evidence_evaluation_id: drift_evidence_id.to_owned(),
        },
    )
    .expect("record a genuine latency drift against the misconfigured Champion");

    let deadline = Instant::now() + Duration::from_secs(60);
    let child = loop {
        plane
            .service_async_messages()
            .expect("advance the drift adaptation reconciliation loop");
        let shown = drift_adaptation_show(&plane, "forge");
        if let Some(child) = shown.adaptation.child_genome_id.clone() {
            break child;
        }
        assert!(
            Instant::now() < deadline,
            "the automatic adaptation never proposed a child"
        );
        thread::sleep(Duration::from_millis(2));
    };
    hephaestus_runtime::set_test_reference_delay(
        child.clone(),
        AUTO_CANARY_REGRESSION_DELAY_MILLIS,
    );
    let finished = drain_drift_adaptation(&mut plane, "forge");
    hephaestus_runtime::clear_test_reference_delay();
    assert_eq!(
        finished.adaptation.finish_reason,
        Some(DriftAdaptationFinishReason::CanaryAborted)
    );
    let finished_event_id = finished
        .adaptation
        .finished_event
        .expect("finished event")
        .event_id;

    let history = drift_history(&plane);
    let canary_id = finished
        .adaptation
        .canary_id
        .clone()
        .expect("an aborted adaptation still names its canary");
    let forged = adaptation_history_with_payload_edit(&history, &finished_event_id, |payload| {
        payload.reason = crate::DriftAdaptationFinishReason::Promoted;
        payload.final_canary_stage = Some(CanaryStage::Completed);
        payload.promotion_transition_id =
            Some(super::canary::canary_id_promotion_transition_id(&canary_id));
    });
    assert!(
        super::adaptation::verify_drift_adaptation_history(&forged).is_err(),
        "a finished event claiming a promotion that never happened must fail closed"
    );

    let connection =
        rusqlite::Connection::open(plane.data_dir.join("events.sqlite3")).expect("open ledger");
    let tampered_event = forged
        .iter()
        .find(|event| event.event_id == finished_event_id)
        .expect("forged finished event exists");
    let changed = connection
        .execute(
            "UPDATE events SET payload = ?1 WHERE event_id = ?2",
            rusqlite::params![
                tampered_event.payload.as_slice(),
                finished_event_id.as_str()
            ],
        )
        .expect("tamper with the recorded finished event");
    assert_eq!(changed, 1);
    drop(connection);
    assert!(
        plane.replay_response().is_err(),
        "replay must reject a forged drift.adaptation_finished claiming a promotion that does not exist"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn auto_canary_on_drift_resumes_idempotently_after_a_restart_mid_pipeline() {
    let _reference_delay_slot = hold_reference_delay_slot();
    let directory = tempdir().expect("auto-canary restart fixture");
    let (mut plane, parent, candidate) = auto_canary_arena_fixture(&directory);
    let token = plane.token_hex.clone();
    let world_id = parent.world_id.clone();

    champion_transition(
        &mut plane,
        &token,
        "seed",
        Command::ChampionSeed {
            transition_id: "seed".to_owned(),
            world_id: world_id.clone(),
            genome_id: candidate.genome_id.clone(),
            reason: "Bootstrap the misconfigured reference lineage.".to_owned(),
        },
    )
    .expect("seed Champion");
    let sibling = assessed_forge_child(
        &mut plane,
        "restart-improve",
        &candidate.genome_id,
        &parent.genome_id,
        true,
    );
    hephaestus_runtime::set_test_reference_delay(
        sibling.child.clone(),
        AUTO_CANARY_REGRESSION_DELAY_MILLIS,
    );
    let drift_evidence_id = "restart-evidence";
    complete_arena_test_job(
        &mut plane,
        drift_evidence_id,
        &candidate.genome_id,
        &sibling.child,
    );
    hephaestus_runtime::clear_test_reference_delay();
    plane
        .select_arena_evaluation(drift_evidence_id)
        .expect("select the latency drift evidence");
    drift_record_cmd(
        &mut plane,
        &token,
        "restart-drift",
        Command::DriftRecord {
            drift_id: "restart".to_owned(),
            world_id: world_id.clone(),
            kind: DriftKind::Latency,
            evidence_evaluation_id: drift_evidence_id.to_owned(),
        },
    )
    .expect("record a genuine latency drift against the misconfigured Champion");

    // Advance partway (through the started event, at minimum) without
    // reaching a terminal state, then drop the daemon mid-pipeline.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        plane
            .service_async_messages()
            .expect("advance the drift adaptation reconciliation loop");
        let shown = drift_adaptation_show(&plane, "restart");
        assert!(!shown.adaptation.finished, "must not finish before restart");
        if shown.adaptation.started {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the automatic adaptation never started"
        );
        thread::sleep(Duration::from_millis(2));
    }
    drop(plane);

    let mut reopened = evolve_reopen_real_worker_fixture(&directory).expect("reopen mid-pipeline");
    assert!(
        matches!(reopened.replay_response(), Ok(ResponseData::Replay { .. })),
        "reopening mid-pipeline must verify cleanly"
    );
    let finished = drain_drift_adaptation(&mut reopened, "restart");
    assert_eq!(
        finished.adaptation.finish_reason,
        Some(DriftAdaptationFinishReason::Promoted)
    );
    let champion = champion_show(&mut reopened, &token, &world_id);
    assert_eq!(
        champion.champion_genome_id.as_deref(),
        finished.adaptation.child_genome_id.as_deref(),
    );
    assert!(matches!(
        reopened
            .replay_response()
            .expect("replay after resumed promotion"),
        ResponseData::Replay { .. }
    ));
}

/// `hephaestus_runtime::set_test_reference_delay` is one process-wide slot, so
/// tests that inject a worker delay (or share its fixtures) must not run in
/// parallel: one test's clear would silently remove another's injected
/// latency mid-evaluation.
fn hold_reference_delay_slot() -> std::sync::MutexGuard<'static, ()> {
    static SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SLOT.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
