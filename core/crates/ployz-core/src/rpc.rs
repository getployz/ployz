use crate::{
    InspectVolumeCopyRequest, SeedVolumeRequest, SetVolumeCopyReply, SetVolumeCopyRequest,
    StorageCapacity, VolumeCopyView,
    metrics::{MetricHistoryReply, MetricHistoryRequest},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::IpAddr,
};
use ts_rs::TS;

mod build_grant;
mod upgrade;
pub use build_grant::*;
pub use upgrade::*;

use ipnet::Ipv4Net;
use prost::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{
    AdvertisedEndpoint, CapabilityName, CertificateHost, ContainerId, ContainerKind,
    ContainerObservation, Machine, MachineId, MachineLogService, MachineName, MachineObservation,
    MachineRuntime, MachineUpdate, ManagementCapability, ManagementClientLabel, Namespace,
    QualifiedService, ResolvedServiceSpec, ServingPointer, StorageChoice, WireGuardPublicKey,
};

mod docker;
mod inspect;

pub use crate::UNREGISTRY_PORT;
pub use docker::*;
pub use inspect::*;

/// The Machine RPC major. Every `RpcRequest` and `RpcResponse` envelope
/// carries it, and decoding an envelope refuses another major.
pub const PROTOCOL_MAJOR: u32 = 1;

/// When a Machine includes a catalogued capability in `describe_contract`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityAdvertisement {
    Always,
    Container,
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

        /// Whether this exact RPC path accepts one buffered request for fan-out.
        /// Client-streaming methods (Exec and Build) require one selected Machine.
        pub fn supports_fanout(path: &str) -> bool {
            matches!(path, $(concat!("/", $package, ".MachineRpc/", $unary_route))|+ | $(concat!("/", $package, ".MachineRpc/", $stream_route))|+)
        }

        /// The capability that names the route at `path`, or `None` for an uncatalogued path.
        #[must_use]
        pub fn capability_of(path: &str) -> Option<&'static str> {
            match path {
                $(concat!("/", $package, ".MachineRpc/", $unary_route) => Some($unary_capability),)+
                $(concat!("/", $package, ".MachineRpc/", $stream_route) => Some($stream_capability),)+
                EXEC_PATH => Some(EXEC_CONTAINER_CAPABILITY),
                BUILD_PATH => Some(BUILD_CAPABILITY),
                _ => None,
            }
        }

        /// gRPC path of the Exec stream.
        pub const EXEC_PATH: &str = concat!("/", $package, ".MachineRpc/Exec");
        /// gRPC path of the Build stream.
        pub const BUILD_PATH: &str = concat!("/", $package, ".MachineRpc/Build");

        /// Bidirectional exec is outside the unary catalog.
        pub const EXEC_CONTAINER_CAPABILITY: &str = "ployz.container.exec.v1";
        /// One admitted Build over the existing bidirectional Machine stream.
        pub const BUILD_CAPABILITY: &str = "ployz.build.v1";

        /// The daemon can take a Certificate Policy from cluster state.
        pub const CERTIFICATE_POLICY_CAPABILITY: &str = "ployz.certificates.policy.v1";

        /// `SetVolumeCopy` honours `gen`, takes the `floor` want and refuses `stale`.
        pub const VOLUME_FLOOR_CAPABILITY: &str = "ployz.volume.copy.floor.v1";

        /// `Inspect` can report requested current local Machine storage evidence.
        pub const MACHINE_STORAGE_OBSERVATION_CAPABILITY: &str =
            "ployz.machine.storage-observation.v1";

        const CATALOGUED_CAPABILITIES: &[(&str, CapabilityAdvertisement)] = &[
            $(($unary_capability, CapabilityAdvertisement::$unary_advertisement),)+
            $(($stream_capability, CapabilityAdvertisement::$stream_advertisement),)+
            (EXEC_CONTAINER_CAPABILITY, CapabilityAdvertisement::Container),
            (BUILD_CAPABILITY, CapabilityAdvertisement::Container),
            (VOLUME_FLOOR_CAPABILITY, CapabilityAdvertisement::Container),
            (CERTIFICATE_POLICY_CAPABILITY, CapabilityAdvertisement::Always),
            (
                MACHINE_STORAGE_OBSERVATION_CAPABILITY,
                CapabilityAdvertisement::Always,
            ),
        ];
    };
}

crate::rpc_catalog!(define_capabilities);

/// Every capability name this binary can serve, whether or not a unit is on now.
#[must_use]
pub fn served_capabilities() -> BTreeSet<String> {
    CATALOGUED_CAPABILITIES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect()
}

/// The only protobuf-shaped value understood by tonic and the transparent proxy.
#[derive(Clone, PartialEq, Message)]
pub struct OpaquePayload {
    #[prost(bytes = "vec", tag = "1")]
    pub json: Vec<u8>,
}

/// Generated tonic client and server for the shared Machine service.
#[cfg(not(target_arch = "wasm32"))]
pub mod transport {
    include!(concat!(env!("OUT_DIR"), "/ployz.rpc.v1.MachineRpc.rs"));
}

#[cfg(not(target_arch = "wasm32"))]
pub use transport::{
    machine_rpc_client::MachineRpcClient, machine_rpc_server::MachineRpc,
    machine_rpc_server::MachineRpcServer,
};

