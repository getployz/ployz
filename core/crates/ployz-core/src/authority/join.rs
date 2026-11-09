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
pub fn code(machine: MachineId, key: WireGuardPublicKey) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ployz-join-v1\n");
    digest.update(machine.as_str().as_bytes());
    digest.update(key.0);
    let digest = digest.finalize();
    let bits = u128::from_be_bytes(
        *digest
            .as_slice()
            .first_chunk()
            .expect("SHA-256 has 16 bytes"),
    );
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    (0..20)
        .map(|index| {
            let digit = usize::try_from((bits >> (128 - 5 * (index + 1))) & 31).expect("five bits");
            char::from(*alphabet.get(digit).expect("five bits index 32 digits"))
        })
        .collect()
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
    pub bootstrap: Vec<JoinAllocation>,
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
    pub fn code(&self) -> String {
        code(self.machine.id, self.machine.public_key)
    }
}

/// The joining Machine's proof that it opened its approved key capsule: a MAC under the
/// Cluster's Sealing Key over the signed approval. Only a holder of that key can make one, so
/// an admitting Machine publishes the joiner's row only after seeing it. A joiner Cloud cut off
/// from its capsule never becomes a peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, TS)]
#[ts(type = "string")]
pub struct JoinConfirmation(pub [u8; 32]);

impl Serialize for JoinConfirmation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{self}"))
    }
}

impl<'de> Deserialize<'de> for JoinConfirmation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        super::canonical::hex_bytes(&value)
            .map(Self)
            .ok_or_else(|| {
                serde::de::Error::custom("a join confirmation must be 64 lowercase hex digits")
            })
    }
}

impl std::fmt::Display for JoinConfirmation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        super::canonical::write_hex(&self.0, formatter)
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn join_code_exposes_one_hundred_bits_and_binds_both_identity_and_key() {
        let machine = MachineId::random();
        let key = WireGuardPublicKey([1; 32]);
        let code = code(machine, key);
        assert_eq!(code.len(), 20);
        assert!(
            code.bytes()
                .all(|byte| b"abcdefghijklmnopqrstuvwxyz234567".contains(&byte))
        );
        assert_ne!(code, super::code(MachineId::random(), key));
        assert_ne!(code, super::code(machine, WireGuardPublicKey([2; 32])));
    }

    #[test]
    fn a_join_confirmation_travels_as_lowercase_hex() {
        let confirmation = JoinConfirmation([0xab; 32]);
        let wire = serde_json::to_string(&confirmation).unwrap();
        assert_eq!(wire, format!("\"{}\"", "ab".repeat(32)));
        assert_eq!(
            serde_json::from_str::<JoinConfirmation>(&wire).unwrap(),
            confirmation
        );
        assert!(
            serde_json::from_str::<JoinConfirmation>(&format!("\"{}\"", "AB".repeat(32))).is_err()
        );
        assert!(serde_json::from_str::<JoinConfirmation>("\"abab\"").is_err());
    }
}
