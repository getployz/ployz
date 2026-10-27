use crate::{
    InspectVolumeCopyRequest, SeedVolumeRequest, SetVolumeCopyReply, SetVolumeCopyRequest,
    SetVolumeSizeReply, SetVolumeSizeRequest, StorageCapacity, VolumeCopyView,
    metrics::{MetricHistoryReply, MetricHistoryRequest},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::IpAddr,
};
use ts_rs::TS;

mod build_grant;
mod sealing;
mod sealing_recovery;
mod upgrade;
pub use build_grant::*;
pub use sealing::*;
pub use sealing_recovery::*;
pub use upgrade::*;

use ipnet::Ipv4Net;
use prost::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;

use crate::{
    AdvertisedEndpoint, CapabilityName, CertificateHost, ContainerAddress, ContainerId,
    ContainerKind, ContainerObservation, DeviceCode, DeviceKey, Machine, MachineId,
    MachineLogService, MachineName, MachineObservation, MachineRuntime, MachineUpdate,
    ManagementCapability, ManagementClientLabel, Namespace, PinnedAddresses, PullPolicy,
    QualifiedService, ResolvedServiceSpec, SelectedEndpoint, StorageChoice, WireGuardPublicKey,
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

/// What a Management Client may call on a Machine. A slot is `Full` only when its label
/// is the [`DeviceCode`](crate::DeviceCode) label of the key it holds; `cloud`, every
/// `cli-*` slot Cloud minted and any other label are `Operator`. The local socket is `Full`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ManagementScope {
    /// Drives Deploys and reads state, but never execs into a container, opens a
    /// tunnel, or reads back an env value the Machine opened from Sealed ciphertext.
    Operator,
    Full,
}

impl ManagementScope {
    #[must_use]
    pub fn of(label: &crate::ManagementClientLabel, key: &[u8; 32]) -> Self {
        if *label == DeviceKey::from_bytes(*key).label() {
            Self::Full
        } else {
            Self::Operator
        }
    }
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
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
        bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
        capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
    ) => {
        $(pub const $unary_capability: &str = $unary_capability_name;)+
        $(pub const $stream_capability: &str = $stream_capability_name;)+
        $(pub const $bidi_capability: &str = $bidi_capability_name;)+
        $($(#[$extra_meta])* pub const $extra_capability: &str = $extra_capability_name;)+

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
                $(concat!("/", $package, ".MachineRpc/", $bidi_route) => Some($bidi_capability),)+
                _ => None,
            }
        }

        /// The least scope that may call the route at `path`, or `None` for an uncatalogued path.
        #[must_use]
        pub fn scope_of(path: &str) -> Option<ManagementScope> {
            match path {
                $(concat!("/", $package, ".MachineRpc/", $unary_route) => Some(ManagementScope::$unary_scope),)+
                $(concat!("/", $package, ".MachineRpc/", $stream_route) => Some(ManagementScope::$stream_scope),)+
                $(concat!("/", $package, ".MachineRpc/", $bidi_route) => Some(ManagementScope::$bidi_scope),)+
                _ => None,
            }
        }

        const CATALOGUED_CAPABILITIES: &[(&str, CapabilityAdvertisement)] = &[
            $(($unary_capability, CapabilityAdvertisement::$unary_advertisement),)+
            $(($stream_capability, CapabilityAdvertisement::$stream_advertisement),)+
            $(($bidi_capability, CapabilityAdvertisement::$bidi_advertisement),)+
            $(($extra_capability, CapabilityAdvertisement::$extra_advertisement),)+
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

/// The HTTP/2 window each Machine RPC call advertises, at both ends. A keepalive ping
/// or its answer waits behind a call's unread bytes on a slow link, so one call survives
/// a link down to this over the client's keepalive timeout: about 17 KB/s. It also caps
/// one call's throughput at this per round trip: about 80 Mbit/s at 100 ms.
pub const RPC_STREAM_WINDOW_BYTES: u32 = 1024 * 1024;

/// The HTTP/2 window a whole Machine RPC connection advertises, at both ends. A call its
/// reader left unread, such as a paused `logs -f`, holds its stream window of this until
/// read, so it must be larger: three paused calls fit and the next call still gets
/// through. Calls moving data at once queue up to this ahead of a ping, so together they
/// survive a link down to about 70 KB/s.
pub const RPC_CONNECTION_WINDOW_BYTES: u32 = 4 * 1024 * 1024;

const _: () = assert!(RPC_STREAM_WINDOW_BYTES < RPC_CONNECTION_WINDOW_BYTES);

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
    #[serde(default, skip_serializing_if = "PinnedAddresses::is_empty")]
    pub pinned: PinnedAddresses,
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

/// The env the Log Store kept for a Container, read when Docker no longer answers for it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetainedEnvRequest {
    pub container_id: ContainerId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RetainedEnvReply {
    /// `None` when the store holds no env for the Container; never copied into
    /// replicated observations.
    pub environment: Option<BTreeMap<String, String>>,
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

/// Approve a Device on this Machine, or remove one, at Full scope only.
///
/// `Approve` carries the Device's public key, never a secret; the Machine stores it in
/// the slot the key's [`DeviceCode`] names. Strict for the same reason as
/// [`SetManagementClientRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SetDeviceRequest {
    Approve { key: DeviceKey },
    Remove { code: DeviceCode },
}

/// What a [`SetDeviceRequest`] changed on this Machine. Each outcome is final, so a
/// rerun reports `AlreadyApproved` or `NotPresent` rather than failing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum DeviceSet {
    Approved { code: DeviceCode },
    AlreadyApproved { code: DeviceCode },
    Removed { code: DeviceCode },
    NotPresent { code: DeviceCode },
}

/// Where this Machine's observe process pushes crash events, or `None` to stop pushing.
///
/// Strict for the same reason as [`SetManagementClientRequest`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPushTargetRequest {
    pub url: Option<url::Url>,
}

