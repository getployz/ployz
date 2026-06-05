//! User-facing operation service handlers.

use crate::controllers::{DeploySubmitCommand, OperationControllers};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationOwnerLease,
    OperationStatusSnapshot,
};
use ployz_core::subjects::op_watch;
use ployz_nats::operations::{
    OperationEventLogError, OperationEventReplayReadError, OperationStatusReadError,
    OperationStatusStoreError, ReplayOperationEventsError,
    SubmitDeployError as SubmitDeployRepositoryError,
};
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitClockFailure, DeploySubmitError, DeploySubmitEventFailure,
    DeploySubmitRequest, DeploySubmitStatusFailure, DeploySubmitUnavailableSource,
    EventReplayFailure, OpsStatusError, OpsStatusUnavailableSource, OpsWatchError,
    OpsWatchUnavailableSource, StatusReadFailure,
};

#[must_use]
pub fn owned_operation(
    operation_id: OperationId,
    start_sequence: ployz_core::ops::EventSequence,
    lease: OperationOwnerLease,
) -> AcceptedOperation {
    let watch_subject = op_watch(&operation_id);
    AcceptedOperation {
        operation_id,
        watch_subject,
        start_sequence,
        owner_lease: lease,
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
        .map(|accepted| {
            owned_operation(
                accepted.operation_id,
                accepted.start_sequence,
                accepted.lease,
            )
        })
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
                failure: deploy_submit_event_failure(&source),
            },
        },
        SubmitDeployRepositoryError::StoreStatus(source) => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::StatusStore {
                failure: deploy_submit_status_failure(&source),
            },
        },
        SubmitDeployRepositoryError::Clock { .. } => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::Clock {
                failure: DeploySubmitClockFailure::BeforeUnixEpoch,
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

#[must_use]
pub fn ops_status_missing(operation_id: &OperationId) -> OpsStatusError {
    OpsStatusError::NoSuchOperation {
        operation_id: operation_id.clone(),
    }
}

pub async fn ops_status(
    controllers: &OperationControllers,
    operation_id: OperationId,
) -> Result<OperationStatusSnapshot, OpsStatusError> {
    match controllers.operation_status_snapshot(&operation_id).await {
        Ok(Some(snapshot)) => Ok(snapshot),
        Ok(None) => Err(ops_status_missing(&operation_id)),
        Err(error) => Err(OpsStatusError::Unavailable {
            operation_id,
            source: OpsStatusUnavailableSource::StatusStore {
                failure: status_store_read_failure(&error),
            },
        }),
    }
}

fn ops_watch_error_from_replay_error(
    operation_id: OperationId,
    error: ReplayOperationEventsError,
) -> OpsWatchError {
    match error {
        ReplayOperationEventsError::MissingOperation { operation_id } => {
            OpsWatchError::NoSuchOperation { operation_id }
        }
        ReplayOperationEventsError::LoadStatus(source) => OpsWatchError::Unavailable {
            operation_id,
            source: OpsWatchUnavailableSource::StatusStore {
                failure: status_read_failure(&source),
            },
        },
        ReplayOperationEventsError::ReadEvents(source) => OpsWatchError::Unavailable {
            operation_id,
            source: OpsWatchUnavailableSource::EventLog {
                failure: event_replay_failure(&source),
            },
        },
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
        .map_err(|error| ops_watch_error_from_replay_error(operation_id, error))
}

fn deploy_submit_status_failure(error: &OperationStatusStoreError) -> DeploySubmitStatusFailure {
    match error {
        OperationStatusStoreError::OpenBucket { .. } => DeploySubmitStatusFailure::OpenBucket,
        OperationStatusStoreError::EncodeStatus(_) => DeploySubmitStatusFailure::EncodeStatus,
        OperationStatusStoreError::DecodeStatus(_) => DeploySubmitStatusFailure::DecodeStatus,
        OperationStatusStoreError::EncodeSubmission(_) => {
            DeploySubmitStatusFailure::EncodeSubmission
        }
        OperationStatusStoreError::DecodeSubmission(_) => {
            DeploySubmitStatusFailure::DecodeSubmission
        }
        OperationStatusStoreError::EncodeLease(_) => DeploySubmitStatusFailure::EncodeLease,
        OperationStatusStoreError::DecodeLease(_) => DeploySubmitStatusFailure::DecodeLease,
        OperationStatusStoreError::CasConflict { .. } => DeploySubmitStatusFailure::CasConflict,
        OperationStatusStoreError::GetStatus { .. } => DeploySubmitStatusFailure::GetStatus,
        OperationStatusStoreError::Clock { .. } => DeploySubmitStatusFailure::Clock,
        OperationStatusStoreError::Timeout { .. } => DeploySubmitStatusFailure::Timeout,
    }
}

fn deploy_submit_event_failure(error: &OperationEventLogError) -> DeploySubmitEventFailure {
    match error {
        OperationEventLogError::EncodeEvent(_) => DeploySubmitEventFailure::EncodeEvent,
        OperationEventLogError::DecodeEvent(_) => DeploySubmitEventFailure::DecodeEvent,
        OperationEventLogError::PublishRequest { .. } => DeploySubmitEventFailure::PublishRequest,
        OperationEventLogError::PublishAck { .. } => DeploySubmitEventFailure::PublishAck,
        OperationEventLogError::ReadEvent { .. } => DeploySubmitEventFailure::ReadEvent,
        OperationEventLogError::Timeout { .. } => DeploySubmitEventFailure::Timeout,
        OperationEventLogError::InvalidAckSequence { .. } => {
            DeploySubmitEventFailure::InvalidAckSequence
        }
    }
}

fn status_read_failure(error: &OperationStatusReadError) -> StatusReadFailure {
    match error {
        OperationStatusReadError::DecodeStatus(_) => StatusReadFailure::DecodeStatus,
        OperationStatusReadError::GetStatus { .. } => StatusReadFailure::GetStatus,
        OperationStatusReadError::Timeout { .. } => StatusReadFailure::Timeout,
    }
}

fn status_store_read_failure(error: &OperationStatusStoreError) -> StatusReadFailure {
    match error {
        OperationStatusStoreError::DecodeStatus(_) => StatusReadFailure::DecodeStatus,
        OperationStatusStoreError::DecodeLease(_) => StatusReadFailure::DecodeLease,
        OperationStatusStoreError::GetStatus { .. } => StatusReadFailure::GetStatus,
        OperationStatusStoreError::Clock { .. } => StatusReadFailure::Clock,
        OperationStatusStoreError::Timeout { .. } => StatusReadFailure::Timeout,
        OperationStatusStoreError::OpenBucket { .. }
        | OperationStatusStoreError::EncodeStatus(_)
        | OperationStatusStoreError::EncodeSubmission(_)
        | OperationStatusStoreError::DecodeSubmission(_)
        | OperationStatusStoreError::EncodeLease(_)
        | OperationStatusStoreError::CasConflict { .. } => StatusReadFailure::GetStatus,
    }
}

fn event_replay_failure(error: &OperationEventReplayReadError) -> EventReplayFailure {
    match error {
        OperationEventReplayReadError::DecodeEvent(_) => EventReplayFailure::DecodeEvent,
        OperationEventReplayReadError::ReadEvent { .. } => EventReplayFailure::ReadEvent,
        OperationEventReplayReadError::Timeout { .. } => EventReplayFailure::Timeout,
        OperationEventReplayReadError::InvalidEventSequence { .. } => {
            EventReplayFailure::InvalidEventSequence
        }
        OperationEventReplayReadError::InvalidNextReplaySequence { .. } => {
            EventReplayFailure::InvalidNextReplaySequence
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        deploy_submit_error_from_submit_error, ops_watch_error_from_replay_error,
        status_read_failure,
    };
    use ployz_core::ids::OperationId;
    use ployz_core::ops::EventSequence;
    use ployz_nats::operations::{
        OperationEventLogError, OperationEventReplayReadError, OperationStatusReadError,
        OperationStatusStoreError, ReplayOperationEventsError,
        SubmitDeployError as SubmitDeployRepositoryError,
    };
    use ployz_sdk_types::{
        DeploySubmitError, DeploySubmitEventFailure, DeploySubmitStatusFailure,
        DeploySubmitUnavailableSource, EventReplayFailure, OpsWatchError,
        OpsWatchUnavailableSource, StatusReadFailure,
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
                    failure: DeploySubmitStatusFailure::CasConflict,
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
                    failure: DeploySubmitEventFailure::PublishRequest,
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
            ops_watch_error_from_replay_error(
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
            ops_watch_error_from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::LoadStatus(OperationStatusReadError::GetStatus {
                    message: "kv unavailable".to_owned(),
                }),
            ),
            OpsWatchError::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::StatusStore {
                    failure: StatusReadFailure::GetStatus,
                },
            }
        );
    }

    #[test]
    fn ops_watch_preserves_event_log_failure_context() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            ops_watch_error_from_replay_error(
                operation_id.clone(),
                ReplayOperationEventsError::ReadEvents(OperationEventReplayReadError::ReadEvent {
                    message: "stream unavailable".to_owned(),
                }),
            ),
            OpsWatchError::Unavailable {
                operation_id,
                source: OpsWatchUnavailableSource::EventLog {
                    failure: EventReplayFailure::ReadEvent,
                },
            }
        );
    }

    #[test]
    fn ops_status_preserves_status_store_failure_context() {
        assert_eq!(
            status_read_failure(&OperationStatusReadError::Timeout { operation: "test" }),
            StatusReadFailure::Timeout
        );
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn event_sequence(value: u64) -> EventSequence {
        EventSequence::try_new(value).expect("valid event sequence")
    }
}
