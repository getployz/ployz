//! Prepared storage state, pool selection, and observed-state verification.

use std::path::{Path, PathBuf};

use ployz_core::deploy::{DatasetName, ZfsPoolName};
use ployz_core::machine::{StorageCapability, StorageUnavailableReason};
use ployz_core::storage::{
    PLOYZ_OWNED_ZFS_BACKING_FILE, PLOYZ_OWNED_ZFS_POOL, PROVISIONED_VOLUME_MOUNTPOINT,
    PreparedStorageOrigin, PreparedStorageState, StorageEffectFailure as ZfsEffectError,
};

use super::command::{COMMAND_TIMEOUT, EffectClass, checked, parse_u64};
use super::dataset::gather_pool_capacity_for_state;
use super::preparation::PoolSelection;
use crate::execution::{
    FileMode, HostRunnerCommandOutput, HostRunnerCommandRunner, write_durable_file,
};

pub(super) const PREPARED_STORAGE_FILE: &str = "prepared-storage.json";
pub(super) const STORAGE_DIRECTORY: &str = "/var/lib/ployz/zfs";
const GIBIBYTE: u64 = 1024 * 1024 * 1024;
const MINIMUM_OWNED_POOL_BYTES: u64 = 8 * GIBIBYTE;
const MINIMUM_HOST_HEADROOM_BYTES: u64 = 5 * GIBIBYTE;
const OWNED_POOL_ALLOCATION_CUSHION_BYTES: u64 = 1024 * 1024;

/// Observes the capability prepared by Ployz without importing pools or
/// changing host storage. One prepared descriptor supplies the expected pool,
/// dataset root, and owned backing identity; current module, pool, dataset, and
/// backing state supply the live testimony.
pub fn observe_storage_capability(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    zfs_module_path: &Path,
) -> Result<StorageCapability, ZfsEffectError> {
    let descriptor = state_directory.join(PREPARED_STORAGE_FILE);
    if !descriptor.exists() {
        return Ok(StorageCapability::Unprepared);
    }
    let state = load_prepared_storage_state(state_directory)?;
    if !zfs_module_path.exists() {
        return Ok(StorageCapability::Unavailable {
            reason: StorageUnavailableReason::ZfsModuleMissing,
        });
    }
    let imported = imported_pools(runner)?;
    if !imported.contains(state.pool()) {
        return Ok(StorageCapability::Unavailable {
            reason: StorageUnavailableReason::PoolNotImported {
                pool: state.pool().clone(),
            },
        });
    }
    let health = checked(
        runner,
        "zpool",
        &["list", "-H", "-o", "health", state.pool().as_str()],
        COMMAND_TIMEOUT,
        EffectClass::PoolList,
    )?;
    if health.stdout.trim() != "ONLINE" {
        return Ok(StorageCapability::Unavailable {
            reason: StorageUnavailableReason::PoolFaulted {
                pool: state.pool().clone(),
            },
        });
    }
    verify_prepared_storage_state(runner, &state, COMMAND_TIMEOUT)?;
    let capacity = match gather_pool_capacity_for_state(runner, &state) {
        Ok(capacity) => capacity,
        Err(_) => {
            return Ok(StorageCapability::Unavailable {
                reason: StorageUnavailableReason::CapacityFactsUnavailable,
            });
        }
    };
    Ok(StorageCapability::Ready {
        pool: state.pool().clone(),
        capacity,
    })
}

pub(super) fn load_prepared_storage_state(
    state_directory: &Path,
) -> Result<PreparedStorageState, ZfsEffectError> {
    let path = state_directory.join(PREPARED_STORAGE_FILE);
    let bytes = std::fs::read(&path).map_err(|error| ZfsEffectError::PreparedStateUnavailable {
        message: format!("failed to read {}: {error}", path.display()),
    })?;
    let state: PreparedStorageState = serde_json::from_slice(&bytes).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!("failed to parse {}: {error}", path.display()),
        }
    })?;
    Ok(state)
}

