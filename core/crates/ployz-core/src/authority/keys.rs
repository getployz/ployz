//! Keys, signatures, and the signed envelope every authority statement travels in.

use std::{fmt, marker::PhantomData};

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use ts_rs::TS;

use super::{
    canonical::{self, CanonicalError, hex_bytes, write_hex},
    refusal::AuthorityRefusal,
};
use crate::{DeviceCode, DeviceKey};

/// What a signature over a statement is for. Each purpose signs under its own prefix,
/// so a signature made for one statement never verifies as another.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Workflow,
}

impl Purpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workflow => "workflow",
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
    pub signature: String,
}

impl KeySignature {
    fn new(signer: &Signer, input: &[u8]) -> Self {
        let mut hex = String::with_capacity(128);
        for byte in signer.0.sign(input).to_bytes() {
            use fmt::Write as _;
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Self {
            key: signer.public(),
            signature: hex,
        }
    }

    fn verifies(&self, input: &[u8]) -> bool {
        hex_bytes::<64>(&self.signature)
            .is_some_and(|signature| self.key.verifies(input, &signature))
    }
}

/// A statement as it travels: its canonical body, exactly as signed, and its signatures.
/// On the wire it has the same shape for every statement, which the TypeScript side
/// sees as one type.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(concrete(T = super::restart::RestartApproval))]
pub struct Signed<T> {
    pub body: String,
    pub signatures: Vec<KeySignature>,
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
            statement: PhantomData,
        })
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
}
