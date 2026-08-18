use std::{net::Ipv4Addr, time::Duration};

use ployz::connect::{ConnectError, DialCredential, connect_relay};
use ployz_core::{DescribeContractRequest, MachineId, MachineRpcServer, op};
use ployz_relay::{
    AUTHORIZATION_METADATA, CloudRelayClient, PairingCredential, RegisterRequest, Relay,
    TUNNEL_ID_METADATA, TunnelIo,
};
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, metadata::MetadataValue, transport::Endpoint};

use super::support::{DiscoveryService, test_description};

const PAIRING: &str = "pairing-secret";
const DIAL: &str = "dial-secret";

#[tokio::test]
async fn client_rpc_round_trip_through_relay_attach() {
    let description = test_description();
    let machine_id = description.machine_id;
    let session = RelaySession::start().await;
    let _machine = session
        .spawn_machine(machine_id, DiscoveryService::new(description.clone()))
        .await;

    let mut client = connect_relay(&session.url, dial_credential(), machine_id)
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
        connect_relay(&session.url, bad, machine_id),
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
        connect_relay(&session.url, dial_credential(), MachineId::random()),
    )
    .await
    .expect("unknown Machine ID must not hang");
    let error = match result {
        Ok(_) => panic!("expected unknown Machine ID to fail"),
        Err(error) => error,
    };

    assert!(matches!(error, ConnectError::UnknownMachine), "{error:?}");
}

struct RelaySession {
    url: String,
    _server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl RelaySession {
    async fn start() -> Self {
        let relay = Relay::new(
            PairingCredential::parse(PAIRING).unwrap(),
            DialCredential::parse(DIAL).unwrap(),
        )
        .unwrap();
        let listen = (Ipv4Addr::LOCALHOST, 0).into();
        let (address, server, _) = relay.serve(listen).await.unwrap();
        Self {
            url: format!("http://{address}"),
            _server: server,
        }
    }

    async fn spawn_machine(&self, machine_id: MachineId, service: DiscoveryService) -> FakeMachine {
        FakeMachine::register(&self.url, machine_id, service).await
    }
}

struct FakeMachine {
    _register_hold: mpsc::Sender<RegisterRequest>,
    _accept: tokio::task::JoinHandle<()>,
}

impl FakeMachine {
    async fn register(url: &str, machine_id: MachineId, service: DiscoveryService) -> Self {
        let mut relay = relay_client(url).await;
        let (hold, rx) = mpsc::channel(4);
        hold.send(RegisterRequest::new(&machine_id)).await.unwrap();
        let mut request = Request::new(ReceiverStream::new(rx));
        set_bearer(request.metadata_mut(), PAIRING);
        let mut opens = relay.register(request).await.unwrap().into_inner();
        let accept = tokio::spawn(async move {
            while let Some(Ok(open)) = opens.next().await {
                let mut attach_client = relay.clone();
                let service = service.clone();
                tokio::spawn(async move {
                    serve_attach(&mut attach_client, open, service).await;
                });
            }
        });
        Self {
            _register_hold: hold,
            _accept: accept,
        }
    }
}

async fn serve_attach(
    relay: &mut CloudRelayClient<tonic::transport::Channel>,
    open: ployz_relay::Open,
    service: DiscoveryService,
) {
    let (tx, rx) = mpsc::channel(16);
    let mut request = Request::new(ReceiverStream::new(rx));
    request.metadata_mut().insert(
        TUNNEL_ID_METADATA,
        open.tunnel_id()
            .expect("Open carries a Tunnel ID")
            .as_str()
            .parse()
            .expect("Tunnel ID is ASCII metadata"),
    );
    let inbound = relay.attach(request).await.unwrap().into_inner();
    let io = TunnelIo::new(tx, inbound);
    let _ = tonic::transport::Server::builder()
        .add_service(MachineRpcServer::new(service))
        .serve_with_incoming(tokio_stream::once(Ok::<_, std::io::Error>(io)))
        .await;
}

async fn relay_client(url: &str) -> CloudRelayClient<tonic::transport::Channel> {
    let channel = Endpoint::from_shared(url.to_owned())
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .unwrap();
    CloudRelayClient::new(channel)
}

fn set_bearer(metadata: &mut tonic::metadata::MetadataMap, credential: &str) {
    metadata.insert(
        AUTHORIZATION_METADATA,
        MetadataValue::try_from(format!("Bearer {credential}")).expect("bearer is ASCII"),
    );
}

fn dial_credential() -> DialCredential {
    DialCredential::parse(DIAL).unwrap()
}
