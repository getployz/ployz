//! Storage preparation and operation-scoped evidence.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::deploy::ZfsPoolName;
use ployz_core::ids::OperationId;
use ployz_core::storage::{
    MachineStoragePreparationEvidence as StoragePreparationEvidence, PROVISIONED_VOLUME_MOUNTPOINT,
    PreparedStorageOrigin, PreparedStorageState, StorageEffectFailure as ZfsEffectError,
    StorageOperationEvidenceFile, StoragePreparationProcessIdentity, ZfsDatasetRoot,
};

use super::command::{COMMAND_TIMEOUT, EffectClass, INSTALL_TIMEOUT, checked};
use super::state::{
    PREPARED_STORAGE_FILE, imported_pools, load_and_verify, persist_prepared_storage_state,
    select_pool,
};
use crate::execution::{
    FileMode, HostPlatformProfile, HostRunnerCommandRunner, SupervisorBackend, SupervisorChange,
    SupervisorUnitSpec, ZfsInstall, execute_supervisor_commands, write_durable_file,
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
    supervisor_directory: &Path,
) -> Result<PreparedStorageState, ZfsEffectError> {
    let evidence_file =
        StorageOperationEvidenceFile::in_state_directory(state_directory, operation_id.clone());
    let context = StoragePreparationContext {
        profile,
        operation_id,
        selection,
        state_directory,
        docker_drop_in_directory,
        supervisor_directory,
        evidence_file,
    };
    if context.evidence_file.path().exists() {
        let bytes = std::fs::read(context.evidence_file.path()).map_err(|error| {
            ZfsEffectError::PreparedStateUnavailable {
                message: format!(
                    "failed to read {}: {error}",
                    context.evidence_file.path().display()
                ),
            }
        })?;
        let evidence: StoragePreparationEvidence =
            serde_json::from_slice(&bytes).map_err(|error| {
                ZfsEffectError::PreparedStateUnavailable {
                    message: format!(
                        "failed to parse {}: {error}",
                        context.evidence_file.path().display()
                    ),
                }
            })?;
        context.evidence_file.validate(&evidence)?;
        match evidence {
            StoragePreparationEvidence::Completed { prepared, .. } => Ok(prepared),
            StoragePreparationEvidence::Failed { failure, .. } => Err(failure),
            StoragePreparationEvidence::Cancelled { .. } => Err(ZfsEffectError::Interrupted {
                message: "storage preparation was cancelled".to_owned(),
            }),
            StoragePreparationEvidence::Running { process, .. } => {
                let current = current_process_identity()?;
                if process != current {
                    return Err(ZfsEffectError::ProcessFailed {
                        message: "storage preparation is already owned by another process"
                            .to_owned(),
                    });
                }
                prepare_and_commit(runner, &context)
            }
        }
    } else {
        establish_running_evidence(&context.evidence_file, operation_id)?;
        prepare_and_commit(runner, &context)
    }
}

struct StoragePreparationContext<'a> {
    profile: &'a HostPlatformProfile,
    operation_id: &'a OperationId,
    selection: &'a PoolSelection,
    state_directory: &'a Path,
    docker_drop_in_directory: &'a Path,
    supervisor_directory: &'a Path,
    evidence_file: StorageOperationEvidenceFile,
}

fn prepare_and_commit(
    runner: &mut impl HostRunnerCommandRunner,
    context: &StoragePreparationContext<'_>,
) -> Result<PreparedStorageState, ZfsEffectError> {
    let StoragePreparationContext {
        profile,
        operation_id,
        selection,
        state_directory,
        docker_drop_in_directory,
        supervisor_directory,
        evidence_file,
    } = context;
    std::fs::create_dir_all(evidence_file.directory()).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!(
                "failed to create operation evidence directory {}: {error}",
                evidence_file.directory().display()
            ),
        }
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(
        evidence_file.directory(),
        std::fs::Permissions::from_mode(0o700),
    )
    .map_err(|error| ZfsEffectError::PreparedStateUnavailable {
        message: format!(
            "failed to protect operation evidence directory {}: {error}",
            evidence_file.directory().display()
        ),
    })?;
    let result = prepare_storage(
        runner,
        profile,
        selection,
        state_directory,
        docker_drop_in_directory,
        supervisor_directory,
    );
    let evidence = match &result {
        Ok(prepared) => StoragePreparationEvidence::Completed {
            operation_id: (*operation_id).clone(),
            prepared: prepared.clone(),
        },
        Err(failure) => StoragePreparationEvidence::Failed {
            operation_id: (*operation_id).clone(),
            failure: failure.clone(),
        },
    };
    let bytes = serde_json::to_vec_pretty(&evidence).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!("failed to serialize preparation evidence: {error}"),
        }
    })?;
    write_durable_file(
        evidence_file.directory(),
        evidence_file
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("evidence filename is UTF-8"),
        FileMode::Secret0600,
        &bytes,
    )
    .map_err(|error| ZfsEffectError::PreparedStateUnavailable {
        message: error.to_string(),
    })?;
    result
}

