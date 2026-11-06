//! Device authority for admitting one Machine and releasing the Cluster's Sealing Key.

use crate::{AdvertisedEndpoint, InitialMachinePolicy, MachineName, MachineSubnet};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use super::{AuthorityRefusal, Purpose, Signed, SigningPublicKey, Statement, Timestamp};
use crate::{DeviceKey, Machine, MachineId, SealingPublicKey, WireGuardPublicKey};

/// The immutable enrollment allocation. Runtime observations confer no authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct JoinAllocation {
    pub id: MachineId,
    pub name: MachineName,
    pub subnet: MachineSubnet,
    pub public_key: WireGuardPublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string")]
    pub public_ip: Option<std::net::IpAddr>,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    pub policy: InitialMachinePolicy,
}

impl From<&Machine> for JoinAllocation {
    fn from(machine: &Machine) -> Self {
        Self {
            id: machine.id,
            name: machine.name.clone(),
            subnet: machine.subnet,
            public_key: machine.public_key,
            public_ip: machine.public_ip,
            advertised_endpoints: machine.advertised_endpoints.clone(),
            policy: InitialMachinePolicy {
                labels: machine.labels.clone(),
                accepts_builds: machine.accepts_builds,
                accepts_services: machine.accepts_services,
                accepts_ingress: machine.accepts_ingress,
            },
        }
    }
}

impl JoinAllocation {
    /// Whether this allocation preserves the identity and policy the CLI requested.
    #[must_use]
    pub fn matches_request(&self, request: &crate::RegisterRequest) -> bool {
        self.id == request.machine_id
            && self.name == request.name
            && request
                .assigned_subnet
                .is_none_or(|subnet| self.subnet == subnet)
            && self.public_key == request.public_key
            && self.public_ip == request.public_ip
            && self.advertised_endpoints == request.advertised_endpoints
            && self.policy == request.initial_policy
    }
}

/// The code identifies both the Machine's durable ID and its locally generated key.
#[must_use]
pub fn code(machine: MachineId, key: WireGuardPublicKey) -> crate::DeviceCode {
    let mut digest = Sha256::new();
    digest.update(b"ployz-join-v1\n");
    digest.update(machine.as_str().as_bytes());
    digest.update(key.0);
    DeviceKey::from_bytes(digest.finalize().into()).code()
}

/// One Device's approval of an allocation, its key recipient and initial trusted Devices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct JoinApproval {
    pub cluster: SealingPublicKey,
    pub machine: JoinAllocation,
    #[ts(type = "string")]
    pub network: Ipv4Net,
    /// Currently the WireGuard key also receives the Sealing Key.
    pub sealing_recipient: WireGuardPublicKey,
    pub devices: Vec<DeviceKey>,
    /// Machine IDs scope the approval even when two Clusters share a Sealing key.
    pub admitting_machines: Vec<MachineId>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub signer: DeviceKey,
}

impl Statement for JoinApproval {
    const PURPOSE: Purpose = Purpose::Join;
}

impl JoinApproval {
    /// The code the joining Server prints for a person to compare before approval.
    #[must_use]
    pub fn code(&self) -> crate::DeviceCode {
        code(self.machine.id, self.machine.public_key)
    }
}

/// Verify a join at an admitting Machine using only its own trusted keys and identity.
///
/// # Errors
/// Refuses untrusted signatures, expiry and any allocation, recipient or Cluster mismatch.
pub fn verify(
    signed: &Signed<JoinApproval>,
    cluster: SealingPublicKey,
    admitting: MachineId,
    devices: &[DeviceKey],
    machine: &JoinAllocation,
    network: Ipv4Net,
    now: Timestamp,
) -> Result<JoinApproval, AuthorityRefusal> {
    let body = verify_signature(signed, devices, now)?;
    for (field, matches) in [
        ("a Cluster", body.cluster == cluster),
        (
            "approved Devices",
            body.devices.contains(&body.signer)
                && body.devices.iter().all(|key| devices.contains(key)),
        ),
        (
            "an admitting Machine",
            body.admitting_machines.contains(&admitting),
        ),
        (
            "a Machine allocation",
            &body.machine == machine && body.network == network,
        ),
        (
            "a sealing recipient",
            body.sealing_recipient == machine.public_key,
        ),
    ] {
        if !matches {
            return Err(AuthorityRefusal::Mismatch { field });
        }
    }
    Ok(body)
}

/// Verify the signature and expiry against pinned Devices.
///
/// # Errors
/// Refuses a signer not among `devices`, malformed statements and expired approvals.
pub fn verify_signature(
    signed: &Signed<JoinApproval>,
    devices: &[DeviceKey],
    now: Timestamp,
) -> Result<JoinApproval, AuthorityRefusal> {
    let signers = signed.verified_signers(devices.iter().copied().map(SigningPublicKey::from))?;
    let body = signed.open()?;
    if !signers.contains(&body.signer.into()) {
        return Err(AuthorityRefusal::Mismatch {
            field: "the signer",
        });
    }
    if body.admitting_machines.len() != 1 {
        return Err(AuthorityRefusal::Mismatch {
            field: "one admitting Machine",
        });
    }
    super::validity::verify(body.issued_at, body.expires_at, now)?;
    Ok(body)
}
