//! Storage preparation and operation-scoped evidence.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use ployz_core::deploy::ZfsPoolName;
use ployz_core::ids::OperationId;
use ployz_core::storage::{
    MachineStoragePreparationEvidence as StoragePreparationEvidence, PreparedStorageState,
    StorageEffectFailure as ZfsEffectError, ZfsDatasetRoot,
};

use super::command::{COMMAND_TIMEOUT, EffectClass, INSTALL_TIMEOUT, checked};
use super::state::{
    PREPARED_STORAGE_FILE, VOLUME_MOUNTPOINT, imported_pools, load_and_verify,
    persist_prepared_storage_state, select_pool,
};
use crate::execution::{
    FileMode, HostPlatformProfile, HostRunnerCommandRunner, ZfsInstall, write_durable_file,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolSelection {
    Automatic,
    Explicit(ZfsPoolName),
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
        if !imported.iter().any(|pool| pool == state.pool()) {
            return Err(ZfsEffectError::PreparedStateMismatch {
                message: format!("prepared pool {} is not imported", state.pool().as_str()),
            });
        }
        match selection {
            PoolSelection::Explicit(requested) if requested != state.pool() => {
                return Err(ZfsEffectError::PreparedStateMismatch {
                    message: format!(
                        "prepared pool {} does not equal explicit pool {}",
                        state.pool().as_str(),
                        requested.as_str()
                    ),
                });
            }
            PoolSelection::Automatic | PoolSelection::Explicit(_) => {}
        }
        (state.pool().clone(), state.origin().clone())
    } else {
        select_pool(runner, selection, &imported)?
    };
    let dataset_root = ZfsDatasetRoot::for_pool(&pool);

    checked(
        runner,
        "zpool",
        &["set", "failmode=continue", pool.as_str()],
        COMMAND_TIMEOUT,
        EffectClass::Dataset,
    )?;
    let parent = format!("{}/ployz", pool.as_str());
    ensure_dataset(runner, &parent, None)?;
    ensure_dataset(runner, dataset_root.as_str(), Some(VOLUME_MOUNTPOINT))?;
    let state = PreparedStorageState::try_new(pool, origin, dataset_root).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: error.to_string(),
        }
    })?;
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

fn install_zfs(
    runner: &mut impl HostRunnerCommandRunner,
    profile: &HostPlatformProfile,
) -> Result<(), ZfsEffectError> {
    match profile.zfs_install() {
        Some(ZfsInstall::UbuntuPackages) => {
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
        }
        Some(ZfsInstall::RockyPackages { major_release }) => {
            let major = major_release
                .as_deref()
                .ok_or_else(|| ZfsEffectError::Installation {
                    message: "Rocky VERSION_ID has no major release".to_owned(),
                })?;
            let release =
                format!("https://zfsonlinux.org/epel/zfs-release-3-0.el{major}.noarch.rpm");
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
        }
        None => return Err(ZfsEffectError::UnsupportedPlatform),
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
