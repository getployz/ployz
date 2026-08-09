//! Typed documents stored in the v1 Corrosion tables.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU16;

use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

use crate::deploy::{EnvValue, ImageReference, ReplicaSlot};
use crate::ids::{
    ClusterId, MachineRowId, NamespaceRowId, OperationRowId, RouteBindingRowId, ServiceRowId,
};
use crate::ingress::RouteBindingOrigin;
use crate::machine::{GatewayProcessHealth, GatewayServingStatus, MachineLifecycle, MachineName};
use crate::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use crate::operation::{RouteHostname, RoutePort};

use super::controller::ControllerRevision;
use super::mesh::{BuiltinWireguardKeyMismatch, BuiltinWireguardMemberAddress};
use super::operation::CorrosionDeployState;
use super::principal::OperationInitiator;

/// A table in the additive Corrosion schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CorrosionTable {
    Cluster,
    Machines,
    Peers,
    Tokens,
    Namespaces,
    Services,
    RouteBindings,
    Controller,
    Containers,
    MachineStatus,
    GatewayObservations,
    Operations,
    CertHoldings,
    AcmeHttp01,
}

impl CorrosionTable {
    /// Every table in schema order.
    pub const ALL: [Self; 14] = [
        Self::Cluster,
        Self::Machines,
        Self::Peers,
        Self::Tokens,
        Self::Namespaces,
        Self::Services,
        Self::RouteBindings,
        Self::Controller,
        Self::Containers,
        Self::MachineStatus,
        Self::GatewayObservations,
        Self::Operations,
        Self::CertHoldings,
        Self::AcmeHttp01,
    ];

    /// Returns the table's exact SQL name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cluster => "cluster",
            Self::Machines => "machines",
            Self::Peers => "peers",
            Self::Tokens => "tokens",
            Self::Namespaces => "namespaces",
            Self::Services => "services",
            Self::RouteBindings => "route_bindings",
            Self::Controller => "controller",
            Self::Containers => "containers",
            Self::MachineStatus => "machine_status",
            Self::GatewayObservations => "gateway_observations",
            Self::Operations => "operations",
            Self::CertHoldings => "cert_holdings",
            Self::AcmeHttp01 => "acme_http01",
        }
    }
}

/// The additive JSON document version carried by every Corrosion row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"CorrosionDocumentVersion\">"))]
#[serde(transparent)]
pub struct CorrosionDocumentVersion(u32);

impl CorrosionDocumentVersion {
    pub const V1: Self = Self(1);

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A validated instant serialized in one lexically sortable UTC spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"CorrosionTimestamp\">"))]
#[serde(try_from = "String", into = "String")]
pub struct CorrosionTimestamp(OffsetDateTime);

impl CorrosionTimestamp {
    /// Parses any RFC 3339 offset spelling and stores the corresponding UTC instant.
    pub fn try_new(value: impl Into<String>) -> Result<Self, CorrosionTimestampError> {
        let value = value.into();
        let parsed =
            OffsetDateTime::parse(&value, &Rfc3339).map_err(|error| CorrosionTimestampError {
                value,
                message: error.to_string(),
            })?;
        Ok(Self(parsed.to_offset(UtcOffset::UTC)))
    }

    /// The non-negative duration from `earlier` to this instant; zero when
    /// this instant does not follow it.
    #[must_use]
    pub fn saturating_since(self, earlier: Self) -> std::time::Duration {
        std::time::Duration::try_from(self.0 - earlier.0).unwrap_or(std::time::Duration::ZERO)
    }

    fn canonical_string(self) -> String {
        let timestamp = self.0;
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
            timestamp.year(),
            u8::from(timestamp.month()),
            timestamp.day(),
            timestamp.hour(),
            timestamp.minute(),
            timestamp.second(),
            timestamp.nanosecond(),
        )
    }
}

impl fmt::Display for CorrosionTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.canonical_string())
    }
}