/// The push target the Machine now holds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PushTargetSet {
    #[ts(type = "string | null")]
    pub url: Option<url::Url>,
}

/// An operator's endpoint for reaching `peer` from the Machine that takes the request.
///
/// It sets only that Machine's own Selected Endpoint; `peer`'s record is unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectEndpointRequest {
    pub peer: MachineId,
    pub endpoint: SelectedEndpoint,
}

/// The Machine that took a [`SelectEndpointRequest`] now dials `endpoint` for `peer`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EndpointSelected {
    pub peer: MachineId,
    pub endpoint: SelectedEndpoint,
}

/// The place one Container creation lands on a Machine. The same slot names the
/// same Container across resends; a different spec in it is an occupied slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SlotKey {
    pub namespace: Namespace,
    pub service: crate::ServiceName,
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
        rotation: RunRotation,
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
    /// Take the copy out of routing for good. The Container keeps running.
    Out {
        container: ContainerLocator,
        /// Allow the Service to lose its last routed copy. Without it, an In copy
        /// goes Out only while another In copy is acked by every live proxy.
        allow_empty: bool,
        /// The rotation the caller read; `None` checks nothing.
        expect: Option<crate::CopyRotation>,
    },
    /// Take the copy out of routing for `run`, which alone may route it again. The
    /// Container keeps running until a `Stopped` want stops it.
    Held {
        container: ContainerLocator,
        run: crate::DeploymentLogId,
        /// The rotation the caller read; `None` checks nothing.
        expect: Option<crate::CopyRotation>,
    },
}

/// The rotation a `Running` want asks for.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunRotation {
    /// Write no rotation: a new copy stays Pending, an existing copy keeps its own.
    Pending,
    /// Route the copy once it is healthy. `run` is the run that holds it, when it is Held.
    In {
        run: Option<crate::DeploymentLogId>,
        /// The rotation the caller read; `None` checks nothing.
        expect: Option<crate::CopyRotation>,
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
    /// Orders this copy among its Service's copies; stamped as the `ployz.gen` label.
    pub r#gen: crate::Gen,
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
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImagePulled {
    /// The ports the image's config exposes, as Docker reports them. A Machine that
    /// doesn't report them sends none, which claims nothing about the image.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposed_ports: Vec<ExposedPort>,
}

/// One port an image's config exposes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct ExposedPort {
    pub port: std::num::NonZeroU16,
    pub protocol: crate::TransportProtocol,
}

impl ExposedPort {
    /// Read one of Docker's exposed-port keys, such as `80/tcp`. Answers `None` for a
    /// protocol Ployz doesn't route (sctp) and for anything it can't read.
    #[must_use]
    pub fn from_docker(key: &str) -> Option<Self> {
        let (port, protocol) = key.split_once('/').unwrap_or((key, "tcp"));
        let protocol = match protocol {
            "tcp" => crate::TransportProtocol::Tcp,
            "udp" => crate::TransportProtocol::Udp,
            _ => return None,
        };
        Some(Self {
            port: port.parse().ok()?,
            protocol,
        })
    }
}

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
        unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
        bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
        capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
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
            $(
                #[doc = concat!("The `", $bidi_command, "` stream.")]
                pub struct $bidi_variant;
            )+
        }

        $(
            impl op::$bidi_variant {
                pub const PATH: &'static str = concat!("/", $package, ".MachineRpc/", $bidi_route);
            }
        )+

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
    /// Absent from an issuer that holds no Sealing Key, or predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub sealing_key: Option<crate::SealedSealingKey>,
    /// The issuer's approved Devices, which a fresh joiner admits at Full scope.
    /// Absent from an issuer that predates Devices: the joiner starts with none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<crate::DeviceKey>>")]
    pub devices: Vec<crate::DeviceKey>,
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
        /// Docker OOM-killed a process of the Container's run; `false` from older daemons.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        oom_killed: bool,
        /// The Leave that was removing the Server as it answered, so what the copy did
        /// since was the Leave's doing. `None` from older daemons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        departing: Option<Departure>,
    },
    TimedOut {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        deadline_millis: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_check: Option<crate::LastHealthCheck>,
        /// Docker OOM-killed a process of the Container's run; `false` from older daemons.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        oom_killed: bool,
        /// The Leave that was removing the Server as it answered, so what the copy did
        /// since was the Leave's doing. `None` from older daemons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        departing: Option<Departure>,
    },
}

