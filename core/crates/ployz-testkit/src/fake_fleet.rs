//! A fake fleet of Machines for the agent eval.
//!
//! Each online Machine answers `MachineRpc` on `{socket_dir}/{machine_id}.sock`, the
//! way a real daemon answers on its local socket. Every socket shares one state, so a
//! request routed through web-1 to web-2 sees and changes web-2. Every request is
//! appended to the ledger before its effect, which lets an eval grade the actions an
//! agent issued even when a fault drops the reply.
#![expect(
    clippy::result_large_err,
    reason = "every handler answers tonic's Status, which tonic sizes"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs::File,
    io::{self, Write as _},
    num::{NonZeroU32, NonZeroU64},
    os::unix::net::UnixStream as StdUnixStream,
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard},
};

use ployz_core::{
    CapabilityAdvertisement, ContainerChanged, ContainerDetails, ContainerId, ContainerKind,
    ContainerList, ContainerObservation, ContainerObservationParts, ContainerRuntimeObservation,
    ContractDescription, CopyRole, DockerVolume, DockerVolumeId, DockerVolumeName,
    DockerVolumeStorageObservation, HealthObservation, HistoryContainer, HistoryContainerKind,
    HistoryRow, HistoryStream, LiveLogTarget, LocalMachinePhase, LogBody, LogDirection, LogEntry,
    LogHistoryRequest, LogMetadata, LogOrigin, MANAGED_LABEL, Machine, MachineDetails, MachineId,
    MachineList, MachineName, MachineObservation, MachinePath, MachineRelease, MachineRpc,
    MachineRpcServer, MachineRuntime, MachineUpgradeAttempt, MachineUpgradeOutcome, MachineVersion,
    MembershipObservation, NAMESPACE_LABEL, Namespace, OpaquePayload, PROTOCOL_MAJOR,
    ProvisionedCopy, ProvisionedVolumeMaximumBytes, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT,
    ResolvedServiceSpec, RoutingRequest, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse,
    RuntimeWatchFrame, ServiceId, ServiceName, SlotId, StorageBacking, StorageCapacity, VolumeCopy,
    VolumeCopyView, VolumeInventory, WireGuardPublicKey, WriterEpoch, encode_runtime_watch_frame,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{net::UnixListener, sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming, codec::CompressionEncoding};

const GIB: u64 = 1 << 30;
const POOL_BYTES: u64 = 100 * GIB;
const VOLUME_BYTES: NonZeroU64 = NonZeroU64::new(10 * GIB).expect("10 GiB is not zero");
const USED_BYTES: u64 = GIB;
const FIRST_LOG_NANOS: i64 = 1_700_000_000_000_000_000;
const LOG_STEP_NANOS: i64 = 1_000_000_000;
const DEPLOYMENT_LABEL: &str = "ployz.deployment.id";

/// `dashboard/evals/agent/machines.json`: the fleet as it starts.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FleetFile {
    pub machines: Vec<FleetMachine>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FleetMachine {
    pub name: MachineName,
    pub id: MachineId,
    pub version: MachineVersion,
    pub online: bool,
    #[serde(default)]
    pub containers: Vec<FleetContainer>,
    #[serde(default)]
    pub volumes: Vec<FleetVolume>,
}

/// One Container. The fixture's `id` is the Docker display name; its wire Container ID
/// is derived from it, since a real one is 64 hex characters.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FleetContainer {
    #[serde(rename = "id")]
    pub name: String,
    pub namespace: Namespace,
    pub service: ServiceName,
    pub image: String,
    pub state: RunState,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub deployment: Option<u64>,
    #[serde(default)]
    pub logs: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    Running,
    Exited,
}

/// A writer copy of a Namespace's Volume.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FleetVolume {
    pub namespace: Namespace,
    pub name: DockerVolumeName,
}

/// Per-task changes to the fleet file, passed as `--scenario <json>`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// Mirror slots to seed.
    #[serde(default)]
    pub mirrors: Vec<VolumePlace>,
    /// Writer copies to remove from the fleet file.
    #[serde(default)]
    pub drop_volumes: Vec<VolumePlace>,
    #[serde(default)]
    pub faults: Vec<Fault>,
}

/// One copy of a Namespace's Volume on one Machine.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VolumePlace {
    pub namespace: Namespace,
    pub volume: DockerVolumeName,
    pub machine: MachineName,
}

/// A fault on one Machine's command. Both still record the request and apply it.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "fault", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fault {
    /// Answer the `nth` such request (1-based, retries included) Unavailable.
    DropReply {
        machine: MachineName,
        command: String,
        nth: NonZeroU32,
    },
    /// After the first such request, close that Machine's sockets and mark it offline.
    DisconnectAfter {
        machine: MachineName,
        command: String,
    },
}

/// One request a Machine received, recorded before its effect.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LedgerEntry {
    /// Starts at 1.
    pub seq: u64,
    /// The target Machine's name.
    pub machine: MachineName,
    /// The RPC command, as in `RpcRequestBody::command`.
    pub command: String,
    /// The fixture display name of the Container the request names, if any.
    pub container: Option<String>,
    /// The decoded request payload; `null` for Exec and Build.
    pub request: Value,
}