fn establish_running_evidence(
    evidence_file: &StorageOperationEvidenceFile,
    operation_id: &OperationId,
) -> Result<(), ZfsEffectError> {
    std::fs::create_dir_all(evidence_file.directory()).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!(
                "failed to create operation evidence directory {}: {error}",
                evidence_file.directory().display()
            ),
        }
    })?;
    #[cfg(unix)]
    std::fs::set_permissions(
        evidence_file.directory(),
        std::fs::Permissions::from_mode(0o700),
    )
    .map_err(|error| ZfsEffectError::PreparedStateUnavailable {
        message: format!(
            "failed to protect operation evidence directory {}: {error}",
            evidence_file.directory().display()
        ),
    })?;
    let launched_at_unix_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ZfsEffectError::ProcessFailed {
            message: format!("system clock precedes Unix epoch: {error}"),
        })?
        .as_millis()
        .try_into()
        .map_err(|_| ZfsEffectError::ProcessFailed {
            message: "storage preparation launch time exceeds u64".to_owned(),
        })?;
    let evidence = StoragePreparationEvidence::Running {
        operation_id: operation_id.clone(),
        launched_at_unix_millis,
        process: current_process_identity()?,
    };
    let bytes = serde_json::to_vec_pretty(&evidence).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: format!("failed to serialize running preparation evidence: {error}"),
        }
    })?;
    write_durable_file(
        evidence_file.directory(),
        evidence_file
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .expect("evidence filename is UTF-8"),
        FileMode::Secret0600,
        &bytes,
    )
    .map_err(|error| ZfsEffectError::PreparedStateUnavailable {
        message: error.to_string(),
    })
}

#[cfg(target_os = "linux")]
fn current_process_identity() -> Result<StoragePreparationProcessIdentity, ZfsEffectError> {
    let pid = std::process::id();
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map_err(process_identity_error)?
        .trim()
        .to_owned();
    let stat =
        std::fs::read_to_string(format!("/proc/{pid}/stat")).map_err(process_identity_error)?;
    let (_, tail) = stat
        .rsplit_once(") ")
        .ok_or_else(|| ZfsEffectError::ProcessFailed {
            message: "current process stat has no command terminator".to_owned(),
        })?;
    let start_time_ticks = tail
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| ZfsEffectError::ProcessFailed {
            message: "current process stat has no start time".to_owned(),
        })?
        .parse()
        .map_err(|error| ZfsEffectError::ProcessFailed {
            message: format!("current process start time is invalid: {error}"),
        })?;
    let command = std::fs::read(format!("/proc/{pid}/cmdline")).map_err(process_identity_error)?;
    let expected_command = command
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument))
        .collect::<Vec<_>>()
        .join("\u{0}");
    Ok(StoragePreparationProcessIdentity {
        boot_id,
        pid,
        start_time_ticks,
        expected_command,
    })
}

#[cfg(not(target_os = "linux"))]
fn current_process_identity() -> Result<StoragePreparationProcessIdentity, ZfsEffectError> {
    Err(ZfsEffectError::UnsupportedPlatform)
}

fn process_identity_error(error: std::io::Error) -> ZfsEffectError {
    ZfsEffectError::ProcessFailed {
        message: format!("failed to read storage preparation process identity: {error}"),
    }
}

