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
    DockerVolume, LocalMachinePhase, Machine, MachineId, MachineLogService, MachineName,
    MachineObservation, MachineRuntime, MachineToken, MachineUpdate, PublicIpDiscovery,
    ResolvedServiceSpec, RttObservation, WireGuardDevice, WireGuardPublicKey,
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
pub const GET_CADDY_CONFIG_CAPABILITY: &str = "ployz.caddy.config.v1";
pub const RESERVE_DOMAIN_CAPABILITY: &str = "ployz.dns.reserve.v1";
pub const GET_DOMAIN_CAPABILITY: &str = "ployz.dns.show.v1";
pub const RELEASE_DOMAIN_CAPABILITY: &str = "ployz.dns.release.v1";
pub const CREATE_DOMAIN_RECORDS_CAPABILITY: &str = "ployz.dns.records.create.v1";
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
    #[serde(default)]
    pub public_ip: Option<IpAddr>,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GetCaddyConfigRequest {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReserveDomainRequest {
    pub endpoint: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GetDomainRequest {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReleaseDomainRequest {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DnsRecordType {
    #[serde(rename = "A")]
    A,
    #[serde(rename = "AAAA")]
    Aaaa,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DnsRecordRequest {
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: DnsRecordType,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateDomainRecordsRequest {
    pub records: Vec<DnsRecordRequest>,
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
        package $package:literal
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty),)+ }
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

        /// One marker type per catalog row. Callers construct with
        /// `op::ListMachines::into_request(request)` and select unary RPCs at a
        /// type level with `client.call::<op::ListMachines>(request, target)`.
        pub mod op {
            $(
                #[doc = concat!("The `", $unary_command, "` RPC.")]
                pub struct $unary_variant;
            )+
            $(
                #[doc = concat!("The `", $stream_command, "` RPC.")]
                pub struct $stream_variant;
            )+
        }

        $(
            impl op::$unary_variant {
                pub fn into_request(request: $unary_request) -> RpcRequest {
                    RpcRequestBody::$unary_variant(request).into()
                }
            }

            impl Rpc for op::$unary_variant {
                type Request = $unary_request;
                type Response = $unary_response;

                const PATH: &'static str = concat!("/", $package, ".MachineRpc/", $unary_route);

                fn into_request(request: Self::Request) -> RpcRequest {
                    RpcRequestBody::$unary_variant(request).into()
                }
            }
        )+
        $(
            impl op::$stream_variant {
                pub fn into_request(request: $stream_request) -> RpcRequest {
                    RpcRequestBody::$stream_variant(request).into()
                }
            }
        )+
    };
}

/// One unary Machine RPC, generated from the catalog. The associated `Response` is the
/// response that RPC resolves to, so a request paired with the wrong response is a
/// compile error rather than a runtime `UnexpectedResponse`.
pub trait Rpc {
    type Request;
    type Response: ResponsePayload;

    /// The fully qualified gRPC path this RPC is dispatched on.
    const PATH: &'static str;

    fn into_request(request: Self::Request) -> RpcRequest;
}

crate::rpc_catalog!(define_request_body);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub protocol_major: u32,
    #[serde(flatten)]
    pub body: RpcRequestBody,
}

/// Every request carries the protocol major this build speaks; constructors cannot forget it.
impl From<RpcRequestBody> for RpcRequest {
    fn from(body: RpcRequestBody) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body,
        }
    }
}

impl RpcRequest {
    pub fn encode(&self) -> Result<OpaquePayload, CodecError> {
        OpaquePayload::from_json(self)
    }
}

