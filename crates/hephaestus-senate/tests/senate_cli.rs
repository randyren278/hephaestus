//! End-to-end contracts for the `senate` binary, driven by small fake
//! `claude`/`codex` scripts. Nothing here invokes a real model CLI or touches
//! the network.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

use tempfile::{TempDir, tempdir};

/// Shell preamble every fake shares: reads the prompt from stdin and pulls
/// out the task, senator, and round tags the Senate puts on each prompt.
const PREAMBLE: &str = r#"#!/bin/sh
input=$(cat)
task=$(printf '%s\n' "$input" | sed -n 's/^SENATE-TASK: //p' | head -n 1)
senator=$(printf '%s\n' "$input" | sed -n 's/^SENATOR: //p' | head -n 1)
round=$(printf '%s\n' "$input" | sed -n 's/^ROUND: //p' | head -n 1)
printf '%s %s %s\n' "$task" "$senator" "$round" >> "$(dirname "$0")/calls.log"
printf '%s\n' "$*" > "$(dirname "$0")/args.log"
"#;

struct Fixture {
    directory: TempDir,
    script: PathBuf,
}

impl Fixture {
    fn new(body: &str) -> Self {
        let directory = tempdir().expect("temporary directory");
        let script = directory.path().join("fake-backend");
        fs::write(&script, format!("{PREAMBLE}{body}")).expect("write fake backend");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod fake");
        Self { directory, script }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn senate(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_senate"))
            .args(arguments)
            .arg("--backend-bin")
            .arg(&self.script)
            .arg("--transcript")
            .arg(self.path("transcript.md"))
            .env_remove("SENATE_BACKEND")
            .env_remove("SENATE_BACKEND_BIN")
            .output()
            .expect("run senate")
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.path("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn transcript(&self) -> String {
        fs::read_to_string(self.path("transcript.md")).expect("transcript written")
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "senate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

const AGREEABLE: &str = r#"case "$task" in
  roster) printf 'turing\nlaozi\nconfucius\n' ;;
  opening) printf 'Opening from %s.\n' "$senator" ;;
  draft) printf 'ANSWER:\nDraft of round %s.\nAGREEMENT:\n- shared point\n' "$round" ;;
  deliberate) printf 'VOTE: AGREE\nREASON: sound\nPOSITION:\n%s stands by it.\n' "$senator" ;;
esac
"#;

#[test]
fn a_small_senate_reaches_consensus_and_reports_everything() {
    let fixture = Fixture::new(AGREEABLE);
    let out = fixture.path("answer.md");
    let output = fixture.senate(&[
        "ask",
        "Should we ship?",
        "--size",
        "s",
        "--out",
        out.to_str().unwrap(),
    ]);
    let printed = stdout(&output);

    assert!(printed.contains("Nothing here is any real person's words"));
    assert!(printed.contains("consensus, ratified in round 2 of 2"));
    assert!(printed.contains("## Answer\n\nDraft of round 1."));
    assert!(printed.contains("- shared point"));
    assert!(printed.contains("Alan Turing, Laozi, Confucius"));
    assert!(printed.contains("8 model calls"), "{printed}");
    assert!(!printed.contains("## Dissent"));
    assert_eq!(fs::read_to_string(&out).expect("out file"), printed);

    // roster, 3 openings, a draft, 3 votes: the ratified round needs no redraft.
    let calls = fixture.calls();
    assert_eq!(calls.len(), 8);
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("draft"))
            .count(),
        1
    );

    let transcript = fixture.transcript();
    assert!(transcript.contains("Nothing here is any real person's words"));
    assert!(transcript.contains("## Roster (chosen by the clerk)"));
    assert!(transcript.contains("### in the spirit of Laozi (vote: agree)"));
    assert!(transcript.contains("Opening from confucius."));
    assert!(transcript.contains("The previous draft was ratified."));
}

