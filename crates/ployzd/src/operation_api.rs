//! User-facing operation service handlers.

use crate::controllers::{DeploySubmitCommand, OperationControllers};
use ployz_core::ids::OperationId;
use ployz_core::ops::{OperationEventReplayPage, OperationEventReplayRequest};
use ployz_core::subjects::op_watch;
use ployz_nats::operations::{
    OperationEventLogError, OperationStatusStoreError, ReplayOperationEventsError,
    SubmitDeployError as SubmitDeployRepositoryError,
};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitError, DeploySubmitEventLogFailure, DeploySubmitRequest,
    DeploySubmitStatusStoreFailure, DeploySubmitUnavailableSource, OperationDispatch,
};

#[must_use]
pub fn queued_operation(
    operation_id: OperationId,
    start_sequence: ployz_core::ops::EventSequence,
) -> AcceptedOperation {
    let watch_subject = op_watch(&operation_id);
    AcceptedOperation {
        operation_id,
        dispatch: OperationDispatch::Queued {
            watch_subject,
            start_sequence,
        },
    }
}

impl From<DeploySubmitRequest> for DeploySubmitCommand {
    fn from(value: DeploySubmitRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            idempotency_key: value.idempotency_key,
            service_id: value.service_id,
        }
    }
}

pub async fn deploy_submit(
    controllers: &OperationControllers,
    command: DeploySubmitCommand,
) -> Result<AcceptedOperation, DeploySubmitError> {
    let operation_id = command.operation_id.clone();
    controllers
        .submit_deploy(command)
        .await
        .map(|accepted| queued_operation(accepted.operation_id, accepted.start_sequence))
        .map_err(|error| deploy_submit_error_from_submit_error(operation_id, error))
}