pub(super) fn imported_pools(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<Vec<ZfsPoolName>, ZfsEffectError> {
    let output = checked(
        runner,
        "zpool",
        &["list", "-H", "-o", "name"],
        COMMAND_TIMEOUT,
        EffectClass::PoolList,
    )?;
    if output.stdout_truncated {
        return Err(ZfsEffectError::PoolList {
            message: "imported pool list output was truncated".to_owned(),
        });
    }
    let mut pools = output
        .stdout
        .lines()
        .map(str::trim)
        .filter(|pool| !pool.is_empty())
        .map(|pool| {
            ZfsPoolName::try_new(pool).map_err(|error| ZfsEffectError::PoolList {
                message: error.to_string(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    pools.sort();
    pools.dedup();
    Ok(pools)
}

pub(super) fn select_pool(
    runner: &mut impl HostRunnerCommandRunner,
    selection: &PoolSelection,
    imported: &[ZfsPoolName],
) -> Result<(ZfsPoolName, PreparedStorageOrigin), ZfsEffectError> {
    match selection {
        PoolSelection::Explicit(pool) if imported.iter().any(|name| name == pool) => {
            classify_adopted_pool(runner, pool)
        }
        PoolSelection::Explicit(pool) => {
            Err(ZfsEffectError::ExplicitPoolAbsent { pool: pool.clone() })
        }
        PoolSelection::Automatic => match imported {
            [] => prepare_owned_pool(runner),
            [pool] => classify_adopted_pool(runner, pool),
            pools => Err(ZfsEffectError::AmbiguousPools {
                candidates: pools.to_vec(),
            }),
        },
    }
}

fn classify_adopted_pool(
    runner: &mut impl HostRunnerCommandRunner,
    pool: &ZfsPoolName,
) -> Result<(ZfsPoolName, PreparedStorageOrigin), ZfsEffectError> {
    if pool.as_str() == PLOYZ_OWNED_ZFS_POOL {
        let status = checked(
            runner,
            "zpool",
            &["status", "-P", pool.as_str()],
            COMMAND_TIMEOUT,
            EffectClass::Mismatch,
        )?;
        if pool_status_uses_backing_file(&status, pool, Path::new(PLOYZ_OWNED_ZFS_BACKING_FILE))? {
            return Err(ZfsEffectError::PreparedStateUnavailable {
                message: format!(
                    "pool {PLOYZ_OWNED_ZFS_POOL} uses the Ployz backing image but has no prepared storage descriptor"
                ),
            });
        }
    }
    Ok((pool.clone(), PreparedStorageOrigin::Adopted))
}

fn prepare_owned_pool(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(ZfsPoolName, PreparedStorageOrigin), ZfsEffectError> {
    if owned_backing_file_exists(runner)? {
        return Err(ZfsEffectError::OwnedPoolEvidencePresent {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        });
    }
    checked(
        runner,
        "install",
        &["-d", "-m", "0700", STORAGE_DIRECTORY],
        COMMAND_TIMEOUT,
        EffectClass::OwnedPool,
    )?;
    let output = checked(
        runner,
        "df",
        &["-B1", "--output=size,avail", STORAGE_DIRECTORY],
        COMMAND_TIMEOUT,
        EffectClass::OwnedPool,
    )?;
    let [total_bytes, available_bytes] = parse_filesystem_capacity(&output.stdout)?;
    let required_headroom_bytes = MINIMUM_HOST_HEADROOM_BYTES.max(total_bytes / 5);
    let pool_bytes = (total_bytes / 2).min(
        available_bytes
            .saturating_sub(required_headroom_bytes)
            .saturating_sub(OWNED_POOL_ALLOCATION_CUSHION_BYTES),
    );
    if pool_bytes < MINIMUM_OWNED_POOL_BYTES {
        return Err(ZfsEffectError::OwnedPoolTooSmall {
            total_bytes,
            available_bytes,
            required_headroom_bytes,
            minimum_pool_bytes: MINIMUM_OWNED_POOL_BYTES,
        });
    }
    create_new_owned_backing_file(runner)?;
    if let Err(failure) = checked(
        runner,
        "fallocate",
        &["-l", &pool_bytes.to_string(), PLOYZ_OWNED_ZFS_BACKING_FILE],
        COMMAND_TIMEOUT,
        EffectClass::OwnedPool,
    ) {
        return Err(cleanup_new_owned_backing_file(runner, failure));
    }
    let post_allocation = checked(
        runner,
        "df",
        &["-B1", "--output=size,avail", STORAGE_DIRECTORY],
        COMMAND_TIMEOUT,
        EffectClass::OwnedPool,
    )
    .and_then(|output| parse_filesystem_capacity(&output.stdout));
    let [_, available_after_allocation] = match post_allocation {
        Ok(capacity) => capacity,
        Err(failure) => return Err(cleanup_new_owned_backing_file(runner, failure)),
    };
    if available_after_allocation < required_headroom_bytes {
        return Err(cleanup_new_owned_backing_file(
            runner,
            ZfsEffectError::OwnedPoolHeadroomNotPreserved {
                available_bytes: available_after_allocation,
                required_headroom_bytes,
            },
        ));
    }
    if let Err(failure) = checked(
        runner,
        "zpool",
        &[
            "create",
            "-f",
            PLOYZ_OWNED_ZFS_POOL,
            PLOYZ_OWNED_ZFS_BACKING_FILE,
        ],
        COMMAND_TIMEOUT,
        EffectClass::OwnedPool,
    ) {
        return Err(cleanup_after_failed_owned_pool_create(runner, failure));
    }
    Ok((
        ZfsPoolName::try_new(PLOYZ_OWNED_ZFS_POOL).expect("owned pool constant is valid"),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        },
    ))
}

enum OwnedBackingFileUse {
    CanonicalPool,
    Unused,
}

enum FailedOwnedPoolCreateDisposition {
    Cleanup,
    RetainOwned,
    RetainInconclusive(ZfsEffectError),
}

fn pool_status_uses_backing_file(
    output: &HostRunnerCommandOutput,
    pool: &ZfsPoolName,
    backing_file: &Path,
) -> Result<bool, ZfsEffectError> {
    if output.stdout_truncated {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!("pool status output for {} was truncated", pool.as_str()),
        });
    }
    let mut lines = output.stdout.lines();
    if lines
        .find(|line| {
            line.split_whitespace().collect::<Vec<_>>()
                == ["NAME", "STATE", "READ", "WRITE", "CKSUM"]
        })
        .is_none()
    {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "pool status output for {} has no canonical config header",
                pool.as_str()
            ),
        });
    }
    let Some(root_row) = lines.find(|line| !line.trim().is_empty()) else {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "pool status output for {} has no canonical pool row",
                pool.as_str()
            ),
        });
    };
    let root_columns = root_row.split_whitespace().collect::<Vec<_>>();
    if !matches!(root_columns.as_slice(), [name, _, _, _, _, ..] if *name == pool.as_str()) {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "pool status output for {} has an invalid canonical pool row",
                pool.as_str()
            ),
        });
    }
    let backing_file = backing_file.to_string_lossy();
    let mut vdev_rows = 0;
    let mut uses_backing_file = false;
    for row in lines.take_while(|line| !line.trim().is_empty()) {
        let columns = row.split_whitespace().collect::<Vec<_>>();
        let [path, _, read, write, checksum, ..] = columns.as_slice() else {
            return Err(ZfsEffectError::PreparedStateMismatch {
                message: format!(
                    "pool status output for {} has an invalid vdev row",
                    pool.as_str()
                ),
            });
        };
        if [read, write, checksum]
            .into_iter()
            .any(|value| value.parse::<u64>().is_err())
        {
            return Err(ZfsEffectError::PreparedStateMismatch {
                message: format!(
                    "pool status output for {} has an invalid vdev row",
                    pool.as_str()
                ),
            });
        }
        vdev_rows += 1;
        uses_backing_file |= *path == backing_file;
    }
    if vdev_rows == 0 {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "pool status output for {} has no canonical vdev rows",
                pool.as_str()
            ),
        });
    }
    Ok(uses_backing_file)
}

