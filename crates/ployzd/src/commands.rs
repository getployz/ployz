use std::time::SystemTime;

use ployz::PrimitiveFailure;
use ployz::operation::{
    OperationEvent, OperationEventCursor, OperationId, OperationKind, OperationLedgerPort,
    OperationLiveness, OperationOwner, OperationRecord, OperationSubmission,
};
use thiserror::Error;

use crate::operations::{OperationLeasePolicy, OperationRegistry, OperationRunner};
use crate::report::StartupReport;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

#[derive(Clone)]
pub struct DaemonCommandService<L> {
    startup_report: StartupReport,
    ledger: L,
    runner: OperationRunner<L>,
}

impl<L> DaemonCommandService<L>
where
    L: OperationLedgerPort + Clone + Send + Sync + 'static,
{
    #[must_use]
    pub(crate) fn new(
        startup_report: StartupReport,
        ledger: L,
        owner: OperationOwner,
        registry: OperationRegistry,
        lease_policy: OperationLeasePolicy,
        shutting_down: Arc<AtomicBool>,
    ) -> Self {
        let runner =
            OperationRunner::new(ledger.clone(), registry, owner, lease_policy, shutting_down);
        Self {
            startup_report,
            ledger,
            runner,
        }
    }

    pub async fn handle(
        &self,
        request: DaemonCommandRequest,
    ) -> Result<DaemonCommandResponse, DaemonCommandError> {
        match request {
            DaemonCommandRequest::Status => Ok(DaemonCommandResponse::Status(self.status())),
            DaemonCommandRequest::OperationSubmit(submission) => self
                .submit_operation(submission)
                .await
                .map(DaemonCommandResponse::OperationSubmitted),
            DaemonCommandRequest::OperationGet(operation) => self
                .get_operation(&operation)
                .await
                .map(DaemonCommandResponse::OperationStatus),
            DaemonCommandRequest::OperationStream { operation, cursor } => self
                .stream_operation(&operation, cursor.as_ref())
                .await
                .map(DaemonCommandResponse::OperationEvents),
        }
    }

    #[must_use]
    pub fn status(&self) -> DaemonStatus {
        DaemonStatus {
            startup_report: self.startup_report.clone(),
            command_service_ready: !self.runner.is_shutting_down(),
            last_operation_failure: self.runner.last_failure().map(DaemonOperationFailure::from),
        }
    }

    pub async fn submit_operation(
        &self,
        submission: OperationSubmission,
    ) -> Result<OperationId, DaemonCommandError> {
        if !self.runner.supports(submission.kind()) {
            return Err(DaemonCommandError::UnsupportedOperation {
                kind: submission.kind().clone(),
            });
        }
        self.runner
            .submit(submission)
            .await
            .map_err(DaemonCommandError::operation)
    }

    pub async fn get_operation(
        &self,
        operation: &OperationId,
    ) -> Result<OperationStatusResponse, DaemonCommandError> {
        let record = self
            .ledger
            .get(operation)
            .await
            .map_err(DaemonCommandError::operation)?
            .ok_or_else(|| DaemonCommandError::OperationNotFound {
                operation: operation.clone(),
            })?;
        Ok(OperationStatusResponse {
            liveness: record.liveness_at(SystemTime::now()),
            record,
        })
    }

    pub async fn stream_operation(
        &self,
        operation: &OperationId,
        cursor: Option<&OperationEventCursor>,
    ) -> Result<Vec<OperationEvent>, DaemonCommandError> {
        self.ledger
            .get(operation)
            .await
            .map_err(DaemonCommandError::operation)?
            .ok_or_else(|| DaemonCommandError::OperationNotFound {
                operation: operation.clone(),
            })?;
        self.ledger
            .events_after(operation, cursor)
            .await
            .map_err(DaemonCommandError::operation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonCommandRequest {
    Status,
    OperationSubmit(OperationSubmission),
    OperationGet(OperationId),
    OperationStream {
        operation: OperationId,
        cursor: Option<OperationEventCursor>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonCommandResponse {
    Status(DaemonStatus),
    OperationSubmitted(OperationId),
    OperationStatus(OperationStatusResponse),
    OperationEvents(Vec<OperationEvent>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatus {
    startup_report: StartupReport,
    command_service_ready: bool,
    last_operation_failure: Option<DaemonOperationFailure>,
}

impl DaemonStatus {
    #[must_use]
    pub fn startup_report(&self) -> &StartupReport {
        &self.startup_report
    }

    #[must_use]
    pub fn command_service_ready(&self) -> bool {
        self.command_service_ready
    }

    #[must_use]
    pub fn last_operation_failure(&self) -> Option<&DaemonOperationFailure> {
        self.last_operation_failure.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonOperationFailure {
    operation: OperationId,
    source: PrimitiveFailure,
}

impl DaemonOperationFailure {
    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn source(&self) -> &PrimitiveFailure {
        &self.source
    }
}

impl From<crate::operations::OperationRunnerFailure> for DaemonOperationFailure {
    fn from(value: crate::operations::OperationRunnerFailure) -> Self {
        Self {
            operation: value.operation().clone(),
            source: value.source().clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStatusResponse {
    record: OperationRecord,
    liveness: OperationLiveness,
}

impl OperationStatusResponse {
    #[must_use]
    pub fn record(&self) -> &OperationRecord {
        &self.record
    }

    #[must_use]
    pub fn liveness(&self) -> &OperationLiveness {
        &self.liveness
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DaemonCommandError {
    #[error("operation kind {kind:?} is not supported")]
    UnsupportedOperation { kind: OperationKind },

    #[error("operation {operation:?} was not found")]
    OperationNotFound { operation: OperationId },

    #[error("operation command failed: {source}")]
    Operation { source: PrimitiveFailure },
}

impl DaemonCommandError {
    fn operation(source: PrimitiveFailure) -> Self {
        Self::Operation { source }
    }
}
