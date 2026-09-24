use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
};

use clap::{Parser, Subcommand};
use hephaestus_control::{
    API_VERSION, ApiResponse, ArenaJobProgress, ChampionRecord, ChampionTransitionRecord, Client,
    Command, EvaluationRecord, ForgeAssessmentOutcome, ForgeAssessmentRecord, ForgeProposalRecord,
    GenomeRecord, InvariantRecord, JobState, ResponseData, SelectionRecord, WorldRecord,
    data_dir_from_environment,
};

#[derive(Parser)]
#[command(name = "hephaestus", about = "Hephaestus operator CLI", version)]
struct Arguments {
    /// Canonical daemon data directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Emit the stable API response as JSON.
    #[arg(long)]
    json: bool,
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Show daemon state.
    Status,
    /// Stop new evolution work.
    Freeze,
    /// Resume evolution through the operator boundary.
    Unfreeze,
    /// Terminate all active work.
    Kill {
        /// Confirm that every active run is targeted.
        #[arg(long)]
        all: bool,
    },
    /// Register and inspect immutable Genomes.
    Genome {
        #[command(subcommand)]
        command: GenomeCommand,
    },
    /// Register and inspect immutable Worlds.
    World {
        #[command(subcommand)]
        command: WorldCommand,
    },
    /// Store files in the content-addressed artifact store.
    Artifact {
        #[command(subcommand)]
        command: ArtifactCommand,
    },
    /// Publish the daemon's runtime-result verifier key as an artifact.
    Verifier,
    /// Execute one registered Genome with the offline reference runtime.
    Run {
        /// Content-derived registered Genome identity.
        genome_id: String,
    },
    /// Submit one bounded direct reference run to the daemon writer.
    Submit { job_id: String, genome_id: String },
    /// Inspect or cancel a submitted direct run.
    Job {
        #[command(subcommand)]
        command: JobCommand,
    },
    /// Execute one registered Genome for a World-bound evaluation task.
    Evaluate {
        genome_id: String,
        #[arg(long)]
        task_id: String,
        #[arg(long)]
        input: String,
        #[arg(long, default_value_t = 0)]
        seed: u64,
        #[arg(long, default_value_t = 10_000)]
        wall_millis: u64,
        #[arg(long, default_value_t = 1_048_576)]
        maximum_output_bytes: u64,
        #[arg(long, default_value_t = 0)]
        maximum_cost_microusd: u64,
    },
    /// Seed, promote, roll back, and inspect the Champion of a World.
    Champion {
        #[command(subcommand)]
        command: ChampionCommand,
    },
    /// Run trusted paired evaluations.
    Arena {
        #[command(subcommand)]
        command: ArenaCommand,
    },
    /// Verify and replay the canonical event stream.
    Replay,
    /// Control the local daemon process.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Initialize a local example fixture.
    Init {
        /// Fixture to copy.
        #[arg(long, default_value = "quickstart")]
        fixture: String,
        /// New destination directory (must not already exist).
        path: PathBuf,
    },
    /// Open the local interactive terminal operator interface.
    Tui,
}

#[derive(Subcommand)]
enum GenomeCommand {
    /// Show one canonical Genome record.
    Show {
        /// Content-derived Genome identity.
        genome_id: String,
    },
    /// Print the verified prompt body of a registered Markdown Genome.
    Prompt {
        /// Content-derived Genome identity.
        genome_id: String,
    },
    /// List every registered Genome.
    List,
    /// Compile a JSON, YAML, or Markdown Genome source under a registered World and register it.
    Register {
        /// Genome source file.
        path: PathBuf,
        /// Registered World the Genome is compiled against.
        #[arg(long)]
        world: String,
    },
    /// Propose one prompt operation mutation from an exact trusted Arena selection.
    Propose {
        /// Stable idempotency key for this proposal.
        proposal_id: String,
        /// Canonical selection event ID returned by `arena select`.
        #[arg(long)]
        selection_event: String,
        /// Selected candidate Genome to use as the child parent.
        #[arg(long)]
        parent: String,
        /// Operator-authored hypothesis for the prompt mutation.
        #[arg(long)]
        hypothesis: String,
    },
    /// Record measured evidence for a proposed child; never promotes it.
    Assess {
        /// Stable idempotency key for this assessment.
        assessment_id: String,
        /// Stable proposal identity.
        #[arg(long)]
        proposal: String,
        /// Selection event from a new Arena evaluation of the proposed child.
        #[arg(long)]
        selection_event: String,
    },
}

