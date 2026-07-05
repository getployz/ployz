//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::intent::NatsIntentReader;
use crate::machine_runtime::client::{NatsMachineFactsReader, read_machine_container_snapshots};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::UnusableMachine;
use ployz_core::state::{IntentSnapshot, MachineUsabilityReason, placement_rejection};
use std::fmt;
use std::time::Duration;

use super::DeployExecutionFacts;
use super::preparation::namespace_cleanup_candidates;

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
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let intent =
        intent_reader
            .intent()
            .await
            .map_err(|source| DeployFactLoadError::IntentRead {
                message: source.to_string(),
            })?;
    let (mut machine_scope, mut unusable_machines) =
        load_active_machine_scope(&intent, machine_scope);
    let namespace_route_bindings = intent
        .route_bindings
        .into_iter()
        .filter(|binding| binding.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let namespace_serving_entries = intent
        .serving_target_entries
        .into_iter()
        .filter(|entry| entry.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let (observed_machines, facts_unavailable) = read_machine_container_snapshots(
        facts_reader,
        machine_scope.observed_machine_ids.iter().cloned(),
    )
    .await;
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
fn load_active_machine_scope(
    intent: &IntentSnapshot,
    fallback: DeployExecutionMachineScope,
) -> (DeployExecutionMachineScope, Vec<UnusableMachine>) {
    if intent.active_machines.is_empty() {
        return (fallback, Vec::new());
    }

    let mut eligible = Vec::new();
    let mut observed_ids = Vec::new();
    let mut unusable = Vec::new();
    for machine in &intent.active_machines {
        observed_ids.push(machine.machine_id.clone());
        match placement_rejection(machine.lifecycle) {
            None => eligible.push(machine.machine_id.clone()),
            Some(reason) => unusable.push(UnusableMachine {
                machine_id: machine.machine_id.clone(),
                reason,
            }),
        }
    }

    (
        DeployExecutionMachineScope {
            eligible_machines: eligible,
            observed_machine_ids: observed_ids,
        },
        unusable,
    )
}

fn sorted_unique_machines<'a>(machines: impl IntoIterator<Item = &'a MachineId>) -> Vec<MachineId> {
    machines
        .into_iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// An intent read failed before deploy execution started. The rendered
/// message is failure evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployFactLoadError {
    IntentRead { message: String },
}

impl fmt::Display for DeployFactLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IntentRead { message } => {
                write!(formatter, "intent could not be read: {message}")
            }
        }
    }
}
