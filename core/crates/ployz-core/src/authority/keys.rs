//! Keys, signatures, and the signed envelope every authority statement travels in.

use std::{fmt, marker::PhantomData};

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Digest as _;
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, Timestamp, hex_bytes, write_hex},
    proposal::ProposalApproval,
    refusal::AuthorityRefusal,
    restart::SignerKey,
    session::{Grant, SessionGrant},
    trusted_key::{PasskeyAssertion, TrustedKeys},
};
use crate::{DeviceCode, DeviceKey};

/// What a signature over a statement is for. Each purpose signs under its own prefix,
/// so a signature made for one statement never verifies as another.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Workflow,
    Join,
    Enforce,
    /// Standing authority over every future push, such as an automatic deploy rule. A
    /// Device signs one, or a passkey answers its own prompt for it: a session grant
    /// covers workflows only.
    Rule,
    /// One Volume workflow, which can stop a Service, start one elsewhere and send a Volume's
    /// data to another Machine. Only a Device signs one: a session grant covers none.
    Volume,
}

impl Purpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
            Self::Join => "join",
            Self::Enforce => "enforce",
            Self::Rule => "rule",
            Self::Volume => "volume",
        }
    }
}

impl fmt::Display for Purpose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A body some key signs. Every statement denies unknown fields: a field an older
/// Machine cannot read could be a restriction it would fail to enforce.
pub trait Statement: Serialize + DeserializeOwned {
    const PURPOSE: Purpose;
}

/// The bytes a signer signs: `ployz-authority/v1`, the purpose, and the canonical body,
/// each ended by a newline except the body.
#[must_use]
pub fn signing_input(purpose: Purpose, body: &str) -> Vec<u8> {
    let mut input = Vec::with_capacity(32 + body.len());
    input.extend_from_slice(b"ployz-authority/v1\n");
    input.extend_from_slice(purpose.as_str().as_bytes());
    input.push(b'\n');
    input.extend_from_slice(body.as_bytes());
    input
}

/// An Ed25519 public key, written as 64 lowercase hex digits. A Device's management
/// key is one.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct SigningPublicKey([u8; 32]);

impl SigningPublicKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The Device code of this key, the name a person knows it by.
    #[must_use]
    pub fn code(&self) -> DeviceCode {
        DeviceKey::from_bytes(self.0).code()
    }

    /// Whether `signature` is this key's strict Ed25519 signature over `message`.
    #[must_use]
    pub fn verifies(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        VerifyingKey::from_bytes(&self.0).is_ok_and(|key| {
            key.verify_strict(message, &ed25519_dalek::Signature::from_bytes(signature))
                .is_ok()
        })
    }

    /// # Errors
    /// Returns a [`crate::ValueError`] unless `value` is 64 lowercase hex digits.
    pub fn parse(value: &str) -> Result<Self, crate::ValueError> {
        hex_bytes(value)
            .map(Self)
            .ok_or_else(|| crate::ValueError::new("signing key", value, "64 lowercase hex digits"))
    }
}

impl From<DeviceKey> for SigningPublicKey {
    fn from(key: DeviceKey) -> Self {
        Self(*key.as_bytes())
    }
}

impl From<SigningPublicKey> for DeviceKey {
    fn from(key: SigningPublicKey) -> Self {
        Self::from_bytes(key.0)
    }
}

impl fmt::Display for SigningPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(&self.0, formatter)
    }
}

impl fmt::Debug for SigningPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SigningPublicKey({})", self.code())
    }
}

impl TryFrom<String> for SigningPublicKey {
    type Error = crate::ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SigningPublicKey> for String {
    fn from(key: SigningPublicKey) -> Self {
        key.to_string()
    }
}

/// The private half of a signing key, built from the 32-byte seed a Device keeps in
/// its key file.
pub struct Signer(SigningKey);

impl Signer {
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    #[must_use]
    pub fn public(&self) -> SigningPublicKey {
        SigningPublicKey(self.0.verifying_key().to_bytes())
    }
}

impl fmt::Debug for Signer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Signer({})", self.public().code())
    }
}

/// `signature`, 64 bytes as lowercase hex, made by `key` over a statement's signing input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct KeySignature {
    pub key: SigningPublicKey,
    #[serde(serialize_with = "signature_hex", deserialize_with = "signature_bytes")]
    #[ts(type = "string")]
    pub signature: [u8; 64],
}

