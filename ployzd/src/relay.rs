//! Cloud Relay Register client.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use ployz_core::{CloudPairing, PairingCredential};
use ployz_relay::{ClientError, Open, RegisterRequest, RelayClient, TunnelIo};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::watch,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use tonic::transport::{Server, server::Connected};

use crate::machine_api::MachineApi;

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
    machine_api: MachineApi,
) -> Result<RegisterHold, Error> {
    let client = RelayClient::new(url)?;
    let machine_id = machine_api.machine_id();
    let mut ws = client.register(pairing.as_str(), &machine_id).await?;
    let task = tokio::spawn(async move {
        while let Ok(Some(message)) = ws.recv::<Open>().await {
            if let Some(nonce) = message.ping_nonce() {
                let _ = ws.send(&RegisterRequest::pong(nonce)).await;
                continue;
            }
            tokio::spawn(serve_attach(client.clone(), message, machine_api.clone()));
        }
    });
    Ok(RegisterHold { task })
}

async fn serve_attach(client: RelayClient, open: Open, machine_api: MachineApi) {
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
        .serve_with_incoming(machine_api, tokio_stream::once(Ok::<_, io::Error>(io)))
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

fn register_retry_delay(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.min(5)).min(30))
}

/// Hold Register for stored Cloud Pairing, or idle if none.
///
/// Register loss retries with bounded backoff until Cloud Pairing changes or
/// shutdown. Clearing or replacing pairing cancels the previous retries.
///
/// # Errors
///
/// If the Cloud Pairing watch closes.
pub(crate) async fn run(
    mut pairing: watch::Receiver<Option<CloudPairing>>,
    machine_api: MachineApi,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let mut failures = 0u32;
    loop {
        let current = pairing.borrow_and_update().clone();
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            result = pairing.changed() => {
                result.map_err(|_| io::Error::other("cloud pairing watch closed"))?;
                failures = 0;
            }
            new_failures = hold_current(current, &machine_api, failures) => {
                failures = new_failures;
            }
        }
    }
}

