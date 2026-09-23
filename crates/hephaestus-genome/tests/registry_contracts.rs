use std::{collections::BTreeMap, fs};

use hephaestus_genome::{
    CompileError, CompiledGenome, CompiledWorld, GenomeRecord, RegisteredGenome, RegisteredObjects,
    RegistrationError, RegistrationKind, SourceFormat, WorldRecord, compile_genome,
    compile_markdown_genome, compile_world,
};
use hephaestus_ledger::{ArtifactId, ArtifactStore, EventInput, EventStore, StoredEvent};
use serde::Serialize;
use serde_json::json;
use tempfile::TempDir;

struct Fixture {
    _directory: TempDir,
    artifacts: ArtifactStore,
    ledger: EventStore,
    next_event: u64,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("create fixture directory");
        let artifacts =
            ArtifactStore::open(directory.path().join("blobs")).expect("open artifact store");
        let ledger =
            EventStore::open(directory.path().join("events.sqlite3")).expect("open event store");
        Self {
            _directory: directory,
            artifacts,
            ledger,
            next_event: 1,
        }
    }

    fn append<T: Serialize>(&mut self, event_type: &str, aggregate_id: &str, payload: &T) {
        let sequence = self.next_event;
        self.next_event += 1;
        self.ledger
            .append(EventInput::new(
                format!("fixture-{sequence}"),
                aggregate_id,
                event_type,
                "test-fixture",
                i64::try_from(sequence).expect("fixture sequence fits i64"),
                serde_json::to_vec(payload).expect("encode registration payload"),
            ))
            .expect("append registration event");
    }

    fn history(&self) -> Vec<StoredEvent> {
        self.ledger
            .replay_verified()
            .expect("replay fixture ledger")
    }

    fn replay(&self) -> Result<RegisteredObjects, RegistrationError> {
        RegisteredObjects::replay(&self.history(), &self.artifacts)
    }
}

#[derive(Clone)]
struct WorldFixture {
    compiled: CompiledWorld,
    record: WorldRecord,
}

#[derive(Clone)]
struct GenomeFixture {
    compiled: CompiledGenome,
    record: GenomeRecord,
}

fn world_source(name: &str) -> String {
    json!({
        "schema_version": 1,
        "name": name,
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
        "objectives": ["correctness"],
        "evaluator_artifacts": {}
    })
    .to_string()
}

fn genome_source(name: &str, parents: &[String]) -> String {
    json!({
        "schema_version": 1,
        "name": name,
        "parents": parents,
        "model": { "provider": "deterministic", "family": "reference" },
        "authority": { "workspace_write": false, "network": false },
        "artifacts": {}
    })
    .to_string()
}

fn build_world(fixture: &Fixture, name: &str) -> WorldFixture {
    let compiled = compile_world(&world_source(name), SourceFormat::Json, &fixture.artifacts)
        .expect("compile World fixture");
    let artifact = fixture
        .artifacts
        .put(compiled.canonical_json())
        .expect("store canonical World");
    let record = WorldRecord {
        world_id: compiled.id().to_owned(),
        name: compiled.name().to_owned(),
        artifact_id: artifact.as_str().to_owned(),
    };
    WorldFixture { compiled, record }
}

