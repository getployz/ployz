//! Operator-owned allocation history, never an authoritative Cluster view.
use crate::{Machine, MachineSubnet, RegisterRequest};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// One Entry Machine’s observation used for operator-scoped allocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnrollmentSnapshot {
    /// IPv4 pool from the observed Cluster configuration.
    #[ts(type = "string")]
    pub network: Ipv4Net,
    /// Observed Machines; absence does not prove an assignment is free.
    pub machines: Vec<Machine>,
    /// Entry Machine store versions to carry into publication and Join.
    pub target_versions: BTreeMap<String, i64>,
}

/// Durable operator claim retained across failed publication and Join attempts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnrollmentAssignment {
    /// Retry identity inputs, excluding assigned subnet and runtime observations.
    pub request: RegisterRequest,
    /// IPv4 pool from the observed Cluster configuration.
    #[ts(type = "string")]
    pub network: Ipv4Net,
    /// Durable Machine identity and selected subnet to publish.
    pub machine: Machine,
}

/// Reasons an observed or saved allocation cannot be used safely.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EnrollmentError {
    /// The joining Machine has no advertised endpoint.
    #[error("enrollment requires the joining Machine's durable ID and endpoints")]
    InvalidIdentity,
    /// Retry inputs, identity claims, or subnet claims disagree.
    #[error("enrollment identity or allocation conflicts with a saved or observed assignment")]
    Conflict,
    /// The pool is smaller than one Machine Subnet.
    #[error("cluster IPv4 pool must contain /24 subnets")]
    InvalidNetwork,
    /// Every Machine Subnet is occupied by an observed or saved claim.
    #[error("cluster IPv4 pool has no free /24 in this observation and allocation history")]
    Exhausted,
}

/// Select a /24 using observed and pending claims.
///
/// # Errors
/// Rejects a pool smaller than /24 or an exhausted pool.
pub fn allocate_machine_subnet(
    network: Ipv4Net,
    claimed: impl IntoIterator<Item = MachineSubnet>,
) -> Result<MachineSubnet, EnrollmentError> {
    let claimed = claimed.into_iter().collect::<Vec<_>>();
    network
        .subnets(24)
        .map_err(|_| EnrollmentError::InvalidNetwork)?
        .map(|net| MachineSubnet::try_from(net).expect("/24 is a Machine Subnet"))
        .find(|candidate| !claimed.contains(candidate))
        .ok_or(EnrollmentError::Exhausted)
}

