//! Mapping from repository/controller failures to the client-visible
//! operation API error types. Pure functions; no I/O.

use crate::controllers::{
    MachineAddBootstrapMaterialError, MachineAddSubmitCommandError, SubmitCommandError,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    EventSequence, FailureMessage, ProjectionOperationState, StatusProjectionError,
};
use ployz_nats::operations::{
    OperationEventLogError, OperationEventReplayReadError, OperationStatusReadError,
    OperationStatusStoreError, RecordMachineAddEventError, RecordMachineJoinReportError,
    RedeemMachineJoinTokenError as RedeemMachineJoinTokenRepositoryError,
    ReplayOperationEventsError, SubmitOperationError,
};
use ployz_sdk_types::{
    BootstrapMaterialFailure, DeploySubmitError, EventReplayFailure, MachineAddError,
    MachineAddUnavailableSource, MachineJoinRedeemError, MachineJoinRedeemUnavailableSource,
    MachineJoinReportError, MachineJoinReportUnavailableSource, OperationSubmitClockFailure,
    OperationSubmitEventFailure, OperationSubmitStatusFailure, OperationSubmitUnavailableSource,
    OpsWatchError, OpsWatchUnavailableSource, StatusReadFailure,
};

/// The endpoint-independent core of a submit command failure: either an
/// unavailable source or the duplicate-sequence collision.
enum SubmitFailure {
    InvalidDeployTarget,
    Unavailable(OperationSubmitUnavailableSource),
    DuplicateSequenceMismatch { sequence: EventSequence },
}

fn submit_failure(error: SubmitCommandError) -> SubmitFailure {
    match error {
        SubmitCommandError::Submit(SubmitOperationError::InvalidDeployTarget) => {
            SubmitFailure::InvalidDeployTarget
        }
        SubmitCommandError::Submit(SubmitOperationError::AppendEvent(source)) => {
            SubmitFailure::Unavailable(OperationSubmitUnavailableSource::EventLog {
                failure: operation_submit_event_failure(&source),
            })
        }
        SubmitCommandError::Submit(SubmitOperationError::StoreStatus(source)) => {
            SubmitFailure::Unavailable(OperationSubmitUnavailableSource::StatusStore {
                failure: operation_submit_status_failure(&source),
            })
        }
        SubmitCommandError::Submit(SubmitOperationError::DuplicateSequenceMismatch {
            sequence,
        }) => SubmitFailure::DuplicateSequenceMismatch { sequence },
    }
}

