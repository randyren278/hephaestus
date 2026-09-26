//! `senate`: ask the Senate a question from the command line.

use std::{
    env,
    fmt::Write as _,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::{Parser, Subcommand};
use hephaestus_senate::{
    BackendKind, CliBackend, DISCLAIMER, Senate, Size, default_personas, find_on_path,
    render_answer, render_transcript,
};

#[derive(Parser)]
#[command(
    name = "senate",
    about = "Put a question to a Senate of simulated perspectives and get one debated answer",
    version
)]
struct Arguments {
    #[command(subcommand)]
    command: SenateCommand,
}

#[derive(Subcommand)]
enum SenateCommand {
    /// Debate a question, idea, or brief and print the Senate's answer.
    Ask(AskArguments),
    /// List the personas the Senate can seat.
    Personas,
}

#[derive(clap::Args)]
struct AskArguments {
    /// The question or brief; `-` reads it from stdin.
    question: String,
    /// Senate size: S (3 senators, 2 rounds), M (5, 3), L (9, 4), XL (15, 5).
    #[arg(long, value_enum, ignore_case = true, default_value = "M")]
    size: Size,
    /// Model CLI to use [default: `SENATE_BACKEND`, else claude, else codex].
    #[arg(long, value_enum)]
    backend: Option<BackendKind>,
    /// Path to the backend executable [default: `SENATE_BACKEND_BIN`, else PATH].
    #[arg(long)]
    backend_bin: Option<PathBuf>,
    /// A file whose contents are appended to the question (repeatable).
    #[arg(long)]
    context: Vec<PathBuf>,
    /// Also write the answer to this file.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Where to save the full transcript [default: ~/.senate/transcripts/].
    #[arg(long)]
    transcript: Option<PathBuf>,
    /// Most model calls in flight at once.
    #[arg(long, default_value_t = 4)]
    jobs: usize,
    /// Seed for the fallback roster [default: derived from the question].
    #[arg(long)]
    seed: Option<u64>,
    /// Per-call timeout in seconds.
    #[arg(long, default_value_t = 600)]
    timeout: u64,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let result = match arguments.command {
        SenateCommand::Ask(ask) => run_ask(&ask),
        SenateCommand::Personas => {
            list_personas();
            Ok(())
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("senate: {error}");
            ExitCode::FAILURE
        }
    }
}

fn list_personas() {
    println!("{DISCLAIMER}\n");
    for persona in default_personas() {
        println!(
            "{:<16} {:<22} {:<11} {}",
            persona.id, persona.name, persona.domain, persona.lens
        );
    }
}

fn run_ask(ask: &AskArguments) -> Result<(), String> {
    let question = read_question(ask)?;
    let backend = resolve_backend(ask)?;
    let seed = ask.seed.unwrap_or_else(|| fnv1a(question.as_bytes()));
    let senate = Senate::new(&backend, default_personas(), ask.size, seed, ask.jobs);
    eprintln!(
        "senate: convening size {} ({} senators, up to {} rounds, at most {} model calls) via {}",
        ask.size.name(),
        ask.size.senators(),
        ask.size.round_cap(),
        ask.size.max_calls(),
        backend.program.display()
    );
    let outcome = senate.run(&question).map_err(|error| error.to_string())?;

    let transcript_path = match &ask.transcript {
        Some(path) => path.clone(),
        None => default_transcript_path(&question)?,
    };
    write_file(&transcript_path, &render_transcript(&outcome))?;
    let transcript_display = transcript_path.display().to_string();
    let answer = render_answer(&outcome, backend.kind.program(), Some(&transcript_display));
    if let Some(out) = &ask.out {
        write_file(out, &answer)?;
    }
    print!("{answer}");
    Ok(())
}

fn read_question(ask: &AskArguments) -> Result<String, String> {
    let mut question = if ask.question == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| format!("could not read the question from stdin: {error}"))?;
        text
    } else {
        ask.question.clone()
    };
    for path in &ask.context {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("could not read context {}: {error}", path.display()))?;
        let _ = write!(
            question,
            "\n\nCONTEXT FILE {}:\n{text}",
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned()
            )
        );
    }
    if question.trim().is_empty() {
        return Err("the question is empty".to_owned());
    }
    Ok(question)
}

fn resolve_backend(ask: &AskArguments) -> Result<CliBackend, String> {
    let configured = match (ask.backend, env::var("SENATE_BACKEND")) {
        (Some(kind), _) => Some(kind),
        (None, Ok(name)) if !name.is_empty() => Some(match name.to_ascii_lowercase().as_str() {
            "claude" => BackendKind::Claude,
            "codex" => BackendKind::Codex,
            other => {
                return Err(format!(
                    "SENATE_BACKEND={other} is not a backend; use claude or codex"
                ));
            }
        }),
        _ => None,
    };
    let explicit_bin = ask
        .backend_bin
        .clone()
        .or_else(|| env::var_os("SENATE_BACKEND_BIN").map(PathBuf::from));
    let (kind, program) = match (configured, explicit_bin) {
        (kind, Some(program)) => (kind.unwrap_or(BackendKind::Claude), program),
        (Some(kind), None) => {
            let program = find_on_path(kind.program()).ok_or_else(|| {
                format!(
                    "`{}` was not found on PATH; install it or pass --backend-bin",
                    kind.program()
                )
            })?;
            (kind, program)
        }
        (None, None) => [BackendKind::Claude, BackendKind::Codex]
            .into_iter()
            .find_map(|kind| find_on_path(kind.program()).map(|program| (kind, program)))
            .ok_or_else(|| {
                "neither `claude` (Claude Code) nor `codex` (Codex CLI) was found on PATH; install one and sign in, or pass --backend-bin".to_owned()
            })?,
    };
    Ok(CliBackend {
        kind,
        program,
        timeout: Duration::from_secs(ask.timeout.max(1)),
    })
}

fn default_transcript_path(question: &str) -> Result<PathBuf, String> {
    let home = env::var_os("SENATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| Path::new(&home).join(".senate")))
        .ok_or("HOME is not set; pass --transcript")?;
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let slug: String = question
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-")
        .to_ascii_lowercase();
    Ok(home
        .join("transcripts")
        .join(format!("{seconds}-{slug}.md")))
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    fs::write(path, contents)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// FNV-1a, so the default seed is stable across runs and platforms.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