#[test]
fn claude_is_called_with_isolation_flags_and_never_bare() {
    let fixture = Fixture::new(AGREEABLE);
    stdout(&fixture.senate(&["ask", "q", "--size", "S", "--backend", "claude"]));
    let arguments = fs::read_to_string(fixture.path("args.log")).expect("args logged");
    for flag in [
        "-p",
        "--setting-sources",
        "--strict-mcp-config",
        "--tools",
        "--disable-slash-commands",
    ] {
        assert!(
            arguments.split_whitespace().any(|word| word == flag),
            "{flag} missing from {arguments}"
        );
    }
    assert!(!arguments.contains("--bare"));
}

#[test]
fn dissent_runs_to_the_round_cap_and_is_credited() {
    let fixture = Fixture::new(
        r#"case "$task" in
  roster) printf 'machiavelli gandhi hopper feynman ostrom\n' ;;
  opening) printf 'Opening from %s.\n' "$senator" ;;
  draft) printf 'ANSWER:\nDraft of round %s.\nAGREEMENT:\n- shared point\n' "$round" ;;
  deliberate)
    case "$senator" in
      machiavelli) printf 'VOTE: DISSENT\nREASON: it ignores who holds power\nPOSITION:\nNo.\n' ;;
      hopper) printf 'VOTE: AMEND\nREASON: ship a prototype first\nPOSITION:\nPrototype.\n' ;;
      *) printf 'VOTE: AGREE\nREASON: fine\nPOSITION:\nYes.\n' ;;
    esac ;;
esac
"#,
    );
    let printed = stdout(&fixture.senate(&["ask", "What now?", "--size", "M"]));
    assert!(printed.contains("no consensus after 3 of 3 rounds"));
    assert!(printed.contains("## Answer\n\nDraft of round 3."));
    assert!(printed.contains(
        "## Dissent\n\n- *in the spirit of Niccolo Machiavelli*: it ignores who holds power"
    ));
    assert!(printed.contains("- *in the spirit of Grace Hopper*: ship a prototype first"));
    // roster + 3 rounds of (5 senators + 1 draft) = 19, the M-size maximum.
    assert_eq!(fixture.calls().len(), 19);
    assert!(printed.contains("19 model calls"));
}

#[test]
fn amendments_in_the_ratifying_round_are_folded_into_the_answer() {
    let fixture = Fixture::new(
        r#"printf '%s\n' "$input" >> "$(dirname "$0")/prompts.log"
case "$task" in
  roster) printf 'turing\nlaozi\nconfucius\n' ;;
  opening) printf 'Opening from %s.\n' "$senator" ;;
  draft) printf 'ANSWER:\nShip on Friday.\nAGREEMENT:\n- shared point\n' ;;
  amend) printf 'ANSWER:\nShip on Friday, behind a feature flag.\n' ;;
  deliberate)
    case "$senator" in
      laozi) printf 'VOTE: AMEND\nREASON: put it behind a feature flag\nPOSITION:\nFlag it.\n' ;;
      *) printf 'VOTE: AGREE\nREASON: sound\nPOSITION:\nYes.\n' ;;
    esac ;;
esac
"#,
    );
    let printed = stdout(&fixture.senate(&["ask", "Should we ship?", "--size", "S"]));

    assert!(
        printed.contains("consensus, ratified in round 2 of 2"),
        "{printed}"
    );
    assert!(
        printed.contains("## Answer\n\nShip on Friday, behind a feature flag."),
        "{printed}"
    );
    // The clerk's fold reply listed no agreement, so the ratified draft's stands.
    assert!(printed.contains("- shared point"));
    assert!(printed.contains(
        "## Amendments folded into the answer\n\n- *in the spirit of Laozi*: put it behind a feature flag"
    ));
    assert!(!printed.contains("Amendments proposed in the last vote"));

    // The fold uses the ratifying round's unused clerk call: roster, 3
    // openings, a draft, 3 votes, one amend = 9, within S-size's maximum of 9.
    let calls = fixture.calls();
    assert_eq!(calls.len(), 9, "{calls:?}");
    assert_eq!(
        calls
            .iter()
            .filter(|call| call.starts_with("amend"))
            .count(),
        1
    );
    assert!(printed.contains("9 model calls"));

    let prompts = fs::read_to_string(fixture.path("prompts.log")).expect("prompts logged");
    assert!(prompts.contains("RATIFIED DRAFT:\nShip on Friday.\n"));
    assert!(prompts.contains("- in the spirit of Laozi: put it behind a feature flag"));

    let transcript = fixture.transcript();
    assert!(transcript.contains("The previous draft was ratified."));
    assert!(transcript.contains(
        "### Clerk's draft with amendments folded in\n\nShip on Friday, behind a feature flag."
    ));
}