/// Maximum encoded Runtime Watch message size accepted by Cloud and sent by the daemon.
pub const RUNTIME_WATCH_MESSAGE_SIZE_LIMIT: usize = 64 * 1024 * 1024;

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
    #[error("Could not encode the JSON payload.")]
    EncodeJson(#[source] serde_json::Error),
    #[error("Could not decode the JSON payload.")]
    DecodeJson(#[source] serde_json::Error),
    #[error("unsupported RPC command {0:?}")]
    UnsupportedCommand(String),
    #[error("unsupported protocol major {requested}; this endpoint supports {supported}")]
    UnsupportedProtocolMajor { requested: u32, supported: u32 },
    #[error("expected response kind {expected}, received {}", .actual.escape_debug())]
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },
    #[error("expected request command {expected}, received {actual}")]
    UnexpectedRequest {
        expected: &'static str,
        actual: String,
    },
}

/// The empty payload of the capability-description command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DescribeContractRequest {}

/// This Machine's own enrollment facts, returned with every lifecycle refusal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineNow {
    pub id: MachineId,
    pub phase: crate::LocalMachinePhase,
    #[serde(default)]
    pub machine: Option<Machine>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnrollRequest {
    #[serde(rename = "as")]
    pub role: EnrollAs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_mtu: Option<u32>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnrollAs {
    /// First Machine of a new Cluster.
    Founder {
        name: MachineName,
        #[ts(type = "string")]
        cluster_network: Ipv4Net,
        /// Complete policy committed in the first Machine assignment, before participation.
        initial_policy: crate::InitialMachinePolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(type = "string | null")]
        public_ip: Option<IpAddr>,
        #[serde(default)]
        advertised_endpoints: Vec<AdvertisedEndpoint>,
    },
    /// Joins with the registration a Cluster member issued.
    Member { registration: Registered },
}

/// A resend replays `Enrolled` with the stored record; the daemon restarts only
/// after a fresh enrollment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Enrolled {
    Enrolled {
        machine: Machine,
    },
    /// Enrolled as someone else. `error.code` is `conflict`.
    Refused {
        now: MachineNow,
        error: RpcError,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LeaveRequest {}

/// A successful Leave cannot report a Machine that still participates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LeftPhase {
    Resetting,
    Uninitialized,
}

/// A resend on a Machine already Resetting or Uninitialized replies `Left` with that phase.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Left {
    Left {
        phase: LeftPhase,
        /// Publication removal failed best effort; the local leave happened.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_warning: Option<String>,
    },
    /// An upgrade attempt is nonterminal. `error.code` is `conflict`.
    Refused { now: MachineNow, error: RpcError },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ForgetRequest {
    pub machine_id: MachineId,
}

/// A missing row is success.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Forgotten {
    Forgotten,
    /// `machine_id` is this Machine; a Machine leaves itself with Leave. `error.code` is `invalid_argument`.
    Refused {
        now: MachineNow,
        error: RpcError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RegisterRequest {
    /// Durable identity of the joining Machine.
    pub machine_id: MachineId,
    /// Client-selected subnet, required for Register publication.
    /// Allocation policy callers omit it before selecting an assignment.
    #[serde(default)]
    pub assigned_subnet: Option<crate::MachineSubnet>,
    /// Complete policy committed in the first Machine assignment, before participation.
    pub initial_policy: crate::InitialMachinePolicy,
    pub name: MachineName,
    pub storage: StorageChoice,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub public_ip: Option<IpAddr>,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub runtime: MachineRuntime,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListMachinesRequest {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWatchRequest {}

/// Whether listed Container observations carry real environment values.
///
/// `Redacted` replaces every Service and pre-deploy hook environment value with
/// [`REDACTED_ENVIRONMENT_VALUE`](crate::REDACTED_ENVIRONMENT_VALUE), keeping the keys.
/// Callers that plan, compare, or copy a `resolved_spec` use `Included`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentValues {
    #[default]
    Redacted,
    Included,
}

/// List this Machine's managed Containers; environment values stay redacted unless the caller requests them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListContainersRequest {
    /// Real or redacted environment values; see [`EnvironmentValues`].
    #[serde(default)]
    pub environment: EnvironmentValues,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectContainerRequest {
    pub container_id: ContainerId,
}

/// Exactly one update to a labelled Management Client slot: set it or clear it.
///
/// Neither case carries a secret; `Set` returns the fresh client key in its capability.
/// Strict by the Stable promise's security exception: an unrecognized field may
/// be secret material the daemon must never accept.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetManagementClientRequest {
    Set { label: ManagementClientLabel },
    Clear { label: ManagementClientLabel },
}

/// Confirmation of a slot update. `capability` is present after `Set` and absent after `Clear`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetManagementClientResponse {
    #[ts(type = "string | null")]
    pub capability: Option<ManagementCapability>,
}

/// The place one Container creation lands on a Machine. The same slot names the
/// same Container across resends; a different spec in it is an occupied slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SlotKey {
    pub namespace: Namespace,
    pub kind: ContainerKind,
    pub creation_key: crate::CreationKey,
}

/// A Container named either by its Docker id or by the slot its creation landed on.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContainerLocator {
    Id(ContainerId),
    Key(SlotKey),
}

