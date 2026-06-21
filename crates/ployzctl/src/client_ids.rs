//! Client-side operation IDs for ergonomic commands.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::ids::{NodeId, OperationId, ServiceId, SubjectTokenError};
use ployz_core::ops::OperationIdempotencyKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientGeneratedOperationId {
    pub suffix: String,
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientGeneratedMachineAddIds {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

/// Collision-resistant, readable IDs tied to the command intent.
pub(crate) fn generate_client_deploy_id(
    service_id: &ServiceId,
) -> Result<ClientGeneratedOperationId, ClientGeneratedIdsError> {
    generate_client_operation_id("deploy", service_id.as_str())
}

pub(crate) fn generate_client_machine_add_ids(
    node_id: &NodeId,
) -> Result<ClientGeneratedMachineAddIds, ClientGeneratedIdsError> {
    let generated = generate_client_operation_id("add", node_id.as_str())?;
    Ok(ClientGeneratedMachineAddIds {
        idempotency_key: OperationIdempotencyKey::try_new(format!(
            "idem_add_{}_{}",
            node_id.as_str(),
            generated.suffix
        ))
        .map_err(|source| ClientGeneratedIdsError::IdempotencyKey { source })?,
        operation_id: generated.operation_id,
    })
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
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{sequence}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientGeneratedIdsError {
    OperationId { source: SubjectTokenError },
    IdempotencyKey { source: SubjectTokenError },
}

impl fmt::Display for ClientGeneratedIdsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperationId { source } => {
                write!(formatter, "generated operation id is invalid: {source}")
            }
            Self::IdempotencyKey { source } => {
                write!(formatter, "generated idempotency key is invalid: {source}")
            }
        }
    }
}

impl std::error::Error for ClientGeneratedIdsError {}
