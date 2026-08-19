//! Shared Cloud Relay client. Callers pass Cloud Pairing `relayUrl` (`http` or `https`).

use std::{
    fmt, io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use http::{HeaderName, HeaderValue, StatusCode};
use ployz_core::MachineId;
use prost::Message;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::task::JoinHandle;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as WsMessage, client::IntoClientRequest, error::Error as WsError},
};
use url::Url;

use crate::{
    ATTACH_PATH, DIAL_PATH, HeldRegister, LIST_PATH, ListResponse, MACHINE_ID_HEADER,
    PAIRING_HEADER, REGISTER_PATH, REVOKE_PATH, RegisterRequest, Status, TUNNEL_ID_HEADER,
    TunnelFrame,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Failures talking to a Cloud Relay over the shared client.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Status(#[from] Status),
    #[error("{0}")]
    Transport(String),
}

impl ClientError {
    /// HTTP status when the Relay rejected the call before WebSocket upgrade.
    #[must_use]
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            Self::Status(status) => Some(status.status()),
            Self::Transport(_) => None,
        }
    }

    fn transport(error: impl ToString) -> Self {
        Self::Transport(error.to_string())
    }
}

/// HTTP/1.1 Cloud Relay client. One instance is shared by Register, Dial, List, and Revoke.
#[derive(Clone)]
pub struct RelayClient {
    base: Url,
    http: reqwest::Client,
}

/// WebSocket carrying prost messages. Dropping it ends the Register hold or tunnel.
pub struct RelayWs {
    inner: WsStream,
}

impl fmt::Debug for RelayWs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RelayWs")
    }
}

impl RelayClient {
    /// Build a client for this Cloud Pairing `relayUrl`.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Transport`] when `relay_url` is not `http` or `https`.
    pub fn new(relay_url: impl AsRef<str>) -> Result<Self, ClientError> {
        let base = parse_relay_url(relay_url.as_ref())?;
        let http = reqwest::Client::builder()
            .timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(ClientError::transport)?;
        Ok(Self { base, http })
    }

    /// Hold Register: send hello, receive Open/ping.
    ///
    /// # Errors
    ///
    /// Returns when the Relay rejects the bearer or the socket cannot be opened.
    pub async fn register(
        &self,
        bearer: &str,
        machine_id: &MachineId,
    ) -> Result<RelayWs, ClientError> {
        let mut ws = connect_ws(
            &self.base,
            REGISTER_PATH,
            nonempty(bearer),
            None,
            None,
            None,
        )
        .await?;
        ws.send(&RegisterRequest::new(machine_id)).await?;
        Ok(ws)
    }

    /// Dial a held Register slot.
    ///
    /// # Errors
    ///
    /// Returns when the Dial Credential, pairing, or Machine ID is rejected.
    pub async fn dial(
        &self,
        bearer: &str,
        pairing: &str,
        machine_id: &str,
    ) -> Result<RelayWs, ClientError> {
        connect_ws(
            &self.base,
            DIAL_PATH,
            nonempty(bearer),
            nonempty(pairing),
            nonempty(machine_id),
            None,
        )
        .await
    }

    /// Attach a tunnel opened on Register.
    ///
    /// # Errors
    ///
    /// Returns when the Tunnel ID is missing, invalid, or unknown.
    pub async fn attach(&self, tunnel_id: &str) -> Result<RelayWs, ClientError> {
        connect_ws(
            &self.base,
            ATTACH_PATH,
            None,
            None,
            None,
            nonempty(tunnel_id),
        )
        .await
    }

    /// List held Registers for one pairing.
    ///
    /// # Errors
    ///
    /// Returns when the Dial Credential or pairing is rejected.
    pub async fn list(
        &self,
        bearer: &str,
        pairing: &str,
    ) -> Result<Vec<HeldRegister>, ClientError> {
        self.post::<ListResponse>(LIST_PATH, bearer, pairing)
            .await
            .map(|response| response.registers().to_vec())
    }

