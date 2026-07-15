//! Prepared storage state, pool selection, and observed-state verification.

use std::path::{Path, PathBuf};

use ployz_core::deploy::{DatasetName, ZfsPoolName};
use ployz_core::storage::{
    PLOYZ_OWNED_ZFS_BACKING_FILE, PLOYZ_OWNED_ZFS_POOL, PreparedStorageOrigin,
    PreparedStorageState, StorageEffectFailure as ZfsEffectError,
};

use super::command::{COMMAND_TIMEOUT, EffectClass, checked, parse_last_u64};
use super::preparation::PoolSelection;
use crate::execution::{FileMode, HostRunnerCommandRunner, write_durable_file};

pub(super) const PREPARED_STORAGE_FILE: &str = "prepared-storage.json";
pub(super) const STORAGE_DIRECTORY: &str = "/var/lib/ployz/zfs";
pub(super) const VOLUME_MOUNTPOINT: &str = "/var/lib/ployz/volumes";

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
        if status
            .stdout
            .lines()
            .any(|line| line.trim() == PLOYZ_OWNED_ZFS_BACKING_FILE)
        {
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
    checked(
        runner,
        "install",
        &["-d", "-m", "0700", STORAGE_DIRECTORY],
        COMMAND_TIMEOUT,
        EffectClass::Sparse,
    )?;
    let output = checked(
        runner,
        "df",
        &["-B1", "--output=size", STORAGE_DIRECTORY],
        COMMAND_TIMEOUT,
        EffectClass::Sparse,
    )?;
    let logical_size = parse_last_u64("backing filesystem total bytes", &output.stdout)?;
    checked(
        runner,
        "truncate",
        &[
            "-s",
            &logical_size.to_string(),
            PLOYZ_OWNED_ZFS_BACKING_FILE,
        ],
        COMMAND_TIMEOUT,
        EffectClass::Sparse,
    )?;
    checked(
        runner,
        "zpool",
        &[
            "create",
            "-f",
            PLOYZ_OWNED_ZFS_POOL,
            PLOYZ_OWNED_ZFS_BACKING_FILE,
        ],
        COMMAND_TIMEOUT,
        EffectClass::Sparse,
    )?;
    Ok((
        ZfsPoolName::try_new(PLOYZ_OWNED_ZFS_POOL).expect("owned pool constant is valid"),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        },
    ))
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
    let state = load_prepared_storage_state(state_directory)?;
    checked(
        runner,
        "zpool",
        &["list", "-H", "-o", "name", state.pool().as_str()],
        COMMAND_TIMEOUT,
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
        COMMAND_TIMEOUT,
        EffectClass::Mismatch,
    )?;
    if observed.stdout.trim() != VOLUME_MOUNTPOINT {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset root {} has mountpoint {:?}, expected {VOLUME_MOUNTPOINT}",
                state.dataset_root().as_str(),
                observed.stdout.trim()
            ),
        });
    }
    if let PreparedStorageOrigin::OwnedImage { backing_file } = state.origin() {
        let path = backing_file.to_string_lossy();
        let observed = checked(
            runner,
            "zpool",
            &["status", "-P", state.pool().as_str()],
            COMMAND_TIMEOUT,
            EffectClass::Mismatch,
        )?;
        if !observed.stdout.lines().any(|line| line.trim() == path) {
            return Err(ZfsEffectError::PreparedStateMismatch {
                message: format!(
                    "owned pool {} does not report backing file {}",
                    state.pool().as_str(),
                    backing_file.display()
                ),
            });
        }
    }
    Ok(state)
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
