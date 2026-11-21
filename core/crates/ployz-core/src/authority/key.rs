//! A person's change to the keys a Machine trusts: trust one more key, or stop trusting one
//! for good.
//!
//! A trust widens what a Machine accepts, so it is one-shot: it rides in a bundle, holds
//! only inside the bundle's window on the Machine's floored clock, and names the Members
//! that may apply it. A removal only narrows, so it has no window and may be sent again at
//! any time. Each Machine applies a step to its own record and nothing else.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    keys::{Approver, Purpose, Signed, SignerKey, Statement},
    open::{open_approval, open_one_shot},
    proposal::{ProposalRef, StepBody, StepKind},
    refusal::{AuthorityRefusal, field, first_mismatch},
    trusted_key::{Passkey, PasskeyPublicKey, TrustedKeys},
};
use crate::{DeviceCode, DeviceKey, MachineId, Namespace, SealingPublicKey, WireGuardPublicKey};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct KeyStep {
    pub cluster: SealingPublicKey,
    pub change: KeyChange,
    pub signer: SignerKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<ProposalRef>,
}

impl Statement for KeyStep {
    const PURPOSE: Purpose = Purpose::Workflow;
}

impl StepBody for KeyStep {
    fn kind(&self) -> StepKind {
        StepKind::Key
    }
    fn place(&self) -> Option<ProposalRef> {
        self.proposal
    }
    fn signer(&self) -> SignerKey {
        self.signer
    }
    fn cluster(&self) -> SealingPublicKey {
        self.cluster
    }
    fn namespace(&self) -> Option<&Namespace> {
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyChange {
    /// Trust `key` on each of `members` and no other Machine, so a Server reset since the
    /// approval, which comes back with a new identity, never takes it.
    Trust {
        key: NewKey,
        members: Vec<KeyMember>,
    },
    /// Stop trusting `key` for good. A Machine that applied it never trusts the key again.
    Remove { key: KeyRef },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NewKey {
    Device { key: DeviceKey },
    Passkey { passkey: Passkey },
}

impl NewKey {
    #[must_use]
    pub fn key(&self) -> KeyRef {
        match self {
            Self::Device { key } => KeyRef::Device { key: *key },
            Self::Passkey { passkey } => KeyRef::Passkey {
                key: passkey.public_key,
            },
        }
    }
}

/// A trusted key by its full public key, never by its code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyRef {
    Device { key: DeviceKey },
    Passkey { key: PasskeyPublicKey },
}

impl KeyRef {
    #[must_use]
    pub fn code(&self) -> DeviceCode {
        match self {
            Self::Device { key } => key.code(),
            Self::Passkey { key } => key.code(),
        }
    }
}

/// A Member a key trust names: the Machine ID and WireGuard key its join approved.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct KeyMember {
    pub id: MachineId,
    pub public_key: WireGuardPublicKey,
}

/// What a Machine holds itself when a Key step reaches it.
#[derive(Clone, Copy, Debug)]
pub struct KeyFacts<'a> {
    pub trusted: &'a TrustedKeys,
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub member: KeyMember,
    /// This Machine's floored clock.
    pub now: Timestamp,
}

/// Check a Key step on this Machine. A trust opens as a one-shot step of a bundle whose
/// window holds at `facts.now` and that names this Machine among its Members. A removal
/// opens alone or as a step, with no window. Whether the key was already removed here, and
/// whether a removal leaves a key behind, are the record's to say.
///
/// # Errors
/// Returns the opening's refusal, or a mismatch naming the Cluster or this Machine.
pub fn verify(
    step: &Signed<KeyStep>,
    facts: &KeyFacts<'_>,
) -> Result<(KeyChange, Approver), AuthorityRefusal> {
    let trust = matches!(step.open()?.change, KeyChange::Trust { .. });
    let (body, by, cluster) = if trust {
        open_one_shot(step, facts.trusted, facts.cluster, facts.now)?
    } else {
        open_approval(step, facts.trusted, facts.cluster)?
    };
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        (
            "this Machine",
            match &body.change {
                KeyChange::Trust { members, .. } => members.contains(&facts.member),
                KeyChange::Remove { .. } => true,
            },
        ),
    ])?;
    Ok((body.change, by))
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
