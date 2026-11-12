//! Retiring a Global's copies from one Machine: whether the Machine still takes the slot,
//! the copies it holds, and the sentence a failed retirement is told in.

use ployz_core::error_chain;
use ployz_core::{
    AnonymousVolumes, InspectRequest, LocalMachinePhase, Machine, MachineId,
    MachineStorageObservation, MachineTarget, Namespace, QualifiedService, ResolvedServiceSpec,
    RpcError, ServicePlacementEligibility, op,
};
use tokio_util::sync::CancellationToken;

use super::observe::ServerNames;
use super::slot::unknown_eligibility;
use crate::cluster::Call;
use crate::connect::Client;
use crate::deploy::CopyLifecycle;
use crate::deploy::cleanup::{self, ACK_WAIT, Copy, EmptyPolicy, RotationFailure};
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
            Call::read()
                .to(&MachineTarget::from(machine_id))
                .budget(crate::connect::target_read_timeout()),
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

/// The words a failed retirement is told in. Each caller keeps its own, because what a
/// person is told depends on whose command it was.
pub(crate) enum Sentence<'names> {
    /// A Drain, which names each proxy that did not confirm by its Server's name and stops
    /// before it takes another copy out once cancelled.
    Drain(&'names ServerNames),
    /// Catch-up, which names each such proxy by id and reports a cancel as internal.
    CatchUp,
}

impl Sentence<'_> {
    pub(crate) fn error(&self, failure: RotationFailure) -> RpcError {
        match (self, failure) {
            (_, RotationFailure::Refused(refusal)) => refusal.into_error(),
            (Self::Drain(names), RotationFailure::NotConfirmed(unconfirmed)) => {
                RpcError::unavailable(format!(
                    "not unlisted after {} s; the proxies on {} did not confirm",
                    ACK_WAIT.as_secs(),
                    unconfirmed
                        .machines
                        .iter()
                        .map(|id| names.name(id))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
            (Self::CatchUp, RotationFailure::NotConfirmed(unconfirmed)) => {
                RpcError::unavailable(format!(
                    "{} did not confirm the copy left its routes",
                    unconfirmed
                        .machines
                        .iter()
                        .map(MachineId::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
            (Self::Drain(_), RotationFailure::Cancelled) => {
                RpcError::unavailable("the drain was cancelled while it retired")
            }
            (Self::CatchUp, RotationFailure::Cancelled) => {
                error_chain::rpc_error(ployz_core::RpcErrorCode::Internal, &Failure::cancelled())
            }
        }
    }

    fn stops_between_copies(&self) -> bool {
        matches!(self, Self::Drain(_))
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
    sentence: &Sentence<'_>,
) -> Result<Eligibility, RpcError> {
    let view = participating(client, machine_id, true).await?;
    match spec.placement_eligibility_in_namespace(namespace, &view.machine, view.storage.as_ref()) {
        ServicePlacementEligibility::Eligible => Ok(Eligibility::StillEligible),
        ServicePlacementEligibility::Unknown(reason) => Err(unknown_eligibility(reason)),
        ServicePlacementEligibility::Ineligible(_) => {
            let service = QualifiedService::new(namespace.clone(), spec.name.clone());
            retire_copies(&*client, machine_id, &service, cancellation, sentence).await?;
            Ok(Eligibility::Retired)
        }
    }
}

pub(crate) async fn retire_copies<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    cancellation: &CancellationToken,
    sentence: &Sentence<'_>,
) -> Result<(), RpcError> {
    for observed in client.target_copies(machine_id, service).await? {
        if sentence.stops_between_copies() && cancellation.is_cancelled() {
            return Err(sentence.error(RotationFailure::Cancelled));
        }
        cleanup::retire(
            client,
            &Copy::observed(&observed),
            EmptyPolicy::RequireOtherIn,
            AnonymousVolumes::Remove,
            cancellation,
        )
        .await?
        .failure()
        .map_err(|failure| sentence.error(failure))?;
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
    let sentence = Sentence::Drain(&ServerNames::default());
    for door in [
        QualifiedService::system_tunnel(),
        QualifiedService::system_ingress(),
        QualifiedService::system_ingress_holder(),
    ] {
        for observed in client.target_copies(machine_id, &door).await? {
            if cancellation.is_cancelled() {
                return Err(sentence.error(RotationFailure::Cancelled));
            }
            cleanup::remove(client, &Copy::observed(&observed), AnonymousVolumes::Remove)
                .await?
                .failure()
                .map_err(|failure| sentence.error(failure))?;
        }
    }
    Ok(())
}
