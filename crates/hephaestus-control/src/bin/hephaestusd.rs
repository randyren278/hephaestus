use std::{path::PathBuf, process::ExitCode};

use clap::Parser;
use hephaestus_control::{ControlPlane, data_dir_from_environment};

#[derive(Parser)]
#[command(name = "hephaestusd", about = "Hephaestus canonical local daemon")]
struct Arguments {
    /// Canonical daemon data directory.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Git repository materialized into isolated reference-run worktrees.
    #[arg(long)]
    source_repository: Option<PathBuf>,
    /// Exact World-bound evaluator executable deployed beside the daemon by default.
    #[arg(long)]
    evaluator_executable: Option<PathBuf>,
    /// Bounded reference instruction worker deployed beside the daemon by default.
    #[arg(long)]
    reference_worker_executable: Option<PathBuf>,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    // Test-support builds only: latency-gated end-to-end tests give every
    // reference trial a fixed baseline so scheduling noise cannot cross the
    // canary's 20% regression gate. Release builds do not contain this.
    #[cfg(feature = "test-support")]
    if let Some(millis) = std::env::var("HEPHAESTUS_TEST_REFERENCE_BASELINE_DELAY_MILLIS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        hephaestus_runtime::set_test_reference_baseline_delay(millis);
    }
    let data_dir = match arguments
        .data_dir
        .map_or_else(data_dir_from_environment, Ok)
    {
        Ok(path) => path,
        Err(error) => {
            eprintln!("hephaestusd: {error}");
            return ExitCode::FAILURE;
        }
    };
    let source_repository = match arguments
        .source_repository
        .map_or_else(std::env::current_dir, Ok)
    {
        Ok(path) => path,
        Err(error) => {
            eprintln!("hephaestusd: {error}");
            return ExitCode::FAILURE;
        }
    };
    let opened = match (
        arguments.evaluator_executable,
        arguments.reference_worker_executable,
    ) {
        (Some(evaluator), Some(worker)) => {
            ControlPlane::open_with_repository_evaluator_and_reference_worker(
                data_dir,
                source_repository,
                evaluator,
                worker,
            )
        }
        (Some(evaluator), None) => {
            ControlPlane::open_with_repository_and_evaluator(data_dir, source_repository, evaluator)
        }
        (None, Some(worker)) => ControlPlane::open_with_repository_and_reference_worker(
            data_dir,
            source_repository,
            worker,
        ),
        (None, None) => ControlPlane::open_with_repository(data_dir, source_repository),
    };
    match opened.and_then(ControlPlane::serve) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hephaestusd: {error}");
            ExitCode::FAILURE
        }
    }
}
