//! Public HTTP/JSON/SSE contract for the coreless v2 API.

use serde::{Deserialize, Serialize};
use url::Url;

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::ops::{Deref, DerefMut};

use crate::corrosion::{
    ClusterDocument, CorrosionNamespaceName, CorrosionServiceName, HostPortBindings,
    MachineDocument, MachineEndpointDocument, MachineStatusDocument, NamespaceDocument,
    OperationDocument, Principal, PublishedService, ServiceReplicaCount,
    SourcePrincipalResolutionError, V2ManagedContainerIdentity,
};
use crate::deploy::{ContainerRuntimeSpec, ImageReference, RegistryCredential};
use crate::ids::{DeployName, TokenName};
use crate::install::{InstallArtifactVersion, InstallSha256Digest};
use crate::machine::MachineName;

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
/// Stable endpoint for inspecting one target host before deploy planning.
pub const DEPLOY_INSPECT_ROUTE: &str = "/deploy/inspect";
/// Stable endpoint for preparing exact replicas on one target host.
pub const DEPLOY_PREPARE_ROUTE: &str = "/deploy/prepare";
/// Stable endpoint for retiring exact observed containers on one target host.
pub const DEPLOY_RETIRE_ROUTE: &str = "/deploy/retire";
/// Stable prefix for service log access.
pub const SERVICE_LOGS_ROUTE_PREFIX: &str = "/services";
/// Machine-only endpoint for resolving service-log ownership from local runtime reality.
pub const SERVICE_LOGS_PROBE_ROUTE: &str = "/services/logs/probe";
/// The stable endpoint for the cheap cluster diagnostics projection.
pub const STATUS_ROUTE: &str = "/status";
/// The stable endpoint for the read-only deep diagnostics projection.
pub const DOCTOR_ROUTE: &str = "/doctor";
/// Stable endpoint for removing one valid peer row.
pub const PEER_REMOVE_ROUTE: &str = "/peers/remove";
/// Stable endpoint for removing one service from its namespace document.
pub const SERVICE_REMOVE_ROUTE: &str = "/services/remove";
/// Stable endpoint for removing one valid route-binding row.
pub const ROUTE_REMOVE_ROUTE: &str = "/routes/remove";
/// Stable endpoint for attaching one hostname to one named service.
pub const ROUTE_ATTACH_ROUTE: &str = "/routes/attach";

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
    #[serde(rename = "v2.operation_status")]
    OperationStatus,
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
    #[serde(rename = "v2.route_attach")]
    RouteAttach,
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
            Self::OperationStatus => "v2.operation_status",
            Self::Logs => "v2.logs",
            Self::Diagnostics => "v2.diagnostics",
            Self::PeerRemove => "v2.peer_remove",
            Self::ServiceRemove => "v2.service_remove",
            Self::RouteRemove => "v2.route_remove",
            Self::RouteAttach => "v2.route_attach",
        }
    }
}

/// Every capability this API version knows how to name.
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
    KnownApiFeature::OperationStatus,
    KnownApiFeature::Logs,
    KnownApiFeature::Diagnostics,
    KnownApiFeature::PeerRemove,
    KnownApiFeature::ServiceRemove,
    KnownApiFeature::RouteRemove,
    KnownApiFeature::RouteAttach,
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
    /// Creates an additive capability name not yet known by this API version.
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
    Endpoints,
    MachineStatus,
    Operations,
}

impl LensCollection {
    /// Every collection in stable route order.
    pub const ALL: &'static [Self] = &[
        Self::Machines,
        Self::Services,
        Self::Endpoints,
        Self::MachineStatus,
        Self::Operations,
    ];

    /// The collection's stable path segment.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Machines => "machines",
            Self::Services => "services",
            Self::Endpoints => "endpoints",
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
            "endpoints" => Some(Self::Endpoints),
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
pub fn token_revoke_route(token_id: &TokenName) -> String {
    format!("{TOKEN_REVOKE_ROUTE_PREFIX}/{token_id}")
}

