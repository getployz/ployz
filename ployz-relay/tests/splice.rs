use std::{collections::HashSet, net::Ipv4Addr, time::Duration};

use ployz_core::{InspectRequest, MachineId, OpaquePayload, TunnelId, op};
use ployz_relay::{
    AUTHORIZATION_METADATA, CloudRelayClient, DialCredential, ListRequest, MACHINE_ID_METADATA,
    Open, PAIRING_METADATA, RegisterRequest, Relay, TUNNEL_ID_METADATA, TunnelFrame,
};
use tokio::{net::TcpListener, sync::mpsc, time::timeout};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, metadata::MetadataValue, transport::Endpoint};

const PAIRING: &str = "pairing-secret";
const DIAL: &str = "dial-secret";
const PAIRING_A: &str = "pairing-a";
const PAIRING_B: &str = "pairing-b";
const TEST_DRAIN: Duration = Duration::from_millis(300);

#[tokio::test]
async fn serve_binds_the_requested_address() {
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let listen = probe.local_addr().unwrap();
    drop(probe);
    let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
    let (bound, _server, _goaway) = relay.serve(listen).await.unwrap();
    assert_eq!(bound, listen);
}

#[tokio::test]
async fn register_dial_attach_splices_bytes_both_ways() {
    let session = Session::start().await;
    let (_hold, dial_tx, mut dial_in, attach_tx, mut attach_in) = session.splice().await;

    dial_tx
        .send(TunnelFrame::new(b"cloud-to-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach_in).await.data, b"cloud-to-machine");

    attach_tx
        .send(TunnelFrame::new(b"machine-to-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut dial_in).await.data, b"machine-to-cloud");
}

#[tokio::test]
async fn two_dial_attach_pairs_on_one_register_splice_independently() {
    let mut session = Session::start().await;
    session.register().await;
    let (a_dial_tx, mut a_dial_in, a_attach_tx, mut a_attach_in) = session.accept_tunnel().await;
    let (b_dial_tx, mut b_dial_in, b_attach_tx, mut b_attach_in) = session.accept_tunnel().await;

    a_dial_tx
        .send(TunnelFrame::new(b"a-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_attach_in).await.data, b"a-cloud");

    b_dial_tx
        .send(TunnelFrame::new(b"b-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_attach_in).await.data, b"b-cloud");

    a_attach_tx
        .send(TunnelFrame::new(b"a-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_dial_in).await.data, b"a-machine");

    b_attach_tx
        .send(TunnelFrame::new(b"b-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_dial_in).await.data, b"b-machine");
}

#[tokio::test]
async fn second_register_for_the_same_machine_replaces_the_first() {
    let mut session = Session::start().await;
    session.register().await;
    let mut old_opens = session.opens.take().unwrap();

    session.register().await;

    assert_opens_close(&mut old_opens).await;

    let (dial_tx, mut dial_in, attach_tx, mut attach_in) = session.accept_tunnel().await;
    dial_tx
        .send(TunnelFrame::new(b"after-replace".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach_in).await.data, b"after-replace");
    attach_tx
        .send(TunnelFrame::new(b"machine-after-replace".to_vec()))
        .await
        .unwrap();
    assert_eq!(
        recv_frame(&mut dial_in).await.data,
        b"machine-after-replace"
    );
}

#[tokio::test]
async fn register_rejects_dial_credential() {
    let mut session = Session::start().await;
    let (tx, rx) = mpsc::channel(4);
    tx.send(RegisterRequest::new(&session.machine_id))
        .await
        .unwrap();
    let mut request = Request::new(ReceiverStream::new(rx));
    set_bearer(request.metadata_mut(), DIAL);
    let error = session.machine.register(request).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn attach_of_unknown_tunnel_id_fails_closed() {
    let mut session = Session::start().await;
    session.register().await;
    let mut request = Request::new(ReceiverStream::new(mpsc::channel(1).1));
    request.metadata_mut().insert(
        TUNNEL_ID_METADATA,
        TunnelId::random()
            .as_str()
            .parse()
            .expect("Tunnel ID is ASCII metadata"),
    );
    let error = session.machine.attach(request).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn tunnels_on_the_replaced_register_close() {
    let mut session = Session::start().await;
    session.register().await;
    let (_dial_tx, mut dial_in, _attach_tx, mut attach_in) = session.accept_tunnel().await;

    session.register().await;

    assert_stream_closes(&mut dial_in).await;
    assert_stream_closes(&mut attach_in).await;
}

#[tokio::test]
async fn dial_rejects_an_invalid_machine_id() {
    let mut session = Session::start().await;
    session.register().await;
    let mut request = dial_request(DIAL, PAIRING, &session.machine_id, mpsc::channel(4).1);
    request.metadata_mut().insert(
        MACHINE_ID_METADATA,
        "not-a-machine-id"
            .parse()
            .expect("invalid id text is ASCII metadata"),
    );
    let error = session.cloud.dial(request).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn attach_rejects_an_invalid_tunnel_id() {
    let mut session = Session::start().await;
    session.register().await;
    let mut request = Request::new(ReceiverStream::new(mpsc::channel(1).1));
    request.metadata_mut().insert(
        TUNNEL_ID_METADATA,
        "not-a-tunnel-id"
            .parse()
            .expect("invalid id text is ASCII metadata"),
    );
    let error = session.machine.attach(request).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn pairing_credential_is_rejected_on_dial() {
    let mut session = Session::start().await;
    session.register().await;
    let error = session
        .cloud
        .dial(dial_request(
            PAIRING,
            PAIRING,
            &session.machine_id,
            mpsc::channel(4).1,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn dial_without_pairing_metadata_fails_closed() {
    let mut session = Session::start().await;
    session.register().await;
    let mut request = Request::new(ReceiverStream::new(mpsc::channel(4).1));
    set_bearer(request.metadata_mut(), DIAL);
    request.metadata_mut().insert(
        MACHINE_ID_METADATA,
        session
            .machine_id
            .as_str()
            .parse()
            .expect("Machine ID is ASCII metadata"),
    );
    let error = session.cloud.dial(request).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn revoke_drops_pairing_so_register_fails_and_is_idempotent() {
    let mut session = Session::start().await;
    session.register().await;

    session
        .cloud
        .revoke(revoke_request(DIAL, PAIRING))
        .await
        .unwrap();

    let (tx, rx) = mpsc::channel(4);
    tx.send(RegisterRequest::new(&MachineId::random()))
        .await
        .unwrap();
    let mut register = Request::new(ReceiverStream::new(rx));
    set_bearer(register.metadata_mut(), PAIRING);
    let error = session.machine.register(register).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);

    session
        .cloud
        .revoke(revoke_request(DIAL, PAIRING))
        .await
        .unwrap();
}

#[tokio::test]
async fn pairing_credential_cannot_revoke() {
    let mut session = Session::start().await;
    let error = session
        .cloud
        .revoke(revoke_request(PAIRING, PAIRING))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn revoke_keeps_dial_on_an_existing_register() {
    let (session, dial_tx, _dial_in, _attach_tx, mut attach_in) =
        Session::start().await.splice().await;
    session
        .cloud
        .clone()
        .revoke(revoke_request(DIAL, PAIRING))
        .await
        .unwrap();
    dial_tx
        .send(TunnelFrame::new(b"after-revoke".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach_in).await.data, b"after-revoke");
}

#[tokio::test]
async fn dial_of_unknown_machine_id_fails_closed() {
    let mut session = Session::start().await;
    let error = session
        .cloud
        .dial(dial_request(
            DIAL,
            PAIRING,
            &session.machine_id,
            mpsc::channel(4).1,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn tunnel_bytes_are_not_interpreted_as_machine_rpc() {
    let session = Session::start().await;
    let (_hold, dial_tx, mut dial_in, attach_tx, mut attach_in) = session.splice().await;
    let rpc = op::Inspect::into_request(InspectRequest::default())
        .encode()
        .unwrap();
    let not_rpc = b"\x00\xffnot-a-machine-rpc".to_vec();

    dial_tx
        .send(TunnelFrame::new(rpc.json.clone()))
        .await
        .unwrap();
    let received = recv_frame(&mut attach_in).await.data;
    assert_eq!(received, rpc.json);
    assert_eq!(
        OpaquePayload::new(received).decode_request().unwrap().body,
        rpc.decode_request().unwrap().body
    );

    attach_tx
        .send(TunnelFrame::new(not_rpc.clone()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut dial_in).await.data, not_rpc);
}

#[tokio::test]
async fn after_goaway_a_new_attach_is_refused() {
    let mut session = Session::start().await;
    session.register().await;
    session.goaway();
    let error = session
        .machine
        .attach(attach_request(&TunnelId::random()))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn after_goaway_a_new_list_is_refused() {
    let mut session = Session::start().await;
    session.register().await;
    session.goaway();
    let error = session
        .cloud
        .list(list_request(DIAL, PAIRING))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn in_flight_tunnel_continues_until_drain_then_closes() {
    let session = Session::start_with_drain(TEST_DRAIN).await;
    let (mut session, dial_tx, mut dial_in, attach_tx, mut attach_in) = session.splice().await;
    session.goaway();

    dial_tx
        .send(TunnelFrame::new(b"after-goaway".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach_in).await.data, b"after-goaway");
    attach_tx
        .send(TunnelFrame::new(b"still-open".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut dial_in).await.data, b"still-open");

    assert_stream_closes(&mut attach_in).await;
    assert_stream_closes(&mut dial_in).await;
}

#[tokio::test]
async fn same_machine_id_on_two_pairings_splices_independently() {
    let machine_id = colliding_machine_id();
    let relay = serve_relay().await;
    let mut machine_a = connect(relay.address).await;
    let mut machine_b = connect(relay.address).await;
    let mut cloud_a = connect(relay.address).await;
    let mut cloud_b = connect(relay.address).await;
    let (_hold_a, mut opens_a) = hold_register(&mut machine_a, PAIRING_A, &machine_id).await;
    let (_hold_b, mut opens_b) = hold_register(&mut machine_b, PAIRING_B, &machine_id).await;

    let (a_dial_tx, mut a_dial_in, a_attach_tx, mut a_attach_in, _) = accept_on(
        &mut cloud_a,
        &mut machine_a,
        &mut opens_a,
        PAIRING_A,
        &machine_id,
    )
    .await;
    let (b_dial_tx, mut b_dial_in, b_attach_tx, mut b_attach_in, _) = accept_on(
        &mut cloud_b,
        &mut machine_b,
        &mut opens_b,
        PAIRING_B,
        &machine_id,
    )
    .await;

    a_dial_tx
        .send(TunnelFrame::new(b"a-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_attach_in).await.data, b"a-cloud");
    b_dial_tx
        .send(TunnelFrame::new(b"b-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_attach_in).await.data, b"b-cloud");

    a_attach_tx
        .send(TunnelFrame::new(b"a-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_dial_in).await.data, b"a-machine");
    b_attach_tx
        .send(TunnelFrame::new(b"b-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_dial_in).await.data, b"b-machine");
}

#[tokio::test]
async fn dial_and_list_do_not_see_another_pairings_register() {
    let machine_id = colliding_machine_id();
    let relay = serve_relay().await;
    let mut machine_a = connect(relay.address).await;
    let mut cloud_b = connect(relay.address).await;
    let (_hold_a, mut opens_a) = hold_register(&mut machine_a, PAIRING_A, &machine_id).await;

    let error = cloud_b
        .dial(dial_request(
            DIAL,
            PAIRING_B,
            &machine_id,
            mpsc::channel(4).1,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound);

    let listed = cloud_b
        .list(list_request(DIAL, PAIRING_B))
        .await
        .unwrap()
        .into_inner();
    assert!(listed.registers().is_empty());

    assert!(
        timeout(Duration::from_millis(200), opens_a.recv())
            .await
            .is_err(),
        "Dial on another pairing must not Open this Register"
    );
}

#[tokio::test]
async fn replacing_one_pairing_register_does_not_drop_the_other() {
    let machine_id = colliding_machine_id();
    let relay = serve_relay().await;
    let mut machine_a = connect(relay.address).await;
    let mut machine_b = connect(relay.address).await;
    let mut cloud_a = connect(relay.address).await;
    let mut cloud_b = connect(relay.address).await;
    let (_old_a, mut old_opens_a) = hold_register(&mut machine_a, PAIRING_A, &machine_id).await;
    let (_hold_b, mut opens_b) = hold_register(&mut machine_b, PAIRING_B, &machine_id).await;
    let (_new_a, mut opens_a) = hold_register(&mut machine_a, PAIRING_A, &machine_id).await;

    assert_opens_close(&mut old_opens_a).await;

    let (a_dial_tx, _a_dial_in, _a_attach_tx, mut a_attach_in, _) = accept_on(
        &mut cloud_a,
        &mut machine_a,
        &mut opens_a,
        PAIRING_A,
        &machine_id,
    )
    .await;
    let (b_dial_tx, _b_dial_in, _b_attach_tx, mut b_attach_in, _) = accept_on(
        &mut cloud_b,
        &mut machine_b,
        &mut opens_b,
        PAIRING_B,
        &machine_id,
    )
    .await;

    a_dial_tx
        .send(TunnelFrame::new(b"a-after-replace".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_attach_in).await.data, b"a-after-replace");
    b_dial_tx
        .send(TunnelFrame::new(b"b-still-here".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_attach_in).await.data, b"b-still-here");
}

#[tokio::test]
async fn list_empty_pairing_is_success_and_two_held_registers_return_both_ids() {
    let relay = serve_relay().await;
    let mut cloud = connect(relay.address).await;
    let empty = cloud
        .list(list_request(DIAL, PAIRING))
        .await
        .unwrap()
        .into_inner();
    assert!(empty.registers().is_empty());

    let first = MachineId::random();
    let second = MachineId::random();
    let mut machine_a = connect(relay.address).await;
    let mut machine_b = connect(relay.address).await;
    let _hold_a = hold_register(&mut machine_a, PAIRING, &first).await;
    let _hold_b = hold_register(&mut machine_b, PAIRING, &second).await;

    let listed = cloud
        .list(list_request(DIAL, PAIRING))
        .await
        .unwrap()
        .into_inner();
    let ids: HashSet<_> = listed
        .registers()
        .iter()
        .map(|row| row.machine_id().unwrap())
        .collect();
    assert_eq!(ids, HashSet::from([first, second]));
}

#[tokio::test]
async fn echo_fills_register_rtt_after_a_round_trip() {
    let mut session = Session::start().await;
    session.register().await;
    let ns = wait_for_rtt(&mut session.cloud, PAIRING, session.machine_id).await;
    assert!(ns.is_some(), "path RTT must be present after a pong");
}

#[tokio::test]
async fn list_without_pong_omits_register_rtt() {
    let relay = serve_relay().await;
    let mut machine = connect(relay.address).await;
    let mut cloud = connect(relay.address).await;
    let machine_id = MachineId::random();
    let _hold = start_register(&mut machine, PAIRING, &machine_id).await;

    let listed = cloud
        .list(list_request(DIAL, PAIRING))
        .await
        .unwrap()
        .into_inner();
    let row = listed
        .registers()
        .first()
        .expect("silent Register is listed");
    assert_eq!(row.machine_id().unwrap(), machine_id);
    assert_eq!(row.register_rtt_ns, None);
}

#[tokio::test]
async fn register_after_close_is_a_new_session() {
    let session = Session::start_with_drain(TEST_DRAIN).await;
    let machine_id = session.machine_id;
    let (mut session, _dial_tx, mut dial_in, _attach_tx, mut attach_in) = session.splice().await;
    let old_tunnel = session.tunnel_id.expect("splice opens a tunnel");
    session.goaway();
    session.wait_closed().await;
    assert_stream_closes(&mut attach_in).await;
    assert_stream_closes(&mut dial_in).await;

    let mut next = Session::start_with_machine(machine_id).await;
    let error = next
        .machine
        .attach(attach_request(&old_tunnel))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::NotFound);
    let (_hold, dial_tx, _dial_in, _attach_tx, mut attach_in) = next.splice().await;
    dial_tx
        .send(TunnelFrame::new(b"new-session".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach_in).await.data, b"new-session");
}

struct Session {
    server: Option<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>>,
    goaway: Option<ployz_relay::Goaway>,
    machine: CloudRelayClient<tonic::transport::Channel>,
    cloud: CloudRelayClient<tonic::transport::Channel>,
    machine_id: MachineId,
    register_hold: Option<mpsc::Sender<RegisterRequest>>,
    opens: Option<mpsc::Receiver<Open>>,
    tunnel_id: Option<TunnelId>,
}

impl Session {
    async fn start() -> Self {
        Self::bind(None, MachineId::random()).await
    }

    async fn start_with_drain(drain: Duration) -> Self {
        Self::bind(Some(drain), MachineId::random()).await
    }

    async fn start_with_machine(machine_id: MachineId) -> Self {
        Self::bind(None, machine_id).await
    }

    async fn bind(drain: Option<Duration>, machine_id: MachineId) -> Self {
        let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
        let listen = (Ipv4Addr::LOCALHOST, 0).into();
        let (address, server, goaway) = match drain {
            None => relay.serve(listen).await.unwrap(),
            Some(drain) => relay.serve_with_drain(listen, drain).await.unwrap(),
        };
        Self {
            server: Some(server),
            goaway: Some(goaway),
            machine: connect(address).await,
            cloud: connect(address).await,
            machine_id,
            register_hold: None,
            opens: None,
            tunnel_id: None,
        }
    }

    fn goaway(&mut self) {
        if let Some(goaway) = self.goaway.take() {
            goaway.start();
        }
    }

    async fn wait_closed(&mut self) {
        let server = self.server.take().expect("serve task still running");
        timeout(Duration::from_secs(2), server)
            .await
            .expect("relay did not close after drain")
            .unwrap()
            .unwrap();
    }

    async fn register(&mut self) {
        let (tx, rx) = mpsc::channel(4);
        tx.send(RegisterRequest::new(&self.machine_id))
            .await
            .unwrap();
        let mut request = Request::new(ReceiverStream::new(rx));
        set_bearer(request.metadata_mut(), PAIRING);
        let stream = self.machine.register(request).await.unwrap().into_inner();
        self.register_hold = Some(tx.clone());
        self.opens = Some(forward_opens(tx, stream));
    }

    async fn splice(
        mut self,
    ) -> (
        Self,
        mpsc::Sender<TunnelFrame>,
        tonic::Streaming<TunnelFrame>,
        mpsc::Sender<TunnelFrame>,
        tonic::Streaming<TunnelFrame>,
    ) {
        self.register().await;
        let (dial_tx, dial_in, attach_tx, attach_in) = self.accept_tunnel().await;
        (self, dial_tx, dial_in, attach_tx, attach_in)
    }

    async fn accept_tunnel(
        &mut self,
    ) -> (
        mpsc::Sender<TunnelFrame>,
        tonic::Streaming<TunnelFrame>,
        mpsc::Sender<TunnelFrame>,
        tonic::Streaming<TunnelFrame>,
    ) {
        let opens = self.opens.as_mut().expect("Register is held");
        let (dial_tx, dial_in, attach_tx, attach_in, tunnel_id) = accept_on(
            &mut self.cloud,
            &mut self.machine,
            opens,
            PAIRING,
            &self.machine_id,
        )
        .await;
        self.tunnel_id = Some(tunnel_id);
        (dial_tx, dial_in, attach_tx, attach_in)
    }
}

async fn connect(address: std::net::SocketAddr) -> CloudRelayClient<tonic::transport::Channel> {
    let channel = Endpoint::from_shared(format!("http://{address}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .unwrap();
    CloudRelayClient::new(channel)
}

fn colliding_machine_id() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

struct SharedRelay {
    address: std::net::SocketAddr,
    _server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    _goaway: ployz_relay::Goaway,
}

async fn serve_relay() -> SharedRelay {
    let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
    let listen = (Ipv4Addr::LOCALHOST, 0).into();
    let (address, server, goaway) = relay.serve(listen).await.unwrap();
    SharedRelay {
        address,
        _server: server,
        _goaway: goaway,
    }
}

async fn hold_register(
    client: &mut CloudRelayClient<tonic::transport::Channel>,
    pairing: &str,
    machine_id: &MachineId,
) -> (mpsc::Sender<RegisterRequest>, mpsc::Receiver<Open>) {
    let (tx, stream) = start_register(client, pairing, machine_id).await;
    let opens = forward_opens(tx.clone(), stream);
    (tx, opens)
}

async fn start_register(
    client: &mut CloudRelayClient<tonic::transport::Channel>,
    pairing: &str,
    machine_id: &MachineId,
) -> (mpsc::Sender<RegisterRequest>, tonic::Streaming<Open>) {
    let (tx, rx) = mpsc::channel(4);
    tx.send(RegisterRequest::new(machine_id)).await.unwrap();
    let mut request = Request::new(ReceiverStream::new(rx));
    set_bearer(request.metadata_mut(), pairing);
    let stream = client.register(request).await.unwrap().into_inner();
    (tx, stream)
}

fn forward_opens(
    hold: mpsc::Sender<RegisterRequest>,
    mut stream: tonic::Streaming<Open>,
) -> mpsc::Receiver<Open> {
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            if let Some(nonce) = message.ping_nonce() {
                let _ = hold.send(RegisterRequest::pong(nonce)).await;
            } else if tx.send(message).await.is_err() {
                break;
            }
        }
    });
    rx
}

async fn wait_for_rtt(
    cloud: &mut CloudRelayClient<tonic::transport::Channel>,
    pairing: &str,
    machine_id: MachineId,
) -> Option<i64> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let listed = cloud
            .list(list_request(DIAL, pairing))
            .await
            .unwrap()
            .into_inner();
        if let Some(row) = listed
            .registers()
            .iter()
            .find(|row| row.machine_id().ok() == Some(machine_id))
            && row.register_rtt_ns.is_some()
        {
            return row.register_rtt_ns;
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn accept_on(
    cloud: &mut CloudRelayClient<tonic::transport::Channel>,
    machine: &mut CloudRelayClient<tonic::transport::Channel>,
    opens: &mut mpsc::Receiver<Open>,
    pairing: &str,
    machine_id: &MachineId,
) -> (
    mpsc::Sender<TunnelFrame>,
    tonic::Streaming<TunnelFrame>,
    mpsc::Sender<TunnelFrame>,
    tonic::Streaming<TunnelFrame>,
    TunnelId,
) {
    let (dial_tx, dial_rx) = mpsc::channel(4);
    let dial_in = cloud
        .dial(dial_request(DIAL, pairing, machine_id, dial_rx))
        .await
        .unwrap()
        .into_inner();
    let open = timeout(Duration::from_secs(2), opens.recv())
        .await
        .expect("Open timed out")
        .expect("Open closed");
    let tunnel_id = open.tunnel_id().expect("Open carries a Tunnel ID");
    let (attach_tx, attach_rx) = mpsc::channel(4);
    let mut request = Request::new(ReceiverStream::new(attach_rx));
    request.metadata_mut().insert(
        TUNNEL_ID_METADATA,
        tunnel_id
            .as_str()
            .parse()
            .expect("Tunnel ID is ASCII metadata"),
    );
    let attach_in = machine.attach(request).await.unwrap().into_inner();
    (dial_tx, dial_in, attach_tx, attach_in, tunnel_id)
}

fn dial_request(
    credential: &str,
    pairing: &str,
    machine_id: &MachineId,
    rx: mpsc::Receiver<TunnelFrame>,
) -> Request<ReceiverStream<TunnelFrame>> {
    let mut request = Request::new(ReceiverStream::new(rx));
    set_bearer(request.metadata_mut(), credential);
    set_pairing(request.metadata_mut(), pairing);
    request.metadata_mut().insert(
        MACHINE_ID_METADATA,
        machine_id
            .as_str()
            .parse()
            .expect("Machine ID is ASCII metadata"),
    );
    request
}

fn list_request(credential: &str, pairing: &str) -> Request<ListRequest> {
    let mut request = Request::new(ListRequest {});
    set_bearer(request.metadata_mut(), credential);
    set_pairing(request.metadata_mut(), pairing);
    request
}

fn revoke_request(credential: &str, pairing: &str) -> Request<ployz_relay::RevokeRequest> {
    let mut request = Request::new(ployz_relay::RevokeRequest {});
    set_bearer(request.metadata_mut(), credential);
    set_pairing(request.metadata_mut(), pairing);
    request
}

fn set_bearer(metadata: &mut tonic::metadata::MetadataMap, credential: &str) {
    metadata.insert(
        AUTHORIZATION_METADATA,
        MetadataValue::try_from(format!("Bearer {credential}")).expect("bearer is ASCII"),
    );
}

fn set_pairing(metadata: &mut tonic::metadata::MetadataMap, pairing: &str) {
    metadata.insert(
        PAIRING_METADATA,
        pairing.parse().expect("pairing is ASCII metadata"),
    );
}

async fn recv_frame(stream: &mut tonic::Streaming<TunnelFrame>) -> TunnelFrame {
    timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("frame timed out")
        .unwrap()
        .unwrap()
}

fn attach_request(tunnel_id: &TunnelId) -> Request<ReceiverStream<TunnelFrame>> {
    let mut request = Request::new(ReceiverStream::new(mpsc::channel(1).1));
    request.metadata_mut().insert(
        TUNNEL_ID_METADATA,
        tunnel_id
            .as_str()
            .parse()
            .expect("Tunnel ID is ASCII metadata"),
    );
    request
}

async fn assert_stream_closes<T>(stream: &mut tonic::Streaming<T>) {
    match timeout(Duration::from_secs(2), stream.next()).await {
        Ok(None) | Ok(Some(Err(_))) => {}
        Ok(Some(Ok(_))) => panic!("stream stayed open"),
        Err(_) => panic!("stream did not close"),
    }
}

async fn assert_opens_close(opens: &mut mpsc::Receiver<Open>) {
    match timeout(Duration::from_secs(2), opens.recv()).await {
        Ok(None) => {}
        Ok(Some(_)) => panic!("stream stayed open"),
        Err(_) => panic!("stream did not close"),
    }
}
