//! Operation-owned overlay connectivity proof for a joining machine.

use std::net::Ipv4Addr;
use std::time::Duration;

use ployz_core::dataplane::{DataplaneMember, DataplanePrepareRequest};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::{
    ConnectivityProofEvidence, ConnectivityProofEvidenceError, ConnectivityProofUnreachablePeer,
    RawJoinToken,
};
use ployz_core::ops::{MachineAddOperationState, OperationStatus};
use ployz_sdk_types::MachineJoinReportError;

use crate::operations::deploy::DataplanePreparer;
use crate::roles::machine::client::NatsMachineDataplanePreparer;

use super::OperationApiHandlers;
use super::error_map::{corrupt, machine_join_report_error};
use super::machine_join::endpoint_subnet_for_roster;

const CONNECTIVITY_PROOF_PREPARE_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

// ponytail: keep synchronous join proof bounded here; move it to operation-owned async orchestration when join reporting becomes asynchronous.
pub(super) async fn prove_completed_join(
    handlers: &OperationApiHandlers,
    raw_token: &RawJoinToken,
) -> Result<Option<ConnectivityProofEvidence>, MachineJoinReportError> {
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
) -> Result<Option<ConnectivityProofEvidence>, MachineJoinReportError> {
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

    let joining_subnet = endpoint_subnet_for_roster(
        handlers.dataplane_endpoint_supernet(),
        &existing_machines,
        &joining_machine_id,
    )?;
    let mut members = existing_machines
        .iter()
        .map(|machine| DataplaneMember {
            machine_id: machine.machine_id.clone(),
            endpoint_subnet: machine.endpoint_subnet.clone(),
        })
        .collect::<Vec<_>>();
    members.push(DataplaneMember {
        machine_id: joining_machine_id.clone(),
        endpoint_subnet: joining_subnet,
    });
    let machine_ids = members
        .iter()
        .map(|member| member.machine_id.clone())
        .collect::<Vec<_>>();
    let prepare_request = DataplanePrepareRequest::for_members(operation_id.clone(), members);
    let mut preparer = NatsMachineDataplanePreparer::new(handlers.intent_change_client.clone())
        .with_request_timeout(CONNECTIVITY_PROOF_PREPARE_REQUEST_TIMEOUT);
    preparer
        .prepare_dataplane(prepare_request)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: format!("dataplane prepare failed: {error:?}"),
        })?;

    let peers = existing_machines
        .into_iter()
        .map(|machine| {
            (
                machine.machine_id,
                machine.endpoint_subnet.bridge_gateway_ipv4(),
            )
        })
        .collect::<Vec<_>>();
    let targets = peers.iter().map(|(_, gateway)| *gateway).collect();
    let unreachable = preparer
        .probe_overlay(operation_id, &joining_machine_id, machine_ids, targets)
        .await
        .map_err(|error| MachineJoinReportError::Unavailable {
            message: format!("overlay connectivity probe failed: {error:?}"),
        })?;
    Ok(connectivity_proof_evidence(&peers, &unreachable))
}

fn connectivity_proof_evidence(
    peers: &[(MachineId, Ipv4Addr)],
    unreachable: &[Ipv4Addr],
) -> Option<ConnectivityProofEvidence> {
    let unreachable_peers = peers
        .iter()
        .filter(|(_, gateway)| unreachable.contains(gateway))
        .map(|(machine_id, gateway)| ConnectivityProofUnreachablePeer {
            machine_id: machine_id.clone(),
            gateway: *gateway,
        })
        .collect::<Vec<_>>();
    if unreachable_peers.is_empty() {
        return None;
    }
    let evidence = match ConnectivityProofEvidence::try_new(unreachable_peers) {
        Ok(evidence) => evidence,
        Err(ConnectivityProofEvidenceError::Empty) => {
            unreachable!("non-empty connectivity evidence passed validation")
        }
    };
    Some(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_probe_result_becomes_connectivity_evidence() {
        let gateway = "10.198.1.1".parse().expect("valid gateway");

        let evidence = connectivity_proof_evidence(&[(machine_id("core_1"), gateway)], &[gateway]);

        assert_eq!(
            evidence,
            Some(
                ConnectivityProofEvidence::try_new(vec![ConnectivityProofUnreachablePeer {
                    machine_id: machine_id("core_1"),
                    gateway,
                }])
                .expect("non-empty connectivity evidence")
            )
        );
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }
}
