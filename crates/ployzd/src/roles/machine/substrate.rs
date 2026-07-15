use super::response::{machine_domain_error, machine_success};
use crate::roles::machine::protocol::{
    MachineStoragePrepareDomainError, MachineStoragePrepareReportRpcOk,
    MachineStoragePrepareReportRpcRequest, MachineStoragePrepareReportRpcResponse,
    MachineStoragePrepareRpcOk, MachineStoragePrepareRpcRequest, MachineStoragePrepareRpcResponse,
    MachineSubstrateReportRpcOk, MachineSubstrateReportRpcRequest,
    MachineSubstrateReportRpcResponse, MachineSubstrateUpdateDomainError,
    MachineSubstrateUpdateRpcOk, MachineSubstrateUpdateRpcRequest,
    MachineSubstrateUpdateRpcResponse,
};
use atomic_write_file::AtomicWriteFile;
#[cfg(unix)]
use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::operation::{FailureMessage, MachineSubstrateVersions};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use serde::Deserialize;
use std::io::ErrorKind;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as StdOpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ployz_core::storage::{
    MACHINE_STORAGE_PREPARE_BUDGET, MachineStoragePreparationEvidence, PreparedStorageState,
    StorageEffectFailure, StorageOperationEvidenceFile,
};

const SUBSTRATE_VERSION_FILE: &str = "/var/lib/ployz/substrate-version.json";
const STORAGE_OPERATION_DIRECTORY: &str = "/var/lib/ployz/storage-operations";
const MACHINE_SUBSTRATE_LOCK_FILE: &str = "/var/lib/ployz/machine-substrate.lock";

fn privileged_substrate_lock() -> Arc<tokio::sync::Mutex<()>> {
    static LOCK: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
    LOCK.get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub(crate) async fn handle_storage_prepare(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineStoragePrepareRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Ok(_machine_substrate_guard) = privileged_substrate_lock().try_lock_owned() else {
        return machine_domain_error(MachineStoragePrepareRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed {
                failure: StorageEffectFailure::ProcessFailed {
                    message: "another privileged machine substrate effect is running".to_owned(),
                },
            },
        });
    };
    let prepare = supervise_storage_prepare(
        &request.operation_id,
        Path::new(STORAGE_OPERATION_DIRECTORY),
        MACHINE_STORAGE_PREPARE_BUDGET,
        || {
            let mut command = tokio::process::Command::new("flock");
            command
                .arg("--nonblock")
                .arg(MACHINE_SUBSTRATE_LOCK_FILE)
                .arg("ployz")
                .arg("host")
                .arg("storage-prepare")
                .arg("--operation-id")
                .arg(request.operation_id.as_str());
            if let Some(pool) = request.pool {
                command.arg("--pool").arg(pool.as_str());
            }
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            command.spawn()
        },
    )
    .await;
    match prepare {
        Ok(prepared) => machine_success(MachineStoragePrepareRpcResponse::Ok(
            MachineStoragePrepareRpcOk {
                machine_id,
                pool: prepared.pool().clone(),
            },
        )),
        Err(failure) => machine_domain_error(MachineStoragePrepareRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed { failure },
        }),
    }
}

pub(crate) async fn handle_storage_prepare_report(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineStoragePrepareReportRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match StorageEvidenceRepository::host_default().read_pool(&request.operation_id) {
        Ok(pool) => machine_success(MachineStoragePrepareReportRpcResponse::Ok(
            MachineStoragePrepareReportRpcOk { machine_id, pool },
        )),
        Err(failure) => machine_domain_error(MachineStoragePrepareReportRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed { failure },
        }),
    }
}

#[cfg(test)]
fn read_storage_prepare_evidence_at(
    directory: &Path,
    operation_id: &OperationId,
) -> Result<Option<ployz_core::deploy::ZfsPoolName>, StorageEffectFailure> {
    StorageEvidenceRepository::new(directory).read_pool(operation_id)
}

