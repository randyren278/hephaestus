//! Operator-authored prose revisions through the actual provider/evidence path.

use super::super::forge_revision::{ForgeProposalKind, decode_forge_proposal_kind};
use super::*;
use std::fmt::Write as _;

const BEFORE: &str = "Return the task unchanged.\r\ncafé ";
const AFTER: &str = "Return the task in uppercase.\r\ncafé ";
const HYPOTHESIS: &str = "Explicit uppercase instructions should improve task correctness.";

struct RevisionFixture {
    plane: ControlPlane,
    baseline: GenomeRecord,
    candidate: GenomeRecord,
    source: SelectionRecord,
    prompt_path: String,
}

fn revision_fixture(directory: &TempDir, harness_scope: bool) -> RevisionFixture {
    let (mut plane, _, _) =
        real_worker_arena_fixture_with_invariants(directory, Some(CLEAN_INVARIANTS));
    let token = plane.token_hex.clone();
    let world_path = directory.path().join("arena-world.json");
    let mut world_source: serde_json::Value =
        serde_json::from_slice(&fs::read(&world_path).unwrap()).unwrap();
    world_source["name"] = serde_json::json!("hosted-revision-world");
    world_source["mutation_scope"] = if harness_scope {
        serde_json::json!(["harness"])
    } else {
        serde_json::json!([])
    };
    fs::write(&world_path, serde_json::to_vec(&world_source).unwrap()).unwrap();
    let ResponseData::World { world } = plane.register_world(world_path.to_str().unwrap()).unwrap()
    else {
        panic!("revision World");
    };

    let mut script = "#!/bin/sh\ncase \" $* \" in *' --model=sonnet '*) ;; *) exit 41 ;; esac\ncat > \"$TMPDIR/frame\"\n".to_owned();
    for (role, instruction) in [("before", BEFORE), ("after", AFTER)] {
        for task in ["visible", "sealed"] {
            let frame_name = format!("{role}-{task}.frame");
            fs::write(plane.source_repository.join(&frame_name), format!(
                "HEPHAESTUS-PROVIDER-INPUT-V2\nFollow the agent instructions to complete the task. Section lengths count UTF-8 bytes.\nAGENT-INSTRUCTION {}\n{instruction}\nTASK {}\n{task}\n", instruction.len(), task.len()
            )).unwrap();
            let answer = if role == "after" {
                task.to_uppercase()
            } else {
                task.to_owned()
            };
            let result = serde_json::json!({"type":"result","subtype":"success","result":answer,"total_cost_usd":0.0}).to_string();
            write!(script, "if cmp -s \"$TMPDIR/frame\" '{frame_name}'; then\nprintf '%s\\n' '{result}'\nexit 0\nfi\n").unwrap();
        }
    }
    script.push_str("exit 42\n");
    fixture_git(&plane.source_repository, &["add", "."]);
    fixture_git(
        &plane.source_repository,
        &["commit", "-m", "exact revision frames", "-q"],
    );
    let fake = directory.path().join("revision-fake-claude");
    fs::write(&fake, script).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    plane = plane.with_provider_executables_for_testing("/nonexistent/codex", &fake, Vec::new());
    let artifacts = &plane.storage.as_ref().unwrap().artifacts;
    let prompt = artifacts.put(BEFORE.as_bytes()).unwrap();
    let context = artifacts
        .put(b"Keep this separate artifact unchanged.")
        .unwrap();
    let mut register = |name: &str, parents: Vec<String>| {
        let path = directory.path().join(format!("{name}.json"));
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1,"name":name,"parents":parents,
                "model":{"provider":"claude","family":"sonnet"},
                "authority":{"workspace_write":false,"network":false},
                "artifacts":{"agent.prompt":prompt.as_str(),"agent.context":context.as_str()}
            }))
            .unwrap(),
        )
        .unwrap();
        let Some(ResponseData::Genome { genome }) = dispatch_call(
            &mut plane,
            &token,
            name,
            Command::GenomeRegister {
                path: path.display().to_string(),
                world_id: world.world_id.clone(),
            },
        )
        .data
        else {
            panic!("hosted Genome registration");
        };
        genome
    };
    let baseline = register("hosted-baseline", Vec::new());
    let candidate = register("hosted-candidate", vec![baseline.genome_id.clone()]);
    complete_arena_test_job(
        &mut plane,
        "revision-source",
        &baseline.genome_id,
        &candidate.genome_id,
    );
    let ResponseData::Selection { selection: source } =
        plane.select_arena_evaluation("revision-source").unwrap()
    else {
        panic!("source selection");
    };
    let prompt_path = directory.path().join("revised-prompt.txt");
    fs::write(&prompt_path, AFTER).unwrap();
    RevisionFixture {
        plane,
        baseline,
        candidate,
        source: *source,
        prompt_path: prompt_path.display().to_string(),
    }
}

