use mvp_bus::{BusSession, FactKey, IslandId, PrincipalId};
use mvp_p2panda_facts::{
    PandaFactError, PandaFactStore, PandaFactWireEnvelope, PandaFactWireEnvelopeError,
    PandaFactWriteOutcome,
};

use crate::{PandaNetStream, PandaNetTransportError};

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportOutcome {
    Imported,
    Duplicate,
    Conflict,
    Deferred(PandaNetFactImportDeferred),
    Failed(PandaNetFactImportFailure),
    Rejected(PandaNetFactImportRejection),
}

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportDeferred {
    OutOfOrder {
        island: IslandId,
        principal: PrincipalId,
        key: FactKey,
        missing_operations: u64,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportFailure {
    LocalIngest { message: String },
    LocalStore { message: String },
    InvalidStorePath { message: String },
    MissingPayload { key: FactKey },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportRejection {
    MalformedEnvelope(PandaFactWireEnvelopeError),
    UnauthorizedReplica {
        island: IslandId,
        principal: PrincipalId,
    },
    UntrustedAuthor {
        island: IslandId,
        principal: PrincipalId,
    },
    CrossIsland {
        session: IslandId,
        operation: IslandId,
    },
    InvalidOperation,
    InvalidExtensions,
}

pub async fn import_next_fact(
    stream: &mut PandaNetStream,
    store: &mut PandaFactStore,
    replica_session: &BusSession,
) -> Result<PandaNetFactImportOutcome, PandaNetTransportError> {
    let body = stream.next_body().await?;
    Ok(import_fact_body(&body, store, replica_session).await)
}

pub async fn import_fact_body(
    body: &[u8],
    store: &mut PandaFactStore,
    replica_session: &BusSession,
) -> PandaNetFactImportOutcome {
    let operation = match PandaFactWireEnvelope::decode(body) {
        Ok(operation) => operation,
        Err(error) => {
            return PandaNetFactImportOutcome::Rejected(
                PandaNetFactImportRejection::MalformedEnvelope(error),
            );
        }
    };

    match store
        .import_replica_operation(replica_session, &operation)
        .await
    {
        Ok(PandaFactWriteOutcome::Inserted(_)) => PandaNetFactImportOutcome::Imported,
        Ok(PandaFactWriteOutcome::AlreadyPresent(_)) => PandaNetFactImportOutcome::Duplicate,
        Ok(PandaFactWriteOutcome::Conflict(_)) => PandaNetFactImportOutcome::Conflict,
        Err(error) => classify_fact_error(error),
    }
}

fn classify_fact_error(error: PandaFactError) -> PandaNetFactImportOutcome {
    match error {
        PandaFactError::UnauthorizedReplicaImport { island, principal } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::UnauthorizedReplica {
                island,
                principal,
            })
        }
        PandaFactError::UntrustedAuthorKey {
            island, principal, ..
        } => PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::UntrustedAuthor {
            island,
            principal,
        }),
        PandaFactError::ImportIslandMismatch { session, operation } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::CrossIsland {
                session,
                operation,
            })
        }
        PandaFactError::InvalidOperation => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::InvalidOperation)
        }
        PandaFactError::InvalidExtensions { .. } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::InvalidExtensions)
        }
        PandaFactError::Ingest(error) => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::LocalIngest {
                message: error.to_string(),
            })
        }
        PandaFactError::Store { message } => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::LocalStore { message })
        }
        PandaFactError::InvalidStorePath { path, message } => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::InvalidStorePath {
                message: format!("{}: {message}", path.display()),
            })
        }
        PandaFactError::MissingPayload { key } => {
            PandaNetFactImportOutcome::Failed(PandaNetFactImportFailure::MissingPayload { key })
        }
        PandaFactError::OutOfOrderOperation {
            island,
            principal,
            key,
            missing_operations,
        } => PandaNetFactImportOutcome::Deferred(PandaNetFactImportDeferred::OutOfOrder {
            island,
            principal,
            key,
            missing_operations,
        }),
        PandaFactError::InvalidAuthorKey { .. }
        | PandaFactError::AuthorKeyMismatch { .. }
        | PandaFactError::OutdatedOperation { .. }
        | PandaFactError::PrincipalMismatch { .. }
        | PandaFactError::UnauthorizedWrite { .. } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::InvalidOperation)
        }
    }
}
