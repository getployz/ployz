//! Public HTTP/JSON/SSE contract for the coreless v2 API.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::corrosion::{
    ClusterDocument, ContainerDocument, MachineDocument, MachineStatusDocument, OperationDocument,
    Principal, ServiceDocument, SourcePrincipalResolutionError,
};
use crate::ids::{ContainerId, MachineRowId, OperationRowId, PeerId, ServiceRowId, TokenId};
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
/// Stable endpoint for fencing one machine from the roster and sweeping its testimony.
pub const MACHINE_REMOVE_ROUTE: &str = "/machines/remove";
/// Stable endpoint for removing one operator peer from the roster.
pub const PEER_REMOVE_ROUTE: &str = "/peers/remove";
/// Stable endpoint for a caller-paced upgrade of the answering machine.
pub const MACHINE_UPGRADE_ROUTE: &str = "/machines/upgrade";
/// The only route exposed by the public TLS join door.
pub const JOIN_ROUTE: &str = "/join";

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
    #[serde(rename = "v2.machine_remove")]
    MachineRemove,
    #[serde(rename = "v2.peer_remove")]
    PeerRemove,
    #[serde(rename = "v2.machine_upgrade")]
    MachineUpgrade,
    #[serde(rename = "v2.join_door")]
    JoinDoor,
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
            Self::MachineRemove => "v2.machine_remove",
            Self::PeerRemove => "v2.peer_remove",
            Self::MachineUpgrade => "v2.machine_upgrade",
            Self::JoinDoor => "v2.join_door",
        }
    }
}

/// Every capability this version of Core knows how to name.
pub const KNOWN_API_FEATURES: &[KnownApiFeature] = &[
    KnownApiFeature::Founding,
    KnownApiFeature::Lenses,
    KnownApiFeature::JoinTokens,
    KnownApiFeature::MachineEndpoint,
    KnownApiFeature::MachineRemove,
    KnownApiFeature::PeerRemove,
    KnownApiFeature::MachineUpgrade,
    KnownApiFeature::JoinDoor,
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
    let accepted = accepted.into_iter().collect::<Vec<_>>();
    let requested_id_is_accepted = request.machine_id.as_ref().is_some_and(|machine_id| {
        accepted
            .iter()
            .any(|(accepted_machine_id, _)| accepted_machine_id == machine_id)
    });
    let mut candidates = accepted
        .into_iter()
        .filter_map(|(machine_id, machine_name)| {
            (machine_name == request.machine_name).then_some(machine_id)
        })
        .collect::<Vec<_>>();
    candidates.sort();

    let Some(expected_machine_id) = &request.machine_id else {
        return match candidates.as_slice() {
            [] => Err(MachineRemoveRefusal::NotFound {
                machine_name: request.machine_name.clone(),
            }),
            [machine_id] => Ok(machine_id.clone()),
            _ => Err(MachineRemoveRefusal::Ambiguous {
                machine_name: request.machine_name.clone(),
                machine_ids: candidates,
            }),
        };
    };

    if candidates
        .iter()
        .any(|machine_id| machine_id == expected_machine_id)
    {
        return Ok(expected_machine_id.clone());
    }
    if candidates.is_empty() {
        if requested_id_is_accepted {
            return Err(MachineRemoveRefusal::IdMismatch {
                machine_name: request.machine_name.clone(),
                machine_id: expected_machine_id.clone(),
            });
        }
        return Err(MachineRemoveRefusal::NotFound {
            machine_name: request.machine_name.clone(),
        });
    }
    Err(MachineRemoveRefusal::IdMismatch {
        machine_name: request.machine_name.clone(),
        machine_id: expected_machine_id.clone(),
    })
}

/// Mesh-authenticated request to remove one operator peer from the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PeerRemoveRequest {
    pub peer_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_id: Option<PeerId>,
}

/// The terminal outcome of an operator-peer removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerRemoveReply {
    Removed { peer_id: PeerId },
    AlreadyAbsent { peer_id: PeerId },
}

/// A refusal to resolve or authorize an operator-peer removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PeerRemoveRefusal {
    #[error("peer {peer_name} is not in the accepted roster")]
    NotFound { peer_name: String },
    #[error("peer {peer_name} is ambiguous across identities {peer_ids:?}")]
    Ambiguous {
        peer_name: String,
        peer_ids: Vec<PeerId>,
    },
    #[error("peer {peer_name} does not match identity {peer_id}")]
    IdMismatch { peer_name: String, peer_id: PeerId },
    #[error(
        "peer {peer_name} ({peer_id}) is the authenticated caller; run `ployz peer rm {peer_name} --id {peer_id} --target <context>` from another accepted operator context"
    )]
    SelfRemoval { peer_name: String, peer_id: PeerId },
}