#[derive(Debug, Error)]
pub enum FakeFleetError {
    #[error("no Machine named {0} in the fleet")]
    UnknownMachine(MachineName),
    #[error("{machine} holds no writer copy of {namespace}/{volume}")]
    UnknownVolume {
        namespace: Namespace,
        volume: DockerVolumeName,
        machine: MachineName,
    },
    #[error("no RPC command is named {0}")]
    UnknownCommand(String),
    #[error("fixture Container {name} is invalid: {reason}")]
    Container { name: String, reason: String },
    #[error("fleet state lock is poisoned")]
    Poisoned,
    #[error("ledger write failed")]
    Ledger(#[source] io::Error),
    #[error("socket {path} could not be bound")]
    Bind {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A running fake fleet. Dropping it closes every socket.
pub struct FakeFleet {
    shared: Arc<Shared>,
}

impl FakeFleet {
    /// Load `fleet` with `scenario` applied and bind a socket for every online Machine.
    /// Must run inside a Tokio runtime.
    ///
    /// # Errors
    /// Returns an error when the scenario names a Machine, Volume or command the fleet
    /// lacks, a fixture Container is invalid, or a socket cannot be bound.
    pub fn start(
        fleet: FleetFile,
        scenario: Scenario,
        socket_dir: impl Into<PathBuf>,
        ledger: Option<File>,
    ) -> Result<Self, FakeFleetError> {
        let state = Fleet::new(fleet, scenario, ledger)?;
        let online: Vec<MachineName> = state
            .machines
            .iter()
            .filter(|machine| machine.online)
            .map(|machine| machine.name.clone())
            .collect();
        let fleet = Self {
            shared: Arc::new(Shared {
                fleet: Mutex::new(state),
                served: Mutex::new(BTreeMap::new()),
                socket_dir: socket_dir.into(),
            }),
        };
        for name in online {
            fleet.shared.serve(&name)?;
        }
        Ok(fleet)
    }

    /// Mark `name` online and bind its socket. Idempotent.
    ///
    /// # Errors
    /// Returns an error for an unknown Machine or a socket that cannot be bound.
    pub fn online(&self, name: &MachineName) -> Result<(), FakeFleetError> {
        self.shared.lock()?.machine_mut(name)?.online = true;
        self.shared.serve(name)
    }

    /// Mark `name` offline, close its connections and remove its socket. Idempotent.
    ///
    /// # Errors
    /// Returns an error for an unknown Machine.
    pub fn offline(&self, name: &MachineName) -> Result<(), FakeFleetError> {
        self.shared.lock()?.machine_mut(name)?.online = false;
        self.shared.close(name);
        Ok(())
    }

    /// Every request recorded so far, in order.
    ///
    /// # Errors
    /// Returns an error when the state lock is poisoned.
    pub fn ledger(&self) -> Result<Vec<LedgerEntry>, FakeFleetError> {
        Ok(self.shared.lock()?.ledger.clone())
    }

    /// Whether `name` is online.
    ///
    /// # Errors
    /// Returns an error for an unknown Machine.
    pub fn is_online(&self, name: &MachineName) -> Result<bool, FakeFleetError> {
        Ok(self.shared.lock()?.machine(name)?.online)
    }

    /// Whether the fixture Container `container` on `name` is running.
    ///
    /// # Errors
    /// Returns an error for an unknown Machine or Container.
    pub fn is_running(&self, name: &MachineName, container: &str) -> Result<bool, FakeFleetError> {
        let fleet = self.shared.lock()?;
        fleet
            .machine(name)?
            .containers
            .iter()
            .find(|candidate| candidate.spec.name == container)
            .map(|found| found.spec.state == RunState::Running)
            .ok_or_else(|| FakeFleetError::Container {
                name: container.to_owned(),
                reason: format!("not on {name}"),
            })
    }

    /// Where `name`'s socket is bound while it is online.
    ///
    /// # Errors
    /// Returns an error for an unknown Machine.
    pub fn socket(&self, name: &MachineName) -> Result<PathBuf, FakeFleetError> {
        let id = self.shared.lock()?.machine(name)?.id;
        Ok(self.shared.socket_path(id))
    }
}

impl Drop for FakeFleet {
    fn drop(&mut self) {
        let names: Vec<MachineName> = match self.shared.served.lock() {
            Ok(served) => served.keys().cloned().collect(),
            Err(_) => return,
        };
        for name in names {
            self.shared.close(&name);
        }
    }
}

/// The wire Container ID of a fixture Container: sha256 of its display name.
#[must_use]
pub fn container_id(name: &str) -> ContainerId {
    ContainerId::parse(sha256_hex(name)).expect("sha256 hex is 64 lowercase hex characters")
}

fn service_id(namespace: &Namespace, service: &ServiceName) -> ServiceId {
    let hex = sha256_hex(&format!("{namespace}/{service}"));
    ServiceId::parse(hex.get(..32).unwrap_or_default())
        .expect("a sha256 hex prefix is 32 lowercase hex characters")
}

fn sha256_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

struct Shared {
    fleet: Mutex<Fleet>,
    served: Mutex<BTreeMap<MachineName, Served>>,
    socket_dir: PathBuf,
}

struct Served {
    accept: JoinHandle<()>,
    sockets: Arc<Mutex<Vec<StdUnixStream>>>,
    path: PathBuf,
}

impl Shared {
    fn lock(&self) -> Result<MutexGuard<'_, Fleet>, FakeFleetError> {
        self.fleet.lock().map_err(|_| FakeFleetError::Poisoned)
    }

    fn socket_path(&self, id: MachineId) -> PathBuf {
        self.socket_dir.join(format!("{id}.sock"))
    }

    fn serve(self: &Arc<Self>, name: &MachineName) -> Result<(), FakeFleetError> {
        let id = self.lock()?.machine(name)?.id;
        let mut served = self.served.lock().map_err(|_| FakeFleetError::Poisoned)?;
        if served.contains_key(name) {
            return Ok(());
        }
        let path = self.socket_path(id);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).map_err(|source| FakeFleetError::Bind {
            path: path.clone(),
            source,
        })?;
        let sockets = Arc::new(Mutex::new(Vec::new()));
        let service = FleetService {
            shared: Arc::clone(self),
            machine: id,
        };
        let accept = tokio::spawn(accept_loop(listener, service, Arc::clone(&sockets)));
        served.insert(
            name.clone(),
            Served {
                accept,
                sockets,
                path,
            },
        );
        Ok(())
    }

    fn close(&self, name: &MachineName) {
        let Some(served) = self
            .served
            .lock()
            .ok()
            .and_then(|mut served| served.remove(name))
        else {
            return;
        };
        served.accept.abort();
        // Tonic owns each accepted connection after the accept loop is gone.
        if let Ok(mut sockets) = served.sockets.lock() {
            for socket in sockets.drain(..) {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        }
        let _ = std::fs::remove_file(&served.path);
    }

    /// Record `request` for `target`, apply it, then let a matching fault decide the
    /// answer. Disconnecting happens after the state lock is released.
    fn handle(&self, target: MachineId, request: Received) -> Result<Reply, Status> {
        let (reply, disconnect) = {
            let mut fleet = self.lock().map_err(internal)?;
            let fault = fleet.admit(target, &request)?;
            let reply = match request {
                Received::Body(body) => fleet.apply(target, *body),
                Received::Bare(command) => Err(unimplemented(command)),
            };
            match fault {
                None => (reply, None),
                Some(FaultAction::DropReply) => (Err(Status::unavailable("reply dropped")), None),
                Some(FaultAction::Disconnect) => {
                    let machine = fleet.by_id_mut(target)?;
                    machine.online = false;
                    (
                        Err(Status::unavailable("Machine disconnected")),
                        Some(machine.name.clone()),
                    )
                }
            }
        };
        if let Some(name) = disconnect {
            self.close(&name);
        }
        reply
    }
}

async fn accept_loop(
    listener: UnixListener,
    service: FleetService,
    sockets: Arc<Mutex<Vec<StdUnixStream>>>,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(socket) = stream.into_std() else {
            continue;
        };
        if let (Ok(clone), Ok(mut saved)) = (socket.try_clone(), sockets.lock()) {
            saved.push(clone);
        }
        let Ok(stream) = tokio::net::UnixStream::from_std(socket) else {
            continue;
        };
        let service = service.clone();
        connections.spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_service(
                    MachineRpcServer::new(service)
                        .send_compressed(CompressionEncoding::Gzip)
                        .max_encoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT),
                )
                .serve_with_incoming(tokio_stream::once(Ok::<_, io::Error>(stream)))
                .await;
        });
    }
}