/// Ask a Machine to make one Container match a wanted state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnsureContainerRequest {
    pub want: ContainerWant,
}

/// The state a driver wants one Container in. The Machine converges to it and
/// answers with what it then observes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "a Running want carries the whole resolved spec; one request holds one"
)]
pub enum ContainerWant {
    Running {
        container: RunTarget,
    },
    Stopped {
        container: ContainerLocator,
        drain: crate::Drain,
        #[serde(default)]
        stop: crate::StopOptions,
    },
    Gone {
        container: ContainerLocator,
        drain: crate::Drain,
        #[serde(default)]
        anonymous_volumes: AnonymousVolumes,
    },
}

/// What `Gone` does with the Container's anonymous Volumes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AnonymousVolumes {
    /// They go with the Container.
    #[default]
    Remove,
    /// They stay, since deleting data is a Volume decision.
    Keep,
}

impl From<crate::RemoveContainerPurpose> for AnonymousVolumes {
    fn from(purpose: crate::RemoveContainerPurpose) -> Self {
        match purpose {
            crate::RemoveContainerPurpose::Rollout => Self::Remove,
            crate::RemoveContainerPurpose::Retire => Self::Keep,
        }
    }
}

/// What `Running` runs: a new creation in a slot, or a Container that already exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "a new creation carries the whole resolved spec; one request holds one"
)]
pub enum RunTarget {
    New(NewContainer),
    Existing(ContainerLocator),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NewContainer {
    pub slot: SlotKey,
    pub resolved_spec: ResolvedServiceSpec,
    /// Correlation metadata, never part of the Service configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<crate::DeploymentLogId>,
    /// Credentials for pulling the image; used for this pull only, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_auth: Option<crate::RegistryAuth>,
    /// A `PullImage` already placed the image, so creation uses it without pulling.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub image_prepared: bool,
    #[serde(default, skip_serializing_if = "AnswerWhen::is_started")]
    pub answer: AnswerWhen,
}

/// When the Machine answers a new creation: once it starts, or once its health settles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AnswerWhen {
    #[default]
    Started,
    /// After the Container is ready, fails, or outlives its health deadline; the
    /// answer carries a [`HealthVerdict`].
    Settled,
}

impl AnswerWhen {
    fn is_started(&self) -> bool {
        *self == Self::Started
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogsOptions {
    pub follow: bool,
    pub tail: i32,
    /// Unix nanoseconds; Docker sees whole seconds and ployzd drops the
    /// earlier output of that second.
    #[serde(default)]
    pub since_nanos: Option<i64>,
    #[serde(default)]
    pub until_unix_seconds: Option<i64>,
}

/// What a live log read follows: one Container's output, or one service on the Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveLogTarget {
    Container(ContainerId),
    Machine(MachineLogService),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TailLogsRequest {
    pub target: LiveLogTarget,
    pub options: LogsOptions,
}

/// The most rows one history page carries.
pub const LOG_HISTORY_PAGE_LIMIT: u16 = 5_000;

/// One page of a Machine's Log Store, rows ordered by timestamp.
///
/// Every selector field that is set must match a container's saved metadata.
/// `since_nanos` is inclusive and `until_nanos` exclusive. `cursor` is the
/// `next` of the previous page's [`HistoryRow::End`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogHistoryRequest {
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub deployment: Option<String>,
    #[serde(default)]
    pub container_id: Option<ContainerId>,
    #[serde(default)]
    pub since_nanos: Option<i64>,
    #[serde(default)]
    pub until_nanos: Option<i64>,
    pub direction: LogDirection,
    pub limit: u16,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl LogHistoryRequest {
    /// # Errors
    ///
    /// Names the first field outside the contract.
    pub fn validate(&self) -> Result<(), String> {
        if self.namespace.is_none()
            && self.service.is_none()
            && self.deployment.is_none()
            && self.container_id.is_none()
        {
            return Err("select a namespace, service, deployment, or container".into());
        }
        if !(1..=LOG_HISTORY_PAGE_LIMIT).contains(&self.limit) {
            return Err(format!("limit must be 1..={LOG_HISTORY_PAGE_LIMIT}"));
        }
        if let (Some(since), Some(until)) = (self.since_nanos, self.until_nanos)
            && since > until
        {
            return Err("since_nanos must not be after until_nanos".into());
        }
        Ok(())
    }
}

/// Which end of the time range a history page starts from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogDirection {
    #[default]
    Forward,
    /// Newest first: the last rows of the range.
    Backward,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListImagesRequest {
    #[serde(default)]
    pub reference: Option<String>,
    /// Inspect each listed image for [`ImageSummary::last_tagged`]; one Docker
    /// read per image, so only narrow listings should ask.
    #[serde(default)]
    pub last_tagged: bool,
}

/// Empty payload of the command that returns this Machine's image-ingest TCP destination.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnsureImageIngestRequest {}

/// Named failure in `RpcError.details.reason` when image ingest cannot be opened.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageIngestReason {
    NotParticipating,
    DockerUnavailable,
    UnsupportedContainerdStore,
    ContainerdSocketMissing,
    StartFailed,
}

impl ImageIngestReason {
    /// The `reason` field of an ingest RPC error, if it is one of the frozen names.
    #[must_use]
    pub fn from_details(details: &Value) -> Option<Self> {
        details
            .get("reason")
            .and_then(|reason| Self::deserialize(reason).ok())
    }