#[derive(Subcommand)]
enum ChampionCommand {
    /// Bootstrap the first Champion of a World by explicit operator authority.
    Seed {
        /// Stable idempotency key for this transition.
        transition_id: String,
        /// Registered World identity.
        #[arg(long)]
        world: String,
        /// Registered Genome compiled under that World.
        #[arg(long)]
        genome: String,
        /// Operator-authored reason for the bootstrap.
        #[arg(long)]
        reason: String,
    },
    /// Promote an assessed child whose metrics and invariant evidence pass.
    Promote {
        /// Stable idempotency key for this transition.
        transition_id: String,
        /// Forge assessment of the child against the current Champion.
        #[arg(long)]
        assessment: String,
    },
    /// Restore the previous Champion and quarantine the current one.
    Rollback {
        /// Stable idempotency key for this transition.
        transition_id: String,
        /// Registered World identity.
        #[arg(long)]
        world: String,
        /// Operator-authored reason for the rollback.
        #[arg(long)]
        reason: String,
    },
    /// Show the current Champion and transition history of a World.
    Show {
        /// Registered World identity.
        world_id: String,
    },
}

#[derive(Subcommand)]
enum WorldCommand {
    /// Show one canonical World record.
    Show {
        /// Content-derived World identity.
        world_id: String,
    },
    /// List every registered World.
    List,
    /// Compile a JSON or YAML World source and register it.
    Register {
        /// World source file.
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum ArtifactCommand {
    /// Store one file and print its BLAKE3 artifact address.
    Put {
        /// File to store.
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum ArenaCommand {
    /// Canonicalize a task manifest JSON file and store it as an artifact.
    Manifest {
        /// Manifest source file.
        path: PathBuf,
    },
    /// Compare a parent and candidate using daemon-owned World tasks and budgets.
    Evaluate {
        /// Stable caller-selected evaluation identity.
        evaluation_id: String,
        /// Content-derived registered parent Genome identity.
        parent_genome_id: String,
        /// Content-derived registered candidate Genome identity.
        candidate_genome_id: String,
    },
    /// Calculate and persist the trusted metrics outcome for one evaluation.
    Select {
        /// Stable Arena evaluation identity.
        evaluation_id: String,
    },
    /// Verify and persist aggregate reference-output invariant evidence.
    Invariants {
        /// Stable Arena evaluation identity.
        evaluation_id: String,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Request an audited graceful daemon stop.
    Stop,
}

#[derive(Subcommand)]
enum JobCommand {
    /// Show the durable state of one job.
    Status { job_id: String },
    /// Request cancellation and return before terminal confirmation.
    Kill { job_id: String },
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    if let Some(exit) = handle_local_command(&arguments) {
        return exit;
    }
    let data_dir = match arguments
        .data_dir
        .map_or_else(data_dir_from_environment, Ok)
    {
        Ok(path) => path,
        Err(error) => {
            eprintln!("hephaestus: {error}");
            return ExitCode::FAILURE;
        }
    };
    let evaluation_id = match &arguments.command {
        CliCommand::Arena {
            command: ArenaCommand::Evaluate { evaluation_id, .. },
        } => Some(evaluation_id.clone()),
        _ => None,
    };
    let command = match command_from_cli(arguments.command) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("hephaestus: {message}");
            return ExitCode::FAILURE;
        }
    };
    let client = Client::new(data_dir);
    let mut response = match client.request(command) {
        Ok(response) => response,
        Err(error) => {
            eprintln!("hephaestus: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(evaluation_id) = evaluation_id
        && let Some(ResponseData::ArenaJob { job }) = response.data.as_ref()
        && matches!(
            job.state,
            JobState::Admitted | JobState::Running | JobState::CancellationRequested
        )
    {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            match client.request(Command::JobStatus {
                job_id: evaluation_id.clone(),
            }) {
                Ok(next) => {
                    if let Some(ResponseData::ArenaJob { job }) = next.data.clone()
                        && !matches!(
                            job.state,
                            JobState::Admitted
                                | JobState::Running
                                | JobState::CancellationRequested
                        )
                    {
                        response = arena_final_response(next, job);
                        break;
                    }
                    if next.error.is_some() {
                        response = next;
                        break;
                    }
                }
                Err(error) => {
                    eprintln!("hephaestus: {error}");
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    if let Some(ResponseData::ArenaJob { job }) = response.data.clone()
        && !matches!(
            job.state,
            JobState::Admitted | JobState::Running | JobState::CancellationRequested
        )
    {
        response = arena_final_response(response, job);
    }
    if arguments.json {
        println!(
            "{}",
            serde_json::to_string(&response).expect("API response serialization cannot fail")
        );
    } else {
        print_human(&response);
    }
    if response.error.is_some()
        || matches!(response.data, Some(ResponseData::ArenaJob { job }) if job.state != JobState::Succeeded)
    {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn handle_local_command(arguments: &Arguments) -> Option<ExitCode> {
    match &arguments.command {
        CliCommand::Tui if arguments.json => {
            eprintln!("hephaestus: --json does not apply to the interactive TUI");
            Some(ExitCode::FAILURE)
        }
        CliCommand::Tui => Some(launch_tui(arguments.data_dir.clone())),
        CliCommand::Init { fixture, path } => {
            Some(initialize_fixture(fixture, path, arguments.json))
        }
        _ => None,
    }
}

fn launch_tui(data_dir: Option<PathBuf>) -> ExitCode {
    let mut command = if let Some((node, entrypoint)) = packaged_tui_paths(&current_executable()) {
        let mut command = ProcessCommand::new(node);
        command.arg(entrypoint);
        command
    } else {
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/hephaestus-tui")
            .canonicalize();
        let package = match package {
            Ok(path) if path.join("package.json").is_file() => path,
            _ => {
                eprintln!("hephaestus: packaged or source-checkout TUI assets are unavailable");
                return ExitCode::FAILURE;
            }
        };
        let mut command = ProcessCommand::new("npm");
        command.arg("--prefix").arg(package).args(["run", "start"]);
        command
    };
    if let Some(data_dir) =
        data_dir.or_else(|| std::env::var_os("HEPHAESTUS_HOME").map(PathBuf::from))
    {
        let absolute_data_dir = if data_dir.is_absolute() {
            data_dir
        } else {
            let Ok(cwd) = std::env::current_dir() else {
                eprintln!("hephaestus: could not resolve the TUI data directory");
                return ExitCode::FAILURE;
            };
            cwd.join(data_dir)
        };
        command.env("HEPHAESTUS_HOME", absolute_data_dir);
    }
    let Ok(status) = command.status() else {
        eprintln!("hephaestus: could not start the TUI runtime or source-checkout package");
        return ExitCode::FAILURE;
    };
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from)
}

fn current_executable() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .unwrap_or_default()
}

fn packaged_share_dir(executable: &Path) -> Option<PathBuf> {
    let canonical = executable.canonicalize().ok()?;
    let package_root = canonical.parent()?.parent()?;
    let share = package_root.join("share/hephaestus");
    share.join("package.json").is_file().then_some(share)
}

fn packaged_tui_paths(executable: &Path) -> Option<(PathBuf, PathBuf)> {
    let canonical = executable.canonicalize().ok()?;
    let bin_dir = canonical.parent()?;
    let share = packaged_share_dir(&canonical)?;
    let node = bin_dir.join("node");
    let entrypoint = share.join("tui/main.mjs");
    (node.is_file() && entrypoint.is_file()).then_some((node, entrypoint))
}

fn fixture_source_dir() -> Result<PathBuf, String> {
    if let Some(share) = packaged_share_dir(&current_executable()) {
        let fixtures = share.join("fixtures/quickstart");
        if fixtures.is_dir() {
            return Ok(fixtures);
        }
        return Err("installed quickstart fixture is unavailable".to_owned());
    }
    let source_checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/quickstart");
    source_checkout
        .is_dir()
        .then_some(source_checkout)
        .ok_or_else(|| "quickstart fixture is unavailable".to_owned())
}

fn initialize_fixture(fixture: &str, destination: &Path, json: bool) -> ExitCode {
    if fixture != "quickstart" {
        eprintln!("hephaestus: unsupported fixture: {fixture}");
        return ExitCode::FAILURE;
    }
    let source = match fixture_source_dir() {
        Ok(source) => source,
        Err(error) => {
            eprintln!("hephaestus: {error}");
            return ExitCode::FAILURE;
        }
    };
    let destination = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(destination),
            Err(error) => {
                eprintln!("hephaestus: could not resolve fixture destination: {error}");
                return ExitCode::FAILURE;
            }
        }
    };
    if destination.exists() {
        eprintln!("hephaestus: fixture destination already exists");
        return ExitCode::FAILURE;
    }
    if let Err(error) = copy_fixture_into_new_destination(&source, &destination) {
        eprintln!("hephaestus: could not initialize fixture: {error}");
        return ExitCode::FAILURE;
    }
    if json {
        let output = serde_json::json!({
            "fixture": "quickstart",
            "path": destination.to_string_lossy(),
            "repository": destination.join("repository").to_string_lossy(),
        });
        println!("{output}");
    } else {
        println!("fixture=quickstart path={}", destination.display());
        println!(
            "source_repository={}",
            destination.join("repository").display()
        );
    }
    ExitCode::SUCCESS
}

fn copy_fixture_into_new_destination(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir(destination).map_err(|error| error.to_string())?;
    let result = copy_fixture_entries(source, destination)
        .and_then(|()| initialize_fixture_repository(&destination.join("repository")));
    if let Err(error) = result {
        let _ignored = fs::remove_dir_all(destination);
        return Err(error);
    }
    Ok(())
}

fn copy_fixture_entries(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            fs::create_dir(&target).map_err(|error| error.to_string())?;
            copy_fixture_entries(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), target).map_err(|error| error.to_string())?;
        } else {
            return Err("fixture contains a non-regular entry".to_owned());
        }
    }
    Ok(())
}

fn initialize_fixture_repository(repository: &Path) -> Result<(), String> {
    fs::create_dir(repository).map_err(|error| error.to_string())?;
    fs::write(
        repository.join("README.md"),
        "# Hephaestus quickstart workspace\n\nThis repository is the offline reference-run target.\n",
    )
    .map_err(|error| error.to_string())?;
    run_git(repository, ["init", "-q"])?;
    run_git(repository, ["add", "README.md"])?;
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(["-c", "user.name=Hephaestus Fixture"])
        .args([
            "-c",
            "user.email=fixture@localhost",
            "commit",
            "-m",
            "Initialize quickstart fixture",
        ])
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err("git could not create the fixture commit".to_owned())
    }
}

fn run_git<const N: usize>(repository: &Path, arguments: [&str; N]) -> Result<(), String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err("git could not initialize the fixture repository".to_owned())
    }
}

fn arena_final_response(response: ApiResponse, job: ArenaJobProgress) -> ApiResponse {
    let data = if let Some(evaluation) = job.evaluation.clone() {
        ResponseData::Evaluation { evaluation }
    } else {
        ResponseData::ArenaJob { job }
    };
    ApiResponse {
        version: API_VERSION,
        request_id: response.request_id,
        data: Some(data),
        error: response.error,
    }
}

fn absolute_path(path: PathBuf) -> Result<String, &'static str> {
    let absolute =
        std::fs::canonicalize(path).map_err(|_| "file does not exist or is not readable")?;
    absolute
        .into_os_string()
        .into_string()
        .map_err(|_| "path is not valid UTF-8")
}

