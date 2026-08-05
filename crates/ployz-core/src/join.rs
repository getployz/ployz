//! Join-token, public-door, and machine-arrival contracts.

use std::collections::BTreeSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::corrosion::{
    ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp, MachineDocument,
    MachineStorageIneligibleReason, MachineStorageSelection, MachineStorageSelectionReason,
    MachineTransport, MeshProvider, OperationInitiator, OperatorWriteProvenance, PeerDocument,
    PeerTransport, Sha256Hex, StorageMode, TokenDocument, derive_builtin_wireguard_member,
};
use crate::founding::MINIMUM_ZFS_MEMORY_BYTES;
use crate::ids::{MachineRowId, PeerId, TokenId};
use crate::install::{ExactPloyzVersion, InstallArtifactSpec};
use crate::machine::{MachineLifecycle, MachineName};
use crate::network::{MachineEndpointSubnet, WireGuardPublicKey};

/// The fixed TCP port served by every public join door.
pub const JOIN_DOOR_PORT: u16 = 2_021;
/// Alias that distinguishes the public TCP door from a machine's WireGuard UDP port.
pub const JOIN_DOOR_TCP_PORT: u16 = JOIN_DOOR_PORT;
/// Default lifetime for a newly minted join token.
pub const DEFAULT_JOIN_TOKEN_TTL_SECONDS: u32 = 24 * 60 * 60;
/// Longest join-token lifetime accepted by the public contract.
pub const MAX_JOIN_TOKEN_TTL_SECONDS: u32 = 30 * 24 * 60 * 60;
/// The member ceiling also bounds the endpoint set embedded in one join blob.
pub const MAX_JOIN_DOOR_ENDPOINTS: usize = 256;
/// The repair primitive for a host carrying state from another cluster.
pub const JOIN_RESET_COMMAND: &str = "ployz machine reset";
/// The exact primitive token creation names when no public door is advertised.
pub const MACHINE_ENDPOINT_SET_COMMAND: &str =
    "ployz machine endpoint set <machine> <ip:wireguard-port>";

const JOIN_BLOB_PREFIX: &str = "pzjoin_";
const JOIN_BLOB_VERSION: u8 = 1;
const JOIN_TOKEN_SECRET_BYTES: usize = 32;

/// A 32-byte random join secret. Explicit exposure is required to obtain it.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"JoinTokenSecret\">"))]
pub struct JoinTokenSecret([u8; JOIN_TOKEN_SECRET_BYTES]);

impl JoinTokenSecret {
    #[must_use]
    pub const fn try_from_bytes(bytes: [u8; JOIN_TOKEN_SECRET_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn try_new(value: impl AsRef<str>) -> Result<Self, JoinTokenSecretError> {
        let value = value.as_ref();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| JoinTokenSecretError::InvalidEncoding)?;
        let bytes: [u8; JOIN_TOKEN_SECRET_BYTES] = decoded
            .try_into()
            .map_err(|_| JoinTokenSecretError::InvalidLength)?;
        let secret = Self(bytes);
        if secret.expose_base64() != value {
            return Err(JoinTokenSecretError::NonCanonical);
        }
        Ok(secret)
    }

    #[must_use]
    pub fn expose_base64(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0)
    }

    #[must_use]
    pub fn sha256(&self) -> JoinTokenSecretHash {
        let digest = Sha256::digest(self.0);
        JoinTokenSecretHash(
            Sha256Hex::try_new(format!("{digest:x}"))
                .expect("a formatted SHA-256 digest is canonical"),
        )
    }

    /// Compares a row digest without returning early on a mismatching byte.
    #[must_use]
    pub fn matches_sha256(&self, expected: &Sha256Hex) -> bool {
        constant_time_digest_eq(self.sha256().as_str(), expected.as_str())
    }
}

impl fmt::Debug for JoinTokenSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JoinTokenSecret([redacted])")
    }
}

impl Serialize for JoinTokenSecret {
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        serializer.serialize_str(&self.expose_base64())
    }
}

impl<'de> Deserialize<'de> for JoinTokenSecret {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JoinTokenSecretError {
    #[error("join-token secret is not URL-safe base64")]
    InvalidEncoding,
    #[error("join-token secret must contain exactly 32 bytes")]
    InvalidLength,
    #[error("join-token secret is not in canonical unpadded URL-safe base64")]
    NonCanonical,
}

/// The one-way token value stored in the `tokens` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"JoinTokenSecretHash\">"))]
#[serde(transparent)]
pub struct JoinTokenSecretHash(Sha256Hex);

impl JoinTokenSecretHash {
    pub fn try_new(value: impl Into<String>) -> Result<Self, crate::corrosion::Sha256HexError> {
        Ok(Self(Sha256Hex::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn into_sha256_hex(self) -> Sha256Hex {
        self.0
    }

    /// Compares a presented 256-bit secret without an early-exit byte comparison.
    #[must_use]
    pub fn matches_secret(&self, presented: &JoinTokenSecret) -> bool {
        constant_time_digest_eq(self.as_str(), presented.sha256().as_str())
    }
}

fn constant_time_digest_eq(left: &str, right: &str) -> bool {
    let difference = left
        .bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        });
    std::hint::black_box(difference) == 0 && left.len() == right.len()
}

/// The pinned SHA-256 digest of the cluster door certificate DER.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(type = "Brand<string, \"JoinDoorCertFingerprint\">")
)]
#[serde(transparent)]
pub struct JoinDoorCertFingerprint(Sha256Hex);

impl JoinDoorCertFingerprint {
    pub fn try_new(value: impl Into<String>) -> Result<Self, crate::corrosion::Sha256HexError> {
        Ok(Self(Sha256Hex::try_new(value)?))
    }

