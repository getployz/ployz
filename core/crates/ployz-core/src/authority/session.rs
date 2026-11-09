//! A session grant: one passkey prompt lends a short-lived browser key the right to approve
//! workflows in one Environment of one Cluster, for at most an hour.
//!
//! The browser makes an Ed25519 key, writes a [`GrantBody`] naming it and its limits, and
//! asks the passkey to answer a challenge that is the digest of that body. A Machine checks
//! the answer against a passkey it trusts, so the grant covers exactly the body the person
//! approved. Each statement the browser key then signs still carries its own lifetime, at
//! most [`super::validity::MAX_LIFETIME_MILLIS`], and must sit inside the grant's window.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;
use uuid::Uuid;

use super::{
    AuthorityRefusal, Purpose, SigningPublicKey, Statement, Timestamp,
    canonical::{self},
    keys::signing_input,
    trusted_key::{PasskeyAssertion, TrustedKeys},
    validity,
};
use crate::{DeviceCode, MachineId, Namespace, SealingPublicKey};

/// The longest a grant may last, excluding clock skew.
pub const MAX_GRANT_LIFETIME_MILLIS: u64 = 60 * 60 * 1000;

/// A kind of action a grant lets its browser key approve.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ActionFamily {
    Deploy,
    Restart,
    Settings,
    Start,
    Stop,
}

/// Whether the browser key may lend its grant on. It never may; the field exists so a
/// later grant that could would not read as this one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Delegation {
    None,
}

/// What a passkey approves when it answers a session prompt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GrantBody {
    pub browser_key: SigningPublicKey,
    pub cluster: SealingPublicKey,
    pub environment: Namespace,
    /// The Cloud Environment `environment` deploys, as a [`crate::SealedIdentity`] names
    /// it. The browser key approves only Containers whose Sealed values were sealed for it.
    #[ts(type = "string")]
    pub environment_id: Uuid,
    /// Sorted and distinct.
    #[serde(deserialize_with = "sorted_distinct")]
    pub machines: Vec<MachineId>,
    /// Sorted and distinct.
    #[serde(deserialize_with = "sorted_distinct")]
    pub families: Vec<ActionFamily>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub delegation: Delegation,
}

impl Statement for GrantBody {
    const PURPOSE: Purpose = Purpose::Workflow;
}

fn sorted_distinct<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Ord,
{
    let items = Vec::<T>::deserialize(deserializer)?;
    if items
        .iter()
        .zip(items.iter().skip(1))
        .any(|(left, right)| left >= right)
    {
        return Err(serde::de::Error::custom("must be sorted and distinct"));
    }
    Ok(items)
}

impl GrantBody {
    /// The WebAuthn challenge a passkey answers to approve `body`: the SHA-256 of its
    /// signing input, so an answer made for anything else never reads as a grant.
    #[must_use]
    pub fn challenge(body: &str) -> [u8; 32] {
        Sha256::digest(signing_input(Self::PURPOSE, body)).into()
    }

    /// Check this grant lets its browser key approve a statement of `scope`.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Mismatch`] naming the first part of `scope` the grant
    /// does not cover.
    pub fn covers(&self, scope: &GrantScope<'_>) -> Result<(), AuthorityRefusal> {
        let checks = [
            ("a Cluster", self.cluster == scope.cluster),
            ("an Environment", &self.environment == scope.environment),
            (
                "Machines",
                scope
                    .machines
                    .iter()
                    .all(|machine| self.machines.binary_search(machine).is_ok()),
            ),
            ("an action", self.families.contains(&scope.family)),
            (
                "a window",
                scope.issued_at >= self.issued_at && scope.expires_at <= self.expires_at,
            ),
        ];
        match checks.iter().find(|(_, holds)| !holds) {
            Some((field, _)) => Err(AuthorityRefusal::Mismatch { field }),
            None => Ok(()),
        }
    }
}

/// What a statement signed under a grant asks of it.
#[derive(Clone, Copy, Debug)]
pub struct GrantScope<'a> {
    pub cluster: SealingPublicKey,
    pub environment: &'a Namespace,
    pub machines: &'a [MachineId],
    pub family: ActionFamily,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
}

/// A grant as it travels beside a statement: its canonical body, exactly as the passkey
/// approved it, and the passkey's answer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SessionGrant {
    pub body: String,
    pub assertion: PasskeyAssertion,
}

/// A grant this Machine checked: what it allows, and the passkey that approved it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Grant {
    pub body: GrantBody,
    pub passkey: DeviceCode,
}

impl SessionGrant {
    /// The browser key this grant names, read without checking anything else.
    #[must_use]
    pub fn browser_key(&self) -> Option<SigningPublicKey> {
        canonical::from_canonical::<GrantBody>(&self.body)
            .ok()
            .map(|body| body.browser_key)
    }

    /// Check a passkey `trusted` holds approved exactly this body, and that its window
    /// holds at `now`.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Malformed`] for a body that is not a canonical grant,
    /// the passkey's refusal from [`TrustedKeys::passkey_approval`] (a changed body fails
    /// its challenge), a mismatch for a window longer than an hour or issued in the
    /// future, and [`AuthorityRefusal::Expired`] once it ended.
    pub fn verify(&self, trusted: &TrustedKeys, now: Timestamp) -> Result<Grant, AuthorityRefusal> {
        let body: GrantBody =
            canonical::from_canonical(&self.body).map_err(|error| AuthorityRefusal::Malformed {
                purpose: GrantBody::PURPOSE,
                problem: format!("its session grant: {error}"),
            })?;
        let passkey = trusted.passkey_approval(
            GrantBody::PURPOSE,
            &self.assertion,
            &GrantBody::challenge(&self.body),
        )?;
        validity::verify_within(
            body.issued_at,
            body.expires_at,
            now,
            MAX_GRANT_LIFETIME_MILLIS,
        )?;
        Ok(Grant {
            body,
            passkey: passkey.passkey.code(),
        })
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