fn build_genome(
    fixture: &Fixture,
    world: &WorldFixture,
    name: &str,
    parents: &[GenomeFixture],
) -> GenomeFixture {
    let parent_ids = parents
        .iter()
        .map(|parent| parent.compiled.id().to_owned())
        .collect::<Vec<_>>();
    let compiled_parents = parents
        .iter()
        .map(|parent| (parent.compiled.id().to_owned(), parent.compiled.clone()))
        .collect::<BTreeMap<_, _>>();
    let compiled = compile_genome(
        &genome_source(name, &parent_ids),
        SourceFormat::Json,
        &world.compiled,
        &compiled_parents,
        &fixture.artifacts,
    )
    .expect("compile Genome fixture");
    let artifact = fixture
        .artifacts
        .put(compiled.canonical_json())
        .expect("store canonical Genome");
    let record = GenomeRecord {
        genome_id: compiled.id().to_owned(),
        name: compiled.name().to_owned(),
        world_id: world.compiled.id().to_owned(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: compiled.parents().to_vec(),
    };
    GenomeFixture { compiled, record }
}

fn register_world(fixture: &mut Fixture, world: &WorldFixture) {
    fixture.append("world.registered", &world.record.world_id, &world.record);
}

fn register_genome(fixture: &mut Fixture, genome: &GenomeFixture) {
    fixture.append(
        "genome.registered",
        &genome.record.genome_id,
        &genome.record,
    );
}

#[test]
fn replay_rehydrates_two_worlds_and_parent_child_in_registration_order() {
    let mut fixture = Fixture::new();
    let world_a = build_world(&fixture, "world-a");
    let world_b = build_world(&fixture, "world-b");
    let parent = build_genome(&fixture, &world_a, "g0", &[]);
    let child = build_genome(&fixture, &world_a, "g1", std::slice::from_ref(&parent));
    register_world(&mut fixture, &world_a);
    register_world(&mut fixture, &world_b);
    register_genome(&mut fixture, &parent);
    register_genome(&mut fixture, &child);

    let registered = fixture.replay().expect("replay valid registrations");

    assert_eq!(registered.worlds().count(), 2);
    assert_eq!(registered.genomes().count(), 2);
    assert_eq!(
        registered
            .world(&world_a.record.world_id)
            .expect("World A")
            .registration_sequence(),
        1
    );
    assert_eq!(
        registered
            .world(&world_b.record.world_id)
            .expect("World B")
            .registration_sequence(),
        2
    );
    assert_eq!(
        registered
            .genome(&parent.record.genome_id)
            .expect("parent")
            .registration_sequence(),
        3
    );
    let registered_child = registered.genome(&child.record.genome_id).expect("child");
    assert_eq!(registered_child.registration_sequence(), 4);
    assert_eq!(
        registered_child.compiled().parents(),
        &[parent.record.genome_id]
    );
    assert_eq!(registered_child.record(), &child.record);
}

#[test]
fn replay_requires_the_markdown_genomes_prompt_blob_and_exact_bytes() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "markdown-world");
    let source = "---\nschema_version: 1\nname: markdown-agent\nparents: []\nmodel:\n  provider: deterministic\n  family: reference\nauthority:\n  workspace_write: false\n  network: false\nartifacts: {}\n---\n# Inventory rules\n\nKeep exact prompt spacing.  \n";
    let prompt = "# Inventory rules\n\nKeep exact prompt spacing.  \n";
    let compiled = compile_markdown_genome(
        source,
        &world.compiled,
        &BTreeMap::new(),
        &fixture.artifacts,
    )
    .expect("compile Markdown Genome");
    let canonical: serde_json::Value =
        serde_json::from_slice(compiled.canonical_json()).expect("canonical Genome JSON");
    let prompt_id = ArtifactId::parse(
        canonical["artifacts"]["agent.prompt"]
            .as_str()
            .expect("prompt artifact address"),
    )
    .expect("canonical prompt id");
    assert_eq!(
        fixture.artifacts.get(&prompt_id).unwrap(),
        prompt.as_bytes()
    );

    let genome_artifact = fixture
        .artifacts
        .put(compiled.canonical_json())
        .expect("store canonical Genome");
    let genome = GenomeFixture {
        record: GenomeRecord {
            genome_id: compiled.id().to_owned(),
            name: compiled.name().to_owned(),
            world_id: world.compiled.id().to_owned(),
            artifact_id: genome_artifact.as_str().to_owned(),
            parent_ids: compiled.parents().to_vec(),
        },
        compiled,
    };
    register_world(&mut fixture, &world);
    register_genome(&mut fixture, &genome);
    assert!(
        fixture
            .replay()
            .unwrap()
            .genome(genome.record.genome_id.as_str())
            .is_some()
    );

    let prompt_path = fixture.artifacts.path_for(&prompt_id);
    fs::remove_file(&prompt_path).unwrap();
    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::Compile {
            source: CompileError::UnresolvedArtifact(_),
            kind: RegistrationKind::Genome,
            ..
        })
    ));

    fixture.artifacts.put(prompt.as_bytes()).unwrap();
    fs::write(&prompt_path, b"tampered prompt bytes").unwrap();
    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::Compile {
            source: CompileError::ArtifactIntegrity(_),
            kind: RegistrationKind::Genome,
            ..
        })
    ));
}

#[test]
fn genome_before_world_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let genome = build_genome(&fixture, &world, "g0", &[]);
    register_genome(&mut fixture, &genome);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::WorldNotRegistered(id)) if id == world.record.world_id
    ));
}

