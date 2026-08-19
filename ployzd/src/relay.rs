//! Cloud Relay Register client.

use std::io;

use ployz_core::{CloudPairing, MachineId, PairingCredential};
use ployz_relay::{ClientError, Open, RegisterRequest, RelayClient};
use thiserror::Error;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Held Cloud Relay Register. Dropping it ends the stream.
#[must_use]
pub struct RegisterHold {
    task: JoinHandle<()>,
}

/// Failures holding Cloud Relay Register.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Client(#[from] ClientError),
}

/// Hold Register: hello with Machine ID, Pairing Credential bearer, echo pings.
///
/// # Errors
///
/// If the Relay is unreachable or Register is rejected.
pub async fn hold_register(
    url: &str,
    pairing: &PairingCredential,
    machine_id: &MachineId,
) -> Result<RegisterHold, Error> {
    let mut ws = RelayClient::new(url)?
        .register(pairing.as_str(), machine_id)
        .await?;
    let task = tokio::spawn(async move {
        while let Ok(Some(message)) = ws.recv::<Open>().await {
            // ponytail: Attach is a later ticket
            if let Some(nonce) = message.ping_nonce() {
                let _ = ws.send(&RegisterRequest::pong(nonce)).await;
            }
        }
    });
    Ok(RegisterHold { task })
}

impl Drop for RegisterHold {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Hold Register for stored Cloud Pairing, or idle if none.
///
/// # Errors
///
/// If the Relay is unreachable or Register is rejected.
pub(crate) async fn run(
    pairing: Option<CloudPairing>,
    machine_id: MachineId,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let _hold = match pairing {
        Some(pairing) => Some(
            hold_register(pairing.relay_url(), pairing.secret(), &machine_id)
                .await
                .map_err(io::Error::other)?,
        ),
        None => None,
    };
    shutdown.cancelled().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use http::StatusCode;
    use ployz_core::{CloudPairing, MachineId, PairingCredential};
    use ployz_relay::{ClientError, DialCredential, Relay, RelayClient};
    use tokio_util::sync::CancellationToken;

    use super::{Error, hold_register, run};

    const PAIRING: &str = "pairing-secret";
    const DIAL: &str = "dial-secret";
    const OTHER: &str = "other-pairing";

    #[tokio::test]
    async fn register_hello_appears_on_list_held() {
        let session = Session::start().await;
        let listed = list(&session.url, DIAL, PAIRING).await.unwrap();
        let [row] = listed.as_slice() else {
            panic!("expected one held Register, got {listed:?}");
        };
        assert_eq!(row.machine_id().unwrap(), session.machine_id);
    }

    #[tokio::test]
    async fn register_pings_are_echoed() {
        let session = Session::start().await;
        wait_for_held(&session.url, session.machine_id).await;
    }

    #[tokio::test]
    async fn pairing_credential_cannot_impersonate_cloud() {
        let session = Session::start().await;
        let cloud = client(&session.url);

        let list = cloud.list(PAIRING, PAIRING).await;
        assert_eq!(list.unwrap_err().status(), Some(StatusCode::UNAUTHORIZED));

        let revoke = cloud.revoke(PAIRING, PAIRING).await;
        assert_eq!(revoke.unwrap_err().status(), Some(StatusCode::UNAUTHORIZED));

        let dial = cloud
            .dial(PAIRING, PAIRING, session.machine_id.as_str())
            .await;
        assert_eq!(dial.unwrap_err().status(), Some(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn wrong_pairing_does_not_list_this_machine() {
        let session = Session::start().await;
        let listed = list(&session.url, DIAL, OTHER).await.unwrap();
        assert!(
            listed
                .iter()
                .all(|row| row.machine_id().ok() != Some(session.machine_id))
        );
    }

    #[tokio::test]
    async fn unreachable_relay_fails() {
        let error = match hold_register("not-a-url", &secret(), &MachineId::random()).await {
            Ok(_) => panic!("expected unreachable Relay to fail"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::Client(_)));
    }

    #[tokio::test]
    async fn cloud_pairing_hold_appears_on_list_held() {
        let relay = RelayListen::start().await;
        let machine_id = MachineId::random();
        let shutdown = CancellationToken::new();
        let hold = tokio::spawn(run(
            Some(CloudPairing::parse(&relay.url, secret()).unwrap()),
            machine_id,
            shutdown.clone(),
        ));
        wait_for_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn no_cloud_pairing_stays_off_list_held() {
        let relay = RelayListen::start().await;
        let machine_id = MachineId::random();
        let shutdown = CancellationToken::new();
        let hold = tokio::spawn(run(None, machine_id, shutdown.clone()));
        assert_not_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unreachable_cloud_pairing_fails() {
        let pairing = CloudPairing::parse("not-a-url", secret()).unwrap();
        let error = run(Some(pairing), MachineId::random(), CancellationToken::new())
            .await
            .expect_err("unreachable Relay must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
    }

    struct RelayListen {
        url: String,
        _server: tokio::task::JoinHandle<std::io::Result<()>>,
        _goaway: ployz_relay::Goaway,
    }

    impl RelayListen {
        async fn start() -> Self {
            let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
            let listen = (Ipv4Addr::LOCALHOST, 0).into();
            let (address, server, goaway) = relay.serve(listen).await.unwrap();
            Self {
                url: format!("http://{address}"),
                _server: server,
                _goaway: goaway,
            }
        }
    }

    struct Session {
        url: String,
        machine_id: MachineId,
        _hold: super::RegisterHold,
        _relay: RelayListen,
    }

    impl Session {
        async fn start() -> Self {
            let relay = RelayListen::start().await;
            let machine_id = MachineId::random();
            let hold = hold_register(&relay.url, &secret(), &machine_id)
                .await
                .expect("Register hello is accepted");
            Self {
                url: relay.url.clone(),
                machine_id,
                _hold: hold,
                _relay: relay,
            }
        }
    }

    fn secret() -> PairingCredential {
        PairingCredential::parse(PAIRING).unwrap()
    }

    async fn wait_for_held(url: &str, machine_id: MachineId) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let listed = list(url, DIAL, PAIRING).await.unwrap();
            if listed.iter().any(|row| {
                row.machine_id().ok() == Some(machine_id) && row.register_rtt_ns.is_some()
            }) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "expected {machine_id} on List with path RTT, got {listed:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn assert_not_held(url: &str, machine_id: MachineId) {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let listed = list(url, DIAL, PAIRING).await.unwrap();
        assert!(
            listed
                .iter()
                .all(|row| row.machine_id().ok() != Some(machine_id)),
            "Machine must stay off List, got {listed:?}"
        );
    }

    async fn list(
        url: &str,
        dial: &str,
        pairing: &str,
    ) -> Result<Vec<ployz_relay::HeldRegister>, ClientError> {
        client(url).list(dial, pairing).await
    }

    fn client(url: &str) -> RelayClient {
        RelayClient::new(url).expect("test Relay URL is http")
    }
}