impl HealthVerdict {
    /// The Leave that was removing the Server as it answered for a failed Container.
    #[must_use]
    pub const fn departing(&self) -> Option<&Departure> {
        match self {
            Self::Healthy => None,
            Self::Failed { departing, .. } | Self::TimedOut { departing, .. } => departing.as_ref(),
        }
    }

    /// Whether Docker OOM-killed a process of the failed Container's run.
    #[must_use]
    pub const fn oom_killed(&self) -> bool {
        match self {
            Self::Healthy => false,
            Self::Failed { oom_killed, .. } | Self::TimedOut { oom_killed, .. } => *oom_killed,
        }
    }

    /// Why the Container is not serving, or `None` when it is.
    #[must_use]
    pub fn into_failure(self) -> Option<crate::HealthFailure> {
        match self {
            Self::Healthy => None,
            Self::Failed {
                observation,
                last_check,
                ..
            } => Some(crate::HealthFailure::Runtime {
                observation,
                last_check,
            }),
            Self::TimedOut {
                deadline_millis,
                last_check,
                ..
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
            ContainerRefusalReason::NotParticipating
            | ContainerRefusalReason::SealingKeyMissing => RpcErrorCode::Unavailable,
            ContainerRefusalReason::InvalidIngress
            | ContainerRefusalReason::SealedValueUnopenable => RpcErrorCode::InvalidArgument,
            ContainerRefusalReason::NotEligible
            | ContainerRefusalReason::SlotOccupied
            | ContainerRefusalReason::VolumesNotPrepared
            | ContainerRefusalReason::LastIn
            | ContainerRefusalReason::OlderThanIn
            | ContainerRefusalReason::HeldByOtherRun
            | ContainerRefusalReason::RotationMismatch
            | ContainerRefusalReason::InRotation => RpcErrorCode::Conflict,
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
    /// No other In copy of the Service is acked by every live proxy.
    LastIn,
    /// A newer copy of the Service is already In.
    OlderThanIn,
    HeldByOtherRun,
    /// The copy's rotation is not the one the want expected, or no move leads from it.
    RotationMismatch,
    /// The copy is routed, or held by a run that may route it again.
    InRotation,
    /// This Machine holds no Sealing Key to open the spec's Sealed values.
    SealingKeyMissing,
    /// A Sealed value is bound elsewhere or does not open with this Cluster's key.
    SealedValueUnopenable,
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

/// Read which copies of one Service each live proxy routes, from the target
/// Machine's replicated store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadRoutesRequest {
    pub service: QualifiedService,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoutesRead {
    /// Each Machine not reported Down and the copies its proxy acks. `None` when its
    /// row for the Service has not arrived or does not decode: unknown, not empty.
    #[serde(default)]
    pub routes: BTreeMap<MachineId, Option<BTreeSet<ContainerId>>>,
    /// The Machine list replicated in full, so `routes` names every live Machine.
    #[serde(default)]
    pub complete: bool,
    /// Machines the membership sample reported Down; their routes no longer count.
    #[serde(default)]
    pub down: BTreeSet<MachineId>,
}

/// Read the address this Machine's proxy routes each copy at.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadRoutedAddressesRequest {}

/// What this Machine's proxy serves, per Service and copy. `None` when this daemon
/// can't confirm what its proxy serves, such as before its first load or after it
/// wrote a config the proxy may or may not have read.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoutedAddresses {
    #[serde(default)]
    pub routes: Option<BTreeMap<QualifiedService, BTreeMap<ContainerId, ContainerAddress>>>,
}

/// A listed Machine whose proxy has not acked the copies a reader waits for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unacked {
    /// Its row arrived without them.
    Stale(MachineId),
    /// Its row has not arrived or does not decode.
    Unanswered(MachineId),
}

impl Unacked {
    #[must_use]
    pub const fn machine(&self) -> MachineId {
        match self {
            Self::Stale(machine) | Self::Unanswered(machine) => *machine,
        }
    }
}

impl RoutesRead {
    /// Each listed Machine outside this read's `down` whose proxy acks no copies
    /// `satisfied` accepts.
    pub fn unacked<'read>(
        &'read self,
        satisfied: impl Fn(&BTreeSet<ContainerId>) -> bool + 'read,
    ) -> impl Iterator<Item = Unacked> + 'read {
        self.routes
            .iter()
            .filter(|(machine, _)| !self.down.contains(*machine))
            .filter_map(move |(machine, routed)| match routed {
                None => Some(Unacked::Unanswered(*machine)),
                Some(routed) if !satisfied(routed) => Some(Unacked::Stale(*machine)),
                Some(_) => None,
            })
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
    SetVolumeSizeReply(SetVolumeSizeReply) => "set_volume_size_reply";
    MachineDetails(MachineDetails) => "machine_details";
    Enrolled(Enrolled) => "enrolled";
    SealingKeyWrapped(SealingKeyWrapped) => "sealing_key_wrapped";
    SealingKeyRecovered(SealingKeyRecovered) => "sealing_key_recovered";
    Registered(Registered) => "registered";
    SetManagementClientResponse(SetManagementClientResponse) => "management_client_set";
    DeviceSet(DeviceSet) => "device_set";
    PushTargetSet(PushTargetSet) => "push_target_set";
    EndpointSelected(EndpointSelected) => "endpoint_selected";
    MachineList(MachineList) => "machine_list";
    ContainerList(ContainerList) => "container_list";
    ContainerDetails(ContainerDetails) => "container_details";
    EnsuredContainer(EnsuredContainer) => "ensured_container";
    StorageCapacity(crate::StorageCapacity) => "storage_capacity";
    PreparedVolumes(PreparedVolumes) => "prepared_volumes";
    VolumeInventory(VolumeInventory) => "volume_inventory";
    VolumeRemoved(VolumeRemoved) => "volume_removed";
    ContainerLimitsUpdated(ContainerLimitsUpdated) => "container_limits_updated";
    VolumeCopyView(crate::VolumeCopyView) => "volume_copy_view";
    MachineImages(MachineImages) => "machine_images";
    ImageIngestOpened(ImageIngestOpened) => "image_ingest_opened";
    ImagePulled(ImagePulled) => "image_pulled";
    ImagesRemoved(ImagesRemoved) => "images_removed";
    BuildGrantMinted(BuildGrantMinted) => "build_grant_minted";
    BuildGrantInspected(BuildGrantInspected) => "build_grant_inspected";
    CertificateMaterialPublished(CertificateMaterialPublished) => "certificate_material_published";
    RoutesRead(RoutesRead) => "routes_read";
    RoutedAddresses(RoutedAddresses) => "routed_addresses";
    MachineUpdated(MachineUpdated) => "machine_updated";
    MachineUpgradeAttempt(MachineUpgradeAttempt) => "machine_upgrade_attempt";
    SealingKeyHeld(SealingKeyHeld) => "sealing_key_held";
    SealingKeyShared(SealingKeyShared) => "sealing_key_shared";
    Left(Left) => "left";
    Forgotten(Forgotten) => "forgotten";
    MetricHistoryReply(MetricHistoryReply) => "metric_history";
    RetainedEnvReply(RetainedEnvReply) => "retained_env";
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

/// How a client may repeat a call that failed. Clients decide this; it never
/// crosses the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// Transient: repeat soon, within the call's own budget.
    Now,
    /// The daemon did not serve the call: repeat only where the caller waits
    /// out a restart.
    AfterRestart,
    Never,
}

impl RpcErrorCode {
    #[must_use]
    pub fn retry(&self) -> Retry {
        match self {
            Self::Unavailable => Retry::Now,
            Self::InvalidArgument
            | Self::NotFound
            | Self::Ambiguous
            | Self::Unsupported
            | Self::Conflict
            | Self::Internal
            | Self::Unauthenticated
            | Self::ConfirmationRequired
            | Self::Unknown(_) => Retry::Never,
        }
    }
}

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

