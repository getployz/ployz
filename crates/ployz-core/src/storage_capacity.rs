//! Shared provisioning arithmetic for observed plans and machine-local admission.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{DockerVolumeName, ProvisionedVolumeMaximumBytes, RpcError, RpcErrorCode};

/// One GiB in bytes.
pub const STORAGE_GIB: u64 = 1024 * 1024 * 1024;

/// The physical backing from which a Machine can provision storage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StorageBacking {
    /// No imported Pool or recoverable backing file exists.
    Unallocated {
        /// Total size of the host filesystem in bytes.
        host_total_bytes: u64,
        /// Bytes currently available to allocation on the host filesystem.
        host_available_bytes: u64,
    },
    /// Ployz can grow its preallocated backing file on the host root.
    RootBacked {
        /// Observed usable Pool size in bytes.
        pool_size_bytes: u64,
        /// Logical backing file length in bytes.
        backing_length_bytes: u64,
        /// Physical bytes allocated to the backing file.
        backing_allocated_bytes: u64,
        /// Total size of the host filesystem in bytes.
        host_total_bytes: u64,
        /// Bytes currently available to allocation on the host filesystem.
        host_available_bytes: u64,
    },
    /// An operator-managed Pool cannot be automatically expanded.
    Fixed {
        /// Observed usable Pool size in bytes.
        pool_size_bytes: u64,
    },
}

/// Fresh storage evidence, including commitments whose Docker metadata may be missing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StorageCapacity {
    /// Observed physical backing and host filesystem capacity.
    pub backing: StorageBacking,
    /// Allocated Pool bytes outside the managed datasets, including snapshots and metadata.
    pub unmanaged_used_bytes: u64,
    /// Every managed dataset commitment, including Volumes without Docker metadata.
    pub volumes: BTreeMap<DockerVolumeName, ProvisionedVolumeMaximumBytes>,
}

/// Capacity required by the complete set of requested Volumes on one Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct StorageBudget {
    /// Sum of unique bounds requested by this deployment, including reused Volumes.
    pub requested_bytes: u64,
    /// Bounds not already committed by existing managed datasets.
    pub additional_commitment_bytes: u64,
    /// Conservative backing-growth estimate; initial Pools include one GiB for ZFS size loss.
    /// Allocation rechecks actual usable capacity, which cannot be known before Pool creation.
    pub required_growth_bytes: u64,
    /// Free host filesystem bytes, or remaining usable capacity for a fixed Pool.
    pub available_bytes: u64,
    /// Host filesystem bytes retained for the OS; zero for a fixed Pool.
    pub reserve_bytes: u64,
}

/// Storage admission failure shared by the planner, daemon, and SDK.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum StorageCapacityError {
    #[error("Not enough disk space: about {} more free space is needed, including storage overhead and OS reserve. Free disk space, expand the disk, or reduce requested volume sizes.", readable_shortfall(*required_growth_bytes, *available_bytes, *reserve_bytes))]
    /// The complete request would consume the protected host reserve.
    InsufficientStorage {
        /// Additional physical backing required in bytes.
        required_growth_bytes: u64,
        /// Observed available bytes before preserving the reserve.
        available_bytes: u64,
        /// Host bytes retained for the operating system.
        reserve_bytes: u64,
    },
    #[error(
        "The Machine Pool needs {required_bytes} bytes but has {capacity_bytes} bytes and cannot grow automatically. Expand the Pool or reduce requested volume sizes."
    )]
    /// A fixed Pool cannot hold the complete request.
    PoolCannotGrow {
        /// Total Pool capacity required including overhead and occupancy.
        required_bytes: u64,
        /// Observed usable capacity of the fixed Pool.
        capacity_bytes: u64,
    },
    #[error(
        "Storage capacity is unknown: {message}. Refresh storage observations before deploying."
    )]
    /// Capacity cannot be established from the available evidence.
    StorageCapacityUnknown {
        /// The missing or invalid evidence.
        message: String,
    },
    #[error("Volume {name} already has a different size; it will not be resized or replaced")]
    /// An existing Volume has a different immutable bound.
    VolumeSizeConflict {
        /// The conflicting Volume.
        name: DockerVolumeName,
    },
}

