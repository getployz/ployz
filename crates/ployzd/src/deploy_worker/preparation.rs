//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{
    DeployPreparationError, DeployPreparationInput, DeployRequest, prepare_deploy,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::state::{ActiveRouteState, ActiveServiceState};
use std::time::Duration;

use super::DeployExecutionCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub active_service: Option<ActiveServiceState>,
    pub active_route: Option<ActiveRouteState>,
    pub eligible_machines: Vec<MachineId>,
    pub dataplane_machines: Vec<MachineId>,
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
    pub step_timeout: Duration,
}

pub fn prepare_deploy_execution_command(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
) -> Result<DeployExecutionCommand, DeployCommandPreparationError> {
    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        active_service: facts.active_service,
        active_route: facts.active_route,
        eligible_machines: facts.eligible_machines,
        observed_machines: facts.observed_machines,
    })?;

    Ok(DeployExecutionCommand {
        operation_id,
        request: prepared.request,
        expected_active: prepared.expected_active,
        route_commit: prepared.route_commit,
        eligible_machines: prepared.eligible_machines,
        existing_replicas: prepared.existing_replicas,
        cleanup_candidates: prepared.cleanup_candidates,
        dataplane_machines: facts.dataplane_machines,
        step_timeout: facts.step_timeout,
    })
}

pub type DeployCommandPreparationError = DeployPreparationError;
