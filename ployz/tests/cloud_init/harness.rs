//! Fake enroll HTTP, Relay, and Machine RPC for `ployz init --cloud` tests.

use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
    time::Duration,
};

use ployz::sdk;
use ployz_core::{
    AdvertisedEndpoint, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, Domain,
    InitializeRequest, Initialized, JoinAccepted, JoinRequest, LocalMachinePhase, Machine,
    MachineDetails, MachineId, MachineName, MachineRpc, MachineRpcServer, MachineToken,
    ManagementAddress, OpaquePayload, PROTOCOL_MAJOR, PairingCredential, Registered,
    ReserveDomainRequest, RpcRequestBody, RpcResponse, WireGuardPublicKey,
};
use ployz_relay::{
    AUTHORIZATION_METADATA, CloudRelayClient, DialCredential, RegisterRequest, Relay,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::{
    Request, Response, Status, Streaming,
    metadata::MetadataValue,
    transport::{Endpoint, Server},
};

pub const TOKEN: &str = "pmet_test";
pub const PAIRING: &str = "pairing-secret";
const DIAL: &str = "dial-secret";
pub const CLUSTER_DOMAIN: &str = "abcd12.ployz.dev";

pub struct RelayListen {
    pub url: String,
    _server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
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
}

pub struct EnrollListen {
    pub url: String,
    paths: Arc<Mutex<Vec<String>>>,
    posts: Arc<Mutex<Vec<serde_json::Value>>>,
    callbacks: Arc<Mutex<Vec<serde_json::Value>>>,
    callback_status: Arc<AtomicU16>,
    _server: tokio::task::JoinHandle<()>,
}

impl EnrollListen {
    pub async fn start(body: serde_json::Value) -> Self {
        Self::script([body]).await
    }

    pub async fn script(bodies: impl IntoIterator<Item = serde_json::Value>) -> Self {
        let mut remaining: VecDeque<Vec<u8>> = bodies
            .into_iter()
            .map(|body| serde_json::to_vec(&body).unwrap())
            .collect();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let posts = Arc::new(Mutex::new(Vec::new()));
        let callbacks = Arc::new(Mutex::new(Vec::new()));
        let callback_status = Arc::new(AtomicU16::new(200));
        let recorded_paths = Arc::clone(&paths);
        let recorded_posts = Arc::clone(&posts);
        let recorded_callbacks = Arc::clone(&callbacks);
        let callback_code = Arc::clone(&callback_status);
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0; 8192];
                let n = stream.read(&mut buf).await.unwrap();
                let raw = buf.get(..n).expect("read count is in bounds");
                let request = String::from_utf8_lossy(raw);
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                let is_callback = path.ends_with("/callback");
                recorded_paths.lock().unwrap().push(path);
                if is_callback {
                    recorded_callbacks
                        .lock()
                        .unwrap()
                        .push(enroll_json_body(raw));
                    let status = callback_code.load(Ordering::SeqCst);
                    let reason = if status == 200 { "OK" } else { "Error" };
                    let response = format!(
                        "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                    continue;
                }
                recorded_posts.lock().unwrap().push(enroll_json_body(raw));
                let body = remaining.pop_front().expect("scripted enroll has a body");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.write_all(&body).await.unwrap();
            }
        });
        Self {
            url: format!("http://{address}"),
            paths,
            posts,
            callbacks,
            callback_status,
            _server: server,
        }
    }

    pub fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }

    pub fn posts(&self) -> Vec<serde_json::Value> {
        self.posts.lock().unwrap().clone()
    }

    pub fn callbacks(&self) -> Vec<serde_json::Value> {
        self.callbacks.lock().unwrap().clone()
    }

    pub fn fail_callbacks(&self, status: u16) {
        self.callback_status.store(status, Ordering::SeqCst);
    }
}

fn enroll_json_body(raw: &[u8]) -> serde_json::Value {
    let sep = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP request has a header separator");
    serde_json::from_slice(raw.get(sep + 4..).expect("body follows headers")).unwrap()
}

