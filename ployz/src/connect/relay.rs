//! Cloud Relay Dial: wrap the opaque tunnel as a Channel.

use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use http::StatusCode;
use hyper_util::rt::TokioIo;
use ployz_core::{MachineId, RelayEndpoint};
use ployz_relay::{ClientError, DialCredential, HeldRegister, PairingCredential, RelayClient};
use tonic::transport::{Channel, Endpoint};

use super::ConnectError;

pub(super) async fn connect_channel(
    url: &RelayEndpoint,
    credential: &DialCredential,
    pairing: &PairingCredential,
    machine_id: &MachineId,
) -> Result<Channel, ConnectError> {
    let io = RelayClient::new(url)?
        .dial(credential.as_str(), pairing.as_str(), machine_id.as_str())
        .await?
        .into_io();
    let io = Arc::new(Mutex::new(Some(io)));
    Endpoint::from_static("http://[::]:50051")
        .connect_timeout(Duration::from_secs(5))
        .connect_with_connector(tower::service_fn(move |_| {
            let io = Arc::clone(&io);
            async move {
                io.lock()
                    .expect("relay tunnel mutex poisoned")
                    .take()
                    .ok_or_else(|| io::Error::other("relay tunnel already consumed"))
                    .map(TokioIo::new)
            }
        }))
        .await
        .map_err(ConnectError::from)
}

/// List Machines currently holding Register for this pairing.
///
/// # Errors
/// Returns [`ConnectError::InvalidDialCredential`] when the bearer is rejected,
/// or another [`ConnectError`] when the Relay call fails.
pub(crate) async fn list_held(
    url: &str,
    credential: &DialCredential,
    pairing: &PairingCredential,
) -> Result<Vec<HeldRegister>, ConnectError> {
    let url = RelayEndpoint::parse(url)?;
    Ok(RelayClient::new(&url)?
        .list(credential.as_str(), pairing.as_str())
        .await?)
}

/// Revoke the Cloud Pairing so Register with that Pairing Credential fails afterwards.
///
/// # Errors
/// Returns [`ConnectError::InvalidDialCredential`] when the bearer is rejected.
pub(crate) async fn revoke_pairing(
    url: &str,
    credential: &DialCredential,
    pairing: &PairingCredential,
) -> Result<(), ConnectError> {
    let url = RelayEndpoint::parse(url)?;
    Ok(RelayClient::new(&url)?
        .revoke(credential.as_str(), pairing.as_str())
        .await?)
}

impl From<ClientError> for ConnectError {
    fn from(error: ClientError) -> Self {
        match error.status() {
            Some(StatusCode::UNAUTHORIZED) => Self::InvalidDialCredential,
            Some(StatusCode::NOT_FOUND) => Self::UnknownMachine,
            _ => Self::Relay(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn relay_rejections_preserve_status_and_retry_only_temporary_failures() {
        for (status, retry) in [
            (400, false),
            (401, false),
            (403, false),
            (404, false),
            (408, true),
            (429, true),
            (500, true),
            (501, false),
            (502, true),
            (503, true),
            (504, true),
            (505, false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                assert!(socket.read(&mut request).await.unwrap() > 0);
                socket.write_all(format!("HTTP/1.1 {status} Rejected\r\nContent-Length: 8\r\nConnection: close\r\n\r\nrejected").as_bytes()).await.unwrap();
            });
            let error = list_held(
                &url,
                &DialCredential::parse("dial-secret").unwrap(),
                &PairingCredential::parse("pairing-secret").unwrap(),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.is_setup_retryable(),
                retry,
                "status {status}: {error}"
            );
            assert_eq!(error.is_unreachable(), retry, "status {status}: {error}");
            if status != 401 && status != 404 {
                let ConnectError::Relay(error) = error else {
                    panic!("Relay status must stay typed")
                };
                assert_eq!(error.status().unwrap().as_u16(), status);
            }
            server.await.unwrap();
        }
        let error = ConnectError::from(ClientError::Transport("connection reset".into()));
        assert!(error.is_setup_retryable());
        assert!(error.is_unreachable());
    }
}
