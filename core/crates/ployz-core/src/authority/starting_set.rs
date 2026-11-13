//! Device authority for the Machines an enforcing Machine trusts without a join approval.

use serde::{Deserialize, Serialize};

use ts_rs::TS;

use super::{
    Approver, AuthorityRefusal, Purpose, Signed, Statement, Timestamp,
    restart::SignerKey,
    rule::{RuleStatement, verify_signature},
    trusted_key::TrustedKeys,
};
use crate::{DeviceCode, Machine, MachineId, MachineSubnet, OperationId, WireGuardPublicKey};

/// The parts of a Machine row membership compares. Endpoints and policy may change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Member {
    pub id: MachineId,
    pub public_key: WireGuardPublicKey,
    pub subnet: MachineSubnet,
}

impl Member {
    #[must_use]
    pub fn matches(&self, machine: &Machine) -> bool {
        self == &Self::from(machine)
    }
}

impl From<&Machine> for Member {
    fn from(machine: &Machine) -> Self {
        Self {
            id: machine.id,
            public_key: machine.public_key,
            subnet: machine.subnet,
        }
    }
}

impl From<&super::join::JoinAllocation> for Member {
    fn from(allocation: &super::join::JoinAllocation) -> Self {
        Self {
            id: allocation.id,
            public_key: allocation.public_key,
            subnet: allocation.subnet,
        }
    }
}

/// A person's approval of the Members every Machine trusts when it turns enforcement on.
///
/// A Device signs it, or a trusted passkey answers a prompt of its own for it. A browser
/// key under a session grant never does: the set stands for the Cluster's whole life.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct StartingSet {
    pub operation: OperationId,
    pub members: Vec<Member>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    /// A Device's key writes the same bytes an older daemon read here.
    pub signer: SignerKey,
    /// Keys added while enforcement was off that the person confirmed by fingerprint, so
    /// activation keeps them. Absent keeps none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_keys: Vec<DeviceCode>,
}

impl Statement for StartingSet {
    const PURPOSE: Purpose = Purpose::Enforce;
}

impl RuleStatement for StartingSet {
    fn signer(&self) -> SignerKey {
        self.signer
    }
}

/// Verify a starting set against `trusted`, the keys this Machine lets approve it, and this
/// Machine's own allocation. Returns the set and who approved it.
///
/// # Errors
/// Refuses an untrusted or mismatched signer, a browser key, an expired set, and a set that
/// leaves out this Machine, which scopes the set to one Cluster.
pub fn verify(
    signed: &Signed<StartingSet>,
    trusted: &TrustedKeys,
    own: &Member,
    now: Timestamp,
) -> Result<(StartingSet, Approver), AuthorityRefusal> {
    let (body, approver) = verify_signature(signed, trusted, now)?;
    if !body.members.contains(own) {
        return Err(AuthorityRefusal::Mismatch {
            field: "this Machine",
        });
    }
    super::validity::verify(body.issued_at, body.expires_at, now)?;
    Ok((body, approver))
}
