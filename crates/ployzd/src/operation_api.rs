//! User-facing operation service handlers.

use crate::controllers::OperationControllers;
use ployz_core::ids::OperationId;
use ployz_core::ops::{OperationEventReplayPage, OperationEventReplayRequest};
use ployz_nats::operations::{
    OperationEventLogError, OperationStatusStoreError, ReplayOperationEventsError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsStatusError {
    NoSuchOperation { operation_id: OperationId },
}

#[must_use]
pub fn ops_status_missing(operation_id: &OperationId) -> OpsStatusError {
    OpsStatusError::NoSuchOperation {
        operation_id: operation_id.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpsWatchError {
    NoSuchOperation {
        operation_id: OperationId,
    },
    Unavailable {
        operation_id: OperationId,
        source: OpsWatchUnavailableSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpsWatchUnavailableSource {
    StatusStore(OpsWatchStatusStoreFailure),
    EventLog(OpsWatchEventLogFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpsWatchStatusStoreFailure {
    OpenBucket,
    EncodeStatus,
    DecodeStatus,
    EncodeSubmission,
    DecodeSubmission,
    CasConflict,
    GetStatus,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpsWatchEventLogFailure {
    EncodeEvent,
    DecodeEvent,
    PublishRequest,
    PublishAck,
    ReadEvent,
    Timeout,
    InvalidAckSequence,
    InvalidNextReplaySequence,
}

impl OpsWatchError {
    #[must_use]
    pub fn from_replay_error(operation_id: OperationId, error: ReplayOperationEventsError) -> Self {
        match error {
            ReplayOperationEventsError::MissingOperation { operation_id } => {
                Self::NoSuchOperation { operation_id }
            }
            ReplayOperationEventsError::LoadStatus(source) => Self::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::StatusStore(status_store_failure(&source)),
            },
            ReplayOperationEventsError::ReadEvents(source) => Self::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::EventLog(event_log_failure(&source)),
            },
        }
    }
}

pub async fn ops_watch(
    controllers: &OperationControllers,
    request: OperationEventReplayRequest,
) -> Result<OperationEventReplayPage, OpsWatchError> {
    let operation_id = request.operation_id.clone();
    controllers
        .replay_operation_events(request)
        .await
        .map_err(|error| OpsWatchError::from_replay_error(operation_id, error))
}

fn status_store_failure(error: &OperationStatusStoreError) -> OpsWatchStatusStoreFailure {
    match error {
        OperationStatusStoreError::OpenBucket { .. } => OpsWatchStatusStoreFailure::OpenBucket,
        OperationStatusStoreError::EncodeStatus(_) => OpsWatchStatusStoreFailure::EncodeStatus,
        OperationStatusStoreError::DecodeStatus(_) => OpsWatchStatusStoreFailure::DecodeStatus,
        OperationStatusStoreError::EncodeSubmission(_) => {
            OpsWatchStatusStoreFailure::EncodeSubmission
        }
        OperationStatusStoreError::DecodeSubmission(_) => {
            OpsWatchStatusStoreFailure::DecodeSubmission
        }
        OperationStatusStoreError::CasConflict { .. } => OpsWatchStatusStoreFailure::CasConflict,
        OperationStatusStoreError::GetStatus { .. } => OpsWatchStatusStoreFailure::GetStatus,
        OperationStatusStoreError::Timeout { .. } => OpsWatchStatusStoreFailure::Timeout,
    }
}

fn event_log_failure(error: &OperationEventLogError) -> OpsWatchEventLogFailure {
    match error {
        OperationEventLogError::EncodeEvent(_) => OpsWatchEventLogFailure::EncodeEvent,
        OperationEventLogError::DecodeEvent(_) => OpsWatchEventLogFailure::DecodeEvent,
        OperationEventLogError::PublishRequest { .. } => OpsWatchEventLogFailure::PublishRequest,
        OperationEventLogError::PublishAck { .. } => OpsWatchEventLogFailure::PublishAck,
        OperationEventLogError::ReadEvent { .. } => OpsWatchEventLogFailure::ReadEvent,
        OperationEventLogError::Timeout { .. } => OpsWatchEventLogFailure::Timeout,
        OperationEventLogError::InvalidAckSequence { .. } => {
            OpsWatchEventLogFailure::InvalidAckSequence
        }
        OperationEventLogError::InvalidNextReplaySequence { .. } => {
            OpsWatchEventLogFailure::InvalidNextReplaySequence
        }
    }
}