enum Received {
    Body(Box<RpcRequestBody>),
    /// Exec and Build: streamed, schema-blind, outside the request enum.
    Bare(&'static str),
}

impl Received {
    fn command(&self) -> &'static str {
        match self {
            Self::Body(body) => body.command(),
            Self::Bare(command) => command,
        }
    }

    fn payload(&self) -> Result<Value, Status> {
        let Self::Body(body) = self else {
            return Ok(Value::Null);
        };
        let mut tagged = serde_json::to_value(body).map_err(internal)?;
        Ok(tagged.get_mut("payload").map_or(Value::Null, Value::take))
    }
}

enum Reply {
    Unary(Box<RpcResponse>),
    Frames {
        frames: Vec<OpaquePayload>,
        hold: bool,
    },
}

enum FaultAction {
    DropReply,
    Disconnect,
}

struct ArmedFault {
    fault: Fault,
    fired: bool,
}

struct MachineState {
    name: MachineName,
    id: MachineId,
    version: MachineVersion,
    online: bool,
    containers: Vec<ContainerState>,
    copies: Vec<CopyState>,
    upgrades: Vec<MachineUpgradeAttempt>,
}

struct ContainerState {
    spec: FleetContainer,
    id: ContainerId,
}

struct CopyState {
    namespace: Namespace,
    /// The physical Docker Volume name, `{namespace}_{volume}`.
    name: DockerVolumeName,
    role: CopyRole,
}

