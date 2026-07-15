//! Bounded machine-local ZFS preparation and Provisioned Volume effects.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, ZfsPoolName};
use ployz_core::ids::OperationId;
use serde::{Deserialize, Serialize};

use super::{
    FileMode, HostPlatformProfile, HostRunnerCommandOutput, HostRunnerCommandRunner,
    write_durable_file,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const PREPARED_STORAGE_FILE: &str = "prepared-storage.json";
const OWNED_POOL: &str = "ployz";
const STORAGE_DIRECTORY: &str = "/var/lib/ployz/zfs";
const OWNED_BACKING_FILE: &str = "/var/lib/ployz/zfs/ployz.img";
const VOLUME_MOUNTPOINT: &str = "/var/lib/ployz/volumes";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolSelection {
    Automatic,
    Explicit(ZfsPoolName),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparedStorageOrigin {
    OwnedImage { backing_file: PathBuf },
    Adopted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedStorageState {
    pub pool: String,
    pub origin: PreparedStorageOrigin,
    pub dataset_root: String,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ZfsEffectError {
    #[error("ZFS preparation is unsupported on this host profile")]
    UnsupportedPlatform,
    #[error("ZFS installation or module loading failed: {message}")]
    Installation { message: String },
    #[error("failed to list imported ZFS pools: {message}")]
    PoolList { message: String },
    #[error("multiple imported ZFS pools require an explicit selection: {candidates:?}")]
    AmbiguousPools { candidates: Vec<String> },
    #[error("explicit ZFS pool {pool} is not imported")]
    ExplicitPoolAbsent { pool: String },
    #[error("Ployz sparse image pool preparation failed: {message}")]
    SparsePool { message: String },
    #[error("ZFS property or dataset effect failed: {message}")]
    Dataset { message: String },
    #[error("prepared storage state is unavailable: {message}")]
    PreparedStateUnavailable { message: String },
    #[error("prepared storage state does not match observed ZFS state: {message}")]
    PreparedStateMismatch { message: String },
    #[error("ZFS fact output is invalid: {message}")]
    GatherParse { message: String },
    #[error("quota shrink is forbidden for {dataset}: current={current} requested={requested}")]
    QuotaShrink {
        dataset: String,
        current: u64,
        requested: u64,
    },
    #[error("destructive ZFS effect refused: {message}")]
    DestructiveEffect { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum StoragePreparationEvidence {
    Completed {
        operation_id: OperationId,
        prepared: PreparedStorageState,
    },
    Failed {
        operation_id: OperationId,
        failure: ZfsEffectError,
    },
}

pub fn prepare_storage_for_operation(
    runner: &mut impl HostRunnerCommandRunner,
    profile: &HostPlatformProfile,
    operation_id: &OperationId,
    selection: &PoolSelection,
    state_directory: &Path,
    docker_drop_in_directory: &Path,
) -> Result<PreparedStorageState, ZfsEffectError> {
    let evidence_directory = state_directory.join("storage-operations");
    let evidence_file = format!("{}.json", operation_id.as_str());
    let evidence_path = evidence_directory.join(&evidence_file);
    if evidence_path.exists() {
        let bytes = std::fs::read(&evidence_path).map_err(|error| {
            ZfsEffectError::PreparedStateUnavailable {
                message: format!("failed to read {}: {error}", evidence_path.display()),
            }
        })?;
        let evidence: StoragePreparationEvidence =
            serde_json::from_slice(&bytes).map_err(|error| {
                ZfsEffectError::PreparedStateUnavailable {
                    message: format!("failed to parse {}: {error}", evidence_path.display()),
                }
            })?;
        return match evidence {
            StoragePreparationEvidence::Completed {
                operation_id: recorded,
                prepared,
            } if recorded == *operation_id => Ok(prepared),
            StoragePreparationEvidence::Failed {
                operation_id: recorded,
                failure,
            } if recorded == *operation_id => Err(failure),
            StoragePreparationEvidence::Completed { .. }
            | StoragePreparationEvidence::Failed { .. } => {
                Err(ZfsEffectError::PreparedStateMismatch {
                    message: "operation-scoped preparation evidence names another operation"
                        .to_owned(),
                })
            }
        };
    }

    std::fs::create_dir_all(&evidence_directory).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!(
                "failed to create operation evidence directory {}: {error}",
                evidence_directory.display()
            ),
        }
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(&evidence_directory, std::fs::Permissions::from_mode(0o700)).map_err(
        |error| ZfsEffectError::PreparedStateUnavailable {
            message: format!(
                "failed to protect operation evidence directory {}: {error}",
                evidence_directory.display()
            ),
        },
    )?;
    let result = prepare_storage(
        runner,
        profile,
        selection,
        state_directory,
        docker_drop_in_directory,
    );
    let evidence = match &result {
        Ok(prepared) => StoragePreparationEvidence::Completed {
            operation_id: operation_id.clone(),
            prepared: prepared.clone(),
        },
        Err(failure) => StoragePreparationEvidence::Failed {
            operation_id: operation_id.clone(),
            failure: failure.clone(),
        },
    };
    let bytes = serde_json::to_vec_pretty(&evidence).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!("failed to serialize preparation evidence: {error}"),
        }
    })?;
    write_durable_file(
        &evidence_directory,
        &evidence_file,
        FileMode::Secret0600,
        &bytes,
    )
    .map_err(|error| ZfsEffectError::PreparedStateUnavailable {
        message: error.to_string(),
    })?;
    result
}

pub fn prepare_storage(
    runner: &mut impl HostRunnerCommandRunner,
    profile: &HostPlatformProfile,
    selection: &PoolSelection,
    state_directory: &Path,
    docker_drop_in_directory: &Path,
) -> Result<PreparedStorageState, ZfsEffectError> {
    install_zfs(runner, profile)?;
    let imported = imported_pools(runner)?;
    let existing_state = state_directory.join(PREPARED_STORAGE_FILE);
    let (pool, origin) = if existing_state.exists() {
        let state = load_and_verify(runner, state_directory)?;
        if !imported.iter().any(|pool| pool == &state.pool) {
            return Err(ZfsEffectError::PreparedStateMismatch {
                message: format!("prepared pool {} is not imported", state.pool),
            });
        }
        match selection {
            PoolSelection::Explicit(requested) if requested.as_str() != state.pool => {
                return Err(ZfsEffectError::PreparedStateMismatch {
                    message: format!(
                        "prepared pool {} does not equal explicit pool {}",
                        state.pool,
                        requested.as_str()
                    ),
                });
            }
            PoolSelection::Automatic | PoolSelection::Explicit(_) => {}
        }
        (state.pool, state.origin)
    } else {
        select_pool(runner, selection, &imported)?
    };
    let dataset_root = format!("{pool}/ployz/volumes");

    checked(
        runner,
        "zpool",
        &["set", "failmode=continue", &pool],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    let parent = format!("{pool}/ployz");
    ensure_dataset(runner, &parent, None)?;
    ensure_dataset(runner, &dataset_root, Some(VOLUME_MOUNTPOINT))?;
    let state = PreparedStorageState {
        pool,
        origin,
        dataset_root,
    };
    install_docker_zfs_ordering(runner, docker_drop_in_directory)?;
    persist_prepared_storage_state(state_directory, &state)?;
    Ok(state)
}

fn install_docker_zfs_ordering(
    runner: &mut impl HostRunnerCommandRunner,
    directory: &Path,
) -> Result<(), ZfsEffectError> {
    std::fs::create_dir_all(directory).map_err(|error| ZfsEffectError::Dataset {
        message: format!(
            "failed to create Docker systemd drop-in directory {}: {error}",
            directory.display()
        ),
    })?;
    write_durable_file(
        directory,
        "ployz-zfs.conf",
        FileMode::Plain,
        b"[Unit]\nAfter=zfs.target\n",
    )
    .map_err(|error| ZfsEffectError::Dataset {
        message: error.to_string(),
    })?;
    checked(
        runner,
        "systemctl",
        &["daemon-reload"],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    Ok(())
}

pub fn load_prepared_storage_state(
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
    validate_prepared_state(&state)?;
    Ok(state)
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
            dataset: dataset.as_str().to_owned(),
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
    let available = match &state.origin {
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
                &["list", "-H", "-p", "-o", "free", &state.pool],
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
            &state.dataset_root,
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

fn install_zfs(
    runner: &mut impl HostRunnerCommandRunner,
    profile: &HostPlatformProfile,
) -> Result<(), ZfsEffectError> {
    if profile.is_ubuntu() {
        checked(
            runner,
            "env",
            &["DEBIAN_FRONTEND=noninteractive", "apt-get", "update"],
            INSTALL_TIMEOUT,
            EffectClass::Install,
        )?;
        checked(
            runner,
            "env",
            &[
                "DEBIAN_FRONTEND=noninteractive",
                "apt-get",
                "install",
                "-y",
                "zfsutils-linux",
            ],
            INSTALL_TIMEOUT,
            EffectClass::Install,
        )?;
    } else if profile.is_rocky() {
        let major = profile
            .version_id()
            .and_then(|version| version.split('.').next())
            .filter(|major| !major.is_empty())
            .ok_or_else(|| ZfsEffectError::Installation {
                message: "Rocky VERSION_ID has no major release".to_owned(),
            })?;
        let release = format!("https://zfsonlinux.org/epel/zfs-release-3-0.el{major}.noarch.rpm");
        checked(
            runner,
            "dnf",
            &["install", "-y", &release, "epel-release"],
            INSTALL_TIMEOUT,
            EffectClass::Install,
        )?;
        let kernel = checked(
            runner,
            "uname",
            &["-r"],
            COMMAND_TIMEOUT,
            EffectClass::Install,
        )?;
        let kernel_devel = format!("kernel-devel-{}", kernel.stdout.trim());
        checked(
            runner,
            "dnf",
            &["install", "-y", &kernel_devel, "zfs"],
            INSTALL_TIMEOUT,
            EffectClass::Install,
        )?;
    } else {
        return Err(ZfsEffectError::UnsupportedPlatform);
    }
    checked(
        runner,
        "modprobe",
        &["zfs"],
        COMMAND_TIMEOUT,
        EffectClass::Install,
    )?;
    Ok(())
}

fn imported_pools(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<Vec<String>, ZfsEffectError> {
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
        .map(str::to_owned)
        .collect::<Vec<_>>();
    pools.sort();
    pools.dedup();
    Ok(pools)
}

fn select_pool(
    runner: &mut impl HostRunnerCommandRunner,
    selection: &PoolSelection,
    imported: &[String],
) -> Result<(String, PreparedStorageOrigin), ZfsEffectError> {
    match selection {
        PoolSelection::Explicit(pool) if imported.iter().any(|name| name == pool.as_str()) => {
            classify_adopted_pool(runner, pool.as_str())
        }
        PoolSelection::Explicit(pool) => Err(ZfsEffectError::ExplicitPoolAbsent {
            pool: pool.as_str().to_owned(),
        }),
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
    pool: &str,
) -> Result<(String, PreparedStorageOrigin), ZfsEffectError> {
    if pool == OWNED_POOL {
        let status = checked(
            runner,
            "zpool",
            &["status", "-P", pool],
            COMMAND_TIMEOUT,
            EffectClass::Mismatch,
        )?;
        if status
            .stdout
            .lines()
            .any(|line| line.trim() == OWNED_BACKING_FILE)
        {
            return Err(ZfsEffectError::PreparedStateUnavailable {
                message: format!(
                    "pool {OWNED_POOL} uses the Ployz backing image but has no prepared storage descriptor"
                ),
            });
        }
    }
    Ok((pool.to_owned(), PreparedStorageOrigin::Adopted))
}

fn prepare_owned_pool(
    runner: &mut impl HostRunnerCommandRunner,
) -> Result<(String, PreparedStorageOrigin), ZfsEffectError> {
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
        &["-s", &logical_size.to_string(), OWNED_BACKING_FILE],
        COMMAND_TIMEOUT,
        EffectClass::Sparse,
    )?;
    checked(
        runner,
        "zpool",
        &["create", "-f", OWNED_POOL, OWNED_BACKING_FILE],
        COMMAND_TIMEOUT,
        EffectClass::Sparse,
    )?;
    Ok((
        OWNED_POOL.to_owned(),
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(OWNED_BACKING_FILE),
        },
    ))
}

fn ensure_dataset(
    runner: &mut impl HostRunnerCommandRunner,
    dataset: &str,
    mountpoint: Option<&str>,
) -> Result<(), ZfsEffectError> {
    let listed = runner
        .command_with_timeout(
            "zfs",
            &["list", "-H", "-o", "name", dataset],
            COMMAND_TIMEOUT,
        )
        .map_err(|error| ZfsEffectError::Dataset {
            message: error.to_string(),
        })?;
    if listed.success && listed.stdout.trim() == dataset {
        return Ok(());
    }
    if !listed.success && listed.exit_code != Some(1) {
        return Err(ZfsEffectError::Dataset {
            message: listed.failure,
        });
    }
    let mut args = vec!["create", "-p"];
    let mount_property;
    if let Some(mountpoint) = mountpoint {
        mount_property = format!("mountpoint={mountpoint}");
        args.extend(["-o", mount_property.as_str()]);
    }
    args.push(dataset);
    checked(runner, "zfs", &args, COMMAND_TIMEOUT, EffectClass::Dataset)?;
    Ok(())
}

fn persist_prepared_storage_state(
    state_directory: &Path,
    state: &PreparedStorageState,
) -> Result<(), ZfsEffectError> {
    validate_prepared_state(state)?;
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

fn validate_prepared_state(state: &PreparedStorageState) -> Result<(), ZfsEffectError> {
    let pool = ZfsPoolName::try_new(state.pool.clone()).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: error.to_string(),
        }
    })?;
    let expected_root = format!("{}/ployz/volumes", pool.as_str());
    if state.dataset_root != expected_root {
        return Err(ZfsEffectError::PreparedStateUnavailable {
            message: format!(
                "dataset root {} does not equal {expected_root}",
                state.dataset_root
            ),
        });
    }
    if let PreparedStorageOrigin::OwnedImage { backing_file } = &state.origin
        && (state.pool != OWNED_POOL || backing_file != Path::new(OWNED_BACKING_FILE))
    {
        return Err(ZfsEffectError::PreparedStateUnavailable {
            message: format!(
                "unexpected owned pool identity {} {}",
                state.pool,
                backing_file.display()
            ),
        });
    }
    Ok(())
}

fn load_and_verify(
    runner: &mut impl HostRunnerCommandRunner,
    state_directory: &Path,
) -> Result<PreparedStorageState, ZfsEffectError> {
    let state = load_prepared_storage_state(state_directory)?;
    checked(
        runner,
        "zpool",
        &["list", "-H", "-o", "name", &state.pool],
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
            &state.dataset_root,
        ],
        COMMAND_TIMEOUT,
        EffectClass::Mismatch,
    )?;
    if observed.stdout.trim() != VOLUME_MOUNTPOINT {
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset root {} has mountpoint {:?}, expected {VOLUME_MOUNTPOINT}",
                state.dataset_root,
                observed.stdout.trim()
            ),
        });
    }
    if let PreparedStorageOrigin::OwnedImage { backing_file } = &state.origin {
        let path = backing_file.to_string_lossy();
        let observed = checked(
            runner,
            "zpool",
            &["status", "-P", &state.pool],
            COMMAND_TIMEOUT,
            EffectClass::Mismatch,
        )?;
        if !observed.stdout.lines().any(|line| line.trim() == path) {
            return Err(ZfsEffectError::PreparedStateMismatch {
                message: format!(
                    "owned pool {} does not report backing file {}",
                    state.pool,
                    backing_file.display()
                ),
            });
        }
    }
    Ok(state)
}

