use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    num::NonZeroU64,
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
    CreateVolumeReport, CreateVolumeRequest, DataLoss, DataLossConfirmation, DockerVolume,
    DockerVolumeId, DockerVolumeName, DockerVolumeStorageObservation, LocalMachinePhase,
    LocalMachineRemoved, MANAGED_LABEL, Machine, MachineDetails, MachineId, MachineList,
    MachineName, MachineObservation, MachinePath, MachineRemoved, MachineRpc, MachineRpcServer,
    MachineStorageObservation, MembershipObservation, ObservedDataLoss, OpaquePayload,
    PROJECT_NAME_LABEL, PROTOCOL_MAJOR, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT, Registered,
    RemoveMachineRequest, Rpc, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse,
    RuntimeWatchFrame, RuntimeWatchRequest, VolumeInventory, VolumeObservationFailure,
    VolumeRemoved, WireGuardPublicKey, encode_runtime_watch_frame, op,
};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};
use tonic::{
    Request, Response, Status, Streaming,
    codec::CompressionEncoding,
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
                    .send_compressed(CompressionEncoding::Gzip)
                    .max_encoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT),
            )
            .serve_with_incoming(TcpListenerStream::new(tcp)),
    );
    (address, server)
}

pub(super) enum DescribeOutcome {
    Status(Status),
    Remote(RpcError),
    Hang(tokio::sync::oneshot::Sender<()>),
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
            encode_runtime_watch_frame(frame).map_err(|error| Status::internal(error.to_string()))
        }
        WatchEvent::Payload(payload) => Ok(payload.clone()),
        WatchEvent::Fail(status) => Err(status.clone()),
    };
    let _ = sender.try_send(item);
}

#[derive(Default)]
pub(super) struct EnrollmentTrace {
    pub events: Vec<&'static str>,
    pub published: Option<Registered>,
    pub joined: Option<ployz_core::JoinRequest>,
}

#[derive(Clone)]
pub(super) struct DiscoveryService {
    pub(super) enrollment: Option<Arc<Mutex<EnrollmentTrace>>>,
    pub(super) builds: Option<Arc<BuildRecorder>>,
    description: ContractDescription,
    /// Contracts answered per routed Machine. Absent Machines answer `description`.
    pub(super) descriptions: BTreeMap<MachineId, ContractDescription>,
    pub(super) describe_outcomes: Arc<Mutex<VecDeque<DescribeOutcome>>>,
    pub(super) stream_opens: Arc<AtomicUsize>,
    pub(super) watch_opens: Arc<AtomicUsize>,
    pub(super) list_rpc_calls: Arc<AtomicUsize>,
    pub(super) volume_list_calls: Arc<AtomicUsize>,
    pub(super) inspect_calls: Arc<AtomicUsize>,
    pub(super) storage: MachineStorageObservation,
    pub(super) storage_capacity: Option<ployz_core::StorageCapacity>,
    pub(super) recover_volume_on_storage_inspect: Option<DockerVolume>,
    pub(super) container_list_calls: Arc<Mutex<BTreeMap<MachineId, usize>>>,
    pub(super) container_list_outcomes: Arc<Mutex<ContainerListOutcomes>>,
    pub(super) watch_requests: Arc<Mutex<Vec<RuntimeWatchRequest>>>,
    pub(super) watch_accepts_gzip: Arc<AtomicBool>,
    watch: Arc<WatchHub>,
    pub(super) machines: Vec<MachineObservation>,
    pub(super) listed_volumes: Arc<Mutex<BTreeMap<MachineId, Vec<DockerVolume>>>>,
    pub(super) volume_observation_failures:
        Arc<Mutex<BTreeMap<MachineId, Vec<VolumeObservationFailure>>>>,
    pub(super) listed_containers: Arc<Mutex<Vec<ployz_core::ContainerObservation>>>,
    pub(super) accept_volume_creates: bool,
    pub(super) existing_created_volume: Option<DockerVolume>,
    pub(super) created_volume_verification_error: Option<RpcError>,
    pub(super) create_container_error: Option<RpcError>,
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
            enrollment: None,
            builds: None,
            description,
            descriptions: BTreeMap::new(),
            describe_outcomes: Arc::new(Mutex::new(VecDeque::new())),
            stream_opens: Arc::new(AtomicUsize::new(0)),
            watch_opens: Arc::new(AtomicUsize::new(0)),
            list_rpc_calls: Arc::new(AtomicUsize::new(0)),
            volume_list_calls: Arc::new(AtomicUsize::new(0)),
            inspect_calls: Arc::new(AtomicUsize::new(0)),
            storage: MachineStorageObservation::Ready,
            storage_capacity: None,
            recover_volume_on_storage_inspect: None,
            container_list_calls: Arc::new(Mutex::new(BTreeMap::new())),
            container_list_outcomes: Arc::new(Mutex::new(BTreeMap::new())),
            watch_requests: Arc::new(Mutex::new(Vec::new())),
            watch_accepts_gzip: Arc::new(AtomicBool::new(false)),
            watch: Arc::new(WatchHub::new()),
            machines: vec![machine('a', "one")],
            listed_volumes: Arc::new(Mutex::new(BTreeMap::new())),
            volume_observation_failures: Arc::new(Mutex::new(BTreeMap::new())),
            listed_containers: Arc::new(Mutex::new(Vec::new())),
            accept_volume_creates: false,
            existing_created_volume: None,
            created_volume_verification_error: None,
            create_container_error: None,
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