#[derive(Clone)]
pub struct JoinDaemon {
    inner: Arc<JoinInner>,
}

struct JoinInner {
    registration: Registered,
    joined: AtomicBool,
    join_request: Mutex<Option<JoinRequest>>,
    initialize_request: Mutex<Option<InitializeRequest>>,
    reserve_request: Mutex<Option<ReserveDomainRequest>>,
    _register: Mutex<Option<mpsc::Sender<RegisterRequest>>>,
}

impl JoinDaemon {
    pub fn new(registration: Registered) -> Self {
        Self {
            inner: Arc::new(JoinInner {
                registration,
                joined: AtomicBool::new(false),
                join_request: Mutex::new(None),
                initialize_request: Mutex::new(None),
                reserve_request: Mutex::new(None),
                _register: Mutex::new(None),
            }),
        }
    }

    pub fn join_request(&self) -> JoinRequest {
        self.inner
            .join_request
            .lock()
            .unwrap()
            .clone()
            .expect("Join was called")
    }

    pub fn initialize_request(&self) -> InitializeRequest {
        self.inner
            .initialize_request
            .lock()
            .unwrap()
            .clone()
            .expect("Initialize was called")
    }

    pub fn reserve_request(&self) -> Option<ReserveDomainRequest> {
        self.inner.reserve_request.lock().unwrap().clone()
    }
}

#[expect(
    clippy::result_large_err,
    reason = "MachineRpc uses tonic::Status as the error type"
)]
fn rpc_ok(payload: impl Into<RpcResponse>) -> Result<Response<OpaquePayload>, Status> {
    Ok(Response::new(payload.into().encode().unwrap()))
}

#[expect(
    clippy::result_large_err,
    reason = "MachineRpc uses tonic::Status as the error type"
)]
fn unused<T>() -> Result<T, Status> {
    Err(Status::unimplemented("unused"))
}

#[tonic::async_trait]
impl MachineRpc for JoinDaemon {
    type ExecStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type ContainerLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type MachineLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type RuntimeWatchStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;

    async fn describe_contract(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(ContractDescription {
            machine_id: self.inner.registration.assigned_machine.id,
            protocol_major: PROTOCOL_MAJOR,
            daemon_version: "test".into(),
            capabilities: [
                ployz_core::CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
                    .expect("catalogued capability names are valid"),
            ]
            .into(),
        })
    }

    async fn inspect(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let joined = self.inner.joined.load(Ordering::SeqCst);
        rpc_ok(MachineDetails {
            id: self.inner.registration.assigned_machine.id,
            phase: if joined {
                LocalMachinePhase::Participating
            } else {
                LocalMachinePhase::Uninitialized
            },
            machine: joined.then(|| self.inner.registration.assigned_machine.clone()),
            public_key: self.inner.registration.assigned_machine.public_key,
            advertised_endpoints: self
                .inner
                .registration
                .assigned_machine
                .advertised_endpoints
                .clone(),
            store_version: Default::default(),
            rtts: Vec::new(),
        })
    }

    async fn machine_token(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(MachineToken {
            public_key: self.inner.registration.assigned_machine.public_key,
            public_ip: None,
            advertised_endpoints: self
                .inner
                .registration
                .assigned_machine
                .advertised_endpoints
                .clone(),
            runtime: Default::default(),
        })
    }

    async fn join(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Join(join) = decoded.body else {
            return Err(Status::invalid_argument("expected Join"));
        };
        let pairing = join
            .cloud_pairing
            .clone()
            .ok_or_else(|| Status::invalid_argument("Join must persist Cloud Pairing"))?;
        hold_register(
            pairing.relay_url(),
            pairing.secret(),
            &join.registration.assigned_machine.id,
            &self.inner._register,
        )
        .await;
        *self.inner.join_request.lock().unwrap() = Some(join);
        self.inner.joined.store(true, Ordering::SeqCst);
        rpc_ok(JoinAccepted {})
    }

