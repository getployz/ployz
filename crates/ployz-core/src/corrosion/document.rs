//! Typed documents stored in the v1 Corrosion tables.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU16;

use ipnet::Ipv4Net;

use crate::deploy::ImageReference;
use crate::ids::{ClusterId, MachineId, NamespaceId, OperationId, PeerId, ServiceId, TokenId};
use crate::ingress::RouteBindingOrigin;
use crate::machine::{MachineLifecycle, MachineName};
use crate::network::WireGuardPublicKey;
use crate::operation::{RouteHostname, RoutePort};

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
    Containers,
    MachineStatus,
    Operations,
    CertHoldings,
    AcmeHttp01,
}

impl CorrosionTable {
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
            Self::Containers => "containers",
            Self::MachineStatus => "machine_status",
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

/// A sealed typed document from one fixed Corrosion table.
pub trait CorrosionDocument: private::Sealed + Serialize + DeserializeOwned {
    const TABLE: CorrosionTable;
    const SUPPORTED_VERSION: CorrosionDocumentVersion = CorrosionDocumentVersion::V1;

    fn version(&self) -> CorrosionDocumentVersion;
    fn cluster_id(&self) -> &ClusterId;
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
        namespace_id: NamespaceId,
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
    Disabled,
    Ployz,
    Custom { suffix: RouteHostname },
}

/// Mesh implementation fixed for the life of a cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MeshProvider {
    BuiltinWireguard,
    Tailscale,
}

/// The transport identity and addresses carried by machine and peer rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transport {
    Wireguard {
        pubkey: WireGuardPublicKey,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        addr_v6: Ipv6Addr,
        #[cfg_attr(feature = "ts", ts(type = "string | null"))]
        endpoint: Option<SocketAddr>,
        #[cfg_attr(feature = "ts", ts(type = "string | null"))]
        subnet_v4: Option<Ipv4Net>,
    },
    Tailscale {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        ip: Ipv4Addr,
        #[cfg_attr(feature = "ts", ts(type = "string | null"))]
        subnet_v4: Option<Ipv4Net>,
    },
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

/// Service placement intent, with replica count present only when meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ServicePlacement {
    Replicated { replicas: ServiceReplicaCount },
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"ServiceReplicaCount\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct ServiceReplicaCount(NonZeroU16);

impl ServiceReplicaCount {
    pub fn try_new(value: u16) -> Result<Self, ServiceReplicaCountError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(ServiceReplicaCountError::Zero);
        };
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineLoadBand {
    Idle,
    Normal,
    Hot,
}

/// Operation kind and its typed target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionOperation {
    Build {
        service_id: ServiceId,
    },
    Deploy {
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    MachineAdd {
        target_machine_id: MachineId,
    },
    MachineRemove {
        target_machine_id: MachineId,
    },
    Recovery {
        target_machine_id: MachineId,
    },
}

/// Principal that initiated an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationInitiator {
    Machine { machine_id: MachineId },
    Peer { peer_id: PeerId },
    ApiToken { token_id: TokenId },
}

/// A terminal operation failure class with useful public evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionOperationFailure {
    Precondition {
        message: String,
    },
    MachineUnavailable {
        machine_id: MachineId,
        message: String,
    },
    Timeout {
        stage: String,
        message: String,
    },
    Execution {
        class: CorrosionExecutionFailureClass,
        message: String,
    },
    Interrupted {
        message: String,
    },
    Superseded {
        winner: OperationId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CorrosionExecutionFailureClass {
    BuildFailed,
    ImagePullFailed,
    ContainerStartFailed,
    HealthGateFailed,
    StorageFailed,
    NetworkFailed,
    Internal,
}

/// Created, running, or final operation summary state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CorrosionOperationState {
    Created {
        created_at: String,
    },
    Running {
        started_at: String,
        heartbeat_at: String,
    },
    Succeeded {
        started_at: String,
        completed_at: String,
    },
    Failed {
        started_at: String,
        completed_at: String,
        failure: CorrosionOperationFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ClusterDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub name: String,
    pub storage_default: StorageMode,
    pub hostname_mode: AutomaticHostnameMode,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub prefix: Ipv4Net,
    pub provider: MeshProvider,
    pub acme_directory_url: String,
    pub acme_contact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub name: MachineName,
    pub lifecycle: MachineLifecycle,
    pub transport: Transport,
    pub storage: MachineStorageSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PeerDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub name: String,
    pub transport: Transport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub secret_sha256: Sha256Hex,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NamespaceDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ServiceDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub namespace_id: NamespaceId,
    pub name: String,
    pub image: ImageReference,
    pub env_fingerprints: BTreeMap<String, Sha256Hex>,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub placement: ServicePlacement,
    pub pinned_machines: BTreeSet<MachineId>,
    pub active_deploy: OperationId,
    pub previous_image: Option<ImageReference>,
    pub deployed_at: String,
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct RouteBindingDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub hostname: RouteHostname,
    pub service_id: ServiceId,
    pub namespace_id: NamespaceId,
    pub endpoint_port: RoutePort,
    pub origin: RouteBindingOrigin,
    pub ingress_mode: IngressMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ContainerDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineId,
    pub service_id: ServiceId,
    pub namespace_id: NamespaceId,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub ip: Ipv4Addr,
    pub deploy: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineStatusDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineId,
    pub ployz_version: String,
    pub corrosion_version: String,
    pub architecture: String,
    pub free_disk_bytes: u64,
    pub free_memory_bytes: u64,
    pub load: MachineLoadBand,
    pub observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OperationDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub operation: CorrosionOperation,
    pub initiator: OperationInitiator,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub status: CorrosionOperationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CertHoldingDocument {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineId,
    pub hostname: RouteHostname,
    pub fingerprint: Sha256Hex,
    pub issued_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcmeHttp01Document {
    pub v: CorrosionDocumentVersion,
    pub cluster_id: ClusterId,
    pub machine_id: MachineId,
    pub hostname: RouteHostname,
    pub key_authorization: String,
    pub created_at: String,
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

corrosion_document!(ClusterDocument, CorrosionTable::Cluster);
corrosion_document!(MachineDocument, CorrosionTable::Machines);
corrosion_document!(PeerDocument, CorrosionTable::Peers);
corrosion_document!(TokenDocument, CorrosionTable::Tokens);
corrosion_document!(NamespaceDocument, CorrosionTable::Namespaces);
corrosion_document!(ServiceDocument, CorrosionTable::Services);
corrosion_document!(RouteBindingDocument, CorrosionTable::RouteBindings);
corrosion_document!(ContainerDocument, CorrosionTable::Containers);
corrosion_document!(MachineStatusDocument, CorrosionTable::MachineStatus);
corrosion_document!(OperationDocument, CorrosionTable::Operations);
corrosion_document!(CertHoldingDocument, CorrosionTable::CertHoldings);
corrosion_document!(AcmeHttp01Document, CorrosionTable::AcmeHttp01);

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
            name: self.name.clone(),
        }
    }
}

impl NamedCorrosionDocument for ServiceDocument {
    fn name_claim(&self) -> NameClaim {
        NameClaim::Service {
            namespace_id: self.namespace_id.clone(),
            name: self.name.clone(),
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