struct Fleet {
    machines: Vec<MachineState>,
    faults: Vec<ArmedFault>,
    calls: BTreeMap<(MachineId, &'static str), u32>,
    ledger: Vec<LedgerEntry>,
    sink: Option<File>,
}

impl Fleet {
    fn new(
        file: FleetFile,
        scenario: Scenario,
        sink: Option<File>,
    ) -> Result<Self, FakeFleetError> {
        let mut machines = file
            .machines
            .into_iter()
            .map(MachineState::new)
            .collect::<Result<Vec<_>, _>>()?;
        let mut fleet = Self {
            machines: Vec::new(),
            faults: Vec::new(),
            calls: BTreeMap::new(),
            ledger: Vec::new(),
            sink,
        };
        for place in scenario.drop_volumes {
            let name = physical(&place);
            let machine = find_mut(&mut machines, &place.machine)?;
            let before = machine.copies.len();
            machine.copies.retain(|copy| copy.name != name);
            if machine.copies.len() == before {
                return Err(FakeFleetError::UnknownVolume {
                    namespace: place.namespace,
                    volume: place.volume,
                    machine: place.machine,
                });
            }
        }
        for place in scenario.mirrors {
            let name = physical(&place);
            find_mut(&mut machines, &place.machine)?
                .copies
                .push(CopyState {
                    namespace: place.namespace,
                    name,
                    role: CopyRole::Slot,
                });
        }
        for fault in scenario.faults {
            let (machine, command) = match &fault {
                Fault::DropReply {
                    machine, command, ..
                }
                | Fault::DisconnectAfter { machine, command } => (machine, command),
            };
            find_mut(&mut machines, machine)?;
            if !COMMANDS.contains(&command.as_str()) {
                return Err(FakeFleetError::UnknownCommand(command.clone()));
            }
            fleet.faults.push(ArmedFault {
                fault,
                fired: false,
            });
        }
        fleet.machines = machines;
        Ok(fleet)
    }

    fn machine(&self, name: &MachineName) -> Result<&MachineState, FakeFleetError> {
        self.machines
            .iter()
            .find(|machine| &machine.name == name)
            .ok_or_else(|| FakeFleetError::UnknownMachine(name.clone()))
    }

    fn machine_mut(&mut self, name: &MachineName) -> Result<&mut MachineState, FakeFleetError> {
        find_mut(&mut self.machines, name)
    }

    fn by_id(&self, id: MachineId) -> Result<&MachineState, Status> {
        self.machines
            .iter()
            .find(|machine| machine.id == id)
            .ok_or_else(|| Status::not_found(format!("no Machine {id} in the fleet")))
    }

    fn by_id_mut(&mut self, id: MachineId) -> Result<&mut MachineState, Status> {
        self.machines
            .iter_mut()
            .find(|machine| machine.id == id)
            .ok_or_else(|| Status::not_found(format!("no Machine {id} in the fleet")))
    }

    fn route(&self, routing: RoutingRequest, local: MachineId) -> Result<MachineId, Status> {
        match routing {
            RoutingRequest::Local => Ok(local),
            RoutingRequest::One(target) => self
                .machines
                .iter()
                .find(|machine| ployz_core::machine_matches_target(&machine.record(), &target))
                .map(|machine| machine.id)
                .ok_or_else(|| Status::not_found(format!("no Machine matches {target}"))),
            RoutingRequest::Many(_) => Err(Status::unimplemented(
                "the fake fleet does not fan out requests",
            )),
        }
    }

