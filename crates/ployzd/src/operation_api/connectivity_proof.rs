//! Operation-owned overlay connectivity proof for a joining machine.

use std::net::Ipv4Addr;
use std::time::Duration;

use ployz_core::dataplane::{DataplaneMember, DataplanePrepareRequest, WireGuardPublicKey};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::{
    ConnectivityProofEvidence, ConnectivityProofUnreachablePeer, RawJoinToken,
};
use ployz_core::ops::{MachineAddOperationState, OperationStatus};
use ployz_sdk_types::MachineJoinReportError;

use crate::roles::machine::client::NatsMachineDataplanePreparer;

use super::OperationApiHandlers;
use super::error_map::{corrupt, machine_join_report_error};

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
            prove_overlay_connectivity(handlers, operation_id, machine_id, target.endpoint_subnet)
                .await
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
    joining_subnet: ployz_core::dataplane::MachineEndpointSubnet,
) -> Result<Option<ConnectivityProofEvidence>, MachineJoinReportError> {
    let mesh_lock = handlers.controllers.mesh_lock();
    let _mesh_guard = mesh_lock.lock().await;
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

    let active_members = existing_machines
        .iter()
        .map(|machine| DataplaneMember {
            machine_id: machine.machine_id.clone(),
            endpoint_subnet: machine.endpoint_subnet.clone(),
        })
        .collect::<Vec<_>>();
    let mut members = active_members.clone();
    members.push(DataplaneMember {
        machine_id: joining_machine_id.clone(),
        endpoint_subnet: joining_subnet,
    });
    let machine_ids = members
        .iter()
        .map(|member| member.machine_id.clone())
        .collect::<Vec<_>>();
    let preparer = NatsMachineDataplanePreparer::new(handlers.intent_change_client.clone())
        .with_request_timeout(CONNECTIVITY_PROOF_PREPARE_REQUEST_TIMEOUT);
    let proof = async {
        let ready_machines = preparer
            .prepare_wireguard(DataplanePrepareRequest {
                operation_id: operation_id.clone(),
                membership: members,
            })
            .await
            .map_err(|error| MachineJoinReportError::Unavailable {
                message: format!("wireguard prepare failed: {error:?}"),
            })?;

        // Each existing peer's WireGuard public key (reported by the prepare) is the
        // probe subject; its bridge gateway is retained only for operator-facing
        // evidence, not for reachability, which is decided by the handshake.
        let mut peers = Vec::with_capacity(existing_machines.len());
        for machine in &existing_machines {
            let Some(ready) = ready_machines
                .iter()
                .find(|ready| ready.machine_id == machine.machine_id)
            else {
                return Err(MachineJoinReportError::Unavailable {
                    message: corrupt("dataplane prepare report is missing an active machine"),
                });
            };
            peers.push((
                machine.machine_id.clone(),
                ready.readiness.public_key.clone(),
                machine.endpoint_subnet.bridge_gateway_ipv4(),
            ));
        }
        let public_keys = peers.iter().map(|(_, key, _)| key.clone()).collect();
        let unreachable = preparer
            .probe_overlay(
                operation_id.clone(),
                &joining_machine_id,
                machine_ids,
                public_keys,
            )
            .await
            .map_err(|error| MachineJoinReportError::Unavailable {
                message: format!("overlay connectivity probe failed: {error:?}"),
            })?;
        Ok(connectivity_proof_evidence(&peers, &unreachable))
    }
    .await;

    match proof {
        Ok(None) => Ok(None),
        result => {
            // Rolling the rejected joiner back out of the active mesh is
            // best-effort: a cleanup failure must not erase the proof verdict
            // (the typed unreachable evidence or the proof-infrastructure
            // error) that this operation exists to record. A lingering peer
            // entry converges out on the next successful mesh prepare.
            let _ = restore_active_mesh(&preparer, operation_id, active_members).await;
            result
        }
    }
}

async fn restore_active_mesh(
    preparer: &NatsMachineDataplanePreparer,
    operation_id: OperationId,
    membership: Vec<DataplaneMember>,
) -> Result<(), ployz_core::dataplane::WireGuardEbpfPrepareError> {
    preparer
        .prepare_wireguard(DataplanePrepareRequest {
            operation_id,
            membership,
        })
        .await
        .map(|_| ())
}

fn connectivity_proof_evidence(
    peers: &[(MachineId, WireGuardPublicKey, Ipv4Addr)],
    unreachable: &[WireGuardPublicKey],
) -> Option<ConnectivityProofEvidence> {
    let unreachable_peers = peers
        .iter()
        .filter(|(_, key, _)| unreachable.contains(key))
        .map(
            |(machine_id, _, gateway)| ConnectivityProofUnreachablePeer {
                machine_id: machine_id.clone(),
                gateway: *gateway,
            },
        )
        .collect::<Vec<_>>();
    ConnectivityProofEvidence::try_new(unreachable_peers).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_public_key_becomes_connectivity_evidence() {
        let gateway = "10.198.1.1".parse().expect("valid gateway");
        let key = public_key("core-1-public-key");

        let evidence =
            connectivity_proof_evidence(&[(machine_id("core_1"), key.clone(), gateway)], &[key]);

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

    #[test]
    fn reachable_public_key_yields_no_evidence() {
        let gateway = "10.198.1.1".parse().expect("valid gateway");
        let key = public_key("core-1-public-key");

        assert_eq!(
            connectivity_proof_evidence(&[(machine_id("core_1"), key, gateway)], &[]),
            None
        );
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn public_key(value: &str) -> WireGuardPublicKey {
        WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
    }
}
