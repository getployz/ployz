//! Fake Relay and Pairing Credential registration helpers.

use std::{sync::Mutex, time::Duration};

use ployz::sdk;
use ployz_core::{MachineId, PairingCredential};
use ployz_relay::{ClientError, DialCredential, Open, RegisterRequest, Relay, RelayClient};
use tokio::task::JoinHandle;
use tonic::Status;

const DIAL: &str = "dial-secret";

pub struct RelayListen {
    pub url: String,
    _server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl RelayListen {
    pub async fn start() -> Self {
        let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
        let (address, server, _goaway) = relay
            .serve((std::net::Ipv4Addr::LOCALHOST, 0).into())
            .await
            .unwrap();
        Self {
            url: format!("http://{address}"),
            _server: server,
        }
    }

    pub async fn revoke(&self, pairing: &str) {
        sdk::revoke_pairing(&self.url, DIAL, pairing)
            .await
            .expect("test Relay accepts Dial revoke");
    }
}

pub(super) async fn hold_register(
    url: &str,
    pairing: &PairingCredential,
    machine_id: &MachineId,
    slot: &Mutex<Option<JoinHandle<()>>>,
) -> Result<(), Status> {
    let mut ws = RelayClient::new(&ployz_core::RelayEndpoint::parse(url).unwrap())
        .map_err(status_from_client)?
        .register(pairing.as_str(), machine_id)
        .await
        .map_err(status_from_client)?;
    let hold = tokio::spawn(async move {
        while let Ok(Some(message)) = ws.recv::<Open>().await {
            if let Some(nonce) = message.ping_nonce() {
                let _ = ws.send(&RegisterRequest::pong(nonce)).await;
            }
        }
    });
    if let Some(old) = slot.lock().unwrap().replace(hold) {
        old.abort();
    }
    Ok(())
}

fn status_from_client(error: ClientError) -> Status {
    match error.status() {
        Some(http::StatusCode::UNAUTHORIZED) => Status::unauthenticated(error.to_string()),
        Some(http::StatusCode::BAD_REQUEST) => Status::invalid_argument(error.to_string()),
        Some(http::StatusCode::NOT_FOUND) => Status::not_found(error.to_string()),
        Some(http::StatusCode::SERVICE_UNAVAILABLE) => Status::unavailable(error.to_string()),
        _ => Status::unavailable(error.to_string()),
    }
}

pub async fn wait_for_held(url: &str, pairing: &str, machine_id: MachineId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let listed = sdk::list_held(url, DIAL, pairing).await.unwrap();
        if listed
            .iter()
            .any(|row| row.machine_id().ok() == Some(machine_id) && row.register_rtt_ns.is_some())
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {machine_id} on List with path RTT, got {listed:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub async fn assert_not_held(url: &str, pairing: &str, machine_id: MachineId) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    let listed = sdk::list_held(url, DIAL, pairing).await.unwrap();
    assert!(
        listed
            .iter()
            .all(|row| row.machine_id().ok() != Some(machine_id)),
        "Machine must stay off List for revoked pairing, got {listed:?}"
    );
}
