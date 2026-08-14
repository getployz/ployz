use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};

use ipnet::Ipv4Net;
use prost::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{
    AdvertisedEndpoint, CapabilityName, ContainerId, ContainerKind, ContainerObservation,
    DockerVolume, DockerVolumeName, LocalMachinePhase, Machine, MachineId, MachineLogService,
    MachineName, MachineObservation, ResolvedServiceSpec, WireGuardPublicKey,
};

mod docker;
mod fanout;

pub use docker::*;
pub use fanout::*;

pub const PROTOCOL_MAJOR: u32 = 1;
pub const DESCRIBE_CONTRACT_CAPABILITY: &str = "ployz.rpc.describe-contract.v1";
pub const RESET_MACHINE_CAPABILITY: &str = "ployz.machine.reset.v1";
pub const INSPECT_MACHINE_CAPABILITY: &str = "ployz.machine.inspect.v1";
pub const INITIALIZE_MACHINE_CAPABILITY: &str = "ployz.machine.initialize.v1";
pub const REGISTER_MACHINE_CAPABILITY: &str = "ployz.machine.register.v1";
pub const JOIN_MACHINE_CAPABILITY: &str = "ployz.machine.join.v1";
pub const LIST_MACHINES_CAPABILITY: &str = "ployz.machine.list.v1";
pub const LIST_CONTAINERS_CAPABILITY: &str = "ployz.container.list.v1";
pub const INSPECT_CONTAINER_CAPABILITY: &str = "ployz.container.inspect.v1";
pub const CREATE_CONTAINER_CAPABILITY: &str = "ployz.container.create.v1";
pub const START_CONTAINER_CAPABILITY: &str = "ployz.container.start.v1";
pub const STOP_CONTAINER_CAPABILITY: &str = "ployz.container.stop.v1";
pub const REMOVE_CONTAINER_CAPABILITY: &str = "ployz.container.remove.v1";
pub const EXEC_CONTAINER_CAPABILITY: &str = "ployz.container.exec.v1";
pub const CONTAINER_LOGS_CAPABILITY: &str = "ployz.container.logs.v1";
pub const MACHINE_LOGS_CAPABILITY: &str = "ployz.machine.logs.v1";
pub const LIST_IMAGES_CAPABILITY: &str = "ployz.image.list.v1";
pub const UNREGISTRY_PORT: u16 = 51500;

/// The only protobuf-shaped value understood by tonic and the transparent proxy.
#[derive(Clone, PartialEq, Message)]
pub struct OpaquePayload {
    #[prost(bytes = "vec", tag = "1")]
    pub json: Vec<u8>,
}

/// Generated tonic client and server for the shared Machine service.
pub mod transport {
    include!(concat!(env!("OUT_DIR"), "/ployz.rpc.v1.MachineRpc.rs"));
}

pub use transport::{
    machine_rpc_client::MachineRpcClient, machine_rpc_server::MachineRpc,
    machine_rpc_server::MachineRpcServer,
};

impl OpaquePayload {
    #[must_use]
    pub fn new(json: Vec<u8>) -> Self {
        Self { json }
    }

    pub fn from_json<T: Serialize>(value: &T) -> Result<Self, CodecError> {
        serde_json::to_vec(value)
            .map(Self::new)
            .map_err(CodecError::EncodeJson)
    }

    pub fn decode_json<T: DeserializeOwned>(&self) -> Result<T, CodecError> {
        serde_json::from_slice(&self.json).map_err(CodecError::DecodeJson)
    }

    pub fn decode_request(&self) -> Result<RpcRequest, CodecError> {
        let header: RequestHeader = self.decode_json()?;
        validate_protocol_major(header.protocol_major)?;
        if !matches!(
            header.command.as_str(),
            "describe_contract"
                | "inspect"
                | "initialize"
                | "register"
                | "join"
                | "list_machines"
                | "list_containers"
                | "inspect_container"
                | "create_container"
                | "start_container"
                | "stop_container"
                | "remove_container"
                | "create_volume"
                | "list_volumes"
                | "inspect_volume"
                | "remove_volume"
                | "container_logs"
                | "machine_logs"
                | "list_images"
                | "reset"
        ) {
            return Err(CodecError::UnsupportedCommand(header.command));
        }
        self.decode_json()
    }