    /// An error the caller made because its target Machine didn't answer: the target
    /// sent nothing, so it may answer later. Read back by [`Self::target_unanswered`].
    #[must_use]
    pub fn unanswered_target(code: RpcErrorCode, message: String) -> Self {
        Self {
            code,
            message,
            details: serde_json::json!({ "reason": TARGET_UNANSWERED_REASON }),
            cause: Vec::new(),
        }
    }

    /// `self` from a call that failed while a Leave was removing `server`, so its copy
    /// was the Leave's to stop. Code, message and every other detail stay as they were.
    /// Read back by [`Self::departure`]. Details that aren't an object are kept unmarked.
    #[must_use]
    pub fn overlapping_leave(mut self, server: &crate::MachineName) -> Self {
        if self.details.is_null() {
            self.details = Value::Object(serde_json::Map::new());
        }
        if let Value::Object(details) = &mut self.details {
            details.insert(
                DEPARTING_KEY.into(),
                serde_json::json!(Departure {
                    server: server.clone()
                }),
            );
        }
        self
    }

    /// The Leave that overlapped the call this error answers, if one did.
    #[must_use]
    pub fn departure(&self) -> Option<Departure> {
        Departure::deserialize(self.details.get(DEPARTING_KEY)?).ok()
    }

    /// `self` from a call to `machine_id` whose connection dropped after it went out and
    /// before an answer came back, so the target may have done the work. Read back by
    /// [`Self::reply_lost`]. Details that aren't an object are kept unmarked.
    #[must_use]
    pub fn reply_lost_from(mut self, machine_id: &crate::MachineId) -> Self {
        if self.details.is_null() {
            self.details = Value::Object(serde_json::Map::new());
        }
        if let Value::Object(details) = &mut self.details {
            details.insert(REPLY_LOST_KEY.into(), Value::Bool(true));
            details
                .entry("machine_id")
                .or_insert_with(|| Value::String(machine_id.to_string()));
        }
        self
    }

