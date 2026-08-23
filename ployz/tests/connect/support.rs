use std::{
    collections::{BTreeMap, VecDeque},
    net::{Ipv6Addr, SocketAddr},
    path::PathBuf,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use ployz::{
    connect::{
        BoxProxyStream, Client, ConnectError, Connector, SystemConnector, connect_selected_with,
    },
    context::{Connection, ConnectionSource, SelectedConnections},
};
use ployz_core::{
    AdvertisedEndpoint, ContainerCreated, ContainerId, ContainerList, ContractDescription,
    CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName, LocalMachinePhase,
    LocalMachineRemoved, Machine, MachineDetails, MachineId, MachineList, MachineName,
    MachineObservation, MachineRemoved, MachineRpc, MachineRpcServer, MachineStorageObservation,
    ManagementAddress, MembershipObservation, OpaquePayload, PROTOCOL_MAJOR,
    RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, Registered, RemoveMachineRequest, RpcError, RpcErrorCode,
    RpcRequestBody, RpcResponse, RuntimeWatchFrame, RuntimeWatchRequest,
    RuntimeWatchTransportFrame, VolumeList, VolumeRemoved, WireGuardPublicKey, op,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{
    Request, Response, Status, Streaming,
    transport::{Channel, Server},
};

#[path = "../support/inspect_telemetry.rs"]
mod inspect_telemetry_fixture;

pub(super) async fn serve_discovery(
    service: DiscoveryService,
) -> (
    SocketAddr,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
) {
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = tcp.local_addr().unwrap();
    let server = tokio::spawn(
        Server::builder()
            .add_service(
                MachineRpcServer::new(service)
                    .max_encoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT),
            )
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
    (address, server)
}

pub(super) enum DescribeOutcome {
    Status(Status),
    Remote(RpcError),
    Hang,
}

#[derive(Clone)]
enum WatchEvent {
    Frame(RuntimeWatchFrame),
    Payload(OpaquePayload),
    Fail(Status),
}

struct WatchHub {
    every_open: Mutex<Option<RuntimeWatchFrame>>,
    pending: Mutex<VecDeque<WatchEvent>>,
    live: Mutex<Vec<mpsc::Sender<Result<OpaquePayload, Status>>>>,
}

type ContainerListOutcomes = BTreeMap<MachineId, VecDeque<Result<ContainerList, Status>>>;

impl WatchHub {
    fn new() -> Self {
        Self {
            every_open: Mutex::new(None),
            pending: Mutex::new(VecDeque::new()),
            live: Mutex::new(Vec::new()),
        }
    }

    fn set_every_open(&self, frame: RuntimeWatchFrame) {
        *self.every_open.lock().unwrap() = Some(frame);
    }

    fn push(&self, event: WatchEvent) {
        let mut live = self.live.lock().unwrap();
        live.retain(|sender| !sender.is_closed());
        if live.is_empty() {
            drop(live);
            self.pending.lock().unwrap().push_back(event);
            return;
        }
        for sender in live.iter() {
            send_watch_event(sender, &event);
        }
    }

    fn subscribe(&self) -> mpsc::Receiver<Result<OpaquePayload, Status>> {
        let (sender, receiver) = mpsc::channel(16);
        if let Some(frame) = self.every_open.lock().unwrap().clone() {
            send_watch_event(&sender, &WatchEvent::Frame(frame));
        }
        let mut pending = self.pending.lock().unwrap();
        while let Some(event) = pending.pop_front() {
            send_watch_event(&sender, &event);
        }
        drop(pending);
        self.live.lock().unwrap().push(sender);
        receiver
    }

    fn live_count(&self) -> usize {
        let mut live = self.live.lock().unwrap();
        live.retain(|sender| !sender.is_closed());
        live.len()
    }
}

fn send_watch_event(sender: &mpsc::Sender<Result<OpaquePayload, Status>>, event: &WatchEvent) {
    let item = match event {
        WatchEvent::Frame(frame) => {
            OpaquePayload::from_json(&RuntimeWatchTransportFrame::from_frame(frame))
                .map_err(|error| Status::internal(error.to_string()))
        }
        WatchEvent::Payload(payload) => Ok(payload.clone()),
        WatchEvent::Fail(status) => Err(status.clone()),
    };
    let _ = sender.try_send(item);
}

#[derive(Clone)]
pub(super) struct DiscoveryService {
    description: ContractDescription,
    pub(super) describe_outcomes: Arc<Mutex<VecDeque<DescribeOutcome>>>,
    pub(super) stream_opens: Arc<AtomicUsize>,
    pub(super) watch_opens: Arc<AtomicUsize>,
    pub(super) list_rpc_calls: Arc<AtomicUsize>,
    pub(super) inspect_calls: Arc<AtomicUsize>,
    pub(super) container_list_calls: Arc<Mutex<BTreeMap<MachineId, usize>>>,
    pub(super) container_list_outcomes: Arc<Mutex<ContainerListOutcomes>>,
    pub(super) watch_requests: Arc<Mutex<Vec<RuntimeWatchRequest>>>,
    watch: Arc<WatchHub>,
    pub(super) machines: Vec<MachineObservation>,
    pub(super) listed_volumes: Arc<Mutex<BTreeMap<MachineId, Vec<DockerVolume>>>>,
    pub(super) listed_containers: Arc<Mutex<Vec<ployz_core::ContainerObservation>>>,
    pub(super) accept_volume_creates: bool,
    pub(super) existing_created_volume: Option<DockerVolume>,
    pub(super) created_volumes: Arc<Mutex<Vec<(MachineId, CreateVolumeRequest)>>>,
    pub(super) removed_volumes: Arc<Mutex<Vec<DockerVolumeId>>>,
    pub(super) reset_warning: Arc<Mutex<Option<String>>>,
    pub(super) reset_machines: Arc<Mutex<Vec<MachineId>>>,
    pub(super) removed_machines: Arc<Mutex<Vec<MachineId>>>,
    pub(super) cloud_paired: Arc<AtomicBool>,
    register_error: Arc<Mutex<Option<RpcError>>>,
}

impl DiscoveryService {
    pub(super) fn new(description: ContractDescription) -> Self {
        Self {
            description,
            describe_outcomes: Arc::new(Mutex::new(VecDeque::new())),
            stream_opens: Arc::new(AtomicUsize::new(0)),
            watch_opens: Arc::new(AtomicUsize::new(0)),
            list_rpc_calls: Arc::new(AtomicUsize::new(0)),
            inspect_calls: Arc::new(AtomicUsize::new(0)),
            container_list_calls: Arc::new(Mutex::new(BTreeMap::new())),
            container_list_outcomes: Arc::new(Mutex::new(BTreeMap::new())),
            watch_requests: Arc::new(Mutex::new(Vec::new())),
            watch: Arc::new(WatchHub::new()),
            machines: vec![machine('a', "one")],
            listed_volumes: Arc::new(Mutex::new(BTreeMap::new())),
            listed_containers: Arc::new(Mutex::new(Vec::new())),
            accept_volume_creates: false,
            existing_created_volume: None,
            created_volumes: Arc::new(Mutex::new(Vec::new())),
            removed_volumes: Arc::new(Mutex::new(Vec::new())),
            reset_warning: Arc::new(Mutex::new(None)),
            reset_machines: Arc::new(Mutex::new(Vec::new())),
            removed_machines: Arc::new(Mutex::new(Vec::new())),
            cloud_paired: Arc::new(AtomicBool::new(false)),
            register_error: Arc::new(Mutex::new(None)),
        }
    }

    pub(super) fn set_register_error(&self, error: RpcError) {
        *self.register_error.lock().unwrap() = Some(error);
    }

    pub(super) fn emit_watch_frame_on_open(&self, frame: RuntimeWatchFrame) {
        self.watch.set_every_open(frame);
    }

    pub(super) fn push_watch_frame(&self, frame: RuntimeWatchFrame) {
        self.watch.push(WatchEvent::Frame(frame));
    }

    pub(super) fn push_watch_payload(&self, payload: OpaquePayload) {
        self.watch.push(WatchEvent::Payload(payload));
    }

    pub(super) fn fail_watch(&self, status: Status) {
        self.watch.push(WatchEvent::Fail(status));
    }

    pub(super) fn live_watch_senders(&self) -> usize {
        self.watch.live_count()
    }
}

pub(super) struct CountingConnector {
    inner: SystemConnector,
    connects: Arc<AtomicUsize>,
    redirects: Mutex<VecDeque<SocketAddr>>,
}

impl CountingConnector {
    pub(super) fn new(connects: Arc<AtomicUsize>) -> Self {
        Self::redirecting(connects, std::iter::empty())
    }

    pub(super) fn redirecting(
        connects: Arc<AtomicUsize>,
        redirects: impl IntoIterator<Item = SocketAddr>,
    ) -> Self {
        Self {
            inner: SystemConnector::default(),
            connects,
            redirects: Mutex::new(redirects.into_iter().collect()),
        }
    }
}

#[tonic::async_trait]
impl Connector for CountingConnector {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let redirected = self.redirects.lock().unwrap().pop_front();
        match redirected {
            Some(address) => self.inner.connect(&Connection::tcp(address)).await,
            None => self.inner.connect(connection).await,
        }
    }

    async fn dial_proxy(
        &self,
        _connection: &Connection,
        _network: &str,
        _address: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        Err(ConnectError::Attempt("unused".into()))
    }
}

#[tonic::async_trait]
impl MachineRpc for DiscoveryService {
    type ExecStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type ContainerLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type MachineLogsStream = tokio_stream::Empty<Result<OpaquePayload, Status>>;
    type RuntimeWatchStream = ReceiverStream<Result<OpaquePayload, Status>>;

    async fn describe_contract(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let outcome = self.describe_outcomes.lock().unwrap().pop_front();
        match outcome {
            Some(DescribeOutcome::Status(status)) => return Err(status),
            Some(DescribeOutcome::Remote(error)) => {
                return Ok(Response::new(RpcResponse::from(error).encode().unwrap()));
            }
            Some(DescribeOutcome::Hang) => return std::future::pending().await,
            None => {}
        }
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if !matches!(request.body, RpcRequestBody::DescribeContract(_)) {
            return Err(Status::invalid_argument("expected discovery request"));
        }
        Ok(Response::new(
            RpcResponse::from(self.description.clone())
                .encode()
                .unwrap(),
        ))
    }

    async fn inspect(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.inspect_calls.fetch_add(1, Ordering::SeqCst);
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let RpcRequestBody::Inspect(inspect) = request.body else {
            return Err(Status::invalid_argument("expected Inspect"));
        };
        let telemetry = inspect_telemetry_fixture::observation(inspect.telemetry);
        Ok(Response::new(
            RpcResponse::from(MachineDetails {
                id: self.description.machine_id,
                phase: LocalMachinePhase::Participating,
                machine: None,
                public_key: WireGuardPublicKey([0; 32]),
                advertised_endpoints: Vec::new(),
                store_version: Default::default(),
                rtts: Vec::new(),
                cloud_paired: self.cloud_paired.load(Ordering::SeqCst),
                telemetry,
                storage: inspect
                    .include_storage
                    .then_some(MachineStorageObservation::Ready),
            })
            .encode()
            .unwrap(),
        ))
    }

    async fn machine_token(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn initialize(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn register(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if let Some(error) = self.register_error.lock().unwrap().clone() {
            return Ok(Response::new(RpcResponse::from(error).encode().unwrap()));
        }
        let RpcRequestBody::Register(body) = request.body else {
            return Err(Status::invalid_argument("expected Register"));
        };
        let assigned_machine = Machine {
            id: MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
            name: body.name,
            subnet: "10.210.1.0/24".parse().unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: body.public_key,
            public_ip: body.public_ip,
            advertised_endpoints: body.advertised_endpoints,
            runtime: body.runtime,
        };
        let visible_peers = self
            .machines
            .iter()
            .map(|observation| observation.machine.clone())
            .collect();
        Ok(Response::new(
            RpcResponse::from(Registered {
                assigned_machine,
                visible_peers,
                target_versions: BTreeMap::new(),
            })
            .encode()
            .unwrap(),
        ))
    }

    async fn join(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn set_cloud_pairing(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn list_machines(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.list_rpc_calls.fetch_add(1, Ordering::SeqCst);
        let removed = self.removed_machines.lock().unwrap().clone();
        let machines = self
            .machines
            .iter()
            .filter(|observation| !removed.contains(&observation.machine.id))
            .cloned()
            .collect();
        Ok(Response::new(
            RpcResponse::from(MachineList { machines })
                .encode()
                .unwrap(),
        ))
    }

    async fn list_containers(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.list_rpc_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(machine_id) = request
            .metadata()
            .get("machine")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| MachineId::parse(value).ok())
        {
            *self
                .container_list_calls
                .lock()
                .unwrap()
                .entry(machine_id)
                .or_default() += 1;
            if let Some(outcome) = self
                .container_list_outcomes
                .lock()
                .unwrap()
                .get_mut(&machine_id)
                .and_then(VecDeque::pop_front)
            {
                return outcome.map(|containers| {
                    Response::new(RpcResponse::from(containers).encode().unwrap())
                });
            }
        }
        Ok(Response::new(
            RpcResponse::from(ContainerList {
                containers: self.listed_containers.lock().unwrap().clone(),
            })
            .encode()
            .unwrap(),
        ))
    }

    async fn create_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if !self.accept_volume_creates {
            return Err(Status::unimplemented("unused"));
        }
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap()).unwrap();
        let request = request.into_inner().decode_request().unwrap();
        let RpcRequestBody::CreateVolume(create) = request.body else {
            return Err(Status::invalid_argument("expected create_volume"));
        };
        self.created_volumes
            .lock()
            .unwrap()
            .push((machine_id, create.clone()));
        let volume = self
            .existing_created_volume
            .clone()
            .unwrap_or_else(|| DockerVolume {
                id: DockerVolumeId {
                    machine_id,
                    name: create.name,
                },
                driver: create.driver,
                options: create.options,
                labels: create.labels,
            });
        Ok(Response::new(RpcResponse::from(volume).encode().unwrap()))
    }

    async fn inspect_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn list_volumes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.list_rpc_calls.fetch_add(1, Ordering::SeqCst);
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap()).unwrap();
        let request = request.into_inner().decode_request().unwrap();
        assert!(matches!(request.body, RpcRequestBody::ListVolumes(_)));
        if let Some(volumes) = self.listed_volumes.lock().unwrap().get(&machine_id) {
            return Ok(Response::new(
                RpcResponse::from(VolumeList {
                    volumes: volumes.clone(),
                })
                .encode()
                .unwrap(),
            ));
        }
        let response = if machine_id.as_str().starts_with('b') {
            RpcResponse::from(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "target unavailable".into(),
                details: Value::Null,
            })
        } else {
            RpcResponse::from(VolumeList {
                volumes: vec![DockerVolume {
                    id: DockerVolumeId {
                        machine_id,
                        name: DockerVolumeName::parse("data").unwrap(),
                    },
                    driver: "local".into(),
                    options: Default::default(),
                    labels: Default::default(),
                }],
            })
        };
        Ok(Response::new(response.encode().unwrap()))
    }

    async fn inspect_volume(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn create_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Ok(Response::new(
            RpcResponse::from(ContainerCreated {
                container_id: created_container_id(),
                display_name: "web-1".into(),
            })
            .encode()
            .unwrap(),
        ))
    }

    async fn ensure_global_slot(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn remove_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap()).unwrap();
        let request = request.into_inner().decode_request().unwrap();
        let RpcRequestBody::RemoveVolume(remove) = request.body else {
            return Err(Status::invalid_argument("expected remove_volume"));
        };
        let response = if machine_id.as_str().starts_with('b') {
            RpcResponse::from(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "target unavailable".into(),
                details: Value::Null,
            })
        } else if remove.name.as_str() == "missing" {
            RpcResponse::from(RpcError {
                code: RpcErrorCode::NotFound,
                message: "volume not found".into(),
                details: Value::Null,
            })
        } else if remove.name.as_str() == "busy" && !remove.force {
            RpcResponse::from(RpcError {
                code: RpcErrorCode::Conflict,
                message: "volume is in use".into(),
                details: Value::Null,
            })
        } else {
            self.removed_volumes.lock().unwrap().push(DockerVolumeId {
                machine_id,
                name: remove.name.clone(),
            });
            RpcResponse::from(VolumeRemoved {})
        };
        Ok(Response::new(response.encode().unwrap()))
    }

    async fn start_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Ok(Response::new(
            RpcResponse::from(ployz_core::ContainerChanged {
                container_id: created_container_id(),
            })
            .encode()
            .unwrap(),
        ))
    }

    async fn stop_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn remove_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn exec(
        &self,
        _request: Request<Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::ExecStream>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn container_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::ContainerLogsStream>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn machine_logs(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<Self::MachineLogsStream>, Status> {
        self.stream_opens.fetch_add(1, Ordering::SeqCst);
        Ok(Response::new(tokio_stream::empty()))
    }

    async fn runtime_watch(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<Self::RuntimeWatchStream>, Status> {
        self.watch_opens.fetch_add(1, Ordering::SeqCst);
        let decoded = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        let watch_request = op::RuntimeWatch::from_request_body(decoded.body)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        self.watch_requests.lock().unwrap().push(watch_request);
        Ok(Response::new(ReceiverStream::new(self.watch.subscribe())))
    }

    async fn list_images(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn ensure_image_ingest(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn pull_image_from_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_caddy_config(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn reserve_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn release_domain(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn create_domain_records(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn reset(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn update_machine(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn remove_local_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap()).unwrap();
        let request = request.into_inner().decode_request().unwrap();
        assert!(matches!(
            request.body,
            RpcRequestBody::RemoveLocalMachine(_)
        ));
        self.reset_machines.lock().unwrap().push(machine_id);
        Ok(Response::new(
            RpcResponse::from(LocalMachineRemoved {
                reset_warning: self.reset_warning.lock().unwrap().clone(),
            })
            .encode()
            .unwrap(),
        ))
    }

    async fn remove_machine(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let request = request.into_inner().decode_request().unwrap();
        let RpcRequestBody::RemoveMachine(RemoveMachineRequest { machine_id }) = request.body
        else {
            return Err(Status::invalid_argument("expected remove_machine"));
        };
        self.removed_machines.lock().unwrap().push(machine_id);
        Ok(Response::new(
            RpcResponse::from(MachineRemoved {}).encode().unwrap(),
        ))
    }

    async fn inspect_wireguard(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }
}

fn created_container_id() -> ContainerId {
    ContainerId::parse("1".repeat(64)).unwrap()
}

pub(super) fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation {
        machine: Machine {
            id: machine_id(hex),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        membership: MembershipObservation::Up,
        storage: None,
        selected_endpoint: None,
        rtt: None,
    }
}

pub(super) fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(super) fn test_description() -> ContractDescription {
    ContractDescription {
        machine_id: MachineId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "test".into(),
        capabilities: Default::default(),
    }
}

pub(super) async fn connected_client(
    service: DiscoveryService,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
    Arc<AtomicUsize>,
) {
    let (address, server) = serve_discovery(service).await;
    let connects = Arc::new(AtomicUsize::new(0));
    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp(address)],
        },
        Arc::new(CountingConnector::new(connects.clone())),
    )
    .await
    .unwrap();
    (client, server, connects)
}

pub(super) fn native_addon() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.join("..");
    let target = option_env!("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let names = ["libployz_sdk.so", "libployz_sdk.dylib", "ployz_sdk.dll"];
    for name in names {
        let path = target.join(profile).join(name);
        if path.is_file() {
            return path;
        }
    }
    let status = Command::new("cargo")
        .args(["build", "-p", "ployz-sdk", "--locked"])
        .current_dir(&workspace)
        .status()
        .expect("cargo build -p ployz-sdk");
    assert!(status.success(), "cargo build -p ployz-sdk failed");
    for name in names {
        let path = target.join(profile).join(name);
        if path.is_file() {
            return path;
        }
    }
    panic!(
        "ployz-sdk cdylib was not produced under {}",
        target.join(profile).display()
    );
}