pub(super) fn deploy_submit_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitCommandError,
) -> DeploySubmitError {
    match submit_failure(error) {
        SubmitFailure::InvalidDeployTarget => DeploySubmitError::InvalidTarget {
            operation_id,
            message: FailureMessage::try_new("deploy target must include at least one service")
                .expect("static deploy target failure message is non-empty"),
        },
        SubmitFailure::Unavailable(source) => DeploySubmitError::Unavailable {
            operation_id,
            source,
        },
        SubmitFailure::DuplicateSequenceMismatch { sequence } => {
            DeploySubmitError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
    }
}

pub(super) fn machine_add_error_from_submit_error(
    operation_id: OperationId,
    error: MachineAddSubmitCommandError,
) -> MachineAddError {
    let submit = match error {
        MachineAddSubmitCommandError::JoinTokenMismatch => {
            return MachineAddError::Unavailable {
                operation_id,
                source: MachineAddUnavailableSource::BootstrapMaterial {
                    failure: BootstrapMaterialFailure::IssueJoinToken,
                },
            };
        }
        MachineAddSubmitCommandError::DuplicateIdempotencyKey => {
            return MachineAddError::DuplicateIdempotencyKey { operation_id };
        }
        MachineAddSubmitCommandError::Submit(error) => error,
    };
    match submit_failure(submit) {
        SubmitFailure::InvalidDeployTarget => {
            unreachable!("machine add submit is not deploy target")
        }
        SubmitFailure::Unavailable(source) => MachineAddError::Unavailable {
            operation_id,
            source: source.into(),
        },
        SubmitFailure::DuplicateSequenceMismatch { sequence } => {
            MachineAddError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
    }
}

pub(super) fn bootstrap_material_failure(
    error: MachineAddBootstrapMaterialError,
) -> BootstrapMaterialFailure {
    match error {
        MachineAddBootstrapMaterialError::Clock { .. }
        | MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial => {
            BootstrapMaterialFailure::IssueJoinToken
        }
        MachineAddBootstrapMaterialError::MissingJoinTemplate => {
            BootstrapMaterialFailure::MissingJoinTemplate
        }
    }
}

/// The (operation id, machine-add state) pair when recording a join
/// report failed because the operation already sits in a conflicting
/// machine-add state (invalid transition or terminal).
fn machine_add_state_conflict(
    error: &RecordMachineJoinReportError,
) -> Option<(
    OperationId,
    ployz_core::machine::MachineAddOperationStateName,
)> {
    let RecordMachineJoinReportError::RecordMachineAddEvent(
        RecordMachineAddEventError::ProjectStatus(
            StatusProjectionError::InvalidTransition {
                operation_id,
                current,
                ..
            }
            | StatusProjectionError::TerminalState {
                operation_id,
                current,
                ..
            },
        ),
    ) = error
    else {
        return None;
    };
    let ProjectionOperationState::MachineAdd(state) = current.as_ref() else {
        return None;
    };
    Some((operation_id.clone(), state.name()))
}

pub(super) fn completed_machine_add_operation_id(
    error: &RecordMachineJoinReportError,
) -> Option<OperationId> {
    machine_add_state_conflict(error).and_then(|(operation_id, state)| {
        (state == ployz_core::machine::MachineAddOperationStateName::Completed)
            .then_some(operation_id)
    })
}

pub(super) fn machine_join_report_error(
    error: RecordMachineJoinReportError,
) -> MachineJoinReportError {
    match &error {
        RecordMachineJoinReportError::InvalidJoinToken => {
            return MachineJoinReportError::InvalidJoinToken;
        }
        RecordMachineJoinReportError::UnknownJoinToken => {
            return MachineJoinReportError::UnknownJoinToken;
        }
        RecordMachineJoinReportError::RecordMachineAddEvent(_)
        | RecordMachineJoinReportError::StoreStatus(_)
        | RecordMachineJoinReportError::JoinTokenMismatch { .. } => {}
    }
    if let Some((operation_id, current)) = machine_add_state_conflict(&error) {
        return MachineJoinReportError::OperationNotJoining {
            operation_id,
            current,
        };
    }

    MachineJoinReportError::Unavailable {
        source: record_machine_join_report_unavailable_source(&error),
    }
}

pub(super) fn machine_join_redeem_error_from_repository_error(
    error: RedeemMachineJoinTokenRepositoryError,
) -> MachineJoinRedeemError {
    match error {
        RedeemMachineJoinTokenRepositoryError::Clock { .. } => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::Clock {
                    failure: OperationSubmitClockFailure::BeforeUnixEpoch,
                },
            }
        }
        RedeemMachineJoinTokenRepositoryError::InvalidJoinToken => {
            MachineJoinRedeemError::InvalidJoinToken
        }
        RedeemMachineJoinTokenRepositoryError::UnknownJoinToken => {
            MachineJoinRedeemError::UnknownJoinToken
        }
        RedeemMachineJoinTokenRepositoryError::LoadStatus(source) => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::StatusRead {
                    failure: status_read_failure(&source),
                },
            }
        }
        RedeemMachineJoinTokenRepositoryError::StoreStatus(source) => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::StatusWrite {
                    failure: operation_submit_status_failure(&source),
                },
            }
        }
        RedeemMachineJoinTokenRepositoryError::RecordMachineAddEvent(source) => {
            MachineJoinRedeemError::Unavailable {
                source: record_machine_add_event_unavailable_source(&source),
            }
        }
        // The mint worker has not stored the per-machine material yet:
        // typed not-ready, the keeper retries boundedly.
        RedeemMachineJoinTokenRepositoryError::MissingSecretDelivery { operation_id } => {
            MachineJoinRedeemError::MaterialNotReady { operation_id }
        }
        RedeemMachineJoinTokenRepositoryError::MissingOperation { .. }
        | RedeemMachineJoinTokenRepositoryError::WrongOperationKind { .. }
        | RedeemMachineJoinTokenRepositoryError::JoinTokenMismatch { .. } => {
            MachineJoinRedeemError::Unavailable {
                source: MachineJoinRedeemUnavailableSource::OperationCorrupt,
            }
        }
        RedeemMachineJoinTokenRepositoryError::OperationNotPending {
            operation_id,
            current,
        } => MachineJoinRedeemError::OperationNotPending {
            operation_id,
            current,
        },
        RedeemMachineJoinTokenRepositoryError::JoinRejected {
            operation_id,
            failure,
        } => MachineJoinRedeemError::Rejected {
            operation_id,
            failure,
        },
    }
}

