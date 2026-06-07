//! Launch accepted deploy operations from current NATS state.

use crate::controllers::{AcceptedDeployOperation, OperationControllers};
use crate::deploy_worker::{
    DeployCommandPreparationError, DeployExecutionError, DeployExecutionNodeScope,
    DeployExecutionOutcome, DeployExecutionPorts, DeployFactLoadError, DeployHealthChecker,
    NodeContainerRuntime, WireGuardEbpfPreparer, execute_deploy_operation,
    load_deploy_execution_facts_from_nats, prepare_deploy_execution_command,
};
use crate::operation_lease::with_advisory_operation_lease;
use ployz_core::ids::OperationOwnerId;
use ployz_core::ops::{
    DeployOperationFailure, DeployTransition, FailureMessage, OperationOwnerLease,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operations::{OperationStatusStoreError, RecordDeployTransitionError};
use std::time::Duration;

pub async fn run_deploy_operation<D, N, H>(
    accepted: AcceptedDeployOperation,
    node_scope: DeployExecutionNodeScope,
    stores: DeployLaunchStores,
    ports: DeployLaunchPorts<'_, D, N, H>,
    step_timeout: Duration,
) -> Result<DeployExecutionOutcome, DeployLaunchError>
where
    D: WireGuardEbpfPreparer,
    N: NodeContainerRuntime,
    H: DeployHealthChecker,
{
    let DeployLaunchStores {
        core_state,
        observations,
        controllers,
    } = stores;
    let DeployLaunchPorts {
        wireguard_ebpf,
        node_runtime,
        health_checker,
    } = ports;
    renew_launch_owner_lease(&controllers, &accepted)
        .await
        .map_err(DeployLaunchError::LeaseNotHeld)?;
    let lease_policy = controllers.lease_policy();
    let lease_renewer = controllers.clone();
    let operation_id = accepted.operation_id.clone();
    let request = accepted.target.clone();

    with_advisory_operation_lease(operation_id, lease_policy, lease_renewer, async move {
        let facts = match load_deploy_execution_facts_from_nats(
            &request,
            node_scope,
            &core_state,
            &observations,
            step_timeout,
        )
        .await
        {
            Ok(facts) => facts,
            Err(source) => {
                let failure_record_error = record_launch_failure(
                    &controllers,
                    &accepted,
                    fact_load_failure(&request, &source),
                )
                .await
                .err();
                return Err(DeployLaunchError::LoadFacts {
                    source,
                    failure_record_error,
                });
            }
        };
        let command = match prepare_deploy_execution_command(
            accepted.operation_id.clone(),
            request.clone(),
            facts,
        ) {
            Ok(command) => command,
            Err(source) => {
                let failure_record_error =
                    record_launch_failure(&controllers, &accepted, preparation_failure(&request))
                        .await
                        .err();
                return Err(DeployLaunchError::PrepareCommand {
                    source,
                    failure_record_error,
                });
            }
        };
        let mut recorder = controllers;
        let mut active_state = core_state;

        execute_deploy_operation(
            command,
            DeployExecutionPorts {
                recorder: &mut recorder,
                wireguard_ebpf,
                node_runtime,
                health_checker,
                active_state: &mut active_state,
            },
        )
        .await
        .map_err(DeployLaunchError::Execute)
    })
    .await
}

async fn renew_launch_owner_lease(
    controllers: &OperationControllers,
    accepted: &AcceptedDeployOperation,
) -> Result<OperationOwnerLease, DeployLaunchLeaseError> {
    verify_accepted_deploy_lease(&accepted.lease, &accepted.operation_id)?;
    let Some(lease) = controllers
        .renew_owner_lease(&accepted.operation_id)
        .await
        .map_err(DeployLaunchLeaseError::Renew)?
    else {
        return Err(DeployLaunchLeaseError::NoCurrentLease {
            operation_id: accepted.operation_id.clone(),
            expected_owner: controllers.owner_id().clone(),
        });
    };
    verify_accepted_deploy_lease(&lease, &accepted.operation_id)?;
    if lease.owner_id != *controllers.owner_id() {
        return Err(DeployLaunchLeaseError::NotCurrentOwner {
            lease,
            expected_owner: controllers.owner_id().clone(),
        });
    }

    Ok(lease)
}

fn verify_accepted_deploy_lease(
    lease: &OperationOwnerLease,
    operation_id: &ployz_core::ids::OperationId,
) -> Result<(), DeployLaunchLeaseError> {
    if &lease.operation_id != operation_id {
        return Err(DeployLaunchLeaseError::OperationMismatch {
            lease: lease.clone(),
            expected_operation_id: operation_id.clone(),
        });
    }

    Ok(())
}

async fn record_launch_failure(
    controllers: &OperationControllers,
    accepted: &AcceptedDeployOperation,
    failure: DeployOperationFailure,
) -> Result<(), RecordDeployTransitionError> {
    controllers
        .record_deploy_transition(&accepted.operation_id, DeployTransition::Failed { failure })
        .await
        .map(|_| ())
}

fn fact_load_failure(
    request: &ployz_core::deploy::DeployRequest,
    source: &DeployFactLoadError,
) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: request.service_id.clone(),
        revision_id: request.target_revision.clone(),
        message: launch_failure_message(fact_load_failure_message(source)),
    }
}

fn preparation_failure(request: &ployz_core::deploy::DeployRequest) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: request.service_id.clone(),
        revision_id: request.target_revision.clone(),
        message: launch_failure_message("deploy command could not be prepared"),
    }
}

fn fact_load_failure_message(source: &DeployFactLoadError) -> &'static str {
    match source {
        DeployFactLoadError::ActiveServiceRead { .. } => "active service state could not be loaded",
        DeployFactLoadError::NodeObservationRead { .. } => "node observations could not be loaded",
    }
}

fn launch_failure_message(message: &'static str) -> FailureMessage {
    FailureMessage::try_new(message).expect("static launch failure message is non-empty")
}

#[derive(Debug, Clone)]
pub struct DeployLaunchStores {
    pub core_state: AsyncNatsCoreStateStore,
    pub observations: AsyncNatsObservationStore,
    pub controllers: OperationControllers,
}

pub struct DeployLaunchPorts<'a, D, N, H> {
    pub wireguard_ebpf: &'a mut D,
    pub node_runtime: &'a mut N,
    pub health_checker: &'a mut H,
}

#[derive(Debug)]
pub enum DeployLaunchError {
    LeaseNotHeld(DeployLaunchLeaseError),
    LoadFacts {
        source: DeployFactLoadError,
        failure_record_error: Option<RecordDeployTransitionError>,
    },
    PrepareCommand {
        source: DeployCommandPreparationError,
        failure_record_error: Option<RecordDeployTransitionError>,
    },
    Execute(DeployExecutionError),
}

#[derive(Debug)]
pub enum DeployLaunchLeaseError {
    OperationMismatch {
        lease: OperationOwnerLease,
        expected_operation_id: ployz_core::ids::OperationId,
    },
    NoCurrentLease {
        operation_id: ployz_core::ids::OperationId,
        expected_owner: OperationOwnerId,
    },
    NotCurrentOwner {
        lease: OperationOwnerLease,
        expected_owner: OperationOwnerId,
    },
    Renew(OperationStatusStoreError),
}
