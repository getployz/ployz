//! In-process iroh management transport: serves the Machine API to the one
//! accepted client key, reachable through the Ployz Relay by Management Identity.

use std::{
    convert::Infallible,
    io,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use iroh::{
    Endpoint, RelayMode, RelayUrl, SecretKey,
    endpoint::{
        BindError, Connection, IdleTimeout, QuicTransportConfig, RecvStream, SendStream, VarInt,
        WeakConnectionHandle, presets,
    },
    tls::CaTlsConfig,
};
use ployz_core::{DEFAULT_RELAY_URL, MANAGEMENT_ALPN, MANAGEMENT_PORT};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite, Join, ReadBuf},
    sync::{Semaphore, mpsc, watch},
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::{
    body::Body,
    codegen::{Service, http},
    transport::{Server, server::Connected},
};

use crate::machine::{LocalMachine, LocalMachineRecord};

/// Application close code sent when the remote key is not the accepted client.
pub const REFUSED_BY_IDENTITY: VarInt = VarInt::from_u32(0x50);
/// Application close code sent to a live connection whose key was cleared.
pub const REVOKED: VarInt = VarInt::from_u32(0x51);

/// Authenticated endpoint confirmation that neither accepted nor pending client access remains.
pub const PAIRING_CLEARED: VarInt = VarInt::from_u32(0x52);

const MAX_CONCURRENT_HANDSHAKES: usize = 64;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the management endpoint binds and which relay it uses.
///
/// Production uses the defaults; tests override the port (`0`) and point at an
/// in-process relay, whose self-signed certificate needs a `relay_tls` of
/// `CaTlsConfig::insecure_skip_verify()` (iroh `test-utils` feature).
#[derive(Clone, Debug)]
pub struct ManagementConfig {
    /// The only relay used for management traffic.
    pub relay_url: RelayUrl,
    /// UDP listen port; zero asks the OS for an ephemeral port.
    pub port: u16,
    /// How the relay's HTTPS certificate is verified.
    pub relay_tls: CaTlsConfig,
}

impl Default for ManagementConfig {
    fn default() -> Self {
        Self {
            relay_url: DEFAULT_RELAY_URL
                .parse()
                .expect("DEFAULT_RELAY_URL is a valid relay URL"),
            port: MANAGEMENT_PORT,
            relay_tls: CaTlsConfig::default(),
        }
    }
}

/// The Machine's iroh secret key; its public half is the Management Identity.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ManagementSecret([u8; 32]);

impl ManagementSecret {
    /// Mint a Machine management identity using the OS random source.
    #[must_use]
    pub fn generate() -> Self {
        Self(SecretKey::generate().to_bytes())
    }

    /// Management Identity: the public key clients dial.
    #[must_use]
    pub fn public_key(&self) -> ployz_core::ManagementIdentity {
        ployz_core::ManagementIdentity::from_bytes(*self.secret_key().public().as_bytes())
    }

    fn secret_key(&self) -> SecretKey {
        SecretKey::from_bytes(&self.0)
    }
}

impl std::fmt::Debug for ManagementSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ManagementSecret(REDACTED)")
    }
}

/// Bind the management endpoint: one ALPN, no address lookup, only the configured relay.
///
/// # Errors
/// Returns the iroh bind error when the UDP port or crypto provider is unavailable.
pub async fn bind(
    secret: &ManagementSecret,
    config: &ManagementConfig,
) -> Result<Endpoint, BindError> {
    let transport = QuicTransportConfig::builder()
        .max_idle_timeout(Some(
            IdleTimeout::try_from(IDLE_TIMEOUT).expect("60s is within the idle timeout bounds"),
        ))
        .build();
    Endpoint::builder(presets::Minimal)
        .secret_key(secret.secret_key())
        .alpns(vec![MANAGEMENT_ALPN.to_vec()])
        .relay_mode(RelayMode::custom([config.relay_url.clone()]))
        .ca_tls_config(config.relay_tls.clone())
        .transport_config(transport)
        .bind_addr(format!("0.0.0.0:{}", config.port))
        .expect("literal IPv4 socket address")
        .bind_addr(format!("[::]:{}", config.port))
        .expect("literal IPv6 socket address")
        .bind()
        .await
}

/// Whether a handshake-completed remote key may reach the Machine API.
#[must_use]
pub fn admits(accepted_client: Option<&[u8; 32]>, remote: &[u8; 32]) -> bool {
    accepted_client == Some(remote)
}