fn command_from_cli(command: CliCommand) -> Result<Command, &'static str> {
    Ok(match command {
        CliCommand::Status => Command::Status,
        CliCommand::Freeze => Command::Freeze,
        CliCommand::Unfreeze => Command::Unfreeze,
        CliCommand::Kill { all: true } => Command::KillAll,
        CliCommand::Kill { all: false } => return Err("kill requires --all"),
        CliCommand::Genome { command } => genome_command_from_cli(command)?,
        CliCommand::World {
            command: WorldCommand::Show { world_id },
        } => Command::WorldShow { world_id },
        CliCommand::World {
            command: WorldCommand::List,
        } => Command::WorldList,
        CliCommand::World {
            command: WorldCommand::Register { path },
        } => Command::WorldRegister {
            path: absolute_path(path)?,
        },
        CliCommand::Artifact {
            command: ArtifactCommand::Put { path },
        } => Command::ArtifactPut {
            path: absolute_path(path)?,
        },
        CliCommand::Verifier => Command::VerifierShow,
        CliCommand::Run { genome_id } => Command::RunReference { genome_id },
        CliCommand::Submit { job_id, genome_id } => Command::RunSubmit { job_id, genome_id },
        CliCommand::Job {
            command: JobCommand::Status { job_id },
        } => Command::JobStatus { job_id },
        CliCommand::Job {
            command: JobCommand::Kill { job_id },
        } => Command::JobKill { job_id },
        CliCommand::Evaluate {
            genome_id,
            task_id,
            input,
            seed,
            wall_millis,
            maximum_output_bytes,
            maximum_cost_microusd,
        } => Command::RunEvaluation {
            genome_id,
            task_id,
            input,
            seed,
            wall_millis,
            maximum_output_bytes,
            maximum_cost_microusd,
        },
        CliCommand::Arena {
            command: ArenaCommand::Manifest { path },
        } => Command::ManifestPut {
            path: absolute_path(path)?,
        },
        CliCommand::Arena {
            command:
                ArenaCommand::Evaluate {
                    evaluation_id,
                    parent_genome_id,
                    candidate_genome_id,
                },
        } => Command::EvaluatePair {
            evaluation_id,
            parent_genome_id,
            candidate_genome_id,
        },
        CliCommand::Arena {
            command: ArenaCommand::Select { evaluation_id },
        } => Command::ArenaSelect { evaluation_id },
        CliCommand::Arena {
            command: ArenaCommand::Invariants { evaluation_id },
        } => Command::ArenaInvariants { evaluation_id },
        CliCommand::Champion { command } => champion_command_from_cli(command),
        CliCommand::Replay => Command::Replay,
        CliCommand::Daemon {
            command: DaemonCommand::Stop,
        } => Command::DaemonStop,
        CliCommand::Init { .. } => return Err("init is a local command"),
        CliCommand::Tui => return Err("tui is a local interactive command"),
    })
}

