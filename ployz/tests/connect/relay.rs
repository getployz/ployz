use std::{
    io,
    net::Ipv4Addr,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use ployz::connect::{ConnectError, DialCredential, connect_relay};
use ployz_core::{DescribeContractRequest, MachineId, MachineRpcServer, op};
use ployz_relay::{
    ClientError, Open, PairingCredential, RegisterRequest, Relay, RelayClient, RelayWs, TunnelIo,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::timeout,
};
use tonic::transport::server::Connected;

use super::support::{DiscoveryService, test_description};

pub(super) const PAIRING: &str = "pairing-secret";
pub(super) const DIAL: &str = "dial-secret";

#[tokio::test]
async fn client_rpc_round_trip_through_relay_attach() {
    let description = test_description();
    let machine_id = description.machine_id;
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(machine_id, DiscoveryService::new(description.clone()))
        .await;

    let mut client = connect_relay(
        &session.url,
        dial_credential(),
        pairing_credential(),
        machine_id,
    )
    .await
    .unwrap();

    assert_eq!(
        client
            .call::<op::DescribeContract>(DescribeContractRequest {}, None)
            .await
            .unwrap(),
        description
    );
}

#[tokio::test]
async fn bad_dial_credential_fails_closed() {
    let machine_id = MachineId::random();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(machine_id, DiscoveryService::new(test_description()))
        .await;
    let bad = DialCredential::parse("wrong-secret").unwrap();

    let result = timeout(
        Duration::from_secs(2),
        connect_relay(&session.url, bad, pairing_credential(), machine_id),
    )
    .await
    .expect("bad Dial Credential must not hang");
    let error = match result {
        Ok(_) => panic!("expected invalid Dial Credential to fail"),
        Err(error) => error,
    };

    assert!(
        matches!(error, ConnectError::InvalidDialCredential),
        "{error:?}"
    );
}

#[tokio::test]
async fn unknown_machine_id_fails_closed() {
    let registered = MachineId::random();
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(registered, DiscoveryService::new(test_description()))
        .await;

    let result = timeout(
        Duration::from_secs(2),
        connect_relay(
            &session.url,
            dial_credential(),
            pairing_credential(),
            MachineId::random(),
        ),
    )
    .await
    .expect("unknown Machine ID must not hang");
    let error = match result {
        Ok(_) => panic!("expected unknown Machine ID to fail"),
        Err(error) => error,
    };

    assert!(matches!(error, ConnectError::UnknownMachine), "{error:?}");
}

pub(super) struct RelaySession {
    pub(super) url: String,
    _server: tokio::task::JoinHandle<io::Result<()>>,
}

impl RelaySession {
    pub(super) async fn start() -> Self {
        let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
        let listen = (Ipv4Addr::LOCALHOST, 0).into();
        let (address, server, _) = relay.serve(listen).await.unwrap();
        Self {
            url: format!("http://{address}"),
            _server: server,
        }
    }

    pub(super) async fn spawn_machine(
        &self,
        machine_id: MachineId,
        service: DiscoveryService,
    ) -> FakeMachine {
        FakeMachine::register(&self.url, machine_id, service).await
    }
}

pub(super) struct FakeMachine {
    _accept: tokio::task::JoinHandle<()>,
}

impl FakeMachine {
    async fn register(url: &str, machine_id: MachineId, service: DiscoveryService) -> Self {
        let client = RelayClient::new(url).expect("test Relay URL is http");
        let mut register = client.register(PAIRING, &machine_id).await.unwrap();
        let url = url.to_owned();
        let accept = tokio::spawn(async move {
            while let Ok(Some(open)) = register.recv::<Open>().await {
                if let Some(nonce) = open.ping_nonce() {
                    let _ = register.send(&RegisterRequest::pong(nonce)).await;
                    continue;
                }
                let url = url.clone();
                let service = service.clone();
                tokio::spawn(async move {
                    serve_attach(&url, open, service).await;
                });
            }
        });
        Self { _accept: accept }
    }
}

async fn serve_attach(url: &str, open: Open, service: DiscoveryService) {
    let tunnel = RelayClient::new(url)
        .expect("test Relay URL is http")
        .attach(open.tunnel_id().expect("Open carries a Tunnel ID").as_str())
        .await
        .unwrap();
    let io = Incoming(tunnel.into_io());
    let _ = tonic::transport::Server::builder()
        .add_service(MachineRpcServer::new(service))
        .serve_with_incoming(tokio_stream::once(Ok::<_, io::Error>(io)))
        .await;
}

pub(super) async fn register_with_pairing(
    url: &str,
    pairing: &str,
    machine_id: MachineId,
) -> Result<RelayWs, ClientError> {
    RelayClient::new(url)?.register(pairing, &machine_id).await
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

fn pairing_credential() -> PairingCredential {
    PairingCredential::parse(PAIRING).unwrap()
}

fn dial_credential() -> DialCredential {
    DialCredential::parse(DIAL).unwrap()
}
