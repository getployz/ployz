use std::time::Duration;

use ployz_core::{InspectRequest, MachineId, OpaquePayload, TunnelId, op};
use ployz_relay::{
    AUTHORIZATION_METADATA, CloudRelayClient, DialCredential, MACHINE_ID_METADATA, Open,
    PairingCredential, RegisterRequest, Relay, TUNNEL_ID_METADATA, TunnelFrame,
};
use tokio::{sync::mpsc, time::timeout};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{Request, metadata::MetadataValue, transport::Endpoint};

const PAIRING: &str = "pairing-secret";
const DIAL: &str = "dial-secret";
const TEST_DRAIN: Duration = Duration::from_millis(300);

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

    assert_stream_closes(&mut old_opens).await;

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
    let mut request = session.dial_request(DIAL, &session.machine_id, mpsc::channel(4).1);
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
        .dial(session.dial_request(PAIRING, &session.machine_id, mpsc::channel(4).1))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::PermissionDenied);
}

#[tokio::test]
async fn dial_of_unknown_machine_id_fails_closed() {
    let mut session = Session::start().await;
    let error = session
        .cloud
        .dial(session.dial_request(DIAL, &session.machine_id, mpsc::channel(4).1))
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
    opens: Option<tonic::Streaming<Open>>,
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
        let relay = Relay::new(
            PairingCredential::parse(PAIRING).unwrap(),
            DialCredential::parse(DIAL).unwrap(),
        )
        .unwrap();
        let (address, server, goaway) = match drain {
            None => relay.serve().await.unwrap(),
            Some(drain) => relay.serve_with_drain(drain).await.unwrap(),
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
        let opens = self.machine.register(request).await.unwrap().into_inner();
        self.register_hold = Some(tx);
        self.opens = Some(opens);
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
        let (dial_tx, dial_rx) = mpsc::channel(4);
        let dial_in = self
            .cloud
            .dial(self.dial_request(DIAL, &self.machine_id, dial_rx))
            .await
            .unwrap()
            .into_inner();
        let open = timeout(Duration::from_secs(2), self.opens.as_mut().unwrap().next())
            .await
            .expect("Open timed out")
            .unwrap()
            .unwrap();
        let (attach_tx, attach_rx) = mpsc::channel(4);
        let mut attach_request = Request::new(ReceiverStream::new(attach_rx));
        attach_request.metadata_mut().insert(
            TUNNEL_ID_METADATA,
            open.tunnel_id()
                .expect("Open carries a Tunnel ID")
                .as_str()
                .parse()
                .expect("Tunnel ID is ASCII metadata"),
        );
        let attach_in = self
            .machine
            .attach(attach_request)
            .await
            .unwrap()
            .into_inner();
        self.tunnel_id = Some(open.tunnel_id().expect("Open carries a Tunnel ID"));
        (dial_tx, dial_in, attach_tx, attach_in)
    }

    fn dial_request(
        &self,
        credential: &str,
        machine_id: &MachineId,
        rx: mpsc::Receiver<TunnelFrame>,
    ) -> Request<ReceiverStream<TunnelFrame>> {
        let mut request = Request::new(ReceiverStream::new(rx));
        set_bearer(request.metadata_mut(), credential);
        request.metadata_mut().insert(
            MACHINE_ID_METADATA,
            machine_id
                .as_str()
                .parse()
                .expect("Machine ID is ASCII metadata"),
        );
        request
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

fn set_bearer(metadata: &mut tonic::metadata::MetadataMap, credential: &str) {
    metadata.insert(
        AUTHORIZATION_METADATA,
        MetadataValue::try_from(format!("Bearer {credential}")).expect("bearer is ASCII"),
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
