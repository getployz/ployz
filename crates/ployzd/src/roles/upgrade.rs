//! Small local protocol between the unprivileged API role and root Keeper.
//!
//! The API role can put verified candidate bytes in the shared artifact store,
//! but only Keeper is allowed to change the stable executable links or ask the
//! supervisor to restart a role.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::install::InstallSha256Digest;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

const MAX_MESSAGE_BYTES: usize = 4 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One root-only Keeper effect requested by the local API role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum UpgradeRequest {
    Arm { sha256: InstallSha256Digest },
    Commit,
}

/// The Keeper's bounded outcome for an upgrade effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum UpgradeResponse {
    Armed,
    Committed,
    Refused { message: String },
}

/// Sends one complete request and waits for its complete response.
pub(crate) async fn request(
    socket_path: &Path,
    request: &UpgradeRequest,
) -> Result<UpgradeResponse, UpgradeProtocolError> {
    tokio::time::timeout(REQUEST_TIMEOUT, request_inner(socket_path, request))
        .await
        .map_err(|_| UpgradeProtocolError::Deadline {
            timeout: REQUEST_TIMEOUT,
        })?
}

async fn request_inner(
    socket_path: &Path,
    request: &UpgradeRequest,
) -> Result<UpgradeResponse, UpgradeProtocolError> {
    let mut stream =
        UnixStream::connect(socket_path)
            .await
            .map_err(|source| UpgradeProtocolError::Connect {
                path: socket_path.to_path_buf(),
                message: source.to_string(),
            })?;
    write_message(&mut stream, request).await?;
    stream
        .shutdown()
        .await
        .map_err(|source| UpgradeProtocolError::Write {
            message: source.to_string(),
        })?;
    read_message(&mut stream).await
}

pub(crate) async fn read_request(
    stream: &mut UnixStream,
) -> Result<UpgradeRequest, UpgradeProtocolError> {
    read_message(stream).await
}

pub(crate) async fn write_response(
    stream: &mut UnixStream,
    response: &UpgradeResponse,
) -> Result<(), UpgradeProtocolError> {
    write_message(stream, response).await
}

async fn write_message<Message>(
    stream: &mut UnixStream,
    message: &Message,
) -> Result<(), UpgradeProtocolError>
where
    Message: Serialize,
{
    let bytes = serde_json::to_vec(message).map_err(|error| UpgradeProtocolError::Encode {
        message: error.to_string(),
    })?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(UpgradeProtocolError::TooLarge {
            limit: MAX_MESSAGE_BYTES,
        });
    }
    stream
        .write_all(&bytes)
        .await
        .map_err(|source| UpgradeProtocolError::Write {
            message: source.to_string(),
        })
}

async fn read_message<Message>(stream: &mut UnixStream) -> Result<Message, UpgradeProtocolError>
where
    Message: for<'de> Deserialize<'de>,
{
    let mut bytes = Vec::new();
    stream
        .take((MAX_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| UpgradeProtocolError::Read {
            message: source.to_string(),
        })?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(UpgradeProtocolError::TooLarge {
            limit: MAX_MESSAGE_BYTES,
        });
    }
    if bytes.is_empty() {
        return Err(UpgradeProtocolError::EmptyMessage);
    }
    serde_json::from_slice(&bytes).map_err(|error| UpgradeProtocolError::Decode {
        message: error.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UpgradeProtocolError {
    #[error("could not connect to Keeper upgrade socket {path}: {message}")]
    Connect { path: PathBuf, message: String },
    #[error("could not read Keeper upgrade protocol message: {message}")]
    Read { message: String },
    #[error("could not write Keeper upgrade protocol message: {message}")]
    Write { message: String },
    #[error("could not encode Keeper upgrade protocol message: {message}")]
    Encode { message: String },
    #[error("could not decode Keeper upgrade protocol message: {message}")]
    Decode { message: String },
    #[error("Keeper upgrade protocol message is empty")]
    EmptyMessage,
    #[error("Keeper upgrade protocol message exceeds {limit} bytes")]
    TooLarge { limit: usize },
    #[error("Keeper upgrade request timed out after {timeout:?}")]
    Deadline { timeout: Duration },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_request_round_trips_over_the_local_socket() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("keeper.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("socket listener");
        let digest = InstallSha256Digest::try_new("a".repeat(64)).expect("sha256");
        let expected = UpgradeRequest::Arm {
            sha256: digest.clone(),
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("one client");
            assert_eq!(read_request(&mut stream).await.expect("request"), expected);
            write_response(&mut stream, &UpgradeResponse::Armed)
                .await
                .expect("response");
        });

        assert_eq!(
            request(&socket, &UpgradeRequest::Arm { sha256: digest })
                .await
                .expect("round trip"),
            UpgradeResponse::Armed
        );
        server.await.expect("server task");
    }

    #[tokio::test(start_paused = true)]
    async fn request_has_a_fixed_deadline_when_keeper_stalls() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("keeper.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("socket listener");
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("one client");
            std::future::pending::<()>().await;
        });

        let request = tokio::spawn({
            let socket = socket.clone();
            async move { request(&socket, &UpgradeRequest::Commit).await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(REQUEST_TIMEOUT).await;

        assert!(matches!(
            request.await.expect("client task"),
            Err(UpgradeProtocolError::Deadline { timeout }) if timeout == REQUEST_TIMEOUT
        ));
        server.abort();
    }
}