    /// `self` from a call that never reached its target, so the target did nothing with
    /// it. Read back by [`Self::never_reached`]. Details that aren't an object are kept
    /// unmarked.
    #[must_use]
    pub fn unreached(mut self) -> Self {
        if self.details.is_null() {
            self.details = Value::Object(serde_json::Map::new());
        }
        if let Value::Object(details) = &mut self.details {
            details.insert(REACHED_KEY.into(), Value::Bool(false));
        }
        self
    }

    /// `self`, an [`Self::unanswered_target`] error from a router that couldn't dial the
    /// target, so only small packets went its way. Read back by [`Self::undialed`].
    #[must_use]
    pub fn undialed_target(mut self) -> Self {
        if let Value::Object(details) = &mut self.details {
            details.insert(UNDIALED_KEY.into(), Value::Bool(true));
        }
        self
    }

    /// Whether the router couldn't dial the unanswered target.
    #[must_use]
    pub fn undialed(&self) -> bool {
        self.details.get(UNDIALED_KEY).and_then(Value::as_bool) == Some(true)
    }

    /// Whether the call never reached its target.
    #[must_use]
    pub fn never_reached(&self) -> bool {
        self.details.get(REACHED_KEY).and_then(Value::as_bool) == Some(false)
    }

    /// Whether the connection dropped before the target's answer came back, so the
    /// target may have done the work.
    #[must_use]
    pub fn reply_lost(&self) -> bool {
        self.details.get(REPLY_LOST_KEY).and_then(Value::as_bool) == Some(true)
    }

    /// Whether the target Machine didn't answer, rather than answering with this error.
    #[must_use]
    pub fn target_unanswered(&self) -> bool {
        self.details.get("reason").and_then(Value::as_str) == Some(TARGET_UNANSWERED_REASON)
    }

    /// Whether an entry Machine's routing refused this call because it no longer lists
    /// `machine_id`, as after `ployz server rm`. A daemon from before the tag, or a
    /// Server's own not-found answer, is not.
    #[must_use]
    pub fn server_unlisted(&self, machine_id: &crate::MachineId) -> bool {
        self.code == RpcErrorCode::NotFound
            && self.details.get("reason").and_then(Value::as_str) == Some(NO_SUCH_SERVER_REASON)
            && self
                .details
                .get("targets")
                .and_then(Value::as_array)
                .is_some_and(|targets| {
                    targets
                        .iter()
                        .any(|target| target.as_str() == Some(machine_id.as_str()))
                })
    }

    /// The capability this error says its target Machine lacks, if it is a refusal.
    #[must_use]
    pub fn refusal(&self) -> Option<CapabilityRefusal> {
        CapabilityRefusal::new(&self.code, self.details.clone())
    }

    /// `self` from a pull of `image` under `policy`, naming both when the answer is not
    /// found or the registry refused it. A registry answers not found for a missing tag
    /// and a private repository alike, and some, such as ghcr.io, answer `denied`.
    #[must_use]
    pub fn pulling(mut self, image: &str, policy: PullPolicy) -> Self {
        if self.details.is_null()
            && (self.code == RpcErrorCode::NotFound || self.registry_refused())
        {
            self.details = serde_json::json!({ MISSING_IMAGE: image, PULL_POLICY: policy });
        }
        self
    }

