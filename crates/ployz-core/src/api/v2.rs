//! Public HTTP/JSON/SSE contract for the coreless v2 API.

use serde::{Deserialize, Serialize};
use url::Url;

use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use crate::corrosion::{
    ClusterDocument, ContainerDocument, CorrosionNamespaceName, CorrosionServiceName,
    CorrosionTimestamp, HostPortBindings, MachineDocument, MachineLoadBand, MachineStatusDocument,
    NamespaceDocument, OperationDocument, Principal, ServiceDocument, ServiceReplicaCount,
    SourcePrincipalResolutionError, V2ManagedContainerIdentity,
};
use crate::deploy::{ContainerRuntimeSpec, ImageReference, RegistryCredential, VolumeName};
use crate::ids::{
    ContainerId, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId, TokenId,
};
use crate::install::{InstallArtifactVersion, InstallSha256Digest};
use crate::machine::{MachineLifecycle, MachineName};
use crate::placement::{PlacementPick, PlacementRefusal};

/// The only supported major version of the v2 HTTP contract.
pub const API_MAJOR: u16 = 1;

/// The stable path for the answering machine's advertised API version.
pub const VERSION_ROUTE: &str = "/version";

/// The stable prefix for state lenses.
pub const LENSES_ROUTE: &str = "/lenses";

/// The stable endpoint for writing machine one's initial authority rows.
pub const FOUNDING_ROUTE: &str = "/founding";

/// Stable endpoint for minting a show-once join token.
pub const TOKEN_CREATE_ROUTE: &str = "/tokens/create";
/// Stable endpoint for listing join-token metadata.
pub const TOKEN_LIST_ROUTE: &str = "/tokens/list";
/// Stable prefix for deleting one join-token row by id.
pub const TOKEN_REVOKE_ROUTE_PREFIX: &str = "/tokens/revoke";
/// Stable prefix for changing one machine's advertised WireGuard endpoint.
pub const MACHINE_ENDPOINT_ROUTE_PREFIX: &str = "/machines/endpoint";
/// Stable endpoint for a caller-paced upgrade of the answering machine.
pub const MACHINE_UPGRADE_ROUTE: &str = "/machines/upgrade";
/// Stable endpoint for fencing one machine from the roster and sweeping its testimony.
pub const MACHINE_REMOVE_ROUTE: &str = "/machines/remove";
/// The only route exposed by the public TLS join door.
pub const JOIN_ROUTE: &str = "/join";
/// Stable endpoint for creating one namespace authority row.
pub const NAMESPACE_CREATE_ROUTE: &str = "/namespaces/create";
/// Stable endpoint for removing one empty namespace authority row.
pub const NAMESPACE_REMOVE_ROUTE: &str = "/namespaces/remove";
/// Stable endpoint for submitting one service deploy.
pub const DEPLOY_ROUTE: &str = "/deploy";
/// Stable endpoint for gathering the answering machine's live placement bid.
pub const PLACEMENT_BID_ROUTE: &str = "/deploy/bid";
/// Stable endpoint for executing one deploy verb under a live deploy claim.
pub const DEPLOY_EXECUTE_ROUTE: &str = "/deploy/execute";
/// Stable prefix for one operation summary and its driver-local evidence.
pub const OPERATIONS_ROUTE_PREFIX: &str = "/operations";
/// Stable prefix for service log access.
pub const SERVICE_LOGS_ROUTE_PREFIX: &str = "/services";
/// The stable endpoint for the cheap cluster diagnostics projection.
pub const STATUS_ROUTE: &str = "/status";
/// The stable endpoint for the read-only deep diagnostics projection.
pub const DOCTOR_ROUTE: &str = "/doctor";
/// Stable endpoint for removing one valid peer row.
pub const PEER_REMOVE_ROUTE: &str = "/peers/remove";
/// Stable endpoint for removing one valid service row.
pub const SERVICE_REMOVE_ROW_ROUTE: &str = "/services/remove";
/// Stable endpoint for removing one valid route-binding row.
pub const ROUTE_REMOVE_ROUTE: &str = "/routes/remove";

/// A capability understood by this version of the public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub enum KnownApiFeature {
    #[serde(rename = "v2.founding")]
    Founding,
    #[serde(rename = "v2.lenses")]
    Lenses,
    #[serde(rename = "v2.join_tokens")]
    JoinTokens,
    #[serde(rename = "v2.machine_endpoint")]
    MachineEndpoint,
    #[serde(rename = "v2.machine_upgrade")]
    MachineUpgrade,
    #[serde(rename = "v2.machine_remove")]
    MachineRemove,
    #[serde(rename = "v2.join_door")]
    JoinDoor,
    #[serde(rename = "v2.namespace_primitives")]
    NamespacePrimitives,
    #[serde(rename = "v2.deploy")]
    Deploy,
    #[serde(rename = "v2.placement")]
    Placement,
    #[serde(rename = "v2.operation_evidence")]
    OperationEvidence,
    #[serde(rename = "v2.logs")]
    Logs,
    #[serde(rename = "v2.diagnostics")]
    Diagnostics,
    #[serde(rename = "v2.peer_remove")]
    PeerRemove,
    #[serde(rename = "v2.service_remove")]
    ServiceRemove,
    #[serde(rename = "v2.route_remove")]
    RouteRemove,
}

impl KnownApiFeature {
    /// The capability's stable, namespaced wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Founding => "v2.founding",
            Self::Lenses => "v2.lenses",
            Self::JoinTokens => "v2.join_tokens",
            Self::MachineEndpoint => "v2.machine_endpoint",
            Self::MachineUpgrade => "v2.machine_upgrade",
            Self::MachineRemove => "v2.machine_remove",
            Self::JoinDoor => "v2.join_door",
            Self::NamespacePrimitives => "v2.namespace_primitives",
            Self::Deploy => "v2.deploy",
            Self::Placement => "v2.placement",
            Self::OperationEvidence => "v2.operation_evidence",
            Self::Logs => "v2.logs",
            Self::Diagnostics => "v2.diagnostics",
            Self::PeerRemove => "v2.peer_remove",
            Self::ServiceRemove => "v2.service_remove",
            Self::RouteRemove => "v2.route_remove",
        }
    }
}

