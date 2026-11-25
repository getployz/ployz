//! Retiring a Global's copies from a joining Machine that rules the slot out, and the
//! sentence a failed retirement is told in.

use ployz_core::error_chain;
use ployz_core::{
    AnonymousVolumes, InspectRequest, LocalMachinePhase, Machine, MachineId,
    MachineStorageObservation, MachineTarget, Namespace, QualifiedService, ResolvedServiceSpec,
    RpcError, ServicePlacementEligibility, op,
};
use tokio_util::sync::CancellationToken;

use super::slot::unknown_eligibility;
use crate::cluster::Call;
use crate::connect::Client;
use crate::deploy::CopyLifecycle;
use crate::deploy::cleanup::{self, Copy, EmptyPolicy, Removal, RotationFailure};
use crate::failure::Failure;

/// A Machine's own view of itself while it participates.
pub(crate) struct SelfView {
    pub(crate) machine: Machine,
    pub(crate) storage: Option<MachineStorageObservation>,
}

/// `machine_id`'s own record, read from that Machine.
///
/// # Errors
///
/// Fails when the Machine does not answer, or does not observe itself participating.
pub(crate) async fn participating(
    client: &mut Client,
    machine_id: &MachineId,
    include_storage: bool,
) -> Result<SelfView, RpcError> {
    let details = client
        .call::<op::Inspect>(
            InspectRequest {
                include_storage,
                ..Default::default()
            },
            Call::target_read(&MachineTarget::from(machine_id)),
        )
        .await
        .map_err(crate::connect::rpc_error)?;
    let machine = details
        .machine
        .filter(|machine| {
            machine.id == *machine_id && details.phase == LocalMachinePhase::Participating
        })
        .ok_or_else(|| RpcError::conflict("the Server does not observe itself participating"))?;
    Ok(SelfView {
        machine,
        storage: details.storage,
    })
}

/// What asking a Machine to let go of a Global's slot came to.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Eligibility {
    /// Fresh evidence says the Machine rules the slot out, and its copies are gone.
    Retired,
    /// The Machine takes the slot, so nothing was retired.
    StillEligible,
}

/// What a catch-up retirement that did not finish is told as: each proxy that did not
/// confirm by id, and a cancel as internal.
pub(crate) fn retirement_error(failure: RotationFailure) -> RpcError {
    match failure {
        RotationFailure::Refused(refusal) => refusal.into_error(),
        RotationFailure::NotConfirmed(unconfirmed) => RpcError::unavailable(format!(
            "{} did not confirm the copy left its routes",
            unconfirmed
                .machines
                .iter()
                .map(MachineId::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )),
        RotationFailure::Cancelled => {
            error_chain::rpc_error(ployz_core::RpcErrorCode::Internal, &Failure::cancelled())
        }
    }
}

/// Retire `service`'s copies on `machine_id` when fresh evidence from that Machine says it
/// rules `spec` out in `namespace`. Refuses when its eligibility can't be told.
///
/// # Errors
///
/// Fails when the Machine can't be read, can't tell its eligibility, or a copy does not
/// retire.
pub(crate) async fn retire_if_ineligible(
    client: &mut Client,
    machine_id: &MachineId,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    cancellation: &CancellationToken,
) -> Result<Eligibility, RpcError> {
    let view = participating(client, machine_id, true).await?;
    match spec.placement_eligibility_in_namespace(namespace, &view.machine, view.storage.as_ref()) {
        ServicePlacementEligibility::Eligible => Ok(Eligibility::StillEligible),
        ServicePlacementEligibility::Unknown(reason) => Err(unknown_eligibility(reason)),
        ServicePlacementEligibility::Ineligible(_) => {
            let service = QualifiedService::new(namespace.clone(), spec.name.clone());
            retire_copies(&*client, machine_id, &service, cancellation).await?;
            Ok(Eligibility::Retired)
        }
    }
}

async fn retire_copies<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    for observed in client.target_copies(machine_id, service).await? {
        cleanup::retire(
            client,
            &Copy::observed(&observed),
            EmptyPolicy::RequireOtherIn,
            AnonymousVolumes::keep(),
            cancellation,
        )
        .await?
        .failure()
        .map_err(retirement_error)?;
    }
    Ok(())
}

/// Remove `machine_id`'s Ingress Proxy: every Tunnel Connector and Caddy, then every
/// holder. Docker refuses to remove a holder while a Container joined to its network
/// exists, so one that stays stops the removal before any holder.
pub(crate) async fn retire_ingress_proxy<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    for door in [
        QualifiedService::system_tunnel(),
        QualifiedService::system_ingress(),
        QualifiedService::system_ingress_holder(),
    ] {
        for observed in client.target_copies(machine_id, &door).await? {
            if cancellation.is_cancelled() {
                return Err(RpcError::unavailable(
                    "the Ingress Proxy's removal was cancelled",
                ));
            }
            if let Removal::Refused(refusal) =
                cleanup::remove(client, &Copy::observed(&observed), AnonymousVolumes::keep())
                    .await?
            {
                return Err(refusal.into_error());
            }
        }
    }
    Ok(())
}
