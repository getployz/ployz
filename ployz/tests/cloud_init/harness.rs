//! Fake enroll HTTP, Relay, and Machine RPC for membership join tests.

use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering},
    },
    time::Duration,
};

use ployz::sdk;
use ployz_core::{
    AdvertisedEndpoint, CloudPairingSet, ContainerCreated, ContainerId, ContainerKind,
    ContainerList, ContainerObservation, ContainerRuntimeObservation, ContractDescription,
    DESCRIBE_CONTRACT_CAPABILITY, Domain, EnsureGlobalSlotRequest, HealthObservation,
    InitializeRequest, Initialized, JoinAccepted, JoinRequest, LocalMachinePhase, Machine,
    MachineDetails, MachineId, MachineList, MachineName, MachineObservation, MachineRpc,
    MachineRpcServer, MachineToken, ManagementAddress, MembershipObservation, OpaquePayload,
    PROTOCOL_MAJOR, PairingCredential, Registered, ReserveDomainRequest, ResetAccepted, RpcError,
    RpcErrorCode, RpcRequestBody, RpcResponse, WireGuardPublicKey,
};
use ployz_relay::{ClientError, DialCredential, Open, RegisterRequest, Relay, RelayClient};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use tonic::{Request, Response, Status, Streaming, transport::Server};

#[path = "../support/inspect_telemetry.rs"]
mod inspect_telemetry_fixture;

pub const TOKEN: &str = "pmet_test";
pub const PAIRING: &str = "pairing-secret";
const DIAL: &str = "dial-secret";
pub const CLUSTER_DOMAIN: &str = "abcd12.ployz.dev";

pub struct RelayListen {
    pub url: String,
    _server: tokio::task::JoinHandle<std::io::Result<()>>,
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