/// Every capability this version of Core knows how to name.
pub const KNOWN_API_FEATURES: &[KnownApiFeature] = &[
    KnownApiFeature::Founding,
    KnownApiFeature::Lenses,
    KnownApiFeature::JoinTokens,
    KnownApiFeature::MachineEndpoint,
    KnownApiFeature::MachineUpgrade,
    KnownApiFeature::MachineRemove,
    KnownApiFeature::JoinDoor,
    KnownApiFeature::NamespacePrimitives,
    KnownApiFeature::Deploy,
    KnownApiFeature::Placement,
    KnownApiFeature::OperationEvidence,
    KnownApiFeature::Logs,
    KnownApiFeature::Diagnostics,
    KnownApiFeature::PeerRemove,
    KnownApiFeature::ServiceRemove,
    KnownApiFeature::RouteRemove,
];

/// An advertised capability, including names added by a newer machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApiFeature {
    Known(KnownApiFeature),
    Other(String),
}

impl From<KnownApiFeature> for ApiFeature {
    fn from(feature: KnownApiFeature) -> Self {
        Self::Known(feature)
    }
}

impl ApiFeature {
    /// Creates an additive capability name not yet known by this Core version.
    #[must_use]
    pub fn other(name: impl Into<String>) -> Self {
        Self::Other(name.into())
    }
}

/// The response returned by [`VERSION_ROUTE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ApiVersion {
    pub major: u16,
    pub build: String,
    #[cfg_attr(feature = "ts", ts(type = "Array<ApiFeature>"))]
    pub features: Vec<ApiFeature>,
}

impl ApiVersion {
    /// Builds a version response using the fixed public API major.
    #[must_use]
    pub fn new(build: impl Into<String>, features: impl IntoIterator<Item = ApiFeature>) -> Self {
        Self {
            major: API_MAJOR,
            build: build.into(),
            features: features.into_iter().collect(),
        }
    }
}

/// A collection served by the state-lens API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LensCollection {
    Machines,
    Services,
    Containers,
    MachineStatus,
    Operations,
}

impl LensCollection {
    /// Every collection in stable route order.
    pub const ALL: &'static [Self] = &[
        Self::Machines,
        Self::Services,
        Self::Containers,
        Self::MachineStatus,
        Self::Operations,
    ];

    /// The collection's stable path segment.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Machines => "machines",
            Self::Services => "services",
            Self::Containers => "containers",
            Self::MachineStatus => "machine_status",
            Self::Operations => "operations",
        }
    }

    /// Parses one stable collection path segment.
    #[must_use]
    pub fn parse_segment(value: &str) -> Option<Self> {
        match value {
            "machines" => Some(Self::Machines),
            "services" => Some(Self::Services),
            "containers" => Some(Self::Containers),
            "machine_status" => Some(Self::MachineStatus),
            "operations" => Some(Self::Operations),
            _ => None,
        }
    }
}

/// Builds the exact snapshot route for one lens collection.
#[must_use]
pub fn lens_route(collection: LensCollection) -> String {
    format!("{LENSES_ROUTE}/{}", collection.as_str())
}

/// Builds the exact SSE watch route for one lens collection.
#[must_use]
pub fn lens_watch_route(collection: LensCollection) -> String {
    format!("{}/watch", lens_route(collection))
}

/// Builds the exact token-row deletion route for one canonical id.
#[must_use]
pub fn token_revoke_route(token_id: &TokenId) -> String {
    format!("{TOKEN_REVOKE_ROUTE_PREFIX}/{token_id}")
}

/// Builds the exact summary route for one operation row.
#[must_use]
pub fn operation_route(operation_id: &OperationRowId) -> String {
    format!("{OPERATIONS_ROUTE_PREFIX}/{operation_id}")
}

/// Builds the full-replay-then-follow route for one operation's evidence.
#[must_use]
pub fn operation_watch_route(operation_id: &OperationRowId) -> String {
    format!("{}/watch", operation_route(operation_id))
}

/// Builds the bounded log-tail route for one service row.
#[must_use]
pub fn service_logs_tail_route(service_id: &ServiceRowId) -> String {
    format!("{SERVICE_LOGS_ROUTE_PREFIX}/{service_id}/logs")
}

/// Builds the lossy follow route for one service row's current container.
#[must_use]
pub fn service_logs_follow_route(service_id: &ServiceRowId) -> String {
    format!("{}/follow", service_logs_tail_route(service_id))
}

/// Mesh-authenticated request to create one namespace authority row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CorrosionNamespaceCreateRequest {
    pub namespace_name: CorrosionNamespaceName,
}

/// The exact namespace row accepted by a synchronous create primitive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CorrosionNamespaceCreateReply {
    pub namespace_id: NamespaceRowId,
    pub document: NamespaceDocument,
}

/// A namespace create that cannot claim its human name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrosionNamespaceCreateRefusal {
    NameAlreadyClaimed {
        namespace_name: CorrosionNamespaceName,
        winner: NamespaceRowId,
    },
}

/// Mesh-authenticated selector for removing one exact empty namespace row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CorrosionNamespaceRemoveRequest {
    pub namespace_name: CorrosionNamespaceName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_id: Option<NamespaceRowId>,
}

/// The synchronous result of removing an exact namespace row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrosionNamespaceRemoveReply {
    Removed { namespace_id: NamespaceRowId },
    AlreadyAbsent { namespace_id: NamespaceRowId },
}

/// A namespace removal refusal; removal never performs workload cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrosionNamespaceRemoveRefusal {
    NotFound {
        namespace_name: CorrosionNamespaceName,
    },
    Ambiguous {
        namespace_name: CorrosionNamespaceName,
        namespace_ids: Vec<NamespaceRowId>,
    },
    IdMismatch {
        namespace_name: CorrosionNamespaceName,
        namespace_id: NamespaceRowId,
    },
    NotEmpty {
        namespace_id: NamespaceRowId,
        service_ids: Vec<ServiceRowId>,
        route_binding_count: usize,
    },
    Changed {
        namespace_id: NamespaceRowId,
    },
}

/// Whether a deploy enforces its health gate before promotion.
///
/// [`Self::Skip`] is the emergency escape for a service whose incumbent is
/// already down; the outcome records it as a durable deploy warning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HealthGatePolicy {
    #[default]
    Enforce,
    Skip,
}

/// The complete secret-bearing runtime input for one service deploy.
///
/// Environment values are redacted by their Core value type and are never
/// copied into durable operation evidence or Corrosion rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub namespace_name: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
    pub image: ImageReference,
    pub runtime: ContainerRuntimeSpec,
    #[serde(default)]
    pub health_gate: HealthGatePolicy,
    /// `None` inherits the incumbent row's placement, or one replica on a
    /// first deploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<RequestedPlacement>,
    /// `None` inherits the incumbent row's pin set unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machines: Option<RequestedPins>,
}

