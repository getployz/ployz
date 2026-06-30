//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{
    DeployPreparationError, DeployPreparationInput, DeployRequest, prepare_deploy,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::state::{ActiveRouteState, ActiveServiceState};
use std::time::Duration;

use super::{DeployExecutionCommand, DeployServiceExecutionCommand};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub services: Vec<DeployServiceExecutionFacts>,
    pub eligible_machines: Vec<MachineId>,
    pub dataplane_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
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
            expected_active: prepared.expected_active,
            route_commit: prepared.route_commit,
            eligible_machines: prepared.eligible_machines,
            existing_replicas: prepared.existing_replicas,
            cleanup_candidates: prepared.cleanup_candidates,
        });
    }

    Ok(DeployExecutionCommand {
        operation_id,
        request,
        services,
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
