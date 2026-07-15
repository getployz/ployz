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
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::operation::{FailureMessage, MachineSubstrateVersions};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use serde::Deserialize;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Stdio;

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
    let prepare = tokio::task::spawn_blocking(move || {
        let mut command = std::process::Command::new("ployz");
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
            .spawn()
    })
    .await;
    match prepare {
        Ok(Ok(_child)) => machine_success(MachineStoragePrepareRpcResponse::Ok(
            MachineStoragePrepareRpcOk { machine_id },
        )),
        Ok(Err(error)) => machine_domain_error(MachineStoragePrepareRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed {
                message: failure_message(format!("failed to launch storage preparation: {error}")),
            },
        }),
        Err(error) => machine_domain_error(MachineStoragePrepareRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed {
                message: failure_message(format!("storage preparation task failed: {error}")),
            },
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
        Err(message) => machine_domain_error(MachineStoragePrepareReportRpcResponse::DomainError {
            machine_id,
            error: MachineStoragePrepareDomainError::PreparationFailed { message },
        }),
    }
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum StoragePreparationEvidence {
    Completed {
        operation_id: OperationId,
        prepared: PreparedStorageEvidence,
    },
    Failed {
        operation_id: OperationId,
        failure: serde_json::Value,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparedStorageEvidence {
    pool: String,
    #[serde(rename = "origin")]
    _origin: serde_json::Value,
    #[serde(rename = "dataset_root")]
    _dataset_root: String,
}

fn read_storage_prepare_evidence(
    operation_id: &OperationId,
) -> Result<Option<ployz_core::deploy::ZfsPoolName>, FailureMessage> {
    read_storage_prepare_evidence_at(Path::new(STORAGE_OPERATION_DIRECTORY), operation_id)
}

fn read_storage_prepare_evidence_at(
    directory: &Path,
    operation_id: &OperationId,
) -> Result<Option<ployz_core::deploy::ZfsPoolName>, FailureMessage> {
    let path = directory.join(format!("{}.json", operation_id.as_str()));
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(failure_message(format!(
                "failed to read {}: {error}",
                path.display()
            )));
        }
    };
    let evidence: StoragePreparationEvidence = serde_json::from_slice(&bytes).map_err(|error| {
        failure_message(format!("failed to decode {}: {error}", path.display()))
    })?;
    match evidence {
        StoragePreparationEvidence::Completed {
            operation_id: recorded,
            prepared,
        } if recorded == *operation_id => {
            let PreparedStorageEvidence {
                pool,
                _origin: _,
                _dataset_root: _,
            } = prepared;
            ployz_core::deploy::ZfsPoolName::try_new(pool)
                .map(Some)
                .map_err(|error| failure_message(error.to_string()))
        }
        StoragePreparationEvidence::Failed {
            operation_id: recorded,
            failure,
        } if recorded == *operation_id => Err(failure_message(format!(
            "storage preparation failed: {failure}"
        ))),
        StoragePreparationEvidence::Completed { .. }
        | StoragePreparationEvidence::Failed { .. } => Err(failure_message(
            "storage preparation evidence names another operation".to_owned(),
        )),
    }
}

fn failure_message(message: String) -> FailureMessage {
    FailureMessage::try_new(message).expect("storage preparation evidence is non-empty")
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
}
