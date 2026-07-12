//! Machine-join redeem and report handlers. Reporting completion records
//! the operation event first and then activates the machine in cluster
//! truth (record-then-activate).

use crate::adapters::nats_authorization::MintRequest;
use crate::operations::log::{
    MachineJoinRedemption, RecordMachineJoinReportError, RedeemMachineJoinTokenError,
};
use ployz_core::dataplane::MachineEndpointSubnet;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::{
    MachineAddFailure, MachineName, RawJoinToken, active_machine_from_completed_add,
};
use ployz_core::ops::OperationStatus;
use ployz_core::subjects::INTENT_CHANGED;
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemResult, MachineJoinRedeemed,
    MachineJoinReportError, MachineJoinReportFailure, MachineJoinReportOutcome,
    MachineJoinReportRequest, MachineJoinReported, MachineJoinReportedFailure,
    MachineJoinReportedOutcome,
};

use super::OperationApiHandlers;
use super::connectivity_proof;
use super::error_map::{
    completed_machine_add_operation_id, corrupt, machine_join_redeem_error_from_repository_error,
    machine_join_report_error,
};

pub async fn machine_join_redeem(
    handlers: &OperationApiHandlers,
    request: MachineJoinRedeemRequest,
) -> Result<MachineJoinRedeemed, MachineJoinRedeemError> {
    let raw_token = RawJoinToken::try_new(request.join_token.as_str())
        .map_err(|_| MachineJoinRedeemError::InvalidJoinToken)?;
    match handlers
        .controllers
        .redeem_machine_join_token(&raw_token)
        .await
    {
        Ok(redeemed) => Ok(machine_join_redeemed(redeemed)),
        Err(RedeemMachineJoinTokenError::UnknownJoinToken) => {
            let Some(command) = handlers
                .controllers
                .recover_machine_join_submission(&raw_token)
                .await
                .map_err(machine_join_redeem_error_from_repository_error)?
            else {
                return Err(MachineJoinRedeemError::UnknownJoinToken);
            };
            let operation_id = command.operation_id.clone();
            let idempotency_key = command.idempotency_key.clone();
            let accepted = handlers
                .controllers
                .submit_machine_add(
                    command,
                    handlers.dataplane_endpoint_supernet(),
                    &handlers.machine_roster,
                )
                .await
                .map_err(|error| MachineJoinRedeemError::Unavailable {
                    message: format!("{error:?}"),
                })?;
            handlers.machine_mint.start(MintRequest {
                operation_id: accepted.operation_id.clone(),
                machine_id: accepted.identity.machine_id,
                idempotency_key,
            });
            tokio::spawn({
                let handlers = handlers.clone();
                async move {
                    handlers.publish_pending_machine_joins().await;
                }
            });
            Err(MachineJoinRedeemError::MaterialNotReady { operation_id })
        }
        Err(error) => Err(machine_join_redeem_error_from_repository_error(error)),
    }
}

pub async fn machine_join_report(
    handlers: &OperationApiHandlers,
    request: MachineJoinReportRequest,
) -> Result<MachineJoinReported, MachineJoinReportError> {
    let raw_token = RawJoinToken::try_new(request.join_token.as_str())
        .map_err(|_| MachineJoinReportError::InvalidJoinToken)?;
    let report_target = handlers
        .controllers
        .repository()
        .machine_join_report_target(&raw_token)
        .await
        .map_err(machine_join_report_error)?;
    let (result, outcome) = match request.outcome {
        MachineJoinReportOutcome::Completed => {
            match connectivity_proof::prove_completed_join(handlers, &raw_token).await {
                Ok(Some(evidence)) => {
                    let result = handlers
                        .controllers
                        .repository()
                        .record_machine_join_failed(
                            &raw_token,
                            MachineAddFailure::ConnectivityProofFailed {
                                evidence: evidence.clone(),
                            },
                        )
                        .await;
                    (
                        result,
                        MachineJoinReportedOutcome::Failed {
                            failure: MachineJoinReportedFailure::ConnectivityProofFailed {
                                evidence,
                            },
                        },
                    )
                }
                Ok(None) => {
                    let result = handlers
                        .controllers
                        .repository()
                        .record_machine_join_completed(&raw_token)
                        .await;
                    (result, MachineJoinReportedOutcome::Completed)
                }
                Err(MachineJoinReportError::Unavailable { message }) => {
                    let message = connectivity_proof_unavailable_message(message);
                    let result = handlers
                        .controllers
                        .repository()
                        .record_machine_join_failed(
                            &raw_token,
                            MachineAddFailure::BootstrapFailed {
                                message: message.clone(),
                            },
                        )
                        .await;
                    (
                        result,
                        MachineJoinReportedOutcome::Failed {
                            failure: MachineJoinReportedFailure::BootstrapFailed { message },
                        },
                    )
                }
                Err(error) => return Err(error),
            }
        }
        MachineJoinReportOutcome::Failed {
            failure: MachineJoinReportFailure::BootstrapFailed { message },
        } => {
            let result = handlers
                .controllers
                .repository()
                .record_machine_join_failed(
                    &raw_token,
                    MachineAddFailure::BootstrapFailed {
                        message: message.clone(),
                    },
                )
                .await;
            (
                result,
                MachineJoinReportedOutcome::Failed {
                    failure: MachineJoinReportedFailure::BootstrapFailed { message },
                },
            )
        }
    };
    let reported = match result {
        Ok(reported) => reported,
        Err(error) if matches!(outcome, MachineJoinReportedOutcome::Completed) => {
            if let Some(reported) = repair_completed_machine_join_report(
                handlers,
                &error,
                report_target.endpoint_subnet.clone(),
            )
            .await?
            {
                return Ok(reported);
            }
            return Err(machine_join_report_error(error));
        }
        Err(error) => return Err(machine_join_report_error(error)),
    };

    let status = handlers
        .controllers
        .repository()
        .get(&reported.operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })?
        .ok_or(MachineJoinReportError::Unavailable {
            message: corrupt("missing machine-add operation after join report"),
        })?;

    let OperationStatus::MachineAdd {
        last_event_sequence,
        name,
        machine_id,
        ..
    } = status
    else {
        return Err(MachineJoinReportError::Unavailable {
            message: corrupt("joined operation is not machine-add"),
        });
    };
    if let MachineJoinReportedOutcome::Completed = outcome {
        activate_reported_machine(
            handlers,
            &reported.operation_id,
            &machine_id,
            &name,
            report_target.endpoint_subnet,
        )
        .await?;
        scrub_completed_machine_add_secrets(handlers, &reported.operation_id).await?;
    }

    Ok(MachineJoinReported {
        operation_id: reported.operation_id,
        machine_id: reported.machine_id,
        last_event_sequence,
        outcome,
    })
}