    /// An RPC error that carries this reason in `details`.
    #[must_use]
    pub fn rpc_error(self, message: impl Into<String>) -> RpcError {
        RpcError {
            code: self.rpc_code(),
            message: message.into(),
            details: serde_json::json!({ "reason": self }),
            cause: Vec::new(),
        }
    }

    const fn rpc_code(self) -> RpcErrorCode {
        match self {
            Self::UnsupportedContainerdStore => RpcErrorCode::Unsupported,
            Self::StartFailed => RpcErrorCode::Internal,
            Self::NotParticipating | Self::DockerUnavailable | Self::ContainerdSocketMissing => {
                RpcErrorCode::Unavailable
            }
        }
    }
}

/// Management-plane TCP bind that accepts `docker push` and peer `docker pull`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageIngestDestination {
    /// Machine management-plane address that owns the ingest endpoint.
    pub management_address: crate::ManagementAddress,
    /// Plain-HTTP OCI Distribution port on the management plane.
    pub port: u16,
}

/// Successful `EnsureImageIngest` payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageIngestOpened {
    pub destination: ImageIngestDestination,
}

/// Pull one image from another Machine's image-ingest TCP destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PullImageFromMachineRequest {
    /// Select reference delivery or verified exact-content publication.
    pub pull: PeerImagePull,
    pub source: ImageIngestDestination,
    /// Platform the destination must receive, so a partial source cannot
    /// answer with an index whose selected variant it does not hold.
    pub platform: String,
}

/// Whether peer delivery follows a reference or publishes a tag for exact content.
/// Strict by the Stable promise's security exception: the mode selects digest
/// verification, so a stray field must not blur reference into publication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum PeerImagePull {
    /// Pull this reference; digest references receive a deterministic retention tag.
    Reference { image: String },
    /// Pull pinned content and publish `tag` only after verifying its digest.
    Publish {
        image: crate::ImageDigestReference,
        tag: String,
    },
}

impl PeerImagePull {
    /// The source reference to fetch.
    #[must_use]
    pub fn image(&self) -> &str {
        match self {
            Self::Reference { image } => image,
            Self::Publish { image, .. } => image.as_str(),
        }
    }

    /// The requested destination tag, if this delivery publishes one.
    #[must_use]
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Reference { .. } => None,
            Self::Publish { tag, .. } => Some(tag),
        }
    }
}

/// Make one image present on this Machine as `policy` asks, pulling from its registry.
/// The image must be present afterwards, so `Never` fails for an absent image.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PullImageRequest {
    pub image: String,
    pub policy: crate::PullPolicy,
    /// Credentials for a private image; used for this pull only, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_auth: Option<crate::RegistryAuth>,
}

/// Successful `PullImage` and `PullImageFromMachine` payload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImagePulled {}

/// Remove the superseded build images of each repository from this Machine's store.
/// Repositories are Docker's short names, so a registry prefix stays. Never forced:
/// a reference whose image any Container uses is kept and reported in use.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PruneImagesRequest {
    pub repositories: Vec<String>,
}

/// One result per superseded reference found; empty in the steady state.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ImagesRemoved {
    pub results: Vec<ImageRemoval>,
}

/// What happened to one requested reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ImageRemoval {
    pub reference: String,
    pub outcome: ImageRemovalOutcome,
}

/// Per-reference removal result. Open: a status this build does not know decodes as
/// `unrecognized`, so a newer Machine never breaks an older client's whole report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ImageRemovalOutcome {
    /// The reference no longer exists on this Machine.
    Removed,
    /// A Container, running or not, uses the image; it was kept.
    InUse,
    /// The Machine had no such reference.
    NotFound,
    /// Docker refused for another reason; the reference may remain.
    Failed { message: String },
    /// A status introduced after this build.
    #[serde(other)]
    Unrecognized,
}

/// Publish or clear operator-supplied Certificate Material for one certificate hostname.
///
/// Published material is served as given; ACME never orders, renews, or overwrites it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PublishCertificateMaterialRequest {
    pub hostname: CertificateHost,
    pub change: CertificateMaterialChange,
}

/// Set replaces the hostname's material; Clear removes published material and
/// returns the hostname to ACME.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CertificateMaterialChange {
    Set {
        certificate_chain_pem: String,
        private_key_pem: String,
    },
    Clear,
}

// Requests may be logged; the private key never is.
impl fmt::Debug for CertificateMaterialChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set {
                certificate_chain_pem,
                ..
            } => formatter
                .debug_struct("Set")
                .field("certificate_chain_pem", certificate_chain_pem)
                .field("private_key_pem", &"<redacted>")
                .finish(),
            Self::Clear => formatter.write_str("Clear"),
        }
    }
}

