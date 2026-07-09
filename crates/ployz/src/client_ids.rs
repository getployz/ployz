//! Client-side operation IDs for ergonomic commands.

use ployz_core::ids::{MachineId, NamespaceId, OperationId, ServiceId, SubjectTokenError};
use ployz_core::ops::OperationIdempotencyKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientGeneratedOperationId {
    pub suffix: String,
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientGeneratedDeployId {
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientGeneratedMachineAddIds {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

/// Collision-resistant, readable IDs tied to the command intent.
pub(crate) fn generate_client_deploy_id(
    service_id: &ServiceId,
) -> Result<ClientGeneratedDeployId, ClientGeneratedIdsError> {
    let suffix = generated_id_suffix();
    Ok(ClientGeneratedDeployId {
        idempotency_key: OperationIdempotencyKey::try_new(format!(
            "idem_deploy_{}_{}",
            service_id.as_str(),
            suffix
        ))
        .map_err(|source| ClientGeneratedIdsError::IdempotencyKey { source })?,
    })
}

pub(crate) fn generate_client_machine_add_ids(
    machine_id: &MachineId,
) -> Result<ClientGeneratedMachineAddIds, ClientGeneratedIdsError> {
    let generated = generate_client_operation_id("add", machine_id.as_str())?;
    Ok(ClientGeneratedMachineAddIds {
        idempotency_key: OperationIdempotencyKey::try_new(format!(
            "idem_add_{}_{}",
            machine_id.as_str(),
            generated.suffix
        ))
        .map_err(|source| ClientGeneratedIdsError::IdempotencyKey { source })?,
        operation_id: generated.operation_id,
    })
}

pub(crate) fn generate_client_machine_lifecycle_id(
    action: &'static str,
    machine_id: &MachineId,
) -> Result<ClientGeneratedOperationId, ClientGeneratedIdsError> {
    generate_client_operation_id(action, machine_id.as_str())
}

pub(crate) fn generate_client_machine_update_id(
    machine_id: &MachineId,
) -> Result<ClientGeneratedOperationId, ClientGeneratedIdsError> {
    generate_client_operation_id("update", machine_id.as_str())
}

pub(crate) fn generate_client_service_restart_id(
    service_id: &ServiceId,
) -> Result<ClientGeneratedOperationId, ClientGeneratedIdsError> {
    generate_client_operation_id("restart", service_id.as_str())
}

pub(crate) fn generate_client_namespace_remove_id(
    namespace_id: &NamespaceId,
) -> Result<ClientGeneratedOperationId, ClientGeneratedIdsError> {
    generate_client_operation_id("namespace_rm", namespace_id.as_str())
}

pub(crate) fn generate_client_core_replace_id(
    machine_id: &MachineId,
) -> Result<ClientGeneratedOperationId, ClientGeneratedIdsError> {
    generate_client_operation_id("core_replace", machine_id.as_str())
}

fn generate_client_operation_id(
    action: &'static str,
    subject: &str,
) -> Result<ClientGeneratedOperationId, ClientGeneratedIdsError> {
    let suffix = generated_id_suffix();
    Ok(ClientGeneratedOperationId {
        operation_id: OperationId::try_new(format!("op_{action}_{subject}_{suffix}"))
            .map_err(|source| ClientGeneratedIdsError::OperationId { source })?,
        suffix,
    })
}

fn generated_id_suffix() -> String {
    nuid::next().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ClientGeneratedIdsError {
    #[error("generated operation id is invalid: {source}")]
    OperationId { source: SubjectTokenError },
    #[error("generated idempotency key is invalid: {source}")]
    IdempotencyKey { source: SubjectTokenError },
}
