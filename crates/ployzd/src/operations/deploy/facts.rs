//! Load deploy execution facts from core intent and fresh machine facts RPCs.

use crate::intent::service::NatsIntentReader;
use crate::roles::machine::client::{NatsMachineFactsReader, read_machine_placement_facts};
use ployz_core::dataplane::DataplaneMember;
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::UnusableMachine;
use ployz_core::roles::GatewayRole;
use ployz_core::state::{
    ActiveMachineState, IntentSnapshot, MachineLifecycle, MachineUsabilityReason,
    placement_rejection,
};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
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
    let managed_lease = match &intent.managed_lease {
        ployz_core::state::ManagedLeaseProjection::Ready { lease, .. } => Some(lease.name.clone()),
        ployz_core::state::ManagedLeaseProjection::Unacquired
        | ployz_core::state::ManagedLeaseProjection::RecordOnly { .. } => None,
    };
    let machine_lifecycles = load_machine_lifecycles(&intent, fallback_candidates.clone());
    // Hostnames share one managed DNS lease across the cluster, so minting
    // must see bindings in every namespace. Namespace-scoped removal still
    // filters inside the planner.
    let namespace_route_bindings = intent.route_bindings;
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
    let dataplane_members =
        operation_dataplane_members(request, &active_machines, answering_machines);
    let gateway_public_ips = gateway_public_ips(&active_machines, &placement_facts);
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
        managed_lease,
        gateway_public_ips,
        step_timeout,
    })
}

fn gateway_public_ips(
    active_machines: &[ActiveMachineState],
    placement_facts: &[crate::roles::machine::client::MachinePlacementFacts],
) -> Vec<IpAddr> {
    let gateway_machines = active_machines
        .iter()
        .filter(|machine| matches!(machine.roles.gateway, GatewayRole::Install))
        .map(|machine| machine.machine_id.clone())
        .collect::<BTreeSet<_>>();

    placement_facts
        .iter()
        .filter(|facts| gateway_machines.contains(&facts.machine_id))
        .filter_map(|facts| facts.endpoints.as_ref())
        .flat_map(|endpoints| endpoints.control_endpoints.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn operation_dataplane_members(
    request: &DeployRequest,
    active_machines: &[ActiveMachineState],
    fallback_machines: Vec<MachineId>,
) -> Vec<DataplaneMember> {
    let needs_membership = request.services.iter().any(|service| {
        !service.routes.is_empty()
            || matches!(
                &service.image_source,
                ployz_core::deploy::ImageSource::PushedToSeed { .. }
            )
    });
    if !needs_membership {
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

#[cfg(test)]
mod tests {
    use super::gateway_public_ips;
    use crate::roles::machine::client::MachinePlacementFacts;
    use ployz_core::ids::MachineId;
    use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
    use ployz_core::state::{MachineEndpointObservation, MachineLifecycle};

    #[test]
    fn answering_placement_facts_do_not_supply_gateway_ips_without_intent_roster() {
        let machine_id = MachineId::try_new("machine_a").expect("valid machine id");
        let placement_facts = MachinePlacementFacts {
            machine_id: machine_id.clone(),
            lifecycle: MachineLifecycle::Active,
            containers: Some(
                MachineContainerObservationSnapshot::try_new(machine_id.clone(), [])
                    .expect("valid empty container snapshot"),
            ),
            platform: None,
            endpoints: Some(MachineEndpointObservation {
                machine_id,
                control_endpoints: vec!["203.0.113.10".parse().expect("valid public IP")],
                mesh_endpoints: Vec::new(),
            }),
        };

        assert!(gateway_public_ips(&[], &[placement_facts]).is_empty());
    }
}
