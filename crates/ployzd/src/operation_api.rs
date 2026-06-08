//! User-facing operation service handlers.

use crate::controllers::{
    BackupCreateCommand, DeploySubmitCommand, MachineAddBootstrapMaterialError,
    MachineAddSubmitCommand, OperationControllers,
};
use crate::deploy_runtime::DeployOperationRuntime;
use ployz_core::ids::OperationId;
use ployz_core::machine::RawJoinToken;
use ployz_core::ops::{
    OperationEventReplayPage, OperationEventReplayRequest, OperationOwnerLease, OperationStatus,
    OperationStatusSnapshot, ProjectionOperationState, StatusProjectionError,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_core::subjects::op_watch;
use ployz_nats::operations::{
    MachineJoinRedemption, OperationEventLogError, OperationEventReplayReadError,
    OperationStatusReadError, OperationStatusStoreError, RecordMachineAddEventError,
    RecordMachineJoinReportError,
    RedeemMachineJoinTokenError as RedeemMachineJoinTokenRepositoryError,
    ReplayOperationEventsError, SubmitBackupError as SubmitBackupRepositoryError,
    SubmitDeployError as SubmitDeployRepositoryError,
    SubmitMachineAddError as SubmitMachineAddRepositoryError,
};
use ployz_sdk_types::{
    AcceptedOperation, BackupCreateError, BackupCreateRequest, BackupCreateUnavailableSource,
    BootstrapMaterialFailure, DeploySubmitError, DeploySubmitRequest,
    DeploySubmitUnavailableSource, EventReplayFailure, MachineAddAccepted, MachineAddError,
    MachineAddRequest, MachineAddUnavailableSource, MachineJoinRedeemError,
    MachineJoinRedeemRequest, MachineJoinRedeemResult, MachineJoinRedeemUnavailableSource,
    MachineJoinRedeemed, MachineJoinReportError, MachineJoinReportFailure,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinReportUnavailableSource,
    MachineJoinReported, MachineJoinToken, OperationSubmitClockFailure,
    OperationSubmitEventFailure, OperationSubmitStatusFailure, OpsStatusError,
    OpsStatusUnavailableSource, OpsWatchError, OpsWatchUnavailableSource, StatusReadFailure,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct OperationApiHandlers {
    controllers: OperationControllers,
    deploy_execution: DeploySubmitExecution,
}

impl OperationApiHandlers {
    #[must_use]
    pub fn accept_only(controllers: OperationControllers) -> Self {
        Self {
            controllers,
            deploy_execution: DeploySubmitExecution::AcceptOnly,
        }
    }

    #[must_use]
    pub fn execute_operations(
        controllers: OperationControllers,
        deploy_runtime: DeployOperationRuntime,
    ) -> Self {
        Self {
            controllers,
            deploy_execution: DeploySubmitExecution::Execute(Arc::new(deploy_runtime)),
        }
    }

    #[must_use]
    pub const fn controllers(&self) -> &OperationControllers {
        &self.controllers
    }
}

#[derive(Clone)]
pub enum DeploySubmitExecution {
    AcceptOnly,
    Execute(Arc<DeployOperationRuntime>),
}

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

impl From<BackupCreateRequest> for BackupCreateCommand {
    fn from(value: BackupCreateRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            idempotency_key: value.idempotency_key,
        }
    }
}

pub async fn deploy_submit(
    handlers: &OperationApiHandlers,
    command: DeploySubmitCommand,
) -> Result<AcceptedOperation, DeploySubmitError> {
    let operation_id = command.operation_id.clone();
    let accepted = handlers
        .controllers
        .submit_deploy(command)
        .await
        .map_err(|error| deploy_submit_error_from_submit_error(operation_id, error))?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        accepted.start_sequence,
        accepted.lease.clone(),
    );
    match (&handlers.deploy_execution, accepted.should_start_execution) {
        (DeploySubmitExecution::Execute(runtime), true) => runtime.start(accepted),
        (DeploySubmitExecution::AcceptOnly, true | false)
        | (DeploySubmitExecution::Execute(_), false) => {}
    }

    Ok(operation)
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
        join_bundle: request.join_bundle,
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
        bootstrap_url: material.bootstrap_url,
        join_token: raw_token,
    })
}

pub async fn backup_create(
    handlers: &OperationApiHandlers,
    request: BackupCreateRequest,
) -> Result<AcceptedOperation, BackupCreateError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers
        .submit_backup(request.into())
        .await
        .map_err(|error| backup_create_error_from_submit_error(operation_id, error))?;

    Ok(owned_operation(
        accepted.operation_id,
        accepted.start_sequence,
        accepted.lease,
    ))
}

pub async fn machine_join_redeem(
    controllers: &OperationControllers,
    request: MachineJoinRedeemRequest,
) -> Result<MachineJoinRedeemed, MachineJoinRedeemError> {
    let raw_token = RawJoinToken::try_new(request.join_token.as_str())
        .map_err(|_| MachineJoinRedeemError::InvalidJoinToken)?;
    let redeemed = controllers
        .redeem_machine_join_token(&raw_token)
        .await
        .map_err(machine_join_redeem_error_from_repository_error)?;
    Ok(machine_join_redeemed(redeemed))
}