impl StorageCapacityError {
    /// A readable RPC failure with stable, machine-readable capacity details.
    #[must_use]
    pub fn into_rpc_error(self) -> RpcError {
        let mut details = serde_json::to_value(&self).expect("storage error is JSON serializable");
        if let Self::InsufficientStorage {
            required_growth_bytes,
            available_bytes,
            reserve_bytes,
        } = self
        {
            let fields = details
                .as_object_mut()
                .expect("storage errors serialize as objects");
            fields.insert(
                "shortfall_bytes".into(),
                serde_json::json!(
                    (required_growth_bytes as u128 + reserve_bytes as u128)
                        .saturating_sub(available_bytes as u128)
                        .min(u64::MAX as u128) as u64
                ),
            );
            fields.insert(
                "suggestions".into(),
                serde_json::json!([
                    "Free disk space",
                    "Expand the disk",
                    "Reduce requested volume sizes"
                ]),
            );
        }
        RpcError {
            code: if matches!(self, Self::StorageCapacityUnknown { .. }) {
                RpcErrorCode::Unavailable
            } else {
                RpcErrorCode::Conflict
            },
            message: self.to_string(),
            details,
        }
    }
}

fn readable_shortfall(growth: u64, available: u64, reserve: u64) -> String {
    let bytes = (growth as u128 + reserve as u128).saturating_sub(available as u128);
    for (unit, label) in [(STORAGE_GIB, "GiB"), (1024 * 1024, "MiB"), (1024, "KiB")] {
        if bytes >= unit as u128 {
            return format!(
                "{:.2} {label}",
                (bytes * 100).div_ceil(unit as u128) as f64 / 100.0
            );
        }
    }
    format!("{bytes} B")
}

fn unknown(message: &str) -> StorageCapacityError {
    StorageCapacityError::StorageCapacityUnknown {
        message: message.into(),
    }
}

/// Pool headroom applied consistently when planning, creating, and growing.
///
/// # Errors
/// Returns unknown capacity if arithmetic overflows.
pub fn storage_with_headroom(commitment: u64) -> Result<u64, StorageCapacityError> {
    commitment
        .checked_add((commitment / 10).max(STORAGE_GIB))
        .ok_or_else(|| unknown("commitments overflow u64"))
}

/// Root space preserved for the operating system.
#[must_use]
pub fn storage_host_reserve(total: u64) -> u64 {
    (total / 4).max(10 * STORAGE_GIB)
}

/// Rounded backing growth, preserving the difference between backing length and ZFS size.
///
/// # Errors
/// Returns unknown capacity for invalid observations or overflow.
pub fn storage_growth_target(
    length: u64,
    observed: u64,
    minimum: u64,
) -> Result<u64, StorageCapacityError> {
    if length < observed {
        return Err(unknown("backing length is below the observed Pool size"));
    }
    minimum
        .checked_sub(observed)
        .and_then(|extension| extension.checked_next_multiple_of(STORAGE_GIB))
        .and_then(|extension| length.checked_add(extension))
        .ok_or_else(|| unknown("Pool growth overflow or inconsistent observation"))
}