/// Requested placement intent. Host-published ports exist only on the global
/// variant, so a replicated deploy with published ports is unrepresentable
/// on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestedPlacement {
    /// A mode-preserving replica-count change. Against a global incumbent it
    /// refuses instead of silently unpublishing host ports; converting the
    /// mode is an explicit [`Self::Replicated`] deploy.
    Replicas { replicas: ServiceReplicaCount },
    /// Explicit replicated mode. `None` keeps a replicated incumbent's
    /// count; a first deploy or a global-to-replicated conversion defaults
    /// to one replica.
    Replicated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replicas: Option<ServiceReplicaCount>,
    },
    Global {
        #[serde(default, skip_serializing_if = "HostPortBindings::is_empty")]
        host_ports: HostPortBindings,
    },
}

/// Requested pin intent: pin to named machines, or clear the row's pin set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestedPins {
    Machines { names: PinnedMachineNames },
    Any,
}

/// A non-empty set of machine names to pin a service to; the empty set is
/// expressed as [`RequestedPins::Any`] instead of an empty pin list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Array<MachineName>"))]
#[serde(try_from = "BTreeSet<MachineName>", into = "BTreeSet<MachineName>")]
pub struct PinnedMachineNames(BTreeSet<MachineName>);

impl PinnedMachineNames {
    pub fn try_new(
        names: impl IntoIterator<Item = MachineName>,
    ) -> Result<Self, PinnedMachineNamesError> {
        let names = names.into_iter().collect::<BTreeSet<_>>();
        if names.is_empty() {
            return Err(PinnedMachineNamesError::Empty);
        }
        Ok(Self(names))
    }

    pub fn iter(&self) -> impl Iterator<Item = &MachineName> {
        self.0.iter()
    }
}

impl TryFrom<BTreeSet<MachineName>> for PinnedMachineNames {
    type Error = PinnedMachineNamesError;

    fn try_from(value: BTreeSet<MachineName>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<PinnedMachineNames> for BTreeSet<MachineName> {
    fn from(value: PinnedMachineNames) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinnedMachineNamesError {
    #[error("a machine pin set must name at least one machine")]
    Empty,
}

/// The operation handle returned after the driver durably accepts a deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployAccepted {
    pub operation_id: OperationRowId,
    pub driver_machine_id: MachineRowId,
}

/// A deploy refusal produced before any operation or Docker effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployRefusal {
    NamespaceNotFound {
        namespace_name: CorrosionNamespaceName,
        create_command: String,
    },
    NamespaceAmbiguous {
        namespace_name: CorrosionNamespaceName,
        namespace_ids: Vec<NamespaceRowId>,
    },
    /// The namespace's sole service holds a different name; a deploy only
    /// redeploys the incumbent, and `ployz service remove` frees the namespace.
    DifferentService {
        namespace_id: NamespaceRowId,
        incumbent_service_name: CorrosionServiceName,
    },
    /// More than one service occupies the namespace; the driver refuses to
    /// guess which one the deploy replaces.
    MultipleServices {
        namespace_id: NamespaceRowId,
        service_ids: Vec<ServiceRowId>,
    },
    /// Route bindings exist without any service, so the namespace is neither
    /// empty nor redeployable.
    RoutesWithoutServices {
        namespace_id: NamespaceRowId,
    },
    /// The deterministic pick refused to derive any target set.
    Placement {
        refusal: PlacementRefusal,
    },
    /// A bare replica-count change cannot apply to a global service; the
    /// converting command is an explicit `--mode replicated` deploy.
    ReplicasOnGlobalService,
    /// A requested pin names no accepted roster machine.
    UnknownPinnedMachine {
        machine_name: MachineName,
    },
    BridgeUnavailable,
}

impl DeployRefusal {
    /// Creates the fixed refusal that hands the operator to the namespace primitive.
    #[must_use]
    pub fn namespace_not_found(namespace_name: CorrosionNamespaceName) -> Self {
        let create_command = format!("ployz namespace create {}", namespace_name.as_str());
        Self::NamespaceNotFound {
            namespace_name,
            create_command,
        }
    }
}

/// Mesh-authenticated request for one machine's live placement bid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementBidRequest {
    pub namespace_id: NamespaceRowId,
    pub service_id: ServiceRowId,
    /// The service's declared named volumes; the reply reports which of
    /// these the answering machine holds.
    pub volumes: BTreeSet<VolumeName>,
    /// The incumbent's active deploy; `None` on a first deploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_deploy: Option<OperationRowId>,
}

/// One machine's live placement testimony, gathered at point of use and
/// never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PlacementBid {
    pub machine_id: MachineRowId,
    pub machine_name: MachineName,
    /// The machine's reported CPU architecture, e.g. `x86_64`.
    pub architecture: String,
    pub lifecycle: MachineLifecycle,
    pub free_disk_bytes: u64,
    pub free_memory_bytes: u64,
    pub load: MachineLoadBand,
    /// Every managed container on the machine, across all services.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub total_container_count: usize,
    /// The requested namespace's service containers from live Docker.
    pub service_containers: Vec<ServiceContainerObservation>,
    /// The requested declared volumes the machine holds.
    pub volumes_held: BTreeSet<VolumeName>,
}

/// One live Docker observation of a container in the requested namespace.
///
/// The scope is the namespace, not the service row id: a namespace admits
/// one service, and a failed first deploy's containers carry a generated
/// service row id that never reached a row, so only the namespace names
/// them for the next attempt's sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceContainerObservation {
    pub container_id: ContainerId,
    /// The service row id recovered from the container's own identity.
    pub service_id: ServiceRowId,
    /// The deploy operation that created the container.
    pub deploy: OperationRowId,
    /// Named volumes the container mounts.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub named_volumes: BTreeSet<VolumeName>,
}

/// One deploy verb the driver commands on a target machine. Every verb is
/// bound to its operation and service so the responder can authorize it
/// against the live deploy claim on its own replica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployExecuteRequest {
    pub operation_id: OperationRowId,
    pub namespace_id: NamespaceRowId,
    pub service_id: ServiceRowId,
    pub verb: DeployVerb,
}