fn observe_owned_backing_file_use(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<OwnedBackingFileUse, ZfsEffectError> {
    let pool = ZfsPoolName::try_new(PLOYZ_OWNED_ZFS_POOL).expect("owned pool constant is valid");
    if !imported_pools(runner)?.contains(&pool) {
        return Ok(OwnedBackingFileUse::Unused);
    }
    let status = checked(
        runner,
        "zpool",
        &["status", "-P", PLOYZ_OWNED_ZFS_POOL],
        COMMAND_TIMEOUT,
        EffectClass::OwnedPool,
    )?;
    if pool_status_uses_backing_file(&status, &pool, Path::new(PLOYZ_OWNED_ZFS_BACKING_FILE))? {
        return Ok(OwnedBackingFileUse::CanonicalPool);
    }
    Ok(OwnedBackingFileUse::Unused)
}

fn cleanup_after_failed_owned_pool_create(
    runner: &mut impl HostRunnerCommandRunner,
    failure: ZfsEffectError,
) -> ZfsEffectError {
    let disposition = match observe_owned_backing_file_use(runner) {
        Ok(OwnedBackingFileUse::Unused) => FailedOwnedPoolCreateDisposition::Cleanup,
        Ok(OwnedBackingFileUse::CanonicalPool) => FailedOwnedPoolCreateDisposition::RetainOwned,
        Err(error) => FailedOwnedPoolCreateDisposition::RetainInconclusive(error),
    };
    match disposition {
        FailedOwnedPoolCreateDisposition::Cleanup => {
            cleanup_new_owned_backing_file(runner, failure)
        }
        FailedOwnedPoolCreateDisposition::RetainOwned => ZfsEffectError::OwnedPool {
            message: format!(
                "{failure}; backing file retained because pool {PLOYZ_OWNED_ZFS_POOL} reports it as a vdev"
            ),
        },
        FailedOwnedPoolCreateDisposition::RetainInconclusive(error) => ZfsEffectError::OwnedPool {
            message: format!(
                "{failure}; backing file retained because ownership observation was inconclusive: {error}"
            ),
        },
    }
}

fn create_new_owned_backing_file(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(), ZfsEffectError> {
    let output_file = format!("of={PLOYZ_OWNED_ZFS_BACKING_FILE}");
    let output = runner
        .command_with_timeout(
            "dd",
            &["if=/dev/null", &output_file, "status=none", "conv=excl"],
            COMMAND_TIMEOUT,
        )
        .map_err(|error| ZfsEffectError::OwnedPool {
            message: error.to_string(),
        })?;
    if output.success {
        return Ok(());
    }
    if owned_backing_file_exists(runner)? {
        return Err(ZfsEffectError::OwnedPoolEvidencePresent {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        });
    }
    Err(ZfsEffectError::OwnedPool {
        message: output.failure,
    })
}

fn owned_backing_file_exists(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<bool, ZfsEffectError> {
    let output = runner
        .command_with_timeout(
            "test",
            &["-e", PLOYZ_OWNED_ZFS_BACKING_FILE],
            COMMAND_TIMEOUT,
        )
        .map_err(|error| ZfsEffectError::OwnedPool {
            message: error.to_string(),
        })?;
    match (output.success, output.exit_code) {
        (true, _) => Ok(true),
        (false, Some(1)) => Ok(false),
        (false, _) => Err(ZfsEffectError::OwnedPool {
            message: output.failure,
        }),
    }
}

fn cleanup_new_owned_backing_file(
    runner: &mut impl HostRunnerCommandRunner,
    failure: ZfsEffectError,
) -> ZfsEffectError {
    match checked(
        runner,
        "rm",
        &["-f", "--", PLOYZ_OWNED_ZFS_BACKING_FILE],
        COMMAND_TIMEOUT,
        EffectClass::OwnedPool,
    ) {
        Ok(_) => failure,
        Err(cleanup_failure) => ZfsEffectError::OwnedPool {
            message: format!("{failure}; backing-file cleanup failed: {cleanup_failure}"),
        },
    }
}

fn parse_filesystem_capacity(output: &str) -> Result<[u64; 2], ZfsEffectError> {
    let Some(line) = output.lines().rev().find(|line| !line.trim().is_empty()) else {
        return Err(ZfsEffectError::GatherParse {
            message: "backing filesystem capacity output is empty".to_owned(),
        });
    };
    let values = line.split_whitespace().collect::<Vec<_>>();
    let [total, available] = values.as_slice() else {
        return Err(ZfsEffectError::GatherParse {
            message: format!("invalid backing filesystem capacity row {line:?}"),
        });
    };
    Ok([
        parse_u64("backing filesystem total bytes", total)?,
        parse_u64("backing filesystem available bytes", available)?,
    ])
}

pub(super) fn persist_prepared_storage_state(
    state_directory: &Path,
    state: &PreparedStorageState,
) -> Result<(), ZfsEffectError> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!("failed to serialize prepared storage state: {error}"),
        }
    })?;
    write_durable_file(
        state_directory,
        PREPARED_STORAGE_FILE,
        FileMode::Secret0600,
        &bytes,
    )
    .map_err(|error| ZfsEffectError::PreparedStateUnavailable {
        message: error.to_string(),
    })
}

