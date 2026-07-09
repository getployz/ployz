use super::response::{machine_domain_error, machine_success};
use crate::roles::machine::protocol::{
    MachineSubstrateReportRpcOk, MachineSubstrateReportRpcRequest,
    MachineSubstrateReportRpcResponse, MachineSubstrateUpdateDomainError,
    MachineSubstrateUpdateRpcOk, MachineSubstrateUpdateRpcRequest,
    MachineSubstrateUpdateRpcResponse,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::ops::{FailureMessage, MachineSubstrateVersions};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;

const SUBSTRATE_VERSION_FILE: &str = "/var/lib/ployz/substrate-version.json";

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
