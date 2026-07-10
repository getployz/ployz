use super::current_unix_ms;
use super::facts::observation_state;
use super::response::{
    container_start_error, failure_message, inspect_hint, log_hint, machine_domain_error,
    machine_success, runner_error,
};
use crate::roles::machine::protocol::{
    MachineContainerInspectDomainError, MachineContainerInspectRpcOk,
    MachineContainerInspectRpcRequest, MachineContainerInspectRpcResponse,
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerRemoveRpcResponse, MachineContainerResolveImageDomainError,
    MachineContainerResolveImageRpcOk, MachineContainerResolveImageRpcRequest,
    MachineContainerResolveImageRpcResponse, MachineContainerRestartDomainError,
    MachineContainerRestartRpcRequest, MachineContainerRestartRpcResponse, MachineContainerRpcOk,
    MachineContainerRunDomainError, MachineContainerRunHookDomainError,
    MachineContainerRunHookRpcOk, MachineContainerRunHookRpcRequest,
    MachineContainerRunHookRpcResponse, MachineContainerRunRpcOk, MachineContainerRunRpcRequest,
    MachineContainerRunRpcResponse, MachineContainerStopDomainError,
    MachineContainerStopRpcRequest, MachineContainerStopRpcResponse,
    MachineEnsureEndpointNetworkDomainError, MachineEnsureEndpointNetworkRpcOk,
    MachineEnsureEndpointNetworkRpcRequest, MachineEnsureEndpointNetworkRpcResponse,
    MachineRunContainerOutcome, MachineVolumeRemoveDomainError, MachineVolumeRemoveRpcOk,
    MachineVolumeRemoveRpcRequest, MachineVolumeRemoveRpcResponse,
};
use crate::roles::machine::runner::{
    CreateManagedContainer, MachineContainerRunDecision, MachineContainerRunner,
    MachineContainerRunnerError, decide_container_run,
};
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::machine_runtime::{MachineContainerFactDelta, ManagedContainerObservation};
use ployz_core::subjects::machine_container_facts;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};
use std::time::Duration;

#[derive(Clone)]
pub(crate) struct MachineContainerState<R> {
    pub(crate) runner: R,
    pub(crate) client: ployz_nats::service_runtime::NatsClient,
}

pub(crate) async fn handle_ensure_endpoint_network<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    if let Err(response) = decode_json_request::<MachineEnsureEndpointNetworkRpcRequest>(&request) {
        return response;
    }

    match runner.ensure_endpoint_network().await {
        Ok(()) => machine_success(MachineEnsureEndpointNetworkRpcResponse::Ok(
            MachineEnsureEndpointNetworkRpcOk { machine_id },
        )),
        Err(MachineContainerRunnerError::EnsureEndpointNetwork { message }) => {
            machine_domain_error(MachineEnsureEndpointNetworkRpcResponse::DomainError {
                machine_id,
                error: MachineEnsureEndpointNetworkDomainError::EnsureFailed {
                    message: failure_message(format!("endpoint network ensure failed: {message}")),
                },
            })
        }
        Err(error) => runner_error(error),
    }
}