#[derive(Deserialize)]
struct RequestHeader {
    protocol_major: u32,
    command: String,
}

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
pub struct CaddyConfig {
    pub caddyfile: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Domain {
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DnsRecord {
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: DnsRecordType,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DomainRecords {
    pub records: Vec<DnsRecord>,
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

/// The single response catalog: one row declares a response's wire kind, its typed
/// payload, and the accessor callers use to project it. Adding a response is one row.
macro_rules! define_responses {
    ($(
        $variant:ident($payload:ty) => $wire:literal,
            $accessor:ident($binding:pat_param) -> $projection:ty = $value:expr;
    )+) => {
        crate::value::open_string_enum!(ResponseKind, Unknown {
            $($variant => $wire),+
        });

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
                    $(Self::$variant(_) => ResponseKind::$variant,)+
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
                    $(ResponseKind::$variant => serde_json::from_value(payload).map(Self::$variant),)+
                    ResponseKind::Unknown(kind) => Ok(Self::Unknown { kind, payload }),
                }
            }
        }

        impl RpcResponse {
            $(
                pub fn $accessor(&self) -> Result<$projection, CodecError> {
                    validate_protocol_major(self.protocol_major)?;
                    if let RpcResponseBody::$variant($binding) = &self.body {
                        Ok($value)
                    } else {
                        Err(self.unexpected($wire))
                    }
                }
            )+
        }

        $(
            impl ResponsePayload for $payload {
                fn from_body(body: RpcResponseBody) -> Result<Self, CodecError> {
                    if let RpcResponseBody::$variant(payload) = body {
                        Ok(payload)
                    } else {
                        Err(CodecError::UnexpectedResponse {
                            expected: $wire,
                            actual: body.kind().as_str().to_owned(),
                        })
                    }
                }
            }
        )+
    };
}

/// A typed response payload that can be lifted out of an [`RpcResponseBody`].
/// Implemented for every row of the response catalog.
pub trait ResponsePayload: Sized {
    fn from_body(body: RpcResponseBody) -> Result<Self, CodecError>;
}

define_responses! {
    ContractDescription(ContractDescription) => "contract_description",
        decode_contract_description(description) -> &ContractDescription = description;
    MachineDetails(Box<MachineDetails>) => "machine_details",
        decode_machine_details(details) -> &MachineDetails = details;
    MachineToken(MachineToken) => "machine_token",
        decode_machine_token(token) -> &MachineToken = token;
    Initialized(Initialized) => "initialized",
        decode_initialized(initialized) -> &Machine = &initialized.machine;
    Registered(Registered) => "registered",
        decode_registered(registered) -> &Registered = registered;
    JoinAccepted(JoinAccepted) => "join_accepted",
        decode_join_accepted(_) -> () = ();
    MachineList(MachineList) => "machine_list",
        decode_machine_list(list) -> &[MachineObservation] = &list.machines;
    ContainerList(ContainerList) => "container_list",
        decode_container_list(list) -> &[ContainerObservation] = &list.containers;
    ContainerDetails(Box<ContainerDetails>) => "container_details",
        decode_container_details(details) -> &ContainerObservation = &details.container;
    ContainerCreated(ContainerCreated) => "container_created",
        decode_container_created(created) -> &ContainerCreated = created;
    ContainerChanged(ContainerChanged) => "container_changed",
        decode_container_changed(changed) -> &ContainerId = &changed.container_id;
    VolumeCreated(VolumeCreated) => "volume_created",
        decode_volume_created(created) -> &DockerVolume = &created.volume;
    VolumeList(VolumeList) => "volume_list",
        decode_volume_list(list) -> &[DockerVolume] = &list.volumes;
    VolumeDetails(VolumeDetails) => "volume_details",
        decode_volume_details(details) -> &DockerVolume = &details.volume;
    VolumeRemoved(VolumeRemoved) => "volume_removed",
        decode_volume_removed(_) -> () = ();
    MachineImages(MachineImages) => "machine_images",
        decode_machine_images(images) -> &MachineImages = images;
    CaddyConfig(CaddyConfig) => "caddy_config",
        decode_caddy_config(config) -> &str = &config.caddyfile;
    Domain(Domain) => "domain",
        decode_domain(domain) -> &str = &domain.name;
    DomainRecords(DomainRecords) => "domain_records",
        decode_domain_records(records) -> Vec<DnsRecord> = records.records.clone();
    MachineUpdated(MachineUpdated) => "machine_updated",
        decode_machine_updated(updated) -> &Machine = &updated.machine;
    LocalMachineRemoved(LocalMachineRemoved) => "local_machine_removed",
        decode_local_machine_removed(removed) -> &LocalMachineRemoved = removed;
    MachineRemoved(MachineRemoved) => "machine_removed",
        decode_machine_removed(_) -> () = ();
    WireGuardInspected(WireGuardInspected) => "wireguard_inspected",
        decode_wireguard_inspected(inspected) -> &WireGuardDevice = &inspected.device;
    ResetAccepted(ResetAccepted) => "reset_accepted",
        decode_reset_accepted(_) -> () = ();
    Error(RpcError) => "error",
        decode_error(error) -> &RpcError = error;
}

#[derive(Clone, Debug, PartialEq)]
pub struct RpcResponse {
    pub protocol_major: u32,
    pub body: RpcResponseBody,
}

/// Every response carries the protocol major this build speaks; constructors cannot forget it.
impl From<RpcResponseBody> for RpcResponse {
    fn from(body: RpcResponseBody) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body,
        }
    }
}

