//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{
    DeployCleanupContainer, DeployPreparationError, DeployPreparationInput, DeployRequest,
    prepare_deploy,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, ManagedContainerKind};
use ployz_core::state::{ActiveRouteState, ActiveServiceState};
use std::time::Duration;

use super::{DeployExecutionCommand, DeployServiceExecutionCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub services: Vec<DeployServiceExecutionFacts>,
    pub eligible_machines: Vec<MachineId>,
    pub dataplane_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub namespace_cleanup_candidates: Vec<DeployCleanupContainer>,
    pub step_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployServiceExecutionFacts {
    pub active_service: Option<ActiveServiceState>,
    pub active_route: Option<ActiveRouteState>,
}

pub fn prepare_deploy_execution_command(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
) -> Result<DeployExecutionCommand, DeployCommandPreparationError> {
    let service_requests = request.service_requests();
    let mut service_facts = facts.services.into_iter();
    let mut services = Vec::new();
    for service_request in service_requests {
        let Some(service_facts) = service_facts.next() else {
            return Err(DeployCommandPreparationError::ServiceFactsMissing);
        };
        let prepared = prepare_deploy(DeployPreparationInput {
            request: service_request,
            active_service: service_facts.active_service,
            active_route: service_facts.active_route,
            eligible_machines: facts.eligible_machines.clone(),
            observed_machines: facts.observed_machines.clone(),
        })?;
        services.push(DeployServiceExecutionCommand {
            request: prepared.request,
            route_commit: prepared.route_commit,
            eligible_machines: prepared.eligible_machines,
            existing_replicas: prepared.existing_replicas,
            cleanup_candidates: prepared.cleanup_candidates,
        });
    }

    let namespace_cleanup_candidates = if request.services.is_empty() {
        facts.namespace_cleanup_candidates
    } else {
        Vec::new()
    };

    Ok(DeployExecutionCommand {
        operation_id,
        request,
        services,
        namespace_cleanup_candidates,
        dataplane_machines: facts.dataplane_machines,
        step_timeout: facts.step_timeout,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployCommandPreparationError {
    #[error(transparent)]
    Service(#[from] DeployPreparationError),
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
            revision_id: container.revision_id.clone(),
            operation_id: container.operation_id.clone(),
            step_id: container.step_id.clone(),
            kind: container.kind,
            endpoint_port: container
                .running_service_endpoint()
                .map(|endpoint| endpoint.port),
        })
        .collect()
}
