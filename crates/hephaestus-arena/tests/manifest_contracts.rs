use hephaestus_arena::{ArenaError, TrustedManifest, TrustedTask, Visibility};

fn sealed_manifest() -> TrustedManifest {
    TrustedManifest::new(
        "sealed-v1",
        Visibility::Sealed,
        vec![
            TrustedTask::new("task-b", "candidate input b", "expected-secret-b").unwrap(),
            TrustedTask::new("task-a", "candidate input a", "expected-secret-a").unwrap(),
        ],
    )
    .unwrap()
}

#[test]
fn canonical_manifest_bytes_rehydrate_through_constructor_validation() {
    let manifest = sealed_manifest();
    let bytes = serde_json::to_vec(&manifest).unwrap();

    let parsed = TrustedManifest::from_canonical_bytes(&bytes, Visibility::Sealed).unwrap();

    assert_eq!(serde_json::to_vec(&parsed).unwrap(), bytes);
    assert_eq!(
        parsed.operator_tasks(),
        vec![
            hephaestus_arena::OperatorTask {
                task_id: "task-a".to_owned(),
                input: "candidate input a".to_owned(),
            },
            hephaestus_arena::OperatorTask {
                task_id: "task-b".to_owned(),
                input: "candidate input b".to_owned(),
            },
        ]
    );
}

#[test]
fn manifest_parser_rejects_unknown_noncanonical_and_invalid_content() {
    let canonical = serde_json::to_vec(&sealed_manifest()).unwrap();
    let mut noncanonical = canonical.clone();
    noncanonical.push(b'\n');
    assert!(matches!(
        TrustedManifest::from_canonical_bytes(&noncanonical, Visibility::Sealed),
        Err(ArenaError::NonCanonicalManifest)
    ));

    let unknown =
        String::from_utf8(canonical.clone())
            .unwrap()
            .replacen('{', r#"{"unknown":true,"#, 1);
    assert!(matches!(
        TrustedManifest::from_canonical_bytes(unknown.as_bytes(), Visibility::Sealed),
        Err(ArenaError::Serialization(_))
    ));

    let wrong_schema = String::from_utf8(canonical.clone()).unwrap().replacen(
        r#""schema_version":1"#,
        r#""schema_version":2"#,
        1,
    );
    assert!(matches!(
        TrustedManifest::from_canonical_bytes(wrong_schema.as_bytes(), Visibility::Sealed),
        Err(ArenaError::UnsupportedManifestSchema(2))
    ));

    assert!(matches!(
        TrustedManifest::from_canonical_bytes(&canonical, Visibility::Visible),
        Err(ArenaError::VisibilityMismatch)
    ));

    let duplicate = br#"{"schema_version":1,"manifest_id":"sealed-v1","visibility":"sealed","tasks":[{"task_id":"duplicate","input":"a","expected_output":"x"},{"task_id":"duplicate","input":"b","expected_output":"y"}]}"#;
    assert!(matches!(
        TrustedManifest::from_canonical_bytes(duplicate, Visibility::Sealed),
        Err(ArenaError::DuplicateTaskId(task_id)) if task_id == "duplicate"
    ));
}

#[test]
fn manifest_constructor_rejects_more_tasks_than_the_bound_allows() {
    let tasks = (0..=1_000)
        .map(|index| TrustedTask::new(format!("task-{index}"), "input", "output").unwrap())
        .collect::<Vec<_>>();
    assert!(matches!(
        TrustedManifest::new("oversized", Visibility::Visible, tasks),
        Err(ArenaError::TooManyTasks)
    ));
}

#[test]
fn from_source_json_normalizes_whitespace_key_order_and_task_order() {
    let source = br#"{
        "tasks": [
            {"expected_output": "b", "input": "candidate-b", "task_id": "task-b"},
            {"expected_output": "a", "input": "candidate-a", "task_id": "task-a"}
        ],
        "visibility": "visible",
        "manifest_id": "source-v1",
        "schema_version": 1
    }"#;
    let manifest = TrustedManifest::from_source_json(source).expect("compile source manifest");
    assert_eq!(
        manifest.operator_tasks(),
        vec![
            hephaestus_arena::OperatorTask {
                task_id: "task-a".to_owned(),
                input: "candidate-a".to_owned(),
            },
            hephaestus_arena::OperatorTask {
                task_id: "task-b".to_owned(),
                input: "candidate-b".to_owned(),
            },
        ]
    );
    // The compiled result also round-trips through the canonical-bytes contract.
    let canonical = manifest.canonical_bytes().expect("encode canonical bytes");
    assert!(
        TrustedManifest::from_canonical_bytes(&canonical, Visibility::Visible)
            .expect("rehydrate canonical bytes")
            == manifest
    );
}

#[test]
fn from_source_json_rejects_an_unsupported_schema_version() {
    let source =
        br#"{"schema_version":2,"manifest_id":"source-v1","visibility":"visible","tasks":[]}"#;
    assert!(matches!(
        TrustedManifest::from_source_json(source),
        Err(ArenaError::UnsupportedManifestSchema(2))
    ));
}

#[test]
fn operator_task_view_never_contains_expected_outputs_for_either_visibility() {
    let sealed = sealed_manifest();
    let visible = TrustedManifest::new(
        "visible-v1",
        Visibility::Visible,
        vec![TrustedTask::new("visible", "shown input", "visible-secret").unwrap()],
    )
    .unwrap();

    let sealed_wire = serde_json::to_vec(&sealed.operator_tasks()).unwrap();
    let visible_wire = serde_json::to_vec(&visible.operator_tasks()).unwrap();

    assert!(
        !sealed_wire
            .windows(15)
            .any(|part| part == b"expected-secret")
    );
    assert!(
        !visible_wire
            .windows(14)
            .any(|part| part == b"visible-secret")
    );
    assert!(
        String::from_utf8(sealed_wire)
            .unwrap()
            .contains("candidate input a")
    );
    assert!(
        String::from_utf8(visible_wire)
            .unwrap()
            .contains("shown input")
    );
}