pub(super) fn load_and_verify(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
) -> Result<PreparedStorageState, ZfsEffectError> {
    load_and_verify_with_timeout(runner, state_directory, COMMAND_TIMEOUT)
}

pub(super) fn load_and_verify_with_timeout(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    command_timeout: std::time::Duration,
) -> Result<PreparedStorageState, ZfsEffectError> {
    let state = load_prepared_storage_state(state_directory)?;
    verify_prepared_storage_state(runner, &state, command_timeout)?;
    Ok(state)
}

fn verify_prepared_storage_state(
    runner: &mut impl HostRunnerCommandRunner,
    state: &PreparedStorageState,
    command_timeout: std::time::Duration,
) -> Result<(), ZfsEffectError> {
    checked(
        runner,
        "zpool",
        &["list", "-H", "-o", "name", state.pool().as_str()],
        command_timeout,
        EffectClass::Mismatch,
    )?;
    let observed = checked(
        runner,
        "zfs",
        &[
            "get",
            "-H",
            "-o",
            "value",
            "mountpoint",
            state.dataset_root().as_str(),
        ],
        command_timeout,
        EffectClass::Mismatch,
    )?;
    if observed.stdout.trim() != PROVISIONED_VOLUME_MOUNTPOINT {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset root {} has mountpoint {:?}, expected {PROVISIONED_VOLUME_MOUNTPOINT}",
                state.dataset_root().as_str(),
                observed.stdout.trim()
            ),
        });
    }
    if let PreparedStorageOrigin::OwnedImage { backing_file } = state.origin() {
        let observed = checked(
            runner,
            "zpool",
            &["status", "-P", state.pool().as_str()],
            command_timeout,
            EffectClass::Mismatch,
        )?;
        if !pool_status_uses_backing_file(&observed, state.pool(), backing_file)? {
            return Err(ZfsEffectError::PreparedStateMismatch {
                message: format!(
                    "owned pool {} does not report backing file {}",
                    state.pool().as_str(),
                    backing_file.display()
                ),
            });
        }
    }
    Ok(())
}

pub(super) fn verify_child(
    state: &PreparedStorageState,
    dataset: &DatasetName,
) -> Result<(), ZfsEffectError> {
    if !state.dataset_root().contains(dataset) {
        return Err(ZfsEffectError::DestructiveEffect {
            message: format!(
                "dataset {} is not a direct child of {}",
                dataset.as_str(),
                state.dataset_root().as_str()
            ),
        });
    }
    Ok(())
}