pub fn prepare_storage(
    runner: &mut impl HostRunnerCommandRunner,
    profile: &HostPlatformProfile,
    selection: &PoolSelection,
    state_directory: &Path,
    docker_drop_in_directory: &Path,
    supervisor_directory: &Path,
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
    ensure_dataset(runner, &parent, "none")?;
    ensure_dataset(runner, dataset_root.as_str(), PROVISIONED_VOLUME_MOUNTPOINT)?;
    let state = PreparedStorageState::try_new(pool, origin, dataset_root).map_err(|error| {
        ZfsEffectError::PreparedStateUnavailable {
            message: error.to_string(),
        }
    })?;
    persist_prepared_storage_state(state_directory, &state)?;
    install_docker_zfs_ordering(
        runner,
        profile.supervisor().into(),
        docker_drop_in_directory,
        supervisor_directory,
        state.origin(),
    )?;
    Ok(state)
}

pub(super) fn install_docker_zfs_ordering(
    runner: &mut impl HostRunnerCommandRunner,
    backend: SupervisorBackend,
    directory: &Path,
    supervisor_directory: &Path,
    origin: &PreparedStorageOrigin,
) -> Result<(), ZfsEffectError> {
    if matches!(origin, PreparedStorageOrigin::OwnedImage { .. })
        && backend != SupervisorBackend::Systemd
    {
        return Err(ZfsEffectError::Dataset {
            message: "Ployz-owned ZFS recovery requires systemd".to_owned(),
        });
    }
    std::fs::create_dir_all(directory).map_err(|error| ZfsEffectError::Dataset {
        message: format!(
            "failed to create Docker systemd drop-in directory {}: {error}",
            directory.display()
        ),
    })?;
    let (drop_in, enable_target) = match origin {
        PreparedStorageOrigin::OwnedImage { .. } => {
            let spec = SupervisorUnitSpec::OwnedZfsImport;
            let rendered = backend
                .render(&spec)
                .map_err(|error| ZfsEffectError::Dataset {
                    message: error.to_string(),
                })?;
            write_durable_file(
                supervisor_directory,
                rendered.file_name(),
                FileMode::Plain,
                rendered.contents().as_bytes(),
            )
            .map_err(|error| ZfsEffectError::Dataset {
                message: error.to_string(),
            })?;
            (
                b"[Unit]\nRequires=ployz-owned-zfs-import.service\nAfter=ployz-owned-zfs-import.service\n"
                    .as_slice(),
                Some(spec.target()),
            )
        }
        PreparedStorageOrigin::Adopted => (b"[Unit]\nAfter=zfs.target\n".as_slice(), None),
    };
    write_durable_file(directory, "ployz-zfs.conf", FileMode::Plain, drop_in).map_err(|error| {
        ZfsEffectError::Dataset {
            message: error.to_string(),
        }
    })?;
    if let Some(target) = enable_target {
        execute_supervisor_commands(runner, backend.commands(SupervisorChange::Enable, &target))
            .map_err(|error| ZfsEffectError::Dataset {
                message: error.to_string(),
            })?;
    } else {
        checked(
            runner,
            "systemctl",
            &["daemon-reload"],
            COMMAND_TIMEOUT,
            EffectClass::Dataset,
        )?;
    }
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
            let release =
                format!("https://zfsonlinux.org/epel/zfs-release-3-0.el{major_release}.noarch.rpm");
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
    expected_mountpoint: &str,
) -> Result<(), ZfsEffectError> {
    let listed = runner
        .command_with_timeout(
            "zfs",
            &["list", "-H", "-o", "name,mountpoint", dataset],
            COMMAND_TIMEOUT,
        )
        .map_err(|error| ZfsEffectError::Dataset {
            message: error.to_string(),
        })?;
    if listed.success {
        let expected = format!("{dataset}\t{expected_mountpoint}");
        if listed.stdout.trim() == expected {
            return Ok(());
        }
        return Err(ZfsEffectError::PreparedStateMismatch {
            message: format!(
                "dataset {dataset} has unexpected name/mountpoint row {:?}, expected {expected:?}",
                listed.stdout.trim()
            ),
        });
    }
    if !listed.success && listed.exit_code != Some(1) {
        return Err(ZfsEffectError::Dataset {
            message: listed.failure,
        });
    }
    let mount_property = format!("mountpoint={expected_mountpoint}");
    let mut args = vec!["create", "-p", "-o", mount_property.as_str()];
    args.push(dataset);
    checked(runner, "zfs", &args, COMMAND_TIMEOUT, EffectClass::Dataset)?;
    Ok(())
}
