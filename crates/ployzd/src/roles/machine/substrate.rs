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
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::operation::{FailureMessage, MachineSubstrateVersions};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use serde::Deserialize;
use std::io::ErrorKind;
use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use ployz_core::storage::{
    MACHINE_STORAGE_PREPARE_BUDGET, MachineStoragePreparationEvidence, PreparedStorageState,
    StorageEffectFailure,
};

const SUBSTRATE_VERSION_FILE: &str = "/var/lib/ployz/substrate-version.json";
const STORAGE_OPERATION_DIRECTORY: &str = "/var/lib/ployz/storage-operations";

pub(crate) async fn handle_storage_prepare(
    machine_id: MachineId,
    _state: (),
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<MachineStoragePrepareRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let prepare = supervise_storage_prepare(
        &request.operation_id,
        Path::new(STORAGE_OPERATION_DIRECTORY),
        MACHINE_STORAGE_PREPARE_BUDGET,
        || {
            let mut command = tokio::process::Command::new("ployz");
            command
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
    match read_storage_prepare_evidence(&request.operation_id) {
        Ok(pool) => machine_success(MachineStoragePrepareReportRpcResponse::Ok(
            MachineStoragePrepareReportRpcOk { machine_id, pool },
        )),
        Err(failure) => machine_domain_error(MachineStoragePrepareReportRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed { failure },
        }),
    }
}

fn read_storage_prepare_evidence(
    operation_id: &OperationId,
) -> Result<Option<ployz_core::deploy::ZfsPoolName>, StorageEffectFailure> {
    read_storage_prepare_evidence_at(Path::new(STORAGE_OPERATION_DIRECTORY), operation_id)
}

fn read_storage_prepare_evidence_at(
    directory: &Path,
    operation_id: &OperationId,
) -> Result<Option<ployz_core::deploy::ZfsPoolName>, StorageEffectFailure> {
    let path = directory.join(format!("{}.json", operation_id.as_str()));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(StorageEffectFailure::ProcessFailed {
                message: format!("failed to read {}: {error}", path.display()),
            });
        }
    };
    let evidence: MachineStoragePreparationEvidence =
        serde_json::from_slice(&bytes).map_err(|error| StorageEffectFailure::ProcessFailed {
            message: format!("failed to decode {}: {error}", path.display()),
        })?;
    match evidence {
        MachineStoragePreparationEvidence::Completed {
            operation_id: recorded,
            prepared,
        } if recorded == *operation_id => Ok(Some(prepared.pool().clone())),
        MachineStoragePreparationEvidence::Failed {
            operation_id: recorded,
            failure,
        } if recorded == *operation_id => Err(failure),
        MachineStoragePreparationEvidence::Completed { .. }
        | MachineStoragePreparationEvidence::Failed { .. } => {
            Err(StorageEffectFailure::ProcessFailed {
                message: "storage preparation evidence names another operation".to_owned(),
            })
        }
    }
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
        read_optional_typed_storage_prepare_evidence(evidence_directory, operation_id)?
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
            persist_storage_prepare_failure(evidence_directory, operation_id, &failure)?;
            return Err(failure);
        }
        Err(_) => {
            terminate_storage_prepare_child(&mut child).await?;
            let failure = StorageEffectFailure::OperationTimedOut;
            persist_storage_prepare_failure(evidence_directory, operation_id, &failure)?;
            return Err(failure);
        }
    };
    let Some(evidence) =
        read_optional_typed_storage_prepare_evidence(evidence_directory, operation_id)?
    else {
        return Err(StorageEffectFailure::ProcessFailed {
            message: "storage preparation exited without terminal evidence".to_owned(),
        });
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
            persist_storage_prepare_failure(evidence_directory, operation_id, &failure)?;
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

fn read_optional_typed_storage_prepare_evidence(
    directory: &Path,
    operation_id: &OperationId,
) -> Result<Option<MachineStoragePreparationEvidence>, StorageEffectFailure> {
    let path = directory.join(format!("{}.json", operation_id.as_str()));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(StorageEffectFailure::ProcessFailed {
                message: format!("failed to read {}: {error}", path.display()),
            });
        }
    };
    let evidence =
        serde_json::from_slice::<MachineStoragePreparationEvidence>(&bytes).map_err(|error| {
            StorageEffectFailure::ProcessFailed {
                message: format!("failed to decode {}: {error}", path.display()),
            }
        })?;
    if evidence.operation_id() != operation_id {
        return Err(StorageEffectFailure::ProcessFailed {
            message: "storage preparation evidence names another operation".to_owned(),
        });
    }
    Ok(Some(evidence))
}

fn persist_storage_prepare_failure(
    directory: &Path,
    operation_id: &OperationId,
    failure: &StorageEffectFailure,
) -> Result<(), StorageEffectFailure> {
    std::fs::create_dir_all(directory).map_err(|error| StorageEffectFailure::ProcessFailed {
        message: format!("failed to create {}: {error}", directory.display()),
    })?;
    let path = directory.join(format!("{}.json", operation_id.as_str()));
    let bytes = serde_json::to_vec_pretty(&MachineStoragePreparationEvidence::Failed {
        operation_id: operation_id.clone(),
        failure: failure.clone(),
    })
    .map_err(|error| StorageEffectFailure::ProcessFailed {
        message: format!("failed to encode timeout evidence: {error}"),
    })?;
    let mut file = AtomicWriteFile::options().open(&path).map_err(|error| {
        StorageEffectFailure::ProcessFailed {
            message: format!("failed to create {}: {error}", path.display()),
        }
    })?;
    file.write_all(&bytes)
        .and_then(|()| file.commit())
        .map_err(|error| StorageEffectFailure::ProcessFailed {
            message: format!("failed to commit {}: {error}", path.display()),
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
    let update = tokio::task::spawn_blocking(move || {
        std::process::Command::new("ployz")
            .arg("host")
            .arg("substrate-update")
            .arg("--operation-id")
            .arg(request.operation_id.as_str())
            .arg("--version")
            .arg(request.target_version.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    })
    .await;

    match update {
        Ok(Ok(_child)) => machine_success(MachineSubstrateUpdateRpcResponse::Ok(
            MachineSubstrateUpdateRpcOk { machine_id },
        )),
        Ok(Err(error)) => machine_domain_error(MachineSubstrateUpdateRpcResponse::DomainError {
            machine_id,
            error: MachineSubstrateUpdateDomainError::UpdateFailed {
                message: FailureMessage::try_new(format!(
                    "failed to run ployz host substrate-update: {error}"
                ))
                .expect("process failure message is non-empty"),
            },
        }),
        Err(error) => machine_domain_error(MachineSubstrateUpdateRpcResponse::DomainError {
            machine_id,
            error: MachineSubstrateUpdateDomainError::UpdateFailed {
                message: FailureMessage::try_new(format!("substrate update task failed: {error}"))
                    .expect("task failure message is non-empty"),
            },
        }),
    }
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
            read_optional_typed_storage_prepare_evidence(directory.path(), &operation_id)
                .expect("timeout evidence is readable"),
            Some(MachineStoragePreparationEvidence::Failed {
                failure: StorageEffectFailure::OperationTimedOut,
                ..
            })
        ));

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