#[test]
fn child_before_parent_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let parent = build_genome(&fixture, &world, "g0", &[]);
    let child = build_genome(&fixture, &world, "g1", std::slice::from_ref(&parent));
    register_world(&mut fixture, &world);
    register_genome(&mut fixture, &child);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::ParentNotRegistered(id)) if id == parent.record.genome_id
    ));
}

#[test]
fn cross_world_parent_is_rejected() {
    let mut fixture = Fixture::new();
    let world_a = build_world(&fixture, "world-a");
    let world_b = build_world(&fixture, "world-b");
    let parent = build_genome(&fixture, &world_a, "g0", &[]);
    let child = build_genome(&fixture, &world_b, "g1", std::slice::from_ref(&parent));
    register_world(&mut fixture, &world_a);
    register_world(&mut fixture, &world_b);
    register_genome(&mut fixture, &parent);
    register_genome(&mut fixture, &child);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::CrossWorldParent {
            parent_id,
            parent_world_id,
            child_world_id,
        }) if parent_id == parent.record.genome_id
            && parent_world_id == world_a.record.world_id
            && child_world_id == world_b.record.world_id
    ));
}

#[test]
fn world_registration_aggregate_must_match_identity() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    fixture.append("world.registered", "wrong-world", &world.record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::AggregateMismatch {
            expected,
            actual,
            ..
        }) if expected == world.record.world_id && actual == "wrong-world"
    ));
}

#[test]
fn genome_registration_aggregate_must_match_identity() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let genome = build_genome(&fixture, &world, "g0", &[]);
    register_world(&mut fixture, &world);
    fixture.append("genome.registered", "wrong-genome", &genome.record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::AggregateMismatch {
            expected,
            actual,
            ..
        }) if expected == genome.record.genome_id && actual == "wrong-genome"
    ));
}

#[test]
fn noncanonical_world_source_is_rejected() {
    let mut fixture = Fixture::new();
    let source = format!("{}\n", world_source("world-a"));
    let compiled = compile_world(&source, SourceFormat::Json, &fixture.artifacts)
        .expect("compile noncanonical source");
    let artifact = fixture
        .artifacts
        .put(source.as_bytes())
        .expect("store noncanonical source");
    let record = WorldRecord {
        world_id: compiled.id().to_owned(),
        name: compiled.name().to_owned(),
        artifact_id: artifact.as_str().to_owned(),
    };
    fixture.append("world.registered", &record.world_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::NonCanonicalArtifact {
            kind: RegistrationKind::World,
            id,
        }) if id == record.world_id
    ));
}

#[test]
fn noncanonical_genome_source_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    register_world(&mut fixture, &world);
    let source = format!("{}\n", genome_source("g0", &[]));
    let compiled = compile_genome(
        &source,
        SourceFormat::Json,
        &world.compiled,
        &BTreeMap::new(),
        &fixture.artifacts,
    )
    .expect("compile noncanonical source");
    let artifact = fixture
        .artifacts
        .put(source.as_bytes())
        .expect("store noncanonical source");
    let record = GenomeRecord {
        genome_id: compiled.id().to_owned(),
        name: compiled.name().to_owned(),
        world_id: world.record.world_id.clone(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: Vec::new(),
    };
    fixture.append("genome.registered", &record.genome_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::NonCanonicalArtifact {
            kind: RegistrationKind::Genome,
            id,
        }) if id == record.genome_id
    ));
}

#[test]
fn world_name_metadata_must_match_compiled_artifact() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let mut record = world.record.clone();
    record.name = "forged-name".to_owned();
    fixture.append("world.registered", &record.world_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::MetadataMismatch {
            kind: RegistrationKind::World,
            field: "name",
            ..
        })
    ));
}

#[test]
fn genome_name_metadata_must_match_compiled_artifact() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let genome = build_genome(&fixture, &world, "g0", &[]);
    register_world(&mut fixture, &world);
    let mut record = genome.record.clone();
    record.name = "forged-name".to_owned();
    fixture.append("genome.registered", &record.genome_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::MetadataMismatch {
            kind: RegistrationKind::Genome,
            field: "name",
            ..
        })
    ));
}

#[test]
fn genome_parent_metadata_must_match_compiled_artifact() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let parent = build_genome(&fixture, &world, "g0", &[]);
    let child = build_genome(&fixture, &world, "g1", std::slice::from_ref(&parent));
    register_world(&mut fixture, &world);
    register_genome(&mut fixture, &parent);
    let mut record = child.record.clone();
    record.parent_ids.clear();
    fixture.append("genome.registered", &record.genome_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::MetadataMismatch {
            kind: RegistrationKind::Genome,
            field: "parent_ids",
            ..
        })
    ));
}

