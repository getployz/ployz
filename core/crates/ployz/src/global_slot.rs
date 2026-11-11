//! A Global's slot on one Machine: its fresh eligibility there.

use crate::cluster::Call;
use ployz_core::{
    InspectRequest, MachineId, MachineTarget, Namespace, ResolvedServiceSpec, RpcError,
    ServicePlacementEligibility, ServicePlacementUnknownReason, op,
};

use crate::connect::Client;

/// Fresh eligibility of `spec` on `machine_id`, from that Machine's own observation.
///
/// # Errors
///
/// Fails when the Machine does not answer, or does not observe itself participating.
pub(crate) async fn slot_eligibility(
    client: &mut Client,
    machine_id: &MachineId,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<ServicePlacementEligibility, RpcError> {
    let details = client
        .call::<op::Inspect>(
            InspectRequest {
                include_storage: true,
                ..Default::default()
            },
            Call::read()
                .to(&MachineTarget::from(machine_id))
                .budget(crate::connect::target_read_timeout()),
        )
        .await
        .map_err(crate::connect::rpc_error)?;
    let machine = details
        .machine
        .filter(|machine| {
            machine.id == *machine_id
                && details.phase == ployz_core::LocalMachinePhase::Participating
        })
        .ok_or_else(|| RpcError::conflict("the Server does not observe itself participating"))?;
    Ok(spec.placement_eligibility_in_namespace(namespace, &machine, details.storage.as_ref()))
}

/// The refusal for a slot whose eligibility on the Server can't be told.
pub(crate) fn unknown_eligibility(reason: ServicePlacementUnknownReason) -> RpcError {
    let why = match reason {
        ServicePlacementUnknownReason::MissingStorageEvidence => {
            "its storage support was not observed"
        }
    };
    RpcError::conflict(format!("eligibility on the Server is unknown: {why}"))
}