fn signature_hex<S: serde::Serializer>(
    signature: &[u8; 64],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use fmt::Write as _;
    let mut hex = String::with_capacity(128);
    for byte in signature {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    serializer.serialize_str(&hex)
}

fn signature_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<[u8; 64], D::Error> {
    let value = String::deserialize(deserializer)?;
    hex_bytes(&value)
        .ok_or_else(|| serde::de::Error::custom("signature must be 128 lowercase hex digits"))
}

impl KeySignature {
    fn new(signer: &Signer, input: &[u8]) -> Self {
        Self {
            key: signer.public(),
            signature: signer.0.sign(input).to_bytes(),
        }
    }

    fn verifies(&self, input: &[u8]) -> bool {
        self.key.verifies(input, &self.signature)
    }
}

/// A statement as it travels: its canonical body, exactly as signed, its signatures, and
/// the session grants that vouch for a browser key among the signers. On the wire it has
/// the same shape for every statement, which the TypeScript side sees as one type.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(concrete(T = super::restart::RestartApproval))]
pub struct Signed<T> {
    pub body: String,
    pub signatures: Vec<KeySignature>,
    /// Absent from a Device's envelope, which keeps the bytes a daemon from before grants
    /// reads. A daemon from before grants refuses an envelope that carries one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<SessionGrant>>")]
    pub grants: Vec<SessionGrant>,
    /// The signed proposal this unsigned step belongs to. Present only on a step of a
    /// proposal, which carries no signature of its own. A daemon from before proposals
    /// refuses an envelope that carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<Box<Signed<ProposalApproval>>>,
    /// Passkeys' own answers to [`Signed::challenge`], each a prompt of its own rather
    /// than a session. Only a rule takes one. A daemon from before passkey-signed rules
    /// refuses an envelope that carries one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<PasskeyAssertion>>")]
    pub assertions: Vec<PasskeyAssertion>,
    #[serde(skip)]
    #[ts(skip)]
    statement: PhantomData<T>,
}

impl<T: Statement> Signed<T> {
    /// Encode `statement` and sign it with `signer`.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the statement has no canonical form.
    pub fn sign(statement: &T, signer: &Signer) -> Result<Self, CanonicalError> {
        let body = canonical::to_canonical(statement)?;
        let signatures = vec![KeySignature::new(signer, &signing_input(T::PURPOSE, &body))];
        Ok(Self {
            body,
            signatures,
            grants: Vec::new(),
            proposal: None,
            assertions: Vec::new(),
            statement: PhantomData,
        })
    }

    /// Encode `statement` as one unsigned step of `bundle`, which vouches for it.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the statement has no canonical form.
    pub fn proposed(
        statement: &T,
        bundle: Signed<ProposalApproval>,
    ) -> Result<Self, CanonicalError> {
        Ok(Self {
            body: canonical::to_canonical(statement)?,
            signatures: Vec::new(),
            grants: Vec::new(),
            proposal: Some(Box::new(bundle)),
            assertions: Vec::new(),
            statement: PhantomData,
        })
    }

    /// Encode `statement`, sign it with a browser key, and carry the grant that vouches
    /// for that key.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the statement has no canonical form.
    pub fn sign_under(
        statement: &T,
        signer: &Signer,
        grant: SessionGrant,
    ) -> Result<Self, CanonicalError> {
        let mut signed = Self::sign(statement, signer)?;
        signed.grants.push(grant);
        Ok(signed)
    }

    /// Encode `statement` with no signature, for a passkey to answer [`Self::challenge`]
    /// and [`Self::answered`] to carry the answer.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when the statement has no canonical form.
    pub fn unsigned(statement: &T) -> Result<Self, CanonicalError> {
        Ok(Self {
            body: canonical::to_canonical(statement)?,
            signatures: Vec::new(),
            grants: Vec::new(),
            proposal: None,
            assertions: Vec::new(),
            statement: PhantomData,
        })
    }

    /// Carry a passkey's own answer to [`Self::challenge`].
    #[must_use]
    pub fn answered(mut self, assertion: PasskeyAssertion) -> Self {
        self.assertions.push(assertion);
        self
    }

    /// The WebAuthn challenge a passkey answers to sign this statement itself: the
    /// SHA-256 of its signing input, as a session grant's is.
    #[must_use]
    pub fn challenge(&self) -> [u8; 32] {
        sha2::Sha256::digest(self.signing_input()).into()
    }

    /// Parse the body, refusing it unless it is canonical. Checks no signature.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Malformed`] when the body is not this statement in
    /// canonical form.
    pub fn open(&self) -> Result<T, AuthorityRefusal> {
        canonical::from_canonical(&self.body).map_err(|error| AuthorityRefusal::Malformed {
            purpose: T::PURPOSE,
            problem: error.to_string(),
        })
    }

    #[must_use]
    pub fn signing_input(&self) -> Vec<u8> {
        signing_input(T::PURPOSE, &self.body)
    }

    /// The keys, all among `trusted`, whose signatures on this statement verify.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Unsigned`] when there is no signature,
    /// [`AuthorityRefusal::UnknownKey`] when one names a key not in `trusted`, and
    /// [`AuthorityRefusal::BadSignature`] when one does not verify.
    pub fn verified_signers(
        &self,
        trusted: impl IntoIterator<Item = SigningPublicKey>,
    ) -> Result<Vec<SigningPublicKey>, AuthorityRefusal> {
        if self.signatures.is_empty() {
            return Err(AuthorityRefusal::Unsigned {
                purpose: T::PURPOSE,
            });
        }
        let trusted: Vec<_> = trusted.into_iter().collect();
        let input = self.signing_input();
        self.signatures
            .iter()
            .map(|signature| {
                if !trusted.contains(&signature.key) {
                    return Err(AuthorityRefusal::UnknownKey { key: signature.key });
                }
                if !signature.verifies(&input) {
                    return Err(AuthorityRefusal::BadSignature { key: signature.key });
                }
                Ok(signature.key)
            })
            .collect()
    }

    /// Who signed this statement, each checked against `trusted` at `now`: a Device it
    /// trusts, a browser key a grant in this envelope vouches for, or, on a rule only, a
    /// trusted passkey's own answer to [`Self::challenge`]. A grant only says what its key
    /// may do; the statement's own check decides whether its scope covers this statement.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Unsigned`] when nothing signed,
    /// [`AuthorityRefusal::UnknownKey`] when a key is neither a trusted Device nor named
    /// by a grant here, [`AuthorityRefusal::BadSignature`] when a signature does not
    /// verify, the grant's refusal from [`SessionGrant::verify`], a mismatch naming a
    /// passkey for an answer to a statement that takes none, and the passkey's refusal
    /// from [`TrustedKeys::passkey_approval`].
    pub fn approvers(
        &self,
        trusted: &TrustedKeys,
        now: Timestamp,
    ) -> Result<Vec<Approver>, AuthorityRefusal> {
        if self.signatures.is_empty() && self.assertions.is_empty() {
            return Err(AuthorityRefusal::Unsigned {
                purpose: T::PURPOSE,
            });
        }
        let input = self.signing_input();
        let mut approvers = self
            .signatures
            .iter()
            .map(|signature| {
                let device = trusted.signing_keys().any(|key| key == signature.key);
                let grant = (!device)
                    .then(|| {
                        self.grants
                            .iter()
                            .find(|grant| grant.browser_key() == Some(signature.key))
                    })
                    .flatten();
                if !device && grant.is_none() {
                    return Err(AuthorityRefusal::UnknownKey { key: signature.key });
                }
                if !signature.verifies(&input) {
                    return Err(AuthorityRefusal::BadSignature { key: signature.key });
                }
                Ok(match grant {
                    None => Approver::Device(signature.key),
                    Some(grant) => Approver::Browser {
                        key: signature.key,
                        grant: grant.verify(trusted, now)?,
                    },
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if self.assertions.is_empty() {
            return Ok(approvers);
        }
        // A workflow rests on a passkey only through a session grant, whose limits the
        // workflow's own check reads. A passkey's own answer has none, so only a rule,
        // which no session may sign, takes one.
        if T::PURPOSE != Purpose::Rule {
            return Err(AuthorityRefusal::Mismatch { field: "a passkey" });
        }
        let challenge = self.challenge();
        for assertion in &self.assertions {
            let passkey = trusted.passkey_approval(T::PURPOSE, assertion, &challenge)?;
            approvers.push(Approver::Passkey(passkey.passkey.code()));
        }
        Ok(approvers)
    }
}

/// A signer whose signature on a statement verified, and what vouches for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Approver {
    /// A Device this Machine trusts.
    Device(SigningPublicKey),
    /// A browser key a passkey this Machine trusts lent `grant`.
    Browser { key: SigningPublicKey, grant: Grant },
    /// A passkey this Machine trusts, answering a prompt of its own for this statement.
    Passkey(DeviceCode),
}

impl Approver {
    /// Whether this approver is the signer a statement names, vouched for the way the
    /// statement says: a Device as that Device, a browser key under a grant, a passkey by
    /// its own prompt.
    #[must_use]
    pub fn signs_as(&self, signer: SignerKey) -> bool {
        match (self, signer) {
            (Self::Device(key), SignerKey::Device(device)) => *key == device.into(),
            (Self::Browser { key, .. }, SignerKey::Browser { browser }) => *key == browser,
            (Self::Passkey(_), SignerKey::Passkey(_)) => true,
            _ => false,
        }
    }
}

impl fmt::Display for Approver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(key) => write!(formatter, "device {}", key.code()),
            Self::Browser { key, grant } => write!(
                formatter,
                "passkey {} via browser key {}",
                grant.passkey,
                key.code()
            ),
            Self::Passkey(code) => write!(formatter, "passkey {code}"),
        }
    }
}