fn champion_command_from_cli(command: ChampionCommand) -> Command {
    match command {
        ChampionCommand::Seed {
            transition_id,
            world,
            genome,
            reason,
        } => Command::ChampionSeed {
            transition_id,
            world_id: world,
            genome_id: genome,
            reason,
        },
        ChampionCommand::Promote {
            transition_id,
            assessment,
        } => Command::ChampionPromote {
            transition_id,
            assessment_id: assessment,
        },
        ChampionCommand::Rollback {
            transition_id,
            world,
            reason,
        } => Command::ChampionRollback {
            transition_id,
            world_id: world,
            reason,
        },
        ChampionCommand::Show { world_id } => Command::ChampionShow { world_id },
    }
}

fn genome_command_from_cli(command: GenomeCommand) -> Result<Command, &'static str> {
    Ok(match command {
        GenomeCommand::Show { genome_id } => Command::GenomeShow { genome_id },
        GenomeCommand::Prompt { genome_id } => Command::GenomePrompt { genome_id },
        GenomeCommand::List => Command::GenomeList,
        GenomeCommand::Register { path, world } => Command::GenomeRegister {
            path: absolute_path(path)?,
            world_id: world,
        },
        GenomeCommand::Propose {
            proposal_id,
            selection_event,
            parent,
            hypothesis,
        } => Command::GenomePropose {
            proposal_id,
            selection_event_id: selection_event,
            parent_genome_id: parent,
            hypothesis,
        },
        GenomeCommand::Assess {
            assessment_id,
            proposal,
            selection_event,
        } => Command::GenomeAssess {
            assessment_id,
            proposal_id: proposal,
            selection_event_id: selection_event,
        },
    })
}

