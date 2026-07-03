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
    let active_routes = core_state.active_routes().await.map_err(|source| {
        DeployFactLoadError::ActiveRoutesRead {
            message: source.to_string(),
        }
    })?;
    let mut service_facts = Vec::new();
    for service in &service_requests {
        let active_service = core_state
            .active_service(&service.service_id)
            .await
            .map_err(|source| DeployFactLoadError::ActiveServiceRead {
                service_id: service.service_id.clone(),
                message: source.to_string(),
            })?;
        let active_routes = active_routes
            .iter()
            .filter(|route| route.service_id == service.service_id)
            .cloned()
            .collect();
        service_facts.push(DeployServiceExecutionFacts {
            active_service,
            active_routes,
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
    ActiveServiceRead {
        service_id: ServiceId,
        message: String,
    },
    ActiveRoutesRead {
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
            Self::ActiveServiceRead {
                service_id,
                message,
            } => write!(
                formatter,
                "active service state for {} could not be read: {}",
                service_id.as_str(),
                message
            ),
            Self::ActiveRoutesRead { message } => write!(
                formatter,
                "active route state could not be read: {}",
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
