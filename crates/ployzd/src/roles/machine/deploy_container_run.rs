use std::time::Duration;

use ployz_core::ids::{ContainerId, OperationId, StepId};
use ployz_core::machine::runtime::ManagedContainerIdentity;

use super::protocol::{
    MachineContainerRunDomainError, MachineContainerRunHookDomainError, MachineRunContainerOutcome,
};
use super::response::{failure_message, inspect_hint, log_hint};
use super::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ServiceContainerRunError {
    Domain(MachineContainerRunDomainError),
    Runner(MachineContainerRunnerError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HookContainerRunOutcome {
    pub(crate) container_id: ContainerId,
    pub(crate) exit_code: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HookContainerRunError {
    Domain(MachineContainerRunHookDomainError),
    Runner(MachineContainerRunnerError),
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
        .map_err(ServiceContainerRunError::Runner)?;
    let CreateManagedContainer {
        pull,
        runtime,
        provisioned_volumes,
        identity,
    } = command;

    match decide_container_run(&identity, existing) {
        MachineContainerRunDecision::Create { identity } => {
            let service_id = identity.service_id.clone();
            let namespace_revision_entry_id = identity.namespace_revision_entry_id.clone();
            let container_id = match runner
                .create_managed_container(CreateManagedContainer {
                    pull,
                    runtime,
                    provisioned_volumes,
                    identity,
                })
                .await
            {
                Ok(container_id) => container_id,
                Err(MachineContainerRunnerError::ImagePull { message }) => {
                    return Err(ServiceContainerRunError::Domain(
                        MachineContainerRunDomainError::ImagePullFailed {
                            service_id,
                            namespace_revision_entry_id,
                            message: failure_message(message),
                        },
                    ));
                }
                Err(error @ MachineContainerRunnerError::ListExisting { .. })
                | Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. })
                | Err(error @ MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. })
                | Err(error @ MachineContainerRunnerError::Create { .. })
                | Err(error @ MachineContainerRunnerError::Start { .. })
                | Err(error @ MachineContainerRunnerError::Wait { .. })
                | Err(error @ MachineContainerRunnerError::Stop { .. })
                | Err(error @ MachineContainerRunnerError::Restart { .. })
                | Err(error @ MachineContainerRunnerError::Remove { .. })
                | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
                    return Err(ServiceContainerRunError::Runner(error));
                }
            };

            match runner.start_managed_container(&container_id).await {
                Ok(()) => Ok(MachineRunContainerOutcome::Created { container_id }),
                Err(MachineContainerRunnerError::Start { message, .. }) => {
                    Err(ServiceContainerRunError::Domain(
                        MachineContainerRunDomainError::CreatedContainerStartFailed {
                            container_id: container_id.clone(),
                            message: failure_message(format!("container start failed: {message}")),
                            inspect_hint: inspect_hint(&container_id),
                        },
                    ))
                }
                Err(error @ MachineContainerRunnerError::ListExisting { .. })
                | Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. })
                | Err(error @ MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. })
                | Err(error @ MachineContainerRunnerError::Create { .. })
                | Err(error @ MachineContainerRunnerError::ImagePull { .. })
                | Err(error @ MachineContainerRunnerError::Wait { .. })
                | Err(error @ MachineContainerRunnerError::Stop { .. })
                | Err(error @ MachineContainerRunnerError::Restart { .. })
                | Err(error @ MachineContainerRunnerError::Remove { .. })
                | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
                    Err(ServiceContainerRunError::Runner(error))
                }
            }
        }
        MachineContainerRunDecision::ReuseRunning { container_id } => {
            Ok(MachineRunContainerOutcome::ReusedRunning { container_id })
        }
        MachineContainerRunDecision::StartExisting { container_id } => {
            match runner.start_managed_container(&container_id).await {
                Ok(()) => Ok(MachineRunContainerOutcome::StartedExisting { container_id }),
                Err(MachineContainerRunnerError::Start { message, .. }) => {
                    Err(ServiceContainerRunError::Domain(
                        MachineContainerRunDomainError::ExistingContainerStartFailed {
                            container_id: container_id.clone(),
                            message: failure_message(format!("container start failed: {message}")),
                            inspect_hint: inspect_hint(&container_id),
                        },
                    ))
                }
                Err(error @ MachineContainerRunnerError::ListExisting { .. })
                | Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. })
                | Err(error @ MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. })
                | Err(error @ MachineContainerRunnerError::Create { .. })
                | Err(error @ MachineContainerRunnerError::ImagePull { .. })
                | Err(error @ MachineContainerRunnerError::Wait { .. })
                | Err(error @ MachineContainerRunnerError::Stop { .. })
                | Err(error @ MachineContainerRunnerError::Restart { .. })
                | Err(error @ MachineContainerRunnerError::Remove { .. })
                | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
                    Err(ServiceContainerRunError::Runner(error))
                }
            }
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
        .map_err(HookContainerRunError::Runner)?;
    let CreateManagedContainer {
        pull,
        runtime,
        provisioned_volumes,
        identity,
    } = command;
    let expected_identity = identity.clone();

    let container_id = match decide_container_run(&identity, existing) {
        MachineContainerRunDecision::Create { identity } => {
            let container_id = match runner
                .create_managed_container(CreateManagedContainer {
                    pull,
                    runtime,
                    provisioned_volumes,
                    identity,
                })
                .await
            {
                Ok(container_id) => container_id,
                Err(MachineContainerRunnerError::Create { message }) => {
                    return Err(HookContainerRunError::Domain(
                        MachineContainerRunHookDomainError::CreateFailed {
                            message: failure_message(format!(
                                "hook container create failed: {message}"
                            )),
                        },
                    ));
                }
                Err(error @ MachineContainerRunnerError::ListExisting { .. })
                | Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. })
                | Err(error @ MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. })
                | Err(error @ MachineContainerRunnerError::ImagePull { .. })
                | Err(error @ MachineContainerRunnerError::Start { .. })
                | Err(error @ MachineContainerRunnerError::Wait { .. })
                | Err(error @ MachineContainerRunnerError::Stop { .. })
                | Err(error @ MachineContainerRunnerError::Restart { .. })
                | Err(error @ MachineContainerRunnerError::Remove { .. })
                | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
                    return Err(HookContainerRunError::Runner(error));
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
            Ok(Err(MachineContainerRunnerError::Wait { message, .. })) => {
                return Err(HookContainerRunError::Domain(
                    MachineContainerRunHookDomainError::WaitFailed {
                        container_id: container_id.clone(),
                        message: failure_message(format!("hook container wait failed: {message}")),
                        log_hint: log_hint(&container_id),
                    },
                ));
            }
            Ok(Err(error @ MachineContainerRunnerError::ListExisting { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Create { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::ImagePull { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Start { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Stop { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Restart { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Remove { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::RemoveVolume { .. })) => {
                return Err(HookContainerRunError::Runner(error));
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
                    Err(MachineContainerRunnerError::Stop { message, .. }) => format!(
                        "hook timed out after {}ms and could not be stopped: {message}",
                        timeout.as_millis()
                    ),
                    Err(error @ MachineContainerRunnerError::ListExisting { .. })
                    | Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. })
                    | Err(
                        error @ MachineContainerRunnerError::EndpointNetworkSubnetMismatch {
                            ..
                        },
                    )
                    | Err(error @ MachineContainerRunnerError::Create { .. })
                    | Err(error @ MachineContainerRunnerError::ImagePull { .. })
                    | Err(error @ MachineContainerRunnerError::Start { .. })
                    | Err(error @ MachineContainerRunnerError::Wait { .. })
                    | Err(error @ MachineContainerRunnerError::Restart { .. })
                    | Err(error @ MachineContainerRunnerError::Remove { .. })
                    | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
                        return Err(HookContainerRunError::Runner(error));
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

async fn start_hook_container<R>(
    runner: &R,
    container_id: ContainerId,
) -> Result<ContainerId, HookContainerRunError>
where
    R: MachineContainerRunner,
{
    match runner.start_managed_container(&container_id).await {
        Ok(()) => Ok(container_id),
        Err(MachineContainerRunnerError::Start { message, .. }) => Err(
            HookContainerRunError::Domain(MachineContainerRunHookDomainError::StartFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("hook container start failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            }),
        ),
        Err(error @ MachineContainerRunnerError::ListExisting { .. })
        | Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. })
        | Err(error @ MachineContainerRunnerError::EndpointNetworkSubnetMismatch { .. })
        | Err(error @ MachineContainerRunnerError::Create { .. })
        | Err(error @ MachineContainerRunnerError::ImagePull { .. })
        | Err(error @ MachineContainerRunnerError::Wait { .. })
        | Err(error @ MachineContainerRunnerError::Stop { .. })
        | Err(error @ MachineContainerRunnerError::Restart { .. })
        | Err(error @ MachineContainerRunnerError::Remove { .. })
        | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
            Err(HookContainerRunError::Runner(error))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MachineContainerRunDecision {
    Create {
        identity: ManagedContainerIdentity,
    },
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
        return MachineContainerRunDecision::Create {
            identity: expected.clone(),
        };
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
