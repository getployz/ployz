//! Retire a Drain's chosen Globals on the drained Server, one Global slot at a time.

use ployz_core::{
    EnvironmentValues, LiveServices, Machine, MachineId, ObservedGlobalSlotSpec, QualifiedService,
    RpcError, RpcErrorCode, ServicePlacementEligibility,
};
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError};
use crate::global_slot::{remove_slot, slot_eligibility, unknown_eligibility};

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
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, ConnectError>;
    /// Stop and remove `slot`'s Containers on `machine_id` when fresh evidence says it is
    /// definitely ineligible there; refuse when it is eligible or its eligibility unknown.
    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
    ) -> Result<(), RpcError>;
}

impl RetireClient for Client {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, ConnectError> {
        Client::live_services(self, EnvironmentValues::Redacted).await
    }

    async fn retire_slot(
        &mut self,
        machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
    ) -> Result<(), RpcError> {
        let identity = slot.identity();
        match slot_eligibility(self, machine_id, &identity.namespace, slot.resolved_spec()).await? {
            ServicePlacementEligibility::Ineligible(_) => {
                remove_slot(self, machine_id, &identity.namespace, &identity.name).await
            }
            ServicePlacementEligibility::Eligible => Err(RpcError {
                code: RpcErrorCode::Conflict,
                message: "the Server accepts it again".into(),
                details: serde_json::Value::Null,
            }),
            ServicePlacementEligibility::Unknown(reason) => Err(unknown_eligibility(reason)),
        }
    }
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
        Ok(live) => live,
        Err(error) => return everyone(crate::ui::inline(&error)),
    };
    if !live.containers.all_targets_succeeded() {
        return everyone(format!(
            "cannot retire from partial Service observations: {}",
            crate::failure::partial_failure_details(&live.containers)
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
                    Some(slot) => match client.retire_slot(&server.id, &slot).await {
                        Ok(()) => Retirement::Retired,
                        Err(error) => Retirement::NotRetired(crate::ui::inline(&error)),
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