    /// The image a not-found or refused pull named through [`Self::pulling`].
    #[must_use]
    pub fn missing_image(&self) -> Option<MissingImage<'_>> {
        if self.code != RpcErrorCode::NotFound && !self.registry_refused() {
            return None;
        }
        Some(MissingImage {
            image: self.details.get(MISSING_IMAGE)?.as_str()?,
            policy: self
                .details
                .get(PULL_POLICY)
                .and_then(|policy| PullPolicy::deserialize(policy).ok()),
        })
    }

    /// The host a failed pull could not look up, from Docker's error: the registry,
    /// its token service or a proxy. None when Ployz lost the Machine itself.
    #[must_use]
    pub fn unresolved_host(&self) -> Option<&str> {
        if self.reply_lost() || self.never_reached() || self.target_unanswered() {
            return None;
        }
        self.lines().find_map(|line| {
            let (lookup, _) = line.split_once(": no such host")?;
            let (_, host) = lookup.rsplit_once("lookup ")?;
            host.split_whitespace().next()
        })
    }

    /// Whether the registry refused a pull for want of access, whatever status Docker gave.
    fn registry_refused(&self) -> bool {
        self.lines()
            .any(|line| REGISTRY_REFUSALS.iter().any(|sign| line.contains(sign)))
    }

    fn lines(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.message.as_str()).chain(self.cause.iter().map(String::as_str))
    }

    /// The full disk a failed pull ran into, as [`DiskFull::details`] named it.
    #[must_use]
    pub fn disk_full(&self) -> Option<DiskFull> {
        serde_json::from_value(self.details.get(DISK_FULL)?.clone()).ok()
    }
}

const MISSING_IMAGE: &str = "missing_image";
/// What Docker prints when a registry refuses a pull for want of access.
const REGISTRY_REFUSALS: [&str; 3] = [
    "error from registry: denied",
    "denied: requested access",
    "unauthorized: ",
];
const PULL_POLICY: &str = "pull_policy";
const DISK_FULL: &str = "disk_full";

/// The Leave that overlapped a call: what a Server's Containers did since it began is
/// the Leave's doing. Annotates a failure's details under [`DEPARTING_KEY`], an unhealthy
/// [`HealthVerdict`], and the `ExecutionError::Health` a deploy stores.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Departure {
    pub server: crate::MachineName,
}

/// An image a pull answered not found for, from [`RpcError::missing_image`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingImage<'a> {
    pub image: &'a str,
    /// The policy it was pulled under, when the error recorded one.
    pub policy: Option<PullPolicy>,
}

/// A pull failed while a disk it writes to had almost no free space.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiskFull {
    /// The directory on the full disk: the Docker root, or containerd's image store
    /// when that is the fuller. The wire keeps the name it shipped with.
    pub docker_root: String,
    pub free_bytes: u64,
}

impl DiskFull {
    /// [`RpcError`] details that [`RpcError::disk_full`] reads back.
    #[must_use]
    pub fn details(&self) -> Value {
        serde_json::json!({ DISK_FULL: self })
    }
}

impl DiskFull {
    /// The sentence naming `server`, such as "Server machine-3", for the Server.
    #[must_use]
    pub fn on(&self, server: &str) -> String {
        format!(
            "{server}'s disk is full: {} MB free under {}",
            self.free_bytes / 1_000_000,
            self.docker_root
        )
    }
}

impl fmt::Display for DiskFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.on("The Server"))
    }
}

/// A Machine refused a route because its Ployz lacks the route's capability.
#[derive(Clone, Debug, PartialEq)]
pub struct CapabilityRefusal {
    pub target: String,
    pub capability: String,
    pub details: Value,
}

impl CapabilityRefusal {
    /// The refusal `details` name: `Unsupported`, with string `target` and `capability`.
    #[must_use]
    pub fn new(code: &RpcErrorCode, details: Value) -> Option<Self> {
        if *code != RpcErrorCode::Unsupported {
            return None;
        }
        let target = details.get("target")?.as_str()?.to_owned();
        let capability = details.get("capability")?.as_str()?.to_owned();
        Some(Self {
            target,
            capability,
            details,
        })
    }

    #[must_use]
    pub fn naming(self, target: String) -> Self {
        Self { target, ..self }
    }
}

impl fmt::Display for CapabilityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Server {} needs a newer Ployz. It cannot serve {}.",
            self.target, self.capability
        )
    }
}

/// Metadata an entry Machine's router puts on a status it made because the target
/// Machine did not answer the forwarded call. A status the target sent never has it.
pub const TARGET_UNANSWERED_KEY: &str = "ployz-target-unanswered";

