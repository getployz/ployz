//! Cloud Relay Dial: wrap the opaque tunnel as a Channel.

use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use http::StatusCode;
use hyper_util::rt::TokioIo;
use ployz_core::{MachineId, RelayEndpoint};
use ployz_relay::{
    ClientError, DialCredential, HeldRegister, PairingCredential, RelayClient, TunnelIo,
};
use tonic::transport::{Channel, Endpoint};

use super::ConnectError;

pub(super) async fn connect_channel(
    url: &RelayEndpoint,
    credential: &DialCredential,
    pairing: &PairingCredential,
    machine_id: &MachineId,
) -> Result<Channel, ConnectError> {
    let io = dial_tunnel(url, credential, pairing, machine_id).await?;
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

async fn dial_tunnel(
    url: &RelayEndpoint,
    credential: &DialCredential,
    pairing: &PairingCredential,
    machine_id: &MachineId,
) -> Result<TunnelIo, ConnectError> {
    Ok(RelayClient::new(url)?
        .dial(credential.as_str(), pairing.as_str(), machine_id.as_str())
        .await?
        .into_io())
}

pub(super) async fn list_held(
    url: &str,
    credential: &DialCredential,
    pairing: &PairingCredential,
) -> Result<Vec<HeldRegister>, ConnectError> {
    let url = RelayEndpoint::parse(url)?;
    Ok(RelayClient::new(&url)?
        .list(credential.as_str(), pairing.as_str())
        .await?)
}

pub(super) async fn revoke_pairing(
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
            _ => Self::Attempt(error.to_string().into()),
        }
    }
}
