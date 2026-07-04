//! Load deploy execution facts from current-state stores.

use futures_util::{StreamExt, stream};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::state::placement_rejection;
use ployz_core::ops::UnusableMachine;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use std::fmt;
use std::time::Duration;

use super::DeployExecutionFacts;
use super::preparation::namespace_cleanup_candidates;

const MAX_CONCURRENT_OBSERVATION_READS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionMachineScope {
    pub eligible_machines: Vec<MachineId>,
    pub observed_machine_ids: Vec<MachineId>,
}

impl DeployExecutionMachineScope {
    #[must_use]
    pub fn same_machines(machines: Vec<MachineId>) -> Self {
        Self {
            eligible_machines: machines.clone(),
            observed_machine_ids: machines,
        }
    }
}

pub async fn load_deploy_execution_facts_from_nats(
    request: &DeployRequest,
    machine_scope: DeployExecutionMachineScope,
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let route_bindings = async {
        core_state
            .route_bindings()
            .await
            .map_err(|source| DeployFactLoadError::RouteBindingsRead {
                message: source.to_string(),
            })
    };
    let serving_entries = async {
        core_state.serving_target_entries().await.map_err(|source| {
            DeployFactLoadError::ServingTargetEntriesRead {
                message: source.to_string(),
            }
        })
    };
    let machine_scope = load_active_machine_scope(core_state, machine_scope);
    let (route_bindings, serving_entries, (machine_scope, unusable_machines)) =
        tokio::try_join!(route_bindings, serving_entries, machine_scope)?;
    let namespace_route_bindings = route_bindings
        .into_iter()
        .filter(|binding| binding.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let namespace_serving_entries = serving_entries
        .into_iter()
        .filter(|entry| entry.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let observed_machines =
        load_machine_snapshots(observations, &machine_scope.observed_machine_ids).await?;
    let dataplane_machines =
        routed_dataplane_machines(request, machine_scope.observed_machine_ids.clone());
    let namespace_cleanup_candidates =
        namespace_cleanup_candidates(&request.namespace_id, &observed_machines);
    Ok(DeployExecutionFacts {
        namespace_route_bindings,
        namespace_serving_entries,
        eligible_machines: machine_scope.eligible_machines,
        unusable_machines,
        dataplane_machines,
        observed_machines,
        namespace_cleanup_candidates,
        step_timeout,
    })
}

fn routed_dataplane_machines(
    request: &DeployRequest,
    fallback_machines: Vec<MachineId>,
) -> Vec<MachineId> {
    if request
        .services
        .iter()
        .all(|service| service.routes.is_empty())
    {
        return Vec::new();
    }

    sorted_unique_machines(fallback_machines.iter())
}

/// Placement excludes machines by durable operator intent only: draining
/// machines take no new work, with the rejection carried as typed evidence.
/// Liveness is not consulted — a dead machine answers at the point of use
/// (ADR 0027). The pre-cluster fallback scope (no machine records yet) is
/// used unfiltered. This control-side gate is interim: with bid-based
/// placement the draining machine declines its own bids.
async fn load_active_machine_scope(
    core_state: &AsyncNatsCoreStateStore,
    fallback: DeployExecutionMachineScope,
) -> Result<(DeployExecutionMachineScope, Vec<UnusableMachine>), DeployFactLoadError> {
    let machines = core_state.active_machines().await.map_err(|source| {
        DeployFactLoadError::ActiveMachineRead {
            message: source.to_string(),
        }
    })?;
    if machines.is_empty() {
        return Ok((fallback, Vec::new()));
    }

    let mut eligible = Vec::new();
    let mut observed_ids = Vec::new();
    let mut unusable = Vec::new();
    for machine in machines {
        observed_ids.push(machine.machine_id.clone());
        match placement_rejection(machine.lifecycle) {
            None => eligible.push(machine.machine_id),
            Some(reason) => unusable.push(UnusableMachine {
                machine_id: machine.machine_id,
                reason,
            }),
        }
    }

    Ok((
        DeployExecutionMachineScope {
            eligible_machines: eligible,
            observed_machine_ids: observed_ids,
        },
        unusable,
    ))
}

async fn load_machine_snapshots(
    observations: &AsyncNatsObservationStore,
    machine_ids: &[MachineId],
) -> Result<Vec<MachineContainerObservationSnapshot>, DeployFactLoadError> {
    let mut snapshots = Vec::new();
    let mut reads = stream::iter(machine_ids.iter().cloned())
        .map(|machine_id| async move {
            observations
                .machine_snapshot(&machine_id)
                .await
                .map_err(|source| DeployFactLoadError::MachineObservationRead {
                    machine_id,
                    message: source.to_string(),
                })
        })
        .buffer_unordered(MAX_CONCURRENT_OBSERVATION_READS);

    while let Some(snapshot) = reads.next().await {
        if let Some(snapshot) = snapshot? {
            snapshots.push(snapshot);
        }
    }

    Ok(snapshots)
}

fn sorted_unique_machines<'a>(machines: impl IntoIterator<Item = &'a MachineId>) -> Vec<MachineId> {
    machines
        .into_iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A current-state read failed before deploy execution started. Each variant
/// carries the rendered store-error message as failure evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployFactLoadError {
    RouteBindingsRead {
        message: String,
    },
    ServingTargetEntriesRead {
        message: String,
    },
    ActiveMachineRead {
        message: String,
    },
    MachineObservationRead {
        machine_id: MachineId,
        message: String,
    },
}

impl fmt::Display for DeployFactLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RouteBindingsRead { message } => write!(
                formatter,
                "route binding state could not be read: {}",
                message
            ),
            Self::ServingTargetEntriesRead { message } => write!(
                formatter,
                "serving target entries could not be read: {}",
                message
            ),
            Self::ActiveMachineRead { message } => {
                write!(formatter, "active machines could not be read: {message}")
            }
            Self::MachineObservationRead {
                machine_id,
                message,
            } => write!(
                formatter,
                "machine observations for {} could not be read: {}",
                machine_id.as_str(),
                message
            ),
        }
    }
}
