//! Join-token, public-door credential, and endpoint mutation contracts.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::corrosion::{
    CorrosionTimestamp, MachineDocument, MachineTransport, Sha256Hex, TokenDocument,
};
use crate::ids::{MachineName, TokenName};

use super::admission::JoinDoorRefusal;

/// The fixed TCP port served by every public join door.
pub const JOIN_DOOR_PORT: u16 = 2_021;
/// Default lifetime for a newly minted join token.
pub const DEFAULT_JOIN_TOKEN_TTL_SECONDS: u32 = 24 * 60 * 60;
/// Longest join-token lifetime accepted by the public contract.
pub const MAX_JOIN_TOKEN_TTL_SECONDS: u32 = 30 * 24 * 60 * 60;
/// The member ceiling also bounds the endpoint set embedded in one join blob.
pub const MAX_JOIN_DOOR_ENDPOINTS: usize = 256;
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"JoinTokenSecretHash\">"))]
#[serde(transparent)]
pub struct JoinTokenSecretHash(Sha256Hex);

impl JoinTokenSecretHash {
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn into_sha256_hex(self) -> Sha256Hex {
        self.0
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
    token_id: TokenName,
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
        token_id: TokenName,
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
    pub fn token_id(&self) -> &TokenName {
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

impl FromStr for JoinBlob {
    type Err = JoinBlobError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_parse(value)
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
    NameConflict { name: TokenName },
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
    token_row: Option<(&TokenName, &TokenDocument)>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenCreateRequest {
    pub name: TokenName,
    pub ttl_seconds: JoinTokenTtlSeconds,
}

/// Token metadata and the one value that can never be recovered from its row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenCreateReply {
    pub token_id: TokenName,
    pub blob: JoinBlob,
    pub created_at: CorrosionTimestamp,
    pub expires_at: CorrosionTimestamp,
}

/// A row/reply pair prepared without retaining the plaintext secret in the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTokenCreation {
    pub token_id: TokenName,
    pub document: TokenDocument,
    pub reply: TokenCreateReply,
}

impl PreparedTokenCreation {
    pub fn try_new(
        token_id: TokenName,
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
    pub token_id: TokenName,
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
    rows: impl IntoIterator<Item = (TokenName, TokenDocument)>,
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
    pub token_id: TokenName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TokenRevokeReply {
    pub token_id: TokenName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TokenRevokeRefusal {
    NotFound { token_id: TokenName },
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
    pub machine_id: MachineName,
    pub machine: MachineDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineEndpointSetRefusal {
    NotFound { machine_name: MachineName },
    EndpointPortZero { machine_name: MachineName },
    ProviderDoesNotUseWireguard { machine_id: MachineName },
}

/// Applies only the endpoint field, preserving every other roster decision.
pub fn set_machine_endpoint(
    machine_id: MachineName,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JoinTokenProof {
    pub token_id: TokenName,
    pub secret: JoinTokenSecret,
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