#[allow(clippy::too_many_lines)]
fn print_human(response: &ApiResponse) {
    match (&response.data, &response.error) {
        (
            Some(ResponseData::Status {
                frozen,
                active_runs,
                event_count,
                genome_count,
            }),
            None,
        ) => println!(
            "frozen={frozen} active_runs={active_runs} events={event_count} genomes={genome_count}"
        ),
        (
            Some(ResponseData::Acknowledged {
                frozen,
                killed_runs,
            }),
            None,
        ) => println!("acknowledged frozen={frozen} killed_runs={killed_runs}"),
        (Some(ResponseData::Genome { genome }), None) => println!("{}", genome_human(genome)),
        (Some(ResponseData::ForgeProposal { proposal }), None) => {
            println!("{}", forge_proposal_human(proposal));
        }
        (Some(ResponseData::ForgeAssessment { assessment }), None) => {
            println!("{}", forge_assessment_human(assessment));
        }
        (Some(ResponseData::GenomePrompt { prompt, .. }), None) => print!("{prompt}"),
        (Some(ResponseData::Job { job, progress }), None) => println!(
            "job={} genome={} run={} state={:?} terminal={:?} traces={} last_phase={} last_sequence={}",
            job.job_id,
            job.genome_id,
            job.run_id,
            job.state,
            job.terminal,
            progress.trace_events,
            progress.last_phase.as_deref().unwrap_or("none"),
            progress
                .last_event_sequence
                .map_or_else(|| "none".to_owned(), |value| value.to_string())
        ),
        (Some(ResponseData::Genomes { genomes }), None) => {
            for genome in genomes {
                println!("{}", genome_human(genome));
            }
        }
        (Some(ResponseData::World { world }), None) => println!("{}", world_human(world)),
        (Some(ResponseData::Worlds { worlds }), None) => {
            for world in worlds {
                println!("{}", world_human(world));
            }
        }
        (Some(ResponseData::Artifact { artifact_id, bytes }), None) => {
            println!("{artifact_id} bytes={bytes}");
        }
        (
            Some(ResponseData::Verifier {
                artifact_id,
                public_key_hex,
            }),
            None,
        ) => println!("{artifact_id} public_key={public_key_hex}"),
        (
            Some(ResponseData::Run {
                run_id,
                genome_id,
                world_id,
                source_revision,
                completion_reason,
                latency_millis,
                actual_cost_microusd,
                stdout_artifact_id,
                stderr_artifact_id,
                trace_artifact_ids,
            }),
            None,
        ) => println!(
            "run={run_id} genome={genome_id} world={world_id} revision={source_revision} reason={completion_reason:?} latency_ms={latency_millis} cost_microusd={actual_cost_microusd} stdout={stdout_artifact_id} stderr={stderr_artifact_id} trace_artifacts={}",
            trace_artifact_ids.len()
        ),
        (Some(ResponseData::Evaluation { evaluation }), None) => {
            println!("{}", evaluation_human(evaluation));
        }
        (Some(ResponseData::Selection { selection }), None) => {
            println!("{}", selection_human(selection));
        }
        (Some(ResponseData::ArenaInvariants { invariants }), None) => {
            println!("{}", invariants_human(invariants));
        }
        (Some(ResponseData::ArenaJob { job }), None) => {
            println!("{}", arena_job_human(job));
        }
        (Some(ResponseData::ChampionTransition { transition }), None) => {
            println!("{}", champion_transition_human(transition));
        }
        (Some(ResponseData::Champion { champion }), None) => {
            println!("{}", champion_human(champion));
        }
        (
            Some(ResponseData::Replay {
                event_count,
                frozen,
                active_runs,
                projection_hash,
            }),
            None,
        ) => println!(
            "replayed events={event_count} frozen={frozen} active_runs={active_runs} projection={projection_hash}"
        ),
        (_, Some(error)) => eprintln!("{:?}: {}", error.code, error.message),
        _ => eprintln!("invalid daemon response"),
    }
}

