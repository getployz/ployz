use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use mvp_node::{NodeError, NodeResult};

pub(crate) fn daemon_status(args: &[String]) -> NodeResult<String> {
    let control_socket = parse_control_socket_only(args)?;
    daemon_control_request(control_socket, b"status\n").map(|response| response.trim().to_string())
}

pub(crate) fn daemon_control_request(
    control_socket: PathBuf,
    request: &[u8],
) -> NodeResult<String> {
    let mut stream =
        UnixStream::connect(&control_socket).map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "connect",
            source,
        })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "set status read timeout",
            source,
        })?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "set status write timeout",
            source,
        })?;
    stream
        .write_all(request)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "write status request",
            source,
        })?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket.clone(),
            operation: "finish status request",
            source,
        })?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: control_socket,
            operation: "read status response",
            source,
        })?;
    Ok(response)
}

pub(crate) fn parse_control_socket_only(args: &[String]) -> NodeResult<PathBuf> {
    let mut control_socket = None;
    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--control" => {
                let Some(value) = remaining.next() else {
                    return Err(NodeError::MissingFlagValue { flag: "--control" });
                };
                control_socket = Some(PathBuf::from(value));
            }
            other => {
                return Err(NodeError::UnknownArgument {
                    argument: other.to_string(),
                });
            }
        }
    }
    control_socket.ok_or(NodeError::MissingFlagValue { flag: "--control" })
}
