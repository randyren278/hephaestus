use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

use hephaestus_arena::TrustedTask;
use hephaestus_experience::{Provenance, TraceInput, TraceKind, TraceReceipt};
use hephaestus_genome::{SourceFormat, compile_world};
use hephaestus_ledger::{EventInput, EventStore};
use tempfile::tempdir;

use super::*;

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

#[test]
#[allow(clippy::too_many_lines)]
fn forge_proposal_replays_and_rejects_tampered_selection_and_metadata() {
    let directory = tempdir().expect("Forge projection fixture");
    let (mut plane, parent, candidate) = real_worker_arena_fixture(&directory);
    let evaluation_id = "forge-projection-evaluation";
    assert!(matches!(
        plane
            .submit_arena_job(evaluation_id, &parent.genome_id, &candidate.genome_id)
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
    assert!(matches!(
        verify_arena_evaluation_records(&plane.data_dir, &plane.state),
        Err(ControlError::Projection(message))
            if message == "Arena terminal differs from trusted evaluation evidence"
    ));
    plane
        .state
        .arena_jobs
        .get_mut(evaluation_id)
        .expect("completed Arena record")
        .evaluation = mismatched_record;

    let unavailable_data = directory.path().join("unavailable-evidence");
    fs::create_dir(&unavailable_data).expect("create unavailable evidence fixture");
    symlink(plane.data_dir.join("blobs"), unavailable_data.join("blobs"))
        .expect("reuse canonical evidence artifacts");
    fs::create_dir(unavailable_data.join("events.sqlite3")).expect("block evaluation store path");
    let unavailable = verify_arena_evaluation_records(&unavailable_data, &plane.state);
    assert!(matches!(
        unavailable,
        Err(ControlError::Projection(message))
            if message == "Arena evidence stores are unavailable"
    ));

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
    verify_forge_history(&plane.data_dir, &history, &plane.state.registered)
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
    let missing_selection_result =
        verify_forge_history(&plane.data_dir, &missing_selection, &plane.state.registered);
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
            &plane.data_dir,
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
        verify_forge_history(&plane.data_dir, &wrong_world, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge source World is not registered"
    ));

    let unverified_selection =
        forge_history_with_selection_edit(&history, &selection_event_id, |event| {
            event.hash[0] ^= 0xff;
        });
    assert!(matches!(
        verify_forge_history(
            &plane.data_dir,
            &unverified_selection,
            &plane.state.registered
        ),
        Err(ControlError::Projection(message))
            if message == "Forge source selection is unverified"
    ));

    let noncanonical_proposal =
        forge_history_with_selection_edit(&history, &forge_event_id, |event| {
            event.payload.push(b' ');
        });
    assert!(matches!(
        verify_forge_history(
            &plane.data_dir,
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
        verify_forge_history(&plane.data_dir, &wrong_parent, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge proposal is not bound to its selected candidate"
    ));

    let wrong_selection_hash =
        forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
            payload.selection_event_hash = "0".repeat(64);
        });
    assert!(matches!(
        verify_forge_history(&plane.data_dir, &wrong_selection_hash, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge proposal is not bound to its selected candidate"
    ));

    let wrong_child = forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
        payload.child.name.push_str("-tampered");
    });
    assert!(matches!(
        verify_forge_history(&plane.data_dir, &wrong_child, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge proposal child differs from its registered lineage"
    ));

    let wrong_operation = forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
        payload.operation_after = "identity".to_owned();
    });
    assert!(matches!(
        verify_forge_history(&plane.data_dir, &wrong_operation, &plane.state.registered),
        Err(ControlError::Projection(message))
            if message == "Forge prompt mutation is not the supported one-step operation flip"
    ));

    let invalid_hypothesis =
        forge_history_with_payload_edit(&history, &forge_event_id, |payload| {
            payload.hypothesis = " \n".to_owned();
        });
    assert!(matches!(
        verify_forge_history(&plane.data_dir, &invalid_hypothesis, &plane.state.registered),
        Err(ControlError::Projection(message)) if message == "Forge hypothesis is invalid"
    ));

    let mut invalid_actor = history.clone();
    invalid_actor
        .iter_mut()
        .find(|event| event.event_id == forge_event_id)
        .expect("Forge event exists")
        .actor = "untrusted-actor".to_owned();
    assert!(matches!(
        verify_forge_history(&plane.data_dir, &invalid_actor, &plane.state.registered),
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
    assert!(matches!(
        forge_prompt_mutation(
            &artifacts,
            &plane.state.registered,
            &promptless.genome_id,
            &world.world_id
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
            &world.world_id
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
            &world.world_id
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
                hypothesis: "The child should preserve case.".into(),
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
        r#"{"schema_version":1,"name":"dispatch-world","laws":{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0},"authority_ceiling":{"workspace_write":false,"network":false},"mutation_scope":[],"promotion":{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500},"objectives":["correctness"],"evaluator_artifacts":{}}"#,
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
            &candidate.genome_id
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
                &candidate.genome_id
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
            &candidate.genome_id
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
        verify_selection_history(directory.path(), &[event], &plane.state.registered),
        Err(ControlError::Projection(message)) if message == "selection World is not registered"
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
        plane.submit_arena_job("missing-sealed", &parent.genome_id, &candidate.genome_id),
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
        plane.submit_arena_job("missing-evaluator", &parent.genome_id, &candidate.genome_id),
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
        plane.submit_arena_job("combined-over-limit", &parent.genome_id, &candidate.genome_id),
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
    let (_, parent, candidate) = register_dispatch_arena_objects(&mut plane, &token, directory);
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
                .submit_arena_job(job_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(job_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(job_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("channel-drop", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(trials_error_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(cross_run_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("scoring-failure", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("scorer-launch-failure", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(success_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("scorer-launch-retry", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("cancelled-scoring", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("scoring-timeout", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(deadline_replay_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("scoring-success", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("scoring-commit-failure", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("terminal-write-failure", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("cancel-terminal-write-failure", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job("timeout-terminal-write-failure", &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(failed_commit_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(phase_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(committing_id, &parent.genome_id, &candidate.genome_id)
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
            .submit_arena_job(recovery_id, &parent.genome_id, &candidate.genome_id)
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
    drop(artifacts);
    let world_path = directory.path().join("arena-world.json");
    fs::write(
        &world_path,
        format!(
            r#"{{"schema_version":1,"name":"dispatch-arena","laws":{{"candidate_network":false,"candidate_evaluator_access":false,"maximum_cost_microusd":0}},"authority_ceiling":{{"workspace_write":false,"network":false}},"mutation_scope":[],"promotion":{{"minimum_delta_bps":0,"maximum_regressions":0,"confidence_bps":9500}},"objectives":["correctness"],"evaluator_artifacts":{{"arena.visible_manifest":"{}","arena.sealed_manifest":"{}","arena.evaluator":"{}","arena.runtime_verifier":"{}"}}}}"#,
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
            hypothesis: "valid hypothesis".to_owned(),
        },
        Command::GenomePropose {
            proposal_id: "proposal".to_owned(),
            selection_event_id: String::new(),
            parent_genome_id: "candidate".to_owned(),
            hypothesis: "valid hypothesis".to_owned(),
        },
        Command::GenomePropose {
            proposal_id: "proposal".to_owned(),
            selection_event_id: "selection".to_owned(),
            parent_genome_id: "candidate".to_owned(),
            hypothesis: "\n".to_owned(),
        },
        Command::GenomePropose {
            proposal_id: "proposal".to_owned(),
            selection_event_id: "selection".to_owned(),
            parent_genome_id: "candidate".to_owned(),
            hypothesis: "x".repeat(513),
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
        },
        Command::EvaluatePair {
            evaluation_id: "evaluation".to_owned(),
            parent_genome_id: " ".to_owned(),
            candidate_genome_id: "candidate".to_owned(),
        },
        Command::EvaluatePair {
            evaluation_id: "evaluation".to_owned(),
            parent_genome_id: "parent".to_owned(),
            candidate_genome_id: String::new(),
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
        events,
        artifacts,
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

fn assert_runtime_directories_clean(data_dir: &Path) {
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
        } else {
            assert!(
                !name.starts_with("reference-worker-") && !name.starts_with("sandbox-"),
                "synchronous run left runtime state behind: {name}"
            );
        }
    }
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
