use std::{collections::HashSet, io, net::Ipv4Addr, time::Duration};

use http::StatusCode;
use ployz_core::{InspectRequest, MachineId, OpaquePayload, TunnelId, op};
use ployz_relay::{
    ClientError, DialCredential, Open, RegisterRequest, Relay, RelayClient, RelayWs, TunnelFrame,
};
use tokio::{sync::mpsc, time::timeout};

const PAIRING: &str = "pairing-secret";
const DIAL: &str = "dial-secret";
const PAIRING_A: &str = "pairing-a";
const PAIRING_B: &str = "pairing-b";
const TEST_DRAIN: Duration = Duration::from_millis(300);

#[tokio::test]
async fn serve_binds_the_requested_address() {
    let probe = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let listen = probe.local_addr().unwrap();
    drop(probe);
    let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
    let (bound, _server, _goaway) = relay.serve(listen).await.unwrap();
    assert_eq!(bound, listen);
}

#[tokio::test]
async fn register_dial_attach_splices_bytes_both_ways() {
    let session = Session::start().await;
    let (_session, mut dial, mut attach) = session.splice().await;

    dial.send(&TunnelFrame::new(b"cloud-to-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach).await.data, b"cloud-to-machine");

    attach
        .send(&TunnelFrame::new(b"machine-to-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut dial).await.data, b"machine-to-cloud");
}

#[tokio::test]
async fn two_dial_attach_pairs_on_one_register_splice_independently() {
    let mut session = Session::start().await;
    session.register().await;
    let (mut a_dial, mut a_attach) = session.accept_tunnel().await;
    let (mut b_dial, mut b_attach) = session.accept_tunnel().await;

    a_dial
        .send(&TunnelFrame::new(b"a-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_attach).await.data, b"a-cloud");

    b_dial
        .send(&TunnelFrame::new(b"b-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_attach).await.data, b"b-cloud");

    a_attach
        .send(&TunnelFrame::new(b"a-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_dial).await.data, b"a-machine");

    b_attach
        .send(&TunnelFrame::new(b"b-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_dial).await.data, b"b-machine");
}

#[tokio::test]
async fn second_register_for_the_same_machine_replaces_the_first() {
    let mut session = Session::start().await;
    session.register().await;
    let mut old_opens = session.opens.take().unwrap();

    session.register().await;

    assert_opens_close(&mut old_opens).await;

    let (mut dial, mut attach) = session.accept_tunnel().await;
    dial.send(&TunnelFrame::new(b"after-replace".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach).await.data, b"after-replace");
    attach
        .send(&TunnelFrame::new(b"machine-after-replace".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut dial).await.data, b"machine-after-replace");
}

#[tokio::test]
async fn register_rejects_dial_credential() {
    let session = Session::start().await;
    let error = session
        .machine
        .register(DIAL, &session.machine_id)
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn attach_of_unknown_tunnel_id_fails_closed() {
    let mut session = Session::start().await;
    session.register().await;
    let error = session
        .machine
        .attach(TunnelId::random().as_str())
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tunnels_on_the_replaced_register_close() {
    let mut session = Session::start().await;
    session.register().await;
    let (mut dial, mut attach) = session.accept_tunnel().await;

    session.register().await;

    assert_stream_closes(&mut dial).await;
    assert_stream_closes(&mut attach).await;
}

#[tokio::test]
async fn dial_rejects_an_invalid_machine_id() {
    let mut session = Session::start().await;
    session.register().await;
    let error = session
        .cloud
        .dial(DIAL, PAIRING, "not-a-machine-id")
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn attach_rejects_an_invalid_tunnel_id() {
    let mut session = Session::start().await;
    session.register().await;
    let error = session.machine.attach("not-a-tunnel-id").await.unwrap_err();
    assert_status(&error, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn pairing_credential_is_rejected_on_dial() {
    let mut session = Session::start().await;
    session.register().await;
    let error = session
        .cloud
        .dial(PAIRING, PAIRING, session.machine_id.as_str())
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dial_without_pairing_metadata_fails_closed() {
    let mut session = Session::start().await;
    session.register().await;
    let error = session
        .cloud
        .dial(DIAL, "", session.machine_id.as_str())
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn revoke_drops_pairing_so_register_fails_and_is_idempotent() {
    let mut session = Session::start().await;
    session.register().await;

    session.cloud.revoke(DIAL, PAIRING).await.unwrap();

    let error = session
        .machine
        .register(PAIRING, &MachineId::random())
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::UNAUTHORIZED);

    session.cloud.revoke(DIAL, PAIRING).await.unwrap();
}

#[tokio::test]
async fn pairing_credential_cannot_revoke() {
    let session = Session::start().await;
    let error = session.cloud.revoke(PAIRING, PAIRING).await.unwrap_err();
    assert_status(&error, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoke_keeps_dial_on_an_existing_register() {
    let (session, mut dial, mut attach) = Session::start().await.splice().await;
    session.cloud.revoke(DIAL, PAIRING).await.unwrap();
    dial.send(&TunnelFrame::new(b"after-revoke".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach).await.data, b"after-revoke");
}

#[tokio::test]
async fn dial_of_unknown_machine_id_fails_closed() {
    let session = Session::start().await;
    let error = session
        .cloud
        .dial(DIAL, PAIRING, session.machine_id.as_str())
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tunnel_bytes_are_not_interpreted_as_machine_rpc() {
    let session = Session::start().await;
    let (_session, mut dial, mut attach) = session.splice().await;
    let rpc = op::Inspect::into_request(InspectRequest::default())
        .encode()
        .unwrap();
    let not_rpc = b"\x00\xffnot-a-machine-rpc".to_vec();

    dial.send(&TunnelFrame::new(rpc.json.clone()))
        .await
        .unwrap();
    let received = recv_frame(&mut attach).await.data;
    assert_eq!(received, rpc.json);
    assert_eq!(
        OpaquePayload::new(received).decode_request().unwrap().body,
        rpc.decode_request().unwrap().body
    );

    attach
        .send(&TunnelFrame::new(not_rpc.clone()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut dial).await.data, not_rpc);
}

#[tokio::test]
async fn after_goaway_a_new_attach_is_refused() {
    let mut session = Session::start().await;
    session.register().await;
    session.goaway();
    let error = session
        .machine
        .attach(TunnelId::random().as_str())
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn after_goaway_a_new_list_is_refused() {
    let mut session = Session::start().await;
    session.register().await;
    session.goaway();
    let error = session.cloud.list(DIAL, PAIRING).await.unwrap_err();
    assert_status(&error, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn in_flight_tunnel_continues_until_drain_then_closes() {
    let session = Session::start_with_drain(TEST_DRAIN).await;
    let (mut session, mut dial, mut attach) = session.splice().await;
    session.goaway();

    dial.send(&TunnelFrame::new(b"after-goaway".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach).await.data, b"after-goaway");
    attach
        .send(&TunnelFrame::new(b"still-open".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut dial).await.data, b"still-open");

    assert_stream_closes(&mut attach).await;
    assert_stream_closes(&mut dial).await;
}

#[tokio::test]
async fn same_machine_id_on_two_pairings_splices_independently() {
    let machine_id = colliding_machine_id();
    let relay = serve_relay().await;
    let machine_a = client(&relay.url);
    let machine_b = client(&relay.url);
    let cloud_a = client(&relay.url);
    let cloud_b = client(&relay.url);
    let (_hold_a, mut opens_a) = hold_register(&machine_a, PAIRING_A, &machine_id).await;
    let (_hold_b, mut opens_b) = hold_register(&machine_b, PAIRING_B, &machine_id).await;

    let (mut a_dial, mut a_attach, _) =
        accept_on(&cloud_a, &machine_a, &mut opens_a, PAIRING_A, &machine_id).await;
    let (mut b_dial, mut b_attach, _) =
        accept_on(&cloud_b, &machine_b, &mut opens_b, PAIRING_B, &machine_id).await;

    a_dial
        .send(&TunnelFrame::new(b"a-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_attach).await.data, b"a-cloud");
    b_dial
        .send(&TunnelFrame::new(b"b-cloud".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_attach).await.data, b"b-cloud");

    a_attach
        .send(&TunnelFrame::new(b"a-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_dial).await.data, b"a-machine");
    b_attach
        .send(&TunnelFrame::new(b"b-machine".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_dial).await.data, b"b-machine");
}

#[tokio::test]
async fn dial_and_list_do_not_see_another_pairings_register() {
    let machine_id = colliding_machine_id();
    let relay = serve_relay().await;
    let machine_a = client(&relay.url);
    let cloud_b = client(&relay.url);
    let (_hold_a, mut opens_a) = hold_register(&machine_a, PAIRING_A, &machine_id).await;

    let error = cloud_b
        .dial(DIAL, PAIRING_B, machine_id.as_str())
        .await
        .unwrap_err();
    assert_status(&error, StatusCode::NOT_FOUND);

    let listed = cloud_b.list(DIAL, PAIRING_B).await.unwrap();
    assert!(listed.is_empty());

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
    let machine_a = client(&relay.url);
    let machine_b = client(&relay.url);
    let cloud_a = client(&relay.url);
    let cloud_b = client(&relay.url);
    let (_old_a, mut old_opens_a) = hold_register(&machine_a, PAIRING_A, &machine_id).await;
    let (_hold_b, mut opens_b) = hold_register(&machine_b, PAIRING_B, &machine_id).await;
    let (_new_a, mut opens_a) = hold_register(&machine_a, PAIRING_A, &machine_id).await;

    assert_opens_close(&mut old_opens_a).await;

    let (mut a_dial, mut a_attach, _) =
        accept_on(&cloud_a, &machine_a, &mut opens_a, PAIRING_A, &machine_id).await;
    let (mut b_dial, mut b_attach, _) =
        accept_on(&cloud_b, &machine_b, &mut opens_b, PAIRING_B, &machine_id).await;

    a_dial
        .send(&TunnelFrame::new(b"a-after-replace".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut a_attach).await.data, b"a-after-replace");
    b_dial
        .send(&TunnelFrame::new(b"b-still-here".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut b_attach).await.data, b"b-still-here");
}

#[tokio::test]
async fn list_empty_pairing_is_success_and_two_held_registers_return_both_ids() {
    let relay = serve_relay().await;
    let cloud = client(&relay.url);
    let empty = cloud.list(DIAL, PAIRING).await.unwrap();
    assert!(empty.is_empty());

    let first = MachineId::random();
    let second = MachineId::random();
    let machine_a = client(&relay.url);
    let machine_b = client(&relay.url);
    let _hold_a = hold_register(&machine_a, PAIRING, &first).await;
    let _hold_b = hold_register(&machine_b, PAIRING, &second).await;

    let listed = cloud.list(DIAL, PAIRING).await.unwrap();
    let ids: HashSet<_> = listed.iter().map(|row| row.machine_id().unwrap()).collect();
    assert_eq!(ids, HashSet::from([first, second]));
}

#[tokio::test]
async fn echo_fills_register_rtt_after_a_round_trip() {
    let mut session = Session::start().await;
    session.register().await;
    let ns = wait_for_rtt(&session.cloud, PAIRING, session.machine_id).await;
    assert!(ns.is_some(), "path RTT must be present after a pong");
}

#[tokio::test]
async fn list_without_pong_omits_register_rtt() {
    let relay = serve_relay().await;
    let machine = client(&relay.url);
    let cloud = client(&relay.url);
    let machine_id = MachineId::random();
    let _hold = start_register(&machine, PAIRING, &machine_id).await;

    let listed = cloud.list(DIAL, PAIRING).await.unwrap();
    let row = listed.first().expect("silent Register is listed");
    assert_eq!(row.machine_id().unwrap(), machine_id);
    assert_eq!(row.register_rtt_ns, None);
}

#[tokio::test]
async fn register_after_close_is_a_new_session() {
    let session = Session::start_with_drain(TEST_DRAIN).await;
    let machine_id = session.machine_id;
    let (mut session, mut dial, mut attach) = session.splice().await;
    let old_tunnel = session.tunnel_id.expect("splice opens a tunnel");
    session.goaway();
    session.wait_closed().await;
    assert_stream_closes(&mut attach).await;
    assert_stream_closes(&mut dial).await;

    let next = Session::start_with_machine(machine_id).await;
    let error = next.machine.attach(old_tunnel.as_str()).await.unwrap_err();
    assert_status(&error, StatusCode::NOT_FOUND);
    let (_session, mut dial, mut attach) = next.splice().await;
    dial.send(&TunnelFrame::new(b"new-session".to_vec()))
        .await
        .unwrap();
    assert_eq!(recv_frame(&mut attach).await.data, b"new-session");
}

fn assert_status(error: &ClientError, status: StatusCode) {
    assert_eq!(error.status(), Some(status));
}

struct Session {
    server: Option<tokio::task::JoinHandle<io::Result<()>>>,
    goaway: Option<ployz_relay::Goaway>,
    machine: RelayClient,
    cloud: RelayClient,
    machine_id: MachineId,
    _hold: Option<tokio::task::JoinHandle<()>>,
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
        let url = format!("http://{address}");
        Self {
            server: Some(server),
            goaway: Some(goaway),
            machine: client(&url),
            cloud: client(&url),
            machine_id,
            _hold: None,
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
        let (hold, opens) = hold_register(&self.machine, PAIRING, &self.machine_id).await;
        self._hold = Some(hold);
        self.opens = Some(opens);
    }

    async fn splice(mut self) -> (Self, RelayWs, RelayWs) {
        self.register().await;
        let (dial, attach) = self.accept_tunnel().await;
        (self, dial, attach)
    }

    async fn accept_tunnel(&mut self) -> (RelayWs, RelayWs) {
        let opens = self.opens.as_mut().expect("Register is held");
        let (dial, attach, tunnel_id) =
            accept_on(&self.cloud, &self.machine, opens, PAIRING, &self.machine_id).await;
        self.tunnel_id = Some(tunnel_id);
        (dial, attach)
    }
}

fn client(url: &str) -> RelayClient {
    RelayClient::new(url).expect("test Relay URL is http")
}

fn colliding_machine_id() -> MachineId {
    MachineId::parse("a".repeat(32)).unwrap()
}

struct SharedRelay {
    url: String,
    _server: tokio::task::JoinHandle<io::Result<()>>,
    _goaway: ployz_relay::Goaway,
}

async fn serve_relay() -> SharedRelay {
    let relay = Relay::new(DialCredential::parse(DIAL).unwrap());
    let listen = (Ipv4Addr::LOCALHOST, 0).into();
    let (address, server, goaway) = relay.serve(listen).await.unwrap();
    SharedRelay {
        url: format!("http://{address}"),
        _server: server,
        _goaway: goaway,
    }
}

async fn hold_register(
    client: &RelayClient,
    pairing: &str,
    machine_id: &MachineId,
) -> (tokio::task::JoinHandle<()>, mpsc::Receiver<Open>) {
    let mut ws = start_register(client, pairing, machine_id).await;
    let (tx, rx) = mpsc::channel(16);
    let hold = tokio::spawn(async move {
        while let Ok(Some(open)) = ws.recv::<Open>().await {
            if let Some(nonce) = open.ping_nonce() {
                let _ = ws.send(&RegisterRequest::pong(nonce)).await;
            } else if tx.send(open).await.is_err() {
                break;
            }
        }
    });
    (hold, rx)
}

async fn start_register(client: &RelayClient, pairing: &str, machine_id: &MachineId) -> RelayWs {
    client.register(pairing, machine_id).await.unwrap()
}

async fn wait_for_rtt(cloud: &RelayClient, pairing: &str, machine_id: MachineId) -> Option<i64> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let listed = cloud.list(DIAL, pairing).await.unwrap();
        if let Some(row) = listed
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
    cloud: &RelayClient,
    machine: &RelayClient,
    opens: &mut mpsc::Receiver<Open>,
    pairing: &str,
    machine_id: &MachineId,
) -> (RelayWs, RelayWs, TunnelId) {
    let dial = cloud
        .dial(DIAL, pairing, machine_id.as_str())
        .await
        .unwrap();
    let open = timeout(Duration::from_secs(2), opens.recv())
        .await
        .expect("Open timed out")
        .expect("Open closed");
    let tunnel_id = open.tunnel_id().expect("Open carries a Tunnel ID");
    let attach = machine.attach(tunnel_id.as_str()).await.unwrap();
    (dial, attach, tunnel_id)
}

async fn recv_frame(ws: &mut RelayWs) -> TunnelFrame {
    timeout(Duration::from_secs(2), ws.recv::<TunnelFrame>())
        .await
        .expect("frame timed out")
        .expect("frame")
        .expect("frame closed")
}

async fn assert_stream_closes(ws: &mut RelayWs) {
    match timeout(Duration::from_secs(2), ws.recv::<TunnelFrame>()).await {
        Ok(Ok(None) | Err(_)) => {}
        Ok(Ok(Some(_))) => panic!("stream stayed open"),
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