/// Builds the bounded log-tail route for one named service.
#[must_use]
pub fn service_logs_tail_route(
    namespace_name: &CorrosionNamespaceName,
    service_name: &CorrosionServiceName,
) -> String {
    format!("{SERVICE_LOGS_ROUTE_PREFIX}/{namespace_name}/{service_name}/logs")
}

/// Builds the lossy follow route for one named service's current containers.
#[must_use]
pub fn service_logs_follow_route(
    namespace_name: &CorrosionNamespaceName,
    service_name: &CorrosionServiceName,
) -> String {
    format!(
        "{}/follow",
        service_logs_tail_route(namespace_name, service_name)
    )
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
    pub namespace_name: CorrosionNamespaceName,
    pub document: NamespaceDocument,
}

/// A namespace create that cannot claim its human name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrosionNamespaceCreateRefusal {
    AlreadyExists {
        namespace_name: CorrosionNamespaceName,
    },
}

/// Mesh-authenticated selector for removing one exact empty namespace row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CorrosionNamespaceRemoveRequest {
    pub namespace_name: CorrosionNamespaceName,
}

/// The synchronous result of removing an exact namespace row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrosionNamespaceRemoveReply {
    Removed {
        namespace_name: CorrosionNamespaceName,
    },
    AlreadyAbsent {
        namespace_name: CorrosionNamespaceName,
    },
}

/// A namespace removal refusal; removal never performs workload cleanup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorrosionNamespaceRemoveRefusal {
    NotFound {
        namespace_name: CorrosionNamespaceName,
    },
    NotEmpty {
        namespace_name: CorrosionNamespaceName,
        service_names: Vec<CorrosionServiceName>,
        route_binding_count: usize,
    },
    Changed {
        namespace_name: CorrosionNamespaceName,
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

/// One service declaration in a namespace-wide desired-state snapshot.
///
/// Environment values are redacted by their value type and are never
/// copied into durable operation evidence or Corrosion rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployServiceRequest {
    pub image: ImageReference,
    /// A deploy-scoped pull credential. It may enter node-local workflow
    /// history, but is never copied into Corrosion or operation rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<RegistryCredential>,
    pub runtime: ContainerRuntimeSpec,
    #[serde(default)]
    pub health_gate: HealthGatePolicy,
    /// Omission selects the fixed replicated/one default. A deploy is a
    /// complete snapshot and never inherits an incumbent service's mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<RequestedPlacement>,
    /// Omission selects the fixed unpinned default. A deploy is a complete
    /// snapshot and never inherits incumbent machine pins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machines: Option<PinnedMachineNames>,
}

/// The complete name-keyed service set in one deploy request.
///
/// The object representation makes service identity structural. An empty
/// object requests removal of every service.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(type = "Record<CorrosionServiceName, DeployServiceRequest>")
)]
#[serde(transparent)]
pub struct DeployServices(BTreeMap<CorrosionServiceName, DeployServiceRequest>);

impl FromIterator<(CorrosionServiceName, DeployServiceRequest)> for DeployServices {
    fn from_iter<T: IntoIterator<Item = (CorrosionServiceName, DeployServiceRequest)>>(
        iter: T,
    ) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl Deref for DeployServices {
    type Target = BTreeMap<CorrosionServiceName, DeployServiceRequest>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for DeployServices {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The complete desired state for one namespace reconciliation attempt.
///
/// Every deploy names every service that should remain in the namespace.
/// Omitting an incumbent removes it from the serving projection after every
/// requested replacement has prepared successfully.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployRequest {
    pub namespace_name: CorrosionNamespaceName,
    /// Caller-chosen namespace-scoped identity for this deploy attempt.
    pub deploy_name: DeployName,
    pub services: DeployServices,
}

/// Requested placement intent. Host-published ports exist only on the global
/// variant, so a replicated deploy with published ports is unrepresentable
/// on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestedPlacement {
    Replicated {
        replicas: ServiceReplicaCount,
    },
    Global {
        #[serde(default, skip_serializing_if = "HostPortBindings::is_empty")]
        host_ports: HostPortBindings,
    },
}

/// A non-empty set of machine names to pin a service to. Omission means
/// unpinned placement.
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

/// The operation handle returned after the controller accepts a deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployAccepted {
    pub namespace_name: CorrosionNamespaceName,
    pub deploy_name: DeployName,
    pub controller_machine_name: MachineName,
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
    DeployNameAlreadyUsed {
        namespace_name: CorrosionNamespaceName,
        deploy_name: DeployName,
    },
    HostPortConflict {
        host_port: u16,
        protocol: crate::corrosion::HostPortProtocol,
        first_service: CorrosionServiceName,
        second_service: CorrosionServiceName,
    },
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

/// Fresh target-host deploy facts or one bounded local failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployInspectOutcome {
    Inspected {
        bridge_ready: bool,
        free_disk_bytes: u64,
        load: crate::corrosion::MachineLoadBand,
        containers: Vec<DeployObservedContainer>,
    },
    Failed,
}