/// The Docker effect a deploy verb performs, mirroring the driver's local
/// deploy runtime surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployVerb {
    /// Lists this service's containers from live Docker.
    ListServiceContainers,
    /// Pulls the exact resolved image, forwarding the driver's registry
    /// credential. The credential never reaches evidence or logs.
    PullImage {
        image: ImageReference,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential: Option<RegistryCredential>,
    },
    /// Creates this operation's container from the resolved image and the
    /// request's runtime spec.
    CreateContainer {
        image: ImageReference,
        runtime: Box<ContainerRuntimeSpec>,
        /// The namespace's human name, which derives the container's
        /// internal DNS search domain.
        namespace_name: CorrosionNamespaceName,
        /// Host-published ports for a global service's container; empty for
        /// every replicated create.
        #[serde(default, skip_serializing_if = "HostPortBindings::is_empty")]
        host_ports: HostPortBindings,
    },
    /// Starts a created container, or restarts a stopped incumbent after a
    /// failed cutover gate. The responder refuses a container outside the
    /// verb's service scope.
    StartContainer { container_id: ContainerId },
    /// Stops the container only while its live Docker identity still matches
    /// `expected`; a newer deploy's container refuses instead of stopping.
    StopContainer {
        container_id: ContainerId,
        expected: V2ManagedContainerIdentity,
    },
    /// Polls the started container to a health verdict, or resolves only
    /// its endpoint when the deploy skips the gate.
    HealthGate {
        container_id: ContainerId,
        policy: HealthGatePolicy,
    },
    /// Removes the container only while its live Docker identity still
    /// matches `expected`; a newer deploy's container refuses instead of
    /// being removed.
    RemoveContainer {
        container_id: ContainerId,
        expected: V2ManagedContainerIdentity,
    },
}

/// The typed outcome of one deploy verb.
///
/// [`Self::ClaimNotYetVisible`] is replication lag, not a refusal: the
/// responder's replica has not converged on the operation row yet, and the
/// driver retries within a bounded budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployExecuteOutcome {
    ServiceContainers {
        containers: Vec<ServiceContainerObservation>,
    },
    ImagePulled,
    ContainerCreated {
        container_id: ContainerId,
    },
    ContainerStarted,
    ContainerStopped,
    HealthGated {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        ip: Ipv4Addr,
    },
    ContainerRemoved,
    /// The live container's recovered Docker identity does not match the
    /// verb's expected identity; the verb refused rather than touch another
    /// deploy's container.
    ContainerIdentityMismatch {
        actual: V2ManagedContainerIdentity,
    },
    /// The calling machine is not the driver recorded on the operation.
    CallerNotDriver {
        driver: MachineRowId,
    },
    /// The operation is terminal; no further verbs may act under it.
    ClaimTerminal,
    /// The verb's namespace or service does not match the operation's
    /// target.
    ServiceMismatch,
    ClaimNotYetVisible,
    /// The verb ran and failed; the diagnostic is redaction-safe.
    Failed {
        diagnostic: String,
    },
}

/// One operation summary returned by its row id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationLookupReply {
    pub operation_id: OperationRowId,
    pub operation: OperationDocument,
}

/// A typed refusal to resolve one operation summary row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationLookupRefusal {
    NotFound { operation_id: OperationRowId },
}

/// A positive, stable sequence in one operation's durable evidence file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(type = "SafeInteger<\"OperationEvidenceSequence\">")
)]
#[serde(try_from = "u64", into = "u64")]
pub struct OperationEvidenceSequence(u64);

impl OperationEvidenceSequence {
    pub fn try_new(value: u64) -> Result<Self, OperationEvidenceSequenceError> {
        if value == 0 {
            return Err(OperationEvidenceSequenceError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for OperationEvidenceSequence {
    type Error = OperationEvidenceSequenceError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperationEvidenceSequence> for u64 {
    fn from(value: OperationEvidenceSequence) -> Self {
        value.get()
    }
}

/// Sequence zero cannot identify a durable operation event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation evidence sequence must be positive")]
pub struct OperationEvidenceSequenceError;

/// Typed, redaction-safe progress recorded in driver-local JSONL evidence.
///
/// [`Self::Unrecognized`] is the additive-skew catch-all: a reader older
/// than the writing daemon deserializes any future evidence kind into it
/// instead of failing the replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationEvidence {
    Created,
    OpClaimWon,
    OpClaimLost {
        winner: OperationRowId,
    },
    /// The bids and silences a placement gather observed; together with
    /// [`Self::PlacementPicked`] it replays the pick from JSONL.
    PlacementGathered {
        bids: Vec<PlacementBid>,
        silent: Vec<SilentMachine>,
    },
    PlacementPicked {
        pick: PlacementPick,
    },
    DebrisSwept {
        removed: Vec<ContainerId>,
    },
    PullingImage,
    ImageResolved,
    ContainerCreated {
        container_id: ContainerId,
    },
    ContainerStarted {
        container_id: ContainerId,
    },
    HealthGateSkipped,
    IncumbentStopped {
        container_id: ContainerId,
    },
    IncumbentRestarted {
        container_id: ContainerId,
    },
    PromotionPrepared,
    RowsCommitted,
    ServiceClaimWon,
    ServiceClaimLost {
        winner: ServiceRowId,
    },
    Drained,
    IncumbentRemoved {
        container_id: ContainerId,
    },
    Terminal {
        operation: Box<OperationDocument>,
    },
    #[serde(other)]
    Unrecognized,
}

/// One roster machine that yielded no placement bid, with why its silence
/// was expected or alarming.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct SilentMachine {
    pub machine_id: MachineRowId,
    pub classification: SilenceClassification,
}

/// Whether a machine's placement silence was already explained by its stale
/// WireGuard handshake, or is the alarming kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SilenceClassification {
    /// The peer's handshake was already stale, so it was skipped without
    /// burning the RPC timeout.
    ExpectedSilent { handshake_age_seconds: u64 },
    /// A fresh peer that did not yield a bid.
    AnomalousSilent { reason: AnomalousSilenceReason },
}

/// Why a fresh peer yielded no bid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnomalousSilenceReason {
    /// The bid transport failed before a well-formed reply arrived.
    TransportFailed,
    /// The bid did not complete within its bounded budget.
    TimedOut,
    /// The responder answered a non-OK status instead of a bid.
    Declined { status: u16 },
}

/// One durable operation detail event. Sequences start at one for every attach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationEvidenceEvent {
    pub sequence: OperationEvidenceSequence,
    pub timestamp: CorrosionTimestamp,
    /// The machine the event acts on; `None` when the event names no single
    /// machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineRowId>,
    pub evidence: OperationEvidence,
}

/// A fixed point-of-use WireGuard handshake observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandshakeObservation {
    Ago {
        observed_at: CorrosionTimestamp,
        age_seconds: u64,
    },
    Never {
        observed_at: CorrosionTimestamp,
    },
}

/// Why a point-of-use handshake observation could not be obtained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandshakeObservationUnavailable {
    OwnerNotRostered,
    PeerAbsent,
    UnsupportedProvider,
    KeeperUnavailable,
    KeeperTimedOut,
    KeeperProtocol,
}