    pub fn decode_response(&self) -> Result<RpcResponse, CodecError> {
        let response: RpcResponse = self.decode_json()?;
        validate_protocol_major(response.protocol_major)?;
        Ok(response)
    }
}

fn validate_protocol_major(requested: u32) -> Result<(), CodecError> {
    if requested == PROTOCOL_MAJOR {
        Ok(())
    } else {
        Err(CodecError::UnsupportedProtocolMajor {
            requested,
            supported: PROTOCOL_MAJOR,
        })
    }
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("could not encode JSON payload: {0}")]
    EncodeJson(serde_json::Error),
    #[error("could not decode JSON payload: {0}")]
    DecodeJson(serde_json::Error),
    #[error("unsupported RPC command {0:?}")]
    UnsupportedCommand(String),
    #[error("unsupported protocol major {requested}; this endpoint supports {supported}")]
    UnsupportedProtocolMajor { requested: u32, supported: u32 },
    #[error("expected response kind {expected:?}, received {actual:?}")]
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },
}

/// The empty payload of the capability-description command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DescribeContractRequest {}

/// The empty payload of the reset command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResetRequest {}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectRequest {
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub public_ip_override: Option<IpAddr>,
    #[serde(default = "default_wireguard_port")]
    pub wireguard_port: u16,
}

fn default_wireguard_port() -> u16 {
    51820
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InitializeRequest {
    pub name: MachineName,
    pub cluster_network: Ipv4Net,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub wireguard_mtu: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub name: MachineName,
    pub public_key: WireGuardPublicKey,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub registration: Registered,
    #[serde(default)]
    pub wireguard_mtu: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListMachinesRequest {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListContainersRequest {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectContainerRequest {
    pub container_id: ContainerId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateContainerRequest {
    pub kind: ContainerKind,
    pub resolved_spec: ResolvedServiceSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StartContainerRequest {
    pub container_id: ContainerId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StopContainerRequest {
    pub container_id: ContainerId,
    #[serde(default)]
    pub signal: Option<String>,
    #[serde(default)]
    pub grace_period_seconds: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveContainerRequest {
    pub container_id: ContainerId,
    #[serde(default)]
    pub remove_volumes: bool,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogsOptions {
    pub follow: bool,
    pub tail: i32,
    #[serde(default)]
    pub since: String,
    #[serde(default)]
    pub until: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerLogsRequest {
    pub container_id: ContainerId,
    pub options: LogsOptions,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineLogsRequest {
    pub service: MachineLogService,
    pub options: LogsOptions,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListImagesRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// Commands are closed and own their typed payloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "command", content = "payload")]
pub enum RpcRequestBody {
    DescribeContract(DescribeContractRequest),
    Inspect(InspectRequest),
    Initialize(InitializeRequest),
    Register(RegisterRequest),
    Join(JoinRequest),
    ListMachines(ListMachinesRequest),
    ListContainers(ListContainersRequest),
    InspectContainer(InspectContainerRequest),
    CreateContainer(Box<CreateContainerRequest>),
    StartContainer(StartContainerRequest),
    StopContainer(StopContainerRequest),
    RemoveContainer(RemoveContainerRequest),
    CreateVolume(CreateVolumeRequest),
    ListVolumes(ListVolumesRequest),
    InspectVolume(InspectVolumeRequest),
    RemoveVolume(RemoveVolumeRequest),
    ContainerLogs(ContainerLogsRequest),
    MachineLogs(MachineLogsRequest),
    ListImages(ListImagesRequest),
    Reset(ResetRequest),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub protocol_major: u32,
    #[serde(flatten)]
    pub body: RpcRequestBody,
}

impl RpcRequest {
    #[must_use]
    pub fn describe_contract() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::DescribeContract(DescribeContractRequest {}),
        }
    }

    #[must_use]
    pub fn reset() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::Reset(ResetRequest {}),
        }
    }

    #[must_use]
    pub fn inspect(request: InspectRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::Inspect(request),
        }
    }

    #[must_use]
    pub fn initialize(request: InitializeRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::Initialize(request),
        }
    }

    #[must_use]
    pub fn register(request: RegisterRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::Register(request),
        }
    }

    #[must_use]
    pub fn join(request: JoinRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::Join(request),
        }
    }

    #[must_use]
    pub fn list_machines() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::ListMachines(ListMachinesRequest {}),
        }
    }

    #[must_use]
    pub fn list_containers() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::ListContainers(ListContainersRequest {}),
        }
    }

    #[must_use]
    pub fn create_volume(request: CreateVolumeRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::CreateVolume(request),
        }
    }

    #[must_use]
    pub fn inspect_container(request: InspectContainerRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::InspectContainer(request),
        }
    }

    #[must_use]
    pub fn list_volumes() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::ListVolumes(ListVolumesRequest {}),
        }
    }

    #[must_use]
    pub fn create_container(request: CreateContainerRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::CreateContainer(Box::new(request)),
        }
    }

    #[must_use]
    pub fn inspect_volume(name: DockerVolumeName) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::InspectVolume(InspectVolumeRequest { name }),
        }
    }

    #[must_use]
    pub fn start_container(request: StartContainerRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::StartContainer(request),
        }
    }

    #[must_use]
    pub fn remove_volume(name: DockerVolumeName, force: bool) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::RemoveVolume(RemoveVolumeRequest { name, force }),
        }
    }

    #[must_use]
    pub fn stop_container(request: StopContainerRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::StopContainer(request),
        }
    }

    #[must_use]
    pub fn remove_container(request: RemoveContainerRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::RemoveContainer(request),
        }
    }

    #[must_use]
    pub fn container_logs(request: ContainerLogsRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::ContainerLogs(request),
        }
    }

    #[must_use]
    pub fn machine_logs(request: MachineLogsRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::MachineLogs(request),
        }
    }

    #[must_use]
    pub fn list_images(reference: Option<String>) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::ListImages(ListImagesRequest { reference }),
        }
    }

    pub fn encode(&self) -> Result<OpaquePayload, CodecError> {
        OpaquePayload::from_json(self)
    }
}

