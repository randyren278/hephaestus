use std::{
    io::{self, Read, Write},
    process::ExitCode,
};

use hephaestus_runtime::execute_reference_worker_request;

fn main() -> ExitCode {
    let mut request = Vec::new();
    if io::stdin()
        .take(1_048_590)
        .read_to_end(&mut request)
        .is_err()
    {
        return ExitCode::FAILURE;
    }
    let Ok(output) = execute_reference_worker_request(&request) else {
        return ExitCode::FAILURE;
    };
    if io::stdout().lock().write_all(&output).is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
