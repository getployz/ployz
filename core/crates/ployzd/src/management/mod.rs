//! In-process iroh management transport: serves the Machine API to the client keys
//! of the Management Client slots, reachable through the Ployz Relay by Management Identity.

use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http_body::Frame;
use hyper::server::conn::http2;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    service::TowerToHyperService,
};
use iroh::{
    Endpoint, RelayMode, RelayUrl, SecretKey,
    endpoint::{
        BindError, Connection, IdleTimeout, Incoming, QuicTransportConfig, VarInt, presets,
    },
    tls::CaTlsConfig,
};
use ployz_core::{BUILD_GRANT_ALPN, DEFAULT_RELAY_URL, MANAGEMENT_ALPN, MANAGEMENT_PORT, Rpc, op};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tonic::{
    body::Body,
    codegen::{Service, http},
};

use crate::machine::{LocalMachine, LocalMachineRecord};

pub mod build_grant;
pub use build_grant::BuildGrants;

/// Application close code sent when the remote key is not admitted and no tombstone holds it.
pub const CLIENT_REFUSED: VarInt = VarInt::from_u32(0x50);
/// Application close code sent to a live connection whose key no slot holds any more.
pub const REVOKED: VarInt = VarInt::from_u32(0x51);
/// Authenticated confirmation that the dialing key was cleared: a Cleared tombstone holds it.
pub const CLIENT_CLEARED: VarInt = VarInt::from_u32(0x52);

const MAX_CONCURRENT_HANDSHAKES: usize = 64;
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a revoked connection may take to drain and acknowledge before it closes.
const REVOCATION_DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Bind the management endpoint: the Machine RPC and Build Grant ALPNs, no address
/// lookup, only the configured relay.
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
        .alpns(vec![MANAGEMENT_ALPN.to_vec(), BUILD_GRANT_ALPN.to_vec()])
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

/// Serve `api` over `endpoint` until `shutdown`, then drain every connection.
///
/// Keys other than a Management Client slot's accepted or pending key are closed with
/// [`CLIENT_CLEARED`] when a Cleared tombstone holds them, otherwise [`CLIENT_REFUSED`].
/// Authenticating with a pending key permits read-only identity negotiation. Its first
/// operational RPC activates it. A record change that removes a key from every slot
/// revokes only that key's connections: in-flight `SetManagementClient` responses, such
/// as the caller's own Clear, are delivered and acknowledged, every other stream ends at
/// once, and the connection then closes with [`REVOKED`].
///
/// Connections on the Build Grant ALPN are served by [`build_grant`] alone: only a live
/// grant's key is admitted there, and no Management Client key reaches it.
pub async fn serve<S>(
    endpoint: Endpoint,
    local: LocalMachine,
    api: S,
    grants: Arc<BuildGrants>,
    shutdown: CancellationToken,
) where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    let records = local.owner().watch();
    let handshakes = Arc::new(Semaphore::new(MAX_CONCURRENT_HANDSHAKES));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let incoming = tokio::select! {
            () = shutdown.cancelled() => break,
            _ = connections.join_next(), if !connections.is_empty() => continue,
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => incoming,
                None => break,
            },
        };
        let Ok(permit) = Arc::clone(&handshakes).acquire_owned().await else {
            break;
        };
        connections.spawn(serve_connection(
            incoming,
            permit,
            records.clone(),
            api.clone(),
            Arc::clone(&grants),
            shutdown.clone(),
        ));
    }
    while connections.join_next().await.is_some() {}
    endpoint.close().await;
}

async fn serve_connection<S>(
    incoming: Incoming,
    permit: OwnedSemaphorePermit,
    mut records: watch::Receiver<Arc<LocalMachineRecord>>,
    api: S,
    grants: Arc<BuildGrants>,
    shutdown: CancellationToken,
) where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    let connection = match incoming.await {
        Ok(connection) => connection,
        Err(error) => {
            tracing::debug!(%error, "management handshake failed");
            return;
        }
    };
    if connection.alpn() == BUILD_GRANT_ALPN {
        drop(permit);
        build_grant::serve(connection, &grants, shutdown).await;
        return;
    }
    let remote = *connection.remote_id().as_bytes();
    // Marking this version seen before waiting for peer input means any later
    // change wakes `revoked`: a delayed first stream cannot escape key rotation.
    let refused = refusal(&records.borrow_and_update(), &remote);
    if let Some(code) = refused {
        connection.close(code, b"management access refused");
        return;
    }
    match serve_admitted(&connection, permit, revoked(records, remote), api, shutdown).await {
        Ended::Closed => {}
        Ended::Revoked => connection.close(REVOKED, b"revoked"),
    }
}

/// The close code refusing `remote`, or `None` when a slot admits it.
fn refusal(record: &LocalMachineRecord, remote: &[u8; 32]) -> Option<VarInt> {
    if record.admits_management_client(remote) {
        None
    } else if record.clears_management_client(remote) {
        Some(CLIENT_CLEARED)
    } else {
        Some(CLIENT_REFUSED)
    }
}

