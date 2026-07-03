//! Load deploy execution facts from current-state stores.

use futures_util::{StreamExt, stream};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{MachineId, ServiceId};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use std::fmt;
use std::time::Duration;

use super::preparation::namespace_cleanup_candidates;
use super::{DeployExecutionFacts, DeployServiceExecutionFacts};

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
    let service_requests = request.service_requests();
    let route_bindings = core_state.route_bindings().await.map_err(|source| {
        DeployFactLoadError::RouteBindingsRead {
            message: source.to_string(),
        }
    })?;
    let namespace_route_bindings = route_bindings
        .iter()
        .filter(|binding| binding.namespace_id == request.namespace_id)
        .cloned()
        .collect::<Vec<_>>();
    let namespace_serving_entries = core_state
        .serving_target_entries()
        .await
        .map_err(|source| DeployFactLoadError::ServingTargetEntriesRead {
            message: source.to_string(),
        })?
        .into_iter()
        .filter(|entry| entry.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let mut service_facts = Vec::new();
    for service in &service_requests {
        let serving_target_entry = core_state
            .serving_target_entry(&service.namespace_id, &service.service_id)
            .await
            .map_err(|source| DeployFactLoadError::ServingTargetEntryRead {
                service_id: service.service_id.clone(),
                message: source.to_string(),
            })?;
        let route_bindings = route_bindings
            .iter()
            .filter(|route| {
                route.namespace_id == service.namespace_id
                    && route.service_id == service.service_id
            })
            .cloned()
            .collect();
        service_facts.push(DeployServiceExecutionFacts {
            serving_target_entry,
            route_bindings,
        });
    }
    let machine_scope = load_active_machine_scope(core_state, machine_scope).await?;
    let observed_machines =
        load_machine_snapshots(observations, &machine_scope.observed_machine_ids).await?;
    let dataplane_machines = routed_dataplane_machines(
        &service_requests,
        machine_scope.observed_machine_ids.clone(),
    );
    let namespace_cleanup_candidates = namespace_cleanup_candidates(&observed_machines);
    Ok(DeployExecutionFacts {
        services: service_facts,
        namespace_route_bindings,
        namespace_serving_entries,
        eligible_machines: machine_scope.eligible_machines,
        dataplane_machines,
        observed_machines,
        namespace_cleanup_candidates,
        step_timeout,
    })
}

fn routed_dataplane_machines(
    services: &[ployz_core::deploy::DeployServiceRequest],
    fallback_machines: Vec<MachineId>,
) -> Vec<MachineId> {
    if services.iter().all(|service| service.routes.is_empty()) {
        return Vec::new();
    }

    sorted_unique_machines(fallback_machines.iter())
}

async fn load_active_machine_scope(
    core_state: &AsyncNatsCoreStateStore,
    fallback: DeployExecutionMachineScope,
) -> Result<DeployExecutionMachineScope, DeployFactLoadError> {
    let machines = core_state.active_machines().await.map_err(|source| {
        DeployFactLoadError::ActiveMachineRead {
            message: source.to_string(),
        }
    })?;
    if machines.is_empty() {
        return Ok(fallback);
    }

    Ok(DeployExecutionMachineScope::same_machines(
        machines
            .into_iter()
            .map(|machine| machine.machine_id)
            .collect(),
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
    ServingTargetEntryRead {
        service_id: ServiceId,
        message: String,
    },
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
            Self::ServingTargetEntryRead {
                service_id,
                message,
            } => write!(
                formatter,
                "serving target entry state for {} could not be read: {}",
                service_id.as_str(),
                message
            ),
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