#[test]
fn malformed_world_artifact_is_rejected() {
    let mut fixture = Fixture::new();
    let artifact = fixture
        .artifacts
        .put(br#"{"schema_version":1}"#)
        .expect("store malformed World");
    let record = WorldRecord {
        world_id: format!("hephaestus:world:{}", artifact.as_str()),
        name: "malformed".to_owned(),
        artifact_id: artifact.as_str().to_owned(),
    };
    fixture.append("world.registered", &record.world_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::Compile {
            kind: RegistrationKind::World,
            ..
        })
    ));
}

#[test]
fn missing_registered_artifact_is_rejected() {
    let mut fixture = Fixture::new();
    let missing = "a".repeat(64);
    let record = WorldRecord {
        world_id: format!("hephaestus:world:{missing}"),
        name: "missing".to_owned(),
        artifact_id: missing,
    };
    fixture.append("world.registered", &record.world_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::Ledger(_))
    ));
}

#[test]
fn identical_duplicate_registration_is_idempotent_and_keeps_first_sequence() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    register_world(&mut fixture, &world);
    register_world(&mut fixture, &world);

    let registered = fixture.replay().expect("replay duplicate registration");

    assert_eq!(registered.worlds().count(), 1);
    assert_eq!(
        registered
            .world(&world.record.world_id)
            .expect("registered World")
            .registration_sequence(),
        1
    );
}

#[test]
fn conflicting_duplicate_registration_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    register_world(&mut fixture, &world);
    let mut conflicting = world.record.clone();
    conflicting.name = "changed-after-release".to_owned();
    fixture.append("world.registered", &conflicting.world_id, &conflicting);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::ConflictingRegistration {
            kind: RegistrationKind::World,
            id,
        }) if id == world.record.world_id
    ));
}

#[test]
fn replaying_the_same_history_produces_identical_record_maps() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let parent = build_genome(&fixture, &world, "g0", &[]);
    let child = build_genome(&fixture, &world, "g1", std::slice::from_ref(&parent));
    register_world(&mut fixture, &world);
    register_genome(&mut fixture, &parent);
    register_genome(&mut fixture, &child);
    let history = fixture.history();

    let first = RegisteredObjects::replay(&history, &fixture.artifacts).expect("first replay");
    let second = RegisteredObjects::replay(&history, &fixture.artifacts).expect("second replay");

    assert_eq!(first.world_records(), second.world_records());
    assert_eq!(first.genome_records(), second.genome_records());
    assert_eq!(
        first
            .genomes()
            .map(RegisteredGenome::registration_sequence)
            .collect::<Vec<_>>(),
        second
            .genomes()
            .map(RegisteredGenome::registration_sequence)
            .collect::<Vec<_>>()
    );
}

#[test]
fn conflicting_duplicate_genome_registration_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let genome = build_genome(&fixture, &world, "g0", &[]);
    register_world(&mut fixture, &world);
    register_genome(&mut fixture, &genome);
    let mut conflicting = genome.record.clone();
    conflicting.name = "changed-after-release".to_owned();
    fixture.append("genome.registered", &conflicting.genome_id, &conflicting);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::ConflictingRegistration {
            kind: RegistrationKind::Genome,
            id,
        }) if id == genome.record.genome_id
    ));
}

#[test]
fn pretty_printed_registration_payload_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let pretty = serde_json::to_vec_pretty(&world.record).expect("encode pretty payload");
    assert_ne!(pretty, serde_json::to_vec(&world.record).unwrap());
    fixture
        .ledger
        .append(EventInput::new(
            "pretty-world",
            &world.record.world_id,
            "world.registered",
            "test-fixture",
            1,
            pretty,
        ))
        .expect("append pretty payload");

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::NonCanonicalPayload {
            kind: RegistrationKind::World,
            event_id,
        }) if event_id == "pretty-world"
    ));
}

#[test]
fn identical_duplicate_genome_registration_is_idempotent() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let genome = build_genome(&fixture, &world, "g0", &[]);
    register_world(&mut fixture, &world);
    register_genome(&mut fixture, &genome);
    register_genome(&mut fixture, &genome);

    let registered = fixture
        .replay()
        .expect("replay duplicate Genome registration");

    assert_eq!(registered.genomes().count(), 1);
    let registered_world = registered
        .world(&world.record.world_id)
        .expect("registered World");
    assert_eq!(registered_world.record(), &world.record);
    assert_eq!(
        registered
            .genome(&genome.record.genome_id)
            .expect("registered Genome")
            .registration_sequence(),
        2
    );
}