/// Commands are closed and own their typed payloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateMachineRequest {
    pub update: MachineUpdate,
}

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
                pub const PATH: &'static str = concat!("/", $package, ".MachineRpc/", $stream_route);

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

            /// The gRPC path a unary request is dispatched on; `None` for a streaming command.
            #[must_use]
            pub fn unary_path(&self) -> Option<&'static str> {
                match self {
                    $(Self::$unary_variant(_) => Some(<op::$unary_variant as Rpc>::PATH),)+
                    $(Self::$stream_variant(_) => None,)+
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Registered {
    pub assigned_machine: Machine,
    pub visible_peers: Vec<Machine>,
    pub target_versions: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineList {
    /// Enrollment facts observed by this Entry Machine.
    #[serde(default)]
    pub enrollment: Option<crate::EnrollmentSnapshot>,
    pub machines: Vec<MachineObservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerList {
    pub containers: Vec<ContainerObservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerDetails {
    pub container: ContainerObservation,
    /// Fresh Docker Config.Env; never copied into replicated observations.
    pub environment: Option<BTreeMap<String, String>>,
    /// Local image ID (`sha256:…`) the Container runs; `None` from older daemons.
    #[serde(default)]
    pub image_id: Option<String>,
    /// The newest healthcheck result; `None` without one, or from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<crate::LastHealthCheck>,
    /// Which run of the Container this is; `None` from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<crate::ContainerRun>,
}

/// The Machine's answer to `EnsureContainer`: the Container as now observed, and
/// the refusal when the want could not be met.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnsuredContainer {
    Ensured {
        now: ContainerNow,
        /// How the started Container's health settled, when the want asked to wait for it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        health: Option<HealthVerdict>,
    },
    Refused {
        now: ContainerNow,
        refusal: ContainerRefusal,
    },
}

/// What the Machine observes at the asked-for Container, after any effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "is", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "a present Container is the whole observation; one reply holds one"
)]
pub enum ContainerNow {
    Present(ContainerObservation),
    Absent { of: ContainerLocator },
}

/// How a started Container's health settled before the Machine answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum HealthVerdict {
    Healthy,
    Failed {
        observation: crate::ContainerRuntimeObservation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_check: Option<crate::LastHealthCheck>,
    },
    TimedOut {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        deadline_millis: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_check: Option<crate::LastHealthCheck>,
    },
}

impl HealthVerdict {
    /// Why the Container is not serving, or `None` when it is.
    #[must_use]
    pub fn into_failure(self) -> Option<crate::HealthFailure> {
        match self {
            Self::Healthy => None,
            Self::Failed {
                observation,
                last_check,
            } => Some(crate::HealthFailure::Runtime {
                observation,
                last_check,
            }),
            Self::TimedOut {
                deadline_millis,
                last_check,
            } => Some(crate::HealthFailure::TimedOut {
                deadline_millis,
                last_check,
            }),
        }
    }
}

impl EnsuredContainer {
    /// The observed Container, or the refusal as the error the replaced verbs answered.
    pub fn into_result(self) -> Result<ContainerNow, RpcError> {
        self.into_settled().map(|(now, _)| now)
    }

    /// The observed Container with its health verdict, or the refusal as an error.
    pub fn into_settled(self) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        match self {
            Self::Ensured { now, health } => Ok((now, health)),
            Self::Refused { refusal, .. } => Err(refusal.into_error()),
        }
    }
}