#[test]
fn a_failed_amendment_fold_keeps_the_ratified_draft_and_credits_the_amendment() {
    let fixture = Fixture::new(
        r#"if [ "$task" = amend ]; then echo 'rate limited' >&2; exit 3; fi
case "$task" in
  roster) printf 'turing\nlaozi\nconfucius\n' ;;
  opening) printf 'Opening from %s.\n' "$senator" ;;
  draft) printf 'ANSWER:\nShip on Friday.\n' ;;
  deliberate)
    case "$senator" in
      laozi) printf 'VOTE: AMEND\nREASON: put it behind a feature flag\nPOSITION:\nFlag it.\n' ;;
      *) printf 'VOTE: AGREE\nREASON: sound\nPOSITION:\nYes.\n' ;;
    esac ;;
esac
"#,
    );
    let printed = stdout(&fixture.senate(&["ask", "Should we ship?", "--size", "S"]));
    assert!(
        printed.contains("## Answer\n\nShip on Friday.\n"),
        "{printed}"
    );
    assert!(printed.contains(
        "## Amendments proposed in the last vote\n\n- *in the spirit of Laozi*: put it behind a feature flag"
    ));
    assert!(!printed.contains("Amendments folded into the answer"));
    assert!(!fixture.transcript().contains("amendments folded in"));
}

#[test]
fn a_failed_senator_is_recorded_and_the_debate_continues() {
    let fixture = Fixture::new(
        r#"if [ "$task" = opening ] && [ "$senator" = laozi ]; then echo 'rate limited' >&2; exit 3; fi
case "$task" in
  roster) printf 'turing\nlaozi\nconfucius\n' ;;
  opening) printf 'Opening from %s.\n' "$senator" ;;
  draft) printf 'ANSWER:\nDraft of round %s.\n' "$round" ;;
  deliberate) printf 'VOTE: AGREE\nREASON: ok\nPOSITION:\nok\n' ;;
esac
"#,
    );
    let printed = stdout(&fixture.senate(&["ask", "q", "--size", "S"]));
    assert!(printed.contains("ratified in round 2"));
    let transcript = fixture.transcript();
    assert!(transcript.contains("*No reply this round:"), "{transcript}");
    assert!(transcript.contains("rate limited"));
}

#[test]
fn an_unusable_roster_reply_falls_back_to_a_reproducible_seeded_roster() {
    let body = r#"case "$task" in
  roster) printf 'I cannot choose.\n' ;;
  opening) printf 'Opening from %s.\n' "$senator" ;;
  draft) printf 'ANSWER:\nSynthesis.\n' ;;
  deliberate) printf 'VOTE: AGREE\nREASON: ok\nPOSITION:\nok\n' ;;
esac
"#;
    let first = Fixture::new(body);
    let second = Fixture::new(body);
    stdout(&first.senate(&["ask", "same question", "--size", "L", "--jobs", "3"]));
    stdout(&second.senate(&["ask", "same question", "--size", "L", "--jobs", "1"]));
    let transcript = first.transcript();
    assert!(transcript.contains("## Roster (completed from the seeded roster)"));
    assert_eq!(
        transcript,
        second.transcript(),
        "same replies and seed give the same debate, whatever the concurrency"
    );
    let seated = transcript
        .lines()
        .filter(|line| line.starts_with("- **in the spirit of"))
        .count();
    assert_eq!(seated, 9);

    let reseeded = Fixture::new(body);
    stdout(&reseeded.senate(&["ask", "same question", "--size", "L", "--seed", "99"]));
    assert_ne!(transcript, reseeded.transcript());
}

