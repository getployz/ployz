use std::future::Future;
use std::time::SystemTime;

use crate::error::PrimitiveFailure;

use super::event::{OperationEvent, OperationEventCursor, OperationEventKind};
use super::identity::{OperationId, OperationOwner};
use super::lease::{OperationLease, OperationLeaseEpoch};
use super::status::{OperationRecord, OperationStage, OperationSubmission, OperationTerminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationSubmitOutcome {
    Created,
    Replayed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSubmitResult {
    record: OperationRecord,
    outcome: OperationSubmitOutcome,
}

impl OperationSubmitResult {
    #[must_use]
    pub fn created(record: OperationRecord) -> Self {
        Self {
            record,
            outcome: OperationSubmitOutcome::Created,
        }
    }

    #[must_use]
    pub fn replayed(record: OperationRecord) -> Self {
        Self {
            record,
            outcome: OperationSubmitOutcome::Replayed,
        }
    }

    #[must_use]
    pub fn record(&self) -> &OperationRecord {
        &self.record
    }

    #[must_use]
    pub fn outcome(&self) -> OperationSubmitOutcome {
        self.outcome
    }

    #[must_use]
    pub fn into_record(self) -> OperationRecord {
        self.record
    }
}

pub trait OperationLedgerPort {
    fn submit<'a>(
        &'a self,
        submission: &'a OperationSubmission,
        lease: &'a OperationLease,
        initial_stage: &'a OperationStage,
        now: SystemTime,
    ) -> impl Future<Output = Result<OperationSubmitResult, PrimitiveFailure>> + Send + 'a;

    fn get<'a>(
        &'a self,
        operation: &'a OperationId,
    ) -> impl Future<Output = Result<Option<OperationRecord>, PrimitiveFailure>> + Send + 'a;

    fn append_event_kind<'a>(
        &'a self,
        operation: &'a OperationId,
        kind: &'a OperationProgressEvent,
        owner: &'a OperationOwner,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> impl Future<Output = Result<OperationEvent, PrimitiveFailure>> + Send + 'a;

    fn events_after<'a>(
        &'a self,
        operation: &'a OperationId,
        cursor: Option<&'a OperationEventCursor>,
    ) -> impl Future<Output = Result<Vec<OperationEvent>, PrimitiveFailure>> + Send + 'a;

    fn refresh_lease<'a>(
        &'a self,
        operation: &'a OperationId,
        owner: &'a OperationOwner,
        epoch: OperationLeaseEpoch,
        renewed_lease: &'a OperationLease,
    ) -> impl Future<Output = Result<(), PrimitiveFailure>> + Send + 'a;

    fn write_terminal<'a>(
        &'a self,
        operation: &'a OperationId,
        terminal: &'a OperationTerminal,
        owner: &'a OperationOwner,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> impl Future<Output = Result<OperationRecord, PrimitiveFailure>> + Send + 'a;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationProgressEvent {
    Stage(OperationStage),
}

impl OperationProgressEvent {
    #[must_use]
    pub fn stage(stage: OperationStage) -> Self {
        Self::Stage(stage)
    }

    #[must_use]
    pub fn into_event_kind(self) -> OperationEventKind {
        match self {
            Self::Stage(stage) => OperationEventKind::stage(stage),
        }
    }
}
