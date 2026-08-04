//! Machine storage capability testimony and derived stranded-volume evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::deploy::{DatasetName, VolumeName, ZfsPoolName};
use crate::ids::{MachineId, NamespaceId};
use crate::intent::{VolumeKind, VolumePinState};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageCapability {
    Unprepared,
    Ready {
        pool: ZfsPoolName,
        capacity: PoolCapacityFacts,
    },
    Unavailable {
        reason: StorageUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DatasetQuotaFact {
    pub dataset: DatasetName,
    pub quota_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PoolCapacityFacts {
    pub total_bytes: u64,
    /// Physical bytes consumed beneath the Ployz provisioned dataset root.
    /// Unrelated pool or backing-filesystem allocations are excluded.
    pub provisioned_used_bytes: u64,
    pub free_bytes: u64,
    pub child_quotas: Vec<DatasetQuotaFact>,
}

/// Fresh machine-owned usage testimony for one mounted volume.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VolumeUsageFacts {
    pub used_bytes: u64,
    /// Latest modification time among the volume root and all entries on the
    /// same filesystem, in whole Unix seconds.
    pub last_write_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolCapacityAdmissionFailure {
    #[error(
        "pool capacity facts are inconsistent: total={total_bytes} provisioned_used={provisioned_used_bytes} free={free_bytes}"
    )]
    InconsistentFacts {
        total_bytes: u64,
        provisioned_used_bytes: u64,
        free_bytes: u64,
    },
    #[error("pool capacity arithmetic overflowed")]
    Overflow,
    #[error(
        "quota admission exceeds pool capacity: free={free_bytes} required_headroom={required_headroom_bytes} requested_total={requested_total_bytes}"
    )]
    Exceeded {
        free_bytes: u64,
        required_headroom_bytes: u64,
        requested_total_bytes: u64,
    },
}