impl ContainerNow {
    #[must_use]
    pub fn present(&self) -> Option<&ContainerObservation> {
        match self {
            Self::Present(observation) => Some(observation),
            Self::Absent { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ContainerRefusal {
    pub reason: ContainerRefusalReason,
    pub message: String,
}

impl ContainerRefusal {
    /// The refusal as the error the replaced Container verbs answered.
    #[must_use]
    pub fn into_error(self) -> RpcError {
        let code = match self.reason {
            ContainerRefusalReason::NotFound => RpcErrorCode::NotFound,
            ContainerRefusalReason::NotParticipating => RpcErrorCode::Unavailable,
            ContainerRefusalReason::InvalidIngress => RpcErrorCode::InvalidArgument,
            ContainerRefusalReason::NotEligible
            | ContainerRefusalReason::SlotOccupied
            | ContainerRefusalReason::VolumesNotPrepared => RpcErrorCode::Conflict,
            ContainerRefusalReason::ImagePullFailed | ContainerRefusalReason::Unrecognized => {
                RpcErrorCode::Internal
            }
        };
        RpcError {
            code,
            message: self.message,
            details: Value::Null,
            cause: Vec::new(),
        }
    }
}

/// Why a Machine would not meet a want. An unknown reason from a newer Machine
/// reads as `Unrecognized`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContainerRefusalReason {
    NotParticipating,
    NotEligible,
    InvalidIngress,
    SlotOccupied,
    ImagePullFailed,
    VolumesNotPrepared,
    NotFound,
    #[serde(other)]
    Unrecognized,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageSummary {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub created: i64,
    pub size: i64,
    pub containers: i64,
    pub platforms: Vec<String>,
    /// Unix seconds this Machine last tagged the image, when the listing asked.
    /// Unlike `created`, it differs between reproducible builds.
    #[serde(default)]
    pub last_tagged: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineImages {
    pub containerd_store: bool,
    pub images: Vec<ImageSummary>,
    /// Docker-root filesystem space, when the Machine could read it.
    #[serde(default)]
    pub docker_root: Option<DiskSpace>,
}

/// One filesystem's size and free bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiskSpace {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// The certificate row holds the published material, or no longer holds published material.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CertificateMaterialPublished {}

/// Write one Service's Serving Pointer only if its row still holds `read`.
///
/// `read: None` is a first publish and writes only when no row exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublishServingPointerRequest {
    pub service: QualifiedService,
    pub read: Option<ServingPointer>,
    pub pointer: ServingPointer,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ServingPointerPublication {
    Published,
    /// The row no longer held the value the writer read; nothing was written.
    Superseded {
        current: Option<ServingPointer>,
    },
}

/// Read one Service's Serving Pointer from the target Machine's replicated store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadServingPointerRequest {
    pub service: QualifiedService,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServingPointerRead {
    pub pointer: Option<ServingPointer>,
    /// Machines whose ingress_routes row routes `pointer`'s deploy and every
    /// Container it names, and which deploy that is. Empty without a pointer.
    #[serde(default)]
    pub acks: BTreeMap<MachineId, crate::DeploymentLogId>,
    /// Every Machine not reported Down acks `pointer`. False without a pointer or
    /// while the Machine list is partly replicated.
    #[serde(default)]
    pub all_routed: bool,
}

/// Refusal of a Serving Pointer read or publish by a Machine that is not the lowest-id
/// Machine inviting RPC in its own Membership Observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename = "not_pointer_publisher")]
pub struct NotPointerPublisher {
    /// The Machine the refusing Machine sees as the publisher.
    pub publisher: MachineId,
}

impl NotPointerPublisher {
    /// The refusal carried by `error`, if it is one.
    #[must_use]
    pub fn from_error(error: &RpcError) -> Option<Self> {
        let tagged =
            error.details.get("reason").and_then(Value::as_str) == Some("not_pointer_publisher");
        if error.code != RpcErrorCode::Unavailable || !tagged {
            return None;
        }
        Self::deserialize(&error.details).ok()
    }

    /// An RPC error that carries this refusal in `details`.
    #[must_use]
    pub fn rpc_error(&self) -> RpcError {
        RpcError {
            code: RpcErrorCode::Unavailable,
            message: format!(
                "Serving Pointers are published by Machine {}",
                self.publisher
            ),
            details: serde_json::to_value(self).unwrap_or(Value::Null),
            cause: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineUpdated {
    pub machine: Machine,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LocalMachineRemoved {
    #[serde(default)]
    pub reset_warning: Option<String>,
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
    SetVolumeCopyReply(SetVolumeCopyReply) => "set_volume_copy_reply";
    MachineDetails(MachineDetails) => "machine_details";
    Enrolled(Enrolled) => "enrolled";
    Registered(Registered) => "registered";
    SetManagementClientResponse(SetManagementClientResponse) => "management_client_set";
    MachineList(MachineList) => "machine_list";
    ContainerList(ContainerList) => "container_list";
    ContainerDetails(ContainerDetails) => "container_details";
    EnsuredContainer(EnsuredContainer) => "ensured_container";
    StorageCapacity(crate::StorageCapacity) => "storage_capacity";
    PreparedVolumes(PreparedVolumes) => "prepared_volumes";
    VolumeInventory(VolumeInventory) => "volume_inventory";
    VolumeRemoved(VolumeRemoved) => "volume_removed";
    VolumeCopyView(crate::VolumeCopyView) => "volume_copy_view";
    MachineImages(MachineImages) => "machine_images";
    ImageIngestOpened(ImageIngestOpened) => "image_ingest_opened";
    ImagePulled(ImagePulled) => "image_pulled";
    ImagesRemoved(ImagesRemoved) => "images_removed";
    BuildGrantMinted(BuildGrantMinted) => "build_grant_minted";
    BuildGrantInspected(BuildGrantInspected) => "build_grant_inspected";
    CertificateMaterialPublished(CertificateMaterialPublished) => "certificate_material_published";
    ServingPointerPublication(ServingPointerPublication) => "serving_pointer_publication";
    ServingPointerRead(ServingPointerRead) => "serving_pointer_read";
    MachineUpdated(MachineUpdated) => "machine_updated";
    MachineUpgradeAttempt(MachineUpgradeAttempt) => "machine_upgrade_attempt";
    Left(Left) => "left";
    Forgotten(Forgotten) => "forgotten";
    MetricHistoryReply(MetricHistoryReply) => "metric_history";
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
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
    ConfirmationRequired => "confirmation_required",
});

impl fmt::Display for RpcErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().escape_debug().fmt(f)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Serialize, Deserialize, TS)]
#[error("{message}")]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default)]
    pub details: Value,
    /// Each source below `message` on the peer that failed, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub cause: Vec<String>,
}

impl RpcError {
    /// `error`'s own sentence as the message, and every source below it as a cause.
    #[must_use]
    pub fn caused(code: RpcErrorCode, error: &(dyn std::error::Error + 'static)) -> Self {
        Self {
            code,
            message: error.to_string(),
            details: Value::Null,
            cause: crate::error_chain::causes(error),
        }
    }
}

/// Metadata an entry Machine's router puts on a status it made because the target
/// Machine did not answer the forwarded call. A status the target sent never has it.
pub const TARGET_UNANSWERED_KEY: &str = "ployz-target-unanswered";

/// [`caused_status`] as `Unavailable`, marked [`TARGET_UNANSWERED_KEY`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn target_unanswered_status(error: &(dyn std::error::Error + 'static)) -> tonic::Status {
    let mut status = caused_status(tonic::Code::Unavailable, error);
    status.metadata_mut().insert(
        TARGET_UNANSWERED_KEY,
        tonic::metadata::MetadataValue::from_static("1"),
    );
    status
}

/// A gRPC status with `error`'s own sentence as the message and its sources in
/// the details as `{"cause": [...]}`, read back by [`status_causes`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn caused_status(
    code: tonic::Code,
    error: &(dyn std::error::Error + 'static),
) -> tonic::Status {
    let cause = crate::error_chain::causes(error);
    if cause.is_empty() {
        return tonic::Status::new(code, error.to_string());
    }
    let details = serde_json::to_vec(&serde_json::json!({ "cause": cause }))
        .expect("a list of strings serializes");
    tonic::Status::with_details(code, error.to_string(), details.into())
}

/// [`caused_status`] whose details also carry `hints`: an object keyed like an
/// [`RpcError`]'s details (`next`, `inspect`, `retry`), read back by [`status_details`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn hinted_status(
    code: tonic::Code,
    error: &(dyn std::error::Error + 'static),
    hints: Value,
) -> tonic::Status {
    let mut details = hints;
    if let Some(fields) = details.as_object_mut() {
        fields.insert("cause".into(), crate::error_chain::causes(error).into());
    }
    let details = serde_json::to_vec(&details).expect("a JSON value serializes");
    tonic::Status::with_details(code, error.to_string(), details.into())
}

/// The details a [`caused_status`] or [`hinted_status`] carried, without their
/// causes: `Null` when they carried none, `None` for any other details.
#[must_use]
pub fn status_details(details: &[u8]) -> Option<Value> {
    status_causes(details)?;
    let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(details) else {
        return None;
    };
    fields.remove("cause");
    Some(if fields.is_empty() {
        Value::Null
    } else {
        Value::Object(fields)
    })
}

/// The causes a [`caused_status`] carried, or `None` for any other details.
#[must_use]
pub fn status_causes(details: &[u8]) -> Option<Vec<String>> {
    #[derive(Deserialize)]
    struct Caused {
        cause: Vec<String>,
    }
    serde_json::from_slice::<Caused>(details)
        .ok()
        .map(|caused| caused.cause)
}

#[cfg(test)]
mod rpc_error_wire {
    use super::*;
    use serde_json::json;

    #[test]
    fn rpc_error_round_trips_cause() {
        let error = RpcError {
            code: RpcErrorCode::Internal,
            message: "Could not create the Container.".into(),
            details: Value::Null,
            cause: vec!["Docker responded with status code 500: denied".into()],
        };
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(
            wire.get("cause"),
            Some(&json!(["Docker responded with status code 500: denied"]))
        );
        assert_eq!(serde_json::from_value::<RpcError>(wire).unwrap(), error);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_status_carries_its_causes_in_the_details() {
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Machine did not answer".into(),
            details: Value::Null,
            cause: vec!["keep-alive timed out".into()],
        };
        let status = caused_status(tonic::Code::Unavailable, &error);
        assert_eq!(status.message(), "Machine did not answer");
        assert_eq!(
            status_causes(status.details()),
            Some(vec!["keep-alive timed out".to_owned()])
        );
        assert_eq!(status_causes(b"opaque"), None);
    }

    #[test]
    fn rpc_error_without_cause_parses() {
        let error = serde_json::from_value::<RpcError>(json!({
            "code": "conflict",
            "message": "busy",
            "details": null,
        }))
        .unwrap();
        assert!(error.cause.is_empty());
        assert!(serde_json::to_value(&error).unwrap().get("cause").is_none());
    }
}

#[cfg(test)]
mod set_management_client_wire {
    use super::*;
    use serde_json::json;

    fn machine() -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("first").unwrap(),
            subnet: crate::MachineSubnet::parse("10.210.0.0/24").unwrap(),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            runtime: Default::default(),
            build_concurrency: None,
        }
    }

    fn label(value: &str) -> ManagementClientLabel {
        ManagementClientLabel::parse(value).unwrap()
    }

    #[test]
    fn labelled_updates_round_trip_without_a_secret() {
        for (value, expected) in [
            (
                json!({ "kind": "clear", "label": "cloud" }),
                SetManagementClientRequest::Clear {
                    label: label("cloud"),
                },
            ),
            (
                json!({ "kind": "set", "label": "cloud" }),
                SetManagementClientRequest::Set {
                    label: label("cloud"),
                },
            ),
        ] {
            let request =
                serde_json::from_value::<SetManagementClientRequest>(value.clone()).unwrap();
            assert_eq!(request, expected);
            assert_eq!(serde_json::to_value(&request).unwrap(), value);
        }
    }

    #[test]
    fn updates_require_a_known_case_a_valid_label_and_no_secret() {
        for value in [
            json!({}),
            json!({ "kind": "set" }),
            json!({ "kind": "clear" }),
            json!({ "kind": "unknown", "label": "cloud" }),
            json!({ "kind": "set", "label": "Cloud" }),
            json!({ "kind": "set", "label": "cloud", "secret": "private" }),
            json!({ "kind": "clear", "label": "cloud", "pairing": { "secret": "private" } }),
        ] {
            assert!(serde_json::from_value::<SetManagementClientRequest>(value).is_err());
        }
    }

    #[test]
    fn enroll_names_its_role_and_carries_no_pairing() {
        let founder = serde_json::to_value(EnrollRequest {
            role: EnrollAs::Founder {
                name: MachineName::parse("first").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                initial_policy: Default::default(),
                public_ip: None,
                advertised_endpoints: machine().advertised_endpoints,
            },
            wireguard_mtu: None,
        })
        .unwrap();
        let member = serde_json::to_value(EnrollRequest {
            role: EnrollAs::Member {
                registration: Registered {
                    assigned_machine: machine(),
                    visible_peers: vec![machine()],
                    target_versions: BTreeMap::new(),
                },
            },
            wireguard_mtu: Some(1380),
        })
        .unwrap();
        assert_eq!(founder.pointer("/as/kind"), Some(&json!("founder")));
        assert_eq!(member.pointer("/as/kind"), Some(&json!("member")));
        assert_eq!(member.get("wireguard_mtu"), Some(&json!(1380)));
        for request in [founder, member] {
            assert!(request.get("cloud_pairing").is_none(), "{request}");
            assert!(!request.to_string().contains("secret"), "{request}");
        }
    }

    #[test]
    fn response_carries_optional_capability_without_debug_disclosure() {
        let capability =
            ManagementCapability::new(crate::ManagementIdentity::from_bytes([1; 32]), [2; 32]);
        let text = capability.to_secret_string();
        for (response, expected) in [
            (
                SetManagementClientResponse {
                    capability: Some(capability),
                },
                json!({ "capability": text }),
            ),
            (
                SetManagementClientResponse { capability: None },
                json!({ "capability": null }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&response).unwrap(), expected);
            assert_eq!(
                serde_json::from_value::<SetManagementClientResponse>(expected).unwrap(),
                response
            );
            assert!(!format!("{response:?}").contains(&text[8..]));
        }
    }
}

#[cfg(test)]
mod served_capabilities_tests {
    use super::*;

    macro_rules! catalog_capabilities {
        (
            package $package:literal
            unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident),)+ }
            server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident),)+ }
        ) => {
            [$($unary_capability_name,)+ $($stream_capability_name,)+]
        };
    }

    #[test]
    fn served_capabilities_cover_every_catalog_row_and_the_five_without_one() {
        let served = served_capabilities();
        let rows = crate::rpc_catalog!(catalog_capabilities);
        let without_row = [
            "ployz.container.exec.v1",
            "ployz.build.v1",
            "ployz.volume.copy.floor.v1",
            "ployz.certificates.policy.v1",
            "ployz.machine.storage-observation.v1",
        ];
        for name in rows.iter().chain(&without_row) {
            assert!(served.contains(*name), "{name} is not served");
        }
        assert_eq!(served.len(), rows.len() + without_row.len());
    }
}

