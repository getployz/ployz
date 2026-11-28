//! The keys a Machine trusts: its Devices, which manage it and sign statements, its
//! passkeys, which approve workflows, automatic deploy rules and bundles, whose steps
//! include joins and key changes, and its CI and agent grants, which sign Service versions
//! inside their scope.
//!
//! A passkey is not a Device. It never holds a Management Client slot, so it never opens a
//! connection, and it never signs a statement's envelope. A Machine records one only from a
//! signed key change or join approval, and checks its WebAuthn assertions itself against
//! the public key it recorded.

use crate::authority::MismatchField;
use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use url::Url;

use super::{
    Approver, AuthorityRefusal, SigningPublicKey,
    scope::{Ask, Denied, Grant, KeyKind, KeyScope, Reach, TrustedGrant},
};
use crate::{CheckCode, ClientKey, DeviceCode, DeviceKey, QualifiedService, ValueError};

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
        if value.is_empty() || value.chars().count() > 254 || value.chars().any(unprintable) {
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

/// A control character, or one of Unicode's format characters, which a label refuses.
pub(super) fn unprintable(character: char) -> bool {
    character.is_control() || is_format(character)
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

/// A passkey as a signed key change or join approval names it.
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
            return mismatch(MismatchField::Passkey);
        }
        let Ok(client) =
            serde_json::from_slice::<ClientData>(assertion.client_data_json.as_bytes())
        else {
            return mismatch(MismatchField::Assertion);
        };
        if client.kind != "webauthn.get" {
            return mismatch(MismatchField::AssertionType);
        }
        if client.challenge != URL_SAFE_NO_PAD.encode(challenge) {
            return mismatch(MismatchField::Challenge);
        }
        if client.cross_origin
            || client.origin != self.relying_party.origin.origin().ascii_serialization()
        {
            return mismatch(MismatchField::Origin);
        }
        let data = assertion.authenticator_data.as_bytes();
        let (Some(rp_id_hash), Some(flags)) = (data.get(..32), data.get(32)) else {
            return mismatch(MismatchField::Assertion);
        };
        if rp_id_hash != Sha256::digest(self.relying_party.id.as_bytes()).as_slice() {
            return mismatch(MismatchField::RelyingParty);
        }
        if flags & USER_PRESENT == 0 || flags & USER_VERIFIED == 0 {
            return mismatch(MismatchField::UserVerification);
        }
        let mut signed = data.to_vec();
        signed.extend_from_slice(&Sha256::digest(assertion.client_data_json.as_bytes()));
        if !self
            .public_key
            .verifies(&signed, assertion.signature.as_bytes())
        {
            return mismatch(MismatchField::PasskeySignature);
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

/// A passkey this Machine trusts to approve workflows, rules and bundles, and nothing else.
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

/// One key this Machine trusts, from the source in its record that holds it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrustedKey {
    /// A Device, which reads as a full CLI key over the whole Cluster.
    Device(DeviceKey),
    /// A passkey, which reads as a person's key over the whole Cluster.
    Passkey(TrustedPasskey),
    /// A CI or agent key, which holds the scope its grant names.
    Grant(TrustedGrant),
}

impl TrustedKey {
    #[must_use]
    pub fn kind(&self) -> KeyKind {
        match self {
            Self::Device(_) => KeyKind::FullCli,
            Self::Passkey(_) => KeyKind::Person,
            Self::Grant(held) => held.grant.kind.into(),
        }
    }

    /// The scope a grant names. `None` for a Device or passkey, which hold their kind's
    /// ceiling over the whole Cluster.
    #[must_use]
    pub fn scope(&self) -> Option<&KeyScope> {
        self.grant().map(|grant| &grant.scope)
    }

    /// The grant a CI or agent key holds. `None` for a Device or passkey.
    #[must_use]
    pub fn grant(&self) -> Option<&Grant> {
        match self {
            Self::Grant(held) => Some(&held.grant),
            Self::Device(_) | Self::Passkey(_) => None,
        }
    }

    #[must_use]
    pub fn code(&self) -> DeviceCode {
        match self {
            Self::Device(key) => key.code(),
            Self::Passkey(held) => held.passkey.code(),
            Self::Grant(held) => held.grant.key.code(),
        }
    }

    /// The Ed25519 key that signs a statement's envelope. A passkey signs none.
    #[must_use]
    pub fn signing_key(&self) -> Option<SigningPublicKey> {
        match self {
            Self::Device(key) => Some((*key).into()),
            Self::Grant(held) => Some(held.grant.key),
            Self::Passkey(_) => None,
        }
    }

    fn approves_as(&self, by: &Approver) -> bool {
        match (self, by) {
            (Self::Device(_) | Self::Grant(_), Approver::Signing(key)) => {
                self.signing_key() == Some(*key)
            }
            (Self::Passkey(held), Approver::Passkey(code)) => held.passkey.code() == *code,
            _ => false,
        }
    }
}

/// Every key one Machine trusts, read from its own record and nothing Cloud wrote: its
/// Devices, its passkeys and its grants, as one list.
#[derive(Clone, Debug, Default)]
pub struct TrustedKeys {
    keys: Vec<TrustedKey>,
}

impl TrustedKeys {
    /// The keys of three sources as one list. A Device whose key a grant also holds is
    /// left out, so that key never reads wider than its grant.
    #[must_use]
    pub fn new(
        devices: impl IntoIterator<Item = DeviceKey>,
        passkeys: impl IntoIterator<Item = TrustedPasskey>,
        grants: impl IntoIterator<Item = TrustedGrant>,
    ) -> Self {
        let grants: Vec<TrustedGrant> = grants.into_iter().collect();
        let devices: Vec<TrustedKey> = devices
            .into_iter()
            .filter(|device| {
                !grants
                    .iter()
                    .any(|held| held.grant.key == SigningPublicKey::from(*device))
            })
            .map(TrustedKey::Device)
            .collect();
        Self {
            keys: devices
                .into_iter()
                .chain(passkeys.into_iter().map(TrustedKey::Passkey))
                .chain(grants.into_iter().map(TrustedKey::Grant))
                .collect(),
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &TrustedKey> {
        self.keys.iter()
    }

    pub fn devices(&self) -> impl Iterator<Item = DeviceKey> + '_ {
        self.keys.iter().filter_map(|key| match key {
            TrustedKey::Device(device) => Some(*device),
            TrustedKey::Passkey(_) | TrustedKey::Grant(_) => None,
        })
    }

    pub fn passkeys(&self) -> impl Iterator<Item = &TrustedPasskey> {
        self.keys.iter().filter_map(|key| match key {
            TrustedKey::Passkey(held) => Some(held),
            TrustedKey::Device(_) | TrustedKey::Grant(_) => None,
        })
    }

    pub fn grants(&self) -> impl Iterator<Item = &TrustedGrant> {
        self.keys.iter().filter_map(|key| match key {
            TrustedKey::Grant(held) => Some(held),
            TrustedKey::Device(_) | TrustedKey::Passkey(_) => None,
        })
    }

    /// The keys whose signatures a statement's envelope may carry: Devices and grants. A
    /// passkey whose key happens to be Ed25519 is not among them.
    pub fn signing_keys(&self) -> impl Iterator<Item = SigningPublicKey> + '_ {
        self.keys.iter().filter_map(TrustedKey::signing_key)
    }

    /// Whether the key behind a connection is a full CLI key on this list. Only such a
    /// key's own connection is Full.
    #[must_use]
    pub fn full(&self, remote: &ClientKey) -> bool {
        self.devices()
            .any(|device| ClientKey::from(device) == *remote)
    }

    /// The trusted key that approved as `by`.
    #[must_use]
    pub fn of(&self, by: &Approver) -> Option<&TrustedKey> {
        self.keys.iter().find(|key| key.approves_as(by))
    }

    /// Whether `by` may approve what `ask` asks: its kind signs such a statement, its
    /// scope holds the cap, one of its places holds the target with every Sealed part a
    /// version opens, and its grant names every Volume a version mounts. This is the one
    /// check of what a key may approve, so one key covers a whole effect alone.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::UnknownKey`] or [`AuthorityRefusal::UnknownPasskey`]
    /// when `by` is no key on this list, [`AuthorityRefusal::OutOfScope`] when its kind or
    /// scope doesn't reach, [`AuthorityRefusal::Unscoped`] for a CI key that names no
    /// places, and [`AuthorityRefusal::VolumeNotGranted`] for a Volume its
    /// grant doesn't name.
    pub fn covers(&self, by: &Approver, ask: Ask<'_>) -> Result<(), AuthorityRefusal> {
        let key = self.of(by).ok_or(match by {
            Approver::Signing(key) => AuthorityRefusal::UnknownKey { key: *key },
            Approver::Passkey(_) => AuthorityRefusal::UnknownPasskey,
        })?;
        super::scope::allows(key.kind(), key.grant(), ask).map_err(|denied| match denied {
            Denied::Out => AuthorityRefusal::OutOfScope {
                code: key.code(),
                kind: key.kind(),
                what: ask.what(),
            },
            Denied::Values => AuthorityRefusal::OutOfScope {
                code: key.code(),
                kind: key.kind(),
                what: "Sealed values outside its places",
            },
            Denied::Volume(volume) => AuthorityRefusal::VolumeNotGranted {
                code: key.code(),
                kind: key.kind(),
                volume,
            },
            Denied::Unscoped => AuthorityRefusal::Unscoped { code: key.code() },
        })
    }

    /// Whether `by` still covers a kept Container of `service` that reaches `reach`, as
    /// [`Self::covers`] decides for a version of it. A start reads this, so narrowing or
    /// moving a grant stops every later start outside its new scope. `None` is a Container
    /// kept before its reach was: a Device or passkey still stands for it, and a CI or
    /// agent key, whose grant this Machine can no longer check it against, does not.
    ///
    /// # Errors
    /// As [`Self::covers`], and [`AuthorityRefusal::Mismatch`] naming
    /// [`MismatchField::KeptReach`] for a grant without a kept reach.
    pub fn covers_kept(
        &self,
        by: &Approver,
        service: &QualifiedService,
        reach: Option<&Reach>,
    ) -> Result<(), AuthorityRefusal> {
        let ask = |reach| Ask::Version {
            namespace: &service.namespace,
            service: &service.name,
            forgets: false,
            reaches_host: false,
            reach,
        };
        match reach {
            Some(reach) => self.covers(by, ask(reach)),
            None => {
                self.covers(by, ask(Reach::none()))?;
                match self.of(by).map(TrustedKey::kind) {
                    Some(kind) if kind.whole_cluster() => Ok(()),
                    _ => Err(AuthorityRefusal::Mismatch {
                        field: MismatchField::KeptReach,
                    }),
                }
            }
        }
    }

    /// The trusted passkey that answered `assertion` to `challenge`.
    ///
    /// A credential ID is the authenticator's claim, not a key, so every trusted passkey
    /// under it is tried and only a verifying signature picks one. What the passkey may
    /// approve is [`Self::covers`]'s to say.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::UnknownPasskey`] when no trusted passkey has the
    /// assertion's credential, and the first candidate's refusal when none verifies.
    pub fn passkey_approval(
        &self,
        assertion: &PasskeyAssertion,
        challenge: &[u8],
    ) -> Result<&TrustedPasskey, AuthorityRefusal> {
        let mut candidates = self
            .passkeys()
            .filter(|trusted| trusted.passkey.credential_id == assertion.credential_id);
        let first = candidates.next().ok_or(AuthorityRefusal::UnknownPasskey)?;
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