/// Why an admitted connection stopped being served.
enum Ended {
    /// The peer closed, the connection failed, or the transport shut down.
    Closed,
    /// A record change revoked the key; delivery of the bytes already sent was awaited
    /// for up to `REVOCATION_DELIVERY_TIMEOUT`.
    Revoked,
}

async fn serve_admitted<S>(
    connection: &Connection,
    permit: OwnedSemaphorePermit,
    revoked: impl Future<Output = ()>,
    api: S,
    shutdown: CancellationToken,
) -> Ended
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    tokio::pin!(revoked);
    let (send, recv) = tokio::select! {
        () = &mut revoked => return Ended::Revoked,
        () = shutdown.cancelled() => return Ended::Closed,
        streams = connection.accept_bi() => match streams {
            Ok(streams) => streams,
            Err(error) => {
                tracing::debug!(%error, "management client opened no RPC stream");
                return Ended::Closed;
            }
        },
    };
    drop(permit);
    // Created before serving so the acknowledgement of the final bytes cannot be missed.
    let acknowledged = send.stopped();
    let drain = CancellationToken::new();
    let service = ConnectionApi {
        api,
        connection: connection.clone(),
        drain: drain.clone(),
    };
    let serving = http2::Builder::new(TokioExecutor::new()).serve_connection(
        TokioIo::new(tokio::io::join(recv, send)),
        TowerToHyperService::new(service),
    );
    tokio::pin!(serving);
    let ended = tokio::select! {
        served = serving.as_mut() => {
            if let Err(error) = served {
                tracing::debug!(%error, "management connection ended");
            }
            return Ended::Closed;
        }
        () = &mut revoked => {
            drain.cancel();
            Ended::Revoked
        }
        () = shutdown.cancelled() => Ended::Closed,
    };
    // GOAWAY; once the remaining streams end, hyper finishes the QUIC stream.
    serving.as_mut().graceful_shutdown();
    let drained = async {
        if let Err(error) = serving.await {
            tracing::debug!(%error, "management connection failed while draining");
        }
    };
    let Ended::Revoked = ended else {
        drained.await;
        return ended;
    };
    // Closing discards unacknowledged data, so wait until the peer holds every byte,
    // including the caller's own Clear response. The key can no longer run any RPC,
    // and the bound keeps revocation prompt when a peer never acknowledges.
    let delivered = async {
        drained.await;
        let outcome = acknowledged.await;
        tracing::debug!(?outcome, "revoked management stream delivery settled");
    };
    if tokio::time::timeout(REVOCATION_DELIVERY_TIMEOUT, delivered)
        .await
        .is_err()
    {
        tracing::debug!("revoked management client did not acknowledge in time");
    }
    ended
}

/// Completes once a published record no longer admits `remote`.
async fn revoked(mut records: watch::Receiver<Arc<LocalMachineRecord>>, remote: [u8; 32]) {
    loop {
        if records.changed().await.is_err() {
            // The record owner stopped; shutdown ends this connection.
            return std::future::pending().await;
        }
        if !records
            .borrow_and_update()
            .admits_management_client(&remote)
        {
            return;
        }
    }
}

/// The Machine API as one management connection serves it: each request carries its
/// connection, and once the connection drains every stream but `SetManagementClient`
/// ends.
#[derive(Clone)]
struct ConnectionApi<S> {
    api: S,
    connection: Connection,
    drain: CancellationToken,
}

impl<S> Service<http::Request<hyper::body::Incoming>> for ConnectionApi<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>, Error = Infallible>
        + Send
        + 'static,
    S::Future: Send,
{
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.api.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<hyper::body::Incoming>) -> Self::Future {
        // Clearing its own slot revokes the caller, and the Clear response is its only
        // confirmation on this connection, so that response survives the drain.
        let drain_exempt = request.uri().path() == op::SetManagementClient::PATH;
        let mut request = request.map(Body::new);
        request
            .extensions_mut()
            .insert(self.connection.weak_handle());
        let response = self.api.call(request);
        if drain_exempt {
            return Box::pin(response);
        }
        let drain = self.drain.clone();
        Box::pin(async move {
            tokio::select! {
                response = response => Ok(response?.map(|body| {
                    Body::new(Drained {
                        body,
                        drained: Box::pin(drain.cancelled_owned()),
                    })
                })),
                () = drain.cancelled() => Ok(revoked_status().into_http()),
            }
        })
    }
}

fn revoked_status() -> tonic::Status {
    tonic::Status::unauthenticated("management credential revoked")
}

/// A response body that fails once its connection drains.
struct Drained {
    body: Body,
    drained: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl http_body::Body for Drained {
    type Data = Bytes;
    type Error = tonic::Status;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.drained.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Some(Err(revoked_status())));
        }
        Pin::new(&mut self.body).poll_frame(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_secret_is_redacted_and_its_public_key_is_stable() {
        let secret = ManagementSecret::generate();
        assert_eq!(format!("{secret:?}"), "ManagementSecret(REDACTED)");
        let reloaded: ManagementSecret =
            serde_json::from_value(serde_json::to_value(&secret).unwrap()).unwrap();
        assert_eq!(reloaded.public_key(), secret.public_key());
    }
}
