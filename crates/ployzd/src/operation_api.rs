//! User-facing operation service handlers.

use crate::controllers::{
    DeploySubmitCommand, MachineAddBootstrapMaterialError, MachineAddSubmitCommand,
    OperationControllers,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationOwnerLease,
    OperationStatusSnapshot,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_core::subjects::op_watch;
use ployz_nats::operations::{
    OperationEventLogError, OperationEventReplayReadError, OperationStatusReadError,
    OperationStatusStoreError, ReplayOperationEventsError,
    SubmitDeployError as SubmitDeployRepositoryError,
    SubmitMachineAddError as SubmitMachineAddRepositoryError,
};
use ployz_sdk_types::{
    AcceptedOperation, BootstrapMaterialFailure, DeploySubmitError, DeploySubmitRequest,
    DeploySubmitUnavailableSource, EventReplayFailure, MachineAddAccepted, MachineAddError,
    MachineAddRequest, MachineAddUnavailableSource, MachineBootstrapUrl, MachineJoinToken,
    OperationSubmitClockFailure, OperationSubmitEventFailure, OperationSubmitStatusFailure,
    OpsStatusError, OpsStatusUnavailableSource, OpsWatchError, OpsWatchUnavailableSource,
    StatusReadFailure,
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
            target: value.target,
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

pub async fn machine_add(
    controllers: &OperationControllers,
    request: MachineAddRequest,
) -> Result<MachineAddAccepted, MachineAddError> {
    let operation_id = request.operation_id.clone();
    let material = controllers
        .issue_machine_add_bootstrap_material(&operation_id)
        .map_err(|error| MachineAddError::Unavailable {
            operation_id: operation_id.clone(),
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: bootstrap_material_failure(error),
            },
        })?;
    let command = MachineAddSubmitCommand {
        operation_id: request.operation_id,
        idempotency_key: request.idempotency_key,
        node_id: request.node_id,
        name: request.name,
        gateway: first_node_gateway(request.gateway),
        join_token: material.join_token,
        raw_join_token: material.raw_join_token,
    };

    let accepted = controllers
        .submit_machine_add(command)
        .await
        .map_err(|error| machine_add_error_from_submit_error(operation_id.clone(), error))?;
    let raw_token = MachineJoinToken::try_new(accepted.raw_join_token.as_str()).map_err(|_| {
        MachineAddError::Unavailable {
            operation_id: operation_id.clone(),
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: BootstrapMaterialFailure::IssueJoinToken,
            },
        }
    })?;

    Ok(MachineAddAccepted {
        accepted: owned_operation(
            accepted.operation_id,
            accepted.start_sequence,
            accepted.lease,
        ),
        node_id: accepted.node_id,
        bootstrap_url: MachineBootstrapUrl::try_new(material.bootstrap_url).map_err(|_| {
            MachineAddError::Unavailable {
                operation_id: operation_id.clone(),
                source: MachineAddUnavailableSource::BootstrapMaterial {
                    failure: BootstrapMaterialFailure::RenderBootstrapUrl,
                },
            }
        })?,
        join_token: raw_token,
    })
}

fn deploy_submit_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitDeployRepositoryError,
) -> DeploySubmitError {
    match error {
        SubmitDeployRepositoryError::AppendEvent(source) => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::EventLog {
                failure: operation_submit_event_failure(&source),
            },
        },
        SubmitDeployRepositoryError::StoreStatus(source) => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::StatusStore {
                failure: operation_submit_status_failure(&source),
            },
        },
        SubmitDeployRepositoryError::Clock { .. } => DeploySubmitError::Unavailable {
            operation_id,
            source: DeploySubmitUnavailableSource::Clock {
                failure: OperationSubmitClockFailure::BeforeUnixEpoch,
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

fn machine_add_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitMachineAddRepositoryError,
) -> MachineAddError {
    match error {
        SubmitMachineAddRepositoryError::AppendEvent(source) => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::EventLog {
                failure: operation_submit_event_failure(&source),
            },
        },
        SubmitMachineAddRepositoryError::StoreStatus(source) => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::StatusStore {
                failure: operation_submit_status_failure(&source),
            },
        },
        SubmitMachineAddRepositoryError::Clock { .. } => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::Clock {
                failure: OperationSubmitClockFailure::BeforeUnixEpoch,
            },
        },
        SubmitMachineAddRepositoryError::DuplicateSequenceMismatch { sequence } => {
            MachineAddError::DuplicateSequenceMismatch {
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
        OperationStatusStoreError::EncodeLease(_) => OperationSubmitStatusFailure::EncodeLease,
        OperationStatusStoreError::DecodeLease(_) => OperationSubmitStatusFailure::DecodeLease,
        OperationStatusStoreError::CasConflict { .. } => OperationSubmitStatusFailure::CasConflict,
        OperationStatusStoreError::GetStatus { .. } => OperationSubmitStatusFailure::GetStatus,
        OperationStatusStoreError::Clock { .. } => OperationSubmitStatusFailure::Clock,
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

fn first_node_gateway(gateway: ployz_sdk_types::MachineAddGateway) -> FirstNodeGateway {
    match gateway {
        ployz_sdk_types::MachineAddGateway::Install => FirstNodeGateway::Install,
        ployz_sdk_types::MachineAddGateway::Skip => FirstNodeGateway::Skip,
    }
}

fn bootstrap_material_failure(error: MachineAddBootstrapMaterialError) -> BootstrapMaterialFailure {
    match error {
        MachineAddBootstrapMaterialError::Clock { .. }
        | MachineAddBootstrapMaterialError::InvalidJoinTokenMaterial => {
            BootstrapMaterialFailure::IssueJoinToken
        }
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
        DeploySubmitError, DeploySubmitUnavailableSource, EventReplayFailure,
        OperationSubmitEventFailure, OperationSubmitStatusFailure, OpsWatchError,
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
                SubmitDeployRepositoryError::AppendEvent(OperationEventLogError::PublishRequest {
                    message: "publish unavailable".to_owned(),
                }),
            ),
            DeploySubmitError::Unavailable {
                operation_id,
                source: DeploySubmitUnavailableSource::EventLog {
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