/// Resolves a peer-removal selector only from roster rows accepted by the
/// reader law. The optional row id makes retries and name conflicts explicit.
pub fn select_peer_removal(
    request: &PeerRemoveRequest,
    accepted: impl IntoIterator<Item = (PeerId, String)>,
) -> Result<PeerId, PeerRemoveRefusal> {
    let accepted = accepted.into_iter().collect::<Vec<_>>();
    let requested_id_is_accepted = request.peer_id.as_ref().is_some_and(|peer_id| {
        accepted
            .iter()
            .any(|(accepted_peer_id, _)| accepted_peer_id == peer_id)
    });
    let mut candidates = accepted
        .into_iter()
        .filter_map(|(peer_id, peer_name)| (peer_name == request.peer_name).then_some(peer_id))
        .collect::<Vec<_>>();
    candidates.sort();

    let Some(expected_peer_id) = &request.peer_id else {
        return match candidates.as_slice() {
            [] => Err(PeerRemoveRefusal::NotFound {
                peer_name: request.peer_name.clone(),
            }),
            [peer_id] => Ok(peer_id.clone()),
            _ => Err(PeerRemoveRefusal::Ambiguous {
                peer_name: request.peer_name.clone(),
                peer_ids: candidates,
            }),
        };
    };

    if candidates.iter().any(|peer_id| peer_id == expected_peer_id) {
        return Ok(expected_peer_id.clone());
    }
    if candidates.is_empty() && !requested_id_is_accepted {
        return Err(PeerRemoveRefusal::NotFound {
            peer_name: request.peer_name.clone(),
        });
    }
    Err(PeerRemoveRefusal::IdMismatch {
        peer_name: request.peer_name.clone(),
        peer_id: expected_peer_id.clone(),
    })
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
    MachineRemove,
    PeerRemove,
    MachineUpgrade,
    Join,
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
        if path == MACHINE_ENDPOINT_ROUTE_PREFIX {
            return Some(Self::MachineEndpointSet);
        }
        if path == MACHINE_REMOVE_ROUTE {
            return Some(Self::MachineRemove);
        }
        if path == PEER_REMOVE_ROUTE {
            return Some(Self::PeerRemove);
        }
        if path == MACHINE_UPGRADE_ROUTE {
            return Some(Self::MachineUpgrade);
        }
        if let Some(token_id) = path
            .strip_prefix(TOKEN_REVOKE_ROUTE_PREFIX)
            .and_then(|suffix| suffix.strip_prefix('/'))
            .and_then(|id| TokenId::try_new(id).ok())
        {
            return Some(Self::TokenRevoke(token_id));
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
            Self::MachineRemove => MACHINE_REMOVE_ROUTE.to_owned(),
            Self::PeerRemove => PEER_REMOVE_ROUTE.to_owned(),
            Self::MachineUpgrade => MACHINE_UPGRADE_ROUTE.to_owned(),
            Self::Join => JOIN_ROUTE.to_owned(),
            Self::Lens(collection) => lens_route(*collection),
            Self::LensWatch(collection) => lens_watch_route(*collection),
        }
    }

    /// Returns the one HTTP method accepted by this route.
    #[must_use]
    pub const fn method(&self) -> V2Method {
        match self {
            Self::Version | Self::Lens(_) | Self::LensWatch(_) => V2Method::Get,
            Self::Founding
            | Self::TokenCreate
            | Self::TokenList
            | Self::TokenRevoke(_)
            | Self::MachineEndpointSet
            | Self::MachineRemove
            | Self::PeerRemove
            | Self::MachineUpgrade
            | Self::Join => V2Method::Post,
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
            Self::MachineRemove => KnownApiFeature::MachineRemove,
            Self::PeerRemove => KnownApiFeature::PeerRemove,
            Self::MachineUpgrade => KnownApiFeature::MachineUpgrade,
            Self::Join => KnownApiFeature::JoinDoor,
        }
    }

    /// Enforces the authority assigned to each API surface.
    #[must_use]
    pub const fn accepts_principal(&self, principal: &Principal) -> bool {
        match self {
            Self::Join => matches!(principal, Principal::ApiToken { .. }),
            Self::TokenCreate
            | Self::TokenList
            | Self::TokenRevoke(_)
            | Self::MachineEndpointSet
            | Self::MachineRemove
            | Self::PeerRemove
            | Self::MachineUpgrade => matches!(principal, Principal::Peer { .. }),
            Self::Version | Self::Founding | Self::Lens(_) | Self::LensWatch(_) => {
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