const TARGET_UNANSWERED_REASON: &str = "target_unanswered";
/// The `reason` an entry Machine's routing gives its not-found refusal of a Machine
/// Target it does not list, beside the `targets` it looked up. Read back by
/// [`RpcError::server_unlisted`].
pub const NO_SUCH_SERVER_REASON: &str = "no_such_server";
/// The details key an [`RpcError::overlapping_leave`] mark lives under.
pub const DEPARTING_KEY: &str = "departing";
const REPLY_LOST_KEY: &str = "reply_lost";
const REACHED_KEY: &str = "reached";
const UNDIALED_KEY: &str = "undialed";

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

/// Metadata an entry Machine's router adds to a [`TARGET_UNANSWERED_KEY`] status when its
/// dial to the target failed, so the target never got the call. Its absence says nothing.
pub const TARGET_UNREACHED_KEY: &str = "ployz-target-unreached";

/// [`target_unanswered_status`], also marked [`TARGET_UNREACHED_KEY`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn target_unreached_status(error: &(dyn std::error::Error + 'static)) -> tonic::Status {
    let mut status = target_unanswered_status(error);
    status.metadata_mut().insert(
        TARGET_UNREACHED_KEY,
        tonic::metadata::MetadataValue::from_static("1"),
    );
    status
}

/// The `reason` an entry Machine's router puts in the details of a status it made for a
/// target that did not answer, when its own path probes found that large packets to that
/// target are dropped. Read back by `ConnectError::large_packets_dropped` in the CLI.
pub const LARGE_PACKETS_DROPPED_REASON: &str = "large_packets_dropped";

