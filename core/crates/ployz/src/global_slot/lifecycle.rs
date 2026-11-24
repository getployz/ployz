//! A Machine's own view of itself, and removing its Ingress Proxy.

use ployz_core::{
    AnonymousVolumes, InspectRequest, LocalMachinePhase, Machine, MachineId, MachineTarget,
    QualifiedService, RpcError, op,
};
use tokio_util::sync::CancellationToken;

use crate::cluster::Call;
use crate::connect::Client;
use crate::deploy::CopyLifecycle;
use crate::deploy::cleanup::{self, Copy, Removal};

/// `machine_id`'s own record, read from that Machine.
///
/// # Errors
///
/// Fails when the Machine does not answer, or does not observe itself participating.
pub(crate) async fn participating(
    client: &mut Client,
    machine_id: &MachineId,
) -> Result<Machine, RpcError> {
    let details = client
        .call::<op::Inspect>(
            InspectRequest::default(),
            Call::target_read(&MachineTarget::from(machine_id)),
        )
        .await
        .map_err(crate::connect::rpc_error)?;
    details
        .machine
        .filter(|machine| {
            machine.id == *machine_id && details.phase == LocalMachinePhase::Participating
        })
        .ok_or_else(|| RpcError::conflict("the Server does not observe itself participating"))
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
                cleanup::remove(client, &Copy::observed(&observed), AnonymousVolumes::Remove)
                    .await?
            {
                return Err(refusal.into_error());
            }
        }
    }
    Ok(())
}
