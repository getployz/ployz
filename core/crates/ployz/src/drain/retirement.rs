//! Retire a Drain's chosen Globals on the drained Server, one Global slot at a time, and
//! let go of the copies a move left Out there.

use ployz_core::error_chain;
use ployz_core::{
    AnonymousVolumes, EnvironmentValues, Machine, MachineId, ObservedGlobalSlotSpec,
    QualifiedService, RpcError,
};
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError};
use crate::deploy::cleanup::{self, Copy};
use crate::deploy::{CopyLifecycle, MachineRef, OldCopyEnd, Unconfirmed};
use crate::global_slot::{Eligibility, Live, Sentence, ServerNames, retire_if_ineligible};

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
    async fn live_services(&mut self) -> Result<Live, ConnectError>;
    /// Retire `slot`'s Containers on `machine_id` when fresh evidence says it is definitely
    /// ineligible there; refuse when it is eligible or its eligibility unknown.
    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        names: &ServerNames,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError>;
}

impl RetireClient for Client {
    async fn live_services(&mut self) -> Result<Live, ConnectError> {
        let machines = self.machines().await?;
        let services = self
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await?;
        Ok(Live {
            services,
            names: ServerNames::of(&machines),
        })
    }

    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        names: &ServerNames,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        let namespace = &slot.identity().namespace;
        match retire_if_ineligible(
            self,
            machine_id,
            namespace,
            slot.resolved_spec(),
            cancellation,
            &Sentence::Drain(names),
        )
        .await?
        {
            Eligibility::Retired => Ok(()),
            Eligibility::StillEligible => Err(RpcError::conflict("the Server accepts it again")),
        }
    }
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
    let live = match client.live_services().await {
        Ok(observed) => observed,
        Err(error) => return everyone(error_chain::row(&error)),
    };
    if let Some(details) = live.partial() {
        return everyone(format!(
            "cannot retire from partial Service observations: {details}"
        ));
    }
    let services = live.services.services();
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
                        .retire_slot(&server.id, &slot, &live.names, cancellation)
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