impl StorageCapacity {
    /// Evaluate all requested Volumes together; existing names and shared mounts count once.
    ///
    /// # Errors
    /// Returns a typed shortage, conflicting bound, or unknown capacity.
    pub fn budget(
        &self,
        requested: &BTreeMap<DockerVolumeName, ProvisionedVolumeMaximumBytes>,
    ) -> Result<StorageBudget, StorageCapacityError> {
        let mut additional = 0u64;
        for (name, maximum) in requested {
            match self.volumes.get(name) {
                Some(existing) if existing != maximum => {
                    return Err(StorageCapacityError::VolumeSizeConflict { name: name.clone() });
                }
                Some(_) => {}
                None => {
                    additional = additional
                        .checked_add(maximum.get())
                        .ok_or_else(|| unknown("requested bounds overflow u64"))?
                }
            }
        }
        let commitment = self
            .volumes
            .values()
            .map(|maximum| maximum.get())
            .try_fold(additional, u64::checked_add)
            .ok_or_else(|| unknown("existing commitments overflow u64"))?;
        let requested_bytes = requested
            .values()
            .map(|maximum| maximum.get())
            .try_fold(0u64, u64::checked_add)
            .ok_or_else(|| unknown("requested bounds overflow u64"))?;
        let minimum = storage_with_headroom(commitment)?
            .checked_add(self.unmanaged_used_bytes)
            .ok_or_else(|| unknown("Pool occupancy overflows u64"))?;
        let (growth, available, reserve) = match self.backing {
            StorageBacking::Unallocated {
                host_total_bytes,
                host_available_bytes,
            } => {
                validate_host(host_total_bytes, host_available_bytes)?;
                if !self.volumes.is_empty() || self.unmanaged_used_bytes != 0 {
                    return Err(unknown(
                        "an unallocated Pool cannot have existing commitments or occupancy",
                    ));
                }
                (
                    if additional == 0 {
                        0
                    } else {
                        minimum
                            .checked_add(STORAGE_GIB)
                            .ok_or_else(|| unknown("initial Pool estimate overflows u64"))?
                    },
                    host_available_bytes,
                    storage_host_reserve(host_total_bytes),
                )
            }
            StorageBacking::RootBacked {
                pool_size_bytes,
                backing_length_bytes,
                backing_allocated_bytes,
                host_total_bytes,
                host_available_bytes,
            } => {
                validate_host(host_total_bytes, host_available_bytes)?;
                if pool_size_bytes == 0
                    || self.unmanaged_used_bytes > pool_size_bytes
                    || backing_length_bytes < pool_size_bytes
                    || backing_allocated_bytes < backing_length_bytes
                {
                    return Err(unknown(
                        "Machine Pool backing is not fully allocated or has inconsistent size",
                    ));
                }
                let target = if minimum <= pool_size_bytes {
                    backing_allocated_bytes
                } else {
                    storage_growth_target(backing_length_bytes, pool_size_bytes, minimum)?
                };
                (
                    target.saturating_sub(backing_allocated_bytes),
                    host_available_bytes,
                    storage_host_reserve(host_total_bytes),
                )
            }
            StorageBacking::Fixed { pool_size_bytes } => {
                if pool_size_bytes == 0 || self.unmanaged_used_bytes > pool_size_bytes {
                    return Err(unknown(
                        "fixed Pool occupancy is inconsistent with its size",
                    ));
                }
                if minimum > pool_size_bytes {
                    return Err(StorageCapacityError::PoolCannotGrow {
                        required_bytes: minimum,
                        capacity_bytes: pool_size_bytes,
                    });
                }
                (0, pool_size_bytes.saturating_sub(minimum), 0)
            }
        };
        if growth > 0 && growth as u128 + reserve as u128 > available as u128 {
            return Err(StorageCapacityError::InsufficientStorage {
                required_growth_bytes: growth,
                available_bytes: available,
                reserve_bytes: reserve,
            });
        }
        Ok(StorageBudget {
            requested_bytes,
            additional_commitment_bytes: additional,
            required_growth_bytes: growth,
            available_bytes: available,
            reserve_bytes: reserve,
        })
    }
}