/// `status`, a router's [`target_unanswered_status`], marked
/// [`LARGE_PACKETS_DROPPED_REASON`] with `sentence` as its deepest cause. Code, message
/// and metadata stay.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn large_packets_dropped_status(status: tonic::Status, sentence: String) -> tonic::Status {
    let mut details = match serde_json::from_slice::<Value>(status.details()) {
        Ok(Value::Object(details)) => details,
        _ => serde_json::Map::new(),
    };
    details.insert("reason".into(), LARGE_PACKETS_DROPPED_REASON.into());
    match details.get_mut("cause") {
        Some(Value::Array(causes)) => causes.push(sentence.into()),
        _ => {
            details.insert("cause".into(), Value::Array(vec![sentence.into()]));
        }
    }
    let details = serde_json::to_vec(&details).expect("a JSON object serializes");
    tonic::Status::with_details_and_metadata(
        status.code(),
        status.message(),
        details.into(),
        status.metadata().clone(),
    )
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
    /// A Leave's mark names its Server and leaves the rest of the failure as it was,
    /// and an unmarked Conflict reads back none.
    #[test]
    fn a_leave_marks_a_failure_without_changing_it() {
        let server = crate::MachineName::parse("lc-3").unwrap();
        let departure = Departure {
            server: server.clone(),
        };

        let failed = RpcError {
            code: RpcErrorCode::NotFound,
            message: "No such container".into(),
            details: json!({ "container": "b" }),
            cause: vec!["docker".into()],
        };
        let marked = failed.clone().overlapping_leave(&server);
        assert_eq!(marked.code, failed.code);
        assert_eq!(marked.message, failed.message);
        assert_eq!(marked.cause, failed.cause);
        assert_eq!(marked.details.get("container"), Some(&json!("b")));
        assert_eq!(marked.departure(), Some(departure));

        let held = RpcError {
            code: RpcErrorCode::Conflict,
            message: "slot held".into(),
            details: Value::Null,
            cause: Vec::new(),
        };
        assert_eq!(held.departure(), None);
    }

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
    fn a_routing_refusal_names_the_server_it_does_not_list() {
        let machine = crate::MachineId::parse("1".repeat(32)).unwrap();
        let other = crate::MachineId::parse("2".repeat(32)).unwrap();
        let unlisted = RpcError {
            code: RpcErrorCode::NotFound,
            message: format!("No Server named {:?}", machine.to_string()),
            details: json!({
                "inspect": ["ployz server ls"],
                "reason": NO_SUCH_SERVER_REASON,
                "targets": [machine.to_string()],
            }),
            cause: Vec::new(),
        };
        assert!(unlisted.server_unlisted(&machine));
        assert!(!unlisted.server_unlisted(&other));
        // A daemon from before the tag answers the same refusal without it.
        let untagged = RpcError {
            details: json!({ "inspect": ["ployz server ls"] }),
            ..unlisted.clone()
        };
        assert!(!untagged.server_unlisted(&machine));
        let answered = RpcError {
            code: RpcErrorCode::Internal,
            ..unlisted
        };
        assert!(!answered.server_unlisted(&machine));
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
    fn push_target_requests_carry_a_url_or_null_and_nothing_else() {
        let set = serde_json::from_value::<SetPushTargetRequest>(
            json!({ "url": "https://cloud.example/api/machines/aa/observe" }),
        )
        .unwrap();
        assert_eq!(
            set.url.map(String::from).as_deref(),
            Some("https://cloud.example/api/machines/aa/observe")
        );
        let clear = serde_json::from_value::<SetPushTargetRequest>(json!({ "url": null })).unwrap();
        assert_eq!(clear.url, None);
        for value in [
            json!({ "url": "not a url" }),
            json!({ "url": null, "token": "private" }),
        ] {
            assert!(serde_json::from_value::<SetPushTargetRequest>(value).is_err());
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
            pinned: Default::default(),
        })
        .unwrap();
        let member = serde_json::to_value(EnrollRequest {
            role: EnrollAs::Member {
                registration: Registered {
                    assigned_machine: machine(),
                    visible_peers: vec![machine()],
                    target_versions: BTreeMap::new(),
                    sealing_key: None,
                    devices: Vec::new(),
                },
            },
            wireguard_mtu: Some(1380),
            pinned: Default::default(),
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
            unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
            server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
            bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
            capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
        ) => {
            [$($unary_capability_name,)+ $($stream_capability_name,)+ $($bidi_capability_name,)+ $($extra_capability_name,)+]
        };
    }

    #[test]
    fn served_capabilities_cover_every_catalog_row() {
        let served = served_capabilities();
        let rows = crate::rpc_catalog!(catalog_capabilities);
        for name in rows {
            assert!(served.contains(name), "{name} is not served");
        }
        assert_eq!(served.len(), rows.len());
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
            unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
            server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
            bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
            capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
        ) => {
            $(assert_eq!(
                super::capability_of(&format!("/ployz.rpc.v1.MachineRpc/{}", $unary_route)),
                Some($unary_capability_name),
            );)+
            $(assert_eq!(
                super::capability_of(&format!("/ployz.rpc.v1.MachineRpc/{}", $stream_route)),
                Some($stream_capability_name),
            );)+
            $(assert_eq!(
                super::capability_of(&format!("/ployz.rpc.v1.MachineRpc/{}", $bidi_route)),
                Some($bidi_capability_name),
            );)+
        };
    }

    macro_rules! full_scope_routes {
        (
            package $package:literal
            unary { $($unary_variant:ident: ($unary_method:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident, $unary_scope:ident),)+ }
            server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident, $stream_scope:ident),)+ }
            bidi_streaming { $($bidi_variant:ident: ($bidi_method:ident, $bidi_route:literal, $bidi_command:literal, $bidi_capability:ident, $bidi_capability_name:literal, $bidi_advertisement:ident, $bidi_scope:ident),)+ }
            capabilities { $($(#[$extra_meta:meta])* $extra_capability:ident: ($extra_capability_name:literal, $extra_advertisement:ident),)+ }
        ) => {
            [$($unary_route,)+ $($stream_route,)+ $($bidi_route,)+]
                .into_iter()
                .filter(|route| {
                    super::scope_of(&format!("/ployz.rpc.v1.MachineRpc/{route}"))
                        .expect("every catalogued route has a scope")
                        == super::ManagementScope::Full
                })
                .collect::<Vec<_>>()
        };
    }

    #[test]
    fn only_exec_set_device_and_the_recovery_code_routes_need_full_scope() {
        assert_eq!(
            crate::rpc_catalog!(full_scope_routes),
            ["WrapSealingKey", "RecoverSealingKey", "SetDevice", "Exec"]
        );
        assert_eq!(super::scope_of("/ployz.rpc.v1.MachineRpc/Nope"), None);
    }

    #[test]
    fn only_a_device_slot_holding_its_own_key_is_full() {
        let key = [9; 32];
        let scope = |label: &str, key: &[u8; 32]| {
            super::ManagementScope::of(&crate::ManagementClientLabel::parse(label).unwrap(), key)
        };
        let device = crate::DeviceKey::from_bytes(key).label();
        assert_eq!(scope(device.as_str(), &key), super::ManagementScope::Full);
        assert_eq!(
            scope(device.as_str(), &[8; 32]),
            super::ManagementScope::Operator
        );
        for label in ["cloud", "cli-0123456789abcdef0123456789ab", "laptop"] {
            assert_eq!(scope(label, &key), super::ManagementScope::Operator);
        }
    }

    #[test]
    fn capability_of_names_every_catalogued_route() {
        crate::rpc_catalog!(assert_capability_of);
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