/// Either a real fixed observation or an explicit reason it is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandshakeObservationOutcome {
    Observed {
        observation: HandshakeObservation,
    },
    Unavailable {
        reason: HandshakeObservationUnavailable,
    },
}

/// A typed refusal to replay or follow one operation's driver-local detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationWatchRefusal {
    NotFound {
        operation_id: OperationRowId,
    },
    OwnerNoLongerRostered {
        operation: OperationLookupReply,
        observation: HandshakeObservationOutcome,
    },
    DriverDark {
        operation: OperationLookupReply,
        observation: HandshakeObservationOutcome,
    },
    DetailUnavailable {
        operation: OperationLookupReply,
    },
    ProxyLoop,
}

/// The public SSE envelope for full replay followed by live operation detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationWatchEvent {
    Evidence { event: OperationEvidenceEvent },
    Terminal { refusal: Box<OperationWatchRefusal> },
}

impl OperationWatchEvent {
    /// Returns the stable SSE event name for this envelope.
    #[must_use]
    pub const fn event_name(&self) -> &'static str {
        match self {
            Self::Evidence { .. } => "evidence",
            Self::Terminal { .. } => "terminal",
        }
    }
}

/// A positive, bounded line count for one v2 log tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"CorrosionLogsTailLines\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct CorrosionLogsTailLines(u16);

impl CorrosionLogsTailLines {
    pub const MAX: u16 = 1_000;

    pub fn try_new(value: u16) -> Result<Self, CorrosionLogsTailLinesError> {
        if value == 0 || value > Self::MAX {
            return Err(CorrosionLogsTailLinesError { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for CorrosionLogsTailLines {
    type Error = CorrosionLogsTailLinesError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CorrosionLogsTailLines> for u16 {
    fn from(value: CorrosionLogsTailLines) -> Self {
        value.get()
    }
}

/// A log tail line count outside the supported public bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("v2 log tail lines must be between 1 and {max}, got {value}", max = CorrosionLogsTailLines::MAX)]
pub struct CorrosionLogsTailLinesError {
    pub value: u16,
}

/// A bounded service log request; the server applies its own fixed upper bound.
///
/// `tail_lines: None` attaches without replaying any existing lines — the
/// follow-reconnect form, which still needs to carry the machine selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceLogsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<CorrosionLogsTailLines>,
    /// Selects the replica hosted by the named machine when the service runs
    /// containers on more than one machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineName>,
}

/// Docker's stable stdout/stderr distinction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ServiceLogStream {
    Stdout,
    Stderr,
}

/// One complete log line from a managed container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceLogLine {
    pub stream: ServiceLogStream,
    pub line: String,
}

/// A typed refusal before or during service log access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceLogsRefusal {
    ServiceNotFound {
        service_id: ServiceRowId,
    },
    NoActiveDeploy {
        service_id: ServiceRowId,
    },
    ContainerNotFound {
        service_id: ServiceRowId,
    },
    UnmanagedContainer {
        container_id: ContainerId,
    },
    /// The service runs containers on more than one machine (or the request's
    /// machine selector matched none of them); the listed machine names carry
    /// one entry per container, so stacked replicas repeat their host.
    MachineSelectorRequired {
        machines: Vec<MachineName>,
    },
    /// The hosting machines' roster rows could not be resolved to names, so
    /// no machine selector can be offered; the machines lens is the
    /// inspection primitive.
    HostingMachinesUnresolved {
        machine_ids: Vec<MachineRowId>,
    },
    RemoteOwner {
        machine_id: MachineRowId,
        /// `None` when the owning machine's roster row is no longer readable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine_name: Option<MachineName>,
    },
    DriverDark {
        machine_id: MachineRowId,
        observation: HandshakeObservationOutcome,
    },
    RuntimeUnavailable {
        machine_id: MachineRowId,
    },
}

/// A bounded, non-streaming log tail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceLogsTailReply {
    pub lines: Vec<ServiceLogLine>,
    pub truncated: bool,
}

/// A lossy log stream. Reconnect loss is always explicit as [`Self::Gap`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceLogsFollowEvent {
    Line { log: ServiceLogLine },
    Gap,
    Terminal { refusal: ServiceLogsRefusal },
}

impl ServiceLogsFollowEvent {
    /// Returns the stable SSE event name for this log envelope.
    #[must_use]
    pub const fn event_name(&self) -> &'static str {
        match self {
            Self::Line { .. } => "line",
            Self::Gap => "gap",
            Self::Terminal { .. } => "terminal",
        }
    }
}

/// Mesh-authenticated request to fence one machine from the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineRemoveRequest {
    pub machine_name: MachineName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineRowId>,
}

/// The terminal outcome of a machine-removal fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineRemoveReply {
    Removed { machine_id: MachineRowId },
    AlreadyAbsent { machine_id: MachineRowId },
}

/// A refusal to resolve a machine-removal selector against accepted roster rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineRemoveRefusal {
    NotFound {
        machine_name: MachineName,
    },
    Ambiguous {
        machine_name: MachineName,
        machine_ids: Vec<MachineRowId>,
    },
    IdMismatch {
        machine_name: MachineName,
        machine_id: MachineRowId,
    },
}

