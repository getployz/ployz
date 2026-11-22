//! Keys, signatures, and the signed envelope every authority statement travels in.

use crate::authority::MismatchField;
use std::{fmt, marker::PhantomData};

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::Digest as _;
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError},
    proposal::ProposalApproval,
    refusal::AuthorityRefusal,
    trusted_key::{PasskeyAssertion, TrustedKeys},
};
use crate::{DeviceCode, DeviceKey, separation, value::hex_newtype};

/// What a signature over a statement is for. Each purpose signs under its own prefix,
/// so a signature made for one statement never verifies as another.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Workflow,
    Join,
    /// Standing authority over every future push, such as an automatic deploy rule. A
    /// Device signs one, or a passkey answers its own prompt for it.
    Rule,
}

impl Purpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
            Self::Join => "join",
            Self::Rule => "rule",
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
    /// Whether a passkey's own answer may sign this statement: a rule, and a bundle, whose
    /// steps a person reads before the tap.
    const TAKES_ASSERTION: bool = false;
}

/// The bytes a signer signs: `ployz-authority/v1`, the purpose, and the canonical body,
/// each ended by a newline except the body.
#[must_use]
pub fn signing_input(purpose: Purpose, body: &str) -> Vec<u8> {
    let mut input = Vec::with_capacity(32 + body.len());
    input.extend_from_slice(separation::AUTHORITY_SIGNING);
    input.extend_from_slice(purpose.as_str().as_bytes());
    input.push(b'\n');
    input.extend_from_slice(body.as_bytes());
    input
}

hex_newtype!(
    /// An Ed25519 public key, written as 64 lowercase hex digits. A Device's management
    /// key is one.
    SigningPublicKey, "signing key", 32, "", "64 lowercase hex digits", without_debug
);

impl SigningPublicKey {
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

impl fmt::Debug for SigningPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SigningPublicKey({})", self.code())
    }
}

/// The key a statement says signed it. A Device's key is written bare. A rule a passkey
/// answers for itself says `"passkey"`: the browser writes the body before the person
/// picks which passkey answers, and the answer names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum SignerKey {
    Device(DeviceKey),
    Passkey(OwnPrompt),
}

impl From<DeviceKey> for SignerKey {
    fn from(key: DeviceKey) -> Self {
        Self::Device(key)
    }
}

impl From<SigningPublicKey> for SignerKey {
    fn from(key: SigningPublicKey) -> Self {
        Self::Device(key.into())
    }
}

/// The one value [`SignerKey::Passkey`] takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub enum OwnPrompt {
    #[serde(rename = "passkey")]
    Passkey,
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
    serializer.serialize_str(&crate::hex::encode(signature))
}

