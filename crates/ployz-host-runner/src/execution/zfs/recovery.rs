//! Boot-time recovery for an authorized Ployz-owned ZFS pool.

use std::path::Path;

use ployz_core::storage::{
    PreparedStorageOrigin, PreparedStorageState, StorageEffectFailure as ZfsEffectError,
};

use super::command::{COMMAND_TIMEOUT, EffectClass, checked};
use super::state::{imported_pools, load_prepared_storage_state, verify_prepared_storage_state};
use crate::execution::HostRunnerCommandRunner;

pub fn recover_owned_storage(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
) -> Result<PreparedStorageState, ZfsEffectError> {
    let state = load_prepared_storage_state(state_directory)?;
    let PreparedStorageOrigin::OwnedImage { backing_file } = state.origin() else {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "prepared pool {} is adopted and has no Ployz-owned import authority",
                state.pool().as_str()
            ),
        });
    };
    let Some(backing_file) = backing_file.to_str() else {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "owned pool {} has a non-UTF-8 backing file",
                state.pool().as_str()
            ),
        });
    };
    let imported = imported_pools(runner)?;
    if !imported.contains(state.pool()) {
        checked(
            runner,
            "zpool",
            &["import", "-d", backing_file, state.pool().as_str()],
            COMMAND_TIMEOUT,
            EffectClass::OwnedPool,
        )?;
    }
    let health = checked(
        runner,
        "zpool",
        &["list", "-H", "-o", "health", state.pool().as_str()],
        COMMAND_TIMEOUT,
        EffectClass::Mismatch,
    )?;
    if health.stdout.trim() != "ONLINE" {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "owned pool {} has health {:?}, expected ONLINE",
                state.pool().as_str(),
                health.stdout.trim()
            ),
        });
    }
    verify_prepared_storage_state(runner, &state, COMMAND_TIMEOUT)?;
    let mounted = checked(
        runner,
        "zfs",
        &[
            "get",
            "-H",
            "-o",
            "value",
            "mounted",
            state.dataset_root().as_str(),
        ],
        COMMAND_TIMEOUT,
        EffectClass::Mismatch,
    )?;
    if mounted.stdout.trim() != "yes" {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset root {} has mounted {:?}, expected yes",
                state.dataset_root().as_str(),
                mounted.stdout.trim()
            ),
        });
    }
    Ok(state)
}
