//! Machine-join redeem and report handlers. Reporting completion records
//! the operation event first and then activates the machine in cluster
//! truth (record-then-activate).

use crate::controllers::OperationControllers;
use ployz_core::ids::{NodeId, OperationId};
use ployz_core::machine::{MachineName, RawJoinToken, active_machine_from_completed_add};
use ployz_core::ops::OperationStatus;
use ployz_nats::operations::{MachineJoinRedemption, RecordMachineJoinReportError};
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemResult, MachineJoinRedeemed,
    MachineJoinReportError, MachineJoinReportFailure, MachineJoinReportOutcome,
    MachineJoinReportRequest, MachineJoinReportUnavailableSource, MachineJoinReported,
};

use super::OperationApiHandlers;
use super::error_map::{
    completed_machine_add_operation_id, machine_join_redeem_error_from_repository_error,
    machine_join_report_error, status_read_failure,
};

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
    handlers: &OperationApiHandlers,
    request: MachineJoinReportRequest,
) -> Result<MachineJoinReported, MachineJoinReportError> {
    let raw_token = RawJoinToken::try_new(request.join_token.as_str())
        .map_err(|_| MachineJoinReportError::InvalidJoinToken)?;
    let outcome = request.outcome;
    let result = match outcome.clone() {
        MachineJoinReportOutcome::Completed => {
            handlers
                .controllers
                .repository()
                .record_machine_join_completed(&raw_token)
                .await
        }
        MachineJoinReportOutcome::Failed { failure } => {
            handlers
                .controllers
                .repository()
                .record_machine_join_failed(
                    &raw_token,
                    machine_add_failure_from_join_report_failure(failure),
                )
                .await
        }
    };
    let reported = match result {
        Ok(reported) => reported,
        Err(error) if matches!(outcome, MachineJoinReportOutcome::Completed) => {
            if let Some(reported) = repair_completed_machine_join_report(handlers, &error).await? {
                return Ok(reported);
            }
            return Err(machine_join_report_error(error));
        }
        Err(error) => return Err(machine_join_report_error(error)),
    };

    let status = handlers
        .controllers
        .repository()
        .records()
        .get(&reported.operation_id)
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
        name,
        node_id,
        ..
    } = status
    else {
        return Err(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        });
    };
    if let MachineJoinReportOutcome::Completed = outcome {
        activate_reported_machine(handlers, &reported.operation_id, &node_id, &name).await?;
    }

    Ok(MachineJoinReported {
        operation_id: reported.operation_id,
        node_id: reported.node_id,
        last_event_sequence,
        outcome,
    })
}

async fn repair_completed_machine_join_report(
    handlers: &OperationApiHandlers,
    error: &RecordMachineJoinReportError,
) -> Result<Option<MachineJoinReported>, MachineJoinReportError> {
    let Some(operation_id) = completed_machine_add_operation_id(error) else {
        return Ok(None);
    };
    let Some(status) = handlers
        .controllers
        .repository()
        .records()
        .get(&operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::StatusRead {
                failure: status_read_failure(&error),
            },
        })?
    else {
        return Err(MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::OperationCorrupt,
        });
    };
    let OperationStatus::MachineAdd {
        id,
        node_id,
        name,
        state: ployz_core::machine::MachineAddOperationState::Completed,
        last_event_sequence,
        ..
    } = status
    else {
        return Ok(None);
    };

    activate_reported_machine(handlers, &id, &node_id, &name).await?;
    Ok(Some(MachineJoinReported {
        operation_id: id,
        node_id,
        last_event_sequence,
        outcome: MachineJoinReportOutcome::Completed,
    }))
}

/// Writes the completed machine-add into cluster truth. This is the API
/// layer's own core-state write, not a query-runtime method: it runs only
/// after the join report has been recorded.
async fn activate_reported_machine(
    handlers: &OperationApiHandlers,
    operation_id: &OperationId,
    node_id: &NodeId,
    name: &MachineName,
) -> Result<(), MachineJoinReportError> {
    let active_machine = active_machine_from_completed_add(
        operation_id.clone(),
        node_id.clone(),
        name.clone(),
        ployz_core::machine::MachineAddOperationState::Completed,
    )
    .map_err(|_| MachineJoinReportError::Unavailable {
        source: MachineJoinReportUnavailableSource::OperationCorrupt,
    })?;
    handlers
        .core_state
        .replace_active_machine(&active_machine)
        .await
        .map_err(|_| MachineJoinReportError::Unavailable {
            source: MachineJoinReportUnavailableSource::CoreState,
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
        roles: joined.roles,
        join_bundle: joined.join_bundle,
        secret_delivery: joined.secret_delivery,
        joined_at: joined.joined_at,
        last_event_sequence: joined.last_event_sequence,
        result,
    }
}