/// Enforces a 1.0 quota-to-capacity ratio using the pool's current physical
/// consumption. Bytes already consumed by quota-bearing datasets are part of
/// `provisioned_used_bytes`; only the unconsumed quota headroom must fit in
/// `free_bytes`. Allocations elsewhere in the pool reduce `free_bytes` but
/// never masquerade as consumption of a Ployz quota.
pub fn admit_pool_quota_total(
    facts: &PoolCapacityFacts,
    requested_total_bytes: u64,
) -> Result<(), PoolCapacityAdmissionFailure> {
    let accounted = facts
        .provisioned_used_bytes
        .checked_add(facts.free_bytes)
        .ok_or(PoolCapacityAdmissionFailure::Overflow)?;
    if accounted > facts.total_bytes {
        return Err(PoolCapacityAdmissionFailure::InconsistentFacts {
            total_bytes: facts.total_bytes,
            provisioned_used_bytes: facts.provisioned_used_bytes,
            free_bytes: facts.free_bytes,
        });
    }
    let required_headroom_bytes =
        requested_total_bytes.saturating_sub(facts.provisioned_used_bytes);
    if required_headroom_bytes > facts.free_bytes {
        return Err(PoolCapacityAdmissionFailure::Exceeded {
            free_bytes: facts.free_bytes,
            required_headroom_bytes,
            requested_total_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod capacity_tests {
    use super::*;

    fn facts(provisioned_used_bytes: u64, free_bytes: u64) -> PoolCapacityFacts {
        PoolCapacityFacts {
            total_bytes: 100,
            provisioned_used_bytes,
            free_bytes,
            child_quotas: Vec::new(),
        }
    }

    #[test]
    fn quota_headroom_counts_provisioned_consumption_once() {
        assert_eq!(admit_pool_quota_total(&facts(40, 50), 90), Ok(()));
        assert!(matches!(
            admit_pool_quota_total(&facts(40, 50), 91),
            Err(PoolCapacityAdmissionFailure::Exceeded {
                required_headroom_bytes: 51,
                ..
            })
        ));
    }

    #[test]
    fn unrelated_allocation_never_satisfies_provisioned_headroom() {
        // Ten bytes are allocated elsewhere: total 100, Ployz used 0, free
        // 90. That unrelated consumption must not make a quota of 100 fit.
        assert!(matches!(
            admit_pool_quota_total(&facts(0, 90), 100),
            Err(PoolCapacityAdmissionFailure::Exceeded {
                required_headroom_bytes: 100,
                ..
            })
        ));
    }

    #[test]
    fn volume_ensure_machine_mismatch_is_structured_on_the_wire() {
        let failure = VolumeEnsureFailure::MachineMismatch {
            expected_machine_id: MachineId::try_new("machine_expected").expect("machine id"),
            responder_machine_id: MachineId::try_new("machine_responder").expect("machine id"),
        };

        let encoded = serde_json::to_value(&failure).expect("serialize failure");
        assert_eq!(
            encoded.get("kind").and_then(serde_json::Value::as_str),
            Some("machine_mismatch")
        );
        assert_eq!(
            serde_json::from_value::<VolumeEnsureFailure>(encoded).expect("deserialize failure"),
            failure
        );
    }

    #[test]
    fn volume_shape_mismatch_retains_optional_dataset_on_the_wire() {
        let dataset = DatasetName::for_volume(
            &ZfsPoolName::try_new("tank").expect("pool"),
            &NamespaceId::try_new("default").expect("namespace"),
            &VolumeName::try_new("data").expect("volume"),
        )
        .expect("dataset");
        let failure = VolumeEnsureFailure::DockerShapeMismatch {
            volume_name: VolumeName::try_new("data").expect("volume"),
            retained_dataset: Some(dataset.clone()),
            message: "wrong driver options".to_owned(),
        };

        let encoded = serde_json::to_value(&failure).expect("serialize failure");
        assert_eq!(
            encoded
                .get("retained_dataset")
                .and_then(serde_json::Value::as_str),
            Some(dataset.as_str())
        );
        assert_eq!(
            serde_json::from_value::<VolumeEnsureFailure>(encoded).expect("deserialize failure"),
            failure
        );

        let plain = VolumeEnsureFailure::DockerShapeMismatch {
            volume_name: VolumeName::try_new("cache").expect("volume"),
            retained_dataset: None,
            message: "wrong driver".to_owned(),
        };
        let encoded = serde_json::to_value(&plain).expect("serialize failure");
        assert!(encoded.get("retained_dataset").is_none());
        assert_eq!(
            serde_json::from_value::<VolumeEnsureFailure>(encoded).expect("deserialize failure"),
            plain
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageUnavailableReason {
    ZfsModuleMissing,
    PoolNotImported { pool: ZfsPoolName },
    PoolFaulted { pool: ZfsPoolName },
    CapacityFactsUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageTestimony<'a> {
    NoAnswer,
    Answered(Option<&'a StorageCapability>),
}

/// Typed machine-local failure from ensuring one durable volume shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeEnsureFailure {
    MachineMismatch {
        expected_machine_id: MachineId,
        responder_machine_id: MachineId,
    },
    Dataset {
        dataset: DatasetName,
        failure: crate::storage::StorageEffectFailure,
    },
    DockerShapeMismatch {
        volume_name: VolumeName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_dataset: Option<DatasetName>,
        message: String,
    },
    DockerEnsureFailed {
        volume_name: VolumeName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retained_dataset: Option<DatasetName>,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageCompatibility {
    Compatible,
    MachineSilent,
    TestimonyNotReported,
    Unprepared,
    Unavailable {
        reason: StorageUnavailableReason,
    },
    PoolMismatch {
        expected: ZfsPoolName,
        reported: ZfsPoolName,
    },
}

#[must_use]
pub fn classify_storage_compatibility(
    testimony: StorageTestimony<'_>,
    expected_pool: Option<&ZfsPoolName>,
) -> StorageCompatibility {
    match testimony {
        StorageTestimony::NoAnswer => StorageCompatibility::MachineSilent,
        StorageTestimony::Answered(None) => StorageCompatibility::TestimonyNotReported,
        StorageTestimony::Answered(Some(StorageCapability::Unprepared)) => {
            StorageCompatibility::Unprepared
        }
        StorageTestimony::Answered(Some(StorageCapability::Unavailable { reason })) => {
            StorageCompatibility::Unavailable {
                reason: reason.clone(),
            }
        }
        StorageTestimony::Answered(Some(StorageCapability::Ready { pool, .. })) => {
            match expected_pool {
                Some(expected) if expected != pool => StorageCompatibility::PoolMismatch {
                    expected: expected.clone(),
                    reported: pool.clone(),
                },
                Some(_) | None => StorageCompatibility::Compatible,
            }
        }
    }
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
            let testimony = answered_testimony
                .get(pin.machine_id())
                .map_or(StorageTestimony::NoAnswer, |storage| {
                    StorageTestimony::Answered(*storage)
                });
            let reason = match classify_storage_compatibility(testimony, Some(&expected_pool)) {
                StorageCompatibility::Compatible => return None,
                StorageCompatibility::MachineSilent => StrandedVolumeReason::MachineSilent,
                StorageCompatibility::TestimonyNotReported => {
                    StrandedVolumeReason::StorageTestimonyNotReported
                }
                StorageCompatibility::Unprepared => StrandedVolumeReason::StorageUnprepared,
                StorageCompatibility::Unavailable { reason } => {
                    StrandedVolumeReason::StorageUnavailable { reason }
                }
                StorageCompatibility::PoolMismatch { expected, reported } => {
                    StrandedVolumeReason::PoolMismatch { expected, reported }
                }
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