struct StorageEvidenceRepository<'a> {
    directory: &'a Path,
}

impl<'a> StorageEvidenceRepository<'a> {
    fn new(directory: &'a Path) -> Self {
        Self { directory }
    }

    fn host_default() -> Self {
        Self::new(Path::new(STORAGE_OPERATION_DIRECTORY))
    }

    fn file(&self, operation_id: &OperationId) -> StorageOperationEvidenceFile {
        StorageOperationEvidenceFile::in_evidence_directory(self.directory, operation_id.clone())
    }

    fn read_optional(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<MachineStoragePreparationEvidence>, StorageEffectFailure> {
        let file = self.file(operation_id);
        let bytes = match std::fs::read(file.path()) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StorageEffectFailure::ProcessFailed {
                    message: format!("failed to read {}: {error}", file.path().display()),
                });
            }
        };
        let evidence: MachineStoragePreparationEvidence =
            serde_json::from_slice(&bytes).map_err(|error| {
                StorageEffectFailure::ProcessFailed {
                    message: format!("failed to decode {}: {error}", file.path().display()),
                }
            })?;
        file.validate(&evidence)?;
        Ok(Some(evidence))
    }

    fn read_pool(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<ployz_core::deploy::ZfsPoolName>, StorageEffectFailure> {
        match self.read_optional(operation_id)? {
            Some(MachineStoragePreparationEvidence::Completed { prepared, .. }) => {
                Ok(Some(prepared.pool().clone()))
            }
            Some(MachineStoragePreparationEvidence::Failed { failure, .. }) => Err(failure),
            None => Ok(None),
        }
    }

    fn persist_failure(
        &self,
        operation_id: &OperationId,
        failure: &StorageEffectFailure,
    ) -> Result<(), StorageEffectFailure> {
        std::fs::create_dir_all(self.directory).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to create {}: {error}", self.directory.display()),
            }
        })?;
        #[cfg(unix)]
        std::fs::set_permissions(self.directory, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| StorageEffectFailure::ProcessFailed {
                message: format!("failed to protect {}: {error}", self.directory.display()),
            },
        )?;
        let file = self.file(operation_id);
        let bytes = serde_json::to_vec_pretty(&MachineStoragePreparationEvidence::Failed {
            operation_id: operation_id.clone(),
            failure: failure.clone(),
        })
        .map_err(|error| StorageEffectFailure::ProcessFailed {
            message: format!("failed to encode terminal evidence: {error}"),
        })?;
        let mut atomic = open_secret_atomic(file.path()).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to create {}: {error}", file.path().display()),
            }
        })?;
        atomic
            .write_all(&bytes)
            .and_then(|()| atomic.commit())
            .map_err(|error| StorageEffectFailure::ProcessFailed {
                message: format!("failed to commit {}: {error}", file.path().display()),
            })?;
        Ok(())
    }
}

fn open_secret_atomic(path: &Path) -> std::io::Result<AtomicWriteFile> {
    let mut options = AtomicWriteFile::options();
    #[cfg(unix)]
    {
        AtomicOpenOptionsExt::preserve_mode(&mut options, false);
        StdOpenOptionsExt::mode(&mut options, 0o600);
    }
    options.open(path)
}