pub async fn machine_join_report(
    controllers: &OperationControllers,
    request: MachineJoinReportRequest,
) -> Result<MachineJoinReported, MachineJoinReportError> {
    let raw_token = RawJoinToken::try_new(request.join_token.as_str())
        .map_err(|_| MachineJoinReportError::InvalidJoinToken)?;
    let outcome = request.outcome;
    let result = match outcome.clone() {
        MachineJoinReportOutcome::Completed => {
            controllers.record_machine_join_completed(&raw_token).await
        }
        MachineJoinReportOutcome::Failed { failure } => {
            controllers
                .record_machine_join_failed(
                    &raw_token,
                    machine_add_failure_from_join_report_failure(failure),
                )
                .await
        }
    };
    let reported = result.map_err(machine_join_report_error)?;

    let status = controllers
        .operation_status(&reported.operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::StatusRead {
                failure: status_read_failure(&error),
            },
        })?
        .ok_or(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        })?;

    let OperationStatus::MachineAdd {
        last_event_sequence,
        ..
    } = status
    else {
        return Err(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        });
    };

    Ok(MachineJoinReported {
        operation_id: reported.operation_id,
        node_id: reported.node_id,
        last_event_sequence,
        outcome,
    })
}

fn machine_add_failure_from_join_report_failure(
    failure: MachineJoinReportFailure,
) -> ployz_core::machine::MachineAddFailure {
    match failure {
        MachineJoinReportFailure::BootstrapFailed { message } => {
            ployz_core::machine::MachineAddFailure::BootstrapFailed { message }
        }
    }
}

fn machine_join_redeemed(redemption: MachineJoinRedemption) -> MachineJoinRedeemed {
    let (joined, result) = match redemption {
        MachineJoinRedemption::Joined(joined) => (joined, MachineJoinRedeemResult::Joined),
        MachineJoinRedemption::AlreadyJoined(joined) => {
            (joined, MachineJoinRedeemResult::AlreadyJoined)
        }
    };

    MachineJoinRedeemed {
        operation_id: joined.operation_id,
        node_id: joined.node_id,
        name: joined.name,
        gateway: joined.gateway,
        join_bundle: joined.join_bundle,
        joined_at: joined.joined_at,
        last_event_sequence: joined.last_event_sequence,
        result,
    }
}

fn machine_join_report_error(error: RecordMachineJoinReportError) -> MachineJoinReportError {
    match &error {
        RecordMachineJoinReportError::InvalidJoinToken => {
            return MachineJoinReportError::InvalidJoinToken;
        }
        RecordMachineJoinReportError::UnknownJoinToken => {
            return MachineJoinReportError::UnknownJoinToken;
        }
        RecordMachineJoinReportError::RecordMachineAddEvent(error) => match error {
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
            ) => {
                if let ProjectionOperationState::MachineAdd(state) = current.as_ref() {
                    return MachineJoinReportError::OperationNotJoining {
                        operation_id: operation_id.clone(),
                        current: state.name(),
                    };
                }
            }
            RecordMachineAddEventError::LoadStatus(_)
            | RecordMachineAddEventError::StoreStatus(_)
            | RecordMachineAddEventError::MissingOperation { .. }
            | RecordMachineAddEventError::ProjectStatus(_)
            | RecordMachineAddEventError::AppendEvent(_)
            | RecordMachineAddEventError::StoredEventMismatch { .. }
            | RecordMachineAddEventError::StatusCursorContended => {}
        },
        RecordMachineJoinReportError::StoreStatus(_)
        | RecordMachineJoinReportError::JoinTokenMismatch { .. } => {}
    }

    MachineJoinReportError::Unavailable {
        source: record_machine_join_report_unavailable_source(&error),
    }
}

fn machine_join_redeem_error_from_repository_error(
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

fn backup_create_error_from_submit_error(
    operation_id: OperationId,
    error: SubmitBackupRepositoryError,
) -> BackupCreateError {
    match error {
        SubmitBackupRepositoryError::AppendEvent(source) => BackupCreateError::Unavailable {
            operation_id,
            source: BackupCreateUnavailableSource::EventLog {
                failure: operation_submit_event_failure(&source),
            },
        },
        SubmitBackupRepositoryError::StoreStatus(source) => BackupCreateError::Unavailable {
            operation_id,
            source: BackupCreateUnavailableSource::StatusStore {
                failure: operation_submit_status_failure(&source),
            },
        },
        SubmitBackupRepositoryError::Clock { .. } => BackupCreateError::Unavailable {
            operation_id,
            source: BackupCreateUnavailableSource::Clock {
                failure: OperationSubmitClockFailure::BeforeUnixEpoch,
            },
        },
        SubmitBackupRepositoryError::DuplicateSequenceMismatch { sequence } => {
            BackupCreateError::DuplicateSequenceMismatch {
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
        SubmitMachineAddRepositoryError::JoinTokenMismatch => MachineAddError::Unavailable {
            operation_id,
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: BootstrapMaterialFailure::IssueJoinToken,
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
        | RecordMachineAddEventError::StatusCursorContended => {
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
        | RecordMachineAddEventError::StatusCursorContended => {
            MachineJoinReportUnavailableSource::OperationCorrupt
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