/// Resolves a removal selector only from roster rows already accepted by the
/// reader law. The optional row id disambiguates a name collision without
/// making raw or skipped rows selectable.
pub fn select_machine_removal(
    request: &MachineRemoveRequest,
    accepted: impl IntoIterator<Item = (MachineRowId, MachineName)>,
) -> Result<MachineRowId, MachineRemoveRefusal> {
    match select_removal(&request.machine_name, request.machine_id.as_ref(), accepted) {
        RemovalSelection::Selected(machine_id) => Ok(machine_id),
        RemovalSelection::NotFound => Err(MachineRemoveRefusal::NotFound {
            machine_name: request.machine_name.clone(),
        }),
        RemovalSelection::Ambiguous(machine_ids) => Err(MachineRemoveRefusal::Ambiguous {
            machine_name: request.machine_name.clone(),
            machine_ids,
        }),
        RemovalSelection::IdMismatch(machine_id) => Err(MachineRemoveRefusal::IdMismatch {
            machine_name: request.machine_name.clone(),
            machine_id,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemovalSelection<Id> {
    Selected(Id),
    NotFound,
    Ambiguous(Vec<Id>),
    IdMismatch(Id),
}

fn select_removal<Id, Name>(
    requested_name: &Name,
    requested_id: Option<&Id>,
    accepted: impl IntoIterator<Item = (Id, Name)>,
) -> RemovalSelection<Id>
where
    Id: Clone + Ord,
    Name: PartialEq,
{
    let accepted = accepted.into_iter().collect::<Vec<_>>();
    let requested_id_is_accepted = requested_id.is_some_and(|requested_id| {
        accepted
            .iter()
            .any(|(accepted_id, _)| accepted_id == requested_id)
    });
    let mut candidates = accepted
        .into_iter()
        .filter_map(|(id, name)| (&name == requested_name).then_some(id))
        .collect::<Vec<_>>();
    candidates.sort();

    let Some(expected_id) = requested_id else {
        return match candidates.as_slice() {
            [] => RemovalSelection::NotFound,
            [id] => RemovalSelection::Selected(id.clone()),
            _ => RemovalSelection::Ambiguous(candidates),
        };
    };

    if candidates.iter().any(|id| id == expected_id) {
        return RemovalSelection::Selected(expected_id.clone());
    }
    if candidates.is_empty() && !requested_id_is_accepted {
        return RemovalSelection::NotFound;
    }
    RemovalSelection::IdMismatch(expected_id.clone())
}

/// The exact public route shape, parsed without any daemon-local strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2Route {
    Version,
    Founding,
    TokenCreate,
    TokenList,
    TokenRevoke(TokenId),
    MachineEndpointSet,
    MachineUpgrade,
    MachineRemove,
    Join,
    NamespaceCreate,
    NamespaceRemove,
    Deploy,
    PlacementBid,
    DeployExecute,
    Operation(OperationRowId),
    OperationWatch(OperationRowId),
    ServiceLogsTail(ServiceRowId),
    ServiceLogsFollow(ServiceRowId),
    Status,
    Doctor,
    PeerRemove,
    ServiceRemove,
    RouteRemove,
    Lens(LensCollection),
    LensWatch(LensCollection),
}

/// The HTTP method fixed by one public v2 route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2Method {
    Get,
    Post,
}

impl V2Route {
    /// Parses an exact v2 route path. Query strings are not part of a route.
    #[must_use]
    pub fn parse(path: &str) -> Option<Self> {
        if path == VERSION_ROUTE {
            return Some(Self::Version);
        }
        if path == FOUNDING_ROUTE {
            return Some(Self::Founding);
        }
        if path == TOKEN_CREATE_ROUTE {
            return Some(Self::TokenCreate);
        }
        if path == TOKEN_LIST_ROUTE {
            return Some(Self::TokenList);
        }
        if path == JOIN_ROUTE {
            return Some(Self::Join);
        }
        if path == STATUS_ROUTE {
            return Some(Self::Status);
        }
        if path == DOCTOR_ROUTE {
            return Some(Self::Doctor);
        }
        if path == PEER_REMOVE_ROUTE {
            return Some(Self::PeerRemove);
        }
        if path == SERVICE_REMOVE_ROW_ROUTE {
            return Some(Self::ServiceRemove);
        }
        if path == ROUTE_REMOVE_ROUTE {
            return Some(Self::RouteRemove);
        }
        if path == MACHINE_ENDPOINT_ROUTE_PREFIX {
            return Some(Self::MachineEndpointSet);
        }
        if path == MACHINE_UPGRADE_ROUTE {
            return Some(Self::MachineUpgrade);
        }
        if path == NAMESPACE_CREATE_ROUTE {
            return Some(Self::NamespaceCreate);
        }
        if path == NAMESPACE_REMOVE_ROUTE {
            return Some(Self::NamespaceRemove);
        }
        if path == DEPLOY_ROUTE {
            return Some(Self::Deploy);
        }
        if path == PLACEMENT_BID_ROUTE {
            return Some(Self::PlacementBid);
        }
        if path == DEPLOY_EXECUTE_ROUTE {
            return Some(Self::DeployExecute);
        }
        if path == MACHINE_REMOVE_ROUTE {
            return Some(Self::MachineRemove);
        }
        if let Some(token_id) = path
            .strip_prefix(TOKEN_REVOKE_ROUTE_PREFIX)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .and_then(|id| TokenId::try_new(id).ok())
        {
            return Some(Self::TokenRevoke(token_id));
        }
        if let Some(operation_path) = path
            .strip_prefix(OPERATIONS_ROUTE_PREFIX)
            .and_then(|suffix| suffix.strip_prefix('/'))
        {
            let mut segments = operation_path.split('/');
            let operation_id = segments
                .next()
                .and_then(|id| OperationRowId::try_new(id).ok())?;
            return match segments.next() {
                None => Some(Self::Operation(operation_id)),
                Some("watch") if segments.next().is_none() => {
                    Some(Self::OperationWatch(operation_id))
                }
                Some(_) => None,
            };
        }
        if let Some(service_path) = path
            .strip_prefix(SERVICE_LOGS_ROUTE_PREFIX)
            .and_then(|suffix| suffix.strip_prefix('/'))
        {
            let mut segments = service_path.split('/');
            let service_id = segments
                .next()
                .and_then(|id| ServiceRowId::try_new(id).ok())?;
            if segments.next() != Some("logs") {
                return None;
            }
            return match segments.next() {
                None => Some(Self::ServiceLogsTail(service_id)),
                Some("follow") if segments.next().is_none() => {
                    Some(Self::ServiceLogsFollow(service_id))
                }
                Some(_) => None,
            };
        }
        let collection_path = path
            .strip_prefix(LENSES_ROUTE)
            .and_then(|path| path.strip_prefix('/'))?;
        let mut segments = collection_path.split('/');
        let collection = segments.next().and_then(LensCollection::parse_segment)?;

        match segments.next() {
            None => Some(Self::Lens(collection)),
            Some("watch") if segments.next().is_none() => Some(Self::LensWatch(collection)),
            Some(_) => None,
        }
    }

    /// Builds this route's canonical path.
    #[must_use]
    pub fn path(&self) -> String {
        match self {
            Self::Version => VERSION_ROUTE.to_owned(),
            Self::Founding => FOUNDING_ROUTE.to_owned(),
            Self::TokenCreate => TOKEN_CREATE_ROUTE.to_owned(),
            Self::TokenList => TOKEN_LIST_ROUTE.to_owned(),
            Self::TokenRevoke(token_id) => token_revoke_route(token_id),
            Self::MachineEndpointSet => MACHINE_ENDPOINT_ROUTE_PREFIX.to_owned(),
            Self::MachineUpgrade => MACHINE_UPGRADE_ROUTE.to_owned(),
            Self::MachineRemove => MACHINE_REMOVE_ROUTE.to_owned(),
            Self::Join => JOIN_ROUTE.to_owned(),
            Self::NamespaceCreate => NAMESPACE_CREATE_ROUTE.to_owned(),
            Self::NamespaceRemove => NAMESPACE_REMOVE_ROUTE.to_owned(),
            Self::Deploy => DEPLOY_ROUTE.to_owned(),
            Self::PlacementBid => PLACEMENT_BID_ROUTE.to_owned(),
            Self::DeployExecute => DEPLOY_EXECUTE_ROUTE.to_owned(),
            Self::Operation(operation_id) => operation_route(operation_id),
            Self::OperationWatch(operation_id) => operation_watch_route(operation_id),
            Self::ServiceLogsTail(service_id) => service_logs_tail_route(service_id),
            Self::ServiceLogsFollow(service_id) => service_logs_follow_route(service_id),
            Self::Status => STATUS_ROUTE.to_owned(),
            Self::Doctor => DOCTOR_ROUTE.to_owned(),
            Self::PeerRemove => PEER_REMOVE_ROUTE.to_owned(),
            Self::ServiceRemove => SERVICE_REMOVE_ROW_ROUTE.to_owned(),
            Self::RouteRemove => ROUTE_REMOVE_ROUTE.to_owned(),
            Self::Lens(collection) => lens_route(*collection),
            Self::LensWatch(collection) => lens_watch_route(*collection),
        }
    }

    /// Returns the one HTTP method accepted by this route.
    #[must_use]
    pub const fn method(&self) -> V2Method {
        match self {
            Self::Version
            | Self::Operation(_)
            | Self::OperationWatch(_)
            | Self::Status
            | Self::Doctor
            | Self::Lens(_)
            | Self::LensWatch(_) => V2Method::Get,
            Self::Founding
            | Self::TokenCreate
            | Self::TokenList
            | Self::TokenRevoke(_)
            | Self::MachineEndpointSet
            | Self::MachineUpgrade
            | Self::Join
            | Self::NamespaceCreate
            | Self::NamespaceRemove
            | Self::Deploy
            | Self::PlacementBid
            | Self::DeployExecute
            | Self::ServiceLogsTail(_)
            | Self::ServiceLogsFollow(_)
            | Self::MachineRemove
            | Self::PeerRemove
            | Self::ServiceRemove
            | Self::RouteRemove => V2Method::Post,
        }
    }

    /// Returns the capability that advertises this route.
    #[must_use]
    pub const fn feature(&self) -> KnownApiFeature {
        match self {
            Self::Version | Self::Founding => KnownApiFeature::Founding,
            Self::Lens(_) | Self::LensWatch(_) => KnownApiFeature::Lenses,
            Self::TokenCreate | Self::TokenList | Self::TokenRevoke(_) => {
                KnownApiFeature::JoinTokens
            }
            Self::MachineEndpointSet => KnownApiFeature::MachineEndpoint,
            Self::MachineUpgrade => KnownApiFeature::MachineUpgrade,
            Self::MachineRemove => KnownApiFeature::MachineRemove,
            Self::Join => KnownApiFeature::JoinDoor,
            Self::NamespaceCreate | Self::NamespaceRemove => KnownApiFeature::NamespacePrimitives,
            Self::Deploy => KnownApiFeature::Deploy,
            Self::PlacementBid | Self::DeployExecute => KnownApiFeature::Placement,
            Self::Operation(_) | Self::OperationWatch(_) => KnownApiFeature::OperationEvidence,
            Self::ServiceLogsTail(_) | Self::ServiceLogsFollow(_) => KnownApiFeature::Logs,
            Self::Status | Self::Doctor => KnownApiFeature::Diagnostics,
            Self::PeerRemove => KnownApiFeature::PeerRemove,
            Self::ServiceRemove => KnownApiFeature::ServiceRemove,
            Self::RouteRemove => KnownApiFeature::RouteRemove,
        }
    }

    /// Enforces the authority assigned to each API surface.
    #[must_use]
    pub const fn accepts_principal(&self, principal: &Principal) -> bool {
        match self {
            Self::Join => matches!(principal, Principal::ApiToken { .. }),
            Self::PlacementBid | Self::DeployExecute => {
                matches!(principal, Principal::Machine { .. })
            }
            Self::TokenCreate
            | Self::TokenList
            | Self::TokenRevoke(_)
            | Self::MachineEndpointSet
            | Self::MachineUpgrade
            | Self::MachineRemove
            | Self::NamespaceCreate
            | Self::NamespaceRemove
            | Self::Deploy
            | Self::PeerRemove
            | Self::ServiceRemove
            | Self::RouteRemove => matches!(principal, Principal::Peer { .. }),
            Self::Version
            | Self::Founding
            | Self::Operation(_)
            | Self::OperationWatch(_)
            | Self::ServiceLogsTail(_)
            | Self::ServiceLogsFollow(_)
            | Self::Status
            | Self::Doctor
            | Self::Lens(_)
            | Self::LensWatch(_) => {
                matches!(
                    principal,
                    Principal::Machine { .. } | Principal::Peer { .. }
                )
            }
        }
    }
}

/// An HTTPS artifact URL the answering machine may fetch for an upgrade.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineUpgradeUrl(String);

impl MachineUpgradeUrl {
    /// Validates an absolute HTTPS URL with a host.
    pub fn try_new(value: impl Into<String>) -> Result<Self, MachineUpgradeUrlError> {
        let value = value.into();
        let parsed = Url::parse(&value).map_err(|_| MachineUpgradeUrlError)?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            return Err(MachineUpgradeUrlError);
        }
        Ok(Self(value))
    }

    /// Returns the validated URL exactly as supplied.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineUpgradeUrl {
    type Error = MachineUpgradeUrlError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineUpgradeUrl> for String {
    fn from(value: MachineUpgradeUrl) -> Self {
        value.0
    }
}

/// A URL that is not a host-addressed HTTPS upgrade artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("machine upgrade URL must be an HTTPS URL with a host")]
pub struct MachineUpgradeUrlError;

/// The host supervisor responsible for applying a staged binary swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineUpgradeSupervisor {
    Systemd,
    OpenRc,
}

/// The caller-resolved artifact the answering machine must stage and verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineUpgradeRequest {
    pub version: InstallArtifactVersion,
    pub sha256: InstallSha256Digest,
    pub url: MachineUpgradeUrl,
}