fn operation_submit_status_failure(
    error: &OperationStatusStoreError,
) -> OperationSubmitStatusFailure {
    match error {
        OperationStatusStoreError::OpenBucket { .. } => OperationSubmitStatusFailure::OpenBucket,
        OperationStatusStoreError::EncodeStatus(_) => OperationSubmitStatusFailure::EncodeStatus,
        OperationStatusStoreError::DecodeStatus(_) => OperationSubmitStatusFailure::DecodeStatus,
        OperationStatusStoreError::EncodeSubmission(_) => {
            OperationSubmitStatusFailure::EncodeSubmission
        }
        OperationStatusStoreError::DecodeSubmission(_) => {
            OperationSubmitStatusFailure::DecodeSubmission
        }
        OperationStatusStoreError::CasConflict { .. } => OperationSubmitStatusFailure::CasConflict,
        OperationStatusStoreError::RecordExists { .. } => OperationSubmitStatusFailure::CasConflict,
        OperationStatusStoreError::GetStatus { .. } => OperationSubmitStatusFailure::GetStatus,
        OperationStatusStoreError::Timeout { .. } => OperationSubmitStatusFailure::Timeout,
    }
}

fn operation_submit_event_failure(error: &OperationEventLogError) -> OperationSubmitEventFailure {
    match error {
        OperationEventLogError::EncodeEvent(_) => OperationSubmitEventFailure::EncodeEvent,
        OperationEventLogError::DecodeEvent(_) => OperationSubmitEventFailure::DecodeEvent,
        OperationEventLogError::PublishRequest { .. } => {
            OperationSubmitEventFailure::PublishRequest
        }
        OperationEventLogError::PublishAck { .. } => OperationSubmitEventFailure::PublishAck,
        OperationEventLogError::ReadEvent { .. } => OperationSubmitEventFailure::ReadEvent,
        OperationEventLogError::Timeout { .. } => OperationSubmitEventFailure::Timeout,
        OperationEventLogError::InvalidAckSequence { .. } => {
            OperationSubmitEventFailure::InvalidAckSequence
        }
    }
}

fn record_machine_add_event_unavailable_source(
    error: &RecordMachineAddEventError,
) -> MachineJoinRedeemUnavailableSource {
    match error {
        RecordMachineAddEventError::LoadStatus(error) => {
            MachineJoinRedeemUnavailableSource::StatusRead {
                failure: status_read_failure(error),
            }
        }
        RecordMachineAddEventError::StoreStatus(error) => {
            MachineJoinRedeemUnavailableSource::StatusWrite {
                failure: operation_submit_status_failure(error),
            }
        }
        RecordMachineAddEventError::AppendEvent(error) => {
            MachineJoinRedeemUnavailableSource::EventLog {
                failure: operation_submit_event_failure(error),
            }
        }
        RecordMachineAddEventError::MissingOperation { .. }
        | RecordMachineAddEventError::ProjectStatus(_)
        | RecordMachineAddEventError::StoredEventMismatch { .. }
        | RecordMachineAddEventError::StatusProjectionContended => {
            MachineJoinRedeemUnavailableSource::OperationCorrupt
        }
    }
}