async fn supervise_storage_prepare<F>(
    operation_id: &OperationId,
    evidence_directory: &Path,
    budget: Duration,
    spawn: F,
) -> Result<PreparedStorageState, StorageEffectFailure>
where
    F: FnOnce() -> std::io::Result<tokio::process::Child>,
{
    if let Some(evidence) =
        StorageEvidenceRepository::new(evidence_directory).read_optional(operation_id)?
    {
        return match evidence {
            MachineStoragePreparationEvidence::Completed { prepared, .. } => Ok(prepared),
            MachineStoragePreparationEvidence::Failed { failure, .. } => Err(failure),
        };
    }
    let mut child = spawn().map_err(|error| StorageEffectFailure::ProcessFailed {
        message: format!("failed to launch storage preparation: {error}"),
    })?;
    let status = match tokio::time::timeout(budget, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            terminate_storage_prepare_child(&mut child).await?;
            let failure = StorageEffectFailure::ProcessFailed {
                message: format!("failed waiting for storage preparation: {error}"),
            };
            StorageEvidenceRepository::new(evidence_directory)
                .persist_failure(operation_id, &failure)?;
            return Err(failure);
        }
        Err(_) => {
            terminate_storage_prepare_child(&mut child).await?;
            let failure = StorageEffectFailure::OperationTimedOut;
            StorageEvidenceRepository::new(evidence_directory)
                .persist_failure(operation_id, &failure)?;
            return Err(failure);
        }
    };
    let Some(evidence) =
        StorageEvidenceRepository::new(evidence_directory).read_optional(operation_id)?
    else {
        let failure = StorageEffectFailure::ProcessFailed {
            message: "storage preparation exited without terminal evidence".to_owned(),
        };
        StorageEvidenceRepository::new(evidence_directory)
            .persist_failure(operation_id, &failure)?;
        return Err(failure);
    };
    match evidence {
        MachineStoragePreparationEvidence::Completed { prepared, .. } if status.success() => {
            Ok(prepared)
        }
        MachineStoragePreparationEvidence::Failed { failure, .. } => Err(failure),
        MachineStoragePreparationEvidence::Completed { .. } => {
            let failure = StorageEffectFailure::ProcessFailed {
                message: format!("storage preparation exited with {status}"),
            };
            StorageEvidenceRepository::new(evidence_directory)
                .persist_failure(operation_id, &failure)?;
            Err(failure)
        }
    }
}

async fn terminate_storage_prepare_child(
    child: &mut tokio::process::Child,
) -> Result<(), StorageEffectFailure> {
    child
        .kill()
        .await
        .map_err(|error| StorageEffectFailure::ProcessFailed {
            message: format!("failed to terminate storage preparation: {error}"),
        })
}

pub(crate) async fn handle_substrate_update(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineSubstrateUpdateRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Ok(machine_substrate_guard) = privileged_substrate_lock().try_lock_owned() else {
        return machine_domain_error(MachineSubstrateUpdateRpcResponse::DomainError {
            machine_id,
            error: MachineSubstrateUpdateDomainError::UpdateFailed {
                message: FailureMessage::try_new(
                    "another privileged machine substrate effect is running",
                )
                .expect("busy message is non-empty"),
            },
        });
    };
    let child = tokio::process::Command::new("flock")
        .arg("--nonblock")
        .arg(MACHINE_SUBSTRATE_LOCK_FILE)
        .arg("ployz")
        .arg("host")
        .arg("substrate-update")
        .arg("--operation-id")
        .arg(request.operation_id.as_str())
        .arg("--version")
        .arg(request.target_version.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            return machine_domain_error(MachineSubstrateUpdateRpcResponse::DomainError {
                machine_id,
                error: MachineSubstrateUpdateDomainError::UpdateFailed {
                    message: FailureMessage::try_new(format!(
                        "failed to run ployz host substrate-update: {error}"
                    ))
                    .expect("process failure message is non-empty"),
                },
            });
        }
    };
    tokio::spawn(async move {
        let _machine_substrate_guard = machine_substrate_guard;
        let _ = child.wait().await;
    });
    machine_success(MachineSubstrateUpdateRpcResponse::Ok(
        MachineSubstrateUpdateRpcOk { machine_id },
    ))
}

pub(crate) async fn handle_substrate_report(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineSubstrateReportRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let reported = match read_substrate_update_evidence(&request.operation_id) {
        Ok(reported) => reported,
        Err(message) => {
            return machine_domain_error(MachineSubstrateReportRpcResponse::DomainError {
                machine_id,
                error: MachineSubstrateUpdateDomainError::UpdateFailed { message },
            });
        }
    };
    machine_success(MachineSubstrateReportRpcResponse::Ok(
        MachineSubstrateReportRpcOk {
            machine_id,
            reported,
        },
    ))
}