    async fn initialize(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Initialize(init) = decoded.body else {
            return Err(Status::invalid_argument("expected Initialize"));
        };
        let pairing = init
            .cloud_pairing
            .clone()
            .ok_or_else(|| Status::invalid_argument("Initialize must persist Cloud Pairing"))?;
        let mut machine = self.inner.registration.assigned_machine.clone();
        machine.name = init.name.clone();
        hold_register(
            pairing.relay_url(),
            pairing.secret(),
            &machine.id,
            &self.inner._register,
        )
        .await;
        *self.inner.initialize_request.lock().unwrap() = Some(init);
        self.inner.joined.store(true, Ordering::SeqCst);
        rpc_ok(Initialized { machine })
    }
    async fn register(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn list_machines(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn list_containers(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn create_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn inspect_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn list_volumes(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn inspect_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn create_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn start_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn stop_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn list_images(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn ensure_image_ingest(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn pull_image_from_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn get_caddy_config(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn reserve_domain(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::ReserveDomain(reserve) = decoded.body else {
            return Err(Status::invalid_argument("expected ReserveDomain"));
        };
        *self.inner.reserve_request.lock().unwrap() = Some(reserve);
        rpc_ok(Domain {
            name: CLUSTER_DOMAIN.into(),
        })
    }
    async fn get_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn release_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn create_domain_records(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn reset(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn update_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_local_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn remove_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn inspect_wireguard(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        unused()
    }
    async fn exec(
        &self,
        _request: Request<Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        unused()
    }
    async fn container_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::ContainerLogsStream>, Status> {
        unused()
    }
    async fn machine_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::MachineLogsStream>, Status> {
        unused()
    }
    async fn runtime_watch(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::RuntimeWatchStream>, Status> {
        unused()
    }
}

pub async fn serve_machine(daemon: JoinDaemon) -> SocketAddr {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(daemon))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(tcp)),
    );
    address
}

async fn hold_register(
    url: &str,
    pairing: &PairingCredential,
    machine_id: &MachineId,
    slot: &Mutex<Option<mpsc::Sender<RegisterRequest>>>,
) {
    let channel = Endpoint::from_shared(url.to_owned())
        .unwrap()
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .unwrap();
    let mut relay = CloudRelayClient::new(channel);
    let (tx, rx) = mpsc::channel(4);
    tx.send(RegisterRequest::new(machine_id)).await.unwrap();
    let mut request = Request::new(ReceiverStream::new(rx));
    request.metadata_mut().insert(
        AUTHORIZATION_METADATA,
        MetadataValue::try_from(format!("Bearer {}", pairing.as_str())).unwrap(),
    );
    let mut opens = relay.register(request).await.unwrap().into_inner();
    let pong = tx.clone();
    tokio::spawn(async move {
        while let Some(Ok(message)) = opens.next().await {
            if let Some(nonce) = message.ping_nonce() {
                let _ = pong.send(RegisterRequest::pong(nonce)).await;
            }
        }
    });
    *slot.lock().unwrap() = Some(tx);
}

pub async fn wait_for_held(url: &str, machine_id: MachineId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let listed = sdk::list_held(url, DIAL, PAIRING).await.unwrap();
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

pub fn registration() -> Registered {
    Registered {
        assigned_machine: joiner_machine(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    }
}

fn joiner_machine() -> Machine {
    Machine {
        id: MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        name: MachineName::parse("joiner").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        management_address: ManagementAddress("fd00::1".parse().unwrap()),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
        runtime: Default::default(),
    }
}

pub fn founder_machine() -> Machine {
    Machine {
        id: MachineId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
        name: MachineName::parse("founder").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        management_address: ManagementAddress("fd00::2".parse().unwrap()),
        public_key: WireGuardPublicKey([2; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        runtime: Default::default(),
    }
}
