//! Provisioned Volume dataset effects and facts.

use std::path::Path;

use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes};
use ployz_core::machine::{DatasetQuotaFact, PoolCapacityFacts};
use ployz_core::storage::{
    PROVISIONED_VOLUME_MOUNTPOINT, PreparedStorageOrigin, PreparedStorageState,
    StorageEffectFailure as ZfsEffectError,
};

use super::command::{COMMAND_TIMEOUT, EffectClass, checked, parse_last_u64, parse_u64};
use super::state::{load_and_verify, verify_child};
use crate::execution::HostRunnerCommandRunner;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetFacts {
    pub used_bytes: u64,
    /// Modification time of the dataset mount directory itself. This is
    /// directory metadata testimony, not a recursive signal of file writes.
    pub mount_directory_modified_unix_seconds: u64,
}

pub fn create_dataset(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    dataset: &DatasetName,
    quota: VolumeMaxSizeBytes,
) -> Result<(), ZfsEffectError> {
    let state = load_and_verify(runner, state_directory)?;
    verify_child(&state, dataset)?;
    admit_quota(runner, &state, dataset, quota.get())?;
    checked(
        runner,
        "zfs",
        &[
            "create",
            "-o",
            &format!("quota={}", quota.get()),
            dataset.as_str(),
        ],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    Ok(())
}

pub fn grow_dataset_quota(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    dataset: &DatasetName,
    requested: VolumeMaxSizeBytes,
) -> Result<(), ZfsEffectError> {
    let state = load_and_verify(runner, state_directory)?;
    verify_child(&state, dataset)?;
    let output = checked(
        runner,
        "zfs",
        &["get", "-H", "-p", "-o", "value", "quota", dataset.as_str()],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    let current = parse_u64("dataset quota", output.stdout.trim())?;
    if requested.get() < current {
        return Err(ZfsEffectError::QuotaShrink {
            dataset: dataset.clone(),
            current,
            requested: requested.get(),
        });
    }
    if requested.get() > current {
        admit_quota(runner, &state, dataset, requested.get())?;
        checked(
            runner,
            "zfs",
            &[
                "set",
                &format!("quota={}", requested.get()),
                dataset.as_str(),
            ],
            COMMAND_TIMEOUT,
            EffectClass::Dataset,
        )?;
    }
    Ok(())
}

pub fn gather_dataset_facts(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    dataset: &DatasetName,
) -> Result<DatasetFacts, ZfsEffectError> {
    let state = load_and_verify(runner, state_directory)?;
    verify_child(&state, dataset)?;
    let used = checked(
        runner,
        "zfs",
        &["get", "-H", "-p", "-o", "value", "used", dataset.as_str()],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    let leaf = dataset
        .as_str()
        .rsplit_once('/')
        .map(|(_, leaf)| leaf)
        .ok_or_else(|| ZfsEffectError::GatherParse {
            message: format!("dataset {} has no leaf", dataset.as_str()),
        })?;
    let path = format!("{PROVISIONED_VOLUME_MOUNTPOINT}/{leaf}");
    let directory_modified = checked(
        runner,
        "stat",
        &["-c", "%Y", &path],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    Ok(DatasetFacts {
        used_bytes: parse_u64("dataset used bytes", used.stdout.trim())?,
        mount_directory_modified_unix_seconds: parse_u64(
            "dataset mount-directory modification timestamp",
            directory_modified.stdout.trim(),
        )?,
    })
}

pub fn destroy_dataset(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    dataset: &DatasetName,
) -> Result<(), ZfsEffectError> {
    let state = load_and_verify(runner, state_directory)?;
    verify_child(&state, dataset)?;
    checked(
        runner,
        "zfs",
        &["destroy", dataset.as_str()],
        COMMAND_TIMEOUT,
        EffectClass::Destructive,
    )?;
    Ok(())
}

pub fn gather_pool_capacity(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
) -> Result<PoolCapacityFacts, ZfsEffectError> {
    let state = load_and_verify(runner, state_directory)?;
    gather_pool_capacity_for_state(runner, &state)
}

pub(super) fn gather_pool_capacity_for_state(
    runner: &mut impl HostRunnerCommandRunner,
    state: &PreparedStorageState,
) -> Result<PoolCapacityFacts, ZfsEffectError> {
    let available = match state.origin() {
        PreparedStorageOrigin::OwnedImage { backing_file } => {
            let path = backing_file.to_string_lossy();
            let output = checked(
                runner,
                "df",
                &["-B1", "--output=avail", &path],
                COMMAND_TIMEOUT,
                EffectClass::Dataset,
            )?;
            parse_last_u64("backing filesystem available bytes", &output.stdout)?
        }
        PreparedStorageOrigin::Adopted => {
            let output = checked(
                runner,
                "zpool",
                &["list", "-H", "-p", "-o", "free", state.pool().as_str()],
                COMMAND_TIMEOUT,
                EffectClass::Dataset,
            )?;
            parse_u64("pool available bytes", output.stdout.trim())?
        }
    };
    let output = checked(
        runner,
        "zfs",
        &[
            "list",
            "-H",
            "-p",
            "-d",
            "1",
            "-o",
            "name,quota",
            state.dataset_root().as_str(),
        ],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    let mut child_quotas = Vec::new();
    for line in output
        .stdout
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
    {
        let Some((name, quota)) = line.split_once('\t').or_else(|| line.split_once(' ')) else {
            return Err(ZfsEffectError::GatherParse {
                message: format!("invalid child quota row {line:?}"),
            });
        };
        let dataset =
            DatasetName::try_new(name.trim()).map_err(|error| ZfsEffectError::GatherParse {
                message: error.to_string(),
            })?;
        verify_child(state, &dataset)?;
        child_quotas.push(DatasetQuotaFact {
            dataset,
            quota_bytes: parse_u64("child quota", quota.trim())?,
        });
    }
    child_quotas.sort_by(|left, right| left.dataset.cmp(&right.dataset));
    Ok(PoolCapacityFacts {
        available_bytes: available,
        child_quotas,
    })
}

fn admit_quota(
    runner: &mut impl HostRunnerCommandRunner,
    state: &PreparedStorageState,
    dataset: &DatasetName,
    requested: u64,
) -> Result<(), ZfsEffectError> {
    let facts = gather_pool_capacity_for_state(runner, state)?;
    let requested_total = facts
        .child_quotas
        .iter()
        .filter(|fact| fact.dataset != *dataset)
        .try_fold(requested, |total, fact| total.checked_add(fact.quota_bytes))
        .ok_or(ZfsEffectError::QuotaCapacityExceeded {
            available: facts.available_bytes,
            requested_total: u64::MAX,
        })?;
    if requested_total > facts.available_bytes {
        return Err(ZfsEffectError::QuotaCapacityExceeded {
            available: facts.available_bytes,
            requested_total,
        });
    }
    Ok(())
}
