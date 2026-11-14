//! The keys a Machine trusts: its Devices, which manage it and sign statements, and its
//! passkeys, which may only approve workflows, automatic deploy rules, Volume workflows and
//! Release Channels.
//!
//! A passkey is not a Device. It never holds a Management Client slot, so it never opens a
//! connection, and it never signs a statement's envelope. A Machine records one only when a
//! Full caller approves it, and checks its WebAuthn assertions itself against the public key
//! it recorded.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use url::Url;

use super::{AuthorityRefusal, Purpose, SigningPublicKey};
use crate::{CheckCode, DeviceCode, DeviceKey, ValueError};

/// A passkey's public key: a P-256 point for ES256, or an Ed25519 key for EdDSA.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "PublicKeyWire", into = "PublicKeyWire")]
#[ts(as = "PublicKeyWire")]
pub enum PasskeyPublicKey {
    /// The SEC1 uncompressed point.
    Es256([u8; 65]),
    EdDsa([u8; 32]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
pub enum PasskeyAlgorithm {
    Es256,
    Eddsa,
}

/// The wire form: the algorithm and the key's bytes as unpadded base64url.
#[derive(Serialize, Deserialize, TS)]
#[ts(rename = "PasskeyPublicKey")]
struct PublicKeyWire {
    algorithm: PasskeyAlgorithm,
    key: String,
}

impl PasskeyPublicKey {
    /// # Errors
    /// Returns a [`ValueError`] unless `bytes` is a valid key for `algorithm`.
    pub fn parse(algorithm: PasskeyAlgorithm, bytes: &[u8]) -> Result<Self, ValueError> {
        let invalid =
            || ValueError::new("passkey public key", algorithm.label(), algorithm.shape());
        match algorithm {
            PasskeyAlgorithm::Es256 => {
                let point: [u8; 65] = bytes.try_into().map_err(|_| invalid())?;
                p256::ecdsa::VerifyingKey::from_sec1_bytes(&point).map_err(|_| invalid())?;
                Ok(Self::Es256(point))
            }
            PasskeyAlgorithm::Eddsa => {
                let key: [u8; 32] = bytes.try_into().map_err(|_| invalid())?;
                ed25519_dalek::VerifyingKey::from_bytes(&key).map_err(|_| invalid())?;
                Ok(Self::EdDsa(key))
            }
        }
    }

    #[must_use]
    pub const fn algorithm(&self) -> PasskeyAlgorithm {
        match self {
            Self::Es256(_) => PasskeyAlgorithm::Es256,
            Self::EdDsa(_) => PasskeyAlgorithm::Eddsa,
        }
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Es256(point) => point,
            Self::EdDsa(key) => key,
        }
    }

    /// The code a person reads out for this key, by the same rule as a Device's.
    #[must_use]
    pub fn code(&self) -> DeviceCode {
        DeviceCode::hashing(self.as_bytes())
    }

    /// The code a person compares for this key, by the same rule as a Device's.
    #[must_use]
    pub fn check_code(&self) -> CheckCode {
        CheckCode::hashing(self.as_bytes())
    }

    /// Whether `signature` is this key's signature over `message`: DER-encoded ECDSA over
    /// SHA-256 for ES256, strict Ed25519 for EdDSA. An ES256 signature verifies in either
    /// S form: WebAuthn lets an authenticator return a high S, and nothing keys on a
    /// signature's bytes, since the challenge binds each answer.
    #[must_use]
    pub fn verifies(&self, message: &[u8], signature: &[u8]) -> bool {
        use p256::ecdsa::signature::Verifier as _;
        match self {
            Self::Es256(point) => {
                let (Ok(key), Ok(signature)) = (
                    p256::ecdsa::VerifyingKey::from_sec1_bytes(point),
                    p256::ecdsa::Signature::from_der(signature),
                ) else {
                    return false;
                };
                key.verify(message, &signature).is_ok()
            }
            Self::EdDsa(key) => {
                let (Ok(key), Ok(signature)) = (
                    ed25519_dalek::VerifyingKey::from_bytes(key),
                    ed25519_dalek::Signature::from_slice(signature),
                ) else {
                    return false;
                };
                key.verify_strict(message, &signature).is_ok()
            }
        }
    }
}

impl PasskeyAlgorithm {
    const fn label(self) -> &'static str {
        match self {
            Self::Es256 => "es256",
            Self::Eddsa => "eddsa",
        }
    }

    const fn shape(self) -> &'static str {
        match self {
            Self::Es256 => "a P-256 point, 65 bytes uncompressed",
            Self::Eddsa => "an Ed25519 key, 32 bytes",
        }
    }
}

impl fmt::Debug for PasskeyPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PasskeyPublicKey({})", self.code())
    }
}

impl TryFrom<PublicKeyWire> for PasskeyPublicKey {
    type Error = ValueError;

    fn try_from(wire: PublicKeyWire) -> Result<Self, Self::Error> {
        let bytes = URL_SAFE_NO_PAD
            .decode(&wire.key)
            .map_err(|_| ValueError::new("passkey public key", &wire.key, "unpadded base64url"))?;
        Self::parse(wire.algorithm, &bytes)
    }
}

