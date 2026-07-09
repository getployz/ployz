//! Convert current cluster facts into a deploy execution command.

use ployz_core::dataplane::DataplaneMember;
use ployz_core::deploy::{
    DeployCleanupContainer, DeployPreparationInput, DeployRequest,
    namespace_route_binding_removals, namespace_serving_target_removals, prepare_deploy,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use std::time::Duration;

use super::{DeployExecutionCommand, DeployServiceExecutionCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub namespace_route_bindings: Vec<RouteBindingState>,
    pub namespace_serving_entries: Vec<ServingTargetEntry>,
    pub eligible_machines: Vec<MachineId>,
    pub unusable_machines: Vec<ployz_core::ops::UnusableMachine>,
    pub dataplane_members: Vec<DataplaneMember>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub step_timeout: Duration,
}

#[must_use]
pub fn prepare_deploy_execution_command(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
) -> DeployExecutionCommand {
    let service_requests = request.service_requests();
    let namespace_declared_targets = request
        .services
        .iter()
        .flat_map(|service| service.routes.iter())
        .filter_map(|route| route.target.concrete_target())
        .collect::<Vec<_>>();
    let declared_services = request
        .services
        .iter()
        .map(|service| service.service_id.clone())
        .collect::<Vec<_>>();
    let route_binding_removals = namespace_route_binding_removals(
        &request.namespace_id,
        &namespace_declared_targets,
        &facts.namespace_route_bindings,
    );
    let serving_target_removals = namespace_serving_target_removals(
        &request.namespace_id,
        &declared_services,
        &facts.namespace_serving_entries,
    );
    let draining_machines = facts
        .unusable_machines
        .iter()
        .filter(|unusable| match unusable.reason {
            ployz_core::state::MachineUsabilityReason::Draining => true,
            ployz_core::state::MachineUsabilityReason::FactsUnavailable => false,
        })
        .map(|unusable| unusable.machine_id.clone())
        .collect::<Vec<_>>();
    let mut services = Vec::new();
    for service_request in service_requests {
        let prepared = prepare_deploy(DeployPreparationInput {
            request: service_request,
            eligible_machines: facts.eligible_machines.clone(),
            draining_machines: draining_machines.clone(),
            observed_machines: facts.observed_machines.clone(),
        });
        services.push(DeployServiceExecutionCommand {
            request: prepared.request,
            route_commits: prepared.route_commits,
            eligible_machines: prepared.eligible_machines,
            existing_replicas: prepared.existing_replicas,
            cleanup_candidates: prepared.cleanup_candidates,
        });
    }

    // Manifest omission removes a service: its containers are cleanup
    // candidates on every deploy, not only when the manifest is empty.
    // The candidates are already namespace-scoped at collection.
    let namespace_cleanup_candidates = facts
        .namespace_cleanup_candidates
        .into_iter()
        .filter(|candidate| !declared_services.contains(&candidate.identity.service_id))
        .collect();

    DeployExecutionCommand {
        operation_id,
        request,
        services,
        route_binding_removals,
        serving_target_removals,
        namespace_cleanup_candidates,
        dataplane_members: facts.dataplane_members,
        unusable_machines: facts.unusable_machines,
        step_timeout: facts.step_timeout,
    }
}

pub fn namespace_cleanup_candidates(
    namespace_id: &ployz_core::ids::NamespaceId,
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.is_service() && container.identity.namespace_id == *namespace_id
        })
        .map(|container| DeployCleanupContainer {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            identity: container.identity.clone(),
        })
        .collect()
}