fn record_machine_join_report_unavailable_source(
    error: &RecordMachineJoinReportError,
) -> MachineJoinReportUnavailableSource {
    match error {
        RecordMachineJoinReportError::StoreStatus(error) => {
            MachineJoinReportUnavailableSource::StatusWrite {
                failure: operation_submit_status_failure(error),
            }
        }
        RecordMachineJoinReportError::RecordMachineAddEvent(error) => {
            record_machine_add_report_unavailable_source(error)
        }
        RecordMachineJoinReportError::InvalidJoinToken
        | RecordMachineJoinReportError::UnknownJoinToken
        | RecordMachineJoinReportError::JoinTokenMismatch { .. } => {
            MachineJoinReportUnavailableSource::OperationCorrupt
        }
    }
}

fn record_machine_add_report_unavailable_source(
    error: &RecordMachineAddEventError,
) -> MachineJoinReportUnavailableSource {
    match error {
        RecordMachineAddEventError::LoadStatus(error) => {
            MachineJoinReportUnavailableSource::StatusRead {
                failure: status_read_failure(error),
            }
        }
        RecordMachineAddEventError::StoreStatus(error) => {
            MachineJoinReportUnavailableSource::StatusWrite {
                failure: operation_submit_status_failure(error),
            }
        }
        RecordMachineAddEventError::AppendEvent(error) => {
            MachineJoinReportUnavailableSource::EventLog {
                failure: operation_submit_event_failure(error),
            }
        }
        RecordMachineAddEventError::MissingOperation { .. }
        | RecordMachineAddEventError::ProjectStatus(_)
        | RecordMachineAddEventError::StoredEventMismatch { .. }
        | RecordMachineAddEventError::StatusProjectionContended => {
            MachineJoinReportUnavailableSource::OperationCorrupt
        }
    }
}

pub(super) fn status_read_failure(error: &OperationStatusReadError) -> StatusReadFailure {
    match error {
        OperationStatusReadError::DecodeStatus(_) => StatusReadFailure::DecodeStatus,
        OperationStatusReadError::GetStatus { .. } => StatusReadFailure::GetStatus,
        OperationStatusReadError::Timeout { .. } => StatusReadFailure::Timeout,
    }
}

pub(super) fn ops_watch_error_from_replay_error(
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
    use crate::controllers::SubmitCommandError;
    use ployz_core::ids::OperationId;
    use ployz_core::ops::EventSequence;
    use ployz_nats::operations::{
        OperationEventLogError, OperationEventReplayReadError, OperationStatusReadError,
        OperationStatusStoreError, ReplayOperationEventsError, SubmitOperationError,
    };
    use ployz_sdk_types::{
        DeploySubmitError, EventReplayFailure, OperationSubmitEventFailure,
        OperationSubmitStatusFailure, OperationSubmitUnavailableSource, OpsWatchError,
        OpsWatchUnavailableSource, StatusReadFailure,
    };

    #[test]
    fn deploy_submit_maps_status_store_failure_to_api_error() {
        let operation_id = operation_id("op_123");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                operation_id.clone(),
                SubmitCommandError::Submit(SubmitOperationError::StoreStatus(
                    OperationStatusStoreError::CasConflict {
                        message: "contended".to_owned(),
                    },
                )),
            ),
            DeploySubmitError::Unavailable {
                operation_id,
                source: OperationSubmitUnavailableSource::StatusStore {
                    failure: OperationSubmitStatusFailure::CasConflict,
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
                SubmitCommandError::Submit(SubmitOperationError::AppendEvent(
                    OperationEventLogError::PublishRequest {
                        message: "publish unavailable".to_owned(),
                    },
                )),
            ),
            DeploySubmitError::Unavailable {
                operation_id,
                source: OperationSubmitUnavailableSource::EventLog {
                    failure: OperationSubmitEventFailure::PublishRequest,
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
                SubmitCommandError::Submit(SubmitOperationError::DuplicateSequenceMismatch {
                    sequence: event_sequence(9),
                }),
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