    /// Revoke a Pairing Credential so later Register fails.
    ///
    /// # Errors
    ///
    /// Returns when the Dial Credential or pairing is rejected.
    pub async fn revoke(&self, bearer: &str, pairing: &str) -> Result<(), ClientError> {
        self.post::<crate::RevokeResponse>(REVOKE_PATH, bearer, pairing)
            .await
            .map(|_| ())
    }

    async fn post<T: Message + Default>(
        &self,
        path: &str,
        bearer: &str,
        pairing: &str,
    ) -> Result<T, ClientError> {
        let url = join_url(&self.base, path, false)?;
        let mut request = self.http.post(url).bearer_auth(bearer);
        if let Some(pairing) = nonempty(pairing) {
            request = request.header(PAIRING_HEADER, pairing);
        }
        let response = request.send().await.map_err(ClientError::transport)?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(ClientError::transport)?;
        if !status.is_success() {
            return Err(Status::from_http(status, &bytes).into());
        }
        T::decode(bytes.as_ref()).map_err(ClientError::transport)
    }
}

impl RelayWs {
    /// Send one prost message as a binary WebSocket frame.
    ///
    /// # Errors
    ///
    /// Returns when the socket is closed or the frame cannot be written.
    pub async fn send(&mut self, msg: &impl Message) -> Result<(), ClientError> {
        self.inner
            .send(WsMessage::Binary(msg.encode_to_vec().into()))
            .await
            .map_err(ClientError::transport)
    }

    /// Next prost message, or `None` when the socket closed.
    ///
    /// # Errors
    ///
    /// Returns when the frame cannot be read or decoded.
    pub async fn recv<T: Message + Default>(&mut self) -> Result<Option<T>, ClientError> {
        loop {
            match self.inner.next().await {
                None => return Ok(None),
                Some(Err(error)) => return Err(ClientError::transport(error)),
                Some(Ok(WsMessage::Binary(bytes))) => {
                    return T::decode(bytes.as_ref())
                        .map(Some)
                        .map_err(ClientError::transport);
                }
                Some(Ok(WsMessage::Ping(payload))) => {
                    let _ = self.inner.send(WsMessage::Pong(payload)).await;
                }
                Some(Ok(WsMessage::Pong(_) | WsMessage::Frame(_))) => {}
                Some(Ok(WsMessage::Close(_) | WsMessage::Text(_))) => return Ok(None),
            }
        }
    }

    /// Wrap remaining frames as a byte pipe for inner Machine RPC.
    #[must_use]
    pub fn into_io(self) -> TunnelIo {
        TunnelIo::from_ws(self.inner)
    }
}

/// Byte pipe over Dial or Attach `TunnelFrame`s.
pub struct TunnelIo {
    io: DuplexStream,
    splice: JoinHandle<()>,
}

impl TunnelIo {
    fn from_ws(ws: WsStream) -> Self {
        let (io, other) = tokio::io::duplex(64 * 1024);
        Self {
            io,
            splice: tokio::spawn(copy_ws_bytes(ws, other)),
        }
    }
}

impl Drop for TunnelIo {
    fn drop(&mut self) {
        self.splice.abort();
    }
}

impl AsyncRead for TunnelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(context, buffer)
    }
}

impl AsyncWrite for TunnelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(context)
    }
}