fn signature_bytes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<[u8; 64], D::Error> {
    let value = String::deserialize(deserializer)?;
    crate::hex::decode(&value)
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

/// A statement as it travels: its canonical body, exactly as signed, and its signatures.
/// On the wire it has the same shape for every statement, which the TypeScript side sees
/// as one type.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(concrete(T = super::version::ServiceVersion))]
pub struct Signed<T> {
    pub body: String,
    pub signatures: Vec<KeySignature>,
    /// The signed proposal this unsigned step belongs to. Present only on a step of a
    /// proposal, which carries no signature of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<Box<Signed<ProposalApproval>>>,
    /// Passkeys' own answers to [`Signed::challenge`], each a prompt of its own. Only a
    /// statement whose [`Statement::TAKES_ASSERTION`] holds takes one.
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
            proposal: Some(Box::new(bundle)),
            assertions: Vec::new(),
            statement: PhantomData,
        })
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
    /// SHA-256 of its signing input.
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

    /// Who signed this statement, each checked against `trusted`: a Device or grant it
    /// trusts or, on a statement that takes one, a trusted passkey's own answer to
    /// [`Self::challenge`]. What each may approve is [`TrustedKeys::covers`]'s to say.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Unsigned`] when nothing signed,
    /// [`AuthorityRefusal::UnknownKey`] when a key is neither a trusted Device nor a grant,
    /// [`AuthorityRefusal::BadSignature`] when a signature does not verify, a mismatch
    /// naming a passkey for an answer to a statement that takes none, and the passkey's
    /// refusal from [`TrustedKeys::passkey_approval`].
    pub fn approvers(&self, trusted: &TrustedKeys) -> Result<Vec<Approver>, AuthorityRefusal> {
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
                if !trusted.signing_keys().any(|key| key == signature.key) {
                    return Err(AuthorityRefusal::UnknownKey { key: signature.key });
                }
                if !signature.verifies(&input) {
                    return Err(AuthorityRefusal::BadSignature { key: signature.key });
                }
                Ok(Approver::Signing(signature.key))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if self.assertions.is_empty() {
            return Ok(approvers);
        }
        if !T::TAKES_ASSERTION {
            return Err(AuthorityRefusal::Mismatch {
                field: MismatchField::Passkey,
            });
        }
        let challenge = self.challenge();
        for assertion in &self.assertions {
            let passkey = trusted.passkey_approval(assertion, &challenge)?;
            approvers.push(Approver::Passkey(passkey.passkey.code()));
        }
        Ok(approvers)
    }
}

/// A signer whose signature on a statement verified, and what vouches for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Approver {
    /// A Device or grant this Machine trusts, by the Ed25519 key that signed.
    Signing(SigningPublicKey),
    /// A passkey this Machine trusts, answering a prompt of its own for this statement.
    Passkey(DeviceCode),
}

impl Approver {
    /// Whether this approver is the signer a statement names, vouched for the way the
    /// statement says: a signing key as that key, a passkey by its own prompt.
    #[must_use]
    pub fn signs_as(&self, signer: SignerKey) -> bool {
        match (self, signer) {
            (Self::Signing(key), SignerKey::Device(device)) => *key == device.into(),
            (Self::Passkey(_), SignerKey::Passkey(_)) => true,
            _ => false,
        }
    }
}

impl fmt::Display for Approver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signing(key) => write!(formatter, "key {}", key.code()),
            Self::Passkey(code) => write!(formatter, "passkey {code}"),
        }
    }
}

#[cfg(test)]
#[expect(clippy::indexing_slicing, reason = "JSON fixtures")]
mod tests {
    use super::{KeySignature, SigningPublicKey};

    fn signature() -> KeySignature {
        KeySignature {
            key: SigningPublicKey::from_bytes([0x11; 32]),
            signature: std::array::from_fn(|index| index as u8 * 4),
        }
    }

    #[test]
    fn a_signature_travels_as_128_lowercase_hex_digits() {
        let json = serde_json::to_value(signature()).unwrap();
        assert_eq!(json["key"], "11".repeat(32));
        assert_eq!(
            json["signature"],
            "0004080c1014181c2024282c3034383c4044484c5054585c6064686c7074787c8084888c9094989ca0a4a8acb0b4b8bcc0c4c8ccd0d4d8dce0e4e8ecf0f4f8fc"
        );
        assert_eq!(
            serde_json::from_value::<KeySignature>(json).unwrap(),
            signature()
        );
    }

    #[test]
    fn a_signature_that_is_not_lowercase_hex_of_64_bytes_is_refused() {
        let json = serde_json::to_value(signature()).unwrap();
        let hex = json["signature"].as_str().unwrap().to_owned();
        for refused in [
            hex.to_uppercase(),
            hex[2..].to_owned(),
            format!("{hex}00"),
            String::new(),
        ] {
            let mut edited = json.clone();
            edited["signature"] = refused.into();
            let error = serde_json::from_value::<KeySignature>(edited).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("signature must be 128 lowercase hex digits"),
                "{error}"
            );
        }
    }
}