fn champion_transition_human(transition: &ChampionTransitionRecord) -> String {
    format!(
        "transition={} world={} kind={:?} champion={} previous={} event={} sequence={} hash={}",
        transition.payload.transition_id,
        transition.payload.world_id,
        transition.payload.kind,
        transition.payload.champion_genome_id,
        transition
            .payload
            .previous_champion_genome_id
            .as_deref()
            .unwrap_or("none"),
        transition.event.event_id,
        transition.event.sequence,
        transition.event.event_hash,
    )
}

fn champion_human(champion: &ChampionRecord) -> String {
    let mut lines = vec![format!(
        "world={} champion={} standby={} quarantined={} transitions={}",
        champion.world_id,
        champion.champion_genome_id.as_deref().unwrap_or("none"),
        champion.standby_genome_ids.len(),
        champion.quarantined_genome_ids.len(),
        champion.transitions.len(),
    )];
    lines.extend(champion.transitions.iter().map(champion_transition_human));
    lines.join("\n")
}

fn arena_job_human(job: &ArenaJobProgress) -> String {
    format!(
        "evaluation={} state={:?} phase={:?} trials={}/{}",
        job.evaluation_id, job.state, job.phase, job.completed_trials, job.total_trials
    )
}

fn genome_human(genome: &GenomeRecord) -> String {
    format!(
        "{} {} world={} artifact={} parents={}",
        genome.genome_id,
        genome.name,
        genome.world_id,
        genome.artifact_id,
        genome.parent_ids.join(",")
    )
}

fn world_human(world: &WorldRecord) -> String {
    format!(
        "{} {} artifact={}",
        world.world_id, world.name, world.artifact_id
    )
}

fn evaluation_human(evaluation: &EvaluationRecord) -> String {
    format!(
        "evaluation={} parent={} candidate={} world={} candidate_visible={}/{} parent_visible={}/{} event={} sequence={} aggregate={}",
        evaluation.evaluation_id,
        evaluation.parent_genome_id,
        evaluation.candidate_genome_id,
        evaluation.world_id,
        evaluation.candidate_visible_correct,
        evaluation.visible_total,
        evaluation.parent_visible_correct,
        evaluation.visible_total,
        evaluation.event.event_id,
        evaluation.event.sequence,
        evaluation.event.aggregate_id
    )
}

fn forge_proposal_human(proposal: &ForgeProposalRecord) -> String {
    format!(
        "proposal={} parent={} child={} selection={} mutation={}→{} hypothesis={:?} promotion_eligible={} event={} sequence={} hash={}",
        proposal.payload.proposal_id,
        proposal.payload.parent_genome_id,
        proposal.payload.child.genome_id,
        proposal.payload.selection_event_id,
        proposal.payload.operation_before,
        proposal.payload.operation_after,
        proposal.payload.hypothesis,
        proposal.promotion_eligible,
        proposal.event.event_id,
        proposal.event.sequence,
        proposal.event.event_hash,
    )
}

fn forge_assessment_human(assessment: &ForgeAssessmentRecord) -> String {
    let outcome = match assessment.payload.outcome {
        ForgeAssessmentOutcome::MetricsPassed => "metrics_passed",
        ForgeAssessmentOutcome::MetricsRejected => "metrics_rejected",
    };
    format!(
        "assessment={} proposal={} evaluation={} parent={} child={} outcome={} invariant_gate_verified={} promotion_eligible={} selection_event={} event={} sequence={} hash={}",
        assessment.payload.assessment_id,
        assessment.payload.proposal_id,
        assessment.payload.evaluation_id,
        assessment.payload.parent_genome_id,
        assessment.payload.child_genome_id,
        outcome,
        assessment.payload.invariant_gate_verified,
        assessment.payload.promotion_eligible,
        assessment.payload.selection_event_id,
        assessment.event.event_id,
        assessment.event.sequence,
        assessment.event.event_hash,
    )
}

