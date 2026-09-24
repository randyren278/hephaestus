use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

use hephaestus_experience::{Provenance, TraceInput, TraceKind, TraceReceipt};
use hephaestus_genome::{SourceFormat, compile_world};
use hephaestus_ledger::{EventInput, EventStore};
use tempfile::tempdir;

use super::*;

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
    plane
        .submit_job("lost-result", &genome.genome_id)
        .expect("admit direct reference job");
    let deadline = Instant::now() + Duration::from_secs(10);
    while plane.active_job.is_some() {
        plane
            .service_async_messages()
            .expect("persist evidence and detect closed result channel");
        assert!(Instant::now() < deadline, "executor did not finish");
        thread::sleep(Duration::from_millis(2));
    }
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
    plane
        .service_arena_message()
        .expect("record unexpected worker disconnect");
    assert_eq!(
        plane.state.arena_jobs["channel-drop"].terminal,
        Some(JobTerminal::Interrupted)
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
    database
        .execute_batch("DROP TRIGGER reject_fixture_scored_terminal;")
        .expect("restore successful terminal writes");
    drop(plane);

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
    }
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