#[test]
fn codex_replies_are_read_from_the_last_message_file() {
    let fixture = Fixture::new(
        r#"last=''
previous=''
for argument in "$@"; do
  if [ "$previous" = --output-last-message ]; then last=$argument; fi
  previous=$argument
done
echo 'codex banner noise on stdout'
case "$task" in
  roster) printf 'hume\nrumi\nshannon\n' > "$last" ;;
  opening) printf 'Opening from %s.\n' "$senator" > "$last" ;;
  draft) printf 'ANSWER:\nCodex synthesis.\n' > "$last" ;;
  deliberate) printf 'VOTE: AGREE\nREASON: ok\nPOSITION:\nok\n' > "$last" ;;
esac
"#,
    );
    let printed = stdout(&fixture.senate(&["ask", "q", "--size", "S", "--backend", "codex"]));
    assert!(printed.contains("## Answer\n\nCodex synthesis."));
    assert!(printed.contains("David Hume, Rumi, Claude Shannon"));
    assert!(printed.contains("Backend `codex`"));
    let arguments = fs::read_to_string(fixture.path("args.log")).expect("args logged");
    assert!(arguments.starts_with("exec "));
    assert!(arguments.contains("--ignore-user-config"));
}

#[test]
fn a_hung_call_times_out_and_every_failed_opening_is_an_error() {
    let fixture = Fixture::new("exec /bin/sleep 30\n");
    let output = fixture.senate(&["ask", "q", "--size", "S", "--timeout", "1"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("every senator's opening call failed"),
        "{stderr}"
    );
    assert!(stderr.contains("timed out after 1s"), "{stderr}");
}

#[test]
fn no_installed_backend_is_a_clear_error() {
    let empty = tempdir().expect("empty PATH directory");
    let output = Command::new(env!("CARGO_BIN_EXE_senate"))
        .args(["ask", "q"])
        .env("PATH", empty.path())
        .env_remove("SENATE_BACKEND")
        .env_remove("SENATE_BACKEND_BIN")
        .output()
        .expect("run senate");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("neither `claude` (Claude Code) nor `codex` (Codex CLI) was found on PATH"),
        "{stderr}"
    );
}

#[test]
fn the_backend_can_be_chosen_by_config_and_found_on_path() {
    let fixture = Fixture::new(AGREEABLE);
    let bin = fixture.path("bin");
    fs::create_dir(&bin).expect("bin directory");
    fs::copy(&fixture.script, bin.join("codex")).expect("install fake codex");
    fs::set_permissions(bin.join("codex"), fs::Permissions::from_mode(0o755)).expect("chmod");
    let output = Command::new(env!("CARGO_BIN_EXE_senate"))
        .args(["ask", "q", "--size", "S", "--transcript"])
        .arg(fixture.path("transcript.md"))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("SENATE_BACKEND", "codex")
        .env_remove("SENATE_BACKEND_BIN")
        .output()
        .expect("run senate");
    let printed = stdout(&output);
    assert!(printed.contains("Backend `codex`"));
    let logged = fs::read_to_string(bin.join("args.log")).expect("fake codex ran");
    assert!(logged.starts_with("exec "));
}

#[test]
fn context_files_are_appended_to_the_question() {
    let fixture = Fixture::new(
        r#"printf '%s\n' "$input" >> "$(dirname "$0")/prompts.log"
case "$task" in
  roster) printf 'turing laozi confucius\n' ;;
  draft) printf 'ANSWER:\nok\n' ;;
  *) printf 'VOTE: AGREE\nREASON: ok\nPOSITION:\nok\n' ;;
esac
"#,
    );
    let brief = fixture.path("architecture.md");
    fs::write(&brief, "The system has three services.").expect("write brief");
    stdout(&fixture.senate(&[
        "ask",
        "Improve this document",
        "--size",
        "S",
        "--context",
        brief.to_str().unwrap(),
    ]));
    let prompts = fs::read_to_string(fixture.path("prompts.log")).expect("prompts logged");
    assert!(prompts.contains("CONTEXT FILE architecture.md:\nThe system has three services."));
}

#[test]
fn personas_lists_the_roster_with_the_disclaimer() {
    let output = Command::new(env!("CARGO_BIN_EXE_senate"))
        .arg("personas")
        .output()
        .expect("run senate personas");
    let printed = stdout(&output);
    assert!(printed.starts_with("Simulated perspectives"));
    assert!(printed.contains("socrates"));
    assert!(printed.lines().count() > 16);
}
