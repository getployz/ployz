//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::intent::NatsIntentReader;
use crate::machine_runtime::client::{
    NatsMachineFactsReader, NatsMachinePlacementBidder, read_machine_placement_facts,
};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::UnusableMachine;
use ployz_core::state::{
    IntentSnapshot, MachineLifecycle, MachineUsabilityReason, placement_rejection,
};
use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use super::DeployExecutionFacts;
use super::preparation::namespace_cleanup_candidates;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployMachineCandidates {
    machine_ids: Vec<MachineId>,
}

impl DeployMachineCandidates {
    #[must_use]
    pub fn same_machines(machines: Vec<MachineId>) -> Self {
        Self {
            machine_ids: sorted_unique_machines(machines.iter()),
        }
    }
}

pub async fn load_deploy_execution_facts_from_nats(
    request: &DeployRequest,
    fallback_candidates: DeployMachineCandidates,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    placement_bidder: &NatsMachinePlacementBidder,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let intent =
        intent_reader
            .intent()
            .await
            .map_err(|source| DeployFactLoadError::IntentRead {
                message: source.to_string(),
            })?;
    let machine_lifecycles = load_machine_lifecycles(&intent, fallback_candidates);
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
    let placement_facts =
        read_machine_placement_facts(facts_reader, placement_bidder, machine_lifecycles).await;
    let observed_machines = placement_facts
        .iter()
        .filter_map(|facts| facts.containers.clone())
        .collect::<Vec<_>>();
    let answering_machines = sorted_unique_machines(
        observed_machines
            .iter()
            .map(MachineContainerObservationSnapshot::machine_id),
    );
    let (eligible_machines, unusable_machines) = classify_machine_usability(&placement_facts);
    let dataplane_machines = routed_dataplane_machines(request, answering_machines);
    let namespace_cleanup_candidates =
        namespace_cleanup_candidates(&request.namespace_id, &observed_machines);
    Ok(DeployExecutionFacts {
        namespace_route_bindings,
        namespace_serving_entries,
        eligible_machines,
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

fn load_machine_lifecycles(
    intent: &IntentSnapshot,
    fallback: DeployMachineCandidates,
) -> Vec<(MachineId, MachineLifecycle)> {
    if intent.active_machines.is_empty() {
        return fallback
            .machine_ids
            .into_iter()
            .map(|machine_id| (machine_id, MachineLifecycle::Active))
            .collect();
    }

    intent
        .active_machines
        .iter()
        .map(|machine| (machine.machine_id.clone(), machine.lifecycle))
        .collect()
}

fn sorted_unique_machines<'a>(machines: impl IntoIterator<Item = &'a MachineId>) -> Vec<MachineId> {
    machines
        .into_iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn classify_machine_usability(
    placement_facts: &[crate::machine_runtime::client::MachinePlacementFacts],
) -> (Vec<MachineId>, Vec<UnusableMachine>) {
    let mut eligible = Vec::new();
    let mut unusable = BTreeMap::new();

    for facts in placement_facts {
        if let Some(reason) = placement_rejection(facts.lifecycle) {
            unusable.insert(facts.machine_id.clone(), reason);
            continue;
        }

        if facts.placement_available && facts.containers.is_some() {
            eligible.push(facts.machine_id.clone());
            continue;
        }

        unusable.insert(
            facts.machine_id.clone(),
            MachineUsabilityReason::FactsUnavailable,
        );
    }

    (
        eligible,
        unusable
            .into_iter()
            .map(|(machine_id, reason)| UnusableMachine { machine_id, reason })
            .collect(),
    )
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
