use super::response::{failure_message, machine_domain_error, machine_success};
use super::service::MachinePloyzNativeMeshPreparer;
use crate::roles::machine::protocol::{
    MachineDataplanePrepareRpcRequest, MachineDataplanePrepareRpcResponse,
    MachinePloyzNativeMeshPrepareDomainError, MachinePloyzNativeMeshPrepareRpcOk,
    MachinePloyzNativeMeshPrepareRpcRequest,
};
use ployz_core::dataplane::PloyzNativeMeshMachineReady;
use ployz_core::ids::MachineId;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};

pub(crate) async fn handle_dataplane_prepare<P>(
    machine_id: MachineId,
    preparer: P,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    P: MachinePloyzNativeMeshPreparer,
{
    let request = match decode_json_request::<MachineDataplanePrepareRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if !request
        .machines
        .iter()
        .any(|candidate| candidate == &machine_id)
    {
        return machine_domain_error(MachineDataplanePrepareRpcResponse::DomainError {
            machine_id: machine_id.clone(),
            error: MachinePloyzNativeMeshPrepareDomainError::Unavailable {
                component: ployz_core::dataplane::PloyzNativeMeshComponent::WireGuard,
                message: failure_message(
                    "ployz native mesh prepare request did not target this machine",
                ),
            },
        });
    }

    match request.request {
        MachinePloyzNativeMeshPrepareRpcRequest::ReadPublicKey => {
            match preparer.read_wireguard_public_key().await {
                Ok(public_key) => machine_success(MachineDataplanePrepareRpcResponse::Ok(
                    MachinePloyzNativeMeshPrepareRpcOk::PublicKey {
                        machine_id,
                        public_key,
                    },
                )),
                Err(error) => {
                    machine_domain_error(MachineDataplanePrepareRpcResponse::DomainError {
                        machine_id,
                        error: error.into(),
                    })
                }
            }
        }
        MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
            endpoint_routes,
            peers,
        } => {
            match preparer
                .prepare_ployz_native_mesh(&endpoint_routes, &peers)
                .await
            {
                Ok(ready) => machine_success(MachineDataplanePrepareRpcResponse::Ok(
                    MachinePloyzNativeMeshPrepareRpcOk::Ready {
                        readiness: PloyzNativeMeshMachineReady { machine_id, ready },
                    },
                )),
                Err(error) => {
                    machine_domain_error(MachineDataplanePrepareRpcResponse::DomainError {
                        machine_id,
                        error: error.into(),
                    })
                }
            }
        }
        MachinePloyzNativeMeshPrepareRpcRequest::ProbeOverlay { targets } => {
            let unreachable = preparer.probe_overlay(&targets).await;
            machine_success(MachineDataplanePrepareRpcResponse::Ok(
                MachinePloyzNativeMeshPrepareRpcOk::OverlayProbe {
                    machine_id,
                    unreachable,
                },
            ))
        }
    }
}