    /// Refuse an offline target unrecorded; otherwise record the request and say which
    /// fault, if any, now fires.
    fn admit(
        &mut self,
        target: MachineId,
        request: &Received,
    ) -> Result<Option<FaultAction>, Status> {
        let machine = self.by_id(target)?;
        if !machine.online {
            return Err(Status::unavailable(format!("{} is offline", machine.name)));
        }
        let command = request.command();
        let payload = request.payload()?;
        let container = payload
            .get("container_id")
            .and_then(Value::as_str)
            .and_then(|id| {
                machine
                    .containers
                    .iter()
                    .find(|container| container.id.as_str() == id)
            })
            .map(|container| container.spec.name.clone());
        let entry = LedgerEntry {
            seq: self.ledger.len() as u64 + 1,
            machine: machine.name.clone(),
            command: command.to_owned(),
            container,
            request: payload,
        };
        if let Some(sink) = &mut self.sink {
            let mut line = serde_json::to_vec(&entry).map_err(internal)?;
            line.push(b'\n');
            sink.write_all(&line)
                .and_then(|()| sink.flush())
                .map_err(internal)?;
        }
        let name = entry.machine.clone();
        self.ledger.push(entry);
        let calls = self.calls.entry((target, command)).or_default();
        *calls += 1;
        let calls = *calls;
        Ok(self
            .faults
            .iter_mut()
            .filter(|armed| !armed.fired)
            .find_map(|armed| {
                let action = match &armed.fault {
                    Fault::DropReply {
                        machine,
                        command: faulted,
                        nth,
                    } if *machine == name && faulted == command && nth.get() == calls => {
                        FaultAction::DropReply
                    }
                    Fault::DisconnectAfter {
                        machine,
                        command: faulted,
                    } if *machine == name && faulted == command => FaultAction::Disconnect,
                    Fault::DropReply { .. } | Fault::DisconnectAfter { .. } => return None,
                };
                armed.fired = true;
                Some(action)
            }))
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every command the fleet does not model answers Unimplemented"
    )]
    fn apply(&mut self, target: MachineId, body: RpcRequestBody) -> Result<Reply, Status> {
        let unary = |response: RpcResponse| Ok(Reply::Unary(Box::new(response)));
        match body {
            RpcRequestBody::DescribeContract(_) => {
                let machine = self.by_id(target)?;
                unary(
                    ContractDescription {
                        machine_id: machine.id,
                        protocol_major: PROTOCOL_MAJOR,
                        daemon_version: machine.version.to_string(),
                        capabilities: [
                            CapabilityAdvertisement::Always,
                            CapabilityAdvertisement::Container,
                            CapabilityAdvertisement::Ingress,
                            CapabilityAdvertisement::Cluster,
                        ]
                        .into_iter()
                        .flat_map(CapabilityAdvertisement::capabilities)
                        .collect::<BTreeSet<_>>(),
                    }
                    .into(),
                )
            }
            RpcRequestBody::Inspect(_) => {
                let machine = self.by_id(target)?;
                let record = machine.record();
                unary(
                    MachineDetails {
                        id: machine.id,
                        phase: LocalMachinePhase::Participating,
                        public_key: record.public_key,
                        machine: Some(record),
                        advertised_endpoints: Vec::new(),
                        store_version: BTreeMap::new(),
                        rtts: Vec::new(),
                        management_clients: Vec::new(),
                        telemetry: None,
                        storage: None,
                        upgrade: machine.upgrades.last().cloned(),
                    }
                    .into(),
                )
            }
            RpcRequestBody::ListMachines(_) => unary(
                MachineList {
                    enrollment: None,
                    machines: self.observations(),
                }
                .into(),
            ),
            RpcRequestBody::ListContainers(_) => {
                let machine = self.by_id(target)?;
                unary(
                    ContainerList {
                        containers: machine.container_observations()?,
                    }
                    .into(),
                )
            }
            RpcRequestBody::InspectContainer(request) => {
                let machine = self.by_id(target)?;
                let Some(container) = machine.container(&request.container_id) else {
                    return unary(not_found(&request.container_id));
                };
                unary(
                    ContainerDetails {
                        container: machine.observe(container)?,
                        environment: Some(container.spec.env.clone()),
                        image_id: None,
                        last_check: None,
                    }
                    .into(),
                )
            }
            RpcRequestBody::StopContainer(request) => {
                unary(self.set_state(target, request.container_id, RunState::Exited)?)
            }
            RpcRequestBody::StartContainer(request) => {
                unary(self.set_state(target, request.container_id, RunState::Running)?)
            }
            RpcRequestBody::InspectStorage(_) => unary(self.by_id(target)?.storage().into()),
            RpcRequestBody::ListVolumes(_) => unary(
                VolumeInventory {
                    volumes: self
                        .by_id(target)?
                        .docker_volumes()
                        .into_iter()
                        .filter(|volume| {
                            !matches!(
                                volume.storage,
                                DockerVolumeStorageObservation::Provisioned {
                                    role: Some(CopyRole::Slot),
                                    ..
                                }
                            )
                        })
                        .collect(),
                    failures: Vec::new(),
                }
                .into(),
            ),
            RpcRequestBody::InspectVolumeCopy(request) => {
                let copy = self
                    .by_id(target)?
                    .copies
                    .iter()
                    .find(|copy| copy.name == request.name)
                    .map(|copy| match copy.role {
                        CopyRole::Slot => VolumeCopy::Slot {
                            slot: SlotId::new(1),
                            readonly: true,
                            newest: None,
                            resume_token: None,
                            pull: None,
                        },
                        CopyRole::Writer | CopyRole::Switching => VolumeCopy::Root {
                            readonly: false,
                            newest: None,
                            epoch: WriterEpoch::default(),
                        },
                    });
                unary(VolumeCopyView { copy }.into())
            }
            RpcRequestBody::RequestMachineUpgrade(request) => {
                let machine = self.by_id_mut(target)?;
                if let Some(attempt) = machine
                    .upgrades
                    .iter()
                    .find(|attempt| attempt.attempt_id == request.attempt_id)
                {
                    return unary(attempt.clone().into());
                }
                let target_version = match request.release {
                    MachineRelease::Exact(version) => version,
                    MachineRelease::Stable | MachineRelease::Beta => machine.version.clone(),
                };
                let attempt = MachineUpgradeAttempt {
                    attempt_id: request.attempt_id,
                    target: target_version.clone(),
                    outcome: MachineUpgradeOutcome::Succeeded {
                        version: target_version.clone(),
                    },
                };
                machine.version = target_version;
                machine.upgrades.push(attempt.clone());
                unary(attempt.into())
            }
            RpcRequestBody::TailLogs(request) => {
                let machine = self.by_id(target)?;
                let LiveLogTarget::Container(container_id) = request.target else {
                    return Err(unimplemented("tail_logs of a Machine service"));
                };
                let Some(container) = machine.container(&container_id) else {
                    return Err(Status::not_found(format!("no Container {container_id}")));
                };
                let options = request.options;
                let until = options
                    .until_unix_seconds
                    .map(|seconds| seconds * 1_000_000_000);
                let mut lines: Vec<(i64, &String)> = timed(&container.spec.logs)
                    .filter(|(nanos, _)| options.since_nanos.is_none_or(|since| *nanos >= since))
                    .filter(|(nanos, _)| until.is_none_or(|until| *nanos <= until))
                    .collect();
                if let Ok(tail) = usize::try_from(options.tail) {
                    lines = lines.split_off(lines.len().saturating_sub(tail));
                }
                Ok(Reply::Frames {
                    frames: machine.log_frames(container, lines)?,
                    hold: options.follow,
                })
            }
            RpcRequestBody::LogHistory(request) => {
                request.validate().map_err(Status::invalid_argument)?;
                let machine = self.by_id(target)?;
                Ok(Reply::Frames {
                    frames: machine.history_frames(&request)?,
                    hold: false,
                })
            }
            RpcRequestBody::RuntimeWatch(_) => {
                let frame = self.watch_frame()?;
                Ok(Reply::Frames {
                    frames: vec![encode_runtime_watch_frame(&frame).map_err(internal)?],
                    hold: true,
                })
            }
            other => Err(unimplemented(other.command())),
        }
    }

