//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{
    DeployPreparationError, DeployPreparationInput, DeployRequest, prepare_deploy,
};
use ployz_core::ids::{NodeId, OperationId};
use ployz_core::node::NodeContainerObservationSnapshot;
use ployz_core::state::ActiveServiceState;
use std::time::Duration;

use super::DeployExecutionCommand;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionFacts {
    pub active_service: Option<ActiveServiceState>,
    pub eligible_nodes: Vec<NodeId>,
    pub observed_nodes: Vec<NodeContainerObservationSnapshot>,
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
        eligible_nodes: facts.eligible_nodes,
        observed_nodes: facts.observed_nodes,
    })?;

    Ok(DeployExecutionCommand {
        operation_id,
        request: prepared.request,
        expected_active: prepared.expected_active,
        eligible_nodes: prepared.eligible_nodes,
        existing_replicas: prepared.existing_replicas,
        step_timeout: facts.step_timeout,
    })
}

pub type DeployCommandPreparationError = DeployPreparationError;
