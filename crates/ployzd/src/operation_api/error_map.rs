//! Mapping from repository/controller failures to client-visible operation API
//! errors. Actionable states stay typed; storage and transport plumbing renders
//! as evidence text.

use crate::controllers::{MachineAddSubmitCommandError, SubmitCommandError};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    EventSequence, FailureMessage, ProjectionOperationState, StatusProjectionError,
};
use ployz_nats::operations::{
    RecordMachineAddEventError, RecordMachineJoinReportError,
    RedeemMachineJoinTokenError as RedeemMachineJoinTokenRepositoryError,
    ReplayOperationEventsError, SubmitOperationError,
};
use ployz_sdk_types::{
    DeploySubmitError, MachineAddError, MachineJoinRedeemError, MachineJoinReportError,
    OpsWatchError,
};

/// The one definition of the corrupt-record evidence prefix, so the
/// sentinel stays greppable and consistent across the operation API.
pub(super) fn corrupt(detail: impl std::fmt::Display) -> String {
    format!("operation record corrupt: {detail}")
}

pub(super) enum SubmitFailure {
    InvalidDeployTarget,
    ResourceBusy {
        namespace_id: ployz_core::ids::NamespaceId,
        owner: OperationId,
    },
    Unavailable {
        message: String,
    },
    DuplicateSequenceMismatch {
        sequence: EventSequence,
    },
}

