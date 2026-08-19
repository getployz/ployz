//! `ployz init --cloud` join and initialize paths against fake enroll HTTP.

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ployz::sdk;
use ployz_core::{
    AdvertisedEndpoint, CloudPairing, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, Domain,
    InitializeRequest, Initialized, JoinAccepted, JoinRequest, LocalMachinePhase, Machine,
    MachineDetails, MachineId, MachineName, MachineRpc, MachineRpcServer, MachineToken,
    ManagementAddress, OpaquePayload, PROTOCOL_MAJOR, PairingCredential, Registered,
    ReserveDomainRequest, RpcRequestBody, RpcResponse, WireGuardPublicKey,
};
use ployz_relay::{
    AUTHORIZATION_METADATA, CloudRelayClient, DialCredential, RegisterRequest, Relay,
};
use serde_json::json;
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

const TOKEN: &str = "pmet_test";
const PAIRING: &str = "pairing-secret";
const DIAL: &str = "dial-secret";
const CLUSTER_DOMAIN: &str = "abcd12.ployz.dev";

#[tokio::test]
async fn cloud_init_join_participates_and_appears_on_list_held() {
    let registration = registration();
    let machine_id = registration.assigned_machine.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "join",
        "pairing": pairing,
        "registration": registration,
    }))
    .await;
    let daemon = JoinDaemon::new(registration.clone());
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{machine_addr}"),
            "init",
            "--cloud",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "joiner",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Joined Machine joiner ({machine_id})")),
        "{stdout}"
    );

    let joined = daemon.join_request();
    let pairing_json = serde_json::to_value(joined.cloud_pairing.as_ref().unwrap()).unwrap();
    assert_eq!(
        pairing_json,
        json!({
            "relayUrl": relay.url,
            "secret": PAIRING,
        })
    );
    assert_eq!(joined.registration.assigned_machine.id, machine_id);

    let paths = enroll.paths();
    assert_eq!(paths, [format!("/api/enroll/{TOKEN}")]);

    wait_for_held(&relay.url, machine_id).await;
}

#[tokio::test]
async fn cloud_init_initialize_participates_and_appears_on_list_held() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "pairing": pairing,
    }))
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{machine_addr}"),
            "init",
            "--cloud",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "founder",
            "--network",
            "10.210.0.0/16",
            "--wg-mtu",
            "1400",
            "--no-caddy",
            "--no-dns",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Initialised Machine founder ({machine_id})")),
        "{stdout}"
    );

    let initialized = daemon.initialize_request();
    assert_eq!(initialized.name.as_str(), "founder");
    assert_eq!(initialized.cluster_network.to_string(), "10.210.0.0/16");
    assert_eq!(initialized.wireguard_mtu, Some(1400));
    let pairing_json = serde_json::to_value(initialized.cloud_pairing.as_ref().unwrap()).unwrap();
    assert_eq!(
        pairing_json,
        json!({
            "relayUrl": relay.url,
            "secret": PAIRING,
        })
    );
    assert!(pairing_json.get("dial").is_none());

    let paths = enroll.paths();
    assert_eq!(paths, [format!("/api/enroll/{TOKEN}")]);

    assert!(
        daemon.reserve_request().is_none(),
        "initialize with --no-dns must not ReserveDomain"
    );

    wait_for_held(&relay.url, machine_id).await;
}

#[tokio::test]
async fn cloud_init_initialize_reserves_cloud_hosted_dns() {
    let founder = founder_machine();
    let machine_id = founder.id;
    let relay = RelayListen::start().await;
    let pairing =
        CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
    let enroll = EnrollListen::start(json!({
        "kind": "initialize",
        "pairing": pairing,
    }))
    .await;
    let daemon = JoinDaemon::new(Registered {
        assigned_machine: founder,
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    });
    let machine_addr = serve_machine(daemon.clone()).await;

    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args([
            "--connect",
            &format!("tcp://{machine_addr}"),
            "init",
            "--cloud",
            TOKEN,
            "--cloud-url",
            &enroll.url,
            "--name",
            "founder",
            "--no-caddy",
            "--yes",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("Reserved Cluster domain: {CLUSTER_DOMAIN}")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("Initialised Machine founder ({machine_id})")),
        "{stdout}"
    );

    let reserved = daemon.reserve_request().expect("ReserveDomain was called");
    assert_eq!(reserved.endpoint, format!("{}/api/dns/v1", enroll.url));
    assert_ne!(reserved.endpoint, "https://dns.uncloud.run/v1");
}

struct RelayListen {
    url: String,
    _server: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

impl RelayListen {
    async fn start() -> Self {
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

struct EnrollListen {
    url: String,
    paths: Arc<Mutex<Vec<String>>>,
    _server: tokio::task::JoinHandle<()>,
}

impl EnrollListen {
    async fn start(body: serde_json::Value) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&paths);
        let body = serde_json::to_vec(&body).unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0; 8192];
                let n = stream.read(&mut buf).await.unwrap();
                let request =
                    String::from_utf8_lossy(buf.get(..n).expect("read count is in bounds"));
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                recorded.lock().unwrap().push(path);
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
            _server: server,
        }
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().unwrap().clone()
    }
}

#[derive(Clone)]
struct JoinDaemon {
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
    fn new(registration: Registered) -> Self {
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

    fn join_request(&self) -> JoinRequest {
        self.inner
            .join_request
            .lock()
            .unwrap()
            .clone()
            .expect("Join was called")
    }

    fn initialize_request(&self) -> InitializeRequest {
        self.inner
            .initialize_request
            .lock()
            .unwrap()
            .clone()
            .expect("Initialize was called")
    }

    fn reserve_request(&self) -> Option<ReserveDomainRequest> {
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

async fn serve_machine(daemon: JoinDaemon) -> SocketAddr {
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

async fn wait_for_held(url: &str, machine_id: MachineId) {
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

fn registration() -> Registered {
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

fn founder_machine() -> Machine {
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
