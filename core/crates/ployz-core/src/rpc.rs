use crate::{
    InspectVolumeCopyRequest, SeedVolumeRequest, SetVolumeCopyReply, SetVolumeCopyRequest,
    SetVolumeSizeReply, SetVolumeSizeRequest, StorageCapacity, VolumeCopyView,
    metrics::{MetricHistoryReply, MetricHistoryRequest},
};
use std::collections::BTreeSet;

mod activation;
mod build_grant;
#[cfg(test)]
mod catalog_tests;
mod container;
mod error;
mod evidence;
mod image;
mod ingress;
mod inspect;
mod logs;
mod machine;
mod management;
mod sealing;
mod sealing_recovery;
mod upgrade;
mod volume;
pub use activation::*;
pub use build_grant::*;
pub use container::*;
pub use error::*;
pub use evidence::*;
pub use image::*;
pub use ingress::*;
pub use inspect::*;
pub use logs::*;
pub use machine::*;
pub use management::*;
pub use sealing::*;
pub use sealing_recovery::*;
pub use upgrade::*;
pub use volume::*;

use prost::Message;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use serde_json::Value;

use crate::{CapabilityName, DeviceKey};

pub use crate::UNREGISTRY_PORT;

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
    pub fn of(label: &crate::ManagementClientLabel, key: &crate::ClientKey) -> Self {
        if *label == DeviceKey::from_bytes(*key.as_bytes()).label() {
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
#[expect(
    clippy::allow_attributes,
    clippy::allow_attributes_without_reason,
    reason = "tonic writes its own allows"
)]
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
        #[expect(clippy::large_enum_variant, reason = "boxing would make `decode` return `Box<T>`")]
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
    TrustedKeyRemoved(TrustedKeyRemoved) => "trusted_key_removed";
    PendingJoins(PendingJoins) => "pending_joins";
    JoinApproved(JoinApproved) => "join_approved";
    PushTargetSet(PushTargetSet) => "push_target_set";
    DeviceEnforcementSet(DeviceEnforcementSet) => "device_enforcement_set";
    RecipientKeySet(RecipientKeySet) => "recipient_key_set";
    ValuesResealed(ValuesResealed) => "values_resealed";
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
    IngressDoorsSet(IngressDoorsSet) => "ingress_doors_set";
    PrivateAccessSet(PrivateAccessSet) => "private_access_set";
    ClusterNamed(ClusterNamed) => "cluster_named";
    ClusterDoorsDeclared(ClusterDoorsDeclared) => "cluster_doors_declared";
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

#[cfg(test)]
mod set_management_client_wire {
    use std::collections::BTreeMap;

    use super::*;
    use crate::{
        AdvertisedEndpoint, Machine, MachineId, MachineName, ManagementCapability,
        ManagementClientLabel, WireGuardPublicKey,
    };
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
            join_approval: None,
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
                grant: None,
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
                    recipient: None,
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
    fn only_exec_trusted_key_changes_and_the_recovery_code_routes_need_full_scope() {
        assert_eq!(
            crate::rpc_catalog!(full_scope_routes),
            [
                "WrapSealingKey",
                "RecoverSealingKey",
                "SetDevice",
                "SetDeviceEnforcement",
                "SetPasskey",
                "RemoveTrustedKey",
                "Exec"
            ]
        );
        assert_eq!(super::scope_of("/ployz.rpc.v1.MachineRpc/Nope"), None);
    }

    #[test]
    fn only_a_device_slot_holding_its_own_key_is_full() {
        let key = crate::ClientKey::from_bytes([9; 32]);
        let scope = |label: &str, key: &crate::ClientKey| {
            super::ManagementScope::of(&crate::ManagementClientLabel::parse(label).unwrap(), key)
        };
        let device = crate::DeviceKey::from_bytes(*key.as_bytes()).label();
        assert_eq!(scope(device.as_str(), &key), super::ManagementScope::Full);
        assert_eq!(
            scope(device.as_str(), &crate::ClientKey::from_bytes([8; 32])),
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
