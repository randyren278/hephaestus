use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use hephaestus_control::{
    ApiResponse, Client, Command, EvaluationRecord, GenomeRecord, ResponseData, WorldRecord,
    data_dir_from_environment,
};

#[derive(Parser)]
#[command(name = "hephaestus", about = "Hephaestus operator CLI")]
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
}

#[derive(Subcommand)]
enum GenomeCommand {
    /// Show one canonical Genome record.
    Show {
        /// Content-derived Genome identity.
        genome_id: String,
    },
    /// List every registered Genome.
    List,
    /// Compile a JSON or YAML Genome source under a registered World and register it.
    Register {
        /// Genome source file.
        path: PathBuf,
        /// Registered World the Genome is compiled against.
        #[arg(long)]
        world: String,
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
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Request an audited graceful daemon stop.
    Stop,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
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
    let command = match command_from_cli(arguments.command) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("hephaestus: {message}");
            return ExitCode::FAILURE;
        }
    };
    let response = match Client::new(data_dir).request(command) {
        Ok(response) => response,
        Err(error) => {
            eprintln!("hephaestus: {error}");
            return ExitCode::FAILURE;
        }
    };
    if arguments.json {
        println!(
            "{}",
            serde_json::to_string(&response).expect("API response serialization cannot fail")
        );
    } else {
        print_human(&response);
    }
    if response.error.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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
        CliCommand::Genome {
            command: GenomeCommand::Show { genome_id },
        } => Command::GenomeShow { genome_id },
        CliCommand::Genome {
            command: GenomeCommand::List,
        } => Command::GenomeList,
        CliCommand::Genome {
            command: GenomeCommand::Register { path, world },
        } => Command::GenomeRegister {
            path: absolute_path(path)?,
            world_id: world,
        },
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
        CliCommand::Replay => Command::Replay,
        CliCommand::Daemon {
            command: DaemonCommand::Stop,
        } => Command::DaemonStop,
    })
}

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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Arguments, command_from_cli, evaluation_human};
    use hephaestus_control::{Command, EvaluationEventRecord, EvaluationRecord};

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