    pub async fn revoke(&self, pairing: &str) {
        sdk::revoke_pairing(&self.url, DIAL, pairing)
            .await
            .expect("test Relay accepts Dial revoke");
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
    initialize_requests: Mutex<Vec<InitializeRequest>>,
    reserve_request: Mutex<Option<ReserveDomainRequest>>,
    resets: AtomicUsize,
    containers: Mutex<Vec<ContainerObservation>>,
    ensure_requests: Mutex<Vec<EnsureGlobalSlotRequest>>,
    ensure_attempts: AtomicUsize,
    transient_ensure_failures: AtomicUsize,
    target_inspect_attempts: AtomicUsize,
    transient_target_inspect_failures: AtomicUsize,
    fail_ensure: AtomicBool,
    fail_list_on: Mutex<Option<MachineId>>,
    assigned_membership: Mutex<MembershipObservation>,
    _register: Mutex<Option<JoinHandle<()>>>,
}

impl JoinDaemon {
    pub fn new(registration: Registered) -> Self {
        Self {
            inner: Arc::new(JoinInner {
                registration,
                joined: AtomicBool::new(false),
                join_request: Mutex::new(None),
                initialize_requests: Mutex::new(Vec::new()),
                reserve_request: Mutex::new(None),
                resets: AtomicUsize::new(0),
                containers: Mutex::new(Vec::new()),
                ensure_requests: Mutex::new(Vec::new()),
                ensure_attempts: AtomicUsize::new(0),
                transient_ensure_failures: AtomicUsize::new(0),
                target_inspect_attempts: AtomicUsize::new(0),
                transient_target_inspect_failures: AtomicUsize::new(0),
                fail_ensure: AtomicBool::new(false),
                fail_list_on: Mutex::new(None),
                assigned_membership: Mutex::new(MembershipObservation::Up),
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
        self.initialize_requests()
            .last()
            .cloned()
            .expect("Initialize was called")
    }

    pub fn initialize_requests(&self) -> Vec<InitializeRequest> {
        self.inner.initialize_requests.lock().unwrap().clone()
    }

    pub fn reset_count(&self) -> usize {
        self.inner.resets.load(Ordering::SeqCst)
    }

    pub fn reserve_request(&self) -> Option<ReserveDomainRequest> {
        self.inner.reserve_request.lock().unwrap().clone()
    }

    pub fn with_containers(self, containers: Vec<ContainerObservation>) -> Self {
        *self.inner.containers.lock().unwrap() = containers;
        self
    }

    pub fn fail_ensure(self) -> Self {
        self.inner.fail_ensure.store(true, Ordering::SeqCst);
        self
    }

    pub fn transient_ensure_failures(self, failures: usize) -> Self {
        self.inner
            .transient_ensure_failures
            .store(failures, Ordering::SeqCst);
        self
    }

    /// Make this many targeted Inspect attempts transiently unavailable.
    pub fn transient_target_inspect_failures(self, failures: usize) -> Self {
        self.inner
            .transient_target_inspect_failures
            .store(failures, Ordering::SeqCst);
        self
    }

    pub fn fail_list_on(self, machine_id: MachineId) -> Self {
        *self.inner.fail_list_on.lock().unwrap() = Some(machine_id);
        self
    }

    /// Set the entry Machine's membership observation for the assigned Machine.
    pub fn with_membership(self, membership: MembershipObservation) -> Self {
        *self.inner.assigned_membership.lock().unwrap() = membership;
        self
    }

    pub fn ensure_requests(&self) -> Vec<EnsureGlobalSlotRequest> {
        self.inner.ensure_requests.lock().unwrap().clone()
    }

    pub fn ensure_attempts(&self) -> usize {
        self.inner.ensure_attempts.load(Ordering::SeqCst)
    }

    /// Count targeted Inspect attempts received by this fake daemon.
    pub fn target_inspect_attempts(&self) -> usize {
        self.inner.target_inspect_attempts.load(Ordering::SeqCst)
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
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if request
            .metadata()
            .contains_key(ployz_core::ONE_TARGET_HEADER)
        {
            self.inner
                .target_inspect_attempts
                .fetch_add(1, Ordering::SeqCst);
            if self
                .inner
                .transient_target_inspect_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(Status::unavailable("target Machine is not ready"));
            }
        }
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Inspect(inspect) = request.body else {
            return Err(Status::invalid_argument("expected Inspect"));
        };
        let joined = self.inner.joined.load(Ordering::SeqCst);
        let telemetry = inspect_telemetry_fixture::observation(inspect.telemetry);
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
            cloud_paired: false,
            telemetry,
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
        if let Some(pairing) = join.cloud_pairing.clone() {
            hold_register(
                pairing.relay_url(),
                pairing.secret(),
                &join.registration.assigned_machine.id,
                &self.inner._register,
            )
            .await?;
        }
        *self.inner.join_request.lock().unwrap() = Some(join);
        self.inner.joined.store(true, Ordering::SeqCst);
        rpc_ok(JoinAccepted {})
    }

    async fn set_cloud_pairing(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::SetCloudPairing(set) = decoded.body else {
            return Err(Status::invalid_argument("expected SetCloudPairing"));
        };
        match set.cloud_pairing {
            Some(pairing) => {
                hold_register(
                    pairing.relay_url(),
                    pairing.secret(),
                    &self.inner.registration.assigned_machine.id,
                    &self.inner._register,
                )
                .await?;
            }
            None => {
                if let Some(old) = self.inner._register.lock().unwrap().take() {
                    old.abort();
                }
            }
        }
        rpc_ok(CloudPairingSet {})
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
        let pairing = init.cloud_pairing.clone();
        let mut machine = self.inner.registration.assigned_machine.clone();
        machine.name = init.name.clone();
        self.inner.initialize_requests.lock().unwrap().push(init);
        self.inner.joined.store(true, Ordering::SeqCst);
        if let Some(pairing) = pairing
            && let Err(status) = hold_register(
                pairing.relay_url(),
                pairing.secret(),
                &machine.id,
                &self.inner._register,
            )
            .await
        {
            if status.code() == tonic::Code::Unauthenticated {
                return rpc_ok(RpcError {
                    code: RpcErrorCode::Unauthenticated,
                    message: status.message().to_owned(),
                    details: serde_json::Value::Null,
                });
            }
            return Err(status);
        }
        rpc_ok(Initialized { machine })
    }
    async fn register(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        rpc_ok(self.inner.registration.clone())
    }
    async fn list_machines(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let assigned = MachineObservation {
            machine: self.inner.registration.assigned_machine.clone(),
            membership: self.inner.assigned_membership.lock().unwrap().clone(),
            selected_endpoint: None,
            rtt: None,
        };
        let mut machines = vec![assigned];
        machines.extend(
            self.inner
                .registration
                .visible_peers
                .iter()
                .cloned()
                .map(up_machine),
        );
        rpc_ok(MachineList { machines })
    }
    async fn list_containers(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let target = request
            .metadata()
            .get(ployz_core::ONE_TARGET_HEADER)
            .and_then(|value| value.to_str().ok());
        if self
            .inner
            .fail_list_on
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|machine_id| target == Some(machine_id.as_str()))
        {
            return rpc_ok(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "unreachable".into(),
                details: serde_json::Value::Null,
            });
        }
        rpc_ok(ContainerList {
            containers: self.inner.containers.lock().unwrap().clone(),
        })
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
    async fn ensure_global_slot(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::EnsureGlobalSlot(ensure) = decoded.body else {
            return Err(Status::invalid_argument("expected EnsureGlobalSlot"));
        };
        self.inner.ensure_attempts.fetch_add(1, Ordering::SeqCst);
        if self
            .inner
            .transient_ensure_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(Status::unavailable("transient ensure failure"));
        }
        if self.inner.fail_ensure.load(Ordering::SeqCst) {
            return rpc_ok(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "ensure failed".into(),
                details: serde_json::Value::Null,
            });
        }
        self.inner
            .ensure_requests
            .lock()
            .unwrap()
            .push(ensure.clone());
        let n = self.inner.ensure_requests.lock().unwrap().len();
        let container_id = ContainerId::parse(format!("{n:064x}")).unwrap();
        let machine_id = self.inner.registration.assigned_machine.id;
        self.inner
            .containers
            .lock()
            .unwrap()
            .push(ContainerObservation {
                container_id,
                display_name: format!("{}-slot", ensure.resolved_spec.name),
                created_at_unix_nanos: n as i64,
                machine_id,
                project_name: ensure.project_name,
                service_id: ensure.resolved_spec.service_id,
                service_name: ensure.resolved_spec.name.clone(),
                kind: ContainerKind::ServiceContainer,
                runtime: ContainerRuntimeObservation::Running {
                    health: HealthObservation::NotConfigured,
                },
                effective_healthcheck: None,
                resolved_spec: ensure.resolved_spec,
                address: None,
                labels: Default::default(),
            });
        rpc_ok(ContainerCreated {
            container_id,
            display_name: format!("slot-{n}"),
        })
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
        rpc_ok(RpcError {
            code: RpcErrorCode::NotFound,
            message: "no reserved domain".into(),
            details: serde_json::Value::Null,
        })
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
        self.inner.resets.fetch_add(1, Ordering::SeqCst);
        self.inner.joined.store(false, Ordering::SeqCst);
        if let Some(hold) = self.inner._register.lock().unwrap().take() {
            hold.abort();
        }
        rpc_ok(ResetAccepted {})
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
    slot: &Mutex<Option<JoinHandle<()>>>,
) -> Result<(), Status> {
    let mut ws = RelayClient::new(url)
        .map_err(status_from_client)?
        .register(pairing.as_str(), machine_id)
        .await
        .map_err(status_from_client)?;
    let hold = tokio::spawn(async move {
        while let Ok(Some(message)) = ws.recv::<Open>().await {
            if let Some(nonce) = message.ping_nonce() {
                let _ = ws.send(&RegisterRequest::pong(nonce)).await;
            }
        }
    });
    if let Some(old) = slot.lock().unwrap().replace(hold) {
        old.abort();
    }
    Ok(())
}

fn status_from_client(error: ClientError) -> Status {
    match error.status() {
        Some(http::StatusCode::UNAUTHORIZED) => Status::unauthenticated(error.to_string()),
        Some(http::StatusCode::BAD_REQUEST) => Status::invalid_argument(error.to_string()),
        Some(http::StatusCode::NOT_FOUND) => Status::not_found(error.to_string()),
        Some(http::StatusCode::SERVICE_UNAVAILABLE) => Status::unavailable(error.to_string()),
        _ => Status::unavailable(error.to_string()),
    }
}

pub async fn wait_for_held(url: &str, pairing: &str, machine_id: MachineId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let listed = sdk::list_held(url, DIAL, pairing).await.unwrap();
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

pub async fn assert_not_held(url: &str, pairing: &str, machine_id: MachineId) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    let listed = sdk::list_held(url, DIAL, pairing).await.unwrap();
    assert!(
        listed
            .iter()
            .all(|row| row.machine_id().ok() != Some(machine_id)),
        "Machine must stay off List for revoked pairing, got {listed:?}"
    );
}

pub fn registration() -> Registered {
    Registered {
        assigned_machine: joiner_machine(),
        visible_peers: Vec::new(),
        target_versions: Default::default(),
    }
}

pub fn caddy_on(machine: &Machine) -> ContainerObservation {
    let spec = ployz::caddy::service_spec("caddy:2.10.0".into(), Vec::new(), None).to_resolved(
        ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
        ployz_core::ResolvedUpdateConfig::default(),
    );
    ContainerObservation {
        container_id: ContainerId::parse("a".repeat(64)).unwrap(),
        display_name: "caddy-a".into(),
        created_at_unix_nanos: 1,
        machine_id: machine.id,
        project_name: ployz_core::ProjectName::system(),
        service_id: spec.service_id,
        service_name: spec.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    }
}

fn up_machine(machine: Machine) -> MachineObservation {
    MachineObservation {
        machine,
        membership: MembershipObservation::Up,
        selected_endpoint: None,
        rtt: None,
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