/// Serve `api` over `endpoint` until `shutdown`.
///
/// Keys other than the accepted or pending key receive [`REFUSED_BY_IDENTITY`],
/// or [`PAIRING_CLEARED`] when no client keys remain. Authenticating
/// with the pending key permits read-only identity negotiation. Its first operational
/// RPC activates it. A Clear or replacement activation closes old connections with
/// [`REVOKED`].
///
/// # Errors
/// Returns the tonic transport error when the RPC server fails.
pub async fn serve<S>(
    endpoint: Endpoint,
    local: LocalMachine,
    api: S,
    shutdown: CancellationToken,
) -> io::Result<()>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    let mut records = local.owner().watch();
    let live: Arc<Mutex<Vec<WeakConnectionHandle>>> = Arc::default();
    let (accepted_tx, accepted_rx) = mpsc::channel::<io::Result<ManagementIo>>(16);
    let acceptor = tokio::spawn(accept_loop(
        endpoint.clone(),
        records.clone(),
        Arc::clone(&live),
        accepted_tx,
        shutdown.clone(),
    ));
    let revoker = tokio::spawn({
        let live = Arc::clone(&live);
        let shutdown = shutdown.clone();
        async move {
            loop {
                tokio::select! {
                    () = shutdown.cancelled() => break,
                    changed = records.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        revoke_others(&live, &records);
                    }
                }
            }
        }
    });
    let served = Server::builder()
        .serve_with_incoming_shutdown(
            api,
            ReceiverStream::new(accepted_rx),
            shutdown.cancelled_owned(),
        )
        .await;
    acceptor.abort();
    revoker.abort();
    endpoint.close().await;
    served.map_err(io::Error::other)
}

fn revoke_others(
    live: &Mutex<Vec<WeakConnectionHandle>>,
    records: &watch::Receiver<Arc<LocalMachineRecord>>,
) {
    let mut live = live.lock().expect("live connection list is not poisoned");
    let record = records.borrow();
    live.retain(|weak| {
        let Some(connection) = weak.upgrade() else {
            return false;
        };
        let keep = admits(
            record.accepted_client().as_ref(),
            connection.remote_id().as_bytes(),
        ) || admits(
            record.pending_client().as_ref(),
            connection.remote_id().as_bytes(),
        );
        if !keep {
            connection.close(REVOKED, b"revoked");
        }
        keep && connection.close_reason().is_none()
    });
}

async fn accept_loop(
    endpoint: Endpoint,
    records: watch::Receiver<Arc<LocalMachineRecord>>,
    live: Arc<Mutex<Vec<WeakConnectionHandle>>>,
    accepted: mpsc::Sender<io::Result<ManagementIo>>,
    shutdown: CancellationToken,
) {
    let handshakes = Arc::new(Semaphore::new(MAX_CONCURRENT_HANDSHAKES));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        let incoming = tokio::select! {
            () = shutdown.cancelled() => return,
            _ = tasks.join_next(), if !tasks.is_empty() => continue,
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => incoming,
                None => return,
            },
        };
        let Ok(permit) = Arc::clone(&handshakes).acquire_owned().await else {
            return;
        };
        let records = records.clone();
        let live = Arc::clone(&live);
        let accepted = accepted.clone();
        tasks.spawn(async move {
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::debug!(%error, "management handshake failed");
                    return;
                }
            };
            {
                // Admission and registration share the revoker's lock, before waiting
                // for peer input: a delayed first stream cannot escape key rotation.
                let mut live = live.lock().expect("live connection list is not poisoned");
                let record = records.borrow();
                if !admits(
                    record.accepted_client().as_ref(),
                    connection.remote_id().as_bytes(),
                ) && !admits(
                    record.pending_client().as_ref(),
                    connection.remote_id().as_bytes(),
                ) {
                    let code = if !record.has_management_client() {
                        PAIRING_CLEARED
                    } else {
                        REFUSED_BY_IDENTITY
                    };
                    connection.close(code, b"management access refused");
                    return;
                }
                live.retain(|weak| {
                    weak.upgrade()
                        .is_some_and(|connection| connection.close_reason().is_none())
                });
                live.push(connection.weak_handle());
            }
            let (send, recv) = match connection.accept_bi().await {
                Ok(streams) => streams,
                Err(error) => {
                    tracing::debug!(%error, "management client opened no RPC stream");
                    return;
                }
            };
            drop(permit);
            let io = ManagementIo {
                io: tokio::io::join(recv, send),
                _connection: connection,
            };
            let _ = accepted.send(Ok(io)).await;
        });
    }
}

/// One accepted RPC stream, shaped for tonic's incoming-connection stream.
pub struct ManagementIo {
    io: Join<RecvStream, SendStream>,
    // Keeps the QUIC connection open for as long as tonic serves this stream.
    _connection: Connection,
}

impl AsyncRead for ManagementIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for ManagementIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

impl Connected for ManagementIo {
    type ConnectInfo = WeakConnectionHandle;

    fn connect_info(&self) -> Self::ConnectInfo {
        self._connection.weak_handle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_accepted_client_key_is_admitted() {
        let accepted = [1; 32];
        assert!(admits(Some(&accepted), &accepted));
        assert!(!admits(Some(&accepted), &[2; 32]));
        assert!(!admits(None, &accepted));
    }

    #[test]
    fn management_secret_is_redacted_and_its_public_key_is_stable() {
        let secret = ManagementSecret::generate();
        assert_eq!(format!("{secret:?}"), "ManagementSecret(REDACTED)");
        let reloaded: ManagementSecret =
            serde_json::from_value(serde_json::to_value(&secret).unwrap()).unwrap();
        assert_eq!(reloaded.public_key(), secret.public_key());
    }
}
