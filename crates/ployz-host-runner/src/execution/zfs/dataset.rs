//! Provisioned Volume dataset effects and facts.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes};
use ployz_core::machine::{
    DatasetQuotaFact, PoolCapacityAdmissionFailure, PoolCapacityFacts, admit_pool_quota_total,
};
use ployz_core::storage::{
    DATASET_DESTROY_INNER_COMMAND_TIMEOUT, PROVISIONED_VOLUME_MOUNTPOINT, PreparedStorageOrigin,
    PreparedStorageState, StorageEffectFailure as ZfsEffectError,
};

use super::command::{COMMAND_TIMEOUT, EffectClass, checked, parse_u64};
use super::state::{load_and_verify, load_and_verify_with_timeout, verify_child};
use crate::execution::HostRunnerCommandRunner;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetFacts {
    pub used_bytes: u64,
    /// Modification time of the dataset mount directory itself. This is
    /// directory metadata testimony, not a recursive signal of file writes.
    pub mount_directory_modified_unix_seconds: u64,
}

const DATASET_ENSURE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const DATASET_ENSURE_LOCK_RETRY: Duration = Duration::from_millis(25);
const COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 4 * 1024;

pub fn ensure_dataset(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
    dataset: &DatasetName,
    quota: VolumeMaxSizeBytes,
) -> Result<(), ZfsEffectError> {
    let state = load_and_verify(runner, state_directory)?;
    verify_child(&state, dataset)?;
    let _lock = DatasetEnsureLock::acquire(state_directory, state.pool().as_str())?;
    let facts = gather_pool_capacity_for_state(runner, &state)?;
    let current = facts
        .child_quotas
        .iter()
        .find(|fact| fact.dataset == *dataset)
        .map(|fact| fact.quota_bytes);
    if current.is_some() {
        verify_existing_dataset_shape(runner, dataset)?;
    }
    if let Some(current) = current
        && quota.get() < current
    {
        return Err(ZfsEffectError::QuotaShrink {
            dataset: dataset.clone(),
            current,
            requested: quota.get(),
        });
    }
    if current == Some(quota.get()) {
        return Ok(());
    }
    admit_quota(&facts, dataset, quota.get())?;
    match current {
        None => {
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
        }
        Some(_) => {
            checked(
                runner,
                "zfs",
                &["set", &format!("quota={}", quota.get()), dataset.as_str()],
                COMMAND_TIMEOUT,
                EffectClass::Dataset,
            )?;
        }
    }
    Ok(())
}

fn verify_existing_dataset_shape(
    runner: &mut impl HostRunnerCommandRunner,
    dataset: &DatasetName,
) -> Result<(), ZfsEffectError> {
    let observed = checked(
        runner,
        "zfs",
        &[
            "get",
            "-H",
            "-o",
            "value",
            "mountpoint,mounted",
            dataset.as_str(),
        ],
        COMMAND_TIMEOUT,
        EffectClass::Mismatch,
    )?;
    let mut values = observed.stdout.lines();
    let mountpoint = values.next();
    let mounted = values.next();
    let (Some(mountpoint), Some(mounted)) = (mountpoint, mounted) else {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset {} returned invalid mountpoint/mounted testimony {:?}",
                dataset.as_str(),
                observed.stdout.trim()
            ),
        });
    };
    if values.next().is_some() {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset {} returned invalid mountpoint/mounted testimony {:?}",
                dataset.as_str(),
                observed.stdout.trim()
            ),
        });
    }
    let Some((_, leaf)) = dataset.as_str().rsplit_once('/') else {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!("dataset {} has no canonical child leaf", dataset.as_str()),
        });
    };
    let expected_mountpoint = format!("{PROVISIONED_VOLUME_MOUNTPOINT}/{leaf}");
    if mountpoint != expected_mountpoint || mounted != "yes" {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset {} has mountpoint {:?} and mounted {:?}, expected {:?} and mounted yes",
                dataset.as_str(),
                mountpoint,
                mounted,
                expected_mountpoint
            ),
        });
    }
    Ok(())
}

pub(super) struct DatasetEnsureLock {
    file: File,
}

impl DatasetEnsureLock {
    pub(super) fn acquire(state_directory: &Path, pool: &str) -> Result<Self, ZfsEffectError> {
        let path = dataset_ensure_lock_path(state_directory, pool);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| lock_error(&path, error))?;
        let deadline = Instant::now() + DATASET_ENSURE_LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(ZfsEffectError::OperationTimedOut);
                    }
                    thread::sleep(DATASET_ENSURE_LOCK_RETRY);
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(lock_error(&path, error));
                }
            }
        }
    }
}

impl Drop for DatasetEnsureLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(super) fn dataset_ensure_lock_path(state_directory: &Path, pool: &str) -> PathBuf {
    state_directory.join(format!("storage-dataset-ensure-{pool}.lock"))
}

fn lock_error(path: &Path, error: io::Error) -> ZfsEffectError {
    ZfsEffectError::Dataset {
        message: format!("failed to lock {}: {error}", path.display()),
    }
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
    let state = load_and_verify_with_timeout(
        runner,
        state_directory,
        DATASET_DESTROY_INNER_COMMAND_TIMEOUT,
    )?;
    verify_child(&state, dataset)?;
    if !direct_child_listing_contains(
        runner,
        &state,
        dataset,
        DATASET_DESTROY_INNER_COMMAND_TIMEOUT,
    )? {
        return Ok(());
    }
    checked(
        runner,
        "zfs",
        &["destroy", dataset.as_str()],
        DATASET_DESTROY_INNER_COMMAND_TIMEOUT,
        EffectClass::Destructive,
    )?;
    Ok(())
}

