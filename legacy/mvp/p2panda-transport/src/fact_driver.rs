use mvp_bus::{BusSession, FactKey, IslandId, PrincipalId};
use mvp_p2panda_facts::{
    PandaFactError, PandaFactExtensions, PandaFactWriteOutcome, SharedPandaFactStore,
};
use p2panda_core::{Operation, Topic};

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
pub struct PandaNetFactImportReport {
    pub attempted: usize,
    pub imported: usize,
    pub duplicate: usize,
    pub conflict: usize,
    pub deferred: Vec<PandaNetFactImportDeferred>,
    pub rejected: Vec<PandaNetFactImportRejection>,
    pub failed: Vec<PandaNetFactImportFailure>,
}

impl PandaNetFactImportReport {
    #[must_use]
    pub fn new(attempted: usize) -> Self {
        Self {
            attempted,
            imported: 0,
            duplicate: 0,
            conflict: 0,
            deferred: Vec::new(),
            rejected: Vec::new(),
            failed: Vec::new(),
        }
    }

    pub fn record(&mut self, outcome: PandaNetFactImportOutcome) {
        match outcome {
            PandaNetFactImportOutcome::Imported => {
                self.imported += 1;
            }
            PandaNetFactImportOutcome::Duplicate => {
                self.duplicate += 1;
            }
            PandaNetFactImportOutcome::Conflict => {
                self.conflict += 1;
            }
            PandaNetFactImportOutcome::Deferred(deferred) => {
                self.deferred.push(deferred);
            }
            PandaNetFactImportOutcome::Rejected(rejected) => {
                self.rejected.push(rejected);
            }
            PandaNetFactImportOutcome::Failed(failed) => {
                self.failed.push(failed);
            }
        }
    }
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
    PendingQueueFull { max: usize },
}

#[derive(Debug, PartialEq, Eq)]
pub enum PandaNetFactImportRejection {
    MalformedOperation {
        message: String,
    },
    OperationTooLarge {
        size: usize,
        max: usize,
    },
    UnauthorizedReplica {
        island: IslandId,
        principal: PrincipalId,
    },
    UntrustedAuthor {
        island: IslandId,
        principal: PrincipalId,
    },
    AuthorKeyMismatch {
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

pub(crate) async fn import_p2panda_operation_into_shared_store(
    operation: Operation<PandaFactExtensions>,
    store: &SharedPandaFactStore,
    replica_session: &BusSession,
    topic: Topic,
) -> PandaNetFactImportOutcome {
    if operation.body().is_none() {
        return PandaNetFactImportOutcome::Rejected(
            PandaNetFactImportRejection::MalformedOperation {
                message: "p2panda operation has no payload body".to_string(),
            },
        );
    }
    match store
        .import_replica_p2panda_operation(replica_session, topic, operation)
        .await
    {
        Ok(outcome) => classify_write_outcome(outcome),
        Err(error) => classify_fact_error(error),
    }
}

fn classify_write_outcome(outcome: PandaFactWriteOutcome) -> PandaNetFactImportOutcome {
    match outcome {
        PandaFactWriteOutcome::Inserted(_) => PandaNetFactImportOutcome::Imported,
        PandaFactWriteOutcome::AlreadyPresent(_) => PandaNetFactImportOutcome::Duplicate,
        PandaFactWriteOutcome::Conflict(_) => PandaNetFactImportOutcome::Conflict,
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
        PandaFactError::AuthorKeyMismatch { island, principal } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::AuthorKeyMismatch {
                island,
                principal,
            })
        }
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
        | PandaFactError::InvalidAuthorPrivateKey { .. }
        | PandaFactError::PrincipalMismatch { .. }
        | PandaFactError::UnauthorizedWrite { .. } => {
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::InvalidOperation)
        }
    }
}