    fn set_state(
        &mut self,
        target: MachineId,
        container_id: ContainerId,
        state: RunState,
    ) -> Result<RpcResponse, Status> {
        let machine = self.by_id_mut(target)?;
        let Some(container) = machine
            .containers
            .iter_mut()
            .find(|container| container.id == container_id)
        else {
            return Ok(not_found(&container_id));
        };
        container.spec.state = state;
        Ok(ContainerChanged { container_id }.into())
    }

    fn observations(&self) -> Vec<MachineObservation> {
        self.machines
            .iter()
            .map(|machine| {
                MachineObservation::new(
                    machine.record(),
                    if machine.online {
                        MembershipObservation::Up
                    } else {
                        MembershipObservation::Down
                    },
                )
            })
            .collect()
    }

    fn watch_frame(&self) -> Result<RuntimeWatchFrame, Status> {
        let mut containers = Vec::new();
        let mut volumes = Vec::new();
        for machine in self.machines.iter().filter(|machine| machine.online) {
            containers.extend(machine.container_observations()?);
            volumes.extend(machine.docker_volumes());
        }
        Ok(RuntimeWatchFrame {
            machines: self.observations(),
            containers,
            volumes,
            certificates: Vec::new(),
            incomplete_ids: Default::default(),
            observed_at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(internal)?,
        })
    }
}

impl MachineState {
    fn new(machine: FleetMachine) -> Result<Self, FakeFleetError> {
        let copies = machine
            .volumes
            .into_iter()
            .map(|volume| CopyState {
                name: volume.namespace.volume_name(&volume.name),
                namespace: volume.namespace,
                role: CopyRole::Writer,
            })
            .collect();
        Ok(Self {
            name: machine.name,
            id: machine.id,
            version: machine.version,
            online: machine.online,
            containers: machine
                .containers
                .into_iter()
                .map(|spec| ContainerState {
                    id: container_id(&spec.name),
                    spec,
                })
                .collect(),
            copies,
            upgrades: Vec::new(),
        })
    }

