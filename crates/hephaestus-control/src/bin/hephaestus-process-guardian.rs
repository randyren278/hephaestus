use std::process::ExitCode;

fn main() -> ExitCode {
    match hephaestus_runtime::run_process_guardian() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