pub(super) fn submit_failure(error: SubmitCommandError) -> SubmitFailure {
    match error {
        SubmitCommandError::Clock { message } => SubmitFailure::Unavailable {
            message: format!("clock: {message}"),
        },
        SubmitCommandError::NamespaceBusy {
            namespace_id,
            owner,
        } => SubmitFailure::ResourceBusy {
            namespace_id,
            owner,
        },
        SubmitCommandError::NamespaceLock(source) => SubmitFailure::Unavailable {
            message: source.to_string(),
        },
        SubmitCommandError::Submit(SubmitOperationError::InvalidDeployTarget) => {
            SubmitFailure::InvalidDeployTarget
        }
        SubmitCommandError::Submit(SubmitOperationError::AppendEvent(source)) => {
            SubmitFailure::Unavailable {
                message: source.to_string(),
            }
        }
        SubmitCommandError::Submit(SubmitOperationError::StoreStatus(source)) => {
            SubmitFailure::Unavailable {
                message: source.to_string(),
            }
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
        SubmitFailure::ResourceBusy {
            namespace_id,
            owner,
        } => DeploySubmitError::ResourceBusy {
            operation_id,
            namespace_id,
            owner_operation_id: owner,
        },
        SubmitFailure::Unavailable { message } => DeploySubmitError::Unavailable {
            operation_id,
            message,
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
                message: "machine-add join token mismatch".to_owned(),
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
        SubmitFailure::ResourceBusy { .. } => {
            unreachable!("machine add submit has no namespace lock")
        }
        SubmitFailure::Unavailable { message } => MachineAddError::Unavailable {
            operation_id,
            message,
        },
        SubmitFailure::DuplicateSequenceMismatch { sequence } => {
            MachineAddError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
    }
}

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
    if let Some((operation_id, current)) = machine_add_state_conflict(&error) {
        return MachineJoinReportError::OperationNotJoining {
            operation_id,
            current,
        };
    }
    match error {
        RecordMachineJoinReportError::InvalidJoinToken => MachineJoinReportError::InvalidJoinToken,
        RecordMachineJoinReportError::UnknownJoinToken => MachineJoinReportError::UnknownJoinToken,
        RecordMachineJoinReportError::StoreStatus(source) => MachineJoinReportError::Unavailable {
            message: source.to_string(),
        },
        RecordMachineJoinReportError::RecordMachineAddEvent(source) => {
            MachineJoinReportError::Unavailable {
                message: source.to_string(),
            }
        }
        RecordMachineJoinReportError::JoinTokenMismatch { operation_id } => {
            MachineJoinReportError::Unavailable {
                message: corrupt(format_args!(
                    "join token mismatch for {}",
                    operation_id.as_str()
                )),
            }
        }
    }
}

pub(super) fn machine_join_redeem_error_from_repository_error(
    error: RedeemMachineJoinTokenRepositoryError,
) -> MachineJoinRedeemError {
    match error {
        RedeemMachineJoinTokenRepositoryError::InvalidJoinToken => {
            MachineJoinRedeemError::InvalidJoinToken
        }
        RedeemMachineJoinTokenRepositoryError::UnknownJoinToken => {
            MachineJoinRedeemError::UnknownJoinToken
        }
        RedeemMachineJoinTokenRepositoryError::MissingSecretDelivery { operation_id } => {
            MachineJoinRedeemError::MaterialNotReady { operation_id }
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
        RedeemMachineJoinTokenRepositoryError::Clock { message } => {
            MachineJoinRedeemError::Unavailable {
                message: format!("clock: {message}"),
            }
        }
        RedeemMachineJoinTokenRepositoryError::LoadStatus(source) => {
            MachineJoinRedeemError::Unavailable {
                message: source.to_string(),
            }
        }
        RedeemMachineJoinTokenRepositoryError::StoreStatus(source) => {
            MachineJoinRedeemError::Unavailable {
                message: source.to_string(),
            }
        }
        RedeemMachineJoinTokenRepositoryError::RecordMachineAddEvent(source) => {
            MachineJoinRedeemError::Unavailable {
                message: source.to_string(),
            }
        }
        RedeemMachineJoinTokenRepositoryError::MissingOperation { operation_id } => {
            MachineJoinRedeemError::Unavailable {
                message: corrupt(format_args!("missing operation {}", operation_id.as_str())),
            }
        }
        RedeemMachineJoinTokenRepositoryError::WrongOperationKind { operation_id } => {
            MachineJoinRedeemError::Unavailable {
                message: corrupt(format_args!(
                    "{} is not a machine-add operation",
                    operation_id.as_str()
                )),
            }
        }
        RedeemMachineJoinTokenRepositoryError::JoinTokenMismatch { operation_id } => {
            MachineJoinRedeemError::Unavailable {
                message: corrupt(format_args!(
                    "join token mismatch for {}",
                    operation_id.as_str()
                )),
            }
        }
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
            message: source.to_string(),
        },
        ReplayOperationEventsError::ReadEvents(source) => OpsWatchError::Unavailable {
            operation_id,
            message: source.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{deploy_submit_error_from_submit_error, ops_watch_error_from_replay_error};
    use crate::controllers::SubmitCommandError;
    use ployz_core::ids::{NamespaceId, OperationId};
    use ployz_core::ops::EventSequence;
    use ployz_nats::operations::{
        OperationEventLogError, OperationEventReplayReadError, OperationStatusReadError,
        OperationStatusStoreError, ReplayOperationEventsError, SubmitOperationError,
    };
    use ployz_sdk_types::{DeploySubmitError, OpsWatchError};

    #[test]
    fn deploy_submit_renders_status_store_failure_to_message() {
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
                message: "operation status CAS conflict: contended".to_owned(),
            }
        );
    }

    #[test]
    fn deploy_submit_renders_event_log_failure_to_message() {
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
                message: "publish operation event: publish unavailable".to_owned(),
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
    fn deploy_submit_maps_namespace_busy_to_resource_busy() {
        let submitted_operation_id = operation_id("op_new");
        let namespace_id = namespace_id("ns_prod");
        let owner = operation_id("op_running");

        assert_eq!(
            deploy_submit_error_from_submit_error(
                submitted_operation_id.clone(),
                SubmitCommandError::NamespaceBusy {
                    namespace_id: namespace_id.clone(),
                    owner: owner.clone(),
                },
            ),
            DeploySubmitError::ResourceBusy {
                operation_id: submitted_operation_id,
                namespace_id,
                owner_operation_id: owner,
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
    fn ops_watch_renders_status_store_failure_to_message() {
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
                message: "get operation status: kv unavailable".to_owned(),
            }
        );
    }

    #[test]
    fn ops_watch_renders_event_log_failure_to_message() {
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
                message: "read operation event: stream unavailable".to_owned(),
            }
        );
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn namespace_id(value: &str) -> NamespaceId {
        NamespaceId::try_new(value).expect("valid namespace id")
    }

    fn event_sequence(value: u64) -> EventSequence {
        EventSequence::try_new(value).expect("valid event sequence")
    }
}