fn revise(fixture: &mut RevisionFixture, proposal_id: &str) -> Result<ResponseData, ExecuteError> {
    fixture.plane.revise_genome(
        proposal_id,
        &fixture.source.event.event_id,
        &fixture.candidate.genome_id,
        &fixture.prompt_path,
        HYPOTHESIS,
    )
}

#[test]
#[allow(clippy::too_many_lines)]
fn hosted_forge_revision_preserves_configuration_assesses_promotes_and_restarts() {
    let directory = tempdir().unwrap();
    let mut fixture = revision_fixture(&directory, true);
    // A Selection names the candidate even when it did not earn promotion.
    assert!(!fixture.source.receipt.metrics_eligible());
    let token = fixture.plane.token_hex.clone();
    let response = dispatch_call(
        &mut fixture.plane,
        &token,
        "author-prose-revision",
        Command::GenomeRevise {
            proposal_id: "prose-child".to_owned(),
            selection_event_id: fixture.source.event.event_id.clone(),
            parent_genome_id: fixture.candidate.genome_id.clone(),
            prompt_path: fixture.prompt_path.clone(),
            hypothesis: HYPOTHESIS.to_owned(),
        },
    );
    assert!(
        response.error.is_none(),
        "revision command failed: {:?}",
        response.error
    );
    let Some(ResponseData::ForgeRevision { revision }) = response.data else {
        panic!("revision response");
    };
    assert_eq!(revision.payload.schema_version, 2);
    assert_eq!(
        revision.payload.selection_event_hash,
        fixture.source.event.event_hash
    );
    assert!(!revision.promotion_eligible);
    let parent = fixture
        .plane
        .compiled_genome(&fixture.candidate.genome_id)
        .unwrap();
    let child = fixture
        .plane
        .compiled_genome(&revision.payload.child.genome_id)
        .unwrap();
    let mut expected: serde_json::Value = serde_json::from_slice(parent.canonical_json()).unwrap();
    expected["name"] = serde_json::json!(format!("{}-forge-prose-child", parent.name()));
    expected["parents"] = serde_json::json!([fixture.candidate.genome_id]);
    expected["artifacts"]["agent.prompt"] =
        serde_json::json!(revision.payload.prompt_artifact_after);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(child.canonical_json()).unwrap(),
        expected
    );
    let artifacts = &fixture.plane.storage.as_ref().unwrap().artifacts;
    assert_eq!(
        artifacts
            .get(&ArtifactId::parse(&revision.payload.prompt_artifact_after).unwrap())
            .unwrap(),
        AFTER.as_bytes()
    );
    assert_eq!(
        revise(&mut fixture, "prose-child").unwrap(),
        ResponseData::ForgeRevision {
            revision: revision.clone()
        }
    );
    fs::remove_file(&fixture.prompt_path).unwrap();
    assert!(
        matches!(
            revise(&mut fixture, "prose-child"),
            Err(ExecuteError::Invalid("file is not readable"))
        ),
        "a retry still verifies the supplied file's bytes"
    );
    fs::write(&fixture.prompt_path, AFTER).unwrap();
    fs::write(&fixture.prompt_path, "Changed retry content").unwrap();
    assert!(
        matches!(revise(&mut fixture, "prose-child"), Err(ExecuteError::Rejected(reason)) if reason == "proposal_id is already bound to different proposal content")
    );
    fs::write(&fixture.prompt_path, AFTER).unwrap();
    assert!(
        matches!(fixture.plane.revise_genome("prose-child", &fixture.source.event.event_id, &fixture.candidate.genome_id, &fixture.prompt_path, "Changed hypothesis"), Err(ExecuteError::Rejected(reason)) if reason == "proposal_id is already bound to different proposal content")
    );
    assert!(
        matches!(fixture.plane.propose_genome("prose-child", "missing-source", "missing-parent", HYPOTHESIS), Err(ExecuteError::Rejected(reason)) if reason == "proposal_id is already bound to a revision proposal")
    );

    complete_arena_test_job(
        &mut fixture.plane,
        "revision-comparison",
        &fixture.candidate.genome_id,
        &revision.payload.child.genome_id,
    );
    let ResponseData::Selection { selection } = fixture
        .plane
        .select_arena_evaluation("revision-comparison")
        .unwrap()
    else {
        panic!("child selection");
    };
    assert!(selection.receipt.metrics_eligible());
    let ResponseData::ForgeAssessment { assessment } = fixture
        .plane
        .assess_genome("prose-assessment", "prose-child", &selection.event.event_id)
        .unwrap()
    else {
        panic!("revision assessment");
    };
    assert_eq!(
        assessment.payload.outcome,
        ForgeAssessmentOutcome::MetricsPassed
    );
    assert!(!assessment.payload.promotion_eligible);
    assert_eq!(
        assessment.payload.proposal_event_hash,
        revision.event.event_hash
    );
    let token = fixture.plane.token_hex.clone();
    champion_transition(
        &mut fixture.plane,
        &token,
        "seed-prose",
        Command::ChampionSeed {
            transition_id: "seed-prose".to_owned(),
            world_id: fixture.candidate.world_id.clone(),
            genome_id: fixture.candidate.genome_id.clone(),
            reason: "Bootstrap hosted prompt".to_owned(),
        },
    )
    .unwrap();
    fixture
        .plane
        .check_arena_invariants("revision-comparison")
        .unwrap();
    champion_transition(
        &mut fixture.plane,
        &token,
        "promote-prose",
        Command::ChampionPromote {
            transition_id: "promote-prose".to_owned(),
            assessment_id: "prose-assessment".to_owned(),
        },
    )
    .unwrap();
    assert!(
        matches!(fixture.plane.gene_extract("unsupported-prose-gene", "promote-prose"), Err(ExecuteError::Rejected(reason)) if reason == "Gene extraction requires a catalog-edge proposal")
    );
    let history = fixture
        .plane
        .storage
        .as_ref()
        .unwrap()
        .ledger
        .replay_verified()
        .unwrap();
    assert_eq!(
        history
            .iter()
            .filter(|e| e.event_id == revision.event.event_id)
            .count(),
        1
    );
    assert_eq!(
        history
            .iter()
            .filter(|e| e.event_type == "gene.extracted")
            .count(),
        0
    );
    let data_dir = fixture.plane.data_dir.clone();
    let repository = fixture.plane.source_repository.clone();
    let evaluator = fixture.plane.evaluator_executable.clone();
    let worker = fixture.plane.reference_worker_executable.clone();
    drop(fixture.plane);
    let mut reopened = ControlPlane::open_with_repository_evaluator_and_reference_worker(
        &data_dir,
        &repository,
        &evaluator,
        &worker,
    )
    .unwrap();
    assert_eq!(
        reopened
            .revise_genome(
                "prose-child",
                &fixture.source.event.event_id,
                &fixture.candidate.genome_id,
                &fixture.prompt_path,
                HYPOTHESIS
            )
            .unwrap(),
        ResponseData::ForgeRevision { revision }
    );
    assert!(matches!(
        reopened.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
    assert_eq!(
        champion_show(&mut reopened, &token, &fixture.candidate.world_id)
            .champion_genome_id
            .as_deref(),
        Some(assessment.payload.child_genome_id.as_str())
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn hosted_forge_revision_rejects_invalid_sources_files_freeze_and_cross_kind_retry() {
    let directory = tempdir().unwrap();
    let mut fixture = revision_fixture(&directory, true);
    let before = fixture
        .plane
        .storage
        .as_ref()
        .unwrap()
        .ledger
        .replay_verified()
        .unwrap()
        .len();
    for (bytes, expected) in [
        (b" \r\n\t".to_vec(), "revised prompt must not be blank"),
        (vec![0xff], "source file is not UTF-8 text"),
        (
            BEFORE.as_bytes().to_vec(),
            "revised prompt must change its exact bytes",
        ),
        (
            vec![b'x'; usize::try_from(MAX_SOURCE_FILE_BYTES).unwrap() + 1],
            "file exceeds the size limit",
        ),
    ] {
        fs::write(&fixture.prompt_path, bytes).unwrap();
        let error = revise(&mut fixture, "invalid-prose").unwrap_err();
        let message = match error {
            ExecuteError::Rejected(message) => message,
            ExecuteError::Invalid(message) => message.to_owned(),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(message, expected);
    }
    fs::write(&fixture.prompt_path, AFTER).unwrap();
    assert!(
        matches!(fixture.plane.revise_genome("wrong-parent", &fixture.source.event.event_id, &fixture.baseline.genome_id, &fixture.prompt_path, HYPOTHESIS), Err(ExecuteError::Rejected(reason)) if reason == "the parent must be the selected candidate under the same World")
    );
    assert!(matches!(
        fixture.plane.revise_genome(
            "missing-selection",
            "missing",
            &fixture.candidate.genome_id,
            &fixture.prompt_path,
            HYPOTHESIS
        ),
        Err(ExecuteError::NotFound)
    ));
    assert!(
        fixture
            .plane
            .revise_genome(
                "relative",
                &fixture.source.event.event_id,
                &fixture.candidate.genome_id,
                "revised.txt",
                HYPOTHESIS
            )
            .is_err()
    );
    assert!(
        fixture
            .plane
            .revise_genome(
                "bad id",
                &fixture.source.event.event_id,
                &fixture.candidate.genome_id,
                &fixture.prompt_path,
                HYPOTHESIS
            )
            .is_err()
    );
    assert!(
        fixture
            .plane
            .revise_genome(
                "bad-hypothesis",
                &fixture.source.event.event_id,
                &fixture.candidate.genome_id,
                &fixture.prompt_path,
                "\n"
            )
            .is_err()
    );
    for proposal_id in ["evolve-manual", "adapt-manual"] {
        assert!(
            matches!(revise(&mut fixture, proposal_id), Err(ExecuteError::Rejected(reason)) if reason == "proposal_id uses a reserved automatic-proposal prefix")
        );
    }
    fixture.plane.state.freeze = FreezeState::frozen(&fixture.plane.operator_token);
    assert!(matches!(
        revise(&mut fixture, "frozen-prose"),
        Err(ExecuteError::Invalid("evolution is frozen"))
    ));
    fixture
        .plane
        .state
        .freeze
        .unfreeze(&fixture.plane.operator_token)
        .unwrap();
    assert_eq!(
        fixture
            .plane
            .storage
            .as_ref()
            .unwrap()
            .ledger
            .replay_verified()
            .unwrap()
            .len(),
        before
    );

    let reference_parent = fixture
        .plane
        .state
        .registered
        .genomes()
        .find(|genome| {
            genome.compiled().model_provider() == "deterministic"
                && !genome.record().parent_ids.is_empty()
        })
        .unwrap()
        .record()
        .clone();
    let ancestor = reference_parent.parent_ids[0].clone();
    complete_arena_test_job(
        &mut fixture.plane,
        "catalog-source",
        &ancestor,
        &reference_parent.genome_id,
    );
    let ResponseData::Selection { selection } = fixture
        .plane
        .select_arena_evaluation("catalog-source")
        .unwrap()
    else {
        panic!("catalog source");
    };
    assert!(
        matches!(fixture.plane.revise_genome("reference-prose", &selection.event.event_id, &reference_parent.genome_id, &fixture.prompt_path, HYPOTHESIS), Err(ExecuteError::Rejected(reason)) if reason == "prompt revisions require a hosted-provider parent")
    );
    fixture
        .plane
        .propose_genome(
            "catalog-child",
            &selection.event.event_id,
            &reference_parent.genome_id,
            HYPOTHESIS,
        )
        .unwrap();
    assert!(
        matches!(revise(&mut fixture, "catalog-child"), Err(ExecuteError::Rejected(reason)) if reason == "proposal_id is already bound to a catalog proposal")
    );
    assert!(matches!(
        fixture.plane.replay_response(),
        Ok(ResponseData::Replay { .. })
    ));
}

#[test]
fn hosted_forge_revision_requires_world_harness_authority() {
    let directory = tempdir().unwrap();
    let mut fixture = revision_fixture(&directory, false);
    let before = fixture
        .plane
        .storage
        .as_ref()
        .unwrap()
        .ledger
        .replay_verified()
        .unwrap()
        .len();
    assert!(
        matches!(revise(&mut fixture, "unauthorized-prose"), Err(ExecuteError::Rejected(reason)) if reason == "World mutation scope does not authorize harness mutations")
    );
    assert_eq!(
        fixture
            .plane
            .storage
            .as_ref()
            .unwrap()
            .ledger
            .replay_verified()
            .unwrap()
            .len(),
        before
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn hosted_forge_revision_history_rejects_tampered_bindings_and_canonical_payloads() {
    let directory = tempdir().unwrap();
    let mut fixture = revision_fixture(&directory, true);
    let ResponseData::ForgeRevision { revision } = revise(&mut fixture, "tamper-prose").unwrap()
    else {
        panic!("revision response");
    };
    let history = fixture
        .plane
        .storage
        .as_ref()
        .unwrap()
        .ledger
        .replay_verified()
        .unwrap();
    for case in [
        "selection-hash",
        "selection-id",
        "evaluation-id",
        "world",
        "parent",
        "before",
        "after",
        "artifact-name",
        "hypothesis",
        "schema-1",
        "schema-3",
        "unknown-field",
        "noncanonical",
        "actor",
        "aggregate",
        "event-id",
        "causal-order",
    ] {
        let mut changed = history.clone();
        let event = changed
            .iter_mut()
            .find(|e| e.event_id == revision.event.event_id)
            .unwrap();
        let mut value = serde_json::to_value(&revision.payload).unwrap();
        match case {
            "selection-hash" => value["selection_event_hash"] = serde_json::json!("0".repeat(64)),
            "selection-id" => value["selection_event_id"] = serde_json::json!("missing-selection"),
            "evaluation-id" => value["evaluation_id"] = serde_json::json!("wrong-evaluation"),
            "world" => value["world_id"] = serde_json::json!(fixture.baseline.genome_id),
            "parent" => value["parent_genome_id"] = serde_json::json!(fixture.baseline.genome_id),
            "before" => {
                value["prompt_artifact_before"] = serde_json::json!(
                    fixture
                        .plane
                        .storage
                        .as_ref()
                        .unwrap()
                        .artifacts
                        .put(b"An unrelated before prompt.")
                        .unwrap()
                        .as_str()
                );
            }
            "after" => value["prompt_artifact_after"] = value["prompt_artifact_before"].clone(),
            "artifact-name" => value["artifact_name"] = serde_json::json!("agent.context"),
            "hypothesis" => value["hypothesis"] = serde_json::json!("\n"),
            "schema-1" => value["schema_version"] = serde_json::json!(1),
            "schema-3" => value["schema_version"] = serde_json::json!(3),
            "unknown-field" => value["unexpected"] = serde_json::json!(true),
            "actor" => event.actor = "untrusted".to_owned(),
            "aggregate" => event.aggregate_id.push_str("wrong"),
            "event-id" => event.event_id.push_str("wrong"),
            "causal-order" | "noncanonical" => (),
            _ => unreachable!(),
        }
        event.payload = serde_json::to_vec(&value).unwrap();
        if case == "noncanonical" {
            event.payload.push(b' ');
        }
        if case == "causal-order" {
            let proposal_sequence = event.sequence;
            changed
                .iter_mut()
                .find(|source| source.event_id == fixture.source.event.event_id)
                .unwrap()
                .sequence = proposal_sequence + 1;
        }
        assert!(
            verify_forge_history(
                &fixture.plane.storage.as_ref().unwrap().artifacts,
                &changed,
                &fixture.plane.state.registered
            )
            .is_err(),
            "accepted {case}"
        );
        if case != "causal-order" {
            // Recompute a valid ledger hash chain. Startup must reject the
            // semantic tamper independently of ledger-integrity detection.
            let copied = tempdir().unwrap();
            let data_dir = copied.path().join("data");
            copy_directory(&fixture.plane.data_dir, &data_dir);
            for name in ["events.sqlite3", "events.sqlite3-wal", "events.sqlite3-shm"] {
                let path = data_dir.join(name);
                if path.exists() {
                    fs::remove_file(path).unwrap();
                }
            }
            let mut ledger = EventStore::open(data_dir.join("events.sqlite3")).unwrap();
            for event in &changed {
                ledger
                    .append(EventInput::new(
                        &event.event_id,
                        &event.aggregate_id,
                        &event.event_type,
                        &event.actor,
                        event.timestamp_millis,
                        &event.payload,
                    ))
                    .unwrap();
            }
            ledger
                .replay_verified()
                .expect("tamper fixture has a valid hash chain");
            drop(ledger);
            assert!(
                ControlPlane::open_with_repository_evaluator_and_reference_worker(
                    &data_dir,
                    &fixture.plane.source_repository,
                    &fixture.plane.evaluator_executable,
                    &fixture.plane.reference_worker_executable
                )
                .is_err(),
                "startup accepted {case}"
            );
        }
    }
    for case in ["model", "other-artifact"] {
        let original = fixture
            .plane
            .compiled_genome(&revision.payload.child.genome_id)
            .unwrap();
        let mut source: serde_json::Value =
            serde_json::from_slice(original.canonical_json()).unwrap();
        let parent = fixture
            .plane
            .compiled_genome(&fixture.candidate.genome_id)
            .unwrap();
        let artifacts = &fixture.plane.storage.as_ref().unwrap().artifacts;
        if case == "model" {
            source["model"]["family"] = serde_json::json!("opus");
        } else {
            source["artifacts"]["agent.context"] =
                serde_json::json!(artifacts.put(b"changed context").unwrap().as_str());
        }
        let world = fixture
            .plane
            .registered_world(&fixture.candidate.world_id)
            .unwrap();
        let compiled = compile_genome(
            &source.to_string(),
            SourceFormat::Json,
            &world,
            &BTreeMap::from([(fixture.candidate.genome_id.clone(), parent)]),
            artifacts,
        )
        .unwrap();
        let mut payload = revision.payload.clone();
        payload.child.genome_id = compiled.id().to_owned();
        payload.child.artifact_id = artifacts
            .put(compiled.canonical_json())
            .unwrap()
            .as_str()
            .to_owned();
        let mut changed = history.clone();
        changed
            .iter_mut()
            .find(|e| e.event_id == revision.event.event_id)
            .unwrap()
            .payload = serde_json::to_vec(&serde_json::to_value(payload).unwrap()).unwrap();
        let registered = RegisteredObjects::replay(&changed, artifacts)
            .expect("forged child is valid compiler output");
        assert!(
            verify_forge_history(artifacts, &changed, &registered).is_err(),
            "Forge accepted an unrelated {case} change"
        );
    }
    let event = history
        .iter()
        .find(|e| e.event_id == revision.event.event_id)
        .unwrap();
    assert!(matches!(
        decode_forge_proposal_kind(event).unwrap(),
        ForgeProposalKind::Revision(_)
    ));
    assert!(
        decode_forge_proposal(event).is_err(),
        "catalog-only evolution must reject revisions"
    );
    let mut duplicate = history.clone();
    duplicate.push(event.clone());
    assert!(
        verify_forge_history(
            &fixture.plane.storage.as_ref().unwrap().artifacts,
            &duplicate,
            &fixture.plane.state.registered
        )
        .is_err()
    );
    let artifact = ArtifactId::parse(&revision.payload.prompt_artifact_after).unwrap();
    let artifacts = ArtifactStore::open(fixture.plane.data_dir.join("blobs")).unwrap();
    fs::write(artifacts.path_for(&artifact), b"corrupt content").unwrap();
    assert!(
        matches!(fixture.plane.replay_response(), Err(ExecuteError::Internal)),
        "replay must re-read changed CAS bytes"
    );
    let data_dir = fixture.plane.data_dir.clone();
    let repository = fixture.plane.source_repository.clone();
    let evaluator = fixture.plane.evaluator_executable.clone();
    let worker = fixture.plane.reference_worker_executable.clone();
    drop(fixture.plane);
    assert!(
        ControlPlane::open_with_repository_evaluator_and_reference_worker(
            &data_dir,
            &repository,
            &evaluator,
            &worker
        )
        .is_err()
    );
}

#[test]
fn hosted_forge_revision_command_validates_fields_before_auditing_or_reading() {
    let directory = tempdir().unwrap();
    let mut plane = ControlPlane::open(directory.path()).unwrap();
    let token = plane.token_hex.clone();
    let base = Command::GenomeRevise {
        proposal_id: "prose".to_owned(),
        selection_event_id: "selection".to_owned(),
        parent_genome_id: "parent".to_owned(),
        prompt_path: "/unread".to_owned(),
        hypothesis: HYPOTHESIS.to_owned(),
    };
    let before = plane
        .storage
        .as_ref()
        .unwrap()
        .ledger
        .replay_verified()
        .unwrap()
        .len();
    for field in ["proposal", "selection", "parent", "path", "hypothesis"] {
        let mut command = base.clone();
        if let Command::GenomeRevise {
            proposal_id,
            selection_event_id,
            parent_genome_id,
            prompt_path,
            hypothesis,
        } = &mut command
        {
            match field {
                "proposal" => *proposal_id = "bad id".to_owned(),
                "selection" => selection_event_id.clear(),
                "parent" => parent_genome_id.clear(),
                "path" => prompt_path.clear(),
                "hypothesis" => *hypothesis = "\n".to_owned(),
                _ => unreachable!(),
            }
        }
        let response = dispatch_call(&mut plane, &token, field, command);
        assert_eq!(response.error.unwrap().code, ApiErrorCode::InvalidRequest);
    }
    assert_eq!(
        plane
            .storage
            .as_ref()
            .unwrap()
            .ledger
            .replay_verified()
            .unwrap()
            .len(),
        before
    );
    assert_eq!(event_type(&base), "control.genome_revise");
}

#[test]
fn hosted_forge_revision_keeps_historical_catalog_payload_bytes_and_schema() {
    // A frozen schema-1 shape without later optional catalog annotations.
    let bytes = br#"{"analysis_binding":null,"artifact_name":"agent.prompt","child":{"artifact_id":"sha256:child","genome_id":"child","name":"child","parent_ids":["parent"],"world_id":"world"},"evaluation_id":"evaluation","hypothesis":"Improve correctness.","operation_after":"ascii_uppercase","operation_before":"identity","parent_genome_id":"parent","prompt_artifact_after":"sha256:after","prompt_artifact_before":"sha256:before","proposal_id":"historical","schema_version":1,"selection_event_hash":"hash","selection_event_id":"selection","world_id":"world"}"#;
    let mut event = StoredEvent {
        sequence: 1,
        event_id: forge_event_id("historical"),
        aggregate_id: forge_aggregate_id("historical"),
        event_type: "forge.proposed".to_owned(),
        actor: OPERATOR_ACTOR.to_owned(),
        timestamp_millis: 0,
        payload: bytes.to_vec(),
        previous_hash: [0; 32],
        hash: [0; 32],
    };
    let ForgeProposalKind::Catalog(payload) = decode_forge_proposal_kind(&event).unwrap() else {
        panic!("historical catalog");
    };
    assert_eq!(
        serde_json::to_vec(&serde_json::to_value(&payload).unwrap()).unwrap(),
        bytes
    );
    let mut value = serde_json::to_value(payload).unwrap();
    value["schema_version"] = serde_json::json!(2);
    event.payload = serde_json::to_vec(&value).unwrap();
    assert!(
        decode_forge_proposal(&event).is_err(),
        "catalog-only evolution must require schema 1"
    );
}
