//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{
    DeployCleanupContainer, DeployPreparationInput, DeployRequest,
    namespace_route_binding_removals, namespace_serving_target_removals, prepare_deploy,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, ManagedContainerKind};
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use std::time::Duration;

use super::{DeployExecutionCommand, DeployServiceExecutionCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub services: Vec<DeployServiceExecutionFacts>,
    pub namespace_route_bindings: Vec<RouteBindingState>,
    pub namespace_serving_entries: Vec<ServingTargetEntry>,
    pub eligible_machines: Vec<MachineId>,
    pub dataplane_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub step_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployServiceExecutionFacts {
    pub serving_target_entry: Option<ServingTargetEntry>,
    pub route_bindings: Vec<RouteBindingState>,
}

pub fn prepare_deploy_execution_command(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
) -> Result<DeployExecutionCommand, DeployCommandPreparationError> {
    let service_requests = request.service_requests();
    let namespace_declared_targets = request
        .services
        .iter()
        .flat_map(|service| service.routes.iter())
        .map(|route| route.target.clone())
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
    let mut service_facts = facts.services.into_iter();
    let mut services = Vec::new();
    for service_request in service_requests {
        let Some(service_facts) = service_facts.next() else {
            return Err(DeployCommandPreparationError::ServiceFactsMissing);
        };
        let prepared = prepare_deploy(DeployPreparationInput {
            request: service_request,
            serving_target_entry: service_facts.serving_target_entry,
            eligible_machines: facts.eligible_machines.clone(),
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
    let namespace_cleanup_candidates = facts
        .namespace_cleanup_candidates
        .into_iter()
        .filter(|candidate| !declared_services.contains(&candidate.service_id))
        .collect();

    Ok(DeployExecutionCommand {
        operation_id,
        request,
        services,
        route_binding_removals,
        serving_target_removals,
        namespace_cleanup_candidates,
        dataplane_machines: facts.dataplane_machines,
        step_timeout: facts.step_timeout,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployCommandPreparationError {
    #[error("deploy service facts are missing")]
    ServiceFactsMissing,
}

pub fn namespace_cleanup_candidates(
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
        .filter(|container| {
            container.kind == ManagedContainerKind::Service && container.state.is_running()
        })
        .map(|container| DeployCleanupContainer {
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            service_id: container.service_id.clone(),
            namespace_revision_entry_id: container.namespace_revision_entry_id.clone(),
            operation_id: container.operation_id.clone(),
            step_id: container.step_id.clone(),
            kind: container.kind,
        })
        .collect()
}
