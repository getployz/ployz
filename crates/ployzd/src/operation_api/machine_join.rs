//! Machine-join redeem and report handlers. Reporting completion records
//! the operation event first and then activates the machine in cluster
//! truth (record-then-activate).

use crate::adapters::nats_authorization::MintRequest;
use crate::operations::deploy::DataplanePreparer;
use crate::operations::log::{
    MachineJoinRedemption, RecordMachineJoinReportError, RedeemMachineJoinTokenError,
};
use crate::roles::machine::client::NatsMachineDataplanePreparer;
use ployz_core::dataplane::{DataplanePrepareRequest, endpoint_bridge_gateway_ipv4};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::{
    ConnectivityProofEvidence, ConnectivityProofUnreachablePeer, MachineAddFailure, MachineName,
    RawJoinToken, active_machine_from_completed_add,
};
use ployz_core::ops::{MachineAddOperationState, OperationStatus};
use ployz_core::subjects::INTENT_CHANGED;
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemResult, MachineJoinRedeemed,
    MachineJoinReportError, MachineJoinReportFailure, MachineJoinReportOutcome,
    MachineJoinReportRequest, MachineJoinReported,
};

use super::OperationApiHandlers;
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
                .submit_machine_add(command)
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
    let mut outcome = request.outcome;
    let result = match outcome.clone() {
        MachineJoinReportOutcome::Completed => {
            match connectivity_proof_for_completed_join(handlers, &raw_token).await? {
                Some(failure) => {
                    let MachineAddFailure::ConnectivityProofFailed { evidence } = &failure else {
                        return Err(MachineJoinReportError::Unavailable {
                            message: corrupt(
                                "connectivity gate produced a non-connectivity failure",
                            ),
                        });
                    };
                    outcome = MachineJoinReportOutcome::Failed {
                        failure: MachineJoinReportFailure::ConnectivityProofFailed {
                            evidence: evidence.clone(),
                        },
                    };
                    handlers
                        .controllers
                        .repository()
                        .record_machine_join_failed(&raw_token, failure)
                        .await
                }
                None => {
                    handlers
                        .controllers
                        .repository()
                        .record_machine_join_completed(&raw_token)
                        .await
                }
            }
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
    if let MachineJoinReportOutcome::Completed = outcome {
        activate_reported_machine(handlers, &reported.operation_id, &machine_id, &name).await?;
        scrub_completed_machine_add_secrets(handlers, &reported.operation_id).await?;
    }

    Ok(MachineJoinReported {
        operation_id: reported.operation_id,
        machine_id: reported.machine_id,
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

    activate_reported_machine(handlers, &id, &machine_id, &name).await?;
    scrub_completed_machine_add_secrets(handlers, &id).await?;
    Ok(Some(MachineJoinReported {
        operation_id: id,
        machine_id,
        last_event_sequence,
        outcome: MachineJoinReportOutcome::Completed,
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
) -> Result<(), MachineJoinReportError> {
    let endpoint_subnet = allocate_endpoint_subnet(handlers, machine_id).await?;
    let active_machine = active_machine_from_completed_add(
        operation_id.clone(),
        machine_id.clone(),
        name.clone(),
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

/// Records the machine's endpoint subnet in roster intent. The machine
/// daemon derives its own bridge subnet from its machine id unless
/// `PLOYZ_DATAPLANE_ENDPOINT_SUBNET` overrides it, so the roster prefers
/// that same derivation: intent and the machine's dataplane then agree
/// without a delivery channel. A machine whose derived subnet is already
/// assigned falls back to the next free subnet in the configured supernet
/// and must be configured with a matching subnet override.
async fn allocate_endpoint_subnet(
    handlers: &OperationApiHandlers,
    machine_id: &MachineId,
) -> Result<ployz_core::dataplane::MachineEndpointSubnet, MachineJoinReportError> {
    let machines = handlers
        .machine_roster
        .active_machines()
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })?;
    if let Some(machine) = machines
        .iter()
        .find(|machine| machine.machine_id == *machine_id)
    {
        return Ok(machine.endpoint_subnet.clone());
    }
    let derived = ployz_core::dataplane::MachineEndpointSubnet::try_new(
        ployz_core::dataplane::default_endpoint_subnet(machine_id),
    )
    .map_err(|error| MachineJoinReportError::Unavailable {
        message: error.to_string(),
    })?;
    if machines
        .iter()
        .all(|machine| machine.endpoint_subnet != derived)
    {
        return Ok(derived);
    }
    let assigned = machines.into_iter().map(|machine| machine.endpoint_subnet);
    handlers
        .dataplane_endpoint_supernet()
        .allocate_next(assigned)
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })
}

fn machine_add_failure_from_join_report_failure(
    failure: MachineJoinReportFailure,
) -> ployz_core::machine::MachineAddFailure {
    match failure {
        MachineJoinReportFailure::BootstrapFailed { message } => {
            ployz_core::machine::MachineAddFailure::BootstrapFailed { message }
        }
        MachineJoinReportFailure::ConnectivityProofFailed { evidence } => {
            ployz_core::machine::MachineAddFailure::ConnectivityProofFailed { evidence }
        }
    }
}

async fn connectivity_proof_for_completed_join(
    handlers: &OperationApiHandlers,
    raw_token: &RawJoinToken,
) -> Result<Option<MachineAddFailure>, MachineJoinReportError> {
    let target = handlers
        .controllers
        .repository()
        .machine_join_report_target(raw_token)
        .await
        .map_err(machine_join_report_error)?;
    let status = handlers
        .controllers
        .repository()
        .get(&target.operation_id)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })?
        .ok_or(MachineJoinReportError::Unavailable {
            message: corrupt("missing machine-add operation before connectivity proof"),
        })?;
    let OperationStatus::MachineAdd {
        id: operation_id,
        machine_id,
        state,
        ..
    } = status
    else {
        return Err(MachineJoinReportError::Unavailable {
            message: corrupt("joined operation is not machine-add"),
        });
    };
    match state {
        MachineAddOperationState::Joining { .. } => {
            prove_overlay_connectivity(handlers, operation_id, machine_id).await
        }
        MachineAddOperationState::Pending { .. }
        | MachineAddOperationState::Completed
        | MachineAddOperationState::Failed { .. }
        | MachineAddOperationState::Cancelled { .. } => Ok(None),
    }
}

async fn prove_overlay_connectivity(
    handlers: &OperationApiHandlers,
    operation_id: OperationId,
    joining_machine_id: MachineId,
) -> Result<Option<MachineAddFailure>, MachineJoinReportError> {
    let existing_machines = handlers
        .machine_roster
        .active_machines()
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: error.to_string(),
        })?;
    if existing_machines.is_empty() {
        return Ok(None);
    }

    let joining_subnet = allocate_endpoint_subnet(handlers, &joining_machine_id).await?;
    let mut machine_ids = existing_machines
        .iter()
        .map(|machine| machine.machine_id.clone())
        .collect::<Vec<_>>();
    machine_ids.push(joining_machine_id.clone());
    let mut prepare_request =
        DataplanePrepareRequest::for_machines(operation_id.clone(), machine_ids.clone());
    for member in &mut prepare_request.membership {
        if member.machine_id == joining_machine_id {
            member.endpoint_subnet = joining_subnet.clone();
            continue;
        }
        let Some(machine) = existing_machines
            .iter()
            .find(|machine| machine.machine_id == member.machine_id)
        else {
            return Err(MachineJoinReportError::Unavailable {
                message: corrupt("dataplane membership is missing an active machine"),
            });
        };
        member.endpoint_subnet = machine.endpoint_subnet.clone();
    }

    let mut preparer = NatsMachineDataplanePreparer::new(handlers.intent_change_client.clone())
        .with_facts_reader(handlers.facts_reader.clone())
        .with_request_timeout(handlers.deploy_driver.step_timeout());
    preparer
        .prepare_dataplane(prepare_request)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: format!("dataplane prepare failed: {error:?}"),
        })?;

    let mut peers = Vec::with_capacity(existing_machines.len());
    for machine in existing_machines {
        let subnet = machine.endpoint_subnet.as_string();
        let Some(gateway) = endpoint_bridge_gateway_ipv4(&subnet) else {
            return Err(MachineJoinReportError::Unavailable {
                message: corrupt("active machine endpoint subnet has no IPv4 bridge gateway"),
            });
        };
        peers.push((machine.machine_id, gateway));
    }
    let targets = peers.iter().map(|(_, gateway)| *gateway).collect();
    let unreachable = preparer
        .probe_overlay(operation_id, &joining_machine_id, machine_ids, targets)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: format!("overlay connectivity probe failed: {error:?}"),
        })?;
    Ok(connectivity_proof_failure(&peers, &unreachable))
}