#[test]
fn non_json_registration_payload_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    fixture
        .ledger
        .append(EventInput::new(
            "garbage-world",
            &world.record.world_id,
            "world.registered",
            "test-fixture",
            1,
            b"not json",
        ))
        .expect("append garbage payload");

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::InvalidPayload {
            kind: RegistrationKind::World,
            event_id,
        }) if event_id == "garbage-world"
    ));
}

#[test]
fn non_utf8_genome_artifact_is_rejected() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    register_world(&mut fixture, &world);
    let artifact = fixture
        .artifacts
        .put(&[0xff, 0xfe, 0x00])
        .expect("store binary artifact");
    let record = GenomeRecord {
        genome_id: format!("hephaestus:genome:{}", artifact.as_str()),
        name: "binary".to_owned(),
        world_id: world.record.world_id.clone(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: Vec::new(),
    };
    fixture.append("genome.registered", &record.genome_id, &record);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::InvalidUtf8 {
            kind: RegistrationKind::Genome,
            id,
        }) if id == record.genome_id
    ));
}

#[test]
fn genome_artifact_wider_than_its_world_is_rejected_with_the_compiler_reason() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    register_world(&mut fixture, &world);
    let widened = json!({
        "schema_version": 1,
        "name": "widened",
        "parents": [],
        "model": { "provider": "deterministic", "family": "reference" },
        "authority": { "workspace_write": false, "network": true },
        "artifacts": {}
    });
    let artifact = fixture
        .artifacts
        .put(&serde_json::to_vec(&widened).unwrap())
        .expect("store widened Genome");
    let record = GenomeRecord {
        genome_id: format!("hephaestus:genome:{}", artifact.as_str()),
        name: "widened".to_owned(),
        world_id: world.record.world_id.clone(),
        artifact_id: artifact.as_str().to_owned(),
        parent_ids: Vec::new(),
    };
    fixture.append("genome.registered", &record.genome_id, &record);

    let error = fixture.replay().expect_err("widened Genome rejected");
    assert!(matches!(
        &error,
        RegistrationError::Compile {
            kind: RegistrationKind::Genome,
            id,
            ..
        } if id == &record.genome_id
    ));
    assert!(std::error::Error::source(&error).is_some());
    assert!(error.to_string().contains("Compile"));
}

#[test]
fn registration_errors_expose_their_causes() {
    let mut fixture = Fixture::new();
    let missing = "b".repeat(64);
    let record = WorldRecord {
        world_id: format!("hephaestus:world:{missing}"),
        name: "missing".to_owned(),
        artifact_id: missing,
    };
    fixture.append("world.registered", &record.world_id, &record);
    let ledger_error = fixture.replay().expect_err("missing artifact rejected");
    assert!(matches!(ledger_error, RegistrationError::Ledger(_)));
    assert!(std::error::Error::source(&ledger_error).is_some());

    let plain = RegistrationError::WorldNotRegistered("w".to_owned());
    assert!(std::error::Error::source(&plain).is_none());
    assert!(plain.to_string().contains("WorldNotRegistered"));
}

#[test]
fn world_record_identity_must_match_compiled_artifact() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let mut mislabeled = world.record.clone();
    mislabeled.world_id = format!("hephaestus:world:{}", "c".repeat(64));
    fixture.append("world.registered", &mislabeled.world_id, &mislabeled);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::MetadataMismatch {
            kind: RegistrationKind::World,
            field: "world_id",
            ..
        })
    ));
}

#[test]
fn genome_record_identity_must_match_compiled_artifact() {
    let mut fixture = Fixture::new();
    let world = build_world(&fixture, "world-a");
    let genome = build_genome(&fixture, &world, "g0", &[]);
    register_world(&mut fixture, &world);
    let mut mislabeled = genome.record.clone();
    mislabeled.genome_id = format!("hephaestus:genome:{}", "d".repeat(64));
    fixture.append("genome.registered", &mislabeled.genome_id, &mislabeled);

    assert!(matches!(
        fixture.replay(),
        Err(RegistrationError::MetadataMismatch {
            kind: RegistrationKind::Genome,
            field: "genome_id",
            ..
        })
    ));
}