async fn copy_ws_bytes(ws: WsStream, io: DuplexStream) {
    let (mut sink, mut stream) = ws.split();
    let (mut reader, mut writer) = tokio::io::split(io);
    let to_relay = async {
        let mut buf = vec![0_u8; 16 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let Some(data) = buf.get(..n) else { break };
                    if sink
                        .send(WsMessage::Binary(
                            TunnelFrame::new(data.to_vec()).encode_to_vec().into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    };
    let from_relay = async {
        while let Some(Ok(message)) = stream.next().await {
            match message {
                WsMessage::Binary(bytes) => {
                    let Ok(frame) = TunnelFrame::decode(bytes.as_ref()) else {
                        break;
                    };
                    if writer.write_all(&frame.data).await.is_err() {
                        break;
                    }
                }
                WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => {}
                WsMessage::Close(_) | WsMessage::Text(_) => break,
            }
        }
        let _ = writer.shutdown().await;
    };
    tokio::join!(to_relay, from_relay);
}

async fn connect_ws(
    base: &Url,
    path: &str,
    bearer: Option<&str>,
    pairing: Option<&str>,
    machine_id: Option<&str>,
    tunnel_id: Option<&str>,
) -> Result<RelayWs, ClientError> {
    let url = join_url(base, path, true)?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(ClientError::transport)?;
    let headers = request.headers_mut();
    if let Some(bearer) = bearer {
        headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::try_from(format!("Bearer {bearer}")).map_err(ClientError::transport)?,
        );
    }
    set_header(headers, PAIRING_HEADER, pairing)?;
    set_header(headers, MACHINE_ID_HEADER, machine_id)?;
    set_header(headers, TUNNEL_ID_HEADER, tunnel_id)?;
    let result = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| ClientError::transport("relay connect timed out"))?;
    match result {
        Ok((ws, _)) => Ok(RelayWs { inner: ws }),
        Err(WsError::Http(response)) => Err(Status::from_http(
            response.status(),
            response.body().as_deref().unwrap_or(&[]),
        )
        .into()),
        Err(error) => Err(ClientError::transport(error)),
    }
}

fn set_header(
    headers: &mut http::HeaderMap,
    name: &'static str,
    value: Option<&str>,
) -> Result<(), ClientError> {
    let Some(value) = value else {
        return Ok(());
    };
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::try_from(value).map_err(ClientError::transport)?,
    );
    Ok(())
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn join_url(base: &Url, path: &str, websocket: bool) -> Result<Url, ClientError> {
    let mut url = base
        .join(path.trim_start_matches('/'))
        .map_err(ClientError::transport)?;
    if websocket {
        let scheme = match url.scheme() {
            "https" => "wss",
            "http" => "ws",
            _ => return Err(ClientError::transport("relayUrl must be http or https")),
        };
        url.set_scheme(scheme)
            .map_err(|()| ClientError::transport("relayUrl must be http or https"))?;
    }
    Ok(url)
}

fn parse_relay_url(relay_url: &str) -> Result<Url, ClientError> {
    let url = Url::parse(relay_url).map_err(ClientError::transport)?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        _ => Err(ClientError::transport("relayUrl must be http or https")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_relay_url_maps_to_wss_and_keeps_https_for_post() {
        let base = parse_relay_url("https://relay.ployz.dev").unwrap();
        assert_eq!(
            join_url(&base, REGISTER_PATH, true).unwrap().as_str(),
            "wss://relay.ployz.dev/register"
        );
        assert_eq!(
            join_url(&base, LIST_PATH, false).unwrap().as_str(),
            "https://relay.ployz.dev/list"
        );
    }

    #[test]
    fn http_relay_url_maps_to_ws() {
        let base = parse_relay_url("http://127.0.0.1:8080").unwrap();
        assert_eq!(
            join_url(&base, REGISTER_PATH, true).unwrap().as_str(),
            "ws://127.0.0.1:8080/register"
        );
        let base = parse_relay_url("http://127.0.0.1:8080/").unwrap();
        assert_eq!(
            join_url(&base, DIAL_PATH, true).unwrap().as_str(),
            "ws://127.0.0.1:8080/dial"
        );
    }

    #[test]
    fn other_schemes_are_rejected() {
        assert!(RelayClient::new("ws://relay.example").is_err());
        assert!(RelayClient::new("not-a-url").is_err());
    }
}