fn deploy_submit_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitDeployRepositoryError,
) -> DeploySubmitError {
    match error {
        SubmitDeployRepositoryError::AppendEvent(source) => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::EventLog {
                failure: deploy_submit_event_log_failure(&source),
            },
        },
        SubmitDeployRepositoryError::StoreStatus(source) => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::StatusStore {
                failure: deploy_submit_status_store_failure(&source),
            },
        },
        SubmitDeployRepositoryError::DuplicateSequenceMismatch { sequence } => {
            DeploySubmitError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
    }
}

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
    fn from_replay_error(operation_id: OperationId, error: ReplayOperationEventsError) -> Self {
        match error {
            ReplayOperationEventsError::MissingOperation { operation_id } => {
                Self::NoSuchOperation { operation_id }
            }
            ReplayOperationEventsError::LoadStatus(source) => Self::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::StatusStore(ops_watch_status_store_failure(
                    &source,
                )),
            },
            ReplayOperationEventsError::ReadEvents(source) => Self::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::EventLog(ops_watch_event_log_failure(&source)),
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

fn deploy_submit_status_store_failure(
    error: &OperationStatusStoreError,
) -> DeploySubmitStatusStoreFailure {
    match error {
        OperationStatusStoreError::OpenBucket { .. } => DeploySubmitStatusStoreFailure::OpenBucket,
        OperationStatusStoreError::EncodeStatus(_) => DeploySubmitStatusStoreFailure::EncodeStatus,
        OperationStatusStoreError::DecodeStatus(_) => DeploySubmitStatusStoreFailure::DecodeStatus,
        OperationStatusStoreError::EncodeSubmission(_) => {
            DeploySubmitStatusStoreFailure::EncodeSubmission
        }
        OperationStatusStoreError::DecodeSubmission(_) => {
            DeploySubmitStatusStoreFailure::DecodeSubmission
        }
        OperationStatusStoreError::CasConflict { .. } => {
            DeploySubmitStatusStoreFailure::CasConflict
        }
        OperationStatusStoreError::GetStatus { .. } => DeploySubmitStatusStoreFailure::GetStatus,
        OperationStatusStoreError::Timeout { .. } => DeploySubmitStatusStoreFailure::Timeout,
    }
}

fn deploy_submit_event_log_failure(error: &OperationEventLogError) -> DeploySubmitEventLogFailure {
    match error {
        OperationEventLogError::EncodeEvent(_) => DeploySubmitEventLogFailure::EncodeEvent,
        OperationEventLogError::DecodeEvent(_) => DeploySubmitEventLogFailure::DecodeEvent,
        OperationEventLogError::PublishRequest { .. } => {
            DeploySubmitEventLogFailure::PublishRequest
        }
        OperationEventLogError::PublishAck { .. } => DeploySubmitEventLogFailure::PublishAck,
        OperationEventLogError::ReadEvent { .. } => DeploySubmitEventLogFailure::ReadEvent,
        OperationEventLogError::Timeout { .. } => DeploySubmitEventLogFailure::Timeout,
        OperationEventLogError::InvalidAckSequence { .. } => {
            DeploySubmitEventLogFailure::InvalidAckSequence
        }
        OperationEventLogError::InvalidNextReplaySequence { .. } => {
            DeploySubmitEventLogFailure::InvalidNextReplaySequence
        }
    }
}

fn ops_watch_status_store_failure(error: &OperationStatusStoreError) -> OpsWatchStatusStoreFailure {
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

fn ops_watch_event_log_failure(error: &OperationEventLogError) -> OpsWatchEventLogFailure {
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

#[cfg(test)]
mod tests {
    use super::{
        OpsWatchError, OpsWatchEventLogFailure, OpsWatchStatusStoreFailure,
        OpsWatchUnavailableSource, deploy_submit_error_from_submit_error,
    };
    use ployz_core::ids::OperationId;
    use ployz_core::ops::EventSequence;
    use ployz_nats::operations::{
        OperationEventLogError, OperationStatusStoreError, ReplayOperationEventsError,
        SubmitDeployError as SubmitDeployRepositoryError,
    };
    use ployz_sdk_types::{
        DeploySubmitError, DeploySubmitEventLogFailure, DeploySubmitStatusStoreFailure,
        DeploySubmitUnavailableSource,
    };

    #[test]
    fn deploy_submit_maps_status_store_failure_to_api_error() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                operation_id.clone(),
                SubmitDeployRepositoryError::StoreStatus(OperationStatusStoreError::CasConflict {
                    message: "contended".to_owned(),
                }),
            ),
            DeploySubmitError::Unavailable {
                operation_id,
                source: DeploySubmitUnavailableSource::StatusStore {
                    failure: DeploySubmitStatusStoreFailure::CasConflict,
                },
            }
        );
    }

    #[test]
    fn deploy_submit_maps_event_log_failure_to_api_error() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                operation_id.clone(),
                SubmitDeployRepositoryError::AppendEvent(OperationEventLogError::PublishRequest {
                    message: "publish unavailable".to_owned(),
                }),
            ),
            DeploySubmitError::Unavailable {
                operation_id,
                source: DeploySubmitUnavailableSource::EventLog {
                    failure: DeploySubmitEventLogFailure::PublishRequest,
                },
            }
        );
    }

    #[test]
    fn deploy_submit_preserves_duplicate_sequence_mismatch() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                operation_id.clone(),
                SubmitDeployRepositoryError::DuplicateSequenceMismatch {
                    sequence: event_sequence(9),
                },
            ),
            DeploySubmitError::DuplicateSequenceMismatch {
                operation_id,
                sequence: event_sequence(9),
            }
        );
    }

    #[test]
    fn ops_watch_maps_missing_operation_to_api_error() {
        let operation_id = operation_id("op_missing");

        assert_eq!(
            OpsWatchError::from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::MissingOperation {
                    operation_id: operation_id.clone(),
                },
            ),
            OpsWatchError::NoSuchOperation { operation_id }
        );
    }

    #[test]
    fn ops_watch_preserves_status_store_failure_context() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            OpsWatchError::from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::LoadStatus(OperationStatusStoreError::GetStatus {
                    message: "kv unavailable".to_owned(),
                }),
            ),
            OpsWatchError::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::StatusStore(
                    OpsWatchStatusStoreFailure::GetStatus,
                ),
            }
        );
    }

    #[test]
    fn ops_watch_preserves_event_log_failure_context() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            OpsWatchError::from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::ReadEvents(OperationEventLogError::ReadEvent {
                    message: "stream unavailable".to_owned(),
                }),
            ),
            OpsWatchError::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::EventLog(OpsWatchEventLogFailure::ReadEvent),
            }
        );
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn event_sequence(value: u64) -> EventSequence {
        EventSequence::try_new(value).expect("valid event sequence")
    }
}
