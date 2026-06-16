//! Client-side operation/idempotency IDs for ergonomic commands.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::ids::{NodeId, OperationId, ServiceId, SubjectTokenError};
use ployz_core::ops::OperationIdempotencyKey;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientGeneratedIds {
    pub suffix: String,
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientOperationKind<'id> {
    Deploy { service_id: &'id ServiceId },
    MachineAdd { node_id: &'id NodeId },
}

impl<'id> ClientOperationKind<'id> {
    const fn action(self) -> &'static str {
        match self {
            Self::Deploy { .. } => "deploy",
            Self::MachineAdd { .. } => "add",
        }
    }

    fn subject(self) -> &'id str {
        match self {
            Self::Deploy { service_id } => service_id.as_str(),
            Self::MachineAdd { node_id } => node_id.as_str(),
        }
    }
}

/// Collision-resistant, readable IDs tied to the command intent.
pub(crate) fn generate_client_operation_ids(
    kind: ClientOperationKind<'_>,
) -> Result<ClientGeneratedIds, ClientGeneratedIdsError> {
    let suffix = generated_id_suffix();
    let action = kind.action();
    let subject = kind.subject();
    Ok(ClientGeneratedIds {
        operation_id: OperationId::try_new(format!("op_{action}_{subject}_{suffix}"))
            .map_err(|source| ClientGeneratedIdsError::OperationId { source })?,
        idempotency_key: OperationIdempotencyKey::try_new(format!(
            "idem_{action}_{subject}_{suffix}"
        ))
        .map_err(|source| ClientGeneratedIdsError::IdempotencyKey { source })?,
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
