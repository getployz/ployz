//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::intent::service::NatsIntentReader;
use crate::roles::machine::client::{NatsMachineFactsReader, read_machine_placement_facts};
use ployz_core::dataplane::DataplaneMember;
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::UnusableMachine;
use ployz_core::state::{
    ActiveMachineState, IntentSnapshot, MachineLifecycle, MachineUsabilityReason,
    placement_rejection,
};
use std::collections::BTreeMap;
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
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let intent =
        intent_reader
            .intent()
            .await
            .map_err(|source| DeployFactLoadError::IntentRead {
                message: source.to_string(),
            })?;
    let active_machines = intent.active_machines.clone();
    let machine_lifecycles = load_machine_lifecycles(&intent, fallback_candidates.clone());
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
    let namespace_volume_pins = intent
        .volume_pins
        .into_iter()
        .filter(|pin| pin.namespace_id == request.namespace_id)
        .collect::<Vec<_>>();
    let placement_facts = read_machine_placement_facts(facts_reader, machine_lifecycles).await;
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
    let machine_platforms = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .platform
                .clone()
                .map(|platform| (facts.machine_id.clone(), platform))
        })
        .collect();
    let dataplane_members = routed_dataplane_members(request, &active_machines, answering_machines);
    let namespace_cleanup_candidates =
        namespace_cleanup_candidates(&request.namespace_id, &observed_machines);
    Ok(DeployExecutionFacts {
        namespace_route_bindings,
        namespace_serving_entries,
        namespace_volume_pins,
        eligible_machines,
        unusable_machines,
        dataplane_members,
        observed_machines,
        machine_platforms,
        namespace_cleanup_candidates,
        step_timeout,
    })
}

fn routed_dataplane_members(
    request: &DeployRequest,
    active_machines: &[ActiveMachineState],
    fallback_machines: Vec<MachineId>,
) -> Vec<DataplaneMember> {
    if request
        .services
        .iter()
        .all(|service| service.routes.is_empty())
    {
        return Vec::new();
    }

    if !active_machines.is_empty() {
        return active_machines
            .iter()
            .map(|machine| DataplaneMember {
                machine_id: machine.machine_id.clone(),
                endpoint_subnet: machine.endpoint_subnet.clone(),
            })
            .collect();
    }

    sorted_unique_machines(fallback_machines.iter())
        .into_iter()
        .map(DataplaneMember::default_for_machine)
        .collect()
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
    placement_facts: &[crate::roles::machine::client::MachinePlacementFacts],
) -> (Vec<MachineId>, Vec<UnusableMachine>) {
    let mut eligible = Vec::new();
    let mut unusable = BTreeMap::new();

    for facts in placement_facts {
        if let Some(reason) = placement_rejection(facts.lifecycle) {
            unusable.insert(facts.machine_id.clone(), reason);
            continue;
        }

        // Eligibility is reachability plus operator intent: a machine that
        // answered with its facts and is not draining can take work. Placement
        // does not ask a machine to bid — a dead machine is silent here and
        // fails again at the point of use (ADR 0027).
        if facts.containers.is_some() {
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
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployFactLoadError {
    #[error("intent could not be read: {message}")]
    IntentRead { message: String },
}