impl From<PasskeyPublicKey> for PublicKeyWire {
    fn from(key: PasskeyPublicKey) -> Self {
        Self {
            algorithm: key.algorithm(),
            key: URL_SAFE_NO_PAD.encode(key.as_bytes()),
        }
    }
}

/// Bytes written as unpadded base64url, as WebAuthn values travel.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct Base64Url(Vec<u8>);

impl Base64Url {
    #[must_use]
    pub const fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<String> for Base64Url {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        URL_SAFE_NO_PAD
            .decode(&value)
            .map(Self)
            .map_err(|_| ValueError::new("base64url value", &value, "unpadded base64url"))
    }
}

impl From<Base64Url> for String {
    fn from(value: Base64Url) -> Self {
        URL_SAFE_NO_PAD.encode(value.0)
    }
}

/// The WebAuthn credential ID a passkey answers with, at most 1023 bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "Base64Url", into = "Base64Url")]
#[ts(type = "string")]
pub struct CredentialId(Vec<u8>);

impl TryFrom<Base64Url> for CredentialId {
    type Error = ValueError;

    fn try_from(value: Base64Url) -> Result<Self, Self::Error> {
        if value.0.is_empty() || value.0.len() > 1023 {
            return Err(ValueError::new(
                "credential ID",
                String::from(value),
                "1 to 1023 bytes",
            ));
        }
        Ok(Self(value.0))
    }
}

impl From<CredentialId> for Base64Url {
    fn from(id: CredentialId) -> Self {
        Self(id.0)
    }
}

/// The Cloud account a passkey belongs to, shown beside it. Cloud names it, so it is only
/// a label: printable, at most 254 characters, never an identity a Machine checks. Control
/// and Unicode format characters, such as the bidi override U+202E, are refused so Cloud
/// can't make a terminal show another account.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct PasskeyAccount(String);

impl PasskeyAccount {
    /// # Errors
    /// Returns a [`ValueError`] for an empty, overlong or unprintable account.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        if value.is_empty()
            || value.chars().count() > 254
            || value
                .chars()
                .any(|character| character.is_control() || is_format(character))
        {
            return Err(ValueError::new(
                "passkey account",
                value,
                "1 to 254 printable characters",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Unicode's format characters (general category Cf): invisible, and some reorder text.
const fn is_format(character: char) -> bool {
    matches!(
        character,
        '\u{AD}'
            | '\u{600}'..='\u{605}'
            | '\u{61C}'
            | '\u{6DD}'
            | '\u{70F}'
            | '\u{890}'..='\u{891}'
            | '\u{8E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

impl fmt::Display for PasskeyAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for PasskeyAccount {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<PasskeyAccount> for String {
    fn from(account: PasskeyAccount) -> Self {
        account.0
    }
}

/// Where a passkey answers: the WebAuthn relying party ID it was made for, and the page
/// origin a person approves on. The CLI takes both from its own Cloud URL.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RelyingParty {
    pub id: String,
    #[ts(type = "string")]
    pub origin: Url,
}

impl RelyingParty {
    /// The relying party a page at `cloud` registers passkeys for.
    ///
    /// # Errors
    /// Returns a [`ValueError`] when `cloud` names no host.
    pub fn of(cloud: &Url) -> Result<Self, ValueError> {
        let id = cloud
            .host_str()
            .ok_or_else(|| ValueError::new("Cloud URL", cloud.as_str(), "a URL with a host"))?;
        let origin = Url::parse(&cloud.origin().ascii_serialization())
            .map_err(|_| ValueError::new("Cloud URL", cloud.as_str(), "an http or https URL"))?;
        Ok(Self {
            id: id.to_owned(),
            origin,
        })
    }
}

/// A passkey as a Full caller approves it on a Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Passkey {
    pub credential_id: CredentialId,
    pub public_key: PasskeyPublicKey,
    pub account: PasskeyAccount,
    pub relying_party: RelyingParty,
}

impl Passkey {
    #[must_use]
    pub fn code(&self) -> DeviceCode {
        self.public_key.code()
    }

    /// Check `assertion` is this passkey's user-verified answer to `challenge`, made on
    /// its relying party's origin.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first part that does not hold.
    pub fn verify_assertion(
        &self,
        assertion: &PasskeyAssertion,
        challenge: &[u8],
    ) -> Result<(), AuthorityRefusal> {
        let mismatch = |field| Err(AuthorityRefusal::Mismatch { field });
        if assertion.credential_id != self.credential_id {
            return mismatch("a passkey");
        }
        let Ok(client) =
            serde_json::from_slice::<ClientData>(assertion.client_data_json.as_bytes())
        else {
            return mismatch("an assertion");
        };
        if client.kind != "webauthn.get" {
            return mismatch("an assertion type");
        }
        if client.challenge != URL_SAFE_NO_PAD.encode(challenge) {
            return mismatch("a challenge");
        }
        if client.cross_origin
            || client.origin != self.relying_party.origin.origin().ascii_serialization()
        {
            return mismatch("an origin");
        }
        let data = assertion.authenticator_data.as_bytes();
        let (Some(rp_id_hash), Some(flags)) = (data.get(..32), data.get(32)) else {
            return mismatch("an assertion");
        };
        if rp_id_hash != Sha256::digest(self.relying_party.id.as_bytes()).as_slice() {
            return mismatch("a relying party");
        }
        if flags & USER_PRESENT == 0 || flags & USER_VERIFIED == 0 {
            return mismatch("user verification");
        }
        let mut signed = data.to_vec();
        signed.extend_from_slice(&Sha256::digest(assertion.client_data_json.as_bytes()));
        if !self
            .public_key
            .verifies(&signed, assertion.signature.as_bytes())
        {
            return mismatch("a passkey signature");
        }
        Ok(())
    }
}

const USER_PRESENT: u8 = 0x01;
const USER_VERIFIED: u8 = 0x04;

/// The fields of `clientDataJSON` a Machine checks. Others, such as `topOrigin`, are ignored.
#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default)]
    cross_origin: bool,
}

/// A passkey's answer to a WebAuthn `navigator.credentials.get` call, every part unpadded
/// base64url as the browser returns it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PasskeyAssertion {
    pub credential_id: CredentialId,
    pub authenticator_data: Base64Url,
    pub client_data_json: Base64Url,
    pub signature: Base64Url,
}

/// Who approved a passkey on this Machine, as the Machine saw its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum PasskeyApprover {
    /// A Device's own connection to this Machine.
    Device { code: DeviceCode },
    /// A caller on this Server's local socket: its terminal, or SSH to it.
    Server,
    /// A Full call that reached this Machine over the mesh, routed from another Machine
    /// that checked its caller. This Machine can't name that caller.
    Routed,
    /// An approver a newer daemon recorded, which this one can't name.
    #[serde(other)]
    Unknown,
}

impl fmt::Display for PasskeyApprover {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device { code } => write!(formatter, "device {code}"),
            Self::Server => formatter.write_str("server"),
            Self::Routed => formatter.write_str("routed"),
            Self::Unknown => formatter.write_str("unknown"),
        }
    }
}

