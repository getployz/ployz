//! Provisioned Volume dataset effects and facts.

use std::path::Path;

use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes};
use ployz_core::storage::{PreparedStorageOrigin, StorageEffectFailure as ZfsEffectError};
use serde::{Deserialize, Serialize};

use super::command::{COMMAND_TIMEOUT, EffectClass, checked, parse_last_u64, parse_u64};
use super::state::{VOLUME_MOUNTPOINT, load_and_verify, verify_child};
use crate::execution::HostRunnerCommandRunner;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetQuotaFact {
    pub dataset: DatasetName,
    pub quota_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolCapacityFacts {
    pub available_bytes: u64,
    pub child_quotas: Vec<DatasetQuotaFact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetFacts {
    pub used_bytes: u64,
    pub last_write_unix_seconds: u64,
}

pub fn create_dataset(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    dataset: &DatasetName,
    quota: VolumeMaxSizeBytes,
) -> Result<(), ZfsEffectError> {
    let state = load_and_verify(runner, state_directory)?;
    verify_child(&state, dataset)?;
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
    let path = format!("{VOLUME_MOUNTPOINT}/{leaf}");
    let last_write = checked(
        runner,
        "stat",
        &["-c", "%Y", &path],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    Ok(DatasetFacts {
        used_bytes: parse_u64("dataset used bytes", used.stdout.trim())?,
        last_write_unix_seconds: parse_u64(
            "dataset last-write timestamp",
            last_write.stdout.trim(),
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
        verify_child(&state, &dataset)?;
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