fn connectivity_proof_failure(
    peers: &[(MachineId, std::net::Ipv4Addr)],
    unreachable: &[std::net::Ipv4Addr],
) -> Option<MachineAddFailure> {
    let unreachable_peers = peers
        .iter()
        .filter(|(_, gateway)| unreachable.contains(gateway))
        .map(|(machine_id, gateway)| ConnectivityProofUnreachablePeer {
            machine_id: machine_id.clone(),
            gateway: *gateway,
        })
        .collect::<Vec<_>>();
    ConnectivityProofEvidence::try_new(unreachable_peers)
        .ok()
        .map(|evidence| MachineAddFailure::ConnectivityProofFailed { evidence })
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
        join_bundle: joined.join_bundle,
        secret_delivery: joined.secret_delivery,
        joined_at: joined.joined_at,
        last_event_sequence: joined.last_event_sequence,
        result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_probe_result_becomes_machine_add_connectivity_failure() {
        let gateway = "10.198.1.1".parse().expect("valid gateway");

        let failure = connectivity_proof_failure(&[(machine_id("core_1"), gateway)], &[gateway]);

        assert_eq!(
            failure,
            Some(MachineAddFailure::ConnectivityProofFailed {
                evidence: ConnectivityProofEvidence::try_new(vec![
                    ConnectivityProofUnreachablePeer {
                        machine_id: machine_id("core_1"),
                        gateway,
                    },
                ])
                .expect("non-empty connectivity evidence"),
            })
        );
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }
}