#[derive(Deserialize)]
struct RequestHeader {
    protocol_major: u32,
    command: String,
}

crate::value::open_string_enum!(ResponseKind, Unknown {
    ContractDescription => "contract_description",
    MachineDetails => "machine_details",
    Initialized => "initialized",
    Registered => "registered",
    JoinAccepted => "join_accepted",
    MachineList => "machine_list",
    ContainerList => "container_list",
    ContainerDetails => "container_details",
    ContainerCreated => "container_created",
    ContainerChanged => "container_changed",
    VolumeCreated => "volume_created",
    VolumeList => "volume_list",
    VolumeDetails => "volume_details",
    VolumeRemoved => "volume_removed",
    MachineImages => "machine_images",
    ResetAccepted => "reset_accepted",
    Error => "error",
});

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResetAccepted {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineDetails {
    pub id: MachineId,
    pub phase: LocalMachinePhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<Machine>,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub store_version: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Initialized {
    pub machine: Machine,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Registered {
    pub assigned_machine: Machine,
    pub visible_peers: Vec<Machine>,
    pub target_versions: BTreeMap<String, i64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct JoinAccepted {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineList {
    pub machines: Vec<MachineObservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerList {
    pub containers: Vec<ContainerObservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerDetails {
    pub container: ContainerObservation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerCreated {
    pub container_id: ContainerId,
    pub display_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerChanged {
    pub container_id: ContainerId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageSummary {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub created: i64,
    pub size: i64,
    pub containers: i64,
    pub platforms: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineImages {
    pub containerd_store: bool,
    pub images: Vec<ImageSummary>,
}

/// Known responses own typed payloads; future responses retain their raw value.
#[derive(Clone, Debug, PartialEq)]
pub enum RpcResponseBody {
    ContractDescription(ContractDescription),
    MachineDetails(MachineDetails),
    Initialized(Initialized),
    Registered(Registered),
    JoinAccepted(JoinAccepted),
    MachineList(MachineList),
    ContainerList(ContainerList),
    ContainerDetails(Box<ContainerDetails>),
    ContainerCreated(ContainerCreated),
    ContainerChanged(ContainerChanged),
    VolumeCreated(VolumeCreated),
    VolumeList(VolumeList),
    VolumeDetails(VolumeDetails),
    VolumeRemoved(VolumeRemoved),
    MachineImages(MachineImages),
    ResetAccepted(ResetAccepted),
    Error(RpcError),
    Unknown { kind: String, payload: Value },
}

impl RpcResponseBody {
    #[must_use]
    pub fn kind(&self) -> ResponseKind {
        match self {
            Self::ContractDescription(_) => ResponseKind::ContractDescription,
            Self::MachineDetails(_) => ResponseKind::MachineDetails,
            Self::Initialized(_) => ResponseKind::Initialized,
            Self::Registered(_) => ResponseKind::Registered,
            Self::JoinAccepted(_) => ResponseKind::JoinAccepted,
            Self::MachineList(_) => ResponseKind::MachineList,
            Self::ContainerList(_) => ResponseKind::ContainerList,
            Self::ContainerDetails(_) => ResponseKind::ContainerDetails,
            Self::ContainerCreated(_) => ResponseKind::ContainerCreated,
            Self::ContainerChanged(_) => ResponseKind::ContainerChanged,
            Self::VolumeCreated(_) => ResponseKind::VolumeCreated,
            Self::VolumeList(_) => ResponseKind::VolumeList,
            Self::VolumeDetails(_) => ResponseKind::VolumeDetails,
            Self::VolumeRemoved(_) => ResponseKind::VolumeRemoved,
            Self::MachineImages(_) => ResponseKind::MachineImages,
            Self::ResetAccepted(_) => ResponseKind::ResetAccepted,
            Self::Error(_) => ResponseKind::Error,
            Self::Unknown { kind, .. } => ResponseKind::Unknown(kind.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RpcResponse {
    pub protocol_major: u32,
    pub body: RpcResponseBody,
}

impl RpcResponse {
    #[must_use]
    pub fn contract_description(description: ContractDescription) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ContractDescription(description),
        }
    }

    #[must_use]
    pub fn error(error: RpcError) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::Error(error),
        }
    }

    #[must_use]
    pub fn reset_accepted() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ResetAccepted(ResetAccepted {}),
        }
    }

    #[must_use]
    pub fn machine_details(details: MachineDetails) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::MachineDetails(details),
        }
    }

    #[must_use]
    pub fn initialized(machine: Machine) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::Initialized(Initialized { machine }),
        }
    }

    #[must_use]
    pub fn registered(registered: Registered) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::Registered(registered),
        }
    }

    #[must_use]
    pub fn join_accepted() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::JoinAccepted(JoinAccepted {}),
        }
    }

    #[must_use]
    pub fn machine_list(machines: Vec<MachineObservation>) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::MachineList(MachineList { machines }),
        }
    }

    #[must_use]
    pub fn container_list(containers: Vec<ContainerObservation>) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ContainerList(ContainerList { containers }),
        }
    }

    #[must_use]
    pub fn volume_created(volume: DockerVolume) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::VolumeCreated(VolumeCreated { volume }),
        }
    }

    #[must_use]
    pub fn container_details(container: ContainerObservation) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ContainerDetails(Box::new(ContainerDetails { container })),
        }
    }

    #[must_use]
    pub fn volume_list(volumes: Vec<DockerVolume>) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::VolumeList(VolumeList { volumes }),
        }
    }

    #[must_use]
    pub fn container_created(created: ContainerCreated) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ContainerCreated(created),
        }
    }

    #[must_use]
    pub fn volume_details(volume: DockerVolume) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::VolumeDetails(VolumeDetails { volume }),
        }
    }

    #[must_use]
    pub fn container_changed(container_id: ContainerId) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::ContainerChanged(ContainerChanged { container_id }),
        }
    }

    #[must_use]
    pub fn volume_removed() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::VolumeRemoved(VolumeRemoved {}),
        }
    }

    #[must_use]
    pub fn machine_images(images: MachineImages) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::MachineImages(images),
        }
    }

    #[must_use]
    pub fn kind(&self) -> ResponseKind {
        self.body.kind()
    }

    pub fn decode_contract_description(&self) -> Result<&ContractDescription, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::ContractDescription(description) = &self.body {
            Ok(description)
        } else {
            Err(self.unexpected("contract_description"))
        }
    }

    pub fn decode_machine_details(&self) -> Result<&MachineDetails, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::MachineDetails(details) = &self.body {
            Ok(details)
        } else {
            Err(self.unexpected("machine_details"))
        }
    }

    pub fn decode_initialized(&self) -> Result<&Machine, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::Initialized(initialized) = &self.body {
            Ok(&initialized.machine)
        } else {
            Err(self.unexpected("initialized"))
        }
    }

    pub fn decode_registered(&self) -> Result<&Registered, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::Registered(registered) = &self.body {
            Ok(registered)
        } else {
            Err(self.unexpected("registered"))
        }
    }

    pub fn decode_join_accepted(&self) -> Result<(), CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::JoinAccepted(_) = &self.body {
            Ok(())
        } else {
            Err(self.unexpected("join_accepted"))
        }
    }

    pub fn decode_machine_list(&self) -> Result<&[MachineObservation], CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::MachineList(list) = &self.body {
            Ok(&list.machines)
        } else {
            Err(self.unexpected("machine_list"))
        }
    }

    pub fn decode_container_list(&self) -> Result<&[ContainerObservation], CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::ContainerList(list) = &self.body {
            Ok(&list.containers)
        } else {
            Err(self.unexpected("container_list"))
        }
    }

    pub fn decode_container_details(&self) -> Result<&ContainerObservation, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::ContainerDetails(details) = &self.body {
            Ok(&details.container)
        } else {
            Err(self.unexpected("container_details"))
        }
    }

    pub fn decode_container_created(&self) -> Result<&ContainerCreated, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::ContainerCreated(created) = &self.body {
            Ok(created)
        } else {
            Err(self.unexpected("container_created"))
        }
    }

    pub fn decode_container_changed(&self) -> Result<&ContainerId, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::ContainerChanged(changed) = &self.body {
            Ok(&changed.container_id)
        } else {
            Err(self.unexpected("container_changed"))
        }
    }

    pub fn decode_volume_created(&self) -> Result<&DockerVolume, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::VolumeCreated(created) = &self.body {
            Ok(&created.volume)
        } else {
            Err(self.unexpected("volume_created"))
        }
    }

    pub fn decode_volume_list(&self) -> Result<&[DockerVolume], CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::VolumeList(list) = &self.body {
            Ok(&list.volumes)
        } else {
            Err(self.unexpected("volume_list"))
        }
    }

    pub fn decode_volume_details(&self) -> Result<&DockerVolume, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::VolumeDetails(details) = &self.body {
            Ok(&details.volume)
        } else {
            Err(self.unexpected("volume_details"))
        }
    }

    pub fn decode_volume_removed(&self) -> Result<(), CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::VolumeRemoved(_) = &self.body {
            Ok(())
        } else {
            Err(self.unexpected("volume_removed"))
        }
    }

    pub fn decode_machine_images(&self) -> Result<&MachineImages, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::MachineImages(images) = &self.body {
            Ok(images)
        } else {
            Err(self.unexpected("machine_images"))
        }
    }

    pub fn decode_reset_accepted(&self) -> Result<(), CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::ResetAccepted(_) = &self.body {
            Ok(())
        } else {
            Err(self.unexpected("reset_accepted"))
        }
    }

    fn unexpected(&self, expected: &'static str) -> CodecError {
        CodecError::UnexpectedResponse {
            expected,
            actual: self.kind().as_str().to_owned(),
        }
    }

    pub fn encode(&self) -> Result<OpaquePayload, CodecError> {
        OpaquePayload::from_json(self)
    }
}

