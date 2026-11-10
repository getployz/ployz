//! The approval a person gives to turn Private Access on: one Tailscale auth key, sealed
//! to one Cluster, that a router may join a tailnet with.
//!
//! A router advertises the whole Cluster network to the tailnet its key joins, so the
//! key is the tailnet. The approval binds the sealed key's digest, so a Machine places a
//! router only for a key a person approved. Turning Private Access off needs none.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use ts_rs::TS;

use super::{
    canonical::{Timestamp, hex_bytes, write_hex},
    keys::{Approver, Purpose, Signed, Statement},
    refusal::{AuthorityRefusal, field, first_mismatch},
    restart::SignerKey,
    trusted_key::TrustedKeys,
};
use crate::{Ciphertext, SealingPublicKey};

/// SHA-256 of a sealed auth key's bytes, written as 64 lowercase hex digits.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct AuthKeyDigest([u8; 32]);

impl AuthKeyDigest {
    #[must_use]
    pub fn of(sealed: &Ciphertext) -> Self {
        Self(sha2::Sha256::digest(sealed.as_bytes()).into())
    }

    /// # Errors
    /// Returns a [`crate::ValueError`] unless `value` is 64 lowercase hex digits.
    pub fn parse(value: &str) -> Result<Self, crate::ValueError> {
        hex_bytes(value).map(Self).ok_or_else(|| {
            crate::ValueError::new("auth key digest", value, "64 lowercase hex digits")
        })
    }
}

impl fmt::Display for AuthKeyDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_hex(&self.0, formatter)
    }
}

impl fmt::Debug for AuthKeyDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "AuthKeyDigest({self})")
    }
}

impl TryFrom<String> for AuthKeyDigest {
    type Error = crate::ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<AuthKeyDigest> for String {
    fn from(digest: AuthKeyDigest) -> Self {
        digest.to_string()
    }
}

/// A person approves the Private Access router joining the tailnet of one sealed auth
/// key, in one Cluster, for a bounded lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PrivateAccessApproval {
    /// The Cluster's Sealing public key, which every Machine holds and Cloud cannot mint.
    pub cluster: SealingPublicKey,
    /// The digest of the sealed auth key the router joins with.
    pub auth_key: AuthKeyDigest,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// A Device, or `"passkey"` for a passkey's own prompt. A browser key is refused.
    pub signer: SignerKey,
}

impl Statement for PrivateAccessApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
    const PASSKEY_ANSWERS: bool = true;
}

/// What a client knows when it drafts an approval: the Cluster, the key it sealed to it,
/// when, and who signs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PrivateAccessTerms {
    pub cluster: SealingPublicKey,
    pub sealed_auth_key: Ciphertext,
    pub issued_at: Timestamp,
    pub signer: SignerKey,
}

impl PrivateAccessApproval {
    /// The approval of `terms`, for the longest lifetime a Machine takes.
    #[must_use]
    pub fn drafted(terms: &PrivateAccessTerms) -> Self {
        Self {
            cluster: terms.cluster,
            auth_key: AuthKeyDigest::of(&terms.sealed_auth_key),
            issued_at: terms.issued_at,
            expires_at: terms.issued_at.plus(super::validity::MAX_LIFETIME_MILLIS),
            signer: terms.signer,
        }
    }
}

/// What a Machine knows when it checks a Private Access approval.
#[derive(Clone, Debug)]
pub struct PrivateAccessFacts<'a> {
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub trusted: &'a TrustedKeys,
    /// The sealed auth key the request carries.
    pub sealed: &'a Ciphertext,
    pub now: Timestamp,
}

/// Check `approval` lets this Machine join the tailnet of `facts.sealed` now: signed by a
/// Device this Machine trusts, or answered by a trusted passkey's own prompt. A browser
/// key is refused even under a valid session grant, since a grant lent for deploys says
/// nothing about which tailnet reaches the Cluster. So is an envelope that rides a
/// proposal.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a signature problem, a malformed body, a
/// signer that is not a trusted Device or passkey, a body naming another Cluster or auth
/// key, an invalid lifetime, or an expired window.
pub fn verify(
    approval: &Signed<PrivateAccessApproval>,
    facts: &PrivateAccessFacts<'_>,
) -> Result<Approver, AuthorityRefusal> {
    let approvers = approval.approvers(facts.trusted, facts.now)?;
    let body = approval.open()?;
    if approval.proposal.is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: "a proposal",
        });
    }
    let by = approvers
        .into_iter()
        .find(|approver| {
            matches!(approver, Approver::Device(_) | Approver::Passkey(_))
                && approver.signs_as(body.signer)
        })
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })?;
    let cluster = facts.cluster.ok_or(AuthorityRefusal::NoClusterIdentity)?;
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        (
            "an auth key",
            body.auth_key == AuthKeyDigest::of(facts.sealed),
        ),
    ])?;
    super::validity::verify(body.issued_at, body.expires_at, facts.now)?;
    Ok(by)
}

#[cfg(test)]
#[path = "private_access_tests.rs"]
mod tests;
