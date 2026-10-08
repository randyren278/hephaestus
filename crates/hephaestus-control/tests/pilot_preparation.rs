//! Real CLI/daemon contracts for registration-only pilot preparation.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use hephaestus_control::{Client, Command, ResponseData};
use serde_json::Value;
use tempfile::tempdir;

const CLI: &str = env!("CARGO_BIN_EXE_hephaestus");
const DAEMON: &str = env!("CARGO_BIN_EXE_hephaestusd");
const EVALUATOR: &str = env!("CARGO_BIN_EXE_hephaestus-reference-evaluator");

fn cli(home: &Path, arguments: &[&str]) -> Output {
    ProcessCommand::new(CLI)
        .args(arguments)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .env("GIT_DIR", home.join("foreign/.git"))
        .env("GIT_WORK_TREE", home.join("foreign"))
        .env("GIT_INDEX_FILE", home.join("foreign-index"))
        .output()
        .expect("CLI starts")
}

fn json(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(home: &Path, data: &Path, repository: &Path, provider: &Path) -> Daemon {
    let mut child = Daemon(
        ProcessCommand::new(DAEMON)
            .args([
                "--data-dir",
                data.to_str().unwrap(),
                "--source-repository",
                repository.to_str().unwrap(),
                "--evaluator-executable",
                EVALUATOR,
            ])
            .env_clear()
            .env("HOME", home)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HEPHAESTUS_CODEX_EXECUTABLE", provider)
            .env("HEPHAESTUS_CLAUDE_EXECUTABLE", provider)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("daemon starts"),
    );
    let client = Client::new(data);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if client
            .request(Command::Status)
            .is_ok_and(|response| response.error.is_none())
        {
            return child;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "daemon exited before readiness"
        );
        assert!(Instant::now() < deadline, "daemon readiness timeout");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn installed_pilot_preparation_is_frozen_idempotent_and_never_launches_a_provider() {
    let root = tempdir().unwrap();
    let backing = root.path().join("canonical-home-directory-with-a-name-that-exceeds-the-platform-unix-socket-path-limit-when-resolved");
    fs::create_dir(&backing).unwrap();
    let alias_root = tempfile::Builder::new()
        .prefix("hpilot-")
        .tempdir_in("/tmp")
        .unwrap();
    let home_path = alias_root.path().join("home");
    std::os::unix::fs::symlink(&backing, &home_path).unwrap();
    let home = home_path.as_path();
    let hooks = home.join("hooks");
    fs::create_dir(&hooks).unwrap();
    let hook_marker = home.join("git-hook-was-launched");
    let hook = hooks.join("pre-commit");
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch '{}'\nexit 91\n", hook_marker.display()),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    let template = home.join("git-template");
    fs::create_dir(&template).unwrap();
    fs::write(template.join("template-marker"), "must not copy").unwrap();
    let foreign = home.join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("owner.txt"), "preserve me").unwrap();
    // A user's signing preference must not prevent fixture initialization.
    fs::write(
        home.join(".gitconfig"),
        format!("[commit]\n gpgsign = true\n[gpg]\n program = /nonexistent-fixture-signer\n[core]\n hooksPath = {}\n[init]\n templateDir = {}\n", hooks.display(), template.display()),
    )
    .unwrap();
    let pack = home.join("pilot");
    let initialized = json(&cli(
        home,
        &[
            "--json",
            "init",
            "--fixture",
            "support-triage",
            pack.to_str().unwrap(),
        ],
    ));
    assert_eq!(initialized["fixture"], "support-triage");
    let repository = pack.join("repository");
    let tracked = ProcessCommand::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["ls-files"])
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").expect("PATH"))
        .output()
        .unwrap();
    assert!(tracked.status.success());
    assert_eq!(String::from_utf8(tracked.stdout).unwrap(), "README.md\n");
    assert!(pack.join("tasks/sealed.json").is_file());
    assert!(!hook_marker.exists());
    assert!(!repository.join(".git/template-marker").exists());
    assert_eq!(
        fs::read_to_string(foreign.join("owner.txt")).unwrap(),
        "preserve me"
    );
    assert!(!foreign.join(".git").exists());
    assert!(!home.join("foreign-index").exists());
    let original_baseline = fs::read(pack.join("baseline.template.md")).unwrap();
    let original_candidate = fs::read(pack.join("candidate.template.md")).unwrap();
    let repeat_init = cli(
        home,
        &[
            "init",
            "--fixture",
            "support-triage",
            pack.to_str().unwrap(),
        ],
    );
    assert!(!repeat_init.status.success());
    assert_eq!(
        fs::read(pack.join("candidate.template.md")).unwrap(),
        original_candidate
    );

    let marker = home.join("provider-was-launched");
    let provider = home.join("provider-stub");
    fs::write(
        &provider,
        format!("#!/bin/sh\ntouch '{}'\nexit 91\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700)).unwrap();
    let data = pack.join("data");
    assert!(data.join("control.sock").as_os_str().len() < 104);
    assert!(backing.join("pilot/data/control.sock").as_os_str().len() > 108);
    let daemon = start(home, &data, &repository, &provider);
    let prepare = |provider, model, cost| {
        cli(
            home,
            &[
                "--data-dir",
                data.to_str().unwrap(),
                "--json",
                "pilot",
                "prepare",
                pack.to_str().unwrap(),
                "--provider",
                provider,
                "--model",
                model,
                "--cost-microusd",
                cost,
            ],
        )
    };
    let client = Client::new(&data);
    let before_invalid_cost = client.request(Command::GenomeList).unwrap().data;
    for cost in ["0", "1000000001", "18446744073709551615"] {
        assert!(
            !prepare("codex", "offline-profile-model", cost)
                .status
                .success()
        );
    }
    assert_eq!(
        client.request(Command::GenomeList).unwrap().data,
        before_invalid_cost
    );
    let first = json(&prepare("codex", "offline-profile-model", "250000"));
    let second = json(&prepare("codex", "offline-profile-model", "250000"));
    for key in [
        "world_id",
        "parent_genome_id",
        "candidate_genome_id",
        "baseline_profile",
        "candidate_profile",
    ] {
        assert_eq!(first[key], second[key], "retry changed {key}");
    }
    assert_eq!(first["frozen"], true);
    assert_eq!(first["provider_work_started"], false);
    for role in ["baseline_profile", "candidate_profile"] {
        let profile = &first[role];
        assert_eq!(profile["provider"], "codex");
        assert_eq!(profile["family"], "offline-profile-model");
        assert_eq!(profile["world"]["world_id"], first["world_id"]);
        assert_eq!(profile["workspace_write"], false);
        assert_eq!(profile["network"], true);
        assert_eq!(profile["output_scoring"], "json_canonical");
        assert_eq!(profile["visible_tasks"], 12);
        assert_eq!(profile["sealed_tasks"], 12);
        assert_eq!(profile["reported_cost_limit_microusd"], "250000");
        assert_eq!(profile["paired_total_wall_millis"], 14_410_000);
    }
    let setup = PathBuf::from(first["setup_directory"].as_str().unwrap());
    assert_eq!(
        fs::metadata(&setup).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for file in ["world.json", "baseline.md", "candidate.md"] {
        assert!(setup.join(file).is_file());
    }
    let quoted_model = json(&prepare("claude", "null", "250000"));
    assert_eq!(quoted_model["baseline_profile"]["family"], "null");
    assert_eq!(quoted_model["candidate_profile"]["provider"], "claude");
    let malformed = prepare("codex", "model\nworkspace_write: true", "250000");
    assert!(!malformed.status.success());
    let bad_candidate = String::from_utf8(original_candidate.clone())
        .unwrap()
        .replace("schema_version: 1", "schema_version: 999");
    fs::write(pack.join("candidate.template.md"), bad_candidate).unwrap();
    let interrupted = prepare("codex", "offline-profile-model", "250000");
    assert!(!interrupted.status.success());
    assert!(String::from_utf8_lossy(&interrupted.stderr).contains("preparation files retained"));
    fs::write(pack.join("candidate.template.md"), &original_candidate).unwrap();
    let recovered = json(&prepare("codex", "offline-profile-model", "250000"));
    assert_eq!(recovered["world_id"], first["world_id"]);
    assert_eq!(
        recovered["candidate_genome_id"],
        first["candidate_genome_id"]
    );
    assert_eq!(
        fs::read(pack.join("baseline.template.md")).unwrap(),
        original_baseline
    );
    assert_eq!(
        fs::read(pack.join("candidate.template.md")).unwrap(),
        original_candidate
    );
    assert!(!marker.exists(), "preparation invoked a provider");

    let before = client.request(Command::GenomeList).unwrap().data;
    client.request(Command::Unfreeze).unwrap();
    let rejected = prepare("claude", "different-model", "500000");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("frozen daemon"));
    assert_eq!(client.request(Command::GenomeList).unwrap().data, before);
    assert!(matches!(
        client.request(Command::Status).unwrap().data,
        Some(ResponseData::Status {
            frozen: false,
            active_runs: 0,
            ..
        })
    ));
    assert!(!marker.exists());
    client.request(Command::Freeze).unwrap();
    let replay = client.request(Command::Replay).unwrap();
    assert!(replay.error.is_none());
    drop(daemon);
    let _restarted = start(home, &data, &repository, &provider);
    let third = json(&prepare("codex", "offline-profile-model", "250000"));
    assert_eq!(third["parent_genome_id"], first["parent_genome_id"]);
    assert_eq!(third["candidate_profile"], first["candidate_profile"]);
    assert!(!marker.exists());
}
