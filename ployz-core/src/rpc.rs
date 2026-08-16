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
pub const UNREGISTRY_PORT: u16 = 51500;

/// When a Machine includes a catalogued capability in `describe_contract`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityAdvertisement {
    Always,
    Container,
    Caddy,
    Cluster,
}

impl CapabilityAdvertisement {
    /// Capability names this class advertises, in catalog order.
    pub fn capabilities(self) -> impl Iterator<Item = CapabilityName> {
        CATALOGUED_CAPABILITIES
            .iter()
            .filter(move |(_, class)| *class == self)
            .map(|(name, _)| CapabilityName::parse(*name).expect("static capability name is valid"))
    }
}

macro_rules! define_capabilities {
    (
        package $package:literal
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident),)+ }
    ) => {
        $(pub const $unary_capability: &str = $unary_capability_name;)+
        $(pub const $stream_capability: &str = $stream_capability_name;)+

        /// Bidirectional exec is outside the unary catalog.
        pub const EXEC_CONTAINER_CAPABILITY: &str = "ployz.container.exec.v1";

        const CATALOGUED_CAPABILITIES: &[(&str, CapabilityAdvertisement)] = &[
            $(($unary_capability, CapabilityAdvertisement::$unary_advertisement),)+
            $(($stream_capability, CapabilityAdvertisement::$stream_advertisement),)+
            (EXEC_CONTAINER_CAPABILITY, CapabilityAdvertisement::Container),
        ];
    };
}

crate::rpc_catalog!(define_capabilities);

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
    #[error("expected request command {expected:?}, received {actual:?}")]
    UnexpectedRequest {
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
pub struct DnsRecord {
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: DnsRecordType,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateDomainRecordsRequest {
    pub records: Vec<DnsRecord>,
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
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident),)+ }
    ) => {
        /// Commands are closed and own their typed payloads.
        ///
        /// The catalog stores caller-facing request types unboxed so
        /// `Rpc::Request` is the payload. That makes a few variants large.
        #[allow(clippy::large_enum_variant)]
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
                    op::$unary_variant::into_request(request)
                }

                fn from_request_body(body: RpcRequestBody) -> Result<Self::Request, CodecError> {
                    match body {
                        RpcRequestBody::$unary_variant(request) => Ok(request),
                        other => Err(CodecError::UnexpectedRequest {
                            expected: $unary_command,
                            actual: other.command().to_owned(),
                        }),
                    }
                }

                fn from_body(body: RpcResponseBody) -> Result<Self::Response, CodecError> {
                    <$unary_response as FromResponseBody>::from_body(body)
                }
            }
        )+
        $(
            impl op::$stream_variant {
                pub fn into_request(request: $stream_request) -> RpcRequest {
                    RpcRequestBody::$stream_variant(request).into()
                }

                pub fn from_request_body(
                    body: RpcRequestBody,
                ) -> Result<$stream_request, CodecError> {
                    match body {
                        RpcRequestBody::$stream_variant(request) => Ok(request),
                        other => Err(CodecError::UnexpectedRequest {
                            expected: $stream_command,
                            actual: other.command().to_owned(),
                        }),
                    }
                }
            }
        )+

        impl RpcRequestBody {
            #[must_use]
            pub fn command(&self) -> &'static str {
                match self {
                    $(Self::$unary_variant(_) => $unary_command,)+
                    $(Self::$stream_variant(_) => $stream_command,)+
                }
            }
        }
    };
}

/// One unary Machine RPC, generated from the catalog. The associated `Response` is the
/// envelope that RPC resolves to, so a request paired with the wrong response is a
/// compile error rather than a runtime `UnexpectedResponse`.
pub trait Rpc {
    type Request;
    type Response;

    /// The fully qualified gRPC path this RPC is dispatched on.
    const PATH: &'static str;

    fn into_request(request: Self::Request) -> RpcRequest;

    fn from_request_body(body: RpcRequestBody) -> Result<Self::Request, CodecError>;

    /// Lift this RPC's envelope out of a decoded body. Prefer [`RpcResponse::decode`],
    /// which checks the protocol major first.
    fn from_body(body: RpcResponseBody) -> Result<Self::Response, CodecError>;
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

/// Envelope identity: one row is the wire kind and the payload stored in that
/// variant. Callers lift with [`RpcResponse::decode`] and read fields on the envelope.
trait FromResponseBody: Sized {
    fn from_body(body: RpcResponseBody) -> Result<Self, CodecError>;
}

macro_rules! define_responses {
    ($($variant:ident($payload:ty) => $wire:literal;)+) => {
        crate::value::open_string_enum!(ResponseKind, Unknown {
            $($variant => $wire),+
        });

        /// Known responses own typed payloads; future responses retain their raw value.
        ///
        /// Envelope identity stores `Rpc::Response` in the variant. Inspect
        /// payloads are large; boxing them would make `decode` return `Box<T>`.
        #[allow(clippy::large_enum_variant)]
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

        $(
            impl FromResponseBody for $payload {
                fn from_body(body: RpcResponseBody) -> Result<Self, CodecError> {
                    match body {
                        RpcResponseBody::$variant(payload) => Ok(payload),
                        other => Err(CodecError::UnexpectedResponse {
                            expected: $wire,
                            actual: other.kind().as_str().to_owned(),
                        }),
                    }
                }
            }

            impl From<$payload> for RpcResponse {
                fn from(payload: $payload) -> Self {
                    RpcResponseBody::$variant(payload).into()
                }
            }
        )+
    };
}

define_responses! {
    ContractDescription(ContractDescription) => "contract_description";
    MachineDetails(MachineDetails) => "machine_details";
    MachineToken(MachineToken) => "machine_token";
    Initialized(Initialized) => "initialized";
    Registered(Registered) => "registered";
    JoinAccepted(JoinAccepted) => "join_accepted";
    MachineList(MachineList) => "machine_list";
    ContainerList(ContainerList) => "container_list";
    ContainerDetails(ContainerDetails) => "container_details";
    ContainerCreated(ContainerCreated) => "container_created";
    ContainerChanged(ContainerChanged) => "container_changed";
    DockerVolume(DockerVolume) => "docker_volume";
    VolumeList(VolumeList) => "volume_list";
    VolumeRemoved(VolumeRemoved) => "volume_removed";
    MachineImages(MachineImages) => "machine_images";
    CaddyConfig(CaddyConfig) => "caddy_config";
    Domain(Domain) => "domain";
    DomainRecords(DomainRecords) => "domain_records";
    MachineUpdated(MachineUpdated) => "machine_updated";
    LocalMachineRemoved(LocalMachineRemoved) => "local_machine_removed";
    MachineRemoved(MachineRemoved) => "machine_removed";
    WireGuardInspected(WireGuardInspected) => "wireguard_inspected";
    ResetAccepted(ResetAccepted) => "reset_accepted";
    Error(RpcError) => "error";
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
    pub fn kind(&self) -> ResponseKind {
        self.body.kind()
    }

    pub fn encode(&self) -> Result<OpaquePayload, CodecError> {
        OpaquePayload::from_json(self)
    }

    /// Lift this RPC's envelope out of the response, validating the protocol major first.
    pub fn decode<T: Rpc>(self) -> Result<T::Response, CodecError> {
        validate_protocol_major(self.protocol_major)?;
        T::from_body(self.body)
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

#[derive(Clone, Debug, Error, PartialEq, Serialize, Deserialize)]
#[error("{message}")]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
}