fn verify_child(state: &PreparedStorageState, dataset: &DatasetName) -> Result<(), ZfsEffectError> {
    let expected = format!("{}/", state.dataset_root);
    if !dataset.as_str().starts_with(&expected) {
        return Err(ZfsEffectError::DestructiveEffect {
            message: format!(
                "dataset {} is not a direct child of {}",
                dataset.as_str(),
                state.dataset_root
            ),
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum EffectClass {
    Install,
    PoolList,
    Sparse,
    Dataset,
    Destructive,
    Mismatch,
}

fn checked(
    runner: &mut impl HostRunnerCommandRunner,
    program: &str,
    args: &[&str],
    timeout: Duration,
    class: EffectClass,
) -> Result<HostRunnerCommandOutput, ZfsEffectError> {
    let output = runner
        .command_with_timeout(program, args, timeout)
        .map_err(|error| effect_error(class, error.to_string()))?;
    if !output.success {
        return Err(effect_error(class, output.failure));
    }
    Ok(output)
}

fn effect_error(class: EffectClass, message: String) -> ZfsEffectError {
    match class {
        EffectClass::Install => ZfsEffectError::Installation { message },
        EffectClass::PoolList => ZfsEffectError::PoolList { message },
        EffectClass::Sparse => ZfsEffectError::SparsePool { message },
        EffectClass::Dataset => ZfsEffectError::Dataset { message },
        EffectClass::Destructive => ZfsEffectError::DestructiveEffect { message },
        EffectClass::Mismatch => ZfsEffectError::PreparedStateMismatch { message },
    }
}

fn parse_u64(label: &str, value: &str) -> Result<u64, ZfsEffectError> {
    value.parse().map_err(|error| ZfsEffectError::GatherParse {
        message: format!("{label} {value:?}: {error}"),
    })
}

fn parse_last_u64(label: &str, output: &str) -> Result<u64, ZfsEffectError> {
    let Some(value) = output.split_whitespace().last() else {
        return Err(ZfsEffectError::GatherParse {
            message: format!("{label} output is empty"),
        });
    };
    parse_u64(label, value)
}

#[cfg(test)]
mod tests;
