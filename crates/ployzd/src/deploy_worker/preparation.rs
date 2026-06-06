//! Convert current cluster facts into a deploy execution command.

use ployz_core::deploy::{DeployRequest, ExistingServiceReplica};
use ployz_core::ids::{NodeId, OperationId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::state::{ActiveServiceState, ExpectedActiveService};
use std::fmt;
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
    let expected_active = expected_active_service(&request, facts.active_service)?;

    Ok(DeployExecutionCommand {
        operation_id,
        request: request.clone(),
        expected_active,
        eligible_nodes: facts.eligible_nodes,
        existing_replicas: existing_replicas(&request, &facts.observed_nodes),
        step_timeout: facts.step_timeout,
    })
}

fn expected_active_service(
    request: &DeployRequest,
    active_service: Option<ActiveServiceState>,
) -> Result<ExpectedActiveService, DeployCommandPreparationError> {
    let Some(active_service) = active_service else {
        return Ok(ExpectedActiveService::Absent);
    };

    if active_service.service_id != request.service_id {
        return Err(DeployCommandPreparationError::ActiveServiceMismatch {
            expected_service_id: request.service_id.clone(),
            actual_service_id: active_service.service_id,
        });
    }

    Ok(ExpectedActiveService::Revision(
        active_service.active_revision,
    ))
}

fn existing_replicas(
    request: &DeployRequest,
    observed_nodes: &[NodeContainerObservationSnapshot],
) -> Vec<ExistingServiceReplica> {
    observed_nodes
        .iter()
        .flat_map(NodeContainerObservationSnapshot::containers)
        .filter(|container| is_running_target_service_container(container, request))
        .map(|container| ExistingServiceReplica {
            node_id: container.node_id.clone(),
            container_id: container.container_id.clone(),
        })
        .collect()
}

fn is_running_target_service_container(
    container: &ManagedContainerObservation,
    request: &DeployRequest,
) -> bool {
    container.kind == ManagedContainerKind::Service
        && container.state == ContainerRuntimeState::Running
        && container.service_id == request.service_id
        && container.revision_id == request.target_revision
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployCommandPreparationError {
    ActiveServiceMismatch {
        expected_service_id: ployz_core::ids::ServiceId,
        actual_service_id: ployz_core::ids::ServiceId,
    },
}

impl fmt::Display for DeployCommandPreparationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveServiceMismatch {
                expected_service_id,
                actual_service_id,
            } => write!(
                formatter,
                "active service state belongs to {}, not {}",
                actual_service_id.as_str(),
                expected_service_id.as_str()
            ),
        }
    }
}