fn selection_human(selection: &SelectionRecord) -> String {
    format!(
        "selection={} world={} correctness_regressions={} correctness_improvements={} lower_bps={} metrics_eligible={} pareto_dominates={} invariant_gate_verified={} promotion_eligible={} event={} sequence={} aggregate={} receipt={}",
        selection.evaluation_id,
        selection.world_id,
        selection.receipt.correctness_regressions(),
        selection.receipt.correctness_improvements(),
        selection.receipt.lower_bps(),
        selection.receipt.metrics_eligible(),
        selection.receipt.candidate_pareto_dominates(),
        selection.receipt.invariant_gate_verified(),
        selection.receipt.promotion_eligible(),
        selection.event.event_id,
        selection.event.sequence,
        selection.event.aggregate_id,
        selection.event.receipt_artifact_id,
    )
}

fn invariants_human(invariants: &InvariantRecord) -> String {
    let receipt = &invariants.receipt;
    let predicates = receipt
        .predicates
        .iter()
        .map(|predicate| {
            let suffix = predicate
                .forbidden_ascii_byte
                .map_or_else(String::new, |byte| format!("_byte_{byte}"));
            format!(
                "{}{}:parent={},candidate={},regressions={}",
                predicate.predicate,
                suffix,
                predicate.parent_violations,
                predicate.candidate_violations,
                predicate.paired_regressions
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "invariants evaluation={} world={} checks={} trials={} candidate_violations={} paired_regressions={} maximum_regressions={} regressions_within_budget={} candidate_contract_satisfied={} predicates=[{}] event={} sequence={} hash={} receipt={}",
        receipt.evaluation_id,
        receipt.world_id,
        receipt.total_checks,
        receipt.total_evaluated_trials,
        receipt.total_candidate_violations,
        receipt.total_paired_regressions,
        receipt.maximum_regressions,
        receipt.regressions_within_budget,
        receipt.candidate_contract_satisfied,
        predicates,
        invariants.event.event_id,
        invariants.event.sequence,
        invariants.event.event_hash,
        invariants.event.receipt_artifact_id,
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use std::{fs, path::Path, time::SystemTime};

    use super::{
        Arguments, command_from_cli, copy_fixture_into_new_destination, evaluation_human,
        fixture_source_dir, packaged_tui_paths,
    };
    use hephaestus_control::{Command, EvaluationEventRecord, EvaluationRecord};

    #[test]
    fn genome_assess_maps_stable_ids_to_the_authenticated_command() {
        let arguments = Arguments::try_parse_from([
            "hephaestus",
            "genome",
            "assess",
            "assessment-1",
            "--proposal",
            "proposal-1",
            "--selection-event",
            "selection-event-1",
        ])
        .expect("assessment command parses");

        assert_eq!(
            command_from_cli(arguments.command).expect("command maps"),
            Command::GenomeAssess {
                assessment_id: "assessment-1".to_owned(),
                proposal_id: "proposal-1".to_owned(),
                selection_event_id: "selection-event-1".to_owned(),
            }
        );
    }

    #[test]
    fn champion_commands_map_flags_to_authenticated_commands() {
        let parse = |arguments: &[&str]| {
            command_from_cli(
                Arguments::try_parse_from(arguments)
                    .expect("CLI parses")
                    .command,
            )
            .expect("command maps")
        };
        assert_eq!(
            parse(&[
                "hephaestus",
                "champion",
                "seed",
                "seed-1",
                "--world",
                "world-1",
                "--genome",
                "genome-1",
                "--reason",
                "initial"
            ]),
            Command::ChampionSeed {
                transition_id: "seed-1".to_owned(),
                world_id: "world-1".to_owned(),
                genome_id: "genome-1".to_owned(),
                reason: "initial".to_owned(),
            }
        );
        assert_eq!(
            parse(&[
                "hephaestus",
                "champion",
                "promote",
                "promote-1",
                "--assessment",
                "assessment-1"
            ]),
            Command::ChampionPromote {
                transition_id: "promote-1".to_owned(),
                assessment_id: "assessment-1".to_owned(),
            }
        );
        assert_eq!(
            parse(&[
                "hephaestus",
                "champion",
                "rollback",
                "rollback-1",
                "--world",
                "world-1",
                "--reason",
                "regressed"
            ]),
            Command::ChampionRollback {
                transition_id: "rollback-1".to_owned(),
                world_id: "world-1".to_owned(),
                reason: "regressed".to_owned(),
            }
        );
        assert_eq!(
            parse(&["hephaestus", "champion", "show", "world-1"]),
            Command::ChampionShow {
                world_id: "world-1".to_owned(),
            }
        );
    }

    #[test]
    fn arena_invariants_maps_evaluation_identity_to_authenticated_command() {
        let arguments =
            Arguments::try_parse_from(["hephaestus", "arena", "invariants", "evaluation-1"])
                .expect("CLI parses");

        assert_eq!(
            command_from_cli(arguments.command).expect("command maps"),
            Command::ArenaInvariants {
                evaluation_id: "evaluation-1".to_owned(),
            }
        );
    }

    #[test]
    fn arena_evaluate_maps_positional_identifiers_to_paired_command() {
        let arguments = Arguments::try_parse_from([
            "hephaestus",
            "arena",
            "evaluate",
            "evaluation-1",
            "parent-1",
            "candidate-1",
        ])
        .expect("CLI parses");

        assert_eq!(
            command_from_cli(arguments.command).expect("command maps"),
            Command::EvaluatePair {
                evaluation_id: "evaluation-1".to_owned(),
                parent_genome_id: "parent-1".to_owned(),
                candidate_genome_id: "candidate-1".to_owned(),
            }
        );
    }

    #[test]
    fn init_cli_accepts_the_quickstart_fixture_and_destination() {
        let arguments = Arguments::try_parse_from([
            "hephaestus",
            "init",
            "--fixture",
            "quickstart",
            "/tmp/quickstart",
        ])
        .expect("fixture command parses");

        assert!(
            matches!(arguments.command, super::CliCommand::Init { fixture, path }
            if fixture == "quickstart" && path.as_path() == Path::new("/tmp/quickstart"))
        );
    }

    #[test]
    fn packaged_tui_paths_follow_the_relocated_executable() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock is after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "hephaestus-package-layout-{}-{nonce}",
            std::process::id()
        ));
        let bin = root.join("bin");
        let share = root.join("share/hephaestus");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(share.join("tui")).unwrap();
        fs::write(bin.join("hephaestus"), b"cli").unwrap();
        fs::write(bin.join("node"), b"node").unwrap();
        fs::write(share.join("package.json"), b"{}\n").unwrap();
        fs::write(share.join("tui/main.mjs"), b"process.exit(0)\n").unwrap();
        let canonical_root = root.canonicalize().unwrap();

        assert_eq!(
            packaged_tui_paths(&bin.join("hephaestus")),
            Some((
                canonical_root.join("bin/node"),
                canonical_root.join("share/hephaestus/tui/main.mjs")
            ))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_checkout_fixture_path_resolves_from_control_crate() {
        let fixture = fixture_source_dir().expect("source checkout fixture is available");
        assert!(fixture.join("world.template.json").is_file());
        assert!(fixture.join("tasks/visible.json").is_file());
    }

    #[test]
    fn fixture_copy_losing_destination_creation_race_preserves_existing_files() {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock is after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "hephaestus-fixture-race-{}-{nonce}",
            std::process::id()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("example.md"), "fixture").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("owner.txt"), "preserve me").unwrap();

        let result = copy_fixture_into_new_destination(&source, &destination);

        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(destination.join("owner.txt")).unwrap(),
            "preserve me"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn arena_select_maps_evaluation_identity_to_trusted_selection_command() {
        let arguments =
            Arguments::try_parse_from(["hephaestus", "arena", "select", "evaluation-1"])
                .expect("CLI parses");

        assert_eq!(
            command_from_cli(arguments.command).expect("command maps"),
            Command::ArenaSelect {
                evaluation_id: "evaluation-1".to_owned(),
            }
        );
    }

    #[test]
    fn evaluation_human_output_is_aggregate_only() {
        let evaluation = EvaluationRecord {
            evaluation_id: "evaluation-1".to_owned(),
            world_id: "world-1".to_owned(),
            parent_genome_id: "parent-1".to_owned(),
            candidate_genome_id: "candidate-1".to_owned(),
            parent_visible_correct: 2,
            candidate_visible_correct: 3,
            visible_total: 4,
            event: EvaluationEventRecord {
                sequence: 9,
                event_id: "evaluation:evaluation-1:recorded".to_owned(),
                aggregate_id: "evaluation:evaluation-1".to_owned(),
                event_type: "evaluation.recorded".to_owned(),
                actor: "arena-plane".to_owned(),
                timestamp_millis: 1_234,
            },
        };

        assert_eq!(
            evaluation_human(&evaluation),
            "evaluation=evaluation-1 parent=parent-1 candidate=candidate-1 world=world-1 candidate_visible=3/4 parent_visible=2/4 event=evaluation:evaluation-1:recorded sequence=9 aggregate=evaluation:evaluation-1"
        );
    }
}