fn validate_host(total: u64, available: u64) -> Result<(), StorageCapacityError> {
    if total == 0 || available > total {
        return Err(unknown("invalid host filesystem capacity"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_overflow_and_inconsistent_observations_hold_capacity_unknown() {
        assert!(storage_with_headroom(u64::MAX).is_err());
        assert!(storage_growth_target(u64::MAX, 1, 2).is_err());
        assert!(storage_growth_target(1, 2, 3).is_err());
        assert!(storage_growth_target(3, 2, 1).is_err());
    }

    #[test]
    fn occupied_pools_and_unusable_backing_bytes_do_not_count_as_capacity() {
        let name = DockerVolumeName::parse("data").unwrap();
        let maximum = |gib| {
            ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(gib * STORAGE_GIB).unwrap(),
            )
        };
        let mut capacity = StorageCapacity {
            backing: StorageBacking::Fixed {
                pool_size_bytes: 100 * STORAGE_GIB,
            },
            unmanaged_used_bytes: 95 * STORAGE_GIB,
            volumes: BTreeMap::new(),
        };
        assert!(matches!(
            capacity.budget(&BTreeMap::from([(name.clone(), maximum(10))])),
            Err(StorageCapacityError::PoolCannotGrow { .. })
        ));
        capacity.backing = StorageBacking::RootBacked {
            pool_size_bytes: 15 * STORAGE_GIB / 4,
            backing_length_bytes: 4 * STORAGE_GIB,
            backing_allocated_bytes: 4 * STORAGE_GIB,
            host_total_bytes: 80 * STORAGE_GIB,
            host_available_bytes: 20 * STORAGE_GIB,
        };
        capacity.unmanaged_used_bytes = 0;
        let error = capacity
            .budget(&BTreeMap::from([(name, maximum(3))]))
            .unwrap_err()
            .into_rpc_error();
        assert_eq!(error.details.get("shortfall_bytes").unwrap(), STORAGE_GIB);
    }

    #[test]
    fn reused_volumes_recheck_space_consumed_by_snapshots_and_other_data() {
        let volumes = BTreeMap::from([(
            DockerVolumeName::parse("data").unwrap(),
            ProvisionedVolumeMaximumBytes::new(
                std::num::NonZeroU64::new(90 * STORAGE_GIB).unwrap(),
            ),
        )]);
        let mut capacity = StorageCapacity {
            backing: StorageBacking::Fixed {
                pool_size_bytes: 100 * STORAGE_GIB,
            },
            unmanaged_used_bytes: 20 * STORAGE_GIB,
            volumes: volumes.clone(),
        };
        assert!(matches!(
            capacity.budget(&volumes),
            Err(StorageCapacityError::PoolCannotGrow { .. })
        ));
        capacity.backing = StorageBacking::RootBacked {
            pool_size_bytes: 100 * STORAGE_GIB,
            backing_length_bytes: 101 * STORAGE_GIB,
            backing_allocated_bytes: 101 * STORAGE_GIB,
            host_total_bytes: 200 * STORAGE_GIB,
            host_available_bytes: 60 * STORAGE_GIB,
        };
        assert!(matches!(
            capacity.budget(&volumes),
            Err(StorageCapacityError::InsufficientStorage { .. })
        ));
        capacity.unmanaged_used_bytes = 0;
        assert_eq!(capacity.budget(&volumes).unwrap().required_growth_bytes, 0);
    }

    #[test]
    fn shortage_messages_never_round_a_positive_shortfall_to_zero() {
        for (bytes, expected) in [
            (1, "1 B"),
            (1024, "1.00 KiB"),
            (1_100_000, "1.05 MiB"),
            (43_744_232_448, "40.74 GiB"),
        ] {
            let error = StorageCapacityError::InsufficientStorage {
                required_growth_bytes: bytes,
                available_bytes: 0,
                reserve_bytes: 0,
            };
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn complete_deploy_shortage_is_structured_and_existing_volumes_are_reused() {
        let volume = |name: &str, gib| {
            (
                DockerVolumeName::parse(name).unwrap(),
                ProvisionedVolumeMaximumBytes::new(
                    std::num::NonZeroU64::new(gib * STORAGE_GIB).unwrap(),
                ),
            )
        };
        let mut capacity = StorageCapacity {
            backing: StorageBacking::Unallocated {
                host_total_bytes: 80 * STORAGE_GIB,
                host_available_bytes: 60 * STORAGE_GIB,
            },
            unmanaged_used_bytes: 0,
            volumes: BTreeMap::new(),
        };
        let requested = BTreeMap::from([
            volume("postgres", 10),
            volume("redis", 4),
            volume("data", 30),
            volume("server", 8),
        ]);
        let error = capacity.budget(&requested).unwrap_err().into_rpc_error();
        assert_eq!(error.details.get("code").unwrap(), "insufficient_storage");
        assert_eq!(
            error.details.get("shortfall_bytes").unwrap(),
            19_542_101_196u64
        );
        assert!(error.message.contains("18.20 GiB"));
        capacity.backing = StorageBacking::Fixed {
            pool_size_bytes: 60 * STORAGE_GIB,
        };
        capacity.volumes = requested.clone();
        assert_eq!(
            capacity
                .budget(&requested)
                .unwrap()
                .additional_commitment_bytes,
            0
        );
    }
}