    #[must_use]
    pub fn for_certificate_der(certificate_der: &[u8]) -> Self {
        let digest = Sha256::digest(certificate_der);
        Self::try_new(format!("{digest:x}")).expect("a formatted SHA-256 digest is canonical")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns the digest bytes consumed by a pinned-certificate verifier.
    #[must_use]
    pub fn decoded_bytes(&self) -> [u8; 32] {
        let mut decoded = [0_u8; 32];
        for (output, pair) in decoded
            .iter_mut()
            .zip(self.as_str().as_bytes().chunks_exact(2))
        {
            let [high, low] = pair else {
                unreachable!("SHA-256 hexadecimal pairs contain two bytes")
            };
            *output = (hex_nibble(*high) << 4) | hex_nibble(*low);
        }
        decoded
    }
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!("Sha256Hex guarantees lowercase hexadecimal"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinBlobPayload {
    v: u8,
    token_id: TokenId,
    secret: JoinTokenSecret,
    door_cert_sha256: JoinDoorCertFingerprint,
    endpoints: AdvertisedJoinDoorEndpoints,
}

/// The show-once opaque credential accepted by `ployz machine join`.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"JoinBlob\">"))]
pub struct JoinBlob {
    encoded: String,
    payload: JoinBlobPayload,
}

impl JoinBlob {
    pub fn try_new(
        token_id: TokenId,
        secret: JoinTokenSecret,
        door_cert_sha256: JoinDoorCertFingerprint,
        endpoints: Vec<SocketAddr>,
    ) -> Result<Self, JoinBlobError> {
        let payload = JoinBlobPayload {
            v: JOIN_BLOB_VERSION,
            token_id,
            secret,
            door_cert_sha256,
            endpoints: AdvertisedJoinDoorEndpoints::try_new(endpoints)?,
        };
        Self::from_payload(payload)
    }

    fn from_payload(payload: JoinBlobPayload) -> Result<Self, JoinBlobError> {
        if payload.v != JOIN_BLOB_VERSION {
            return Err(JoinBlobError::UnsupportedVersion { found: payload.v });
        }
        let json = serde_json::to_vec(&payload).map_err(|_| JoinBlobError::InvalidPayload)?;
        let encoded = format!(
            "{JOIN_BLOB_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        );
        Ok(Self { encoded, payload })
    }

    pub fn try_parse(value: impl AsRef<str>) -> Result<Self, JoinBlobError> {
        let value = value.as_ref();
        let encoded = value
            .strip_prefix(JOIN_BLOB_PREFIX)
            .ok_or(JoinBlobError::MissingPrefix)?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| JoinBlobError::InvalidEncoding)?;
        let payload: JoinBlobPayload =
            serde_json::from_slice(&json).map_err(|_| JoinBlobError::InvalidPayload)?;
        let blob = Self::from_payload(payload)?;
        if blob.encoded != value {
            return Err(JoinBlobError::NonCanonical);
        }
        Ok(blob)
    }

    /// Explicitly exposes the show-once opaque blob for presentation or transport.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.encoded
    }

    #[must_use]
    pub fn token_id(&self) -> &TokenId {
        &self.payload.token_id
    }

    #[must_use]
    pub fn secret(&self) -> &JoinTokenSecret {
        &self.payload.secret
    }

    #[must_use]
    pub fn door_cert_fingerprint(&self) -> &JoinDoorCertFingerprint {
        &self.payload.door_cert_sha256
    }

    #[must_use]
    pub fn endpoints(&self) -> &[SocketAddr] {
        self.payload.endpoints.as_slice()
    }
}

impl fmt::Debug for JoinBlob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JoinBlob([redacted])")
    }
}

impl Serialize for JoinBlob {
    fn serialize<Serializer>(
        &self,
        serializer: Serializer,
    ) -> Result<Serializer::Ok, Serializer::Error>
    where
        Serializer: serde::Serializer,
    {
        serializer.serialize_str(self.expose())
    }
}

