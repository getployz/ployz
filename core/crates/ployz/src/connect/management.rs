//! Management transport endpoint ownership, authenticated dialing, and channel cleanup.

use super::{ConnectError, connect_stream};
use crate::context::ConnectionError;
use iroh::{
    Endpoint as IrohEndpoint, EndpointAddr, PublicKey, RelayMode, RelayUrl, SecretKey,
    endpoint::{VarInt, presets},
    tls::CaTlsConfig,
};
use ployz_core::{
    DEFAULT_RELAY_URL, DescribeContractRequest, MANAGEMENT_ALPN, MachineRpcClient,
    ManagementCapability, op,
};
use std::{
    collections::HashMap,
    io,
    pin::Pin,
    sync::{Arc, OnceLock, Weak},
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tonic::transport::Channel;

/// Application close codes the daemon refuses an unadmitted key with: never admitted or
/// rotated away, and confirmed cleared.
const CLIENT_REFUSED: VarInt = VarInt::from_u32(0x50);
const CLIENT_CLEARED: VarInt = VarInt::from_u32(0x52);

/// The relay the management transport dials through. Production uses the compiled
/// [`DEFAULT_RELAY_URL`] with the embedded WebPKI roots; tests point at an in-process relay.
#[derive(Clone, Debug)]
pub struct ManagementRelay {
    url: RelayUrl,
    tls: CaTlsConfig,
}

impl Default for ManagementRelay {
    fn default() -> Self {
        Self {
            url: DEFAULT_RELAY_URL
                .parse()
                .expect("the compiled relay URL is valid"),
            tls: CaTlsConfig::default(),
        }
    }
}

impl ManagementRelay {
    /// Test hook: another relay and the trust roots that verify it, such as
    /// `iroh::test_utils::run_relay_server` with `CaTlsConfig::insecure_skip_verify()`
    /// (that constructor needs iroh's `test-utils` feature, so it stays in test crates).
    #[must_use]
    pub fn custom(url: RelayUrl, tls: CaTlsConfig) -> Self {
        Self { url, tls }
    }
}

struct ManagementEndpoint {
    endpoint: IrohEndpoint,
    runtime: tokio::runtime::Handle,
}

impl Drop for ManagementEndpoint {
    fn drop(&mut self) {
        let endpoint = self.endpoint.clone();
        self.runtime.spawn(async move { endpoint.close().await });
    }
}

type EndpointKey = ([u8; 32], RelayUrl);

async fn management_endpoint(
    secret: &[u8; 32],
    relay: &ManagementRelay,
) -> Result<Arc<ManagementEndpoint>, ConnectError> {
    static ENDPOINTS: OnceLock<tokio::sync::Mutex<HashMap<EndpointKey, Weak<ManagementEndpoint>>>> =
        OnceLock::new();
    // ponytail: serialize endpoint binds to prevent duplicate relay identities;
    // use per-key locks if concurrent first-dial throughput becomes a bottleneck.
    let mut endpoints = ENDPOINTS.get_or_init(Default::default).lock().await;
    endpoints.retain(|_, endpoint| endpoint.strong_count() > 0);
    let key = (*secret, relay.url.clone());
    if let Some(endpoint) = endpoints.get(&key).and_then(Weak::upgrade) {
        return Ok(endpoint);
    }
    let endpoint = IrohEndpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(secret))
        .relay_mode(RelayMode::custom([relay.url.clone()]))
        .ca_tls_config(relay.tls.clone())
        .bind()
        .await
        .map_err(|error| ConnectError::Attempt(format!("management bind: {error}").into()))?;
    let endpoint = Arc::new(ManagementEndpoint {
        endpoint,
        runtime: tokio::runtime::Handle::current(),
    });
    endpoints.insert(key, Arc::downgrade(&endpoint));
    Ok(endpoint)
}

/// Dial the Machine's Management Identity by key with only the relay as a hint, open the
/// single RPC stream and confirm the daemon accepted this capability before handing the
/// channel out.
pub(super) async fn connect_management(
    capability: &ManagementCapability,
    relay: &ManagementRelay,
) -> Result<Channel, ConnectError> {
    let machine = PublicKey::from_bytes(capability.machine().as_bytes())
        .map_err(|_| ConnectionError::ManagementCapability)?;
    let endpoint = management_endpoint(capability.client_secret(), relay).await?;
    let address = EndpointAddr::new(machine).with_relay_url(relay.url.clone());
    let connection = endpoint
        .endpoint
        .connect(address, MANAGEMENT_ALPN)
        .await
        .map_err(|error| ConnectError::Attempt(format!("management dial: {error}").into()))?;
    let session = Arc::new(ManagementConnection {
        connection,
        _endpoint: endpoint,
    });
    let probe = async {
        let (send, receive) =
            session.connection.open_bi().await.map_err(|error| {
                ConnectError::Attempt(format!("management stream: {error}").into())
            })?;
        let stream = ManagementIo {
            io: tokio::io::join(receive, send),
            _session: Arc::clone(&session),
        };
        let channel = connect_stream(stream, Duration::from_secs(15)).await?;
        // The daemon refuses by identity right after the handshake, which the client only
        // observes once it reads. Probe before the channel is trusted so refusal is
        // distinguishable from an unreachable Machine.
        MachineRpcClient::new(channel.clone())
            .describe_contract(
                op::DescribeContract::into_request(DescribeContractRequest {}).encode()?,
            )
            .await?;
        Ok::<_, ConnectError>(channel)
    };
    probe
        .await
        .map_err(|error| match session.connection.close_reason() {
            Some(iroh::endpoint::ConnectionError::ApplicationClosed(close))
                if close.error_code == CLIENT_REFUSED =>
            {
                ConnectError::ClientRefused
            }
            Some(iroh::endpoint::ConnectionError::ApplicationClosed(close))
                if close.error_code == CLIENT_CLEARED =>
            {
                ConnectError::ClientCleared
            }
            _ => error,
        })
}

// The channel owns its endpoint. Dropping a session closes the QUIC connection
// promptly and releases its UDP sockets and relay connection after draining.
struct ManagementIo {
    io: tokio::io::Join<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>,
    _session: Arc<ManagementConnection>,
}

struct ManagementConnection {
    connection: iroh::endpoint::Connection,
    _endpoint: Arc<ManagementEndpoint>,
}

impl Drop for ManagementConnection {
    fn drop(&mut self) {
        self.connection
            .close(VarInt::from_u32(0), b"session closed");
    }
}

impl AsyncRead for ManagementIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for ManagementIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_can_be_dropped_without_an_entered_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let endpoint = runtime.block_on(async {
            ManagementEndpoint {
                endpoint: IrohEndpoint::builder(presets::Minimal)
                    .relay_mode(RelayMode::Disabled)
                    .bind()
                    .await
                    .unwrap(),
                runtime: tokio::runtime::Handle::current(),
            }
        });
        std::thread::spawn(move || drop(endpoint)).join().unwrap();
    }
}
