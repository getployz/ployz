//! Bounded local protocol for point-in-time Keeper handshake observations.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::corrosion::CorrosionTimestamp;
use ployz_core::network::WireGuardPublicKey;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::watch;

pub(crate) const MAX_CONTROL_MESSAGE_BYTES: usize = 4 * 1024;
pub(crate) const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HandshakeControlRequest {
    ObserveHandshake { public_key: WireGuardPublicKey },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum HandshakeControlResponse {
    Observed {
        observed_at: CorrosionTimestamp,
        age_seconds: Option<u64>,
    },
    Unavailable {
        reason: HandshakeControlUnavailable,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HandshakeControlUnavailable {
    PeerAbsent,
    UnsupportedProvider,
    ProviderTimedOut,
    ProviderFailed,
    Protocol,
}

pub(crate) async fn request(
    socket_path: &Path,
    request: &HandshakeControlRequest,
    mut shutdown: watch::Receiver<bool>,
) -> Result<HandshakeControlResponse, HandshakeControlProtocolError> {
    if *shutdown.borrow() {
        return Err(HandshakeControlProtocolError::Shutdown);
    }
    tokio::select! {
        result = tokio::time::timeout(
            CONTROL_REQUEST_TIMEOUT,
            request_inner(socket_path, request),
        ) => result.map_err(|_| HandshakeControlProtocolError::Deadline {
            timeout: CONTROL_REQUEST_TIMEOUT,
        })?,
        () = wait_for_shutdown(&mut shutdown) => Err(HandshakeControlProtocolError::Shutdown),
    }
}

async fn request_inner(
    socket_path: &Path,
    request: &HandshakeControlRequest,
) -> Result<HandshakeControlResponse, HandshakeControlProtocolError> {
    let mut stream = UnixStream::connect(socket_path).await.map_err(|source| {
        HandshakeControlProtocolError::Connect {
            path: socket_path.to_path_buf(),
            message: source.to_string(),
        }
    })?;
    write_message(&mut stream, request).await?;
    stream
        .shutdown()
        .await
        .map_err(|source| HandshakeControlProtocolError::Write {
            message: source.to_string(),
        })?;
    read_message(&mut stream).await
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() || shutdown.changed().await.is_err() {
            return;
        }
    }
}

pub(crate) async fn read_request<Reader>(
    reader: &mut Reader,
) -> Result<HandshakeControlRequest, HandshakeControlProtocolError>
where
    Reader: AsyncRead + Unpin,
{
    read_message(reader).await
}

pub(crate) async fn write_response<Writer>(
    writer: &mut Writer,
    response: &HandshakeControlResponse,
) -> Result<(), HandshakeControlProtocolError>
where
    Writer: AsyncWrite + Unpin,
{
    write_message(writer, response).await
}

async fn write_message<Writer, Message>(
    writer: &mut Writer,
    message: &Message,
) -> Result<(), HandshakeControlProtocolError>
where
    Writer: AsyncWrite + Unpin,
    Message: Serialize,
{
    let bytes =
        serde_json::to_vec(message).map_err(|error| HandshakeControlProtocolError::Encode {
            message: error.to_string(),
        })?;
    if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err(HandshakeControlProtocolError::TooLarge {
            limit: MAX_CONTROL_MESSAGE_BYTES,
        });
    }
    writer
        .write_all(&bytes)
        .await
        .map_err(|source| HandshakeControlProtocolError::Write {
            message: source.to_string(),
        })
}

async fn read_message<Reader, Message>(
    reader: &mut Reader,
) -> Result<Message, HandshakeControlProtocolError>
where
    Reader: AsyncRead + Unpin,
    Message: for<'de> Deserialize<'de>,
{
    let mut bytes = Vec::new();
    reader
        .take((MAX_CONTROL_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| HandshakeControlProtocolError::Read {
            message: source.to_string(),
        })?;
    if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err(HandshakeControlProtocolError::TooLarge {
            limit: MAX_CONTROL_MESSAGE_BYTES,
        });
    }
    if bytes.is_empty() {
        return Err(HandshakeControlProtocolError::EmptyMessage);
    }
    serde_json::from_slice(&bytes).map_err(|error| HandshakeControlProtocolError::Decode {
        message: error.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HandshakeControlProtocolError {
    #[error("could not connect to Keeper control socket {path}: {message}")]
    Connect { path: PathBuf, message: String },
    #[error("could not read Keeper control protocol message: {message}")]
    Read { message: String },
    #[error("could not write Keeper control protocol message: {message}")]
    Write { message: String },
    #[error("could not encode Keeper control protocol message: {message}")]
    Encode { message: String },
    #[error("could not decode Keeper control protocol message: {message}")]
    Decode { message: String },
    #[error("Keeper control protocol message is empty")]
    EmptyMessage,
    #[error("Keeper control protocol message exceeds {limit} bytes")]
    TooLarge { limit: usize },
    #[error("Keeper control request timed out after {timeout:?}")]
    Deadline { timeout: Duration },
    #[error("Keeper control request was interrupted by shutdown")]
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> WireGuardPublicKey {
        WireGuardPublicKey::try_new("BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=")
            .expect("public key")
    }

    #[tokio::test]
    async fn oversized_and_malformed_messages_are_rejected() {
        let (mut oversized_writer, mut oversized_reader) = tokio::io::duplex(8 * 1024);
        let oversized = tokio::spawn(async move {
            oversized_writer
                .write_all(&vec![b'x'; MAX_CONTROL_MESSAGE_BYTES + 1])
                .await
                .expect("write oversized request");
        });
        assert_eq!(
            read_request(&mut oversized_reader).await,
            Err(HandshakeControlProtocolError::TooLarge {
                limit: MAX_CONTROL_MESSAGE_BYTES,
            })
        );
        oversized.await.expect("writer task");

        let (mut malformed_writer, mut malformed_reader) = tokio::io::duplex(256);
        malformed_writer
            .write_all(b"{not-json}")
            .await
            .expect("write malformed request");
        malformed_writer.shutdown().await.expect("finish request");
        assert!(matches!(
            read_request(&mut malformed_reader).await,
            Err(HandshakeControlProtocolError::Decode { .. })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn client_deadline_and_shutdown_are_bounded() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("keeper-control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("listener");
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("client");
            std::future::pending::<()>().await;
        });
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let client = tokio::spawn({
            let socket = socket.clone();
            async move {
                request(
                    &socket,
                    &HandshakeControlRequest::ObserveHandshake { public_key: key() },
                    shutdown_rx,
                )
                .await
            }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(CONTROL_REQUEST_TIMEOUT).await;
        assert!(matches!(
            client.await.expect("client task"),
            Err(HandshakeControlProtocolError::Deadline { timeout })
                if timeout == CONTROL_REQUEST_TIMEOUT
        ));
        server.abort();

        let missing = directory.path().join("missing.sock");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        shutdown_tx.send(true).expect("shutdown");
        assert_eq!(
            request(
                &missing,
                &HandshakeControlRequest::ObserveHandshake { public_key: key() },
                shutdown_rx,
            )
            .await,
            Err(HandshakeControlProtocolError::Shutdown)
        );
    }
}
