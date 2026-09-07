//! Plan-wide provisioning budgets and capacity-aware volume placement.

use crate::deploy::{DeploySnapshot, PlanError};
use ployz_core::{
    DockerVolumeName, MachineObservation, ProvisionedVolumeMaximumBytes, RawVolumeSource,
    ServiceVolume, StorageBudget, StorageCapacity, StorageCapacityError,
};
use std::collections::BTreeMap;

type Volumes = BTreeMap<DockerVolumeName, ProvisionedVolumeMaximumBytes>;

pub(super) fn bounds<'volume>(volumes: impl Iterator<Item = &'volume ServiceVolume>) -> Volumes {
    volumes
        .filter_map(|volume| match volume.source.kind() {
            RawVolumeSource::Provisioned {
                name,
                maximum_bytes,
                ..
            } => Some((name.clone(), *maximum_bytes)),
            RawVolumeSource::Bind { .. }
            | RawVolumeSource::External { .. }
            | RawVolumeSource::Ordinary { .. }
            | RawVolumeSource::Tmpfs { .. } => None,
        })
        .collect()
}

/// Dataset inventory must be known before absence can authorize a new placement.
///
/// # Errors
/// Returns a Machine-specific unknown-capacity error for failed or omitted observations.
pub(super) fn capacity<'snapshot>(
    snapshot: &'snapshot DeploySnapshot,
    machine: &MachineObservation,
) -> Result<&'snapshot StorageCapacity, PlanError> {
    match snapshot.storage_capacity.get(&machine.machine.id) {
        Some(Ok(capacity)) => Ok(capacity),
        failure => Err(PlanError::Storage {
            machine_id: machine.machine.id,
            machine: machine.machine.name.clone(),
            source: StorageCapacityError::StorageCapacityUnknown {
                message: match failure {
                    Some(Err(error)) => error.message.clone(),
                    _ => "the Machine did not return fresh storage capacity".into(),
                },
            },
        }),
    }
}

pub(super) fn budget(
    snapshot: &DeploySnapshot,
    machine: &MachineObservation,
    volumes: &Volumes,
) -> Result<StorageBudget, PlanError> {
    capacity(snapshot, machine)?
        .budget(volumes)
        .map_err(|source| PlanError::Storage {
            machine_id: machine.machine.id,
            machine: machine.machine.name.clone(),
            source,
        })
}