impl RpcResponse {
    #[must_use]
    pub fn contract_description(description: ContractDescription) -> Self {
        RpcResponseBody::ContractDescription(description).into()
    }

    #[must_use]
    pub fn error(error: RpcError) -> Self {
        RpcResponseBody::Error(error).into()
    }

    #[must_use]
    pub fn reset_accepted() -> Self {
        RpcResponseBody::ResetAccepted(ResetAccepted {}).into()
    }

    #[must_use]
    pub fn machine_details(details: MachineDetails) -> Self {
        RpcResponseBody::MachineDetails(Box::new(details)).into()
    }

    #[must_use]
    pub fn machine_token(token: MachineToken) -> Self {
        RpcResponseBody::MachineToken(token).into()
    }

    #[must_use]
    pub fn initialized(machine: Machine) -> Self {
        RpcResponseBody::Initialized(Initialized { machine }).into()
    }

    #[must_use]
    pub fn registered(registered: Registered) -> Self {
        RpcResponseBody::Registered(registered).into()
    }

    #[must_use]
    pub fn join_accepted() -> Self {
        RpcResponseBody::JoinAccepted(JoinAccepted {}).into()
    }

    #[must_use]
    pub fn machine_list(machines: Vec<MachineObservation>) -> Self {
        RpcResponseBody::MachineList(MachineList { machines }).into()
    }

    #[must_use]
    pub fn container_list(containers: Vec<ContainerObservation>) -> Self {
        RpcResponseBody::ContainerList(ContainerList { containers }).into()
    }

    #[must_use]
    pub fn volume_created(volume: DockerVolume) -> Self {
        RpcResponseBody::VolumeCreated(VolumeCreated { volume }).into()
    }

    #[must_use]
    pub fn machine_updated(machine: Machine) -> Self {
        RpcResponseBody::MachineUpdated(MachineUpdated { machine }).into()
    }

    #[must_use]
    pub fn container_details(container: ContainerObservation) -> Self {
        RpcResponseBody::ContainerDetails(Box::new(ContainerDetails { container })).into()
    }

    #[must_use]
    pub fn volume_list(volumes: Vec<DockerVolume>) -> Self {
        RpcResponseBody::VolumeList(VolumeList { volumes }).into()
    }

    #[must_use]
    pub fn local_machine_removed(removed: LocalMachineRemoved) -> Self {
        RpcResponseBody::LocalMachineRemoved(removed).into()
    }

    #[must_use]
    pub fn container_created(created: ContainerCreated) -> Self {
        RpcResponseBody::ContainerCreated(created).into()
    }

    #[must_use]
    pub fn volume_details(volume: DockerVolume) -> Self {
        RpcResponseBody::VolumeDetails(VolumeDetails { volume }).into()
    }

    #[must_use]
    pub fn machine_removed() -> Self {
        RpcResponseBody::MachineRemoved(MachineRemoved {}).into()
    }

    #[must_use]
    pub fn container_changed(container_id: ContainerId) -> Self {
        RpcResponseBody::ContainerChanged(ContainerChanged { container_id }).into()
    }

    #[must_use]
    pub fn volume_removed() -> Self {
        RpcResponseBody::VolumeRemoved(VolumeRemoved {}).into()
    }

    #[must_use]
    pub fn machine_images(images: MachineImages) -> Self {
        RpcResponseBody::MachineImages(images).into()
    }

    #[must_use]
    pub fn caddy_config(caddyfile: String) -> Self {
        RpcResponseBody::CaddyConfig(CaddyConfig { caddyfile }).into()
    }

    #[must_use]
    pub fn domain(name: String) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::Domain(Domain { name }),
        }
    }

    #[must_use]
    pub fn domain_records(records: Vec<DnsRecord>) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            body: RpcResponseBody::DomainRecords(DomainRecords { records }),
        }
    }

    #[must_use]
    pub fn wireguard_inspected(device: WireGuardDevice) -> Self {
        RpcResponseBody::WireGuardInspected(WireGuardInspected { device }).into()
    }

    #[must_use]
    pub fn kind(&self) -> ResponseKind {
        self.body.kind()
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

    /// Lift the typed payload out of this response, validating the protocol major first.
    pub fn decode<P: ResponsePayload>(self) -> Result<P, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        P::from_body(self.body)
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
    Unauthenticated => "unauthenticated",
});

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}
