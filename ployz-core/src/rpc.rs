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
    MachineName, MachineObservation, MachineRuntime, MachineToken, MachineUpdate,
    PublicIpDiscovery, ResolvedServiceSpec, RttObservation, WireGuardDevice, WireGuardPublicKey,
    framing::{FramingError, grpc_frame_payload},
};

mod docker;

pub use docker::*;

pub const PROTOCOL_MAJOR: u32 = 1;
pub const DESCRIBE_CONTRACT_CAPABILITY: &str = "ployz.rpc.describe-contract.v1";
pub const RESET_MACHINE_CAPABILITY: &str = "ployz.machine.reset.v1";
pub const INSPECT_MACHINE_CAPABILITY: &str = "ployz.machine.inspect.v1";
pub const MACHINE_TOKEN_CAPABILITY: &str = "ployz.machine.token.v1";
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
pub const UPDATE_MACHINE_CAPABILITY: &str = "ployz.machine.update.v1";
pub const REMOVE_LOCAL_MACHINE_CAPABILITY: &str = "ployz.machine.remove-local.v1";
pub const REMOVE_MACHINE_CAPABILITY: &str = "ployz.machine.remove.v1";
pub const INSPECT_WIREGUARD_CAPABILITY: &str = "ployz.wireguard.inspect.v1";
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

    pub fn decode_grpc_frame(frame: &[u8]) -> Result<Self, FramingError> {
        Self::decode(grpc_frame_payload(frame)?)
            .map_err(|error| FramingError::InvalidEnvelope(error.to_string()))
    }

    pub fn decode_request(&self) -> Result<RpcRequest, CodecError> {
        let header: RequestHeader = self.decode_json()?;
        validate_protocol_major(header.protocol_major)?;
        if !RPC_COMMANDS.contains(&header.command.as_str()) {
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectRequest {
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub public_ip_override: Option<IpAddr>,
    #[serde(default = "default_wireguard_port")]
    pub wireguard_port: u16,
    #[serde(default)]
    pub include_rtts: bool,
}

impl Default for InspectRequest {
    fn default() -> Self {
        Self {
            advertised_endpoints: Vec::new(),
            public_ip_override: None,
            wireguard_port: default_wireguard_port(),
            include_rtts: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineTokenRequest {
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub public_ip: PublicIpDiscovery,
    #[serde(default = "default_wireguard_port")]
    pub wireguard_port: u16,
}

impl Default for MachineTokenRequest {
    fn default() -> Self {
        Self {
            advertised_endpoints: Vec::new(),
            public_ip: PublicIpDiscovery::Auto,
            wireguard_port: default_wireguard_port(),
        }
    }
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
    #[serde(default)]
    pub public_ip: Option<IpAddr>,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub runtime: MachineRuntime,
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
pub struct UpdateMachineRequest {
    pub update: MachineUpdate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveLocalMachineRequest {
    #[serde(default)]
    pub restart_on_cleanup_failure: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoveMachineRequest {
    pub machine_id: MachineId,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectWireGuardRequest {}

macro_rules! define_request_body {
    (
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal),)+ }
    ) => {
        /// Commands are closed and own their typed payloads.
        #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case", tag = "command", content = "payload")]
        pub enum RpcRequestBody {
            $($unary_variant($unary_request),)+
            $($stream_variant($stream_request),)+
        }

        const RPC_COMMANDS: &[&str] = &[$($unary_command,)+ $($stream_command,)+];
    };
}

crate::rpc_catalog!(define_request_body);

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
    pub fn machine_token(request: MachineTokenRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::MachineToken(request),
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
    pub fn update_machine(update: MachineUpdate) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::UpdateMachine(UpdateMachineRequest { update }),
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
    pub fn remove_local_machine(request: RemoveLocalMachineRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::RemoveLocalMachine(request),
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
    pub fn remove_machine(request: RemoveMachineRequest) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::RemoveMachine(request),
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
    pub fn remove_volume(name: DockerVolumeName, force: bool) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::RemoveVolume(RemoveVolumeRequest { name, force }),
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

    #[must_use]
    pub fn inspect_wireguard() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcRequestBody::InspectWireguard(InspectWireGuardRequest {}),
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
    MachineToken => "machine_token",
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
    MachineUpdated => "machine_updated",
    LocalMachineRemoved => "local_machine_removed",
    MachineRemoved => "machine_removed",
    WireGuardInspected => "wireguard_inspected",
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
    #[serde(default)]
    pub rtts: Vec<RttObservation>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineUpdated {
    pub machine: Machine,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalMachineRemoved {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_warning: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineRemoved {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WireGuardInspected {
    pub device: WireGuardDevice,
}

macro_rules! define_response_body {
    ($($variant:ident($payload:ty) => $kind:ident,)+) => {
        /// Known responses own typed payloads; future responses retain their raw value.
        #[derive(Clone, Debug, PartialEq)]
        pub enum RpcResponseBody {
            $($variant($payload),)+
            Unknown { kind: String, payload: Value },
        }

        impl RpcResponseBody {
            #[must_use]
            pub fn kind(&self) -> ResponseKind {
                match self {
                    $(Self::$variant(_) => ResponseKind::$kind,)+
                    Self::Unknown { kind, .. } => ResponseKind::Unknown(kind.clone()),
                }
            }

            fn encode_payload(&self) -> Result<Value, serde_json::Error> {
                match self {
                    $(Self::$variant(payload) => serde_json::to_value(payload),)+
                    Self::Unknown { payload, .. } => Ok(payload.clone()),
                }
            }

            fn decode_payload(kind: ResponseKind, payload: Value) -> Result<Self, serde_json::Error> {
                match kind {
                    $(ResponseKind::$kind => serde_json::from_value(payload).map(Self::$variant),)+
                    ResponseKind::Unknown(kind) => Ok(Self::Unknown { kind, payload }),
                }
            }
        }
    };
}

define_response_body! {
    ContractDescription(ContractDescription) => ContractDescription,
    MachineDetails(Box<MachineDetails>) => MachineDetails,
    MachineToken(MachineToken) => MachineToken,
    Initialized(Initialized) => Initialized,
    Registered(Registered) => Registered,
    JoinAccepted(JoinAccepted) => JoinAccepted,
    MachineList(MachineList) => MachineList,
    ContainerList(ContainerList) => ContainerList,
    ContainerDetails(Box<ContainerDetails>) => ContainerDetails,
    ContainerCreated(ContainerCreated) => ContainerCreated,
    ContainerChanged(ContainerChanged) => ContainerChanged,
    VolumeCreated(VolumeCreated) => VolumeCreated,
    VolumeList(VolumeList) => VolumeList,
    VolumeDetails(VolumeDetails) => VolumeDetails,
    VolumeRemoved(VolumeRemoved) => VolumeRemoved,
    MachineImages(MachineImages) => MachineImages,
    MachineUpdated(MachineUpdated) => MachineUpdated,
    LocalMachineRemoved(LocalMachineRemoved) => LocalMachineRemoved,
    MachineRemoved(MachineRemoved) => MachineRemoved,
    WireGuardInspected(WireGuardInspected) => WireGuardInspected,
    ResetAccepted(ResetAccepted) => ResetAccepted,
    Error(RpcError) => Error,
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
            body: RpcResponseBody::MachineDetails(Box::new(details)),
        }
    }

    #[must_use]
    pub fn machine_token(token: MachineToken) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::MachineToken(token),
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
    pub fn machine_updated(machine: Machine) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::MachineUpdated(MachineUpdated { machine }),
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
    pub fn local_machine_removed(removed: LocalMachineRemoved) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::LocalMachineRemoved(removed),
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
    pub fn machine_removed() -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::MachineRemoved(MachineRemoved {}),
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
    pub fn wireguard_inspected(device: WireGuardDevice) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::WireGuardInspected(WireGuardInspected { device }),
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

    pub fn decode_machine_token(&self) -> Result<&MachineToken, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::MachineToken(token) = &self.body {
            Ok(token)
        } else {
            Err(self.unexpected("machine_token"))
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

    pub fn decode_machine_updated(&self) -> Result<&Machine, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::MachineUpdated(updated) = &self.body {
            Ok(&updated.machine)
        } else {
            Err(self.unexpected("machine_updated"))
        }
    }

    pub fn decode_local_machine_removed(&self) -> Result<&LocalMachineRemoved, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::LocalMachineRemoved(removed) = &self.body {
            Ok(removed)
        } else {
            Err(self.unexpected("local_machine_removed"))
        }
    }

    pub fn decode_machine_removed(&self) -> Result<(), CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::MachineRemoved(_) = &self.body {
            Ok(())
        } else {
            Err(self.unexpected("machine_removed"))
        }
    }

    pub fn decode_wireguard_inspected(&self) -> Result<&WireGuardDevice, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        if let RpcResponseBody::WireGuardInspected(inspected) = &self.body {
            Ok(&inspected.device)
        } else {
            Err(self.unexpected("wireguard_inspected"))
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
        let payload = self
            .body
            .encode_payload()
            .map_err(serde::ser::Error::custom)?;
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
        let body = RpcResponseBody::decode_payload(wire.kind, wire.payload)
            .map_err(serde::de::Error::custom)?;
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
