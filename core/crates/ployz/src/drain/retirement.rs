//! Retire a Drain's chosen Globals on the drained Server, one Global slot at a time, and
//! let go of the copies a move left Out there.

use ployz_core::error_chain;
use ployz_core::{
    AnonymousVolumes, EnvironmentValues, LiveServices, Machine, MachineId, MachineName,
    ObservedGlobalSlotSpec, QualifiedService, RpcError, ServicePlacementEligibility,
};
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError};
use crate::deploy::cleanup::{self, ACK_WAIT, Copy, EmptyPolicy, RotationFailure};
use crate::deploy::{CopyLifecycle, MachineRef, OldCopyEnd, Unconfirmed};
use crate::global_slot::{slot_eligibility, unknown_eligibility};

/// What retiring one Global on the drained Server came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Retirement {
    /// Its removal there was acknowledged, or a full observation found none of it left.
    Retired,
    /// It may still run there, and why.
    NotRetired(String),
    /// Cancellation came first.
    NotAttempted,
}

/// What retirement needs from a Cluster.
pub(crate) trait RetireClient {
    /// What every Server runs, with each Server's name.
    async fn live_services(&mut self) -> Result<(LiveServices<RpcError>, Names), ConnectError>;
    /// Retire `slot`'s Containers on `machine_id` when fresh evidence says it is definitely
    /// ineligible there; refuse when it is eligible or its eligibility unknown.
    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        names: &Names,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError>;
}

pub(crate) type Names = Vec<(MachineId, MachineName)>;

/// `id`'s Server name, or `id` itself when the membership doesn't list it.
pub(crate) fn server_name(names: &Names, id: &MachineId) -> String {
    names
        .iter()
        .find(|(named, _)| named == id)
        .map_or_else(|| id.to_string(), |(_, name)| name.to_string())
}

impl RetireClient for Client {
    async fn live_services(&mut self) -> Result<(LiveServices<RpcError>, Names), ConnectError> {
        let machines = self.machines().await?;
        let live = self
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await?;
        let names = machines
            .into_iter()
            .map(|machine| (machine.machine.id, machine.machine.name))
            .collect();
        Ok((live, names))
    }

    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        names: &Names,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        let identity = slot.identity();
        match slot_eligibility(self, machine_id, &identity.namespace, slot.resolved_spec()).await? {
            ServicePlacementEligibility::Ineligible(_) => {
                retire_copies(self, machine_id, identity, names, cancellation).await
            }
            ServicePlacementEligibility::Eligible => {
                Err(RpcError::conflict("the Server accepts it again"))
            }
            ServicePlacementEligibility::Unknown(reason) => Err(unknown_eligibility(reason)),
        }
    }
}

pub(crate) async fn copies_on<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
) -> Result<Vec<Copy>, RpcError> {
    Ok(client
        .service_containers(service)
        .await?
        .iter()
        .filter(|observation| observation.machine_id == *machine_id)
        .map(Copy::observed)
        .collect())
}

pub(crate) async fn retire_copies<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    names: &Names,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    for copy in copies_on(client, machine_id, service).await? {
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        ended(
            cleanup::retire(
                client,
                &copy,
                EmptyPolicy::RequireOtherIn,
                AnonymousVolumes::Remove,
                cancellation,
            )
            .await?
            .failure(),
            names,
        )?;
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
        for copy in copies_on(client, machine_id, &door).await? {
            if cancellation.is_cancelled() {
                return Err(cancelled());
            }
            ended(
                cleanup::remove(client, &copy, AnonymousVolumes::Remove)
                    .await?
                    .failure(),
                &Names::new(),
            )?;
        }
    }
    Ok(())
}

pub(crate) async fn let_go<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> OldCopyEnd {
    let unlisted = cleanup::wait_unlisted(
        client,
        &copy.machine_id,
        &copy.service,
        &copy.container_id,
        cancellation,
    )
    .await
    .map(cleanup::Waited::failure);
    let ended = match unlisted {
        Ok(Ok(())) => cleanup::remove(client, copy, AnonymousVolumes::Remove)
            .await
            .map(cleanup::Removal::failure),
        unlisted => unlisted,
    };
    let unconfirmed = match ended {
        Ok(Ok(())) => return OldCopyEnd::Stopped,
        Ok(Err(failure)) => Unconfirmed::of(failure, name),
        Err(error) => Unconfirmed::Failed {
            detail: error_chain::row(&error),
        },
    };
    OldCopyEnd::Running { unconfirmed }
}

fn ended(rotated: Result<(), RotationFailure>, names: &Names) -> Result<(), RpcError> {
    rotated.map_err(|failure| match failure {
        RotationFailure::Refused(refusal) => refusal.into_error(),
        RotationFailure::NotConfirmed(machines) => RpcError::unavailable(format!(
            "not unlisted after {} s; the proxies on {} did not confirm",
            ACK_WAIT.as_secs(),
            machines
                .machines
                .iter()
                .map(|id| server_name(names, id))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        RotationFailure::Cancelled => cancelled(),
    })
}

fn cancelled() -> RpcError {
    RpcError::unavailable("the drain was cancelled while it retired")
}

/// Retire `globals` on `server` only: each is stopped and removed there when fresh evidence
/// says the Server rules it out, and held when its eligibility is unknown. It starts
/// nothing, touches no other Global or Server, and stops at the next Global once
/// `cancellation` fires.
///
/// Returns each of `globals`, in order, with its retirement. The caller observes what
/// still runs.
pub(crate) async fn retire_globals<C: RetireClient>(
    client: &mut C,
    server: &Machine,
    globals: &[QualifiedService],
    cancellation: &CancellationToken,
) -> Vec<(QualifiedService, Retirement)> {
    let everyone = |error: String| {
        globals
            .iter()
            .map(|identity| (identity.clone(), Retirement::NotRetired(error.clone())))
            .collect()
    };
    let (live, names) = match client.live_services().await {
        Ok(observed) => observed,
        Err(error) => return everyone(error_chain::row(&error)),
    };
    if !live.containers.all_targets_succeeded() {
        return everyone(format!(
            "cannot retire from partial Service observations: {}",
            crate::failure::partial_failure_details(&live.containers, |id| server_name(&names, id))
        ));
    }
    let services = live.services();
    let mut retirements = Vec::with_capacity(globals.len());
    for global in globals {
        let retirement = if cancellation.is_cancelled() {
            Retirement::NotAttempted
        } else {
            match services.iter().find(|service| service.identity == *global) {
                None => Retirement::Retired,
                Some(service) => match service.observed_global_slot() {
                    None => Retirement::NotRetired(
                        "its newest Container is not Global; deploy it first".into(),
                    ),
                    Some(slot) => match client
                        .retire_slot(&server.id, &slot, &names, cancellation)
                        .await
                    {
                        Ok(()) => Retirement::Retired,
                        Err(error) => Retirement::NotRetired(error_chain::row(&error)),
                    },
                },
            }
        };
        retirements.push((global.clone(), retirement));
    }
    retirements
}

#[cfg(test)]
#[path = "retirement_tests.rs"]
mod tests;
