use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--hold-worker-group")) {
        return match hephaestus_runtime::hold_process_group_anchor() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }
    match hephaestus_runtime::run_process_guardian() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
