//! Cloud Relay Register client.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use ployz_core::{CloudPairing, MachineId, MachineRpcServer, PairingCredential};
use ployz_relay::{ClientError, Open, RegisterRequest, RelayClient, TunnelIo};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::watch,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tonic::transport::{Server, server::Connected};

use crate::rpc::MachineService;

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

/// Hold Register: hello with Machine ID, Pairing Credential bearer, echo pings,
/// and Attach each Open to serve Machine RPC.
///
/// # Errors
///
/// If the Relay is unreachable or Register is rejected.
pub async fn hold_register(
    url: &str,
    pairing: &PairingCredential,
    machine_id: &MachineId,
    service: MachineService,
) -> Result<RegisterHold, Error> {
    let client = RelayClient::new(url)?;
    let mut ws = client.register(pairing.as_str(), machine_id).await?;
    let task = tokio::spawn(async move {
        while let Ok(Some(message)) = ws.recv::<Open>().await {
            if let Some(nonce) = message.ping_nonce() {
                let _ = ws.send(&RegisterRequest::pong(nonce)).await;
                continue;
            }
            tokio::spawn(serve_attach(client.clone(), message, service.clone()));
        }
    });
    Ok(RegisterHold { task })
}

async fn serve_attach(client: RelayClient, open: Open, service: MachineService) {
    let tunnel_id = match open.tunnel_id() {
        Ok(tunnel_id) => tunnel_id,
        Err(error) => {
            tracing::warn!(error = %error, "relay Open dropped");
            return;
        }
    };
    let tunnel = match client.attach(tunnel_id.as_str()).await {
        Ok(tunnel) => tunnel,
        Err(error) => {
            tracing::warn!(error = %error, "relay Open dropped");
            return;
        }
    };
    let io = Incoming(tunnel.into_io());
    let _ = Server::builder()
        .add_service(MachineRpcServer::new(service))
        .serve_with_incoming(tokio_stream::once(Ok::<_, io::Error>(io)))
        .await;
}

struct Incoming(TunnelIo);

impl Connected for Incoming {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for Incoming {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(context, buffer)
    }
}

impl AsyncWrite for Incoming {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(context)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(context)
    }
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
    mut pairing: watch::Receiver<Option<CloudPairing>>,
    machine_id: MachineId,
    service: MachineService,
    shutdown: CancellationToken,
) -> io::Result<()> {
    loop {
        let current = pairing.borrow_and_update().clone();
        let _hold = match current {
            Some(current) => Some(
                hold_register(
                    current.relay_url(),
                    current.secret(),
                    &machine_id,
                    service.clone(),
                )
                .await
                .map_err(io::Error::other)?,
            ),
            None => None,
        };
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            changed = pairing.changed() => {
                changed.map_err(|_| io::Error::other("cloud pairing watch closed"))?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::Ipv4Addr,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use http::StatusCode;
    use ployz::connect::connect_relay;
    use ployz_core::{CloudPairing, DescribeContractRequest, MachineId, PairingCredential, op};
    use ployz_relay::{
        ClientError, DialCredential, PairingCredential as RelayPairing, Relay, RelayClient,
    };
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    use super::{Error, hold_register, run};
    use crate::{machine::LocalMachineStore, rpc::MachineService};

    const PAIRING: &str = "pairing-secret";
    const DIAL: &str = "dial-secret";
    const OTHER: &str = "other-pairing";

    #[tokio::test]
    async fn cloud_dial_reaches_machine_rpc() {
        let session = Session::start().await;
        let mut client = connect_relay(
            &session.url,
            DialCredential::parse(DIAL).unwrap(),
            RelayPairing::parse(PAIRING).unwrap(),
            session.machine_id,
        )
        .await
        .unwrap();
        let description = client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap();
        assert_eq!(description.machine_id, session.machine_id);
    }

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
        let error = match hold_register(
            "not-a-url",
            &secret(),
            &MachineId::random(),
            test_service().1,
        )
        .await
        {
            Ok(_) => panic!("expected unreachable Relay to fail"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::Client(_)));
    }

    #[tokio::test]
    async fn cloud_pairing_hold_appears_on_list_held() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_service();
        let shutdown = CancellationToken::new();
        let (_pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, machine_id, service, shutdown.clone()));
        wait_for_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn no_cloud_pairing_stays_off_list_held() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_service();
        let shutdown = CancellationToken::new();
        let (_pairing_tx, pairing_rx) = watch::channel(None);
        let hold = tokio::spawn(run(pairing_rx, machine_id, service, shutdown.clone()));
        assert_not_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cloud_pairing_arriving_later_appears_on_list_held() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_service();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) = watch::channel(None);
        let hold = tokio::spawn(run(pairing_rx, machine_id, service, shutdown.clone()));
        assert_not_held(&relay.url, machine_id).await;
        pairing_tx.send_replace(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        wait_for_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn clearing_cloud_pairing_leaves_list() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_service();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, machine_id, service, shutdown.clone()));
        wait_for_held(&relay.url, machine_id).await;
        pairing_tx.send_replace(None);
        assert_not_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn replacing_cloud_pairing_moves_register_hold() {
        let first = RelayListen::start().await;
        let second = RelayListen::start().await;
        let (machine_id, service) = test_service();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&first.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, machine_id, service, shutdown.clone()));
        wait_for_held(&first.url, machine_id).await;
        pairing_tx.send_replace(Some(CloudPairing::parse(&second.url, secret()).unwrap()));
        wait_for_held(&second.url, machine_id).await;
        assert_not_held(&first.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unreachable_cloud_pairing_fails() {
        let pairing = CloudPairing::parse("not-a-url", secret()).unwrap();
        let error = run(
            watch::channel(Some(pairing)).1,
            MachineId::random(),
            test_service().1,
            CancellationToken::new(),
        )
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
            let (machine_id, service) = test_service();
            let hold = hold_register(&relay.url, &secret(), &machine_id, service)
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

    fn test_service() -> (MachineId, MachineService) {
        let data_dir = std::env::temp_dir().join(format!("ployzd-relay-{}", MachineId::random()));
        let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
        let machine_id = store.lock().unwrap().record().id();
        let (reset, _) = watch::channel(false);
        (machine_id, MachineService::new(store, reset))
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