/// The synchronous acknowledgement after a verified artifact has been staged and armed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineUpgradeReply {
    pub version: InstallArtifactVersion,
    pub sha256: InstallSha256Digest,
}

/// A refusal before the answering machine's binary changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpgradeRefusal {
    UnsupportedSupervisor {
        supervisor: MachineUpgradeSupervisor,
    },
    DownloadFailed {
        message: String,
    },
    Sha256Mismatch {
        expected: InstallSha256Digest,
        got: InstallSha256Digest,
    },
    StagingFailed {
        message: String,
    },
    KeeperRefused {
        message: String,
    },
}

/// One accepted machine roster row exposed by the machines lens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineLensRow {
    pub id: MachineRowId,
    pub document: MachineDocument,
}

/// One accepted service row exposed by the services lens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ServiceLensRow {
    pub id: ServiceRowId,
    pub document: ServiceDocument,
}

/// One container testimony row exposed by the containers lens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ContainerLensRow {
    /// The Docker-owned container row key.
    pub id: ContainerId,
    pub document: ContainerDocument,
}

/// One machine testimony row exposed by the machine-status lens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineStatusLensRow {
    pub id: MachineRowId,
    pub document: MachineStatusDocument,
}

impl MachineStatusLensRow {
    /// Preserves the machine-status table's key/document identity invariant.
    pub fn try_new(
        id: MachineRowId,
        document: MachineStatusDocument,
    ) -> Result<Self, MachineStatusLensRowIdentityError> {
        if id != document.machine_id {
            return Err(MachineStatusLensRowIdentityError {
                id,
                document_machine_id: document.machine_id,
            });
        }
        Ok(Self { id, document })
    }
}

