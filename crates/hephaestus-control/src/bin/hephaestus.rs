use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
    thread,
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand};
use hephaestus_control::{
    API_VERSION, ApiResponse, ArenaJobProgress, CanaryRecord, CanaryTransitionRecord,
    ChampionRecord, ChampionTransitionRecord, Client, Command, DenialEntry, DriftKind, DriftRecord,
    EvaluationListEntry, EvaluationRecord, EvolutionRunRecord, EvolutionRunState,
    ForgeAnalysisRecord, ForgeAssessmentOutcome, ForgeAssessmentRecord, ForgeProposalRecord,
    GeneRecord, GeneSpeciesRecord, GeneSummary, GeneTransferRecord, GenomeRecord, InvariantRecord,
    JobState, MetaBootstrapInterval, MetaLineageOutcome, MetaLineageSpec, MetaReceiptRecord,
    MetaStrategyRecord, ResponseData, RunListEntry, SelectionRecord, WorldRecord,
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
    /// Record drift observations derived from verified evidence.
    Drift {
        #[command(subcommand)]
        command: DriftCommand,
    },
    /// Start, advance, live-check, and inspect staged canary rollouts.
    Canary {
        #[command(subcommand)]
        command: CanaryCommand,
    },
    /// Extract, transfer, and speciate reusable Genes across lineages.
    Gene {
        #[command(subcommand)]
        command: GeneCommand,
    },
    /// Run trusted paired evaluations.
    Arena {
        #[command(subcommand)]
        command: ArenaCommand,
    },
    /// Start, inspect, and cancel unattended multi-generation evolution runs.
    Evolve {
        #[command(subcommand)]
        command: EvolveCommand,
    },
    /// Register Evolver strategy Genomes and run meta-evaluations of them
    /// over held-out base lineages (recursive evolution of the Evolver).
    Meta {
        #[command(subcommand)]
        command: MetaCommand,
    },
    /// Cluster failure evidence and suggest hypotheses and mutations.
    Forge {
        #[command(subcommand)]
        command: ForgeCommand,
    },
    /// Verify and replay the canonical event stream.
    Replay,
    /// List recent direct runs and jobs, newest first.
    Runs {
        /// Maximum entries to return (1-200).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// List recent Arena evaluations, newest first.
    Evaluations {
        /// Maximum entries to return (1-200).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// List recent refused operator requests and recorded runtime denials, newest first.
    Denials {
        /// Maximum entries to return (1-200).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Control the local daemon process.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Manage authenticated remote worker credentials and jobs.
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
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
enum WorkerCommand {
    /// Mint one scoped, expiring credential for a remote worker.
    CredentialMint {
        /// Operator-chosen stable worker identity.
        worker_id: String,
        /// Time-to-live in seconds.
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: u64,
    },
    /// Revoke one previously minted credential.
    CredentialRevoke {
        /// Content-derived credential identity.
        credential_id: String,
    },
    /// Admit one bounded direct reference run for remote-worker execution.
    Submit {
        /// Caller-selected idempotency key.
        job_id: String,
        /// Content-derived registered Genome identity.
        genome_id: String,
    },
    /// Inspect one admitted remote-worker job.
    Status {
        /// Stable job identity returned by `submit`.
        job_id: String,
    },
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
    ///
    /// Supply either `--hypothesis`, or both `--analysis` and `--cluster` to
    /// derive the hypothesis and mutation from one verified `forge analyze` cluster.
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
        hypothesis: Option<String>,
        /// Verified `forge analyze` analysis to derive the hypothesis and mutation from.
        #[arg(long)]
        analysis: Option<String>,
        /// Index into that analysis's clusters, in canonical signature order.
        #[arg(long)]
        cluster: Option<u32>,
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
enum DriftCommand {
    /// Record one drift observation derived from a verified paired evaluation
    /// of the current Champion. Drift never directly replaces a Champion.
    Record {
        /// Stable idempotency key for this drift record.
        drift_id: String,
        /// Registered World the drift was observed under.
        #[arg(long)]
        world: String,
        /// Kind of shift the evidence must cite.
        #[arg(long, value_enum)]
        kind: DriftKindArg,
        /// Evaluation identity supplying the cited `SelectionReceipt`.
        #[arg(long)]
        evidence: String,
    },
    /// Show one recorded drift observation.
    Show {
        /// Stable drift idempotency key.
        drift_id: String,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DriftKindArg {
    Latency,
    Cost,
    Correctness,
    Workload,
}

impl From<DriftKindArg> for DriftKind {
    fn from(value: DriftKindArg) -> Self {
        match value {
            DriftKindArg::Latency => Self::Latency,
            DriftKindArg::Cost => Self::Cost,
            DriftKindArg::Correctness => Self::Correctness,
            DriftKindArg::Workload => Self::Workload,
        }
    }
}

#[derive(Subcommand)]
enum CanaryCommand {
    /// Start (or idempotently re-admit) a canary bound to a candidate and a
    /// passing Forge assessment. The prior Champion stays Champion.
    Start {
        /// Stable idempotency key for the canary.
        canary_id: String,
        /// Registered World the canary runs under.
        #[arg(long)]
        world: String,
        /// Candidate Genome this canary rolls out.
        #[arg(long)]
        candidate: String,
        /// Forge assessment of the current Champion against the candidate.
        #[arg(long)]
        assessment: String,
    },
    /// Advance a canary on staged health evidence. A regression automatically
    /// aborts the canary instead of advancing it.
    Advance {
        /// Stable canary idempotency key.
        canary_id: String,
        /// Evaluation identity supplying this stage's `SelectionReceipt`.
        #[arg(long)]
        evidence: String,
    },
    /// Check a completed canary's Champion against the previous Champion; a
    /// regression automatically triggers a Champion rollback.
    LiveCheck {
        /// Stable canary idempotency key.
        canary_id: String,
        /// Evaluation identity supplying the live `SelectionReceipt`.
        #[arg(long)]
        evidence: String,
    },
    /// Show one canary's stage and transition history.
    Show {
        /// Stable canary idempotency key.
        canary_id: String,
    },
}

#[derive(Subcommand)]
enum GeneCommand {
    /// Extract a Gene from a promoted, evidence-bound Champion transition.
    Extract {
        /// Stable idempotency key for this Gene.
        gene_id: String,
        /// Champion transition that promoted the origin child.
        #[arg(long)]
        promotion: String,
    },
    /// Apply a Gene's mutation to another lineage's Genome through the
    /// ordinary compiler, producing an unevaluated transfer child.
    Transfer {
        /// Stable idempotency key for this transfer trial.
        trial_id: String,
        /// Gene being transferred.
        #[arg(long)]
        gene: String,
        /// Recipient Genome the Gene's mutation is applied to.
        #[arg(long)]
        to: String,
    },
    /// Record a transfer trial's effect from a verified paired evaluation.
    Record {
        /// Transfer trial being recorded; must already be applied.
        trial_id: String,
        /// Arena evaluation identity of the recipient-versus-child pair.
        #[arg(long)]
        evaluation: String,
    },
    /// Show one Gene, its transfer trials, and any contradiction or species.
    Show {
        /// Gene identity.
        gene_id: String,
    },
    /// List every extracted Gene with its aggregate transfer counts.
    List,
    /// Create a specialist species from persistent domain advantage.
    Speciate {
        /// Stable idempotency key for this species.
        species_id: String,
        /// Gene whose domain advantage is being formalized.
        #[arg(long)]
        gene: String,
        /// Registered World (domain) the species specializes in.
        #[arg(long)]
        domain: String,
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
enum EvolveCommand {
    /// Start (or idempotently re-admit) an unattended, budget-bounded run.
    ///
    /// The World must already have a second registered Genome besides
    /// `--from`: the evolve engine uses it as the fixed comparison baseline
    /// for every generation's diagnostic evaluation.
    Start {
        /// Stable idempotency key for this run.
        run_id: String,
        /// Registered World the run evolves within.
        #[arg(long)]
        world: String,
        /// Genome seeded (or already installed) as generation zero's Champion.
        #[arg(long)]
        from: String,
        /// Hard ceiling on the number of generations this run may complete.
        #[arg(long)]
        generations: u32,
        /// Hard ceiling on the number of paired Arena evaluations (trials)
        /// this run may submit; each generation consumes exactly two.
        #[arg(long)]
        budget: u64,
    },
    /// Show one evolution run's durable, replay-verified progress.
    Status {
        /// Stable run identity returned by `start`.
        run_id: String,
    },
    /// Request cooperative cancellation of one active evolution run.
    Cancel {
        /// Stable run identity returned by `start`.
        run_id: String,
    },
    /// Convenience: registers the bundled Gauntlet "coding" World and its
    /// two reference Genomes against the running daemon (idempotently, if
    /// not already registered), then starts and drives a 3-generation
    /// unattended run to completion.
    ///
    /// Prerequisites this registers for you: `examples/gauntlet/coding`'s
    /// World/Genome/task fixtures, published against the connected daemon's
    /// own evaluator binary (found next to this executable) and runtime
    /// verifier key. The daemon must already be running and reachable at
    /// `--data-dir` (see `hephaestusd`), and must be unfrozen (this command
    /// unfreezes it if needed).
    Coding {
        /// Hard ceiling on the number of paired Arena evaluations (trials)
        /// the run may submit; 3 generations consume exactly 6.
        #[arg(long)]
        budget: u64,
    },
}

#[derive(Subcommand)]
enum MetaCommand {
    /// Register, inspect, and list Evolver strategy Genomes.
    Strategy {
        #[command(subcommand)]
        command: MetaStrategyCommand,
    },
    /// Run a paired meta-evaluation of two Evolver strategies over held-out
    /// base lineages, driving the existing evolve engine unmodified.
    ///
    /// Each lineage is one already-registered World together with the
    /// Genome installed as its generation-zero Champion (exactly what
    /// `evolve start --from` takes); the World must already carry a second
    /// registered Genome to serve as the evolve engine's fixed baseline.
    /// Supply one `--lineage-world`/`--lineage-genome` pair per lineage, in
    /// matching order.
    Evaluate {
        /// Stable idempotency key for this meta-evaluation.
        meta_run_id: String,
        /// Registered Evolver strategy Genome, the "A" side of the comparison.
        #[arg(long = "strategy-a")]
        strategy_a: String,
        /// Registered Evolver strategy Genome, the "B" side of the comparison.
        #[arg(long = "strategy-b")]
        strategy_b: String,
        /// Held-out lineage World identities, one per lineage.
        #[arg(long = "lineage-world", required = true)]
        lineage_world: Vec<String>,
        /// Held-out lineage generation-zero Genome identities, matched by
        /// position to `--lineage-world`.
        #[arg(long = "lineage-genome", required = true)]
        lineage_genome: Vec<String>,
        /// Declared held-out lineage count; must equal the number of
        /// `--lineage-world`/`--lineage-genome` pairs supplied.
        #[arg(long)]
        lineages: usize,
        /// Bootstrap confidence, in basis points (for example `9_500` for 95%).
        #[arg(long, default_value_t = 9_500)]
        confidence_bps: u16,
        /// Deterministic bootstrap resampling seed.
        #[arg(long, default_value_t = 0)]
        seed: u64,
    },
    /// Show one meta-evaluation's durable, replay-verified receipt.
    Show {
        /// Stable meta-evaluation identity returned by `evaluate`.
        meta_run_id: String,
    },
    /// List recent meta-evaluation receipts, newest first.
    List {
        /// Maximum entries to return (1-200).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum MetaStrategyCommand {
    /// Register (or idempotently re-resolve) an Evolver strategy Genome.
    Register {
        /// JSON `EvolverStrategyConfig` source file.
        path: PathBuf,
    },
    /// Show one registered Evolver strategy Genome.
    Show {
        /// Content-derived strategy identity.
        strategy_id: String,
    },
    /// List every registered Evolver strategy Genome.
    List,
}

#[derive(Subcommand)]
enum ForgeCommand {
    /// Cluster one candidate's failed trials and suggest hypotheses and mutations.
    Analyze {
        /// Stable idempotency key for this analysis.
        analysis_id: String,
        /// Stable Arena evaluation identity to cluster.
        #[arg(long)]
        evaluation: String,
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
    if let CliCommand::Evolve {
        command: EvolveCommand::Coding { budget },
    } = &arguments.command
    {
        return run_evolve_coding(&data_dir, *budget, arguments.json);
    }
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

/// Locates the bundled `examples/gauntlet/coding` fixture: a packaged share
/// directory if this is an installed build, otherwise the source checkout
/// next to this crate. Mirrors [`fixture_source_dir`]'s resolution exactly.
fn gauntlet_coding_fixture_dir() -> Result<PathBuf, String> {
    if let Some(share) = packaged_share_dir(&current_executable()) {
        let fixtures = share.join("fixtures/gauntlet-coding");
        if fixtures.is_dir() {
            return Ok(fixtures);
        }
        return Err("installed gauntlet coding fixture is unavailable".to_owned());
    }
    let source_checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/gauntlet/coding");
    source_checkout
        .is_dir()
        .then_some(source_checkout)
        .ok_or_else(|| {
            "gauntlet coding fixture is unavailable (run from a source checkout, or install \
         the bundled fixtures)"
                .to_owned()
        })
}

/// One request/response round trip for `evolve coding`'s registration
/// sequence: prints a clear message and returns `None` on any transport or
/// application-level failure so the caller can bail with `ExitCode::FAILURE`.
fn coding_step(client: &Client, label: &str, command: Command) -> Option<ResponseData> {
    match client.request(command) {
        Ok(response) => {
            if let Some(error) = response.error {
                eprintln!("hephaestus: {label} failed: {}", error.message);
                None
            } else if let Some(data) = response.data {
                Some(data)
            } else {
                eprintln!("hephaestus: {label} returned no data");
                None
            }
        }
        Err(error) => {
            eprintln!("hephaestus: {label} failed: {error}");
            None
        }
    }
}

/// `hephaestus evolve coding --budget <n>`: registers the bundled Gauntlet
/// "coding" World and its parent/candidate reference Genomes against the
/// connected daemon (idempotently, if not already registered), starts a
/// 3-generation unattended evolve run, and polls it to completion.
///
/// This exercises the exact same durable evolve engine as `evolve start`;
/// the only thing it adds is the World/Genome bootstrap an operator would
/// otherwise run by hand (see `examples/gauntlet/coding` and
/// `docs/EVOLUTION.md`). Every Genome here still uses the deterministic
/// reference operations (`identity`/`ascii_uppercase`): Forge's mutation
/// operator only knows how to flip between those two, so this proves the
/// unattended multi-generation *mechanism* against a Gauntlet-flavored
/// World, not that the optimizer can solve a real coding task.
#[allow(clippy::too_many_lines)]
fn run_evolve_coding(data_dir: &Path, budget: u64, json: bool) -> ExitCode {
    const GENERATIONS: u32 = 3;
    const MINIMUM_BUDGET: u64 = 6; // 3 generations x 2 trials each.
    if budget < MINIMUM_BUDGET {
        eprintln!(
            "hephaestus: evolve coding needs --budget of at least {MINIMUM_BUDGET} \
             ({GENERATIONS} generations x 2 trials)"
        );
        return ExitCode::FAILURE;
    }
    let data_dir = match fs::canonicalize(data_dir) {
        Ok(path) => path,
        Err(error) => {
            eprintln!(
                "hephaestus: could not resolve data directory {}: {error} (start `hephaestusd` \
                 first)",
                data_dir.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let examples_dir = match gauntlet_coding_fixture_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("hephaestus: {error}");
            return ExitCode::FAILURE;
        }
    };
    let evaluator_path = current_executable().with_file_name(format!(
        "hephaestus-reference-evaluator{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !evaluator_path.is_file() {
        eprintln!(
            "hephaestus: missing {} next to this executable; run `cargo build --workspace \
             --bins` first",
            evaluator_path.display()
        );
        return ExitCode::FAILURE;
    }
    let work_dir = data_dir.join("work");
    if let Err(error) = fs::create_dir_all(&work_dir) {
        eprintln!(
            "hephaestus: could not create {}: {error}",
            work_dir.display()
        );
        return ExitCode::FAILURE;
    }

    let client = Client::new(data_dir.clone());
    if coding_step(&client, "status", Command::Status).is_none() {
        eprintln!(
            "hephaestus: could not reach a daemon at {} (start `hephaestusd` first)",
            data_dir.display()
        );
        return ExitCode::FAILURE;
    }
    if coding_step(&client, "unfreeze", Command::Unfreeze).is_none() {
        return ExitCode::FAILURE;
    }

    let artifact_id = |data: Option<ResponseData>| match data {
        Some(ResponseData::Artifact { artifact_id, .. }) => Some(artifact_id),
        _ => None,
    };
    let Some(visible_id) = artifact_id(coding_step(
        &client,
        "arena manifest (visible)",
        Command::ManifestPut {
            path: examples_dir
                .join("tasks/visible.json")
                .display()
                .to_string(),
        },
    )) else {
        return ExitCode::FAILURE;
    };
    let Some(sealed_id) = artifact_id(coding_step(
        &client,
        "arena manifest (sealed)",
        Command::ManifestPut {
            path: examples_dir.join("tasks/sealed.json").display().to_string(),
        },
    )) else {
        return ExitCode::FAILURE;
    };
    let Some(evaluator_id) = artifact_id(coding_step(
        &client,
        "artifact put (evaluator)",
        Command::ArtifactPut {
            path: evaluator_path.display().to_string(),
        },
    )) else {
        return ExitCode::FAILURE;
    };
    let Some(ResponseData::Verifier {
        artifact_id: verifier_id,
        ..
    }) = coding_step(&client, "verifier", Command::VerifierShow)
    else {
        return ExitCode::FAILURE;
    };
    let Some(invariants_id) = artifact_id(coding_step(
        &client,
        "artifact put (invariants)",
        Command::ArtifactPut {
            path: examples_dir.join("invariants.json").display().to_string(),
        },
    )) else {
        return ExitCode::FAILURE;
    };

    let world_template = match fs::read_to_string(examples_dir.join("world.template.json")) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("hephaestus: could not read world.template.json: {error}");
            return ExitCode::FAILURE;
        }
    };
    let world_json = world_template
        .replace("__VISIBLE_MANIFEST__", &visible_id)
        .replace("__SEALED_MANIFEST__", &sealed_id)
        .replace("__EVALUATOR__", &evaluator_id)
        .replace("__VERIFIER__", &verifier_id)
        .replace("__INVARIANTS__", &invariants_id);
    let world_path = work_dir.join("gauntlet-coding-world.json");
    if let Err(error) = fs::write(&world_path, world_json) {
        eprintln!(
            "hephaestus: could not write {}: {error}",
            world_path.display()
        );
        return ExitCode::FAILURE;
    }
    let Some(ResponseData::World { world }) = coding_step(
        &client,
        "world register",
        Command::WorldRegister {
            path: world_path.display().to_string(),
        },
    ) else {
        return ExitCode::FAILURE;
    };

    let Some(ResponseData::Genome { genome: parent }) = coding_step(
        &client,
        "genome register (parent)",
        Command::GenomeRegister {
            path: examples_dir.join("parent.md").display().to_string(),
            world_id: world.world_id.clone(),
        },
    ) else {
        return ExitCode::FAILURE;
    };
    let candidate_template = match fs::read_to_string(examples_dir.join("candidate.md")) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("hephaestus: could not read candidate.md: {error}");
            return ExitCode::FAILURE;
        }
    };
    let candidate_path = work_dir.join("gauntlet-coding-candidate.md");
    if let Err(error) = fs::write(
        &candidate_path,
        candidate_template.replace("__PARENT_ID__", &parent.genome_id),
    ) {
        eprintln!(
            "hephaestus: could not write {}: {error}",
            candidate_path.display()
        );
        return ExitCode::FAILURE;
    }
    if coding_step(
        &client,
        "genome register (candidate)",
        Command::GenomeRegister {
            path: candidate_path.display().to_string(),
            world_id: world.world_id.clone(),
        },
    )
    .is_none()
    {
        return ExitCode::FAILURE;
    }

    let run_id = "gauntlet-coding";
    if coding_step(
        &client,
        "evolve start",
        Command::EvolveStart {
            run_id: run_id.to_owned(),
            world_id: world.world_id.clone(),
            from_genome_id: parent.genome_id.clone(),
            generations: GENERATIONS,
            budget,
        },
    )
    .is_none()
    {
        return ExitCode::FAILURE;
    }

    let deadline = Instant::now() + Duration::from_secs(180);
    let final_response = loop {
        match client.request(Command::EvolveStatus {
            run_id: run_id.to_owned(),
        }) {
            Ok(response) => {
                if response.error.is_some() {
                    break response;
                }
                if let Some(ResponseData::Evolution { run }) = &response.data
                    && run.state == EvolutionRunState::Finished
                {
                    break response;
                }
            }
            Err(error) => {
                eprintln!("hephaestus: evolve status failed: {error}");
                return ExitCode::FAILURE;
            }
        }
        if Instant::now() >= deadline {
            eprintln!("hephaestus: evolve coding run did not finish within 180s");
            return ExitCode::FAILURE;
        }
        thread::sleep(Duration::from_millis(50));
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&final_response).expect("API response serialization cannot fail")
        );
    } else {
        print_human(&final_response);
    }
    if final_response.error.is_some() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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

#[allow(clippy::too_many_lines)]
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
        CliCommand::Forge {
            command:
                ForgeCommand::Analyze {
                    analysis_id,
                    evaluation,
                },
        } => Command::ForgeAnalyze {
            analysis_id,
            evaluation_id: evaluation,
        },
        CliCommand::Champion { command } => champion_command_from_cli(command),
        CliCommand::Drift { command } => drift_command_from_cli(command),
        CliCommand::Canary { command } => canary_command_from_cli(command),
        CliCommand::Gene { command } => gene_command_from_cli(command),
        CliCommand::Evolve { command } => evolve_command_from_cli(command)?,
        CliCommand::Meta { command } => meta_command_from_cli(command)?,
        CliCommand::Replay => Command::Replay,
        CliCommand::Runs { limit } => Command::RunList { limit },
        CliCommand::Evaluations { limit } => Command::EvaluationList { limit },
        CliCommand::Denials { limit } => Command::DenialList { limit },
        CliCommand::Daemon {
            command: DaemonCommand::Stop,
        } => Command::DaemonStop,
        CliCommand::Worker {
            command:
                WorkerCommand::CredentialMint {
                    worker_id,
                    ttl_seconds,
                },
        } => Command::WorkerCredentialMint {
            worker_id,
            ttl_seconds,
        },
        CliCommand::Worker {
            command: WorkerCommand::CredentialRevoke { credential_id },
        } => Command::WorkerCredentialRevoke { credential_id },
        CliCommand::Worker {
            command: WorkerCommand::Submit { job_id, genome_id },
        } => Command::RemoteRunSubmit { job_id, genome_id },
        CliCommand::Worker {
            command: WorkerCommand::Status { job_id },
        } => Command::RemoteJobStatus { job_id },
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

fn drift_command_from_cli(command: DriftCommand) -> Command {
    match command {
        DriftCommand::Record {
            drift_id,
            world,
            kind,
            evidence,
        } => Command::DriftRecord {
            drift_id,
            world_id: world,
            kind: kind.into(),
            evidence_evaluation_id: evidence,
        },
        DriftCommand::Show { drift_id } => Command::DriftShow { drift_id },
    }
}

fn canary_command_from_cli(command: CanaryCommand) -> Command {
    match command {
        CanaryCommand::Start {
            canary_id,
            world,
            candidate,
            assessment,
        } => Command::CanaryStart {
            canary_id,
            world_id: world,
            candidate_genome_id: candidate,
            assessment_id: assessment,
        },
        CanaryCommand::Advance {
            canary_id,
            evidence,
        } => Command::CanaryAdvance {
            canary_id,
            evidence_evaluation_id: evidence,
        },
        CanaryCommand::LiveCheck {
            canary_id,
            evidence,
        } => Command::CanaryLiveCheck {
            canary_id,
            evidence_evaluation_id: evidence,
        },
        CanaryCommand::Show { canary_id } => Command::CanaryShow { canary_id },
    }
}

fn gene_command_from_cli(command: GeneCommand) -> Command {
    match command {
        GeneCommand::Extract { gene_id, promotion } => Command::GeneExtract {
            gene_id,
            promotion_transition_id: promotion,
        },
        GeneCommand::Transfer { trial_id, gene, to } => Command::GeneTransfer {
            trial_id,
            gene_id: gene,
            to_genome_id: to,
        },
        GeneCommand::Record {
            trial_id,
            evaluation,
        } => Command::GeneRecord {
            trial_id,
            evaluation_id: evaluation,
        },
        GeneCommand::Show { gene_id } => Command::GeneShow { gene_id },
        GeneCommand::List => Command::GeneList,
        GeneCommand::Speciate {
            species_id,
            gene,
            domain,
        } => Command::GeneSpeciate {
            species_id,
            gene_id: gene,
            domain_world_id: domain,
        },
    }
}

fn evolve_command_from_cli(command: EvolveCommand) -> Result<Command, &'static str> {
    Ok(match command {
        EvolveCommand::Start {
            run_id,
            world,
            from,
            generations,
            budget,
        } => Command::EvolveStart {
            run_id,
            world_id: world,
            from_genome_id: from,
            generations,
            budget,
        },
        EvolveCommand::Status { run_id } => Command::EvolveStatus { run_id },
        EvolveCommand::Cancel { run_id } => Command::EvolveCancel { run_id },
        EvolveCommand::Coding { .. } => return Err("evolve coding is a local convenience command"),
    })
}

fn meta_command_from_cli(command: MetaCommand) -> Result<Command, &'static str> {
    Ok(match command {
        MetaCommand::Strategy { command } => match command {
            MetaStrategyCommand::Register { path } => Command::MetaStrategyRegister {
                path: absolute_path(path)?,
            },
            MetaStrategyCommand::Show { strategy_id } => Command::MetaStrategyShow { strategy_id },
            MetaStrategyCommand::List => Command::MetaStrategyList,
        },
        MetaCommand::Evaluate {
            meta_run_id,
            strategy_a,
            strategy_b,
            lineage_world,
            lineage_genome,
            lineages,
            confidence_bps,
            seed,
        } => {
            if lineage_world.len() != lineage_genome.len() {
                return Err(
                    "--lineage-world and --lineage-genome must be supplied the same number of times",
                );
            }
            if lineage_world.len() != lineages {
                return Err(
                    "--lineages must equal the number of --lineage-world/--lineage-genome pairs supplied",
                );
            }
            let lineages = lineage_world
                .into_iter()
                .zip(lineage_genome)
                .map(|(world_id, from_genome_id)| MetaLineageSpec {
                    world_id,
                    from_genome_id,
                })
                .collect();
            Command::MetaEvaluate {
                meta_run_id,
                strategy_a_id: strategy_a,
                strategy_b_id: strategy_b,
                lineages,
                confidence_bps,
                bootstrap_seed: seed,
            }
        }
        MetaCommand::Show { meta_run_id } => Command::MetaShow { meta_run_id },
        MetaCommand::List { limit } => Command::MetaList { limit },
    })
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
            analysis,
            cluster,
        } => Command::GenomePropose {
            proposal_id,
            selection_event_id: selection_event,
            parent_genome_id: parent,
            hypothesis,
            analysis_id: analysis,
            cluster_index: cluster,
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
        (Some(ResponseData::ForgeAnalysis { analysis }), None) => {
            println!("{}", forge_analysis_human(analysis));
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
        (Some(ResponseData::Drift { drift }), None) => {
            println!("{}", drift_human(drift));
        }
        (Some(ResponseData::CanaryTransition { transition }), None) => {
            println!("{}", canary_transition_human(transition));
        }
        (Some(ResponseData::Canary { canary }), None) => {
            println!("{}", canary_human(canary));
        }
        (Some(ResponseData::Gene { gene }), None) => {
            println!("{}", gene_human(gene));
        }
        (Some(ResponseData::Genes { genes }), None) => {
            for summary in genes {
                println!("{}", gene_summary_human(summary));
            }
        }
        (Some(ResponseData::GeneTransfer { trial }), None) => {
            println!("{}", gene_transfer_human(trial));
        }
        (Some(ResponseData::GeneSpecies { species }), None) => {
            println!("{}", gene_species_human(species));
        }
        (Some(ResponseData::GeneAggregate { aggregate }), None) => {
            println!("{}", gene_human(&aggregate.gene));
            for transfer in &aggregate.transfers {
                println!("{}", gene_transfer_human(transfer));
            }
            if let Some(contradiction) = &aggregate.contradiction {
                println!(
                    "contradiction gene={} positive_trial={} positive_world={} negative_trial={} negative_world={}",
                    contradiction.payload.gene_id,
                    contradiction.payload.positive_trial_id,
                    contradiction.payload.positive_world_id,
                    contradiction.payload.negative_trial_id,
                    contradiction.payload.negative_world_id,
                );
            }
            for species in &aggregate.species {
                println!("{}", gene_species_human(species));
            }
        }
        (Some(ResponseData::Evolution { run }), None) => {
            println!("{}", evolution_human(run));
        }
        (Some(ResponseData::MetaStrategy { strategy }), None) => {
            println!("{}", meta_strategy_human(strategy));
        }
        (Some(ResponseData::MetaStrategies { strategies }), None) => {
            for strategy in strategies {
                println!("{}", meta_strategy_human(strategy));
            }
        }
        (Some(ResponseData::MetaEvaluation { receipt }), None) => {
            println!("{}", meta_receipt_human(receipt));
        }
        (Some(ResponseData::MetaEvaluationList { receipts }), None) => {
            for receipt in receipts {
                println!("{}", meta_receipt_human(receipt));
            }
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
        (Some(ResponseData::RunList { runs }), None) => {
            for run in runs {
                println!("{}", run_list_entry_human(run));
            }
        }
        (Some(ResponseData::EvaluationList { evaluations }), None) => {
            for entry in evaluations {
                println!("{}", evaluation_list_entry_human(entry));
            }
        }
        (Some(ResponseData::DenialList { denials }), None) => {
            for denial in denials {
                println!("{}", denial_human(denial));
            }
        }
        (
            Some(ResponseData::WorkerCredential {
                credential_id,
                token,
                worker_id,
                expires_at_millis,
                scope: _,
            }),
            None,
        ) => {
            println!(
                "credential_id={credential_id} worker_id={worker_id} expires_at_millis={expires_at_millis}"
            );
            println!("token={token}");
        }
        (
            Some(ResponseData::RemoteJob {
                job_id,
                genome_id,
                state,
                completion_reason,
                latency_millis,
                stdout_artifact_id,
            }),
            None,
        ) => {
            print!("job_id={job_id} genome_id={genome_id} state={state:?}");
            if let Some(reason) = completion_reason {
                print!(" completion_reason={reason:?}");
            }
            if let Some(latency) = latency_millis {
                print!(" latency_millis={latency}");
            }
            if let Some(artifact) = stdout_artifact_id {
                print!(" stdout_artifact_id={artifact}");
            }
            println!();
        }
        (Some(ResponseData::McpDenied { reason }), None) => {
            println!("denied: {reason}");
        }
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

fn drift_human(drift: &DriftRecord) -> String {
    format!(
        "drift={} world={} kind={:?} baseline={} shifted={} threshold_bps={} observed_delta_bps={} evidence={} event={} sequence={} hash={}",
        drift.payload.drift_id,
        drift.payload.world_id,
        drift.payload.kind,
        drift.payload.baseline_genome_id,
        drift.payload.shifted_genome_id,
        drift.payload.threshold_bps,
        drift.payload.observed_delta_bps,
        drift.payload.evidence_evaluation_id,
        drift.event.event_id,
        drift.event.sequence,
        drift.event.event_hash,
    )
}

fn canary_transition_human(transition: &CanaryTransitionRecord) -> String {
    format!(
        "canary={} world={} kind={:?} stage={:?} candidate={} previous_champion={} event={} sequence={} hash={}",
        transition.payload.canary_id,
        transition.payload.world_id,
        transition.payload.kind,
        transition.payload.stage,
        transition.payload.candidate_genome_id,
        transition.payload.previous_champion_genome_id,
        transition.event.event_id,
        transition.event.sequence,
        transition.event.event_hash,
    )
}

fn canary_human(canary: &CanaryRecord) -> String {
    let mut lines = vec![format!(
        "canary={} world={} candidate={} previous_champion={} stage={:?} transitions={}",
        canary.canary_id,
        canary.world_id,
        canary.candidate_genome_id,
        canary.previous_champion_genome_id,
        canary.stage,
        canary.transitions.len(),
    )];
    lines.extend(canary.transitions.iter().map(canary_transition_human));
    lines.join("\n")
}

fn gene_human(gene: &GeneRecord) -> String {
    format!(
        "gene={} promotion={} world={} operation={}->{} trials={} threshold={} event={} sequence={} hash={}",
        gene.payload.gene_id,
        gene.payload.promotion_transition_id,
        gene.payload.world_id,
        gene.payload.operation_before,
        gene.payload.operation_after,
        gene.payload.evidence_trials,
        gene.payload.evidence_threshold,
        gene.event.event_id,
        gene.event.sequence,
        gene.event.event_hash,
    )
}

fn gene_summary_human(summary: &GeneSummary) -> String {
    format!(
        "gene={} world={} lineages={} positive={} neutral={} negative={} contradiction={} species={}",
        summary.payload.gene_id,
        summary.payload.world_id,
        summary.lineages,
        summary.positive,
        summary.neutral,
        summary.negative,
        summary.contradiction,
        summary.species_ids.join(","),
    )
}

fn gene_transfer_human(trial: &GeneTransferRecord) -> String {
    let outcome = trial.recorded.as_ref().map_or_else(
        || "unrecorded".to_owned(),
        |recorded| format!("{:?}", recorded.outcome),
    );
    format!(
        "trial={} gene={} to={} child={} world={} outcome={} estimate_bps={}",
        trial.applied.trial_id,
        trial.applied.gene_id,
        trial.applied.to_genome_id,
        trial.applied.child.genome_id,
        trial.applied.world_id,
        outcome,
        trial
            .recorded
            .as_ref()
            .map_or(0, |recorded| recorded.estimate_bps),
    )
}

fn gene_species_human(species: &GeneSpeciesRecord) -> String {
    format!(
        "species={} gene={} domain={} lineages={} average_estimate_bps={}",
        species.payload.species_id,
        species.payload.gene_id,
        species.payload.domain_world_id,
        species.payload.lineage_genome_ids.len(),
        species.payload.average_estimate_bps,
    )
}

fn evolution_human(run: &EvolutionRunRecord) -> String {
    let mut lines = vec![format!(
        "run={} world={} from={} baseline={} state={:?} finish_reason={} generations={}/{} trials={}/{} cancel_requested={}",
        run.run_id,
        run.world_id,
        run.from_genome_id,
        run.baseline_genome_id,
        run.state,
        run.finish_reason
            .map_or_else(|| "none".to_owned(), |reason| format!("{reason:?}")),
        run.generations.len(),
        run.max_generations,
        run.trials_consumed,
        run.max_paired_trials,
        run.cancel_requested,
    )];
    lines.extend(run.generations.iter().map(|generation| {
        format!(
            "  generation={} champion_before={} child={} promoted={} champion_after={} proposal={} assessment={}",
            generation.payload.generation_index,
            generation.payload.champion_before,
            generation.payload.child_genome_id,
            generation.payload.promoted,
            generation.payload.champion_after,
            generation.payload.proposal_id,
            generation.payload.assessment_id,
        )
    }));
    lines.join("\n")
}

fn meta_strategy_human(strategy: &MetaStrategyRecord) -> String {
    format!(
        "strategy={} name={} mutation_prioritization={:?} generations={} experiment_allocation={} candidate_count={} gene_selection={:?}",
        strategy.strategy_id,
        strategy.config.name,
        strategy.config.mutation_prioritization,
        strategy.config.generation_count,
        strategy.config.experiment_allocation,
        strategy.config.candidate_count,
        strategy.config.gene_selection,
    )
}

#[allow(clippy::cast_precision_loss)]
fn meta_interval_human(label: &str, interval: &MetaBootstrapInterval) -> String {
    format!(
        "{label}: estimate={:.4} interval=[{:.4}, {:.4}]",
        interval.estimate_x10000 as f64 / 10_000.0,
        interval.lower_x10000 as f64 / 10_000.0,
        interval.upper_x10000 as f64 / 10_000.0,
    )
}

fn meta_lineage_human(outcome: &MetaLineageOutcome) -> String {
    format!(
        "  lineage world={} from={} a[run={} champion={} promotions={} trials={}] b[run={} champion={} promotions={} trials={}]",
        outcome.world_id,
        outcome.from_genome_id,
        outcome.strategy_a_run_id,
        outcome.strategy_a_champion_genome_id,
        outcome.strategy_a_promotions,
        outcome.strategy_a_trials_consumed,
        outcome.strategy_b_run_id,
        outcome.strategy_b_champion_genome_id,
        outcome.strategy_b_promotions,
        outcome.strategy_b_trials_consumed,
    )
}

fn meta_receipt_human(receipt: &MetaReceiptRecord) -> String {
    let mut lines = vec![format!(
        "meta_run={} strategy_a={} strategy_b={} confidence_bps={} algorithm={}",
        receipt.payload.meta_run_id,
        receipt.payload.strategy_a_id,
        receipt.payload.strategy_b_id,
        receipt.payload.confidence_bps,
        receipt.payload.algorithm,
    )];
    lines.push(meta_interval_human(
        "quality_delta (b-a promotions)",
        &receipt.payload.quality_delta,
    ));
    lines.push(meta_interval_human(
        "cost_delta (b-a trials)",
        &receipt.payload.cost_delta,
    ));
    lines.extend(receipt.payload.lineages.iter().map(meta_lineage_human));
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

fn run_list_entry_human(run: &RunListEntry) -> String {
    format!(
        "run={} job={} genome={} world={} state={:?} reason={} latency_ms={} cost_microusd={}",
        run.run_id,
        run.job_id.as_deref().unwrap_or("none"),
        run.genome_id,
        run.world_id.as_deref().unwrap_or("unknown"),
        run.state,
        run.completion_reason
            .map_or_else(|| "pending".to_owned(), |reason| format!("{reason:?}")),
        run.latency_millis
            .map_or_else(|| "pending".to_owned(), |value| value.to_string()),
        run.actual_cost_microusd
            .map_or_else(|| "pending".to_owned(), |value| value.to_string()),
    )
}

fn evaluation_list_entry_human(entry: &EvaluationListEntry) -> String {
    let mut line = evaluation_human(&entry.evaluation);
    if let Some(selection) = &entry.selection {
        let _ = write!(
            line,
            " selection_metrics_eligible={} estimate_bps={} lower_bps={} upper_bps={} parent_cost={} candidate_cost={} parent_latency_ms={} candidate_latency_ms={}",
            selection.metrics_eligible,
            selection.estimate_bps,
            selection.lower_bps,
            selection.upper_bps,
            selection.parent_cost_microusd,
            selection.candidate_cost_microusd,
            selection.parent_latency_millis,
            selection.candidate_latency_millis,
        );
    }
    if let Some(invariants) = &entry.invariants {
        let _ = write!(
            line,
            " invariant_checks={} invariant_violations={} invariant_regressions={}/{} contract_satisfied={}",
            invariants.total_checks,
            invariants.total_candidate_violations,
            invariants.total_paired_regressions,
            invariants.maximum_regressions,
            invariants.candidate_contract_satisfied,
        );
    }
    if let Some(assessment) = &entry.forge_assessment {
        let _ = write!(
            line,
            " forge_assessment={} outcome={:?}",
            assessment.assessment_id, assessment.outcome
        );
    }
    if !entry.champion_transition_ids.is_empty() {
        let _ = write!(
            line,
            " champion_transitions={}",
            entry.champion_transition_ids.join(",")
        );
    }
    line
}

fn denial_human(denial: &DenialEntry) -> String {
    format!(
        "kind={:?} timestamp_ms={} request={} command={} run={} genome={} world={} client={}",
        denial.kind,
        denial.timestamp_millis,
        denial.request_id.as_deref().unwrap_or("none"),
        denial.command.as_deref().unwrap_or("none"),
        denial.run_id.as_deref().unwrap_or("none"),
        denial.genome_id.as_deref().unwrap_or("none"),
        denial.world_id.as_deref().unwrap_or("none"),
        denial.client_id.as_deref().unwrap_or("none"),
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

fn forge_analysis_human(analysis: &ForgeAnalysisRecord) -> String {
    let clusters = analysis
        .analysis
        .clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| {
            let mutation = cluster
                .suggested_mutation
                .map_or_else(|| "none".to_owned(), |mutation| format!("{mutation:?}"));
            format!(
                "[{index}]{}:visible={},sealed={},mutation={},hypothesis={:?}",
                cluster.signature,
                cluster.visible_count,
                cluster.sealed_count,
                mutation,
                cluster.hypothesis
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "analysis={} evaluation={} world={} visible_failed={} sealed_failed={} clusters=[{}] event={} sequence={} hash={}",
        analysis.analysis.analysis_id,
        analysis.analysis.evaluation_id,
        analysis.analysis.world_id,
        analysis.analysis.total_visible_failed_trials,
        analysis.analysis.total_sealed_failed_trials,
        clusters,
        analysis.event.event_id,
        analysis.event.sequence,
        analysis.event.event_hash,
    )
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use std::{fs, path::Path, time::SystemTime};

    use super::{
        Arguments, command_from_cli, copy_fixture_into_new_destination, evaluation_human,
        fixture_source_dir, gauntlet_coding_fixture_dir, packaged_tui_paths,
    };
    use hephaestus_control::{
        Command, DriftKind, EvaluationEventRecord, EvaluationRecord, MetaLineageSpec,
    };

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
    fn drift_commands_map_flags_to_authenticated_commands() {
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
                "drift",
                "record",
                "drift-1",
                "--world",
                "world-1",
                "--kind",
                "latency",
                "--evidence",
                "evaluation-1"
            ]),
            Command::DriftRecord {
                drift_id: "drift-1".to_owned(),
                world_id: "world-1".to_owned(),
                kind: DriftKind::Latency,
                evidence_evaluation_id: "evaluation-1".to_owned(),
            }
        );
        assert_eq!(
            parse(&["hephaestus", "drift", "show", "drift-1"]),
            Command::DriftShow {
                drift_id: "drift-1".to_owned(),
            }
        );
    }

    #[test]
    fn canary_commands_map_flags_to_authenticated_commands() {
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
                "canary",
                "start",
                "canary-1",
                "--world",
                "world-1",
                "--candidate",
                "genome-2",
                "--assessment",
                "assessment-1"
            ]),
            Command::CanaryStart {
                canary_id: "canary-1".to_owned(),
                world_id: "world-1".to_owned(),
                candidate_genome_id: "genome-2".to_owned(),
                assessment_id: "assessment-1".to_owned(),
            }
        );
        assert_eq!(
            parse(&[
                "hephaestus",
                "canary",
                "advance",
                "canary-1",
                "--evidence",
                "evaluation-1"
            ]),
            Command::CanaryAdvance {
                canary_id: "canary-1".to_owned(),
                evidence_evaluation_id: "evaluation-1".to_owned(),
            }
        );
        assert_eq!(
            parse(&[
                "hephaestus",
                "canary",
                "live-check",
                "canary-1",
                "--evidence",
                "evaluation-2"
            ]),
            Command::CanaryLiveCheck {
                canary_id: "canary-1".to_owned(),
                evidence_evaluation_id: "evaluation-2".to_owned(),
            }
        );
        assert_eq!(
            parse(&["hephaestus", "canary", "show", "canary-1"]),
            Command::CanaryShow {
                canary_id: "canary-1".to_owned(),
            }
        );
    }

    #[test]
    fn evolve_commands_map_flags_to_authenticated_commands() {
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
                "evolve",
                "start",
                "run-1",
                "--world",
                "world-1",
                "--from",
                "genome-1",
                "--generations",
                "3",
                "--budget",
                "6"
            ]),
            Command::EvolveStart {
                run_id: "run-1".to_owned(),
                world_id: "world-1".to_owned(),
                from_genome_id: "genome-1".to_owned(),
                generations: 3,
                budget: 6,
            }
        );
        assert_eq!(
            parse(&["hephaestus", "evolve", "status", "run-1"]),
            Command::EvolveStatus {
                run_id: "run-1".to_owned(),
            }
        );
        assert_eq!(
            parse(&["hephaestus", "evolve", "cancel", "run-1"]),
            Command::EvolveCancel {
                run_id: "run-1".to_owned(),
            }
        );
    }

    #[test]
    fn evolve_coding_is_a_local_convenience_command_with_its_own_fixture() {
        let command =
            Arguments::try_parse_from(["hephaestus", "evolve", "coding", "--budget", "6"])
                .expect("CLI parses")
                .command;
        assert!(matches!(
            command_from_cli(command),
            Err("evolve coding is a local convenience command")
        ));
        let fixture = gauntlet_coding_fixture_dir().expect("bundled fixture resolves");
        for name in [
            "world.template.json",
            "parent.md",
            "candidate.md",
            "invariants.json",
            "tasks/visible.json",
            "tasks/sealed.json",
        ] {
            assert!(
                fixture.join(name).is_file(),
                "gauntlet coding fixture is missing {name}"
            );
        }
    }

    #[test]
    fn meta_commands_map_flags_to_authenticated_commands() {
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
                "meta",
                "evaluate",
                "meta-run-1",
                "--strategy-a",
                "strategy-a",
                "--strategy-b",
                "strategy-b",
                "--lineage-world",
                "world-1",
                "--lineage-genome",
                "genome-1",
                "--lineage-world",
                "world-2",
                "--lineage-genome",
                "genome-2",
                "--lineages",
                "2",
                "--confidence-bps",
                "9000",
                "--seed",
                "7",
            ]),
            Command::MetaEvaluate {
                meta_run_id: "meta-run-1".to_owned(),
                strategy_a_id: "strategy-a".to_owned(),
                strategy_b_id: "strategy-b".to_owned(),
                lineages: vec![
                    MetaLineageSpec {
                        world_id: "world-1".to_owned(),
                        from_genome_id: "genome-1".to_owned(),
                    },
                    MetaLineageSpec {
                        world_id: "world-2".to_owned(),
                        from_genome_id: "genome-2".to_owned(),
                    },
                ],
                confidence_bps: 9_000,
                bootstrap_seed: 7,
            }
        );
        assert_eq!(
            parse(&["hephaestus", "meta", "show", "meta-run-1"]),
            Command::MetaShow {
                meta_run_id: "meta-run-1".to_owned(),
            }
        );
        assert_eq!(
            parse(&["hephaestus", "meta", "strategy", "list"]),
            Command::MetaStrategyList
        );
        assert_eq!(
            parse(&["hephaestus", "meta", "strategy", "show", "strategy-a"]),
            Command::MetaStrategyShow {
                strategy_id: "strategy-a".to_owned(),
            }
        );
    }

    #[test]
    fn meta_evaluate_rejects_a_lineage_count_mismatch() {
        let arguments = Arguments::try_parse_from([
            "hephaestus",
            "meta",
            "evaluate",
            "meta-run-1",
            "--strategy-a",
            "strategy-a",
            "--strategy-b",
            "strategy-b",
            "--lineage-world",
            "world-1",
            "--lineage-genome",
            "genome-1",
            "--lineages",
            "2",
        ])
        .expect("CLI parses");
        assert!(command_from_cli(arguments.command).is_err());
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
    fn evidence_list_commands_default_and_accept_a_bounded_limit() {
        let default_runs = Arguments::try_parse_from(["hephaestus", "runs"]).expect("CLI parses");
        assert_eq!(
            command_from_cli(default_runs.command).expect("command maps"),
            Command::RunList { limit: 20 }
        );

        let explicit_evaluations =
            Arguments::try_parse_from(["hephaestus", "evaluations", "--limit", "5"])
                .expect("CLI parses");
        assert_eq!(
            command_from_cli(explicit_evaluations.command).expect("command maps"),
            Command::EvaluationList { limit: 5 }
        );

        let denials = Arguments::try_parse_from(["hephaestus", "denials", "--limit", "200"])
            .expect("CLI parses");
        assert_eq!(
            command_from_cli(denials.command).expect("command maps"),
            Command::DenialList { limit: 200 }
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