#[derive(Serialize, Deserialize)]
struct WireResponse {
    protocol_major: u32,
    kind: ResponseKind,
    #[serde(default)]
    payload: Value,
}

impl Serialize for RpcResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let payload = match &self.body {
            RpcResponseBody::ContractDescription(description) => {
                serde_json::to_value(description).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::MachineDetails(details) => {
                serde_json::to_value(details).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::Initialized(initialized) => {
                serde_json::to_value(initialized).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::Registered(registered) => {
                serde_json::to_value(registered).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::JoinAccepted(accepted) => {
                serde_json::to_value(accepted).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::MachineList(list) => {
                serde_json::to_value(list).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::ContainerList(list) => {
                serde_json::to_value(list).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::ContainerDetails(details) => {
                serde_json::to_value(details).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::ContainerCreated(created) => {
                serde_json::to_value(created).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::ContainerChanged(changed) => {
                serde_json::to_value(changed).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::VolumeCreated(created) => {
                serde_json::to_value(created).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::VolumeList(list) => {
                serde_json::to_value(list).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::VolumeDetails(details) => {
                serde_json::to_value(details).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::VolumeRemoved(removed) => {
                serde_json::to_value(removed).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::MachineImages(images) => {
                serde_json::to_value(images).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::ResetAccepted(accepted) => {
                serde_json::to_value(accepted).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::Error(error) => {
                serde_json::to_value(error).map_err(serde::ser::Error::custom)?
            }
            RpcResponseBody::Unknown { payload, .. } => payload.clone(),
        };
        WireResponse {
            protocol_major: self.protocol_major,
            kind: self.kind(),
            payload,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WireResponse::deserialize(deserializer)?;
        let body = match wire.kind {
            ResponseKind::ContractDescription => RpcResponseBody::ContractDescription(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::MachineDetails => RpcResponseBody::MachineDetails(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::Initialized => RpcResponseBody::Initialized(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::Registered => RpcResponseBody::Registered(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::JoinAccepted => RpcResponseBody::JoinAccepted(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::MachineList => RpcResponseBody::MachineList(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::ContainerList => RpcResponseBody::ContainerList(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::ContainerDetails => RpcResponseBody::ContainerDetails(Box::new(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            )),
            ResponseKind::ContainerCreated => RpcResponseBody::ContainerCreated(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::ContainerChanged => RpcResponseBody::ContainerChanged(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::VolumeCreated => RpcResponseBody::VolumeCreated(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::VolumeList => RpcResponseBody::VolumeList(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::VolumeDetails => RpcResponseBody::VolumeDetails(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::VolumeRemoved => RpcResponseBody::VolumeRemoved(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::MachineImages => RpcResponseBody::MachineImages(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::ResetAccepted => RpcResponseBody::ResetAccepted(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::Error => RpcResponseBody::Error(
                serde_json::from_value(wire.payload).map_err(serde::de::Error::custom)?,
            ),
            ResponseKind::Unknown(kind) => RpcResponseBody::Unknown {
                kind,
                payload: wire.payload,
            },
        };
        Ok(Self {
            protocol_major: wire.protocol_major,
            body,
        })
    }
}

/// The capabilities currently advertised by one Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractDescription {
    pub machine_id: MachineId,
    pub protocol_major: u32,
    /// Diagnostic only. Callers select behavior using capability names.
    pub daemon_version: String,
    #[serde(default)]
    pub capabilities: BTreeSet<CapabilityName>,
}

impl ContractDescription {
    #[must_use]
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities
            .iter()
            .any(|advertised| advertised.as_str() == capability)
    }
}

crate::value::open_string_enum!(RpcErrorCode, Unknown {
    InvalidArgument => "invalid_argument",
    NotFound => "not_found",
    Ambiguous => "ambiguous",
    Unsupported => "unsupported",
    Unavailable => "unavailable",
    Conflict => "conflict",
    Internal => "internal",
});

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}