/// One exact desired replica on the answering target host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployDesiredReplica {
    pub identity: V2ManagedContainerIdentity,
    #[serde(default, skip_serializing_if = "HostPortBindings::is_empty")]
    pub host_ports: HostPortBindings,
}

/// One exact observed container that a deploy may stop or remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployObservedContainer {
    pub identity: V2ManagedContainerIdentity,
    /// Whether the container was running in the controller's inspection.
    pub running: bool,
    #[serde(default, skip_serializing_if = "HostPortBindings::is_empty")]
    pub host_ports: HostPortBindings,
}

/// One target host's complete service preparation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPrepareRequest {
    pub controller_machine_name: MachineName,
    /// Namespace-scoped deploy identity; every replica must carry it.
    pub operation_id: DeployName,
    pub namespace_name: CorrosionNamespaceName,
    pub service_name: CorrosionServiceName,
    pub image: ImageReference,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<RegistryCredential>,
    pub runtime: ContainerRuntimeSpec,
    pub health_gate: HealthGatePolicy,
    pub replicas: Vec<DeployDesiredReplica>,
    /// Exact observed containers whose published ports overlap this service's
    /// desired bindings. The target creates replacements before stopping them.
    pub stop_before_start: Vec<DeployObservedContainer>,
}

/// One exact replica prepared and creation-gated on its target host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPreparedReplica {
    pub identity: V2ManagedContainerIdentity,
    pub ip: Ipv4Addr,
}

/// The terminal answer from one target-host preparation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployPrepareOutcome {
    Prepared {
        /// Authenticated controller that originated this durable prepare.
        controller_machine_name: MachineName,
        /// The canonical digest-pinned image used for every returned replica.
        image: ImageReference,
        replicas: Vec<DeployPreparedReplica>,
        /// Exact incumbents stopped by this preparation that must be restarted
        /// if the controller abandons the deploy before its state commit.
        displaced_incumbents: Vec<DeployObservedContainer>,
    },
    Refused,
    Failed,
}

/// Machine-authenticated request to retire exact observed containers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRetireRequest {
    pub controller_machine_name: MachineName,
    /// Namespace-scoped deploy identity for this cleanup request.
    pub operation_id: DeployName,
    pub namespace_name: CorrosionNamespaceName,
    pub containers: Vec<DeployObservedContainer>,
    /// Exact displaced incumbents to restart after removing `containers`.
    pub restart_after_retire: Vec<DeployObservedContainer>,
    /// Service prepares to roll back from target-local durable outcomes. When
    /// non-empty, both caller-supplied container lists must be empty; the
    /// target derives the exact cleanup and restart identities itself.
    pub rollback_services: Vec<CorrosionServiceName>,
}

/// The terminal answer from one target-host retirement attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeployRetireOutcome {
    Retired,
    Refused,
    Failed,
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
        namespace_name: CorrosionNamespaceName,
        service_name: CorrosionServiceName,
    },
    ContainerNotFound {
        namespace_name: CorrosionNamespaceName,
        service_name: CorrosionServiceName,
    },
    /// The service runs containers on more than one machine (or the request's
    /// machine selector matched none of them); the listed machine names carry
    /// one entry per container, so stacked replicas repeat their host.
    MachineSelectorRequired {
        machines: Vec<MachineName>,
    },
    RemoteOwner {
        machine_name: MachineName,
    },
    RuntimeUnavailable {
        machine_name: MachineName,
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
}