pub(crate) async fn handle_container_run<R>(
    machine_id: MachineId,
    state: MachineContainerState<R>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerRunRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let existing = match state.runner.existing_managed_containers().await {
        Ok(existing) => existing,
        Err(error) => return runner_error(error),
    };
    match decide_container_run(&request.container, existing) {
        MachineContainerRunDecision::Create { identity } => {
            let service_id = identity.service_id.clone();
            let namespace_revision_entry_id = identity.namespace_revision_entry_id.clone();
            match state
                .runner
                .create_managed_container(CreateManagedContainer {
                    pull: request.pull,
                    runtime: request.runtime,
                    identity,
                })
                .await
            {
                Ok(container_id) => match state.runner.start_managed_container(&container_id).await
                {
                    Ok(()) => {
                        container_run_success(
                            machine_id,
                            &state.runner,
                            &state.client,
                            MachineRunContainerOutcome::Created { container_id },
                        )
                        .await
                    }
                    Err(error) => container_start_error(
                        machine_id,
                        container_id,
                        error,
                        |container_id, message, inspect_hint| {
                            MachineContainerRunDomainError::CreatedContainerStartFailed {
                                container_id,
                                message,
                                inspect_hint,
                            }
                        },
                    ),
                },
                Err(MachineContainerRunnerError::ImagePull { message }) => {
                    machine_domain_error(MachineContainerRunRpcResponse::DomainError {
                        machine_id,
                        error: MachineContainerRunDomainError::ImagePullFailed {
                            service_id,
                            namespace_revision_entry_id,
                            message: failure_message(message),
                        },
                    })
                }
                Err(error) => runner_error(error),
            }
        }
        MachineContainerRunDecision::ReuseRunning { container_id } => {
            container_run_success(
                machine_id,
                &state.runner,
                &state.client,
                MachineRunContainerOutcome::ReusedRunning { container_id },
            )
            .await
        }
        MachineContainerRunDecision::StartExisting { container_id } => {
            match state.runner.start_managed_container(&container_id).await {
                Ok(()) => {
                    container_run_success(
                        machine_id,
                        &state.runner,
                        &state.client,
                        MachineRunContainerOutcome::StartedExisting { container_id },
                    )
                    .await
                }
                Err(error) => container_start_error(
                    machine_id,
                    container_id,
                    error,
                    |container_id, message, inspect_hint| {
                        MachineContainerRunDomainError::ExistingContainerStartFailed {
                            container_id,
                            message,
                            inspect_hint,
                        }
                    },
                ),
            }
        }
        MachineContainerRunDecision::NotStartable {
            container_id,
            state,
        } => machine_domain_error(MachineContainerRunRpcResponse::DomainError {
            machine_id,
            error: MachineContainerRunDomainError::OperationStepContainerNotStartable {
                container_id: container_id.clone(),
                message: failure_message(format!(
                    "operation step container is not startable: {state:?}"
                )),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        MachineContainerRunDecision::Ambiguous {
            operation_id,
            step_id,
            container_ids,
        } => machine_domain_error(MachineContainerRunRpcResponse::DomainError {
            machine_id,
            error: MachineContainerRunDomainError::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            },
        }),
    }
}

pub(crate) async fn handle_container_run_hook<R>(
    machine_id: MachineId,
    state: MachineContainerState<R>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerRunHookRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let existing = match state.runner.existing_managed_containers().await {
        Ok(existing) => existing,
        Err(error) => return runner_error(error),
    };
    let identity = request.container;
    let container_id = match decide_container_run(&identity, existing) {
        MachineContainerRunDecision::Create { identity } => {
            let container_id = match state
                .runner
                .create_managed_container(CreateManagedContainer {
                    pull: request.pull,
                    runtime: request.runtime,
                    identity,
                })
                .await
            {
                Ok(container_id) => container_id,
                Err(MachineContainerRunnerError::Create { message }) => {
                    return machine_domain_error(MachineContainerRunHookRpcResponse::DomainError {
                        machine_id,
                        error: MachineContainerRunHookDomainError::CreateFailed {
                            message: failure_message(format!(
                                "hook container create failed: {message}"
                            )),
                        },
                    });
                }
                Err(error @ MachineContainerRunnerError::ListExisting { .. })
                | Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. })
                | Err(error @ MachineContainerRunnerError::ImagePull { .. })
                | Err(error @ MachineContainerRunnerError::Start { .. })
                | Err(error @ MachineContainerRunnerError::Wait { .. })
                | Err(error @ MachineContainerRunnerError::Stop { .. })
                | Err(error @ MachineContainerRunnerError::Restart { .. })
                | Err(error @ MachineContainerRunnerError::Remove { .. })
                | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
                    return runner_error(error);
                }
            };
            if let Err(error) = state.runner.start_managed_container(&container_id).await {
                return hook_start_error(machine_id, container_id, error);
            }
            container_id
        }
        MachineContainerRunDecision::StartExisting { container_id } => {
            if let Err(error) = state.runner.start_managed_container(&container_id).await {
                return hook_start_error(machine_id, container_id, error);
            }
            container_id
        }
        MachineContainerRunDecision::ReuseRunning { container_id }
        | MachineContainerRunDecision::NotStartable { container_id, .. } => container_id,
        MachineContainerRunDecision::Ambiguous {
            operation_id,
            step_id,
            container_ids,
        } => {
            return machine_domain_error(MachineContainerRunHookRpcResponse::DomainError {
                machine_id,
                error: MachineContainerRunHookDomainError::OperationStepAmbiguous {
                    operation_id,
                    step_id,
                    container_ids,
                },
            });
        }
    };

    let timeout = Duration::from_millis(request.timeout_millis.max(1));
    let exit_code =
        match tokio::time::timeout(timeout, state.runner.wait_managed_container(&container_id))
            .await
        {
            Ok(Ok(exit_code)) => exit_code,
            Ok(Err(MachineContainerRunnerError::Wait { message, .. })) => {
                return machine_domain_error(MachineContainerRunHookRpcResponse::DomainError {
                    machine_id,
                    error: MachineContainerRunHookDomainError::WaitFailed {
                        container_id: container_id.clone(),
                        message: failure_message(format!("hook container wait failed: {message}")),
                        log_hint: log_hint(&container_id),
                    },
                });
            }
            Ok(Err(error @ MachineContainerRunnerError::ListExisting { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::EnsureEndpointNetwork { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Create { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::ImagePull { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Start { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Stop { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Restart { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::Remove { .. }))
            | Ok(Err(error @ MachineContainerRunnerError::RemoveVolume { .. })) => {
                return runner_error(error);
            }
            Err(_) => {
                let message = match state
                    .runner
                    .stop_managed_container(&container_id, &identity)
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
                    | Err(error @ MachineContainerRunnerError::Create { .. })
                    | Err(error @ MachineContainerRunnerError::ImagePull { .. })
                    | Err(error @ MachineContainerRunnerError::Start { .. })
                    | Err(error @ MachineContainerRunnerError::Wait { .. })
                    | Err(error @ MachineContainerRunnerError::Restart { .. })
                    | Err(error @ MachineContainerRunnerError::Remove { .. })
                    | Err(error @ MachineContainerRunnerError::RemoveVolume { .. }) => {
                        return runner_error(error);
                    }
                };
                return machine_domain_error(MachineContainerRunHookRpcResponse::DomainError {
                    machine_id,
                    error: MachineContainerRunHookDomainError::TimedOut {
                        container_id: container_id.clone(),
                        timeout_millis: request.timeout_millis,
                        message: failure_message(message),
                        inspect_hint: inspect_hint(&container_id),
                    },
                });
            }
        };

    machine_success(MachineContainerRunHookRpcResponse::Ok(
        MachineContainerRunHookRpcOk {
            machine_id,
            container_id,
            exit_code,
        },
    ))
}

fn hook_start_error(
    machine_id: MachineId,
    container_id: ContainerId,
    error: MachineContainerRunnerError,
) -> NatsServiceResponse {
    match error {
        MachineContainerRunnerError::Start { message, .. } => {
            machine_domain_error(MachineContainerRunHookRpcResponse::DomainError {
                machine_id,
                error: MachineContainerRunHookDomainError::StartFailed {
                    container_id: container_id.clone(),
                    message: failure_message(format!("hook container start failed: {message}")),
                    inspect_hint: inspect_hint(&container_id),
                },
            })
        }
        error @ (MachineContainerRunnerError::ListExisting { .. }
        | MachineContainerRunnerError::EnsureEndpointNetwork { .. }
        | MachineContainerRunnerError::Create { .. }
        | MachineContainerRunnerError::ImagePull { .. }
        | MachineContainerRunnerError::Wait { .. }
        | MachineContainerRunnerError::Stop { .. }
        | MachineContainerRunnerError::Restart { .. }
        | MachineContainerRunnerError::Remove { .. }
        | MachineContainerRunnerError::RemoveVolume { .. }) => runner_error(error),
    }
}

pub(crate) async fn handle_container_inspect<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerInspectRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match live_container_observation(&machine_id, &runner, &request.container_id).await {
        Some(observation) => machine_success(MachineContainerInspectRpcResponse::Ok(
            MachineContainerInspectRpcOk {
                machine_id,
                observed_at_unix_ms: current_unix_ms(),
                observation: Some(observation),
            },
        )),
        None => match runner.existing_managed_containers().await {
            Ok(_) => machine_success(MachineContainerInspectRpcResponse::Ok(
                MachineContainerInspectRpcOk {
                    machine_id,
                    observed_at_unix_ms: current_unix_ms(),
                    observation: None,
                },
            )),
            Err(error) => machine_domain_error(MachineContainerInspectRpcResponse::DomainError {
                machine_id,
                error: MachineContainerInspectDomainError::InspectFailed {
                    container_id: request.container_id,
                    message: failure_message(format!("container inspect failed: {error:?}")),
                },
            }),
        },
    }
}

pub(crate) async fn handle_container_resolve_image<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerResolveImageRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match runner
        .resolve_registry_image(&request.reference, request.credential.as_ref())
        .await
    {
        Ok(digest) => machine_success(MachineContainerResolveImageRpcResponse::Ok(
            MachineContainerResolveImageRpcOk { machine_id, digest },
        )),
        Err(MachineContainerRunnerError::ImagePull { message }) => {
            machine_domain_error(MachineContainerResolveImageRpcResponse::DomainError {
                machine_id,
                error: MachineContainerResolveImageDomainError::ResolveFailed {
                    message: failure_message(message),
                },
            })
        }
        Err(error) => runner_error(error),
    }
}

pub(crate) async fn handle_container_remove<R>(
    machine_id: MachineId,
    state: MachineContainerState<R>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerRemoveRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match state
        .runner
        .remove_managed_container(&request.container_id, &request.expected_identity)
        .await
    {
        Ok(()) => {
            publish_container_removed_delta(
                &state.client,
                &machine_id,
                request.container_id.clone(),
            )
            .await;
            machine_success(MachineContainerRemoveRpcResponse::Ok(
                MachineContainerRpcOk {
                    machine_id,
                    container_id: request.container_id,
                },
            ))
        }
        Err(MachineContainerRunnerError::Remove {
            container_id,
            message,
        }) => machine_domain_error(MachineContainerRemoveRpcResponse::DomainError {
            machine_id,
            error: MachineContainerRemoveDomainError::RemoveFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("container remove failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        Err(error) => runner_error(error),
    }
}

pub(crate) async fn handle_volume_remove<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineVolumeRemoveRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match runner.remove_volume(&request.docker_volume_name).await {
        Ok(()) => machine_success(MachineVolumeRemoveRpcResponse::Ok(
            MachineVolumeRemoveRpcOk { machine_id },
        )),
        Err(MachineContainerRunnerError::RemoveVolume { message, .. }) => {
            machine_domain_error(MachineVolumeRemoveRpcResponse::DomainError {
                machine_id,
                error: MachineVolumeRemoveDomainError::RemoveFailed {
                    message: failure_message(format!("volume remove failed: {message}")),
                },
            })
        }
        Err(error) => runner_error(error),
    }
}

pub(crate) async fn handle_container_stop<R>(
    machine_id: MachineId,
    state: MachineContainerState<R>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerStopRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match state
        .runner
        .stop_managed_container(&request.container_id, &request.expected_identity)
        .await
    {
        Ok(()) => {
            publish_container_observed_delta(
                &state.client,
                &machine_id,
                &state.runner,
                &request.container_id,
            )
            .await;
            machine_success(MachineContainerStopRpcResponse::Ok(MachineContainerRpcOk {
                machine_id,
                container_id: request.container_id,
            }))
        }
        Err(MachineContainerRunnerError::Stop {
            container_id,
            message,
        }) => machine_domain_error(MachineContainerStopRpcResponse::DomainError {
            machine_id,
            error: MachineContainerStopDomainError::StopFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("container stop failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        Err(error) => runner_error(error),
    }
}

pub(crate) async fn handle_container_restart<R>(
    machine_id: MachineId,
    state: MachineContainerState<R>,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineContainerRestartRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match state
        .runner
        .restart_managed_container(&request.container_id, &request.expected_identity)
        .await
    {
        Ok(()) => {
            publish_container_observed_delta(
                &state.client,
                &machine_id,
                &state.runner,
                &request.container_id,
            )
            .await;
            machine_success(MachineContainerRestartRpcResponse::Ok(
                MachineContainerRpcOk {
                    machine_id,
                    container_id: request.container_id,
                },
            ))
        }
        Err(MachineContainerRunnerError::Restart {
            container_id,
            message,
        }) => machine_domain_error(MachineContainerRestartRpcResponse::DomainError {
            machine_id,
            error: MachineContainerRestartDomainError::RestartFailed {
                container_id: container_id.clone(),
                message: failure_message(format!("container restart failed: {message}")),
                inspect_hint: inspect_hint(&container_id),
            },
        }),
        Err(error) => runner_error(error),
    }
}

fn container_run_ok(
    machine_id: MachineId,
    outcome: MachineRunContainerOutcome,
) -> MachineContainerRunRpcResponse {
    MachineContainerRunRpcResponse::Ok(MachineContainerRunRpcOk {
        machine_id,
        outcome,
    })
}

async fn container_run_success<R>(
    machine_id: MachineId,
    runner: &R,
    client: &ployz_nats::service_runtime::NatsClient,
    outcome: MachineRunContainerOutcome,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    publish_container_observed_delta(client, &machine_id, runner, outcome.container_id()).await;
    machine_success(container_run_ok(machine_id, outcome))
}

async fn publish_container_observed_delta<R>(
    client: &ployz_nats::service_runtime::NatsClient,
    machine_id: &MachineId,
    runner: &R,
    container_id: &ContainerId,
) where
    R: MachineContainerRunner,
{
    let Some(observation) = live_container_observation(machine_id, runner, container_id).await
    else {
        return;
    };
    let delta = MachineContainerFactDelta::ContainerObserved {
        observed_at_unix_ms: current_unix_ms(),
        observation: Box::new(observation),
    };
    publish_machine_container_fact(client, machine_id, &delta).await;
}

async fn publish_container_removed_delta(
    client: &ployz_nats::service_runtime::NatsClient,
    machine_id: &MachineId,
    container_id: ContainerId,
) {
    let delta = MachineContainerFactDelta::ContainerRemoved {
        machine_id: machine_id.clone(),
        container_id,
        observed_at_unix_ms: current_unix_ms(),
    };
    publish_machine_container_fact(client, machine_id, &delta).await;
}

async fn publish_machine_container_fact(
    client: &ployz_nats::service_runtime::NatsClient,
    machine_id: &MachineId,
    delta: &MachineContainerFactDelta,
) {
    let Ok(payload) = serde_json::to_vec(delta) else {
        return;
    };
    let _ = client
        .publish(machine_container_facts(machine_id), payload.into())
        .await;
}

async fn live_container_observation<R>(
    machine_id: &MachineId,
    runner: &R,
    container_id: &ContainerId,
) -> Option<ManagedContainerObservation>
where
    R: MachineContainerRunner,
{
    runner
        .existing_managed_containers()
        .await
        .ok()?
        .into_iter()
        .find(|container| &container.container_id == container_id)
        .map(|container| ManagedContainerObservation {
            machine_id: machine_id.clone(),
            container_id: container.container_id,
            identity: container.identity,
            state: observation_state(container.state),
            health_status: container.health_status,
            resolved_image_identity: container.resolved_image_identity,
            created_at_unix_seconds: container.created_at_unix_seconds,
        })
}
