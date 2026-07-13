use super::response::{failure_message, machine_domain_error, machine_success};
use super::service::MachinePloyzNativeMeshPreparer;
use crate::roles::machine::protocol::{
    MachineDataplanePublicKeyDomainError, MachineDataplanePublicKeyRpcOk,
    MachineDataplanePublicKeyRpcRequest, MachineDataplanePublicKeyRpcResponse,
    MachineDataplaneStatusDomainError, MachineDataplaneStatusRpcOk,
    MachineDataplaneStatusRpcRequest, MachineDataplaneStatusRpcResponse,
};
use ployz_core::dataplane::{
    DataplaneProjectionComponent, DataplaneProjectionFailure, DataplaneProjectionTestimony,
    EndpointBridgeStatus, NativeDataplaneProjectionStatus,
};
use ployz_core::ids::MachineId;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};

use super::projection::MachineProjectionState;
use super::runner::MachineContainerRunner;

#[derive(Clone)]
pub(crate) struct MachineDataplaneStatusState<R, P> {
    pub(crate) runner: R,
    pub(crate) preparer: P,
    pub(crate) projection: MachineProjectionState,
}

pub(crate) async fn handle_dataplane_status<R, P>(
    machine_id: MachineId,
    state: MachineDataplaneStatusState<R, P>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
    P: MachinePloyzNativeMeshPreparer,
{
    let request = match decode_json_request::<MachineDataplaneStatusRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match state
        .preparer
        .read_ployz_native_mesh_status(request.mode)
        .await
    {
        Ok(mut value) => {
            let projection = state.projection.snapshot();
            value.projection = match state.runner.read_endpoint_network_status().await {
                Some(endpoint_bridge) => merge_endpoint_bridge_status(projection, endpoint_bridge),
                None => projection,
            };
            machine_success(MachineDataplaneStatusRpcResponse::Ok(
                MachineDataplaneStatusRpcOk { machine_id, value },
            ))
        }
        Err(message) => machine_domain_error(MachineDataplaneStatusRpcResponse::DomainError {
            machine_id,
            error: MachineDataplaneStatusDomainError::ReadFailed {
                message: failure_message(message),
            },
        }),
    }
}

fn merge_endpoint_bridge_status(
    mut projection: NativeDataplaneProjectionStatus,
    endpoint_bridge: EndpointBridgeStatus,
) -> NativeDataplaneProjectionStatus {
    if !matches!(endpoint_bridge, EndpointBridgeStatus::Ready { .. })
        && let DataplaneProjectionTestimony::Applied { revisions } = &projection.testimony
    {
        projection.testimony = DataplaneProjectionTestimony::Unusable {
            attempted_revisions: Some(revisions.clone()),
            last_applied_revisions: Some(revisions.clone()),
            failure: endpoint_bridge_failure(&endpoint_bridge),
        };
    }
    projection.endpoint_bridge = endpoint_bridge;
    projection
}

fn endpoint_bridge_failure(status: &EndpointBridgeStatus) -> DataplaneProjectionFailure {
    match status {
        EndpointBridgeStatus::Missing => DataplaneProjectionFailure::EndpointBridgeMissing,
        EndpointBridgeStatus::SubnetMismatch { expected, observed } => {
            DataplaneProjectionFailure::EndpointBridgeSubnetMismatch {
                expected: expected.clone(),
                observed: observed.clone(),
            }
        }
        EndpointBridgeStatus::InvalidSubnet { observed } => {
            DataplaneProjectionFailure::ApplyFailed {
                component: DataplaneProjectionComponent::EndpointBridge,
                message: failure_message(format!(
                    "Docker endpoint network has invalid subnet {observed:?}"
                )),
            }
        }
        EndpointBridgeStatus::Unavailable { message } => DataplaneProjectionFailure::ApplyFailed {
            component: DataplaneProjectionComponent::EndpointBridge,
            message: message.clone(),
        },
        EndpointBridgeStatus::Ready { .. } => unreachable!("ready bridge is not a failure"),
    }
}

pub(crate) async fn handle_dataplane_public_key<P>(
    machine_id: MachineId,
    preparer: P,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    P: MachinePloyzNativeMeshPreparer,
{
    let request = match decode_json_request::<MachineDataplanePublicKeyRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if !request
        .machines
        .iter()
        .any(|candidate| candidate == &machine_id)
    {
        return machine_domain_error(MachineDataplanePublicKeyRpcResponse::DomainError {
            machine_id: machine_id.clone(),
            error: MachineDataplanePublicKeyDomainError::Unavailable {
                component: ployz_core::dataplane::PloyzNativeMeshComponent::WireGuard,
                message: failure_message(
                    "dataplane public key request did not target this machine",
                ),
            },
        });
    }

    match preparer.read_wireguard_public_key().await {
        Ok(public_key) => machine_success(MachineDataplanePublicKeyRpcResponse::Ok(
            MachineDataplanePublicKeyRpcOk {
                machine_id,
                public_key,
            },
        )),
        Err(error) => machine_domain_error(MachineDataplanePublicKeyRpcResponse::DomainError {
            machine_id,
            error: error.into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::dataplane::{
        DataplaneProjection, DataplaneProjectionMember, DataplaneProjectionRevisions,
        EndpointBridgeStatus, MachineEndpointSubnet, WireGuardPublicKey,
    };

    #[test]
    fn missing_live_bridge_overrides_applied_testimony_and_retains_revision() {
        let subnet = MachineEndpointSubnet::try_new("10.198.1.0/24").expect("subnet");
        let view = DataplaneProjection::try_new(
            vec![DataplaneProjectionMember {
                machine_id: MachineId::try_new("machine_a").expect("machine id"),
                endpoint_subnet: subnet.clone(),
                mesh_endpoints: Vec::new(),
                wireguard_public_key: WireGuardPublicKey::try_new("key-a").expect("key"),
            }],
            None,
        )
        .expect("projection");
        let revisions = DataplaneProjectionRevisions {
            declared_revision: view.declared_revision,
            target_revision: view.target_revision,
        };
        let cached = NativeDataplaneProjectionStatus {
            endpoint_bridge: EndpointBridgeStatus::Ready { subnet },
            testimony: DataplaneProjectionTestimony::Applied {
                revisions: revisions.clone(),
            },
        };

        let status = merge_endpoint_bridge_status(cached, EndpointBridgeStatus::Missing);

        assert_eq!(status.endpoint_bridge, EndpointBridgeStatus::Missing);
        assert!(matches!(
            status.testimony,
            DataplaneProjectionTestimony::Unusable {
                attempted_revisions: Some(attempted),
                last_applied_revisions: Some(last),
                failure: DataplaneProjectionFailure::EndpointBridgeMissing,
            } if attempted == revisions && last == revisions
        ));
    }
}