#[derive(Deserialize)]
struct SubstrateUpdateEvidence {
    operation_id: OperationId,
    ployzd: InstallArtifactVersion,
}

fn read_substrate_update_evidence(
    operation_id: &OperationId,
) -> Result<MachineSubstrateVersions, FailureMessage> {
    let path = Path::new(SUBSTRATE_VERSION_FILE);
    if !path.exists() {
        return Ok(MachineSubstrateVersions::default());
    }
    let bytes = std::fs::read(path).map_err(|error| {
        FailureMessage::try_new(format!(
            "failed to read substrate update evidence {}: {error}",
            path.display()
        ))
        .expect("substrate update evidence read message is non-empty")
    })?;
    let evidence: SubstrateUpdateEvidence = serde_json::from_slice(&bytes).map_err(|error| {
        FailureMessage::try_new(format!(
            "failed to decode substrate update evidence {}: {error}",
            path.display()
        ))
        .expect("substrate update evidence decode message is non-empty")
    })?;
    if &evidence.operation_id != operation_id {
        return Ok(MachineSubstrateVersions::default());
    }
    Ok(MachineSubstrateVersions {
        ployzd: Some(evidence.ployzd),
        host_runner: None,
    })
}

#[cfg(test)]
mod storage_tests {
    use std::cell::Cell;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn missing_operation_evidence_is_pending() {
        let directory = tempfile::tempdir().expect("temporary evidence directory");
        let operation_id = OperationId::try_new("op_storage_pending").expect("operation id");

        assert_eq!(
            read_storage_prepare_evidence_at(directory.path(), &operation_id).expect("pending"),
            None
        );
    }

    #[test]
    fn completed_operation_evidence_preserves_the_pool() {
        let directory = tempfile::tempdir().expect("temporary evidence directory");
        let operation_id = OperationId::try_new("op_storage_complete").expect("operation id");
        std::fs::write(
            directory.path().join("op_storage_complete.json"),
            r#"{"state":"completed","operation_id":"op_storage_complete","prepared":{"pool":"tank","origin":{"kind":"adopted"},"dataset_root":"tank/ployz/volumes"}}"#,
        )
        .expect("write evidence");

        let pool = read_storage_prepare_evidence_at(directory.path(), &operation_id)
            .expect("valid evidence")
            .expect("terminal evidence");
        assert_eq!(pool.as_str(), "tank");
    }

    #[tokio::test]
    async fn timeout_reaps_child_before_terminal_evidence_and_replay_does_not_spawn() {
        let directory = tempfile::tempdir().expect("temporary evidence directory");
        let operation_id = OperationId::try_new("op_storage_timeout").expect("operation id");
        let pid = Cell::new(None);

        let result = supervise_storage_prepare(
            &operation_id,
            directory.path(),
            Duration::from_millis(10),
            || {
                let mut command = tokio::process::Command::new("sleep");
                command.arg("30").kill_on_drop(true);
                let child = command.spawn()?;
                pid.set(child.id());
                Ok(child)
            },
        )
        .await;

        assert_eq!(result, Err(StorageEffectFailure::OperationTimedOut));
        let Some(pid) = pid.get() else {
            panic!("spawned child must have a process id")
        };
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert!(matches!(
            StorageEvidenceRepository::new(directory.path())
                .read_optional(&operation_id)
                .expect("timeout evidence is readable"),
            Some(MachineStoragePreparationEvidence::Failed {
                failure: StorageEffectFailure::OperationTimedOut,
                ..
            })
        ));
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::metadata(directory.path())
                    .expect("evidence directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(directory.path().join("op_storage_timeout.json"))
                    .expect("evidence file metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let replay = supervise_storage_prepare(
            &operation_id,
            directory.path(),
            Duration::from_millis(10),
            || panic!("terminal replay must not spawn another process"),
        )
        .await;
        assert_eq!(replay, Err(StorageEffectFailure::OperationTimedOut));
    }
}
