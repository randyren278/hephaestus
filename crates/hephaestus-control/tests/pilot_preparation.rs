//! Real CLI/daemon contracts for registration-only pilot preparation.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use hephaestus_control::{ApiErrorCode, Client, Command, JobState, ResponseData};
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
    start_with_evaluator(home, data, repository, provider, Path::new(EVALUATOR))
}

fn start_with_evaluator(
    home: &Path,
    data: &Path,
    repository: &Path,
    provider: &Path,
    evaluator: &Path,
) -> Daemon {
    let mut child = Daemon(
        ProcessCommand::new(DAEMON)
            .args([
                "--data-dir",
                data.to_str().unwrap(),
                "--source-repository",
                repository.to_str().unwrap(),
                "--evaluator-executable",
                evaluator.to_str().unwrap(),
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

#[test]
#[allow(clippy::too_many_lines)]
fn evaluator_mismatch_rejects_before_hosted_work_and_matching_restart_recovers() {
    let root = tempdir().unwrap();
    let home = root.path();
    let pack = home.join("pilot");
    json(&cli(
        home,
        &[
            "--json",
            "init",
            "--fixture",
            "support-triage",
            pack.to_str().unwrap(),
        ],
    ));
    let repository = pack.join("repository");
    let provider = home.join("claude-fixture");
    fs::write(&provider, "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{\"type\":\"result\",\"subtype\":\"success\",\"result\":\"{\\\"queue\\\":\\\"account_access\\\",\\\"priority\\\":\\\"p3\\\"}\",\"total_cost_usd\":0.001}'\n").unwrap();
    fs::set_permissions(&provider, fs::Permissions::from_mode(0o700)).unwrap();
    let data = pack.join("data");
    let daemon = start(home, &data, &repository, &provider);
    let prepared = json(&cli(
        home,
        &[
            "--data-dir",
            data.to_str().unwrap(),
            "--json",
            "pilot",
            "prepare",
            pack.to_str().unwrap(),
            "--provider",
            "claude",
            "--model",
            "offline-profile-model",
            "--cost-microusd",
            "250000",
        ],
    ));
    let parent = prepared["parent_genome_id"].as_str().unwrap().to_owned();
    let candidate = prepared["candidate_genome_id"].as_str().unwrap().to_owned();
    drop(daemon);

    let wrong = home.join("wrong-evaluator");
    let mut bytes = fs::read(EVALUATOR).unwrap();
    bytes.extend_from_slice(b"different installation fixture");
    fs::write(&wrong, bytes).unwrap();
    fs::set_permissions(&wrong, fs::Permissions::from_mode(0o700)).unwrap();
    let mismatched = start_with_evaluator(home, &data, &repository, &provider, &wrong);
    let client = Client::new(&data);
    assert!(client.request(Command::Unfreeze).unwrap().error.is_none());
    let Some(ResponseData::GenomeProfile { profile }) = client
        .request(Command::GenomeProfile {
            genome_id: candidate.clone(),
        })
        .unwrap()
        .data
    else {
        panic!("candidate profile");
    };
    let command = Command::EvaluatePairConfirmed {
        evaluation_id: "evaluator-recovery".to_owned(),
        parent_genome_id: parent.clone(),
        candidate_genome_id: candidate.clone(),
        expected_profile: profile,
    };
    let error = client.request(command.clone()).unwrap().error.unwrap();
    assert_eq!(error.code, ApiErrorCode::InvalidRequest);
    assert_eq!(error.rejected, Some(true));
    assert!(
        error
            .message
            .contains("does not match the World's pinned evaluator")
    );
    assert!(
        error.message.contains("restart") && error.message.contains("used to prepare this World")
    );
    assert!(!error.message.contains(wrong.to_str().unwrap()));
    assert!(
        !error
            .message
            .contains(prepared["world_id"].as_str().unwrap())
    );
    let plain = client
        .request(Command::EvaluatePair {
            evaluation_id: "plain-evaluator-rejection".to_owned(),
            parent_genome_id: parent,
            candidate_genome_id: candidate,
            remote: false,
        })
        .unwrap()
        .error
        .unwrap();
    assert_eq!(plain.code, ApiErrorCode::InvalidRequest);
    assert_eq!(plain.rejected, None);
    assert_eq!(plain.message, error.message);
    for id in ["evaluator-recovery", "plain-evaluator-rejection"] {
        assert_eq!(
            client
                .request(Command::JobStatus {
                    job_id: id.to_owned()
                })
                .unwrap()
                .error
                .unwrap()
                .code,
            ApiErrorCode::NotFound
        );
    }
    assert!(
        matches!(client.request(Command::RunList { limit: 100 }).unwrap().data, Some(ResponseData::RunList { runs }) if runs.is_empty())
    );
    assert!(
        matches!(client.request(Command::EvaluationList { limit: 100 }).unwrap().data, Some(ResponseData::EvaluationList { evaluations }) if evaluations.is_empty())
    );
    assert!(client.request(Command::Replay).unwrap().error.is_none());
    drop(mismatched);

    let matching = start(home, &data, &repository, &provider);
    let response = client.request(command.clone()).unwrap();
    assert!(response.error.is_none(), "{:?}", response.error);
    let Some(ResponseData::ArenaJob { job }) = response.data else {
        panic!("same ID should be admitted after matching restart");
    };
    assert_eq!(job.evaluation_id, "evaluator-recovery");
    let deadline = Instant::now() + Duration::from_secs(60);
    let finished = loop {
        let response = client
            .request(Command::JobStatus {
                job_id: job.evaluation_id.clone(),
            })
            .unwrap();
        assert!(response.error.is_none(), "{:?}", response.error);
        let Some(ResponseData::ArenaJob { job }) = response.data else {
            panic!("Arena progress");
        };
        assert!(
            !matches!(job.state, JobState::Failed | JobState::Interrupted),
            "{job:?}"
        );
        if job.state == JobState::Succeeded {
            let evaluation = job
                .evaluation
                .as_ref()
                .expect("successful comparison evidence");
            assert_eq!(evaluation.world_id, prepared["world_id"].as_str().unwrap());
            assert_eq!(
                evaluation.parent_genome_id,
                prepared["parent_genome_id"].as_str().unwrap()
            );
            assert_eq!(
                evaluation.candidate_genome_id,
                prepared["candidate_genome_id"].as_str().unwrap()
            );
            assert_eq!(job.completed_trials, 48);
            break job;
        }
        assert!(
            Instant::now() < deadline,
            "offline fixture comparison deadline"
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        matches!(client.request(Command::RunList { limit: 100 }).unwrap().data, Some(ResponseData::RunList { runs }) if runs.len() == 48)
    );
    assert!(client.request(Command::Replay).unwrap().error.is_none());
    drop(matching);
    let _restarted = start(home, &data, &repository, &provider);
    let response = client.request(command).unwrap();
    assert!(response.error.is_none());
    assert!(matches!(response.data, Some(ResponseData::ArenaJob { job }) if job == finished));
    assert!(
        matches!(client.request(Command::RunList { limit: 100 }).unwrap().data, Some(ResponseData::RunList { runs }) if runs.len() == 48)
    );
    assert!(client.request(Command::Replay).unwrap().error.is_none());
}
