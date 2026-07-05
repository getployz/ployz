//! Load deploy execution facts from core state and fresh machine facts RPCs.

use crate::machine_runtime::client::NatsMachineFactsReader;
use futures_util::{StreamExt, stream};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::UnusableMachine;
use ployz_core::state::{MachineUsabilityReason, placement_rejection};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use std::fmt;
use std::time::Duration;

use super::DeployExecutionFacts;
use super::preparation::namespace_cleanup_candidates;

const MAX_CONCURRENT_FACT_READS: usize = 16;

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
    facts_reader: &NatsMachineFactsReader,
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
    let (route_bindings, serving_entries, (mut machine_scope, mut unusable_machines)) =
        tokio::try_join!(route_bindings, serving_entries, machine_scope)?;
    let namespace_route_bindings = route_bindings
        .into_iter()
        .filter(|binding| binding.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let namespace_serving_entries = serving_entries
        .into_iter()
        .filter(|entry| entry.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let (observed_machines, facts_unavailable) =
        load_machine_snapshots(facts_reader, &machine_scope.observed_machine_ids).await;
    let answering_machines = sorted_unique_machines(
        observed_machines
            .iter()
            .map(MachineContainerObservationSnapshot::machine_id),
    );
    machine_scope
        .eligible_machines
        .retain(|machine_id| answering_machines.contains(machine_id));
    unusable_machines.extend(
        facts_unavailable
            .into_iter()
            .map(|machine_id| UnusableMachine {
                machine_id,
                reason: MachineUsabilityReason::FactsUnavailable,
            }),
    );
    let dataplane_machines = routed_dataplane_machines(request, answering_machines);
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

/// Durable operator intent builds the candidate scope first. Fresh
/// machine-facts RPCs later decide which candidates have usable operation-time
/// runtime evidence.
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
    facts_reader: &NatsMachineFactsReader,
    machine_ids: &[MachineId],
) -> (Vec<MachineContainerObservationSnapshot>, Vec<MachineId>) {
    let mut snapshots = Vec::new();
    let mut facts_unavailable = Vec::new();
    let mut reads = stream::iter(machine_ids.iter().cloned())
        .map(|machine_id| async move {
            facts_reader
                .machine_facts(&machine_id)
                .await
                .map(|facts| facts.containers().clone())
                .map_err(|_| machine_id)
        })
        .buffer_unordered(MAX_CONCURRENT_FACT_READS);

    while let Some(snapshot) = reads.next().await {
        match snapshot {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(machine_id) => facts_unavailable.push(machine_id),
        }
    }

    snapshots.sort_by(|left, right| left.machine_id().cmp(right.machine_id()));
    facts_unavailable.sort();
    (snapshots, facts_unavailable)
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
    RouteBindingsRead { message: String },
    ServingTargetEntriesRead { message: String },
    ActiveMachineRead { message: String },
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
        }
    }
}