impl<'de> Deserialize<'de> for JoinBlob {
    fn deserialize<Deserializer>(deserializer: Deserializer) -> Result<Self, Deserializer::Error>
    where
        Deserializer: serde::Deserializer<'de>,
    {
        Self::try_parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinBlobError {
    #[error("join blob must start with pzjoin_")]
    MissingPrefix,
    #[error("join blob is not URL-safe base64")]
    InvalidEncoding,
    #[error("join blob payload is malformed")]
    InvalidPayload,
    #[error("join blob version {found} is unsupported")]
    UnsupportedVersion { found: u8 },
    #[error("join blob is not in its canonical encoding")]
    NonCanonical,
    #[error(transparent)]
    Endpoints(#[from] AdvertisedJoinDoorEndpointsError),
}

/// A validated nonempty set of public join-door addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Array<string>"))]
#[serde(try_from = "Vec<SocketAddr>", into = "Vec<SocketAddr>")]
pub struct AdvertisedJoinDoorEndpoints(Vec<SocketAddr>);

impl AdvertisedJoinDoorEndpoints {
    pub fn try_new(
        mut endpoints: Vec<SocketAddr>,
    ) -> Result<Self, AdvertisedJoinDoorEndpointsError> {
        endpoints.sort_unstable();
        endpoints.dedup();
        if endpoints.is_empty() {
            return Err(AdvertisedJoinDoorEndpointsError::Empty);
        }
        if endpoints.len() > MAX_JOIN_DOOR_ENDPOINTS {
            return Err(AdvertisedJoinDoorEndpointsError::TooMany {
                found: endpoints.len(),
                maximum: MAX_JOIN_DOOR_ENDPOINTS,
            });
        }
        if endpoints
            .iter()
            .any(|endpoint| endpoint.port() != JOIN_DOOR_PORT)
        {
            return Err(AdvertisedJoinDoorEndpointsError::WrongPort);
        }
        Ok(Self(endpoints))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[SocketAddr] {
        &self.0
    }
}

impl TryFrom<Vec<SocketAddr>> for AdvertisedJoinDoorEndpoints {
    type Error = AdvertisedJoinDoorEndpointsError;

    fn try_from(value: Vec<SocketAddr>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AdvertisedJoinDoorEndpoints> for Vec<SocketAddr> {
    fn from(value: AdvertisedJoinDoorEndpoints) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdvertisedJoinDoorEndpointsError {
    #[error("at least one public join-door endpoint is required")]
    Empty,
    #[error("join blob advertises {found} endpoints; maximum is {maximum}")]
    TooMany { found: usize, maximum: usize },
    #[error("every advertised join-door endpoint must use the fixed join-door TCP port")]
    WrongPort,
}

/// Why a token could not be created before any row write occurred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenCreateRefusal {
    NoAdvertisedDoorEndpoint { repair_command: String },
    TooManyAdvertisedDoorEndpoints { found: usize, maximum: usize },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TokenCreateResponse {
    Created { reply: TokenCreateReply },
    Refused { refusal: TokenCreateRefusal },
}

impl fmt::Debug for TokenCreateResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created { reply } => formatter
                .debug_struct("Created")
                .field("token_id", &reply.token_id)
                .field("blob", &"[redacted]")
                .finish(),
            Self::Refused { refusal } => formatter
                .debug_struct("Refused")
                .field("refusal", refusal)
                .finish(),
        }
    }
}

/// Derives public HTTPS addresses from accepted WireGuard endpoint IPs.
pub fn advertise_join_door_endpoints<'machine>(
    machines: impl IntoIterator<Item = &'machine MachineDocument>,
) -> Result<AdvertisedJoinDoorEndpoints, TokenCreateRefusal> {
    let endpoints = machines
        .into_iter()
        .filter_map(|machine| match &machine.transport {
            MachineTransport::Wireguard {
                endpoint: Some(endpoint),
                ..
            } => Some(SocketAddr::new(endpoint.ip(), JOIN_DOOR_PORT)),
            MachineTransport::Wireguard { endpoint: None, .. }
            | MachineTransport::Tailscale { .. } => None,
        })
        .collect::<Vec<_>>();
    AdvertisedJoinDoorEndpoints::try_new(endpoints).map_err(|error| match error {
        AdvertisedJoinDoorEndpointsError::Empty | AdvertisedJoinDoorEndpointsError::WrongPort => {
            TokenCreateRefusal::NoAdvertisedDoorEndpoint {
                repair_command: MACHINE_ENDPOINT_SET_COMMAND.to_owned(),
            }
        }
        AdvertisedJoinDoorEndpointsError::TooMany { found, maximum } => {
            TokenCreateRefusal::TooManyAdvertisedDoorEndpoints { found, maximum }
        }
    })
}

/// Verifies a token row at the one public route where API-token principals exist.
pub fn validate_join_token(
    proof: &JoinTokenProof,
    token_row: Option<(&TokenId, &TokenDocument)>,
    now: CorrosionTimestamp,
) -> Result<crate::corrosion::Principal, JoinDoorRefusal> {
    let Some((row_id, document)) = token_row else {
        return Err(JoinDoorRefusal::TokenNotFound {
            token_id: proof.token_id.clone(),
        });
    };
    if row_id != &proof.token_id {
        return Err(JoinDoorRefusal::TokenNotFound {
            token_id: proof.token_id.clone(),
        });
    }
    if now >= document.expires_at {
        return Err(JoinDoorRefusal::TokenExpired {
            token_id: proof.token_id.clone(),
            expires_at: document.expires_at,
        });
    }
    if !proof.secret.matches_sha256(&document.secret_sha256) {
        return Err(JoinDoorRefusal::TokenSecretMismatch {
            token_id: proof.token_id.clone(),
        });
    }
    Ok(crate::corrosion::Principal::ApiToken {
        token_id: proof.token_id.clone(),
    })
}

/// Bounded token lifetime carried in public API requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"JoinTokenTtlSeconds\">"))]
#[serde(try_from = "u32", into = "u32")]
pub struct JoinTokenTtlSeconds(u32);

impl JoinTokenTtlSeconds {
    pub const MIN: u32 = 60;
    pub const MAX: u32 = MAX_JOIN_TOKEN_TTL_SECONDS;

    pub fn try_new(seconds: u32) -> Result<Self, JoinTokenTtlError> {
        if !(Self::MIN..=Self::MAX).contains(&seconds) {
            return Err(JoinTokenTtlError { seconds });
        }
        Ok(Self(seconds))
    }

    #[must_use]
    pub const fn default_v1() -> Self {
        Self(DEFAULT_JOIN_TOKEN_TTL_SECONDS)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for JoinTokenTtlSeconds {
    type Error = JoinTokenTtlError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<JoinTokenTtlSeconds> for u32 {
    fn from(value: JoinTokenTtlSeconds) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("join token TTL must be between 60 and 2592000 seconds, got {seconds}")]
pub struct JoinTokenTtlError {
    pub seconds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinStorageChoice {
    Automatic,
    Flag { mode: StorageMode },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JoinStorageFacts {
    pub imported_zfs_pool: bool,
    pub total_memory_bytes: u64,
}

/// Selects the durable machine storage outcome at admission.
#[must_use]
pub fn select_join_storage(
    cluster_default: StorageMode,
    choice: JoinStorageChoice,
    facts: JoinStorageFacts,
) -> MachineStorageSelection {
    match choice {
        JoinStorageChoice::Flag { mode } => MachineStorageSelection {
            mode,
            reason: MachineStorageSelectionReason::Flag,
        },
        JoinStorageChoice::Automatic
            if cluster_default == StorageMode::Zfs
                && facts.imported_zfs_pool
                && facts.total_memory_bytes >= MINIMUM_ZFS_MEMORY_BYTES =>
        {
            MachineStorageSelection {
                mode: StorageMode::Zfs,
                reason: MachineStorageSelectionReason::Default,
            }
        }
        JoinStorageChoice::Automatic
            if cluster_default == StorageMode::Zfs && facts.imported_zfs_pool =>
        {
            MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Ineligible {
                    reason: MachineStorageIneligibleReason::LowRam,
                },
            }
        }
        JoinStorageChoice::Automatic => MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Default,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinArrival {
    Clean,
    Partial {
        persisted_door_fingerprint: JoinDoorCertFingerprint,
    },
    Complete {
        persisted_door_fingerprint: JoinDoorCertFingerprint,
    },
    /// Founding state has no join fingerprint and belongs to a different journey.
    Founding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinArrivalDisposition {
    Join,
    Resume,
    NoOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinRepairCommand {
    #[serde(rename = "ployz machine reset")]
    ResetMachine,
}

impl JoinRepairCommand {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResetMachine => JOIN_RESET_COMMAND,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinArrivalRefusal {
    #[error("host carries a different cluster door fingerprint; run {repair_command:?}")]
    ForeignState {
        requested_door_fingerprint: JoinDoorCertFingerprint,
        found_door_fingerprint: JoinDoorCertFingerprint,
        repair_command: JoinRepairCommand,
    },
    #[error("host was already used to found a cluster; run {repair_command:?}")]
    FoundingState { repair_command: JoinRepairCommand },
}

impl JoinArrivalRefusal {
    #[must_use]
    pub const fn repair_command(&self) -> JoinRepairCommand {
        match self {
            Self::ForeignState { repair_command, .. } | Self::FoundingState { repair_command } => {
                *repair_command
            }
        }
    }
}

pub fn classify_join_arrival(
    requested_door_fingerprint: &JoinDoorCertFingerprint,
    arrival: JoinArrival,
) -> Result<JoinArrivalDisposition, JoinArrivalRefusal> {
    match arrival {
        JoinArrival::Clean => Ok(JoinArrivalDisposition::Join),
        JoinArrival::Partial {
            persisted_door_fingerprint,
        } if persisted_door_fingerprint == *requested_door_fingerprint => {
            Ok(JoinArrivalDisposition::Resume)
        }
        JoinArrival::Complete {
            persisted_door_fingerprint,
        } if persisted_door_fingerprint == *requested_door_fingerprint => {
            Ok(JoinArrivalDisposition::NoOp)
        }
        JoinArrival::Partial {
            persisted_door_fingerprint,
        }
        | JoinArrival::Complete {
            persisted_door_fingerprint,
        } => Err(JoinArrivalRefusal::ForeignState {
            requested_door_fingerprint: requested_door_fingerprint.clone(),
            found_door_fingerprint: persisted_door_fingerprint,
            repair_command: JoinRepairCommand::ResetMachine,
        }),
        JoinArrival::Founding => Err(JoinArrivalRefusal::FoundingState {
            repair_command: JoinRepairCommand::ResetMachine,
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinRequest {
    pub machine_id: MachineRowId,
    pub name: MachineName,
    pub public_key: WireGuardPublicKey,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub endpoint: Option<SocketAddr>,
    pub storage_choice: JoinStorageChoice,
    pub storage_facts: JoinStorageFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PeerJoinRequest {
    pub peer_id: PeerId,
    pub name: String,
    pub public_key: WireGuardPublicKey,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub endpoint: Option<SocketAddr>,
}

/// Why a join candidate cannot become an authority row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinAdmissionValidationError {
    #[error("join admission supports builtin_wireguard, not {found:?}")]
    UnsupportedProvider { found: MeshProvider },
    #[error("machine subnet {subnet:?} is outside cluster prefix")]
    EndpointSubnetOutsideClusterPrefix { subnet: MachineEndpointSubnet },
    #[error("join endpoint port cannot be zero")]
    EndpointPortZero,
    #[error("join admission provenance must name an API token")]
    ProvenanceIsNotApiToken,
}

/// Builds the exact machine row authorized by one validated door request.
pub fn prepare_machine_admission(
    cluster: &ClusterDocument,
    request: MachineJoinRequest,
    subnet_v4: MachineEndpointSubnet,
    provenance: OperatorWriteProvenance,
) -> Result<AcceptedMachineRow, JoinAdmissionValidationError> {
    validate_admission_inputs(cluster, request.endpoint, &provenance)?;
    if !cluster.prefix.contains_subnet(&subnet_v4) {
        return Err(
            JoinAdmissionValidationError::EndpointSubnetOutsideClusterPrefix { subnet: subnet_v4 },
        );
    }
    let addr_v6 = derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
        .bind_address()
        .get();
    let storage = select_join_storage(
        cluster.storage_default,
        request.storage_choice,
        request.storage_facts,
    );
    Ok(AcceptedMachineRow {
        machine_id: request.machine_id,
        document: MachineDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster.cluster_id.clone(),
            provenance,
            name: request.name,
            lifecycle: MachineLifecycle::Active,
            transport: MachineTransport::Wireguard {
                pubkey: request.public_key,
                addr_v6,
                endpoint: request.endpoint,
                subnet_v4,
            },
            storage,
        },
    })
}

/// Builds the exact peer row authorized by one validated door request.
pub fn prepare_peer_admission(
    cluster: &ClusterDocument,
    request: PeerJoinRequest,
    provenance: OperatorWriteProvenance,
) -> Result<AcceptedPeerRow, JoinAdmissionValidationError> {
    validate_admission_inputs(cluster, request.endpoint, &provenance)?;
    let addr_v6 = derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
        .bind_address()
        .get();
    Ok(AcceptedPeerRow {
        peer_id: request.peer_id,
        document: PeerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster.cluster_id.clone(),
            provenance,
            name: request.name,
            transport: PeerTransport::Wireguard {
                pubkey: request.public_key,
                addr_v6,
                endpoint: request.endpoint,
            },
        },
    })
}

fn validate_admission_inputs(
    cluster: &ClusterDocument,
    endpoint: Option<SocketAddr>,
    provenance: &OperatorWriteProvenance,
) -> Result<(), JoinAdmissionValidationError> {
    if cluster.provider != MeshProvider::BuiltinWireguard {
        return Err(JoinAdmissionValidationError::UnsupportedProvider {
            found: cluster.provider,
        });
    }
    if endpoint.is_some_and(|endpoint| endpoint.port() == 0) {
        return Err(JoinAdmissionValidationError::EndpointPortZero);
    }
    if !matches!(provenance.written_by, OperationInitiator::ApiToken { .. }) {
        return Err(JoinAdmissionValidationError::ProvenanceIsNotApiToken);
    }
    Ok(())
}

/// The request union keeps peer admission structurally free of storage and `/24` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinMemberRequest {
    Machine { request: MachineJoinRequest },
    Peer { request: PeerJoinRequest },
}

/// A PEM-encoded shared door private key that never reveals itself through Debug.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"JoinDoorPrivateKeyPem\">"))]
#[serde(try_from = "String", into = "String")]
pub struct JoinDoorPrivateKeyPem(String);

impl JoinDoorPrivateKeyPem {
    pub fn try_new(value: impl Into<String>) -> Result<Self, JoinDoorPrivateKeyPemError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(JoinDoorPrivateKeyPemError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for JoinDoorPrivateKeyPem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JoinDoorPrivateKeyPem([redacted])")
    }
}

impl TryFrom<String> for JoinDoorPrivateKeyPem {
    type Error = JoinDoorPrivateKeyPemError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<JoinDoorPrivateKeyPem> for String {
    fn from(value: JoinDoorPrivateKeyPem) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("join-door private key PEM cannot be empty")]
pub struct JoinDoorPrivateKeyPemError;

/// Mesh-authenticated request to mint one show-once join credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenCreateRequest {
    pub ttl_seconds: JoinTokenTtlSeconds,
}

/// Token metadata and the one value that can never be recovered from its row.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenCreateReply {
    pub token_id: TokenId,
    pub blob: JoinBlob,
    pub created_at: CorrosionTimestamp,
    pub expires_at: CorrosionTimestamp,
}

impl fmt::Debug for TokenCreateReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TokenCreateReply")
            .field("token_id", &self.token_id)
            .field("blob", &"[redacted]")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A row/reply pair prepared without retaining the plaintext secret in the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTokenCreation {
    pub token_id: TokenId,
    pub document: TokenDocument,
    pub reply: TokenCreateReply,
}

impl PreparedTokenCreation {
    pub fn try_new(
        token_id: TokenId,
        secret: JoinTokenSecret,
        door_cert_fingerprint: JoinDoorCertFingerprint,
        endpoints: AdvertisedJoinDoorEndpoints,
        mut document: TokenDocument,
    ) -> Result<Self, JoinBlobError> {
        document.secret_sha256 = secret.sha256().into_sha256_hex();
        let blob = JoinBlob::try_new(
            token_id.clone(),
            secret,
            door_cert_fingerprint,
            endpoints.into(),
        )?;
        let reply = TokenCreateReply {
            token_id: token_id.clone(),
            blob,
            created_at: document.created_at,
            expires_at: document.expires_at,
        };
        Ok(Self {
            token_id,
            document,
            reply,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TokenListScope {
    Live,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenListRequest {
    pub scope: TokenListScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenListItem {
    pub token_id: TokenId,
    pub created_at: CorrosionTimestamp,
    pub expires_at: CorrosionTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenListReply {
    pub tokens: Vec<TokenListItem>,
}

/// Applies the token-list expiry policy to accepted rows.
pub fn token_list_reply(
    request: TokenListRequest,
    now: CorrosionTimestamp,
    rows: impl IntoIterator<Item = (TokenId, TokenDocument)>,
) -> TokenListReply {
    let mut tokens = rows
        .into_iter()
        .filter(|(_, document)| request.scope == TokenListScope::All || document.expires_at > now)
        .map(|(token_id, document)| TokenListItem {
            token_id,
            created_at: document.created_at,
            expires_at: document.expires_at,
        })
        .collect::<Vec<_>>();
    tokens.sort_by(|left, right| left.token_id.cmp(&right.token_id));
    TokenListReply { tokens }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenRevokeRequest {
    pub token_id: TokenId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenRevokeReply {
    pub token_id: TokenId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenRevokeRefusal {
    NotFound { token_id: TokenId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum TokenRevokeResponse {
    Revoked { reply: TokenRevokeReply },
    Refused { refusal: TokenRevokeRefusal },
}

/// Mesh-authenticated request to change only a WireGuard endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineEndpointSetRequest {
    pub machine_name: MachineName,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub endpoint: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineEndpointSetReply {
    pub machine_id: MachineRowId,
    pub machine: MachineDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineEndpointSetRefusal {
    NotFound { machine_name: MachineName },
    EndpointPortZero { machine_name: MachineName },
    ProviderDoesNotUseWireguard { machine_id: MachineRowId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum MachineEndpointSetResponse {
    Updated { reply: MachineEndpointSetReply },
    Refused { refusal: MachineEndpointSetRefusal },
}

/// Applies only the endpoint field, preserving every other roster decision.
pub fn set_machine_endpoint(
    machine_id: MachineRowId,
    request: &MachineEndpointSetRequest,
    mut machine: MachineDocument,
) -> Result<MachineEndpointSetReply, MachineEndpointSetRefusal> {
    if request.endpoint.port() == 0 {
        return Err(MachineEndpointSetRefusal::EndpointPortZero {
            machine_name: request.machine_name.clone(),
        });
    }
    let MachineTransport::Wireguard { endpoint, .. } = &mut machine.transport else {
        return Err(MachineEndpointSetRefusal::ProviderDoesNotUseWireguard { machine_id });
    };
    *endpoint = Some(request.endpoint);
    Ok(MachineEndpointSetReply {
        machine_id,
        machine,
    })
}

/// The secret proof sent inside the already fingerprint-pinned TLS request.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JoinTokenProof {
    pub token_id: TokenId,
    pub secret: JoinTokenSecret,
}

impl fmt::Debug for JoinTokenProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JoinTokenProof")
            .field("token_id", &self.token_id)
            .field("secret", &"[redacted]")
            .finish()
    }
}

impl JoinBlob {
    #[must_use]
    pub fn token_proof(&self) -> JoinTokenProof {
        JoinTokenProof {
            token_id: self.payload.token_id.clone(),
            secret: self.payload.secret.clone(),
        }
    }
}

/// The complete request accepted at the public join-only route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JoinAdmissionRequest {
    pub token: JoinTokenProof,
    pub member: JoinMemberRequest,
}

/// The public-door name for the admission request contract.
pub type JoinDoorRequest = JoinAdmissionRequest;

/// Public cluster door certificate PEM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"JoinDoorCertificatePem\">"))]
#[serde(try_from = "String", into = "String")]
pub struct JoinDoorCertificatePem(String);

impl JoinDoorCertificatePem {
    pub fn try_new(value: impl Into<String>) -> Result<Self, JoinDoorCertificatePemError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(JoinDoorCertificatePemError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for JoinDoorCertificatePem {
    type Error = JoinDoorCertificatePemError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<JoinDoorCertificatePem> for String {
    fn from(value: JoinDoorCertificatePem) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("join-door certificate PEM cannot be empty")]
pub struct JoinDoorCertificatePemError;

/// Cluster-wide door material installed on an accepted machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JoinDoorMaterial {
    pub certificate_pem: JoinDoorCertificatePem,
    pub private_key_pem: JoinDoorPrivateKeyPem,
    pub fingerprint: JoinDoorCertFingerprint,
}

/// A current roster machine the joiner can reach before Corrosion has synced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ReachableSeedMachine {
    pub machine_id: MachineRowId,
    pub transport: MachineTransport,
}

impl ReachableSeedMachine {
    pub fn try_new(
        machine_id: MachineRowId,
        transport: MachineTransport,
    ) -> Result<Self, JoinAcceptanceValidationError> {
        if matches!(
            transport,
            MachineTransport::Wireguard { endpoint: None, .. }
        ) {
            return Err(JoinAcceptanceValidationError::SeedIsNotReachable);
        }
        Ok(Self {
            machine_id,
            transport,
        })
    }
}

/// Corrosion facts that let a new database join before it owns roster state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CorrosionBootstrapFacts {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub seed_gossip_address: SocketAddr,
}

impl CorrosionBootstrapFacts {
    pub fn try_new(seed_gossip_address: SocketAddr) -> Result<Self, JoinAcceptanceValidationError> {
        if seed_gossip_address.port() == 0 {
            return Err(JoinAcceptanceValidationError::InvalidCorrosionSeed);
        }
        Ok(Self {
            seed_gossip_address,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcceptedMachineRow {
    pub machine_id: MachineRowId,
    pub document: MachineDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcceptedPeerRow {
    pub peer_id: PeerId,
    pub document: PeerDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JoinMachineSubstrateWire {
    ployz_version: ExactPloyzVersion,
    corrosion_version: String,
    artifacts: Vec<InstallArtifactSpec>,
}

/// Exact machine substrate selected by the accepting cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(
    try_from = "JoinMachineSubstrateWire",
    into = "JoinMachineSubstrateWire"
)]
pub struct JoinMachineSubstrate {
    ployz_version: ExactPloyzVersion,
    corrosion_version: String,
    artifacts: Vec<InstallArtifactSpec>,
}

impl JoinMachineSubstrate {
    pub const REQUIRED_INSTALL_PATHS: [&'static str; 6] = [
        "/usr/local/bin/ployzd",
        "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
        "/usr/local/bin/ployz-ebpf-ctl",
        "/usr/local/bin/corrosion",
        "/usr/local/lib/ployz/corrosion-schema-v1.sql",
        "/usr/local/bin/railpack",
    ];

    pub fn try_new(
        ployz_version: ExactPloyzVersion,
        corrosion_version: impl Into<String>,
        artifacts: Vec<InstallArtifactSpec>,
    ) -> Result<Self, JoinAcceptanceValidationError> {
        let substrate = Self {
            ployz_version,
            corrosion_version: corrosion_version.into(),
            artifacts,
        };
        substrate.validate()?;
        Ok(substrate)
    }

    #[must_use]
    pub const fn ployz_version(&self) -> &ExactPloyzVersion {
        &self.ployz_version
    }

    #[must_use]
    pub fn corrosion_version(&self) -> &str {
        &self.corrosion_version
    }

    #[must_use]
    pub fn artifacts(&self) -> &[InstallArtifactSpec] {
        &self.artifacts
    }

    fn validate(&self) -> Result<(), JoinAcceptanceValidationError> {
        if self.corrosion_version.trim().is_empty() {
            return Err(JoinAcceptanceValidationError::MissingCorrosionVersion);
        }

        let mut destinations = BTreeSet::new();
        for artifact in &self.artifacts {
            let destination = artifact.install_path.as_str();
            if !Self::REQUIRED_INSTALL_PATHS.contains(&destination) {
                return Err(JoinAcceptanceValidationError::UnknownInstallArtifact {
                    install_path: destination.to_owned(),
                });
            }
            if !destinations.insert(destination) {
                return Err(JoinAcceptanceValidationError::DuplicateInstallArtifact {
                    install_path: destination.to_owned(),
                });
            }
        }
        for required in Self::REQUIRED_INSTALL_PATHS {
            if !destinations.contains(required) {
                return Err(JoinAcceptanceValidationError::MissingInstallArtifact {
                    install_path: required.to_owned(),
                });
            }
        }
        Ok(())
    }
}

impl TryFrom<JoinMachineSubstrateWire> for JoinMachineSubstrate {
    type Error = JoinAcceptanceValidationError;

    fn try_from(wire: JoinMachineSubstrateWire) -> Result<Self, Self::Error> {
        Self::try_new(wire.ployz_version, wire.corrosion_version, wire.artifacts)
    }
}

impl From<JoinMachineSubstrate> for JoinMachineSubstrateWire {
    fn from(substrate: JoinMachineSubstrate) -> Self {
        Self {
            ployz_version: substrate.ployz_version,
            corrosion_version: substrate.corrosion_version,
            artifacts: substrate.artifacts,
        }
    }
}

/// Variant-specific accepted machine material; peers cannot carry these fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineJoinAccepted {
    pub cluster: ClusterDocument,
    pub machine: AcceptedMachineRow,
    pub seed: ReachableSeedMachine,
    pub door: JoinDoorMaterial,
    pub corrosion: CorrosionBootstrapFacts,
    pub substrate: JoinMachineSubstrate,
}

impl MachineJoinAccepted {
    pub fn try_validate(
        self,
        request: &MachineJoinRequest,
        expected_fingerprint: &JoinDoorCertFingerprint,
    ) -> Result<ValidatedMachineJoinAccepted, JoinAcceptanceValidationError> {
        self.substrate.validate()?;
        if self.cluster.provider != MeshProvider::BuiltinWireguard {
            return Err(JoinAcceptanceValidationError::ProviderMismatch);
        }
        if self.machine.machine_id != request.machine_id {
            return Err(JoinAcceptanceValidationError::AcceptedIdentityMismatch);
        }
        if self.machine.document.cluster_id != self.cluster.cluster_id {
            return Err(JoinAcceptanceValidationError::ClusterMismatch);
        }
        if self.machine.document.name != request.name {
            return Err(JoinAcceptanceValidationError::AcceptedNameMismatch);
        }
        let MachineTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint,
            subnet_v4,
        } = &self.machine.document.transport
        else {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        };
        if pubkey != &request.public_key || endpoint != &request.endpoint {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        }
        if derive_builtin_wireguard_member(&self.cluster.cluster_id, pubkey)
            .bind_address()
            .get()
            != *addr_v6
        {
            return Err(JoinAcceptanceValidationError::AcceptedAddressMismatch);
        }
        if !self.cluster.prefix.contains_subnet(subnet_v4) {
            return Err(JoinAcceptanceValidationError::EndpointSubnetOutsideClusterPrefix);
        }
        if &self.door.fingerprint != expected_fingerprint {
            return Err(JoinAcceptanceValidationError::DoorFingerprintMismatch);
        }
        ReachableSeedMachine::try_new(self.seed.machine_id.clone(), self.seed.transport.clone())?;
        if self.seed.machine_id == request.machine_id {
            return Err(JoinAcceptanceValidationError::SeedIsJoiningMachine);
        }
        let MachineTransport::Wireguard {
            pubkey: seed_public_key,
            addr_v6: seed_addr_v6,
            endpoint: Some(_),
            subnet_v4: seed_subnet_v4,
        } = &self.seed.transport
        else {
            return Err(JoinAcceptanceValidationError::SeedProviderMismatch);
        };
        if derive_builtin_wireguard_member(&self.cluster.cluster_id, seed_public_key)
            .bind_address()
            .get()
            != *seed_addr_v6
        {
            return Err(JoinAcceptanceValidationError::SeedAddressMismatch);
        }
        if !self.cluster.prefix.contains_subnet(seed_subnet_v4) {
            return Err(JoinAcceptanceValidationError::SeedSubnetOutsideClusterPrefix);
        }
        CorrosionBootstrapFacts::try_new(self.corrosion.seed_gossip_address)?;
        if self.corrosion.seed_gossip_address.ip() != IpAddr::V6(*seed_addr_v6) {
            return Err(JoinAcceptanceValidationError::CorrosionSeedDoesNotMatchMachine);
        }
        Ok(ValidatedMachineJoinAccepted(self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMachineJoinAccepted(MachineJoinAccepted);

impl ValidatedMachineJoinAccepted {
    #[must_use]
    pub const fn accepted(&self) -> &MachineJoinAccepted {
        &self.0
    }

    #[must_use]
    pub fn into_accepted(self) -> MachineJoinAccepted {
        self.0
    }
}

/// Variant-specific accepted peer material; no storage or `/24` can appear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PeerJoinAccepted {
    pub cluster: ClusterDocument,
    pub peer: AcceptedPeerRow,
    pub seed: ReachableSeedMachine,
    pub corrosion: CorrosionBootstrapFacts,
}

impl PeerJoinAccepted {
    pub fn try_validate(
        self,
        request: &PeerJoinRequest,
    ) -> Result<ValidatedPeerJoinAccepted, JoinAcceptanceValidationError> {
        if self.cluster.provider != MeshProvider::BuiltinWireguard {
            return Err(JoinAcceptanceValidationError::ProviderMismatch);
        }
        if self.peer.peer_id != request.peer_id
            || self.peer.document.cluster_id != self.cluster.cluster_id
        {
            return Err(JoinAcceptanceValidationError::AcceptedIdentityMismatch);
        }
        if self.peer.document.name != request.name {
            return Err(JoinAcceptanceValidationError::AcceptedNameMismatch);
        }
        let crate::corrosion::PeerTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint,
        } = &self.peer.document.transport
        else {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        };
        if pubkey != &request.public_key || endpoint != &request.endpoint {
            return Err(JoinAcceptanceValidationError::AcceptedTransportMismatch);
        }
        if derive_builtin_wireguard_member(&self.cluster.cluster_id, pubkey)
            .bind_address()
            .get()
            != *addr_v6
        {
            return Err(JoinAcceptanceValidationError::AcceptedAddressMismatch);
        }
        validate_seed(&self.cluster, &self.seed, None, &self.corrosion)?;
        Ok(ValidatedPeerJoinAccepted(self))
    }
}

fn validate_seed(
    cluster: &ClusterDocument,
    seed: &ReachableSeedMachine,
    joining_machine_id: Option<&MachineRowId>,
    corrosion: &CorrosionBootstrapFacts,
) -> Result<(), JoinAcceptanceValidationError> {
    ReachableSeedMachine::try_new(seed.machine_id.clone(), seed.transport.clone())?;
    if joining_machine_id.is_some_and(|joining| joining == &seed.machine_id) {
        return Err(JoinAcceptanceValidationError::SeedIsJoiningMachine);
    }
    let MachineTransport::Wireguard {
        pubkey,
        addr_v6,
        endpoint: Some(_),
        subnet_v4,
    } = &seed.transport
    else {
        return Err(JoinAcceptanceValidationError::SeedProviderMismatch);
    };
    if derive_builtin_wireguard_member(&cluster.cluster_id, pubkey)
        .bind_address()
        .get()
        != *addr_v6
    {
        return Err(JoinAcceptanceValidationError::SeedAddressMismatch);
    }
    if !cluster.prefix.contains_subnet(subnet_v4) {
        return Err(JoinAcceptanceValidationError::SeedSubnetOutsideClusterPrefix);
    }
    CorrosionBootstrapFacts::try_new(corrosion.seed_gossip_address)?;
    if corrosion.seed_gossip_address.ip() != IpAddr::V6(*addr_v6) {
        return Err(JoinAcceptanceValidationError::CorrosionSeedDoesNotMatchMachine);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedPeerJoinAccepted(PeerJoinAccepted);

impl ValidatedPeerJoinAccepted {
    #[must_use]
    pub const fn accepted(&self) -> &PeerJoinAccepted {
        &self.0
    }

    #[must_use]
    pub fn into_accepted(self) -> PeerJoinAccepted {
        self.0
    }
}

/// Whether a matching accepted row must be created or may be reused on retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinAdmissionWrite {
    Create,
    Reuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an accepted row with this id differs from the requested join identity")]
pub struct JoinAdmissionIdempotencyError;

pub fn classify_machine_admission(
    cluster: &ClusterDocument,
    request: &MachineJoinRequest,
    existing: Option<&AcceptedMachineRow>,
) -> Result<JoinAdmissionWrite, JoinAdmissionIdempotencyError> {
    let Some(existing) = existing else {
        return Ok(JoinAdmissionWrite::Create);
    };
    let MachineTransport::Wireguard {
        pubkey,
        addr_v6,
        endpoint,
        ..
    } = &existing.document.transport
    else {
        return Err(JoinAdmissionIdempotencyError);
    };
    let expected_address =
        derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
            .bind_address()
            .get();
    let expected_storage = select_join_storage(
        cluster.storage_default,
        request.storage_choice,
        request.storage_facts,
    );
    if existing.machine_id == request.machine_id
        && existing.document.cluster_id == cluster.cluster_id
        && existing.document.name == request.name
        && pubkey == &request.public_key
        && *addr_v6 == expected_address
        && endpoint == &request.endpoint
        && existing.document.storage == expected_storage
    {
        Ok(JoinAdmissionWrite::Reuse)
    } else {
        Err(JoinAdmissionIdempotencyError)
    }
}

pub fn classify_peer_admission(
    cluster: &ClusterDocument,
    request: &PeerJoinRequest,
    existing: Option<&AcceptedPeerRow>,
) -> Result<JoinAdmissionWrite, JoinAdmissionIdempotencyError> {
    let Some(existing) = existing else {
        return Ok(JoinAdmissionWrite::Create);
    };
    let crate::corrosion::PeerTransport::Wireguard {
        pubkey,
        addr_v6,
        endpoint,
    } = &existing.document.transport
    else {
        return Err(JoinAdmissionIdempotencyError);
    };
    let expected_address =
        derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
            .bind_address()
            .get();
    if existing.peer_id == request.peer_id
        && existing.document.cluster_id == cluster.cluster_id
        && existing.document.name == request.name
        && pubkey == &request.public_key
        && *addr_v6 == expected_address
        && endpoint == &request.endpoint
    {
        Ok(JoinAdmissionWrite::Reuse)
    } else {
        Err(JoinAdmissionIdempotencyError)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinAdmissionAccepted {
    Machine { accepted: MachineJoinAccepted },
    Peer { accepted: PeerJoinAccepted },
}

/// Every refusal returned by the public join door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinDoorRefusal {
    TokenNotFound {
        token_id: TokenId,
    },
    TokenExpired {
        token_id: TokenId,
        expires_at: CorrosionTimestamp,
    },
    TokenSecretMismatch {
        token_id: TokenId,
    },
    InvalidAdmission {
        reason: JoinAdmissionValidationError,
    },
    NameConflict {
        name: String,
    },
    IdentityConflict,
    NoReachableSeed,
    EndpointSubnetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum JoinAdmissionReply {
    Accepted {
        admission: Box<JoinAdmissionAccepted>,
    },
    Refused {
        refusal: JoinDoorRefusal,
    },
}

/// The public-door name for the admission reply contract.
pub type JoinDoorReply = JoinAdmissionReply;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinAcceptanceValidationError {
    #[error("accepted join response belongs to a different cluster")]
    ClusterMismatch,
    #[error("accepted join identity disagrees with the request")]
    AcceptedIdentityMismatch,
    #[error("accepted join name disagrees with the request")]
    AcceptedNameMismatch,
    #[error("accepted join transport disagrees with the request")]
    AcceptedTransportMismatch,
    #[error("accepted join response carries a different door fingerprint")]
    DoorFingerprintMismatch,
    #[error("accepted join response has no reachable seed machine")]
    SeedIsNotReachable,
    #[error("accepted join response has an invalid Corrosion gossip seed")]
    InvalidCorrosionSeed,
    #[error("accepted join response uses a different mesh provider")]
    ProviderMismatch,
    #[error("accepted machine address does not derive from its cluster and key")]
    AcceptedAddressMismatch,
    #[error("accepted machine subnet is outside the cluster prefix")]
    EndpointSubnetOutsideClusterPrefix,
    #[error("the joining machine cannot bootstrap Corrosion from itself")]
    SeedIsJoiningMachine,
    #[error("seed machine transport does not match the cluster provider")]
    SeedProviderMismatch,
    #[error("seed machine address does not derive from its cluster and key")]
    SeedAddressMismatch,
    #[error("seed machine subnet is outside the cluster prefix")]
    SeedSubnetOutsideClusterPrefix,
    #[error("Corrosion bootstrap address does not name the seed machine")]
    CorrosionSeedDoesNotMatchMachine,
    #[error("accepted machine substrate has no exact Corrosion version")]
    MissingCorrosionVersion,
    #[error("accepted machine substrate is missing install artifact {install_path:?}")]
    MissingInstallArtifact { install_path: String },
    #[error("accepted machine substrate repeats install artifact {install_path:?}")]
    DuplicateInstallArtifact { install_path: String },
    #[error("accepted machine substrate names unknown install artifact {install_path:?}")]
    UnknownInstallArtifact { install_path: String },
}
