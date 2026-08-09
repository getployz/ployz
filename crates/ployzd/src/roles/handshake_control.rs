//! Bounded local protocol for point-in-time Keeper handshake observations.

use ployz_core::corrosion::CorrosionTimestamp;
use ployz_core::network::WireGuardPublicKey;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub(crate) const MAX_CONTROL_MESSAGE_BYTES: usize = 4 * 1024;

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
