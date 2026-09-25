//! Dial a Machine's Build Grant transport and serve it as a registry on loopback, so a
//! stock `docker push` makes the layer-aware push through the grant.

use std::{io, net::SocketAddr};

use iroh::{
    Endpoint, EndpointAddr, PublicKey, RelayMode, SecretKey,
    endpoint::{Connection, ConnectionError, VarInt, presets},
};
use ployz_core::{BUILD_GRANT_ALPN, BUILD_GRANT_ENDED, BUILD_GRANT_REFUSED, BuildGrant};
use tokio::net::TcpListener;

use super::ManagementRelay;

/// A loopback registry address forwarding each TCP connection to the grant transport.
/// Dropping it stops forwarding and closes the connection.
pub struct GrantRegistry {
    address: SocketAddr,
    connection: Connection,
    accept: tokio::task::JoinHandle<()>,
    _endpoint: Endpoint,
}

impl GrantRegistry {
    /// Loopback `host:port` to name in image references.
    #[must_use]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Why the Machine stopped serving this grant, when it did.
    #[must_use]
    pub fn refusal(&self) -> Option<&'static str> {
        let ConnectionError::ApplicationClosed(close) = self.connection.close_reason()? else {
            return None;
        };
        if close.error_code == VarInt::from_u32(BUILD_GRANT_REFUSED) {
            Some("the Machine refused the Build Grant: it ended, expired, or was already used")
        } else if close.error_code == VarInt::from_u32(BUILD_GRANT_ENDED) {
            Some("the Build Grant ended during the push")
        } else {
            None
        }
    }
}

impl Drop for GrantRegistry {
    fn drop(&mut self) {
        self.accept.abort();
        self.connection.close(VarInt::from_u32(0), b"push finished");
    }
}

/// Dial the grant's Machine on the Build Grant ALPN and forward a loopback listener to it.
///
/// # Errors
/// Returns bind, dial, or listener failures.
pub async fn open_grant_registry(
    grant: &BuildGrant,
    relay: &ManagementRelay,
) -> io::Result<GrantRegistry> {
    let machine = PublicKey::from_bytes(grant.machine().as_bytes())
        .map_err(|_| io::Error::other("the Build Grant names an invalid Machine identity"))?;
    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(grant.secret()))
        .relay_mode(RelayMode::custom([relay.url.clone()]))
        .ca_tls_config(relay.tls.clone())
        .bind()
        .await
        .map_err(io::Error::other)?;
    let connection = endpoint
        .connect(
            EndpointAddr::new(machine).with_relay_url(relay.url.clone()),
            BUILD_GRANT_ALPN,
        )
        .await
        .map_err(io::Error::other)?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let forwarded = connection.clone();
    let accept = tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let connection = forwarded.clone();
            tokio::spawn(async move {
                let Ok((send, recv)) = connection.open_bi().await else {
                    return;
                };
                let mut stream = tokio::io::join(recv, send);
                // A failed stream fails the pusher's request, which reports it.
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
            });
        }
    });
    Ok(GrantRegistry {
        address,
        connection,
        accept,
        _endpoint: endpoint,
    })
}