fn connectivity_proof_unavailable_message(message: String) -> ployz_core::ops::FailureMessage {
    ployz_core::ops::FailureMessage::try_new(format!(
        "overlay connectivity proof unavailable: {message}"
    ))
    .unwrap_or_else(|_| {
        ployz_core::ops::FailureMessage::try_new("overlay connectivity proof unavailable")
            .expect("static failure message is valid")
    })
}

async fn repair_completed_machine_join_report(
    handlers: &OperationApiHandlers,
    error: &RecordMachineJoinReportError,
    endpoint_subnet: MachineEndpointSubnet,
) -> Result<Option<MachineJoinReported>, MachineJoinReportError> {
    let Some(operation_id) = completed_machine_add_operation_id(error) else {
        return Ok(None);
    };
    let Some(status) = handlers
        .controllers
        .repository()
        .get(&operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })?
    else {
        return Err(MachineJoinReportError::Unavailable {
            message: corrupt("missing completed machine-add operation"),
        });
    };
    let OperationStatus::MachineAdd {
        id,
        machine_id,
        name,
        state: ployz_core::ops::MachineAddOperationState::Completed,
        last_event_sequence,
        ..
    } = status
    else {
        return Ok(None);
    };

    activate_reported_machine(handlers, &id, &machine_id, &name, endpoint_subnet).await?;
    scrub_completed_machine_add_secrets(handlers, &id).await?;
    Ok(Some(MachineJoinReported {
        operation_id: id,
        machine_id,
        last_event_sequence,
        outcome: MachineJoinReportedOutcome::Completed,
    }))
}

async fn scrub_completed_machine_add_secrets(
    handlers: &OperationApiHandlers,
    operation_id: &OperationId,
) -> Result<(), MachineJoinReportError> {
    handlers
        .controllers
        .repository()
        .scrub_machine_add_secrets(operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })
}

/// Writes the completed machine-add into core-owned roster intent. This is
/// the API layer's own write, not a query-runtime method: it runs only after
/// the join report has been recorded.
async fn activate_reported_machine(
    handlers: &OperationApiHandlers,
    operation_id: &OperationId,
    machine_id: &MachineId,
    name: &MachineName,
    endpoint_subnet: MachineEndpointSubnet,
) -> Result<(), MachineJoinReportError> {
    let status = handlers
        .controllers
        .repository()
        .get(operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })?
        .ok_or_else(|| MachineJoinReportError::Unavailable {
            message: corrupt("missing machine-add operation during activation"),
        })?;
    let OperationStatus::MachineAdd { roles, .. } = status else {
        return Err(MachineJoinReportError::Unavailable {
            message: corrupt("activation operation is not machine-add"),
        });
    };
    let active_machine = active_machine_from_completed_add(
        operation_id.clone(),
        machine_id.clone(),
        name.clone(),
        roles,
        endpoint_subnet,
        ployz_core::ops::MachineAddOperationState::Completed,
    )
    .map_err(|_| MachineJoinReportError::Unavailable {
        message: corrupt("completed machine-add did not produce active machine"),
    })?;
    handlers
        .machine_roster
        .replace_active_machine(&active_machine)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })?;
    let _ = handlers
        .intent_change_client
        .publish(INTENT_CHANGED, Vec::new().into())
        .await;
    Ok(())
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
        machine_id: joined.machine_id,
        name: joined.name,
        roles: joined.roles,
        host_port_assurance: joined.host_port_assurance,
        endpoint_subnet: joined.endpoint_subnet,
        join_bundle: joined.join_bundle,
        secret_delivery: joined.secret_delivery,
        joined_at: joined.joined_at,
        last_event_sequence: joined.last_event_sequence,
        result,
    }
}