/// A machine-status row key that disagrees with its machine-owned document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("machine status row key {id} disagrees with document machine id {document_machine_id}")]
pub struct MachineStatusLensRowIdentityError {
    pub id: MachineRowId,
    pub document_machine_id: MachineRowId,
}

/// One operation summary row exposed by the operations lens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct OperationLensRow {
    pub id: OperationRowId,
    pub document: OperationDocument,
}

/// The latest state observed for one lens.
///
/// A machines snapshot converges the cluster and roster table reads without
/// asserting an atomic capture across those separate inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "collection", rename_all = "snake_case")]
pub enum LensSnapshot {
    Machines {
        cluster: ClusterDocument,
        rows: Vec<MachineLensRow>,
    },
    Services {
        rows: Vec<ServiceLensRow>,
    },
    Containers {
        rows: Vec<ContainerLensRow>,
    },
    MachineStatus {
        rows: Vec<MachineStatusLensRow>,
    },
    Operations {
        rows: Vec<OperationLensRow>,
    },
}

/// A bounded retry advisory for a temporarily unavailable Corrosion view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(type = "SafeInteger<\"CorrosionRetryAfterSeconds\">")
)]
#[serde(try_from = "u8", into = "u8")]
pub struct CorrosionRetryAfterSeconds(u8);

impl CorrosionRetryAfterSeconds {
    /// A safe default retry advisory for bounded Corrosion failures.
    pub const DEFAULT: Self = Self(1);
    /// The maximum retry advisory accepted by this public contract.
    pub const MAX: u8 = 60;

    /// Validates a retry advisory without exposing an unbounded wait.
    pub fn try_new(seconds: u8) -> Result<Self, CorrosionRetryAfterSecondsError> {
        if seconds == 0 || seconds > Self::MAX {
            return Err(CorrosionRetryAfterSecondsError { seconds });
        }
        Ok(Self(seconds))
    }

    /// Returns the bounded retry advisory in seconds.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for CorrosionRetryAfterSeconds {
    type Error = CorrosionRetryAfterSecondsError;

    fn try_from(seconds: u8) -> Result<Self, Self::Error> {
        Self::try_new(seconds)
    }
}

impl From<CorrosionRetryAfterSeconds> for u8 {
    fn from(value: CorrosionRetryAfterSeconds) -> Self {
        value.get()
    }
}

/// An invalid public retry advisory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Corrosion retry advisory must be between 1 and {max} seconds, got {seconds}", max = CorrosionRetryAfterSeconds::MAX)]
pub struct CorrosionRetryAfterSecondsError {
    pub seconds: u8,
}

/// The one additive refusal union shared by v2 responses and terminal SSE events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApiRefusal {
    UnknownSource {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        source: std::net::IpAddr,
    },
    AmbiguousSource {
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        source: std::net::IpAddr,
        candidate_count: usize,
    },
    UnsupportedRoute,
    UnsupportedMethod {
        method: String,
    },
    MissingCluster,
    InvalidCluster,
    CorrosionUnavailable {
        retry_after_seconds: CorrosionRetryAfterSeconds,
    },
}

impl From<SourcePrincipalResolutionError> for ApiRefusal {
    fn from(error: SourcePrincipalResolutionError) -> Self {
        match error {
            SourcePrincipalResolutionError::UnknownSource { source } => {
                Self::UnknownSource { source }
            }
            SourcePrincipalResolutionError::AmbiguousSource {
                source,
                candidate_count,
            } => Self::AmbiguousSource {
                source,
                candidate_count,
            },
        }
    }
}

/// The public SSE event name for an initial full lens snapshot.
pub const LENS_SNAPSHOT_EVENT: &str = "snapshot";
/// The public SSE event name for a subsequent full lens state.
pub const LENS_STATE_EVENT: &str = "state";
/// The public SSE event name for a terminal typed refusal.
pub const LENS_TERMINAL_EVENT: &str = "terminal";

/// The SSE envelope for a lens watch.
///
/// A watch that obtains its initial state begins with [`Self::Snapshot`]. A
/// watch that cannot obtain that state instead emits [`Self::Terminal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LensWatchEvent {
    Snapshot { snapshot: LensSnapshot },
    State { snapshot: LensSnapshot },
    Terminal { refusal: ApiRefusal },
}

impl LensWatchEvent {
    /// Creates an initial full-state stream event.
    #[must_use]
    pub fn snapshot(snapshot: LensSnapshot) -> Self {
        Self::Snapshot { snapshot }
    }

    /// Creates a later full-state stream event.
    #[must_use]
    pub fn state(snapshot: LensSnapshot) -> Self {
        Self::State { snapshot }
    }

    /// Creates a terminal stream event with the shared refusal shape.
    #[must_use]
    pub fn terminal(refusal: ApiRefusal) -> Self {
        Self::Terminal { refusal }
    }

    /// Returns the exact SSE event name for this envelope.
    #[must_use]
    pub const fn event_name(&self) -> &'static str {
        match self {
            Self::Snapshot { .. } => LENS_SNAPSHOT_EVENT,
            Self::State { .. } => LENS_STATE_EVENT,
            Self::Terminal { .. } => LENS_TERMINAL_EVENT,
        }
    }
}

#[cfg(test)]
#[path = "v2_tests.rs"]
mod tests;
