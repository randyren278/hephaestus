//! Reference implementation of an authenticated remote worker.
//!
//! Connects to the daemon's dedicated `worker.sock` using a scoped, expiring
//! credential minted by the operator (`hephaestus worker credential-mint`),
//! leases one job at a time, executes it with the exact same isolated
//! sandboxed transform the local `hephaestus-reference-worker` binary uses
//! (`execute_reference_worker_request`), and returns the result for the
//! daemon to sign and record. See docs/REMOTE_WORKERS.md.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::ExitCode,
    thread,
    time::Duration,
};

use clap::Parser;
use hephaestus_control::{RemoteCompletion, WorkerReply, WorkerRequest, data_dir_from_environment};
use hephaestus_runtime::execute_reference_worker_request;

#[derive(Parser)]
#[command(
    name = "hephaestus-remote-worker",
    about = "Authenticated remote worker for the Hephaestus daemon"
)]
struct Arguments {
    /// Canonical daemon data directory whose `worker.sock` this worker connects to.
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Operator-chosen worker identity presenting the credential.
    #[arg(long)]
    worker_id: String,
    /// Raw credential secret, hex-encoded, minted by `hephaestus worker credential-mint`.
    /// Falls back to the `HEPHAESTUS_WORKER_TOKEN` environment variable.
    #[arg(long)]
    token: Option<String>,
    /// Exit after one lease attempt instead of polling forever. Used by tests.
    #[arg(long)]
    once: bool,
    /// Milliseconds to wait between lease attempts when no work is available.
    #[arg(long, default_value_t = 500)]
    poll_millis: u64,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let data_dir = match arguments.data_dir.clone().map_or_else(data_dir_from_environment, Ok) {
        Ok(path) => path,
        Err(_) => return ExitCode::FAILURE,
    };
    let Some(token) = arguments
        .token
        .clone()
        .or_else(|| std::env::var("HEPHAESTUS_WORKER_TOKEN").ok())
    else {
        eprintln!("hephaestus-remote-worker: --token or HEPHAESTUS_WORKER_TOKEN is required");
        return ExitCode::FAILURE;
    };
    let socket_path = data_dir.join("worker.sock");
    loop {
        let lease = send_worker_request(
            &socket_path,
            &WorkerRequest::Lease {
                worker_id: arguments.worker_id.clone(),
                token: token.clone(),
            },
        );
        match lease {
            Ok(WorkerReply::Leased {
                job_id, frame_hex, ..
            }) => {
                let outcome = decode_hex(&frame_hex)
                    .ok()
                    .and_then(|frame| execute_reference_worker_request(&frame).ok());
                let (output_hex, completion) = match outcome {
                    Some(output) => (encode_hex(&output), RemoteCompletion::Success),
                    None => (String::new(), RemoteCompletion::ProviderFailure),
                };
                let submit = WorkerRequest::SubmitResult {
                    worker_id: arguments.worker_id.clone(),
                    token: token.clone(),
                    job_id,
                    output_hex,
                    completion,
                };
                let _ = send_worker_request(&socket_path, &submit);
                if arguments.once {
                    return ExitCode::SUCCESS;
                }
            }
            Ok(WorkerReply::NoWork) => {
                if arguments.once {
                    return ExitCode::SUCCESS;
                }
            }
            Ok(WorkerReply::Error { reason }) => {
                eprintln!("hephaestus-remote-worker: {reason}");
                return ExitCode::FAILURE;
            }
            Ok(WorkerReply::ResultAccepted { .. }) | Err(_) => {
                if arguments.once {
                    return ExitCode::FAILURE;
                }
            }
        }
        if arguments.once {
            return ExitCode::SUCCESS;
        }
        thread::sleep(Duration::from_millis(arguments.poll_millis));
    }
}

fn send_worker_request(
    socket_path: &std::path::Path,
    request: &WorkerRequest,
) -> Result<WorkerReply, ()> {
    let encoded = serde_json::to_vec(request).map_err(|_| ())?;
    let mut stream = UnixStream::connect(socket_path).map_err(|_| ())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|_| ())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|_| ())?;
    stream.write_all(&encoded).map_err(|_| ())?;
    stream.shutdown(std::net::Shutdown::Write).map_err(|_| ())?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).map_err(|_| ())?;
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ()> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(());
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| ()))
        .collect()
}