/// Allocate or resume against one observation and the operator's saved history.
/// The caller must serialize read, allocation and durable save, then release its
/// lock before publishing. Independent stores can still select overlapping subnets.
///
/// # Errors
/// Rejects changed inputs, conflicting identity/subnet claims, or exhaustion.
pub fn allocate_enrollment(
    request: &RegisterRequest,
    snapshot: &EnrollmentSnapshot,
    saved: &[EnrollmentAssignment],
) -> Result<EnrollmentAssignment, EnrollmentError> {
    let id = request.machine_id;
    if request.advertised_endpoints.is_empty() {
        return Err(EnrollmentError::InvalidIdentity);
    }
    if snapshot.network.prefix_len() > 24 {
        return Err(EnrollmentError::InvalidNetwork);
    }
    if snapshot
        .machines
        .iter()
        .chain(saved.iter().map(|a| &a.machine))
        .any(|m| m.id != id && m.public_key == request.public_key)
    {
        return Err(EnrollmentError::Conflict);
    }
    let mut identity = request.clone();
    identity.assigned_subnet = None;
    identity.runtime = Default::default();
    let existing = saved.iter().find(|a| a.machine.id == id);
    if saved.iter().any(|a| a.network != snapshot.network) {
        return Err(EnrollmentError::Conflict);
    }
    if saved
        .iter()
        .any(|a| a.machine.id == id && a.request != identity)
    {
        return Err(EnrollmentError::Conflict);
    }
    let observed = snapshot.machines.iter().find(|m| m.id == id);
    let subnet = existing
        .map(|a| a.machine.subnet)
        .or_else(|| observed.map(|m| m.subnet))
        .or(request.assigned_subnet)
        .map(Ok)
        .unwrap_or_else(|| {
            allocate_machine_subnet(
                snapshot.network,
                snapshot
                    .machines
                    .iter()
                    .map(|m| m.subnet)
                    .chain(saved.iter().map(|a| a.machine.subnet)),
            )
        })?;
    if !snapshot.network.contains(&ipnet::Ipv4Net::from(subnet))
        || request.assigned_subnet.is_some_and(|s| s != subnet)
    {
        return Err(EnrollmentError::Conflict);
    }
    let machine = Machine {
        id,
        name: request.name.clone(),
        subnet,
        public_key: request.public_key,
        public_ip: request.public_ip,
        advertised_endpoints: request.advertised_endpoints.clone(),
        runtime: request.runtime.clone(),
        labels: request.initial_policy.labels.clone(),
        accepts_builds: request.initial_policy.accepts_builds,
        accepts_services: request.initial_policy.accepts_services,
        accepts_ingress: request.initial_policy.accepts_ingress,
    };
    for other in snapshot
        .machines
        .iter()
        .chain(saved.iter().map(|a| &a.machine))
    {
        if other.id == id {
            let mut comparable = other.clone();
            comparable.runtime.clone_from(&machine.runtime);
            if comparable != machine {
                return Err(EnrollmentError::Conflict);
            }
        } else if other.public_key == machine.public_key || other.subnet == subnet {
            return Err(EnrollmentError::Conflict);
        }
    }
    if let Some(existing) = existing {
        return Ok(existing.clone());
    }
    Ok(EnrollmentAssignment {
        request: identity,
        network: snapshot.network,
        machine,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdvertisedEndpoint, MachineId, MachineName, WireGuardPublicKey};

    fn request(seed: u8) -> RegisterRequest {
        RegisterRequest {
            machine_id: MachineId::parse(format!("{seed:032x}")).unwrap(),
            assigned_subnet: None,
            initial_policy: Default::default(),
            name: MachineName::parse("same-name").unwrap(),
            storage: crate::StorageChoice::None,
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            runtime: Default::default(),
        }
    }

    #[test]
    fn enrollment_reuses_saved_assignment_and_accounts_for_pending_and_observed_claims() {
        let mut snapshot = EnrollmentSnapshot {
            network: "10.210.0.0/22".parse().unwrap(),
            machines: vec![],
            target_versions: BTreeMap::new(),
        };
        let first = allocate_enrollment(&request(1), &snapshot, &[]).unwrap();
        assert_eq!(first.machine.id, request(1).machine_id);
        let second =
            allocate_enrollment(&request(2), &snapshot, std::slice::from_ref(&first)).unwrap();
        assert_ne!(first.machine.subnet, second.machine.subnet);
        snapshot.machines.push(second.machine.clone());
        let third =
            allocate_enrollment(&request(3), &snapshot, std::slice::from_ref(&first)).unwrap();
        assert_eq!(third.machine.subnet, "10.210.2.0/24".parse().unwrap());
        assert_eq!(
            allocate_enrollment(&request(1), &snapshot, std::slice::from_ref(&first)).unwrap(),
            first
        );
        snapshot.machines.push(first.machine.clone());
        snapshot.machines.last_mut().unwrap().runtime.daemon_version = "new-runtime".into();
        assert_eq!(
            allocate_enrollment(&request(1), &snapshot, &[]).unwrap(),
            first
        );
    }

    #[test]
    fn enrollment_rejects_identity_input_network_and_publication_conflicts() {
        let snapshot = EnrollmentSnapshot {
            network: "10.210.0.0/16".parse().unwrap(),
            machines: vec![],
            target_versions: BTreeMap::new(),
        };
        let first = allocate_enrollment(&request(1), &snapshot, &[]).unwrap();
        for change in 0..5 {
            let mut changed = request(1);
            match change {
                0 => changed.public_key = WireGuardPublicKey([9; 32]),
                1 => changed.name = MachineName::parse("other").unwrap(),
                2 => changed.storage = crate::StorageChoice::Zfs,
                3 => changed.initial_policy.accepts_builds = false,
                _ => changed.assigned_subnet = Some("10.210.9.0/24".parse().unwrap()),
            }
            assert_eq!(
                allocate_enrollment(&changed, &snapshot, std::slice::from_ref(&first)),
                Err(EnrollmentError::Conflict)
            );
        }
        let mut other_network = snapshot.clone();
        other_network.network = "10.211.0.0/16".parse().unwrap();
        assert_eq!(
            allocate_enrollment(&request(1), &other_network, std::slice::from_ref(&first)),
            Err(EnrollmentError::Conflict)
        );
        let mut claimed = snapshot;
        let mut collision = first.machine.clone();
        collision.id = request(2).machine_id;
        collision.public_key = request(2).public_key;
        claimed.machines.push(collision);
        assert_eq!(
            allocate_enrollment(&request(1), &claimed, &[first]),
            Err(EnrollmentError::Conflict)
        );
    }

    #[test]
    fn enrollment_reports_exhaustion_and_requires_durable_identity_and_endpoints() {
        let snapshot = EnrollmentSnapshot {
            network: "10.210.0.0/24".parse().unwrap(),
            machines: vec![],
            target_versions: BTreeMap::new(),
        };
        let first = allocate_enrollment(&request(1), &snapshot, &[]).unwrap();
        assert_eq!(
            allocate_enrollment(&request(2), &snapshot, &[first]),
            Err(EnrollmentError::Exhausted)
        );
        let mut wire = serde_json::to_value(request(1)).unwrap();
        wire.as_object_mut().unwrap().remove("machine_id");
        assert!(serde_json::from_value::<RegisterRequest>(wire).is_err());
        let mut invalid = request(1);
        invalid.advertised_endpoints.clear();
        assert_eq!(
            allocate_enrollment(&invalid, &snapshot, &[]),
            Err(EnrollmentError::InvalidIdentity)
        );
    }
}
