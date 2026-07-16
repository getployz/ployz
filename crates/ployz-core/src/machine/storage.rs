//! Machine storage capability testimony and derived stranded-volume evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::deploy::{VolumeName, ZfsPoolName};
use crate::ids::{MachineId, NamespaceId};
use crate::intent::{VolumeKind, VolumePinState};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageCapability {
    Unprepared,
    Ready { pool: ZfsPoolName },
    Unavailable { reason: StorageUnavailableReason },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageUnavailableReason {
    ZfsModuleMissing,
    PoolNotImported { pool: ZfsPoolName },
    PoolFaulted { pool: ZfsPoolName },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineStorageTestimony {
    machine_id: MachineId,
    storage: Option<StorageCapability>,
}

impl MachineStorageTestimony {
    #[must_use]
    pub const fn new(machine_id: MachineId, storage: Option<StorageCapability>) -> Self {
        Self {
            machine_id,
            storage,
        }
    }

    #[must_use]
    pub const fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    #[must_use]
    pub const fn storage(&self) -> Option<&StorageCapability> {
        self.storage.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct StrandedVolumeAlarm {
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
    pub machine_id: MachineId,
    pub reason: StrandedVolumeReason,
}

impl StrandedVolumeAlarm {
    #[must_use]
    pub const fn new(
        namespace_id: NamespaceId,
        volume_name: VolumeName,
        machine_id: MachineId,
        reason: StrandedVolumeReason,
    ) -> Self {
        Self {
            namespace_id,
            volume_name,
            machine_id,
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StrandedVolumeReason {
    MachineSilent,
    StorageTestimonyNotReported,
    StorageUnprepared,
    StorageUnavailable {
        reason: StorageUnavailableReason,
    },
    PoolMismatch {
        expected: ZfsPoolName,
        reported: ZfsPoolName,
    },
}

#[must_use]
pub fn derive_stranded_volume_alarms(
    pins: &[VolumePinState],
    known_machine_ids: &[MachineId],
    answered_testimony: &[MachineStorageTestimony],
) -> Vec<StrandedVolumeAlarm> {
    let known_machine_ids = known_machine_ids.iter().collect::<BTreeSet<_>>();
    let answered_testimony = answered_testimony
        .iter()
        .map(|testimony| (testimony.machine_id(), testimony.storage()))
        .collect::<BTreeMap<_, _>>();
    let mut alarms = pins
        .iter()
        .filter_map(|pin| {
            let VolumeKind::Provisioned { dataset, .. } = pin.kind() else {
                return None;
            };
            if !known_machine_ids.contains(pin.machine_id()) {
                return None;
            }

            let expected_pool = dataset.pool();
            let reason = match answered_testimony.get(pin.machine_id()) {
                None => StrandedVolumeReason::MachineSilent,
                Some(None) => StrandedVolumeReason::StorageTestimonyNotReported,
                Some(Some(StorageCapability::Unprepared)) => {
                    StrandedVolumeReason::StorageUnprepared
                }
                Some(Some(StorageCapability::Unavailable { reason })) => {
                    StrandedVolumeReason::StorageUnavailable {
                        reason: (*reason).clone(),
                    }
                }
                Some(Some(StorageCapability::Ready { pool })) if pool != &expected_pool => {
                    StrandedVolumeReason::PoolMismatch {
                        expected: expected_pool,
                        reported: (*pool).clone(),
                    }
                }
                Some(Some(StorageCapability::Ready { .. })) => return None,
            };

            Some(StrandedVolumeAlarm::new(
                pin.namespace_id().clone(),
                pin.volume_name().clone(),
                pin.machine_id().clone(),
                reason,
            ))
        })
        .collect::<Vec<_>>();
    alarms.sort();
    alarms
}
