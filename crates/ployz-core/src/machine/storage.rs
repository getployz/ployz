//! Machine-local storage capability and capacity testimony.

use serde::{Deserialize, Serialize};

use crate::deploy::{DatasetName, ZfsPoolName};

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