async fn hold_current(
    current: Option<CloudPairing>,
    machine_api: &MachineApi,
    failures: u32,
) -> u32 {
    let Some(current) = current else {
        return std::future::pending().await;
    };
    match hold_register(current.relay_url(), current.secret(), machine_api.clone()).await {
        Ok(mut hold) => {
            let _ = (&mut hold.task).await;
            0
        }
        Err(error) => {
            tracing::warn!(error = %error, "relay Register retry");
            tokio::time::sleep(register_retry_delay(failures)).await;
            failures.saturating_add(1)
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
    use ployz::connect::{ConnectError, connect_relay};
    use ployz_core::{
        CloudPairing, DescribeContractRequest, MachineId, MachineTarget, PairingCredential,
        RpcErrorCode, op,
    };
    use ployz_relay::{
        ClientError, DialCredential, PairingCredential as RelayPairing, Relay, RelayClient,
    };
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    use super::{Error, hold_register, register_retry_delay, run};
    use crate::{machine::LocalMachineStore, machine_api::MachineApi};

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
    async fn cloud_dial_with_unknown_machine_target_does_not_run_on_the_entry() {
        let session = Session::start().await;
        let mut client = connect_relay(
            &session.url,
            DialCredential::parse(DIAL).unwrap(),
            RelayPairing::parse(PAIRING).unwrap(),
            session.machine_id,
        )
        .await
        .unwrap();
        let other = MachineId::random();
        let error = client
            .call::<op::DescribeContract>(
                DescribeContractRequest {},
                Some(&MachineTarget::from(&other)),
            )
            .await
            .expect_err("unknown Machine Target must not execute on the Relay entry");
        let ConnectError::Rpc(transport) = error else {
            panic!("expected InvalidArgument transport, got {error:?}");
        };
        let error = transport.to_rpc_error();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        assert!(
            error.message.contains("Machine selectors were not found"),
            "{}",
            error.message
        );
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
        let error = match hold_register("not-a-url", &secret(), test_api().1).await {
            Ok(_) => panic!("expected unreachable Relay to fail"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::Client(_)));
    }

    #[tokio::test]
    async fn cloud_pairing_hold_appears_on_list_held() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (_pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        wait_for_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn no_cloud_pairing_stays_off_list_held() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (_pairing_tx, pairing_rx) = watch::channel(None);
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        assert_not_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn cloud_pairing_arriving_later_appears_on_list_held() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) = watch::channel(None);
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        assert_not_held(&relay.url, machine_id).await;
        pairing_tx.send_replace(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        wait_for_held(&relay.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn clearing_cloud_pairing_leaves_list() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
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
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&first.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        wait_for_held(&first.url, machine_id).await;
        pairing_tx.send_replace(Some(CloudPairing::parse(&second.url, secret()).unwrap()));
        wait_for_held(&second.url, machine_id).await;
        assert_not_held(&first.url, machine_id).await;
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unreachable_cloud_pairing_retries_until_shutdown() {
        let pairing = CloudPairing::parse("not-a-url", secret()).unwrap();
        let shutdown = CancellationToken::new();
        let (_pairing_tx, pairing_rx) = watch::channel(Some(pairing));
        let hold = tokio::spawn(run(pairing_rx, test_api().1, shutdown.clone()));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!hold.is_finished(), "unreachable Relay must not stop run");
        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn closed_cloud_pairing_watch_fails_run() {
        let pairing = CloudPairing::parse("not-a-url", secret()).unwrap();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) = watch::channel(Some(pairing));
        drop(pairing_tx);
        let error = run(pairing_rx, test_api().1, shutdown)
            .await
            .expect_err("closed watch must fail run");
        assert_eq!(error.to_string(), "cloud pairing watch closed");
    }

    #[tokio::test]
    async fn register_reconnects_after_stream_end() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (_pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        wait_for_held(&relay.url, machine_id).await;

        let displacer = hold_register(&relay.url, &secret(), test_api().1)
            .await
            .expect("second Register displaces the held stream");
        drop(displacer);
        wait_for_held(&relay.url, machine_id).await;

        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn register_reconnects_after_relay_restart() {
        let relay = RelayListen::start().await;
        let url = relay.url.clone();
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (_pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        wait_for_held(&url, machine_id).await;

        let relay = relay.restart().await;
        wait_for_held_within(&relay.url, machine_id, Duration::from_secs(5)).await;

        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn clearing_cloud_pairing_cancels_register_retries() {
        let relay = RelayListen::start().await;
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&relay.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        wait_for_held(&relay.url, machine_id).await;

        pairing_tx.send_replace(Some(
            CloudPairing::parse("http://127.0.0.1:1", secret()).unwrap(),
        ));
        pairing_tx.send_replace(None);
        let relay = relay.restart().await;
        assert_not_held(&relay.url, machine_id).await;

        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn replacing_cloud_pairing_cancels_retries_for_the_previous_pairing() {
        let first = RelayListen::start().await;
        let second = RelayListen::start().await;
        let (machine_id, service) = test_api();
        let shutdown = CancellationToken::new();
        let (pairing_tx, pairing_rx) =
            watch::channel(Some(CloudPairing::parse(&first.url, secret()).unwrap()));
        let hold = tokio::spawn(run(pairing_rx, service, shutdown.clone()));
        wait_for_held(&first.url, machine_id).await;

        pairing_tx.send_replace(Some(
            CloudPairing::parse("http://127.0.0.1:1", secret()).unwrap(),
        ));
        pairing_tx.send_replace(Some(CloudPairing::parse(&second.url, secret()).unwrap()));
        wait_for_held(&second.url, machine_id).await;
        let first = first.restart().await;
        assert_not_held(&first.url, machine_id).await;

        shutdown.cancel();
        hold.await.unwrap().unwrap();
    }

    #[test]
    fn register_retry_delay_doubles_until_the_cap() {
        assert_eq!(register_retry_delay(0), Duration::from_secs(1));
        assert_eq!(register_retry_delay(1), Duration::from_secs(2));
        assert_eq!(register_retry_delay(4), Duration::from_secs(16));
        assert_eq!(register_retry_delay(5), Duration::from_secs(30));
        assert_eq!(register_retry_delay(100), Duration::from_secs(30));
    }

    struct RelayListen {
        url: String,
        address: std::net::SocketAddr,
        server: tokio::task::JoinHandle<std::io::Result<()>>,
        goaway: ployz_relay::Goaway,
    }

    impl RelayListen {
        async fn start() -> Self {
            Self::bind((Ipv4Addr::LOCALHOST, 0).into()).await
        }

        async fn bind(listen: std::net::SocketAddr) -> Self {
            loop {
                match Relay::new(DialCredential::parse(DIAL).unwrap())
                    .serve_with_drain(listen, Duration::from_millis(10))
                    .await
                {
                    Ok((address, server, goaway)) => {
                        return Self {
                            url: format!("http://{address}"),
                            address,
                            server,
                            goaway,
                        };
                    }
                    Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                }
            }
        }

        async fn restart(self) -> Self {
            let listen = self.address;
            self.goaway.start();
            let _ = self.server.await;
            Self::bind(listen).await
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
            let (machine_id, api) = test_api();
            let hold = hold_register(&relay.url, &secret(), api)
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

    fn test_api() -> (MachineId, MachineApi) {
        let data_dir = std::env::temp_dir().join(format!("ployzd-relay-{}", MachineId::random()));
        let store = Arc::new(Mutex::new(LocalMachineStore::open(&data_dir).unwrap()));
        let machine_id = store.lock().unwrap().record().id();
        let (reset, _) = watch::channel(false);
        (
            machine_id,
            MachineApi::builder(store, reset)
                .build()
                .expect("test Machine record is readable"),
        )
    }

    async fn wait_for_held(url: &str, machine_id: MachineId) {
        wait_for_held_within(url, machine_id, Duration::from_secs(2)).await;
    }

    async fn wait_for_held_within(url: &str, machine_id: MachineId, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            match list(url, DIAL, PAIRING).await {
                Ok(listed)
                    if listed.iter().any(|row| {
                        row.machine_id().ok() == Some(machine_id) && row.register_rtt_ns.is_some()
                    }) =>
                {
                    return;
                }
                Ok(_) | Err(_) => {}
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "expected {machine_id} on List with path RTT"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn assert_not_held(url: &str, machine_id: MachineId) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            match list(url, DIAL, PAIRING).await {
                Ok(listed)
                    if listed
                        .iter()
                        .all(|row| row.machine_id().ok() != Some(machine_id)) =>
                {
                    break;
                }
                Ok(_) | Err(_) => {}
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "expected {machine_id} off List"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
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
