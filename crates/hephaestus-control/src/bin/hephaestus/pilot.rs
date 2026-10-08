//! Native, registration-only preparation for the bundled hosted pilot.

use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Subcommand, ValueEnum};
use hephaestus_control::{
    Client, Command, GenomeProfileRecord, MAX_EVALUATION_COST_MICROUSD, ResponseData,
};
use hephaestus_runtime::RunSpec;
use serde::Serialize;

#[derive(Subcommand)]
pub enum PilotCommand {
    /// Register the support-triage World and prompt pair while the daemon is frozen.
    Prepare {
        /// Support-triage directory created by `init --fixture support-triage`.
        pack: PathBuf,
        /// Explicit hosted provider for both prompts; no provider is invoked here.
        #[arg(long, value_enum)]
        provider: Provider,
        /// Exact model identifier for both prompts (availability is not checked).
        #[arg(long)]
        model: String,
        /// Reported per-trial ceiling in integer micro-USD, not a billing cap.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=MAX_EVALUATION_COST_MICROUSD))]
        cost_microusd: u64,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Provider {
    Codex,
    Claude,
}

impl Provider {
    const fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Serialize)]
struct Preparation {
    fixture: &'static str,
    setup_directory: PathBuf,
    world_id: String,
    parent_genome_id: String,
    candidate_genome_id: String,
    baseline_profile: Box<GenomeProfileRecord>,
    candidate_profile: Box<GenomeProfileRecord>,
    frozen: bool,
    provider_work_started: bool,
}

pub fn run(command: &PilotCommand, data_dir: &Path, json: bool) -> ExitCode {
    let PilotCommand::Prepare {
        pack,
        provider,
        model,
        cost_microusd,
    } = command;
    match prepare(pack, *provider, model, *cost_microusd, data_dir) {
        Ok(preparation) => {
            if !json {
                println!("Prepared support-triage while frozen. No provider work was started.");
                println!("Profiles and immutable identities:");
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&preparation).expect("preparation is serializable")
            );
            if !json {
                println!(
                    "Review both profiles and authorize provider usage before unfreezing. Recorded USD is not a bill; Codex reports no USD."
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("hephaestus: pilot preparation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn request(client: &Client, command: Command) -> Result<ResponseData, String> {
    let response = client.request(command).map_err(|error| error.to_string())?;
    if let Some(error) = response.error {
        return Err(error.message);
    }
    response
        .data
        .ok_or_else(|| "daemon returned no data".to_owned())
}

fn require_frozen(client: &Client) -> Result<(), String> {
    match request(client, Command::Status)? {
        ResponseData::Status {
            frozen: true,
            active_runs: 0,
            ..
        } => Ok(()),
        ResponseData::Status { .. } => {
            Err("a frozen daemon with no active runs is required; freeze it and retry".to_owned())
        }
        _ => Err("daemon returned an unexpected status response".to_owned()),
    }
}

fn artifact(client: &Client, command: Command) -> Result<String, String> {
    match request(client, command)? {
        ResponseData::Artifact { artifact_id, .. } | ResponseData::Verifier { artifact_id, .. } => {
            Ok(artifact_id)
        }
        _ => Err("daemon returned an unexpected artifact response".to_owned()),
    }
}

fn replace_required(text: &str, placeholder: &str, value: &str) -> Result<String, String> {
    if text.matches(placeholder).count() != 1 {
        return Err(format!(
            "pack must contain exactly one {placeholder} placeholder"
        ));
    }
    Ok(text.replace(placeholder, value))
}

fn prompt_template(text: &str, provider: Provider, model: &str) -> Result<String, String> {
    RunSpec::validate_provider_model(model).map_err(|error| error.to_string())?;
    let text = replace_required(text, "__PROVIDER__", provider.name())?;
    // JSON strings are also valid quoted YAML scalars: model IDs such as
    // `null` must stay strings and cannot inject frontmatter or instructions.
    replace_required(
        &text,
        "__MODEL_ID__",
        &serde_json::to_string(model).map_err(|error| error.to_string())?,
    )
}

fn write_setup(directory: &Path, name: &str, content: &str) -> Result<String, String> {
    let path = directory.join(name);
    fs::write(&path, content).map_err(|error| error.to_string())?;
    Ok(path.display().to_string())
}

#[allow(clippy::too_many_lines)]
fn prepare(
    pack: &Path,
    provider: Provider,
    model: &str,
    cost: u64,
    data_dir: &Path,
) -> Result<Preparation, String> {
    let pack = pack
        .canonicalize()
        .map_err(|error| format!("could not resolve pilot pack: {error}"))?;
    let read = |name: &str| {
        fs::read_to_string(pack.join(name))
            .map_err(|error| format!("could not read {name}: {error}"))
    };
    let baseline = prompt_template(&read("baseline.template.md")?, provider, model)?;
    let candidate = prompt_template(&read("candidate.template.md")?, provider, model)?;
    let mut world = replace_required(
        &read("world.template.json")?,
        "\"__COST_CEILING_MICROUSD__\"",
        &cost.to_string(),
    )?;
    for placeholder in [
        "__VISIBLE_MANIFEST__",
        "__SEALED_MANIFEST__",
        "__EVALUATOR__",
        "__VERIFIER__",
    ] {
        replace_required(&world, placeholder, "preflight")?;
    }
    replace_required(&candidate, "__PARENT_ID__", "preflight")?;
    // Keep the operator's socket address: resolving a short symlink can
    // exceed the platform's Unix-socket path limit even for a running daemon.
    let client = Client::new(data_dir.to_path_buf());
    let data_dir = data_dir
        .canonicalize()
        .map_err(|error| format!("start a frozen daemon first: {error}"))?;
    require_frozen(&client)?;
    let evaluator = super::current_executable().with_file_name(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !evaluator.is_file() {
        return Err("reference evaluator is missing beside the installed CLI".to_owned());
    }
    let temporary = tempfile::Builder::new()
        .prefix("pilot-prepare-")
        .tempdir_in(&data_dir)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let setup = temporary.keep();
    eprintln!(
        "hephaestus: preparation files retained at {} (registration is resumable, not an atomic transaction)",
        setup.display()
    );
    for (placeholder, command) in [
        (
            "__VISIBLE_MANIFEST__",
            Command::ManifestPut {
                path: pack.join("tasks/visible.json").display().to_string(),
            },
        ),
        (
            "__SEALED_MANIFEST__",
            Command::ManifestPut {
                path: pack.join("tasks/sealed.json").display().to_string(),
            },
        ),
        (
            "__EVALUATOR__",
            Command::ArtifactPut {
                path: evaluator.display().to_string(),
            },
        ),
        ("__VERIFIER__", Command::VerifierShow),
    ] {
        world = replace_required(&world, placeholder, &artifact(&client, command)?)?;
    }
    let ResponseData::World { world } = request(
        &client,
        Command::WorldRegister {
            path: write_setup(&setup, "world.json", &world)?,
        },
    )?
    else {
        return Err("daemon returned an unexpected World response".to_owned());
    };
    let ResponseData::Genome { genome: parent } = request(
        &client,
        Command::GenomeRegister {
            path: write_setup(&setup, "baseline.md", &baseline)?,
            world_id: world.world_id.clone(),
        },
    )?
    else {
        return Err("daemon returned an unexpected baseline response".to_owned());
    };
    let candidate = replace_required(&candidate, "__PARENT_ID__", &parent.genome_id)?;
    let ResponseData::Genome { genome: child } = request(
        &client,
        Command::GenomeRegister {
            path: write_setup(&setup, "candidate.md", &candidate)?,
            world_id: world.world_id.clone(),
        },
    )?
    else {
        return Err("daemon returned an unexpected candidate response".to_owned());
    };
    let mut profiles = Vec::new();
    for genome_id in [&parent.genome_id, &child.genome_id] {
        let ResponseData::GenomeProfile { profile } = request(
            &client,
            Command::GenomeProfile {
                genome_id: genome_id.clone(),
            },
        )?
        else {
            return Err("daemon returned an unexpected profile response".to_owned());
        };
        profiles.push(profile);
    }
    require_frozen(&client)?;
    let candidate_profile = profiles.pop().expect("two profiles requested");
    let baseline_profile = profiles.pop().expect("two profiles requested");
    Ok(Preparation {
        fixture: "support-triage",
        setup_directory: setup,
        world_id: world.world_id,
        parent_genome_id: parent.genome_id,
        candidate_genome_id: child.genome_id,
        baseline_profile,
        candidate_profile,
        frozen: true,
        provider_work_started: false,
    })
}

#[cfg(test)]
mod tests {
    use super::{Provider, prompt_template};

    #[test]
    fn model_values_are_quoted_and_frontmatter_injection_is_rejected() {
        let template = "provider: __PROVIDER__\nfamily: __MODEL_ID__\n";
        assert_eq!(
            prompt_template(template, Provider::Codex, "null").unwrap(),
            "provider: codex\nfamily: \"null\"\n"
        );
        for model in ["", "model\nworkspace_write: true", "model\"", "模型"] {
            assert!(prompt_template(template, Provider::Claude, model).is_err());
        }
        assert!(prompt_template("family: __MODEL_ID__", Provider::Codex, "model").is_err());
        assert!(
            prompt_template(
                "__PROVIDER__ __PROVIDER__ __MODEL_ID__",
                Provider::Claude,
                "model"
            )
            .is_err()
        );
    }
}