    pub(super) fn end_watch(&self) {
        self.watch.live.lock().unwrap().clear();
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
    type BuildStream = ReceiverStream<Result<OpaquePayload, Status>>;

    async fn build(
        &self,
        request: Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<Response<Self::BuildStream>, Status> {
        let recorder = self
            .builds
            .clone()
            .ok_or_else(|| Status::unimplemented("Build is not used by this fixture"))?;
        let route = ployz_core::routing_from_metadata(request.metadata()).unwrap();
        let machine_id = self.description.machine_id;
        let (sender, receiver) = mpsc::channel(2);
        tokio::spawn(async move {
            use ployz_build::{
                Output,
                remote::{self, Event, Input, Outcome},
            };
            let mut request = request.into_inner();
            let first = request.message().await.unwrap().unwrap();
            let frame = remote::decode(&first).unwrap();
            let targets = match &frame {
                Input::Start(definition) => &definition.targets,
                Input::Check(targets) => targets,
                Input::Entry { .. } | Input::Data(_) | Input::Finish | Input::Cancel => {
                    panic!("expected Build start or capability check")
                }
            };
            if matches!(frame, Input::Start(_)) {
                recorder.routes.lock().unwrap().push(route);
                recorder
                    .targets
                    .lock()
                    .unwrap()
                    .push(targets.iter().map(|target| target.name.clone()).collect());
            }
            if recorder.queued {
                sender
                    .send(Ok(remote::encode(&Event::Progress(
                        ployz_build::Progress::Stage(ployz_build::Stage::Queued),
                    ))
                    .unwrap()))
                    .await
                    .unwrap();
                assert!(
                    tokio::time::timeout(std::time::Duration::from_millis(50), request.message())
                        .await
                        .is_err(),
                    "client uploaded before admission"
                );
            }
            if matches!(frame, Input::Start(_))
                && let Some(outcome) = &recorder.admission_outcome
            {
                let _ = sender
                    .send(Ok(
                        remote::encode(&Event::Finished(outcome.clone())).unwrap()
                    ))
                    .await;
                return;
            }
            sender
                .send(Ok(remote::encode(&Event::Admitted {
                    machine_id,
                    active_timeout: ployz_build::EXECUTION_TIMEOUT,
                })
                .unwrap()))
                .await
                .unwrap();
            let Input::Start(definition) = frame else {
                sender
                    .send(Ok(remote::encode(&Event::Finished(
                        Outcome::CapabilitiesChecked { machine_id },
                    ))
                    .unwrap()))
                    .await
                    .unwrap();
                return;
            };
            let mut upload = remote::Upload::new().unwrap();
            loop {
                let payload = request.message().await.unwrap().unwrap();
                let frame: Input = remote::decode(&payload).unwrap();
                let finish = matches!(frame, Input::Finish);
                upload.accept(frame).unwrap();
                if finish {
                    break;
                }
            }
            recorder.uploads.fetch_add(1, Ordering::SeqCst);
            let outcome = match definition.output {
                Output::Validate => Outcome::Validated { machine_id },
                Output::Registry => Outcome::Published { machine_id },
                Output::Load => Outcome::Images {
                    machine_id,
                    images: definition
                        .targets
                        .iter()
                        .map(|_| ployz_build::BuiltImage {
                            reference: format!("sha256:{}", "1".repeat(64)),
                            tags: vec!["example.test/api:built".into()],
                            platforms: vec!["linux/amd64".into()],
                            location: "unix:///var/run/docker.sock".into(),
                        })
                        .collect(),
                },
            };
            let _ = sender
                .send(Ok(remote::encode(&Event::Finished(outcome)).unwrap()))
                .await;
        });
        Ok(Response::new(ReceiverStream::new(receiver)))
    }
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
            Some(DescribeOutcome::Hang(received)) => {
                received.send(()).unwrap();
                return std::future::pending().await;
            }
            None => {}
        }
        let metadata = request.metadata().clone();
        let request = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        if !matches!(request.body, RpcRequestBody::DescribeContract(_)) {
            return Err(Status::invalid_argument("expected discovery request"));
        }
        let routed = match ployz_core::routing_from_metadata(&metadata) {
            Ok(ployz_core::RoutingRequest::One(target)) => self
                .machines
                .iter()
                .find(|observation| {
                    ployz_core::machine_matches_target(&observation.machine, &target)
                })
                .and_then(|observation| self.descriptions.get(&observation.machine.id)),
            _ => None,
        };
        Ok(Response::new(
            RpcResponse::from(routed.unwrap_or(&self.description).clone())
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
                storage: inspect.include_storage.then_some(self.storage),
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
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: body.machine_id,
            name: body.name,
            subnet: body.assigned_subnet.expect("client supplies subnet"),
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
        let registered = Registered {
            assigned_machine,
            visible_peers,
            target_versions: BTreeMap::new(),
        };
        if let Some(trace) = &self.enrollment {
            let mut trace = trace.lock().unwrap();
            trace.events.push("publish");
            trace.published = Some(registered.clone());
        }
        Ok(Response::new(
            RpcResponse::from(registered).encode().unwrap(),
        ))
    }

    async fn join(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        let trace = self
            .enrollment
            .as_ref()
            .ok_or_else(|| Status::unimplemented("unused"))?;
        let request =
            op::Join::from_request_body(request.into_inner().decode_request().unwrap().body)
                .unwrap();
        let mut trace = trace.lock().unwrap();
        assert_eq!(
            trace.published.as_ref(),
            Some(&request.registration),
            "publication must precede Join"
        );
        trace.events.push("join");
        let already_accepted = trace.joined.is_some();
        trace.joined = Some(request);
        if !already_accepted {
            return Err(Status::internal(
                "lost Join response after durable acceptance",
            ));
        }
        Ok(Response::new(
            RpcResponse::from(ployz_core::JoinAccepted { already_accepted })
                .encode()
                .unwrap(),
        ))
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
        let machines: Vec<_> = self
            .machines
            .iter()
            .filter(|observation| !removed.contains(&observation.machine.id))
            .cloned()
            .collect();
        Ok(Response::new(
            RpcResponse::from(MachineList {
                enrollment: self
                    .enrollment
                    .as_ref()
                    .map(|_| ployz_core::EnrollmentSnapshot {
                        network: "10.210.0.0/16".parse().unwrap(),
                        machines: machines
                            .iter()
                            .map(|observation| observation.machine.clone())
                            .collect(),
                        target_versions: BTreeMap::new(),
                    }),
                machines,
            })
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
            .unwrap_or_else(|| created_volume(machine_id, create));
        let report = match self.created_volume_verification_error.clone() {
            Some(error) => CreateVolumeReport::Unverified {
                id: volume.id,
                error,
            },
            None => CreateVolumeReport::Verified { volume },
        };
        Ok(Response::new(RpcResponse::from(report).encode().unwrap()))
    }

    async fn request_machine_upgrade(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn inspect_machine_upgrade(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn inspect_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn get_container_observations(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        Err(Status::unimplemented("unused"))
    }

    async fn inspect_storage(
        &self,
        _request: tonic::Request<ployz_core::OpaquePayload>,
    ) -> Result<tonic::Response<ployz_core::OpaquePayload>, tonic::Status> {
        let capacity = self.storage_capacity.as_ref().ok_or_else(|| {
            Status::unimplemented("storage capacity not supplied by this fixture")
        })?;
        if let Some(volume) = &self.recover_volume_on_storage_inspect {
            self.listed_volumes
                .lock()
                .unwrap()
                .insert(volume.id.machine_id, vec![volume.clone()]);
            self.volume_observation_failures
                .lock()
                .unwrap()
                .remove(&volume.id.machine_id);
        }
        Ok(Response::new(
            RpcResponse::from(capacity.clone()).encode().unwrap(),
        ))
    }
    async fn prepare_volumes(
        &self,
        _request: tonic::Request<ployz_core::OpaquePayload>,
    ) -> Result<tonic::Response<ployz_core::OpaquePayload>, tonic::Status> {
        Err(tonic::Status::unimplemented(
            "storage preparation not supplied by this fixture",
        ))
    }

    async fn list_volumes(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.list_rpc_calls.fetch_add(1, Ordering::SeqCst);
        self.volume_list_calls.fetch_add(1, Ordering::SeqCst);
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap()).unwrap();
        let request = request.into_inner().decode_request().unwrap();
        assert!(matches!(request.body, RpcRequestBody::ListVolumes(_)));
        if let Some(volumes) = self.listed_volumes.lock().unwrap().get(&machine_id) {
            return Ok(Response::new(
                RpcResponse::from(VolumeInventory {
                    volumes: volumes.clone(),
                    failures: self
                        .volume_observation_failures
                        .lock()
                        .unwrap()
                        .get(&machine_id)
                        .cloned()
                        .unwrap_or_default(),
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
            RpcResponse::from(VolumeInventory {
                volumes: vec![DockerVolume {
                    id: DockerVolumeId {
                        machine_id,
                        name: DockerVolumeName::parse("data").unwrap(),
                    },
                    options: Default::default(),
                    labels: Default::default(),
                    storage: ployz_core::DockerVolumeStorageObservation::Plain {
                        driver: "local".into(),
                    },
                }],
                failures: Vec::new(),
            })
        };
        Ok(Response::new(response.encode().unwrap()))
    }

    async fn inspect_volume(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        self.inspect_calls.fetch_add(1, Ordering::SeqCst);
        let machine_id =
            MachineId::parse(request.metadata().get("machine").unwrap().to_str().unwrap()).unwrap();
        let request = request.into_inner().decode_request().unwrap();
        let RpcRequestBody::InspectVolume(inspect) = request.body else {
            return Err(Status::invalid_argument("expected inspect_volume"));
        };
        let response = self
            .listed_volumes
            .lock()
            .unwrap()
            .get(&machine_id)
            .and_then(|volumes| volumes.iter().find(|volume| volume.id.name == inspect.name))
            .cloned()
            .map_or_else(
                || {
                    RpcResponse::from(RpcError {
                        code: RpcErrorCode::NotFound,
                        message: format!("Docker Volume {:?} was not found", inspect.name),
                        details: Value::Null,
                    })
                },
                RpcResponse::from,
            );
        Ok(Response::new(response.encode().unwrap()))
    }

    async fn create_container(
        &self,
        _request: Request<OpaquePayload>,
    ) -> Result<Response<OpaquePayload>, Status> {
        if let Some(error) = &self.create_container_error {
            return Ok(Response::new(
                RpcResponse::from(error.clone()).encode().unwrap(),
            ));
        }
        Ok(Response::new(
            RpcResponse::from(ContainerCreated {
                container_id: created_container_id(),
                display_name: "web-1".into(),
            })
            .encode()
            .unwrap(),
        ))
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
        if remove.name.as_str() == "slow" {
            std::future::pending::<()>().await;
        }
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
        self.watch_accepts_gzip.store(
            request
                .metadata()
                .get("grpc-accept-encoding")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|encodings| encodings.split(',').any(|item| item.trim() == "gzip")),
            Ordering::SeqCst,
        );
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

    async fn get_ingress_proxy_config(
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

pub(super) fn created_volume(machine_id: MachineId, create: CreateVolumeRequest) -> DockerVolume {
    let storage = if create.driver == "ployz" {
        let size = create.options.get("size").unwrap();
        let (amount, suffix) = size.split_at(size.len() - 1);
        let multiplier = match suffix {
            "b" => 1,
            "k" => 1024_u64,
            "m" => 1024_u64.pow(2),
            "g" => 1024_u64.pow(3),
            "t" => 1024_u64.pow(4),
            _ => panic!("unexpected Provisioned Volume size {size}"),
        };
        DockerVolumeStorageObservation::Provisioned {
            mountpoint: MachinePath::parse(format!("/var/lib/ployz-volumes/{}", create.name))
                .unwrap(),
            bound_bytes: NonZeroU64::new(amount.parse::<u64>().unwrap() * multiplier).unwrap(),
            used_bytes: 0,
        }
    } else {
        DockerVolumeStorageObservation::Plain {
            driver: create.driver,
        }
    };
    DockerVolume {
        id: DockerVolumeId {
            machine_id,
            name: create.name,
        },
        options: create.options,
        labels: create.labels,
        storage,
    }
}

fn created_container_id() -> ContainerId {
    ContainerId::parse("1".repeat(64)).unwrap()
}

pub(super) fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation::new(
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: machine_id(hex),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        MembershipObservation::Up,
    )
}

pub(super) fn volume_id(machine_id: MachineId, name: &str) -> DockerVolumeId {
    DockerVolumeId {
        machine_id,
        name: DockerVolumeName::parse(name).unwrap(),
    }
}

pub(super) fn docker_volume(machine_id: MachineId, name: &str) -> DockerVolume {
    DockerVolume {
        id: volume_id(machine_id, name),
        options: Default::default(),
        labels: Default::default(),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}

pub(super) fn owned_volume(machine_id: MachineId, name: &str, project: &str) -> DockerVolume {
    DockerVolume {
        labels: BTreeMap::from([
            (MANAGED_LABEL.to_owned(), String::new()),
            (PROJECT_NAME_LABEL.to_owned(), project.to_owned()),
        ]),
        ..docker_volume(machine_id, name)
    }
}

pub(super) fn machine_named(id: &MachineId, name: &str) -> MachineObservation {
    let mut observation = machine('a', name);
    observation.machine.id = *id;
    observation
}

pub(super) fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(super) fn confirmation(data_loss: impl IntoIterator<Item = DataLoss>) -> DataLossConfirmation {
    let observed = ObservedDataLoss {
        data_loss: data_loss.into_iter().collect(),
    };
    observed
        .confirm_names(observed.data_loss.iter().map(DataLoss::name))
        .expect("all observed Data Loss is named")
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
    // Keep semantic RPC tests inside Tokio so paused time never outruns OS I/O.
    // Transport tests use serve_discovery / SystemConnector directly.
    struct MemoryConnector {
        incoming: mpsc::UnboundedSender<Result<tokio::io::DuplexStream, std::io::Error>>,
        connects: Arc<AtomicUsize>,
    }
    #[tonic::async_trait]
    impl Connector for MemoryConnector {
        async fn connect(&self, _: &Connection) -> Result<Channel, ConnectError> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            let incoming = self.incoming.clone();
            Channel::from_static("http://memory.invalid")
                .connect_with_connector(tower::service_fn(move |_| {
                    let (client, server) = tokio::io::duplex(64 * 1024);
                    incoming.send(Ok(server)).unwrap();
                    async move { Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(client)) }
                }))
                .await
                .map_err(Into::into)
        }

        async fn dial_proxy(
            &self,
            _: &Connection,
            _: &str,
            _: &str,
        ) -> Result<BoxProxyStream, ConnectError> {
            Err(ConnectError::Attempt("unused".into()))
        }
    }
    let (incoming, receiver) = mpsc::unbounded_channel();
    let server = tokio::spawn(
        Server::builder()
            .add_service(MachineRpcServer::new(service))
            .serve_with_incoming(tokio_stream::wrappers::UnboundedReceiverStream::new(
                receiver,
            )),
    );
    let connects = Arc::new(AtomicUsize::new(0));
    let client = connect_selected_with(
        SelectedConnections {
            source: ConnectionSource::Direct,
            connections: vec![Connection::tcp("127.0.0.1:1".parse().unwrap())],
        },
        Arc::new(MemoryConnector {
            incoming,
            connects: connects.clone(),
        }),
    )
    .await
    .unwrap();
    (client, server, connects)
}

/// Records the owned Build transport. It never invokes a build tool and makes
/// no claim that its synthetic image exists; informing tests establish that.
#[derive(Default)]
pub(super) struct BuildRecorder {
    pub(super) routes: Mutex<Vec<ployz_core::RoutingRequest>>,
    pub(super) targets: Mutex<Vec<Vec<String>>>,
    pub(super) uploads: AtomicUsize,
    pub(super) queued: bool,
    pub(super) admission_outcome: Option<ployz_build::remote::Outcome>,
}
