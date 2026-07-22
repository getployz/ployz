//! Machine-local privileged substrate services.

mod storage_prepare;

pub(crate) use storage_prepare::{
    StoragePrepareRuntime, handle_storage_prepare, handle_storage_prepare_cancel,
    handle_storage_prepare_report,
};

use super::protocol::{
    MachineSubstrateReportRpcOk, MachineSubstrateReportRpcRequest,
    MachineSubstrateReportRpcResponse, MachineSubstrateUpdateDomainError,
    MachineSubstrateUpdateRpcOk, MachineSubstrateUpdateRpcRequest,
    MachineSubstrateUpdateRpcResponse,
};
use super::response::{machine_domain_error, machine_success};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::install::InstallArtifactVersion;
use ployz_core::operation::{
    FailureMessage, MACHINE_SUBSTRATE_UPDATE_LEAK_BACKSTOP,
    MACHINE_SUBSTRATE_UPDATE_TERMINATION_GRACE, MachineSubstrateVersions,
};
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const SUBSTRATE_VERSION_FILE: &str = "/var/lib/ployz/substrate-version.json";
pub(super) const MACHINE_SUBSTRATE_LOCK_FILE: &str = "/var/lib/ployz/machine-substrate.lock";

#[derive(Clone)]
struct PrivilegedSubstrateLock {
    gate: Arc<tokio::sync::Mutex<()>>,
    owner: Arc<Mutex<Option<OperationId>>>,
}

pub(super) struct PrivilegedSubstrateGuard {
    _gate: tokio::sync::OwnedMutexGuard<()>,
    owner: Arc<Mutex<Option<OperationId>>>,
    operation_id: OperationId,
}

impl Drop for PrivilegedSubstrateGuard {
    fn drop(&mut self) {
        let mut owner = self
            .owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if owner.as_ref() == Some(&self.operation_id) {
            *owner = None;
        }
    }
}

fn privileged_substrate_lock() -> PrivilegedSubstrateLock {
    static LOCK: OnceLock<PrivilegedSubstrateLock> = OnceLock::new();
    LOCK.get_or_init(|| PrivilegedSubstrateLock {
        gate: Arc::new(tokio::sync::Mutex::new(())),
        owner: Arc::new(Mutex::new(None)),
    })
    .clone()
}

pub(super) fn try_privileged_substrate(
    operation_id: &OperationId,
) -> Result<PrivilegedSubstrateGuard, Option<OperationId>> {
    let lock = privileged_substrate_lock();
    let mut owner = lock
        .owner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let gate = lock.gate.try_lock_owned().map_err(|_| owner.clone())?;
    *owner = Some(operation_id.clone());
    drop(owner);
    Ok(PrivilegedSubstrateGuard {
        _gate: gate,
        owner: lock.owner,
        operation_id: operation_id.clone(),
    })
}

fn substrate_update_command(
    operation_id: &OperationId,
    version: &InstallArtifactVersion,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("flock");
    command
        .arg("--no-fork")
        .arg("--nonblock")
        .arg(MACHINE_SUBSTRATE_LOCK_FILE)
        .arg("ployz")
        .arg("host")
        .arg("substrate-update")
        .arg("--operation-id")
        .arg(operation_id.as_str())
        .arg("--version")
        .arg(version.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
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
    let machine_substrate_guard = match try_privileged_substrate(&request.operation_id) {
        Ok(guard) => guard,
        Err(owner) => {
            let owner = owner
                .as_ref()
                .map_or("another operation", OperationId::as_str);
            return machine_domain_error(MachineSubstrateUpdateRpcResponse::DomainError {
                machine_id,
                error: MachineSubstrateUpdateDomainError::UpdateFailed {
                    message: FailureMessage::try_new(format!(
                        "privileged machine substrate effect is owned by {owner}"
                    ))
                    .expect("busy message is non-empty"),
                },
            });
        }
    };
    let child = substrate_update_command(&request.operation_id, &request.target_version).spawn();
    let child = match child {
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
    tokio::spawn(supervise_substrate_update(
        machine_substrate_guard,
        child,
        MACHINE_SUBSTRATE_UPDATE_LEAK_BACKSTOP,
        MACHINE_SUBSTRATE_UPDATE_TERMINATION_GRACE,
    ));
    machine_success(MachineSubstrateUpdateRpcResponse::Ok(
        MachineSubstrateUpdateRpcOk { machine_id },
    ))
}

async fn supervise_substrate_update(
    _guard: PrivilegedSubstrateGuard,
    mut child: tokio::process::Child,
    leak_backstop: Duration,
    termination_grace: Duration,
) {
    if let Ok(Ok(_)) = tokio::time::timeout(leak_backstop, child.wait()).await {
        return;
    }
    let _ = tokio::time::timeout(termination_grace, async {
        let _ = child.kill().await;
        let _ = child.wait().await;
    })
    .await;
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
        FailureMessage::try_new(format!("failed to read {}: {error}", path.display()))
            .expect("substrate evidence read message is non-empty")
    })?;
    let evidence: SubstrateUpdateEvidence = serde_json::from_slice(&bytes).map_err(|error| {
        FailureMessage::try_new(format!("failed to decode {}: {error}", path.display()))
            .expect("substrate evidence decode message is non-empty")
    })?;
    if &evidence.operation_id != operation_id {
        return Ok(MachineSubstrateVersions::default());
    }
    Ok(MachineSubstrateVersions {
        ployzd: Some(evidence.ployzd),
        host_runner: None,
    })
}
