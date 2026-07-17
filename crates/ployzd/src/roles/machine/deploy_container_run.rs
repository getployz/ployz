use std::time::Duration;

use ployz_core::ids::{ContainerId, OperationId, StepId};
use ployz_core::machine::runtime::ManagedContainerIdentity;
use ployz_core::network::MachineEndpointSubnet;

use super::protocol::{
    MachineContainerRunDomainError, MachineContainerRunHookDomainError, MachineRunContainerOutcome,
};
use super::response::{failure_message, inspect_hint, log_hint};
use super::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerCreateError, MachineContainerListError, MachineContainerRunner,
    MachineContainerStartError, MachineContainerStopError, MachineContainerWaitError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceContainerInfrastructureError {
    List {
        message: String,
    },
    Create {
        message: String,
    },
    EnsureEndpointNetwork {
        message: String,
    },
    EndpointNetworkSubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookContainerInfrastructureError {
    List {
        message: String,
    },
    ImagePull {
        message: String,
    },
    EnsureEndpointNetwork {
        message: String,
    },
    EndpointNetworkSubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
    TimeoutStopList {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceContainerRunError {
    Domain(MachineContainerRunDomainError),
    Infrastructure(ServiceContainerInfrastructureError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HookContainerRunOutcome {
    pub(crate) container_id: ContainerId,
    pub(crate) exit_code: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookContainerRunError {
    Domain(MachineContainerRunHookDomainError),
    Infrastructure(HookContainerInfrastructureError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceContainerStart {
    Created,
    Existing,
}

pub(crate) async fn run_service_container<R>(
    runner: &R,
    command: CreateManagedContainer,
) -> Result<MachineRunContainerOutcome, ServiceContainerRunError>
where
    R: MachineContainerRunner,
{
    let existing = runner
        .existing_managed_containers()
        .await
        .map_err(|error| match error {
            MachineContainerListError::ListExisting { message } => {
                ServiceContainerRunError::Infrastructure(
                    ServiceContainerInfrastructureError::List { message },
                )
            }
        })?;
    match decide_container_run(&command.identity, existing) {
        MachineContainerRunDecision::Create => {
            let service_id = command.identity.service_id.clone();
            let namespace_revision_entry_id = command.identity.namespace_revision_entry_id.clone();
            let container_id = match runner.create_managed_container(command).await {
                Ok(container_id) => container_id,
                Err(MachineContainerCreateError::ImagePull { message }) => {
                    return Err(ServiceContainerRunError::Domain(
                        MachineContainerRunDomainError::ImagePullFailed {
                            service_id,
                            namespace_revision_entry_id,
                            message: failure_message(message),
                        },
                    ));
                }
                Err(MachineContainerCreateError::Create { message }) => {
                    return Err(ServiceContainerRunError::Infrastructure(
                        ServiceContainerInfrastructureError::Create { message },
                    ));
                }
                Err(MachineContainerCreateError::EnsureEndpointNetwork { message }) => {
                    return Err(ServiceContainerRunError::Infrastructure(
                        ServiceContainerInfrastructureError::EnsureEndpointNetwork { message },
                    ));
                }
                Err(MachineContainerCreateError::EndpointNetworkSubnetMismatch {
                    expected,
                    observed,
                }) => {
                    return Err(ServiceContainerRunError::Infrastructure(
                        ServiceContainerInfrastructureError::EndpointNetworkSubnetMismatch {
                            expected,
                            observed,
                        },
                    ));
                }
            };

            start_service_container(runner, container_id, ServiceContainerStart::Created).await
        }
        MachineContainerRunDecision::ReuseRunning { container_id } => {
            Ok(MachineRunContainerOutcome::ReusedRunning { container_id })
        }
        MachineContainerRunDecision::StartExisting { container_id } => {
            start_service_container(runner, container_id, ServiceContainerStart::Existing).await
        }
        MachineContainerRunDecision::NotStartable {
            container_id,
            state,
        } => Err(ServiceContainerRunError::Domain(
            MachineContainerRunDomainError::OperationStepContainerNotStartable {
                container_id: container_id.clone(),
                message: failure_message(format!(
                    "operation step container is not startable: {state:?}"
                )),
                inspect_hint: inspect_hint(&container_id),
            },
        )),
        MachineContainerRunDecision::Ambiguous {
            operation_id,
            step_id,
            container_ids,
        } => Err(ServiceContainerRunError::Domain(
            MachineContainerRunDomainError::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            },
        )),
    }
}

pub(crate) async fn run_hook_container<R>(
    runner: &R,
    command: CreateManagedContainer,
    timeout_millis: u64,
) -> Result<HookContainerRunOutcome, HookContainerRunError>
where
    R: MachineContainerRunner,
{
    let existing = runner
        .existing_managed_containers()
        .await
        .map_err(|error| match error {
            MachineContainerListError::ListExisting { message } => {
                HookContainerRunError::Infrastructure(HookContainerInfrastructureError::List {
                    message,
                })
            }
        })?;
    let expected_identity = command.identity.clone();

    let container_id = match decide_container_run(&command.identity, existing) {
        MachineContainerRunDecision::Create => {
            let container_id = match runner.create_managed_container(command).await {
                Ok(container_id) => container_id,
                Err(MachineContainerCreateError::Create { message }) => {
                    return Err(HookContainerRunError::Domain(
                        MachineContainerRunHookDomainError::CreateFailed {
                            message: failure_message(format!(
                                "hook container create failed: {message}"
                            )),
                        },
                    ));
                }
                Err(MachineContainerCreateError::ImagePull { message }) => {
                    return Err(HookContainerRunError::Infrastructure(
                        HookContainerInfrastructureError::ImagePull { message },
                    ));
                }
                Err(MachineContainerCreateError::EnsureEndpointNetwork { message }) => {
                    return Err(HookContainerRunError::Infrastructure(
                        HookContainerInfrastructureError::EnsureEndpointNetwork { message },
                    ));
                }
                Err(MachineContainerCreateError::EndpointNetworkSubnetMismatch {
                    expected,
                    observed,
                }) => {
                    return Err(HookContainerRunError::Infrastructure(
                        HookContainerInfrastructureError::EndpointNetworkSubnetMismatch {
                            expected,
                            observed,
                        },
                    ));
                }
            };
            start_hook_container(runner, container_id).await?
        }
        MachineContainerRunDecision::StartExisting { container_id } => {
            start_hook_container(runner, container_id).await?
        }
        MachineContainerRunDecision::ReuseRunning { container_id }
        | MachineContainerRunDecision::NotStartable { container_id, .. } => container_id,
        MachineContainerRunDecision::Ambiguous {
            operation_id,
            step_id,
            container_ids,
        } => {
            return Err(HookContainerRunError::Domain(
                MachineContainerRunHookDomainError::OperationStepAmbiguous {
                    operation_id,
                    step_id,
                    container_ids,
                },
            ));
        }
    };

    let timeout = Duration::from_millis(timeout_millis.max(1));
    let exit_code =
        match tokio::time::timeout(timeout, runner.wait_managed_container(&container_id)).await {
            Ok(Ok(exit_code)) => exit_code,
            Ok(Err(MachineContainerWaitError::Wait { message, .. })) => {
                return Err(HookContainerRunError::Domain(
                    MachineContainerRunHookDomainError::WaitFailed {
                        container_id: container_id.clone(),
                        message: failure_message(format!("hook container wait failed: {message}")),
                        log_hint: log_hint(&container_id),
                    },
                ));
            }
            Err(_) => {
                let message = match runner
                    .stop_managed_container(&container_id, &expected_identity)
                    .await
                {
                    Ok(()) => format!(
                        "hook timed out after {}ms and was stopped",
                        timeout.as_millis()
                    ),
                    Err(MachineContainerStopError::Stop { message, .. }) => format!(
                        "hook timed out after {}ms and could not be stopped: {message}",
                        timeout.as_millis()
                    ),
                    Err(MachineContainerStopError::ListExisting { message }) => {
                        return Err(HookContainerRunError::Infrastructure(
                            HookContainerInfrastructureError::TimeoutStopList { message },
                        ));
                    }
                };
                return Err(HookContainerRunError::Domain(
                    MachineContainerRunHookDomainError::TimedOut {
                        container_id: container_id.clone(),
                        timeout_millis,
                        message: failure_message(message),
                        inspect_hint: inspect_hint(&container_id),
                    },
                ));
            }
        };

    Ok(HookContainerRunOutcome {
        container_id,
        exit_code,
    })
}

async fn start_service_container<R>(
    runner: &R,
    container_id: ContainerId,
    start: ServiceContainerStart,
) -> Result<MachineRunContainerOutcome, ServiceContainerRunError>
where
    R: MachineContainerRunner,
{
    match runner.start_managed_container(&container_id).await {
        Ok(()) => Ok(match start {
            ServiceContainerStart::Created => MachineRunContainerOutcome::Created { container_id },
            ServiceContainerStart::Existing => {
                MachineRunContainerOutcome::StartedExisting { container_id }
            }
        }),
        Err(MachineContainerStartError::Start { message, .. }) => {
            let message = failure_message(format!("container start failed: {message}"));
            let inspect_hint = inspect_hint(&container_id);
            Err(ServiceContainerRunError::Domain(match start {
                ServiceContainerStart::Created => {
                    MachineContainerRunDomainError::CreatedContainerStartFailed {
                        container_id,
                        message,
                        inspect_hint,
                    }
                }
                ServiceContainerStart::Existing => {
                    MachineContainerRunDomainError::ExistingContainerStartFailed {
                        container_id,
                        message,
                        inspect_hint,
                    }
                }
            }))
        }
    }
}

async fn start_hook_container<R>(
    runner: &R,
    container_id: ContainerId,
) -> Result<ContainerId, HookContainerRunError>
where
    R: MachineContainerRunner,
{
    match runner.start_managed_container(&container_id).await {
        Ok(()) => Ok(container_id),
        Err(MachineContainerStartError::Start { message, .. }) => Err(
            HookContainerRunError::Domain(MachineContainerRunHookDomainError::StartFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("hook container start failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            }),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineContainerRunDecision {
    Create,
    ReuseRunning {
        container_id: ContainerId,
    },
    StartExisting {
        container_id: ContainerId,
    },
    NotStartable {
        container_id: ContainerId,
        state: ExistingManagedContainerState,
    },
    Ambiguous {
        operation_id: OperationId,
        step_id: StepId,
        container_ids: Vec<ContainerId>,
    },
}

#[must_use]
fn decide_container_run(
    expected: &ManagedContainerIdentity,
    existing: impl IntoIterator<Item = ExistingManagedContainer>,
) -> MachineContainerRunDecision {
    let mut matches = existing
        .into_iter()
        .filter(|container| container.identity == *expected);

    let Some(first) = matches.next() else {
        return MachineContainerRunDecision::Create;
    };

    let rest = matches.collect::<Vec<_>>();
    if !rest.is_empty() {
        let container_ids = std::iter::once(first.container_id)
            .chain(rest.into_iter().map(|container| container.container_id))
            .collect();
        return MachineContainerRunDecision::Ambiguous {
            operation_id: expected.operation_id.clone(),
            step_id: expected.step_id.clone(),
            container_ids,
        };
    }

    let ExistingManagedContainer {
        container_id,
        state,
        ..
    } = first;

    match state {
        ExistingManagedContainerState::Running { .. } => {
            MachineContainerRunDecision::ReuseRunning { container_id }
        }
        ExistingManagedContainerState::StartableStopped => {
            MachineContainerRunDecision::StartExisting { container_id }
        }
        ExistingManagedContainerState::NotStartable { .. } => {
            MachineContainerRunDecision::NotStartable {
                container_id,
                state,
            }
        }
    }
}

#[cfg(test)]
#[path = "deploy_container_run_tests.rs"]
mod tests;