    fn record(&self) -> Machine {
        let byte = self.id.as_str().bytes().last().unwrap_or(b'0');
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: self.id,
            name: self.name.clone(),
            subnet: format!("10.210.{byte}.0/24")
                .parse()
                .expect("an octet below 256 forms a valid /24"),
            public_key: WireGuardPublicKey([byte; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: MachineRuntime {
                daemon_version: self.version.to_string(),
                ..MachineRuntime::default()
            },
            build_concurrency: None,
        }
    }

    fn container(&self, id: &ContainerId) -> Option<&ContainerState> {
        self.containers.iter().find(|container| &container.id == id)
    }

    fn container_observations(&self) -> Result<Vec<ContainerObservation>, Status> {
        self.containers
            .iter()
            .map(|container| self.observe(container))
            .collect()
    }

    fn observe(&self, container: &ContainerState) -> Result<ContainerObservation, Status> {
        let spec = &container.spec;
        let service_id = service_id(&spec.namespace, &spec.service);
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": service_id,
            "name": spec.service,
            "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": spec.image, "pull_policy": "missing"},
        }))
        .map_err(internal)?;
        let mut labels = BTreeMap::from([
            ("ployz.service.id".to_owned(), service_id.to_string()),
            ("ployz.service.name".to_owned(), spec.service.to_string()),
            (NAMESPACE_LABEL.to_owned(), spec.namespace.to_string()),
        ]);
        if let Some(deployment) = spec.deployment {
            labels.insert(DEPLOYMENT_LABEL.to_owned(), deployment.to_string());
        }
        ContainerObservation::try_from(ContainerObservationParts {
            container_id: container.id,
            display_name: spec.name.clone(),
            created_at_unix_nanos: FIRST_LOG_NANOS,
            machine_id: self.id,
            namespace: spec.namespace.clone(),
            kind: ContainerKind::ServiceContainer,
            runtime: match spec.state {
                RunState::Running => ContainerRuntimeObservation::Running {
                    health: HealthObservation::Healthy,
                },
                RunState::Exited => ContainerRuntimeObservation::Exited { code: 0 },
            },
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels,
        })
        .map_err(internal)
    }

    fn docker_volumes(&self) -> Vec<DockerVolume> {
        self.copies
            .iter()
            .map(|copy| {
                let (root, labels) = match copy.role {
                    CopyRole::Slot => (
                        format!("/var/lib/ployz-mirror/{}/fs", copy.name),
                        BTreeMap::new(),
                    ),
                    CopyRole::Writer | CopyRole::Switching => (
                        format!("/var/lib/ployz/{}", copy.name),
                        BTreeMap::from([
                            (MANAGED_LABEL.to_owned(), String::new()),
                            (NAMESPACE_LABEL.to_owned(), copy.namespace.to_string()),
                        ]),
                    ),
                };
                DockerVolume {
                    id: DockerVolumeId {
                        machine_id: self.id,
                        name: copy.name.clone(),
                    },
                    options: BTreeMap::new(),
                    labels,
                    storage: DockerVolumeStorageObservation::Provisioned {
                        mountpoint: MachinePath::parse(root)
                            .expect("a fixture mountpoint is an absolute path"),
                        bound_bytes: VOLUME_BYTES,
                        used_bytes: USED_BYTES,
                        role: Some(copy.role),
                    },
                }
            })
            .collect()
    }

    fn storage(&self) -> StorageCapacity {
        let maximum = ProvisionedVolumeMaximumBytes::new(VOLUME_BYTES);
        StorageCapacity {
            backing: StorageBacking::Fixed {
                pool_size_bytes: POOL_BYTES,
            },
            unmanaged_used_bytes: 0,
            volumes: self
                .copies
                .iter()
                .filter(|copy| copy.role != CopyRole::Slot)
                .map(|copy| (copy.name.clone(), maximum))
                .collect(),
            copies: self
                .copies
                .iter()
                .map(|copy| {
                    (
                        copy.name.clone(),
                        ProvisionedCopy {
                            role: copy.role,
                            maximum_bytes: maximum,
                            used_bytes: USED_BYTES,
                        },
                    )
                })
                .collect(),
        }
    }

    fn log_frames(
        &self,
        container: &ContainerState,
        lines: Vec<(i64, &String)>,
    ) -> Result<Vec<OpaquePayload>, Status> {
        let metadata = LogMetadata {
            origin: LogOrigin::Service {
                service_id: service_id(&container.spec.namespace, &container.spec.service),
                service_name: container.spec.service.clone(),
                container_id: container.id,
                hook: None,
            },
            machine_id: self.id,
            machine_name: self.name.clone(),
        };
        lines
            .into_iter()
            .map(|(nanos, line)| {
                LogEntry {
                    metadata: metadata.clone(),
                    timestamp_unix_nanos: nanos,
                    body: LogBody::Stdout(line.clone().into_bytes()),
                }
                .encode()
                .map_err(internal)
            })
            .collect()
    }
    fn history_frames(&self, request: &LogHistoryRequest) -> Result<Vec<OpaquePayload>, Status> {
        let containers: Vec<&ContainerState> = self
            .containers
            .iter()
            .filter(|container| {
                request.container_id.is_none_or(|id| id == container.id)
                    && request
                        .namespace
                        .as_deref()
                        .is_none_or(|namespace| namespace == container.spec.namespace.as_str())
                    && request
                        .service
                        .as_deref()
                        .is_none_or(|service| service == container.spec.service.as_str())
                    && request.deployment.as_deref().is_none_or(|deployment| {
                        container.spec.deployment.map(|d| d.to_string()).as_deref()
                            == Some(deployment)
                    })
            })
            .collect();
        let mut lines: Vec<(i64, &ContainerState, &String)> = containers
            .iter()
            .flat_map(|container| {
                timed(&container.spec.logs).map(move |(nanos, line)| (nanos, *container, line))
            })
            .filter(|(nanos, _, _)| request.since_nanos.is_none_or(|since| *nanos >= since))
            .filter(|(nanos, _, _)| request.until_nanos.is_none_or(|until| *nanos < until))
            .collect();
        lines.sort_by_key(|(nanos, _, _)| *nanos);
        let limit = usize::from(request.limit);
        if request.direction == LogDirection::Backward {
            lines = lines.split_off(lines.len().saturating_sub(limit));
        } else {
            lines.truncate(limit);
        }
        let mut rows = Vec::new();
        let mut announced = Vec::new();
        for (nanos, container, line) in lines {
            if !announced.contains(&container.id) {
                announced.push(container.id);
                rows.push(HistoryRow::Container(HistoryContainer {
                    container_id: container.id,
                    namespace: Some(container.spec.namespace.to_string()),
                    service: Some(container.spec.service.to_string()),
                    deployment: container.spec.deployment.map(|d| d.to_string()),
                    replica: container.spec.name.clone(),
                    kind: HistoryContainerKind::Service,
                }));
            }
            rows.push(HistoryRow::Line {
                container_id: container.id,
                ts: nanos,
                stream: HistoryStream::Stdout,
                text: line.clone().into_bytes(),
            });
        }
        rows.push(HistoryRow::End { next: None });
        rows.iter()
            .map(|row| row.encode().map_err(internal))
            .collect()
    }
}