fn direct_child_listing_contains(
    runner: &mut impl HostRunnerCommandRunner,
    state: &PreparedStorageState,
    requested: &DatasetName,
    command_timeout: Duration,
) -> Result<bool, ZfsEffectError> {
    let root = state.dataset_root().as_str();
    let output = checked(
        runner,
        "zfs",
        &["list", "-H", "-d", "1", "-o", "name", root],
        command_timeout,
        EffectClass::Destructive,
    )?;
    if output.stdout.len() >= COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES {
        return Err(ZfsEffectError::GatherParse {
            message: "direct-child dataset listing reached the command output capture limit"
                .to_owned(),
        });
    }
    let mut saw_root = false;
    let mut children = Vec::new();
    for row in output.stdout.lines() {
        if row != row.trim() || row.is_empty() {
            return Err(ZfsEffectError::GatherParse {
                message: format!("invalid direct-child dataset row {row:?}"),
            });
        }
        if row == root {
            if saw_root {
                return Err(ZfsEffectError::GatherParse {
                    message: format!("duplicate dataset root row {row:?}"),
                });
            }
            saw_root = true;
            continue;
        }
        let child = DatasetName::try_new(row).map_err(|error| ZfsEffectError::GatherParse {
            message: format!("invalid direct-child dataset row {row:?}: {error}"),
        })?;
        verify_child(state, &child)?;
        if children.contains(&child) {
            return Err(ZfsEffectError::GatherParse {
                message: format!("duplicate direct-child dataset row {row:?}"),
            });
        }
        children.push(child);
    }
    if !saw_root {
        return Err(ZfsEffectError::GatherParse {
            message: format!("direct-child dataset listing omitted root {root}"),
        });
    }
    Ok(children.contains(requested))
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
    let (total_bytes, free_bytes) = match state.origin() {
        PreparedStorageOrigin::OwnedImage { backing_file } => {
            let path = backing_file.to_string_lossy();
            let output = checked(
                runner,
                "df",
                &["-B1", "--output=size,avail", &path],
                COMMAND_TIMEOUT,
                EffectClass::Dataset,
            )?;
            parse_capacity_row("backing filesystem capacity", &output.stdout)?
        }
        PreparedStorageOrigin::Adopted => {
            let output = checked(
                runner,
                "zpool",
                &["list", "-H", "-p", "-o", "size,free", state.pool().as_str()],
                COMMAND_TIMEOUT,
                EffectClass::Dataset,
            )?;
            parse_capacity_row("pool capacity", &output.stdout)?
        }
    };
    let provisioned_used = checked(
        runner,
        "zfs",
        &[
            "get",
            "-H",
            "-p",
            "-o",
            "value",
            "used",
            state.dataset_root().as_str(),
        ],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    let provisioned_used_bytes = parse_u64(
        "provisioned dataset-root used bytes",
        provisioned_used.stdout.trim(),
    )?;
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
        total_bytes,
        provisioned_used_bytes,
        free_bytes,
        child_quotas,
    })
}

fn parse_capacity_row(label: &str, output: &str) -> Result<(u64, u64), ZfsEffectError> {
    let Some(line) = output.lines().rev().find(|line| !line.trim().is_empty()) else {
        return Err(ZfsEffectError::GatherParse {
            message: format!("missing {label}"),
        });
    };
    let values = line.split_whitespace().collect::<Vec<_>>();
    let [total, free] = values.as_slice() else {
        return Err(ZfsEffectError::GatherParse {
            message: format!("invalid {label} row {line:?}"),
        });
    };
    Ok((
        parse_u64(&format!("{label} total bytes"), total)?,
        parse_u64(&format!("{label} free bytes"), free)?,
    ))
}

fn admit_quota(
    facts: &PoolCapacityFacts,
    dataset: &DatasetName,
    requested: u64,
) -> Result<(), ZfsEffectError> {
    let requested_total = facts
        .child_quotas
        .iter()
        .filter(|fact| fact.dataset != *dataset)
        .try_fold(requested, |total, fact| total.checked_add(fact.quota_bytes))
        .ok_or_else(|| ZfsEffectError::GatherParse {
            message: "dataset quota total overflowed".to_owned(),
        })?;
    admit_pool_quota_total(facts, requested_total).map_err(|failure| match failure {
        PoolCapacityAdmissionFailure::Exceeded {
            free_bytes,
            required_headroom_bytes,
            requested_total_bytes,
        } => ZfsEffectError::QuotaCapacityExceeded {
            total_bytes: facts.total_bytes,
            provisioned_used_bytes: facts.provisioned_used_bytes,
            free_bytes,
            required_headroom_bytes,
            requested_total_bytes,
        },
        PoolCapacityAdmissionFailure::Overflow
        | PoolCapacityAdmissionFailure::InconsistentFacts { .. } => ZfsEffectError::GatherParse {
            message: failure.to_string(),
        },
    })
}