/// The terminal outcome of a machine-removal fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineRemoveReply {
    Removed { machine_name: MachineName },
}

/// A refusal to resolve a machine-removal selector against accepted roster rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineRemoveRefusal {
    NotFound { machine_name: MachineName },
    ConcurrentMutation { machine_name: MachineName },
}

/// Resolves a removal selector only from roster rows already accepted by the
/// reader law. The machine name is the row key.
pub fn select_machine_removal(
    request: &MachineRemoveRequest,
    accepted: impl IntoIterator<Item = MachineName>,
) -> Result<MachineName, MachineRemoveRefusal> {
    accepted
        .into_iter()
        .find(|name| name == &request.machine_name)
        .ok_or_else(|| MachineRemoveRefusal::NotFound {
            machine_name: request.machine_name.clone(),
        })
}

/// The exact public route shape, parsed without any daemon-local strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V2Route {
    Version,
    Founding,
    TokenCreate,
    TokenList,
    TokenRevoke(TokenName),
    MachineEndpointSet,
    MachineUpgrade,
    MachineRemove,
    Join,
    NamespaceCreate,
    NamespaceRemove,
    Deploy,
    DeployInspect,
    DeployPrepare,
    DeployRetire,
    ServiceLogsProbe,
    ServiceLogsTail(CorrosionNamespaceName, CorrosionServiceName),
    ServiceLogsFollow(CorrosionNamespaceName, CorrosionServiceName),
    Status,
    Doctor,
    PeerRemove,
    ServiceRemove,
    RouteRemove,
    RouteAttach,
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
    /// Whether this request changes cluster truth and therefore belongs on the
    /// preferred controller. Target-local host effects are deliberately not
    /// included.
    #[must_use]
    pub const fn is_controller_mutation(&self) -> bool {
        matches!(
            self,
            Self::Join
                | Self::TokenCreate
                | Self::TokenRevoke(_)
                | Self::MachineEndpointSet
                | Self::MachineRemove
                | Self::NamespaceCreate
                | Self::NamespaceRemove
                | Self::Deploy
                | Self::PeerRemove
                | Self::ServiceRemove
                | Self::RouteRemove
                | Self::RouteAttach
        )
    }

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
        if path == SERVICE_REMOVE_ROUTE {
            return Some(Self::ServiceRemove);
        }
        if path == ROUTE_REMOVE_ROUTE {
            return Some(Self::RouteRemove);
        }
        if path == ROUTE_ATTACH_ROUTE {
            return Some(Self::RouteAttach);
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
        if path == DEPLOY_INSPECT_ROUTE {
            return Some(Self::DeployInspect);
        }
        if path == DEPLOY_PREPARE_ROUTE {
            return Some(Self::DeployPrepare);
        }
        if path == DEPLOY_RETIRE_ROUTE {
            return Some(Self::DeployRetire);
        }
        if path == SERVICE_LOGS_PROBE_ROUTE {
            return Some(Self::ServiceLogsProbe);
        }
        if path == MACHINE_REMOVE_ROUTE {
            return Some(Self::MachineRemove);
        }
        if let Some(token_id) = path
            .strip_prefix(TOKEN_REVOKE_ROUTE_PREFIX)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .and_then(|id| TokenName::try_new(id).ok())
        {
            return Some(Self::TokenRevoke(token_id));
        }
        if let Some(service_path) = path
            .strip_prefix(SERVICE_LOGS_ROUTE_PREFIX)
            .and_then(|suffix| suffix.strip_prefix('/'))
        {
            let mut segments = service_path.split('/');
            let namespace_name = segments
                .next()
                .and_then(|id| CorrosionNamespaceName::try_new(id).ok())?;
            let service_name = segments
                .next()
                .and_then(|name| CorrosionServiceName::try_new(name).ok())?;
            if segments.next() != Some("logs") {
                return None;
            }
            return match segments.next() {
                None => Some(Self::ServiceLogsTail(namespace_name, service_name)),
                Some("follow") if segments.next().is_none() => {
                    Some(Self::ServiceLogsFollow(namespace_name, service_name))
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
            Self::DeployInspect => DEPLOY_INSPECT_ROUTE.to_owned(),
            Self::DeployPrepare => DEPLOY_PREPARE_ROUTE.to_owned(),
            Self::DeployRetire => DEPLOY_RETIRE_ROUTE.to_owned(),
            Self::ServiceLogsProbe => SERVICE_LOGS_PROBE_ROUTE.to_owned(),
            Self::ServiceLogsTail(namespace, service) => {
                service_logs_tail_route(namespace, service)
            }
            Self::ServiceLogsFollow(namespace, service) => {
                service_logs_follow_route(namespace, service)
            }
            Self::Status => STATUS_ROUTE.to_owned(),
            Self::Doctor => DOCTOR_ROUTE.to_owned(),
            Self::PeerRemove => PEER_REMOVE_ROUTE.to_owned(),
            Self::ServiceRemove => SERVICE_REMOVE_ROUTE.to_owned(),
            Self::RouteRemove => ROUTE_REMOVE_ROUTE.to_owned(),
            Self::RouteAttach => ROUTE_ATTACH_ROUTE.to_owned(),
            Self::Lens(collection) => lens_route(*collection),
            Self::LensWatch(collection) => lens_watch_route(*collection),
        }
    }

    /// Returns the one HTTP method accepted by this route.
    #[must_use]
    pub const fn method(&self) -> V2Method {
        match self {
            Self::Version | Self::Status | Self::Doctor | Self::Lens(_) | Self::LensWatch(_) => {
                V2Method::Get
            }
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
            | Self::DeployInspect
            | Self::DeployPrepare
            | Self::DeployRetire
            | Self::ServiceLogsProbe
            | Self::ServiceLogsTail(_, _)
            | Self::ServiceLogsFollow(_, _)
            | Self::MachineRemove
            | Self::PeerRemove
            | Self::ServiceRemove
            | Self::RouteRemove => V2Method::Post,
            Self::RouteAttach => V2Method::Post,
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
            Self::Deploy | Self::DeployInspect | Self::DeployPrepare | Self::DeployRetire => {
                KnownApiFeature::Deploy
            }
            Self::ServiceLogsProbe
            | Self::ServiceLogsTail(_, _)
            | Self::ServiceLogsFollow(_, _) => KnownApiFeature::Logs,
            Self::Status | Self::Doctor => KnownApiFeature::Diagnostics,
            Self::PeerRemove => KnownApiFeature::PeerRemove,
            Self::ServiceRemove => KnownApiFeature::ServiceRemove,
            Self::RouteRemove => KnownApiFeature::RouteRemove,
            Self::RouteAttach => KnownApiFeature::RouteAttach,
        }
    }

    /// Enforces the authority assigned to each API surface.
    #[must_use]
    pub const fn accepts_principal(&self, principal: &Principal) -> bool {
        match self {
            Self::Join => matches!(
                principal,
                Principal::Machine { .. } | Principal::ApiToken { .. }
            ),
            Self::DeployInspect
            | Self::DeployPrepare
            | Self::DeployRetire
            | Self::ServiceLogsProbe => {
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
            Self::RouteAttach => matches!(principal, Principal::Peer { .. }),
            Self::Version
            | Self::Founding
            | Self::ServiceLogsTail(_, _)
            | Self::ServiceLogsFollow(_, _)
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

/// One named service entry flattened from a Namespace intent document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ServiceLensRow {
    pub key: String,
    pub document: PublishedService,
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
        cluster: Box<ClusterDocument>,
        rows: Vec<MachineDocument>,
    },
    Services {
        rows: Vec<ServiceLensRow>,
    },
    Endpoints {
        rows: Vec<MachineEndpointDocument>,
    },
    MachineStatus {
        rows: Vec<MachineStatusDocument>,
    },
    Operations {
        rows: Vec<OperationDocument>,
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