/// A passkey this Machine trusts to approve workflows, and nothing else.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct TrustedPasskey {
    pub passkey: Passkey,
    pub approved_by: PasskeyApprover,
}

/// Read a list of trusted passkeys, skipping each entry this build can't read, such as
/// one a newer daemon wrote with an algorithm it doesn't know. Skipping fails closed: that
/// passkey approves nothing here.
///
/// # Errors
/// Returns the deserializer's error when the value is not a list.
pub fn readable_passkeys<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<TrustedPasskey>, D::Error> {
    Ok(Vec::<serde_json::Value>::deserialize(deserializer)?
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect())
}

/// Every key one Machine trusts, read from its own record and nothing Cloud wrote.
#[derive(Clone, Debug, Default)]
pub struct TrustedKeys {
    pub devices: Vec<DeviceKey>,
    pub passkeys: Vec<TrustedPasskey>,
}

impl TrustedKeys {
    /// The keys whose signatures a statement's envelope may carry. Only Devices sign; a
    /// passkey whose key happens to be Ed25519 is not among them.
    pub fn signing_keys(&self) -> impl Iterator<Item = SigningPublicKey> + '_ {
        self.devices.iter().copied().map(SigningPublicKey::from)
    }

    /// The trusted passkey that answered `assertion` to `challenge`, when a statement of
    /// `purpose` may rest on a passkey at all. Workflows may, through a session grant, and
    /// rules and Volume workflows, through a prompt of their own.
    ///
    /// A credential ID is the authenticator's claim, not a key, so every trusted passkey
    /// under it is tried and only a verifying signature picks one.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::ApproveOnly`] for a join or enforcement statement,
    /// [`AuthorityRefusal::UnknownPasskey`] when no trusted passkey has the assertion's
    /// credential, and the first candidate's refusal when none verifies.
    pub fn passkey_approval(
        &self,
        purpose: Purpose,
        assertion: &PasskeyAssertion,
        challenge: &[u8],
    ) -> Result<&TrustedPasskey, AuthorityRefusal> {
        let mut candidates = self
            .passkeys
            .iter()
            .filter(|trusted| trusted.passkey.credential_id == assertion.credential_id);
        let first = candidates.next().ok_or(AuthorityRefusal::UnknownPasskey)?;
        if !matches!(
            purpose,
            Purpose::Workflow | Purpose::Rule | Purpose::Volume | Purpose::Enforce
        ) {
            return Err(AuthorityRefusal::ApproveOnly {
                code: first.passkey.code(),
                purpose,
            });
        }
        let refusal = match first.passkey.verify_assertion(assertion, challenge) {
            Ok(()) => return Ok(first),
            Err(refusal) => refusal,
        };
        candidates
            .find(|trusted| {
                trusted
                    .passkey
                    .verify_assertion(assertion, challenge)
                    .is_ok()
            })
            .ok_or(refusal)
    }
}

#[cfg(test)]
#[path = "trusted_key_tests.rs"]
pub(crate) mod tests;