impl TryFrom<String> for CorrosionTimestamp {
    type Error = CorrosionTimestampError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CorrosionTimestamp> for String {
    fn from(value: CorrosionTimestamp) -> Self {
        value.canonical_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid Corrosion timestamp {value}: {message}")]
pub struct CorrosionTimestampError {
    pub value: String,
    pub message: String,
}

/// A sealed typed document from one fixed Corrosion table.
pub trait CorrosionDocument: private::Sealed + Serialize + DeserializeOwned {
    const TABLE: CorrosionTable;
    const SUPPORTED_VERSION: CorrosionDocumentVersion = CorrosionDocumentVersion::V1;

    fn version(&self) -> CorrosionDocumentVersion;
    fn cluster_id(&self) -> &ClusterId;
}

/// A non-roster document accepted through the ordinary cluster/version fence.
pub trait OrdinaryCorrosionDocument:
    CorrosionDocument + private::OrdinaryCorrosionDocumentSealed
{
}

/// A machine or peer document whose transport must match the cluster provider.
pub trait RosterCorrosionDocument:
    CorrosionDocument + private::RosterCorrosionDocumentSealed
{
    fn mesh_provider(&self) -> MeshProvider;
}

/// The collision domain of one named operator-authority document.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "table", rename_all = "snake_case")]
pub enum NameClaim {
    Machine {
        name: String,
    },
    Peer {
        name: String,
    },
    Namespace {
        name: String,
    },
    Service {
        namespace_id: NamespaceRowId,
        name: String,
    },
    RouteBinding {
        hostname: RouteHostname,
    },
}

/// A document whose human handle is resolved by the lowest-ULID reader law.
pub trait NamedCorrosionDocument: CorrosionDocument {
    fn name_claim(&self) -> NameClaim;
}

mod private {
    pub trait Sealed {}
    pub trait OrdinaryCorrosionDocumentSealed {}
    pub trait RosterCorrosionDocumentSealed {}
}

/// A lowercase hexadecimal SHA-256 digest that cannot carry source secrets.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"Sha256Hex\">"))]
#[serde(try_from = "String", into = "String")]
pub struct Sha256Hex(String);

impl Sha256Hex {
    pub const TEXT_LENGTH: usize = 64;

    pub fn try_new(value: impl Into<String>) -> Result<Self, Sha256HexError> {
        let value = value.into();
        if value.len() != Self::TEXT_LENGTH {
            return Err(Sha256HexError::InvalidLength {
                value,
                expected: Self::TEXT_LENGTH,
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(Sha256HexError::InvalidCharacter { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Sha256Hex {
    type Error = Sha256HexError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<Sha256Hex> for String {
    fn from(value: Sha256Hex) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Sha256HexError {
    #[error("SHA-256 digest must contain exactly {expected} characters: {value}")]
    InvalidLength { value: String, expected: usize },
    #[error("SHA-256 digest must contain only lowercase hexadecimal characters: {value}")]
    InvalidCharacter { value: String },
}

/// Computes the row-safe identity of an environment value's JSON representation.
pub fn fingerprint_env_value(value: &EnvValue) -> Result<Sha256Hex, serde_json::Error> {
    let encoded = serde_json::to_vec(value)?;
    Ok(Sha256Hex(format!("{:x}", Sha256::digest(encoded))))
}

macro_rules! corrosion_dns_label {
    (
        $(#[$meta:meta])*
        pub struct $name:ident;
        error: $error:ident;
        brand: $brand:literal;
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[cfg_attr(feature = "ts", ts(type = $brand))]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub const MAX_BYTES: usize = 63;

            pub fn try_new(value: impl Into<String>) -> Result<Self, $error> {
                let value = value.into();
                if value.is_empty() {
                    return Err($error::Empty);
                }
                if value.len() > Self::MAX_BYTES {
                    return Err($error::TooLong {
                        value,
                        maximum: Self::MAX_BYTES,
                    });
                }
                if value.starts_with('-') || value.ends_with('-') {
                    return Err($error::EdgeHyphen { value });
                }
                if !value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                {
                    return Err($error::InvalidCharacter { value });
                }
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::ops::Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
        pub enum $error {
            #[error("DNS label is empty")]
            Empty,
            #[error("DNS label exceeds {maximum} bytes: {value}")]
            TooLong { value: String, maximum: usize },
            #[error("DNS label cannot begin or end with '-': {value}")]
            EdgeHyphen { value: String },
            #[error("DNS label must contain only lowercase ASCII letters, digits, or '-': {value}")]
            InvalidCharacter { value: String },
        }
    };
}

corrosion_dns_label! {
    /// A namespace handle valid as one label beneath `.internal`.
    pub struct CorrosionNamespaceName;
    error: CorrosionNamespaceNameError;
    brand: "Brand<string, \"CorrosionNamespaceName\">";
}

corrosion_dns_label! {
    /// A service handle valid as one label beneath its namespace.
    pub struct CorrosionServiceName;
    error: CorrosionServiceNameError;
    brand: "Brand<string, \"CorrosionServiceName\">";
}

/// Cluster-wide storage default selected during init.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    Plain,
    Zfs,
}

/// Cluster-wide automatic-hostname choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AutomaticHostnameMode {
    #[serde(alias = "ployz")]
    Disabled,
    Custom {
        suffix: RouteHostname,
    },
}

/// Mesh implementation fixed for the life of a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MeshProvider {
    BuiltinWireguard,
    Tailscale,
}

/// A machine's mesh identity plus its required container subnet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineTransport {
    Wireguard {
        pubkey: WireGuardPublicKey,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        addr_v6: Ipv6Addr,
        #[cfg_attr(feature = "ts", ts(type = "string | null"))]
        endpoint: Option<SocketAddr>,
        subnet_v4: MachineEndpointSubnet,
    },
    Tailscale {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        ip: Ipv4Addr,
        subnet_v4: MachineEndpointSubnet,
    },
}

impl MachineTransport {
    const fn mesh_provider(&self) -> MeshProvider {
        match self {
            Self::Wireguard { .. } => MeshProvider::BuiltinWireguard,
            Self::Tailscale { .. } => MeshProvider::Tailscale,
        }
    }
}

/// A peer's mesh identity; peers never own container address space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerTransport {
    Wireguard {
        pubkey: WireGuardPublicKey,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        addr_v6: Ipv6Addr,
        #[cfg_attr(feature = "ts", ts(type = "string | null"))]
        endpoint: Option<SocketAddr>,
    },
    Tailscale {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        ip: Ipv4Addr,
    },
}

impl PeerTransport {
    const fn mesh_provider(&self) -> MeshProvider {
        match self {
            Self::Wireguard { .. } => MeshProvider::BuiltinWireguard,
            Self::Tailscale { .. } => MeshProvider::Tailscale,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PeerTransportRepresentation {
    Wireguard {
        pubkey: WireGuardPublicKey,
        addr_v6: Ipv6Addr,
        endpoint: Option<SocketAddr>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    Tailscale {
        ip: Ipv4Addr,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

impl<'de> Deserialize<'de> for PeerTransport {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let representation = PeerTransportRepresentation::deserialize(deserializer)?;
        match representation {
            PeerTransportRepresentation::Wireguard {
                pubkey,
                addr_v6,
                endpoint,
                extra,
            } => {
                reject_peer_subnet::<Deserializer::Error>(&extra)?;
                Ok(Self::Wireguard {
                    pubkey,
                    addr_v6,
                    endpoint,
                })
            }
            PeerTransportRepresentation::Tailscale { ip, extra } => {
                reject_peer_subnet::<Deserializer::Error>(&extra)?;
                Ok(Self::Tailscale { ip })
            }
        }
    }
}

fn reject_peer_subnet<Error>(extra: &BTreeMap<String, serde_json::Value>) -> Result<(), Error>
where
    Error: serde::de::Error,
{
    if extra.contains_key("subnet_v4") {
        return Err(Error::custom("peer transport cannot carry subnet_v4"));
    }
    Ok(())
}

/// Why admission chose a machine's storage mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineStorageSelectionReason {
    Default,
    Flag,
    Ineligible {
        reason: MachineStorageIneligibleReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineStorageIneligibleReason {
    LowRam,
}

/// The durable storage outcome recorded at machine admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineStorageSelection {
    pub mode: StorageMode,
    pub reason: MachineStorageSelectionReason,
}

/// Service placement intent, with per-mode data present only when meaningful.
///
/// Host-published ports live only on the global variant: a replicated
/// service can never bind a host port, so port collisions across replicas
/// are unrepresentable instead of checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ServicePlacement {
    Replicated {
        replicas: ServiceReplicaCount,
    },
    Global {
        /// Host-published ports; an absent field reads as none published.
        #[serde(default, skip_serializing_if = "HostPortBindings::is_empty")]
        host_ports: HostPortBindings,
    },
}

/// The transport a host-published port forwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HostPortProtocol {
    Tcp,
    Udp,
}

/// One host-published port forwarded into a global service's container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct HostPortBinding {
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub host_port: NonZeroU16,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub container_port: NonZeroU16,
    pub protocol: HostPortProtocol,
}

/// A global service's host-published port set. One host port binds at most
/// once per protocol; the same number may forward both TCP and UDP. The set
/// backing makes equality order-insensitive and collapses an exact repeat of
/// the same binding.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Array<HostPortBinding>"))]
#[serde(try_from = "Vec<HostPortBinding>", into = "Vec<HostPortBinding>")]
pub struct HostPortBindings(BTreeSet<HostPortBinding>);

impl HostPortBindings {
    pub fn try_new(
        bindings: impl IntoIterator<Item = HostPortBinding>,
    ) -> Result<Self, HostPortBindingsError> {
        let bindings = bindings.into_iter().collect::<BTreeSet<_>>();
        let mut claimed = BTreeSet::new();
        for binding in &bindings {
            if !claimed.insert((binding.protocol, binding.host_port)) {
                return Err(HostPortBindingsError::DuplicateHostPort {
                    host_port: binding.host_port,
                    protocol: binding.protocol,
                });
            }
        }
        Ok(Self(bindings))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &HostPortBinding> {
        self.0.iter()
    }

    /// The number of published bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl TryFrom<Vec<HostPortBinding>> for HostPortBindings {
    type Error = HostPortBindingsError;

    fn try_from(value: Vec<HostPortBinding>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<HostPortBindings> for Vec<HostPortBinding> {
    fn from(value: HostPortBindings) -> Self {
        value.0.into_iter().collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HostPortBindingsError {
    #[error("host port {host_port} is published more than once for {protocol:?}")]
    DuplicateHostPort {
        host_port: NonZeroU16,
        protocol: HostPortProtocol,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"ServiceReplicaCount\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct ServiceReplicaCount(NonZeroU16);

impl ServiceReplicaCount {
    /// The small-cluster ceiling keeps one serial node preparation inside its
    /// fixed activity deadline.
    pub const MAX: u16 = 8;

    pub fn try_new(value: u16) -> Result<Self, ServiceReplicaCountError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(ServiceReplicaCountError::Zero);
        };
        if value.get() > Self::MAX {
            return Err(ServiceReplicaCountError::AboveCeiling {
                requested: value.get(),
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for ServiceReplicaCount {
    type Error = ServiceReplicaCountError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ServiceReplicaCount> for u16 {
    fn from(value: ServiceReplicaCount) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServiceReplicaCountError {
    #[error("replicated services require at least one replica")]
    Zero,
    #[error(
        "replicated services support at most {max} replicas, got {requested}",
        max = ServiceReplicaCount::MAX
    )]
    AboveCeiling { requested: u16 },
}

/// How a Route Binding reaches the gateway that terminates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum IngressMode {
    Direct,
    CloudflareTunnel,
    TailscaleFunnel,
}

/// Coarse point-in-time load testimony used by placement bids.
///
/// The variant order is the placement preference order: `Idle` sorts before
/// `Normal` before `Hot`, so a lower band wins the placement load tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineLoadBand {
    Idle,
    Normal,
    Hot,
}

/// The operator principal and instant responsible for one authority-row write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OperatorWriteProvenance {
    pub written_by: OperationInitiator,
    pub written_at: CorrosionTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ClusterDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub provenance: OperatorWriteProvenance,
    pub name: String,
    pub storage_default: StorageMode,
    pub hostname_mode: AutomaticHostnameMode,
    pub prefix: MachineEndpointSupernet,
    pub provider: MeshProvider,
    pub acme_directory_url: String,
    pub acme_contact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub provenance: OperatorWriteProvenance,
    pub name: MachineName,
    pub lifecycle: MachineLifecycle,
    pub transport: MachineTransport,
    pub storage: MachineStorageSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PeerDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub provenance: OperatorWriteProvenance,
    pub name: String,
    pub transport: PeerTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub provenance: OperatorWriteProvenance,
    pub secret_sha256: Sha256Hex,
    pub created_at: CorrosionTimestamp,
    pub expires_at: CorrosionTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NamespaceDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub provenance: OperatorWriteProvenance,
    pub name: CorrosionNamespaceName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ServiceDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub provenance: OperatorWriteProvenance,
    pub namespace_id: NamespaceRowId,
    pub name: CorrosionServiceName,
    pub image: ImageReference,
    pub env_fingerprints: BTreeMap<String, Sha256Hex>,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub placement: ServicePlacement,
    pub pinned_machines: BTreeSet<MachineRowId>,
    pub active_deploy: OperationRowId,
    pub previous_image: Option<ImageReference>,
    pub deployed_at: CorrosionTimestamp,
    pub operation_id: OperationRowId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RouteBindingDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub provenance: OperatorWriteProvenance,
    pub hostname: RouteHostname,
    pub service_id: ServiceRowId,
    pub namespace_id: NamespaceRowId,
    pub endpoint_port: RoutePort,
    pub origin: RouteBindingOrigin,
    pub ingress_mode: IngressMode,
}

/// The cluster's current preferred-controller appointment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ControllerDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterName,
    pub preferred_machine_id: MachineName,
    pub appointment_id: ControllerRevision,
    #[serde(default = "ancient_controller_heartbeat")]
    pub heartbeat_at: CorrosionTimestamp,
}

fn ancient_controller_heartbeat() -> CorrosionTimestamp {
    CorrosionTimestamp::try_new("1970-01-01T00:00:00Z")
        .expect("the Unix epoch is a valid Corrosion timestamp")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ContainerDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineRowId,
    pub service_id: ServiceRowId,
    pub namespace_id: NamespaceRowId,
    /// Stable replica identity used to authorize logs and exact retirement.
    /// Rows written before replica slots existed represented global services.
    #[serde(default = "global_replica_slot")]
    pub replica_slot: ReplicaSlot,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub ip: Ipv4Addr,
    pub deploy: OperationRowId,
}

const fn global_replica_slot() -> ReplicaSlot {
    ReplicaSlot::Global
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineStatusDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineRowId,
    pub ployz_version: String,
    pub corrosion_version: String,
    pub architecture: String,
    pub free_disk_bytes: u64,
    pub free_memory_bytes: u64,
    pub load: MachineLoadBand,
    pub observed_at: CorrosionTimestamp,
    /// `None` denotes an additive v1 status document without mesh testimony.
    /// Keeper mesh status writes populate this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<MeshConvergenceTestimony>,
    /// `None` denotes a status document written before isolation testimony.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_isolation: Option<ContainerIsolationTestimony>,
    /// `None` denotes a row written before live peer-handshake testimony existed.
    /// A current writer publishes `Some`, including an empty map on a one-machine roster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_handshakes: Option<BTreeMap<MachineRowId, WireGuardHandshakeEvidence>>,
}

/// Machine-owned testimony from one gateway process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct GatewayObservationDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineRowId,
    pub observed_at: CorrosionTimestamp,
    pub listen_addr: SocketAddr,
    pub serving: GatewayServingStatus,
    pub routes: Vec<GatewayRouteObservation>,
    pub aggregate_failures: Vec<GatewayProjectionAggregateFailure>,
    pub process_health: GatewayProcessHealth,
}

/// The latest local result for one valid Route Binding row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct GatewayRouteObservation {
    pub route_binding_id: RouteBindingRowId,
    pub hostname: RouteHostname,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub outcome: GatewayRouteProjectionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum GatewayRouteProjectionOutcome {
    Applied {
        availability: GatewayRouteAvailability,
    },
    Failed {
        failure: GatewayRouteProjectionFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GatewayRouteAvailability {
    Serving {
        upstream_count: usize,
    },
    Unavailable {
        reason: GatewayRouteUnavailableReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayRouteUnavailableReason {
    ServiceMissing,
    ServiceNamespaceMismatch,
    NoUpstream,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "failure", rename_all = "snake_case")]
pub enum GatewayRouteProjectionFailure {
    Shadowed {
        winner_route_binding_id: RouteBindingRowId,
    },
    UnsupportedIngressMode {
        ingress_mode: IngressMode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayProjectionInputKind {
    Cluster,
    Machines,
    Services,
    RouteBindings,
    Containers,
}

/// Aggregate evidence for rejected input rows that have no usable typed identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct GatewayProjectionAggregateFailure {
    pub input: GatewayProjectionInputKind,
    pub rejected_rows: usize,
}

/// The latest WireGuard handshake evidence observed for one remote machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WireGuardHandshakeEvidence {
    Never,
    At { unix_seconds: u64 },
}

/// Independent host prerequisites for the cgroup isolation wall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContainerIsolationDegradationReason {
    MissingControlProgram { path: String },
    MissingBytecode { path: String },
    MissingBpffs { path: String },
    MissingCgroupV2 { path: String },
    DesiredSetTooLarge { desired: usize, capacity: usize },
    HostEffect { message: String },
}

/// Keeper's durable testimony for the independently converged isolation wall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ContainerIsolationTestimony {
    Converged {
        attempted_at: CorrosionTimestamp,
        last_successful_converge: CorrosionTimestamp,
        entries: usize,
    },
    Degraded {
        attempted_at: CorrosionTimestamp,
        last_successful_converge: Option<CorrosionTimestamp>,
        reason: ContainerIsolationDegradationReason,
    },
}

/// Current successful evidence for one independently converged host component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MeshComponentReady {
    pub converged_at: CorrosionTimestamp,
}

/// Current failure evidence for one independently converged host component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MeshComponentDegraded {
    pub message: String,
}

/// Why a component was deliberately not attempted in the latest sequential fold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MeshNotAttemptedReason {
    DependencyDegraded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MeshComponentNotAttempted {
    pub reason: MeshNotAttemptedReason,
}

/// Typed eBPF failure classes needed by status and `doctor`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EbpfMeshDegradationReason {
    MissingBridge { ifname: String },
    HostEffect { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct EbpfMeshDegraded {
    pub reason: EbpfMeshDegradationReason,
}

/// A degraded testimony with the failing component combination encoded in its variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "components", rename_all = "snake_case")]
pub enum MeshDegradation {
    Wireguard {
        wireguard: MeshComponentDegraded,
        ebpf: MeshComponentNotAttempted,
    },
    Ebpf {
        wireguard: MeshComponentReady,
        ebpf: EbpfMeshDegraded,
    },
}

/// Keeper's durable testimony about its latest builtin mesh convergence attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MeshConvergenceTestimony {
    NoRoster {
        attempted_at: CorrosionTimestamp,
    },
    KeyMismatch {
        attempted_at: CorrosionTimestamp,
        mismatches: Vec<BuiltinWireguardKeyMismatch>,
    },
    Converged {
        bind_address: BuiltinWireguardMemberAddress,
        attempted_at: CorrosionTimestamp,
        last_successful_converge: CorrosionTimestamp,
        wireguard: MeshComponentReady,
        ebpf: MeshComponentReady,
    },
    Degraded {
        bind_address: BuiltinWireguardMemberAddress,
        attempted_at: CorrosionTimestamp,
        last_successful_converge: Option<CorrosionTimestamp>,
        degradation: MeshDegradation,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OperationDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineRowId,
    pub initiator: OperationInitiator,
    pub namespace_id: NamespaceRowId,
    pub service_id: ServiceRowId,
    pub created_at: CorrosionTimestamp,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub(super) state: CorrosionDeployState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CertHoldingDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineRowId,
    pub hostname: RouteHostname,
    pub fingerprint: Sha256Hex,
    pub issued_at: CorrosionTimestamp,
    pub expires_at: CorrosionTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcmeHttp01Document {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineRowId,
    pub hostname: RouteHostname,
    pub key_authorization: String,
    pub created_at: CorrosionTimestamp,
}

macro_rules! corrosion_document {
    ($document:ty, $table:expr) => {
        impl private::Sealed for $document {}

        impl CorrosionDocument for $document {
            const TABLE: CorrosionTable = $table;

            fn version(&self) -> CorrosionDocumentVersion {
                self.v
            }

            fn cluster_id(&self) -> &ClusterId {
                &self.cluster_id
            }
        }
    };
}

macro_rules! ordinary_corrosion_document {
    ($($document:ty),+ $(,)?) => {
        $(
            impl private::OrdinaryCorrosionDocumentSealed for $document {}
            impl OrdinaryCorrosionDocument for $document {}
        )+
    };
}

corrosion_document!(ClusterDocument, CorrosionTable::Cluster);
corrosion_document!(MachineDocument, CorrosionTable::Machines);
corrosion_document!(PeerDocument, CorrosionTable::Peers);
corrosion_document!(TokenDocument, CorrosionTable::Tokens);
corrosion_document!(NamespaceDocument, CorrosionTable::Namespaces);
corrosion_document!(ServiceDocument, CorrosionTable::Services);
corrosion_document!(RouteBindingDocument, CorrosionTable::RouteBindings);
corrosion_document!(ControllerDocument, CorrosionTable::Controller);
corrosion_document!(ContainerDocument, CorrosionTable::Containers);
corrosion_document!(MachineStatusDocument, CorrosionTable::MachineStatus);
corrosion_document!(
    GatewayObservationDocument,
    CorrosionTable::GatewayObservations
);
corrosion_document!(OperationDocument, CorrosionTable::Operations);
corrosion_document!(CertHoldingDocument, CorrosionTable::CertHoldings);
corrosion_document!(AcmeHttp01Document, CorrosionTable::AcmeHttp01);

ordinary_corrosion_document!(
    ClusterDocument,
    TokenDocument,
    NamespaceDocument,
    ServiceDocument,
    RouteBindingDocument,
    ControllerDocument,
    ContainerDocument,
    MachineStatusDocument,
    GatewayObservationDocument,
    OperationDocument,
    CertHoldingDocument,
    AcmeHttp01Document,
);

impl private::RosterCorrosionDocumentSealed for MachineDocument {}

impl RosterCorrosionDocument for MachineDocument {
    fn mesh_provider(&self) -> MeshProvider {
        self.transport.mesh_provider()
    }
}

impl private::RosterCorrosionDocumentSealed for PeerDocument {}

impl RosterCorrosionDocument for PeerDocument {
    fn mesh_provider(&self) -> MeshProvider {
        self.transport.mesh_provider()
    }
}

impl NamedCorrosionDocument for MachineDocument {
    fn name_claim(&self) -> NameClaim {
        NameClaim::Machine {
            name: self.name.as_str().to_owned(),
        }
    }
}

impl NamedCorrosionDocument for PeerDocument {
    fn name_claim(&self) -> NameClaim {
        NameClaim::Peer {
            name: self.name.clone(),
        }
    }
}

impl NamedCorrosionDocument for NamespaceDocument {
    fn name_claim(&self) -> NameClaim {
        NameClaim::Namespace {
            name: self.name.as_str().to_owned(),
        }
    }
}

impl NamedCorrosionDocument for ServiceDocument {
    fn name_claim(&self) -> NameClaim {
        NameClaim::Service {
            namespace_id: self.namespace_id.clone(),
            name: self.name.as_str().to_owned(),
        }
    }
}

impl NamedCorrosionDocument for RouteBindingDocument {
    fn name_claim(&self) -> NameClaim {
        NameClaim::RouteBinding {
            hostname: self.hostname.clone(),
        }
    }
}
