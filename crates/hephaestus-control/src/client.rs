use std::{
    fs,
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{API_VERSION, ApiRequest, ApiResponse, Command, ControlError};

// A 1 MiB UTF-8 Genome prompt can expand to six bytes per byte when JSON
// escapes control characters. Leave bounded space for the response envelope.
const MAX_RESPONSE_BYTES: usize = 7 * 1_048_576;
const MAX_RESPONSE_READ_BYTES: u64 = 7 * 1_048_576 + 1;

/// Authenticated client for the daemon's owner-only Unix socket.
pub struct Client {
    data_dir: PathBuf,
}

impl Client {
    /// Creates a client rooted at the daemon's canonical data directory.
    #[must_use]
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Sends one typed command and returns its typed response.
    ///
    /// # Errors
    ///
    /// Fails when the token or socket cannot be read, transport bounds are exceeded,
    /// or the daemon returns malformed protocol JSON.
    pub fn request(&self, command: Command) -> Result<ApiResponse, ControlError> {
        let token = read_token(&self.data_dir.join("operator.token"))?;
        let request_id = format!(
            "cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| ControlError::Protocol("system clock precedes Unix epoch"))?
                .as_nanos()
        );
        let request = ApiRequest {
            version: API_VERSION,
            request_id,
            token,
            command,
        };
        let encoded = serde_json::to_vec(&request)?;
        let mut stream = UnixStream::connect(self.data_dir.join("control.sock"))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(&encoded)?;
        stream.shutdown(Shutdown::Write)?;

        let mut bytes = Vec::new();
        stream
            .take(MAX_RESPONSE_READ_BYTES)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(ControlError::Protocol("daemon response exceeds limit"));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}

fn read_token(path: &Path) -> Result<String, ControlError> {
    let token = fs::read_to_string(path)?;
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ControlError::Protocol("operator token is malformed"));
    }
    Ok(token)
}