#[cfg(test)]
mod streaming_routing_tests {
    #[test]
    fn only_catalogued_single_request_methods_allow_fanout() {
        assert!(super::supports_fanout("/ployz.rpc.v1.MachineRpc/Inspect"));
        for path in [
            "/ployz.rpc.v1.MachineRpc/Build",
            "/ployz.rpc.v1.MachineRpc/Exec",
            "/other.Service/Inspect",
        ] {
            assert!(!super::supports_fanout(path));
        }
    }

    macro_rules! assert_capability_of {
        (
            package $package:literal
            unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident),)+ }
            server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident),)+ }
        ) => {
            $(assert_eq!(
                super::capability_of(&format!("/ployz.rpc.v1.MachineRpc/{}", $unary_route)),
                Some($unary_capability_name),
            );)+
            $(assert_eq!(
                super::capability_of(&format!("/ployz.rpc.v1.MachineRpc/{}", $stream_route)),
                Some($stream_capability_name),
            );)+
        };
    }

    #[test]
    fn capability_of_names_every_catalogued_route() {
        crate::rpc_catalog!(assert_capability_of);
        assert_eq!(
            super::capability_of("/ployz.rpc.v1.MachineRpc/Exec"),
            Some("ployz.container.exec.v1")
        );
        assert_eq!(
            super::capability_of("/ployz.rpc.v1.MachineRpc/Build"),
            Some("ployz.build.v1")
        );
        assert_eq!(
            super::capability_of("/ployz.rpc.v1.MachineRpc/SetVolumeCopy"),
            Some("ployz.volume.copy.set.v1")
        );
        for path in [
            "/ployz.rpc.v1.MachineRpc/Nope",
            "/other.Service/Inspect",
            "",
        ] {
            assert_eq!(super::capability_of(path), None);
        }
    }
}

#[cfg(test)]
mod not_pointer_publisher_tests {
    use super::*;

    #[test]
    fn only_an_unavailable_error_tagged_as_the_refusal_redirects() {
        let refusal = NotPointerPublisher {
            publisher: MachineId::parse("1".repeat(32)).unwrap(),
        };
        let error = refusal.rpc_error();
        assert_eq!(
            NotPointerPublisher::from_error(&error),
            Some(refusal.clone())
        );

        let untagged = RpcError {
            details: serde_json::json!({ "publisher": refusal.publisher }),
            ..error.clone()
        };
        assert_eq!(NotPointerPublisher::from_error(&untagged), None);

        let other_code = RpcError {
            code: RpcErrorCode::Conflict,
            ..error
        };
        assert_eq!(NotPointerPublisher::from_error(&other_code), None);
    }
}
