//! A person's change to the keys a Machine trusts: trust one more key, edit the scope of a
//! CI or agent key, or stop trusting one for good.
//!
//! A trust widens what a Machine accepts, so it is one-shot: it rides in a bundle, holds
//! only inside the bundle's window on the Machine's floored clock, and names the Members
//! that may apply it. A trust of a CI or agent key the Machine already holds as the same
//! kind edits its scope and Volumes, when its bundle was issued after the one that set
//! them, so an older and wider step can't be replayed. A removal only narrows, so it has no
//! window and may be sent again at any time. Each Machine applies a step to its own record
//! and nothing else.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    keys::{Approver, Purpose, Signed, SignerKey, SigningPublicKey, Statement},
    open::{open_approval, open_one_shot},
    proposal::{ProposalRef, StepBody, StepKind},
    refusal::{AuthorityRefusal, MismatchField, first_mismatch},
    scope::{Ask, Grant, GrantKind, GrantVolume, KeyKind, KeyScope},
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
    fn ask(&self) -> Ask<'_> {
        Ask::Key
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

/// A key to trust. A Device and a passkey carry no scope: they hold their kind's ceiling
/// over the whole Cluster, so no step can narrow them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NewKey {
    Device {
        key: DeviceKey,
    },
    Passkey {
        passkey: Passkey,
    },
    Ci {
        key: SigningPublicKey,
        scope: KeyScope,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(optional, as = "Option<Vec<GrantVolume>>")]
        volumes: Vec<GrantVolume>,
    },
    Agent {
        key: SigningPublicKey,
        scope: KeyScope,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(optional, as = "Option<Vec<GrantVolume>>")]
        volumes: Vec<GrantVolume>,
    },
}

impl NewKey {
    #[must_use]
    pub fn key(&self) -> KeyRef {
        match self {
            Self::Device { key } => KeyRef::Signing { key: *key },
            Self::Passkey { passkey } => KeyRef::Passkey {
                key: passkey.public_key,
            },
            Self::Ci { key, .. } | Self::Agent { key, .. } => {
                KeyRef::Signing { key: (*key).into() }
            }
        }
    }

    /// The grant a CI or agent key holds once a bundle issued at `issued_at` trusts it.
    /// `None` for a Device or passkey.
    #[must_use]
    pub fn grant(self, issued_at: Timestamp) -> Option<Grant> {
        let (key, kind, scope, volumes) = match self {
            Self::Ci {
                key,
                scope,
                volumes,
            } => (key, GrantKind::Ci, scope, volumes),
            Self::Agent {
                key,
                scope,
                volumes,
            } => (key, GrantKind::Agent, scope, volumes),
            Self::Device { .. } | Self::Passkey { .. } => return None,
        };
        Some(Grant {
            key,
            kind,
            scope,
            volumes,
            issued_at,
        })
    }

    /// Whether a CI or agent key's scope fits its kind. A Device and a passkey always do.
    fn fits(&self) -> bool {
        match self {
            Self::Ci { scope, .. } => scope.fits(KeyKind::Ci),
            Self::Agent { scope, .. } => scope.fits(KeyKind::Agent),
            Self::Device { .. } | Self::Passkey { .. } => true,
        }
    }
}

/// A trusted key by its full public key, never by its code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum KeyRef {
    /// A Device's, CI or agent key's Ed25519 key. Written `device`, as Devices were the
    /// only such keys when it was named.
    #[serde(rename = "device")]
    Signing {
        key: DeviceKey,
    },
    Passkey {
        key: PasskeyPublicKey,
    },
}

impl KeyRef {
    #[must_use]
    pub fn code(&self) -> DeviceCode {
        match self {
            Self::Signing { key } => key.code(),
            Self::Passkey { key } => key.code(),
        }
    }
}

/// Read a list of removed keys, skipping each entry this build can't read, such as a kind
/// a newer daemon removed. A key this build can't name it can't remove either.
///
/// # Errors
/// Returns the deserializer's error when the value is not a list.
pub fn readable_keys<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<KeyRef>, D::Error> {
    Ok(Vec::<serde_json::Value>::deserialize(deserializer)?
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect())
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

/// A Key step this Machine accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyVerified {
    /// Trust `key`, under a bundle issued at `issued_at`.
    Trust {
        key: NewKey,
        issued_at: Timestamp,
    },
    Remove {
        key: KeyRef,
    },
}

/// Check a Key step on this Machine. A trust opens as a one-shot step of a bundle whose
/// window holds at `facts.now` and that names this Machine among its Members. A removal
/// opens alone or as a step, with no window. Whether the key was already removed here,
/// whether a trust edits a grant, and whether a removal leaves a key behind, are the
/// record's to say.
///
/// # Errors
/// Returns the opening's refusal, or a mismatch naming the Cluster, this Machine, or a
/// CI or agent key scope its kind may not hold.
pub fn verify(
    step: &Signed<KeyStep>,
    facts: &KeyFacts<'_>,
) -> Result<(KeyVerified, Approver), AuthorityRefusal> {
    let trust = matches!(step.open()?.change, KeyChange::Trust { .. });
    let (body, by, cluster) = if trust {
        open_one_shot(step, facts.trusted, facts.cluster, facts.now)?
    } else {
        open_approval(step, facts.trusted, facts.cluster)?
    };
    first_mismatch([
        (MismatchField::Cluster, body.cluster == cluster),
        (
            MismatchField::ThisMachine,
            match &body.change {
                KeyChange::Trust { members, .. } => members.contains(&facts.member),
                KeyChange::Remove { .. } => true,
            },
        ),
        (
            MismatchField::KeyScope,
            match &body.change {
                KeyChange::Trust { key, .. } => key.fits(),
                KeyChange::Remove { .. } => true,
            },
        ),
    ])?;
    let verified = match body.change {
        KeyChange::Trust { key, .. } => {
            let bundle = step.proposal.as_deref().ok_or(AuthorityRefusal::Mismatch {
                field: MismatchField::Proposal,
            })?;
            KeyVerified::Trust {
                key,
                issued_at: bundle.open()?.issued_at,
            }
        }
        KeyChange::Remove { key } => KeyVerified::Remove { key },
    };
    Ok((verified, by))
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
