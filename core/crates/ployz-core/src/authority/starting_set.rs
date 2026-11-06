//! Device authority for the Machines an enforcing Machine trusts without a join approval.

use serde::{Deserialize, Serialize};

use super::{AuthorityRefusal, Purpose, Signed, SigningPublicKey, Statement, Timestamp};
use crate::{DeviceKey, Machine, MachineId, MachineSubnet, OperationId, WireGuardPublicKey};

/// The parts of a Machine row membership compares. Endpoints and policy may change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

/// One Device's approval of the Members every Machine trusts when it turns enforcement on.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartingSet {
    pub operation: OperationId,
    pub members: Vec<Member>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub signer: DeviceKey,
}

impl Statement for StartingSet {
    const PURPOSE: Purpose = Purpose::Enforce;
}

/// Verify a starting set against this Machine's own Devices and allocation.
///
/// # Errors
/// Refuses an untrusted or mismatched signer, an expired set, and a set that leaves out
/// this Machine, which scopes the set to one Cluster.
pub fn verify(
    signed: &Signed<StartingSet>,
    devices: &[DeviceKey],
    own: &Member,
    now: Timestamp,
) -> Result<StartingSet, AuthorityRefusal> {
    let signers = signed.verified_signers(devices.iter().copied().map(SigningPublicKey::from))?;
    let body = signed.open()?;
    if !signers.contains(&body.signer.into()) {
        return Err(AuthorityRefusal::Mismatch {
            field: "the signer",
        });
    }
    if !body.members.contains(own) {
        return Err(AuthorityRefusal::Mismatch {
            field: "this Machine",
        });
    }
    super::validity::verify(body.issued_at, body.expires_at, now)?;
    Ok(body)
}