/// Each fixture line with its timestamp, one second apart.
fn timed(lines: &[String]) -> impl Iterator<Item = (i64, &String)> {
    (0_i64..)
        .zip(lines)
        .map(|(index, line)| (FIRST_LOG_NANOS + index * LOG_STEP_NANOS, line))
}

fn find_mut<'a>(
    machines: &'a mut [MachineState],
    name: &MachineName,
) -> Result<&'a mut MachineState, FakeFleetError> {
    machines
        .iter_mut()
        .find(|machine| &machine.name == name)
        .ok_or_else(|| FakeFleetError::UnknownMachine(name.clone()))
}

fn physical(place: &VolumePlace) -> DockerVolumeName {
    place.namespace.volume_name(&place.volume)
}

fn not_found(container_id: &ContainerId) -> RpcResponse {
    RpcError {
        code: RpcErrorCode::NotFound,
        message: format!("no Container {container_id}"),
        details: Value::Null,
        cause: Vec::new(),
    }
    .into()
}

fn unimplemented(command: &str) -> Status {
    Status::unimplemented(format!("the fake fleet does not implement {command}"))
}

fn internal(error: impl std::fmt::Display) -> Status {
    Status::internal(error.to_string())
}

#[derive(Clone)]
struct FleetService {
    shared: Arc<Shared>,
    /// The Machine whose socket this connection arrived on.
    machine: MachineId,
}

impl FleetService {
    fn target(&self, metadata: &tonic::metadata::MetadataMap) -> Result<MachineId, Status> {
        let routing = ployz_core::routing_from_metadata(metadata)
            .map_err(|error| Status::invalid_argument(error.to_string()))?;
        self.shared
            .lock()
            .map_err(internal)?
            .route(routing, self.machine)
    }

    fn decode(
        &self,
        request: Request<OpaquePayload>,
    ) -> Result<(MachineId, RpcRequestBody), Status> {
        let target = self.target(request.metadata())?;
        let body = request
            .into_inner()
            .decode_request()
            .map_err(|error| Status::invalid_argument(error.to_string()))?
            .body;
        Ok((target, body))
    }

    fn unary(&self, request: Request<OpaquePayload>) -> Result<Response<OpaquePayload>, Status> {
        let (target, body) = self.decode(request)?;
        match self.shared.handle(target, Received::Body(Box::new(body)))? {
            Reply::Unary(response) => Ok(Response::new(response.encode().map_err(internal)?)),
            Reply::Frames { .. } => Err(Status::internal("a unary command produced a stream")),
        }
    }

    fn stream(&self, request: Request<OpaquePayload>) -> Result<Response<FrameStream>, Status> {
        let (target, body) = self.decode(request)?;
        match self.shared.handle(target, Received::Body(Box::new(body)))? {
            Reply::Frames { frames, hold } => Ok(Response::new(frame_stream(frames, hold))),
            Reply::Unary(_) => Err(Status::internal("a streaming command produced one reply")),
        }
    }

    fn bare<T>(
        &self,
        metadata: &tonic::metadata::MetadataMap,
        command: &'static str,
    ) -> Result<T, Status> {
        let target = self.target(metadata)?;
        self.shared.handle(target, Received::Bare(command))?;
        Err(unimplemented(command))
    }
}

type FrameStream = ReceiverStream<Result<OpaquePayload, Status>>;

/// The frames, then end of stream unless `hold` keeps it open until the client leaves.
fn frame_stream(frames: Vec<OpaquePayload>, hold: bool) -> FrameStream {
    let (sender, receiver) = mpsc::channel(frames.len().max(1));
    for frame in frames {
        let _ = sender.try_send(Ok(frame));
    }
    if hold {
        tokio::spawn(async move { sender.closed().await });
    }
    ReceiverStream::new(receiver)
}

/// Builds the service impl and the `COMMANDS` table from the catalog: every unary RPC
/// forwards to [`FleetService::unary`], every server stream to [`FleetService::stream`].
macro_rules! fleet_service {
    (
        package $package:literal
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident),)+ }
    ) => {
        /// Every command a fault may name.
        const COMMANDS: &[&str] = &[$($unary_command,)+ $($stream_command,)+ "exec", "build"];

        #[tonic::async_trait]
        impl MachineRpc for FleetService {
            type ExecStream = FrameStream;
            type BuildStream = FrameStream;
            type LogHistoryStream = FrameStream;
            type TailLogsStream = FrameStream;
            type RuntimeWatchStream = FrameStream;

            $(
                async fn $unary_method(
                    &self,
                    request: Request<OpaquePayload>,
                ) -> Result<Response<OpaquePayload>, Status> {
                    self.unary(request)
                }
            )+

            $(
                async fn $stream_method(
                    &self,
                    request: Request<OpaquePayload>,
                ) -> Result<Response<FrameStream>, Status> {
                    self.stream(request)
                }
            )+

            async fn exec(
                &self,
                request: Request<Streaming<OpaquePayload>>,
            ) -> Result<Response<FrameStream>, Status> {
                self.bare(request.metadata(), "exec")
            }

            async fn build(
                &self,
                request: Request<Streaming<OpaquePayload>>,
            ) -> Result<Response<FrameStream>, Status> {
                self.bare(request.metadata(), "build")
            }
        }
    };
}

ployz_core::rpc_catalog!(fleet_service);
