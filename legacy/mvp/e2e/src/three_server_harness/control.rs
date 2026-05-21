use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::thread;
use std::time::Instant;

use super::{POLL, ROLE_WAIT_TIMEOUT};

pub(super) fn unix_json_request(
    socket: &Path,
    request: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let bytes = serde_json::to_vec(request).map_err(|error| format!("encode request: {error}"))?;
    let mut stream = connect_unix(socket)?;
    stream
        .write_all(&bytes)
        .map_err(|error| format!("write '{}': {error}", socket.display()))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("shutdown write '{}': {error}", socket.display()))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|error| format!("read '{}': {error}", socket.display()))?;
    serde_json::from_slice(&response).map_err(|error| {
        format!(
            "decode response from '{}': {error}; body={}",
            socket.display(),
            String::from_utf8_lossy(&response)
        )
    })
}

fn connect_unix(socket: &Path) -> Result<UnixStream, String> {
    let deadline = Instant::now() + ROLE_WAIT_TIMEOUT;
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(format!("connect '{}': {error}", socket.display()));
                }
                thread::sleep(POLL);
            }
        }
    }
}
