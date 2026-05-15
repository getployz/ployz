use ployz_api::{
    MachineStorageAuthorityPeer, MachineStoragePromoteRequest, MachineStoragePromotionFailure,
    MachineStoragePromotionFailureCause,
};
use ployz_model::{
    AuthorityId, MachineLifecycle, MachineMembership, MachineStorageRole, StorageParticipation,
};
use std::collections::BTreeSet;

use super::promotion::StoragePromotionError;

pub(super) struct StoragePromotionPlan {
    pub(super) targets: Vec<MachineMembership>,
    pub(super) previous_authority_peers: Vec<MachineStorageAuthorityPeer>,
    pub(super) authority_peers: Vec<MachineMembership>,
    pub(super) authority_peer_payloads: Vec<MachineStorageAuthorityPeer>,
}

impl StoragePromotionPlan {
    pub(super) fn build(
        request: &MachineStoragePromoteRequest,
        machines: &[MachineMembership],
        local_record: &MachineMembership,
    ) -> Result<Self, StoragePromotionError> {
        let authorities = authority_members(machines, local_record)?;
        let targets = promotion_targets(request, machines)?;
        validate_final_replica_count(request, authorities.as_slice(), targets.as_slice())?;

        let previous_authority_peers = authorities
            .iter()
            .map(MachineStorageAuthorityPeer::from)
            .collect::<Vec<_>>();
        let mut authority_peers = authorities;
        authority_peers.extend(targets.clone().into_iter().map(|mut target| {
            target.storage_role = MachineStorageRole::default_authority();
            target
        }));
        let authority_peer_payloads = authority_peers
            .iter()
            .map(MachineStorageAuthorityPeer::from)
            .collect::<Vec<_>>();

        Ok(Self {
            targets,
            previous_authority_peers,
            authority_peers,
            authority_peer_payloads,
        })
    }
}

fn authority_members(
    machines: &[MachineMembership],
    local_record: &MachineMembership,
) -> Result<Vec<MachineMembership>, StoragePromotionError> {
    let default_authority = AuthorityId::default_authority();
    let mut authorities = machines
        .iter()
        .filter(|machine| {
            machine.lifecycle == MachineLifecycle::Active
                && machine.storage()
                && matches!(
                    &machine.storage_participation(),
                    StorageParticipation::Authority { authority_id }
                        if authority_id == &default_authority
                )
        })
        .cloned()
        .collect::<Vec<_>>();
    if authorities
        .iter()
        .any(|machine| machine.id == local_record.id)
    {
        return Ok(authorities);
    }

    if local_record.lifecycle != MachineLifecycle::Active
        || !local_record.storage()
        || !matches!(
            &local_record.storage_participation(),
            StorageParticipation::Authority { authority_id } if authority_id == &default_authority
        )
    {
        return Err(StoragePromotionError::InvalidLocalAuthority {
            message: format!(
                "local authority must be active storage for {} (lifecycle={}, storage={}, participation={:?})",
                default_authority,
                local_record.lifecycle,
                local_record.storage(),
                local_record.storage_participation()
            ),
        });
    }
    authorities.push(local_record.clone());
    Ok(authorities)
}

fn promotion_targets(
    request: &MachineStoragePromoteRequest,
    machines: &[MachineMembership],
) -> Result<Vec<MachineMembership>, StoragePromotionError> {
    let mut failed = duplicate_target_failures(&request.targets);
    if !failed.is_empty() {
        return Err(StoragePromotionError::Preflight { failed });
    }

    let mut targets = Vec::new();
    for target in &request.targets {
        match machines.iter().find(|machine| machine.id.as_str() == *target) {
            Some(machine)
                if machine.lifecycle == MachineLifecycle::Active
                    && machine.storage()
                    && machine.storage_participation() == StorageParticipation::Candidate =>
            {
                targets.push(machine.clone());
            }
            Some(machine) => failed.push(MachineStoragePromotionFailure {
                machine_id: target.clone(),
                cause: MachineStoragePromotionFailureCause::InvalidCandidate,
                message: format!(
                    "machine must be an active storage candidate (lifecycle={}, storage={}, participation={:?})",
                    machine.lifecycle,
                    machine.storage(),
                    machine.storage_participation()
                ),
            }),
            None => failed.push(MachineStoragePromotionFailure {
                machine_id: target.clone(),
                cause: MachineStoragePromotionFailureCause::MachineNotFound,
                message: "machine not found".into(),
            }),
        }
    }
    if failed.is_empty() {
        Ok(targets)
    } else {
        Err(StoragePromotionError::Preflight { failed })
    }
}

fn duplicate_target_failures(targets: &[String]) -> Vec<MachineStoragePromotionFailure> {
    let mut failed = Vec::new();
    let mut seen_targets = BTreeSet::new();
    for target in targets {
        if !seen_targets.insert(target.clone()) {
            failed.push(MachineStoragePromotionFailure {
                machine_id: target.clone(),
                cause: MachineStoragePromotionFailureCause::DuplicateTarget,
                message: "duplicate promotion target".into(),
            });
        }
    }
    failed
}

fn validate_final_replica_count(
    request: &MachineStoragePromoteRequest,
    authorities: &[MachineMembership],
    targets: &[MachineMembership],
) -> Result<(), StoragePromotionError> {
    let final_authority_ids = authorities
        .iter()
        .chain(targets.iter())
        .map(|machine| machine.id.clone())
        .collect::<BTreeSet<_>>();
    let final_authority_count = final_authority_ids.len();
    if final_authority_count == request.replicas.replicas() {
        return Ok(());
    }

    Err(StoragePromotionError::ReplicaCount {
        message: format!(
            "storage promotion to {} replicas requires exactly {} active authority participants after promotion; current authorities={}, targets={}",
            request.replicas,
            request.replicas.replicas(),
            authorities.len(),
            targets.len()
        ),
    })
}
