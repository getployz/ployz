use super::current_unix_ms;
use super::deploy_container_run::{
    HookContainerInfrastructureError, HookContainerRunError, ServiceContainerInfrastructureError,
    ServiceContainerRunError, run_hook_container, run_service_container,
};
use super::facts::observation_state;
use super::response::{
    container_list_error, failure_message, inspect_hint, machine_domain_error, machine_success,
};
use crate::roles::machine::protocol::{
    MachineContainerInspectDomainError, MachineContainerInspectRpcOk,
    MachineContainerInspectRpcRequest, MachineContainerInspectRpcResponse,
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerRemoveRpcResponse, MachineContainerResolveImageDomainError,
    MachineContainerResolveImageRpcOk, MachineContainerResolveImageRpcRequest,
    MachineContainerResolveImageRpcResponse, MachineContainerRestartDomainError,
    MachineContainerRestartRpcRequest, MachineContainerRestartRpcResponse, MachineContainerRpcOk,
    MachineContainerRunHookRpcOk, MachineContainerRunHookRpcRequest,
    MachineContainerRunHookRpcResponse, MachineContainerRunRpcOk, MachineContainerRunRpcRequest,
    MachineContainerRunRpcResponse, MachineContainerStopDomainError,
    MachineContainerStopRpcRequest, MachineContainerStopRpcResponse, MachineRunContainerOutcome,
    MachineVolumeEnsureRpcOk, MachineVolumeEnsureRpcRequest, MachineVolumeEnsureRpcResponse,
    MachineVolumeRemoveDomainError, MachineVolumeRemoveRpcOk, MachineVolumeRemoveRpcRequest,
    MachineVolumeRemoveRpcResponse,
};
use crate::roles::machine::runner::{
    CreateManagedContainer, MachineContainerListError, MachineContainerRemoveError,
    MachineContainerRestartError, MachineContainerRunner, MachineContainerStopError,
    MachineRegistryImageResolveError, MachineVolumeRemoveError,
};
use crate::roles::machine::volume::docker_volume_name;
use ployz_core::ids::{ContainerId, MachineId};
use ployz_core::intent::VolumePinState;
use ployz_core::machine::VolumeEnsureFailure;
use ployz_core::machine::runtime::{MachineContainerFactDelta, ManagedContainerObservation};
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceRequest, NatsServiceResponse, decode_json_request,
};
use ployz_nats::subjects::machine_container_facts;

#[derive(Clone)]
pub(crate) struct MachineContainerState<R> {
    pub(crate) runner: R,
    pub(crate) client: ployz_nats::service_runtime::NatsClient,
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
    match run_service_container(
        &state.runner,
        CreateManagedContainer {
            pull: request.pull,
            runtime: request.runtime,
            provisioned_volumes: request.provisioned_volumes,
            identity: request.container,
        },
    )
    .await
    {
        Ok(outcome) => {
            container_run_success(machine_id, &state.runner, &state.client, outcome).await
        }
        Err(ServiceContainerRunError::Domain(error)) => {
            machine_domain_error(MachineContainerRunRpcResponse::DomainError { machine_id, error })
        }
        Err(ServiceContainerRunError::Infrastructure(error)) => {
            service_container_infrastructure_error(error)
        }
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
    match run_hook_container(
        &state.runner,
        CreateManagedContainer {
            pull: request.pull,
            runtime: request.runtime,
            provisioned_volumes: request.provisioned_volumes,
            identity: request.container,
        },
        request.timeout_millis,
    )
    .await
    {
        Ok(outcome) => machine_success(MachineContainerRunHookRpcResponse::Ok(
            MachineContainerRunHookRpcOk {
                machine_id,
                container_id: outcome.container_id,
                exit_code: outcome.exit_code,
            },
        )),
        Err(HookContainerRunError::Domain(error)) => {
            machine_domain_error(MachineContainerRunHookRpcResponse::DomainError {
                machine_id,
                error,
            })
        }
        Err(HookContainerRunError::Infrastructure(error)) => {
            hook_container_infrastructure_error(error)
        }
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
        Err(MachineRegistryImageResolveError::ImagePull { message }) => {
            let message = match request.credential.as_ref() {
                Some(credential) => credential.redact_secret_in(message),
                None => message,
            };
            machine_domain_error(MachineContainerResolveImageRpcResponse::DomainError {
                machine_id,
                error: MachineContainerResolveImageDomainError::ResolveFailed {
                    message: failure_message(message),
                },
            })
        }
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
        Err(MachineContainerRemoveError::Remove {
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
        Err(MachineContainerRemoveError::ListExisting { message }) => {
            container_list_error(MachineContainerListError::ListExisting { message })
        }
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

    let volume = match &request {
        MachineVolumeRemoveRpcRequest::DockerReference { volume, .. } => volume,
        MachineVolumeRemoveRpcRequest::ProvisionedDataset { volume, .. } => volume.volume(),
    };
    if volume.machine_id() != &machine_id {
        return machine_domain_error(MachineVolumeRemoveRpcResponse::DomainError {
            error: MachineVolumeRemoveDomainError::MachineMismatch {
                expected_machine_id: volume.machine_id().clone(),
                responder_machine_id: machine_id.clone(),
            },
            machine_id,
        });
    }

    match request {
        MachineVolumeRemoveRpcRequest::DockerReference { volume, .. } => {
            let docker_volume_name =
                docker_volume_name(volume.namespace_id(), volume.volume_name());
            match runner.remove_volume(&docker_volume_name).await {
                Ok(()) => machine_success(MachineVolumeRemoveRpcResponse::Ok(
                    MachineVolumeRemoveRpcOk { machine_id },
                )),
                Err(MachineVolumeRemoveError::RemoveVolume { message, .. }) => {
                    machine_domain_error(MachineVolumeRemoveRpcResponse::DomainError {
                        machine_id,
                        error: MachineVolumeRemoveDomainError::DockerRemoveFailed {
                            message: failure_message(format!("volume remove failed: {message}")),
                        },
                    })
                }
            }
        }
        MachineVolumeRemoveRpcRequest::ProvisionedDataset { volume, .. } => {
            let pin = volume.volume();
            let dataset = volume.dataset();
            let docker_volume_name = docker_volume_name(pin.namespace_id(), pin.volume_name());
            if let Err(error) = runner.remove_volume(&docker_volume_name).await {
                return match error {
                    MachineVolumeRemoveError::RemoveVolume { message, .. } => {
                        machine_domain_error(MachineVolumeRemoveRpcResponse::DomainError {
                            machine_id,
                            error: MachineVolumeRemoveDomainError::DockerRemoveFailed {
                                message: failure_message(format!(
                                    "volume remove failed before dataset destroy: {message}"
                                )),
                            },
                        })
                    }
                };
            }
            match runner.destroy_provisioned_dataset(dataset).await {
                Ok(()) => machine_success(MachineVolumeRemoveRpcResponse::Ok(
                    MachineVolumeRemoveRpcOk { machine_id },
                )),
                Err(failure) => machine_domain_error(MachineVolumeRemoveRpcResponse::DomainError {
                    machine_id,
                    error: MachineVolumeRemoveDomainError::DatasetDestroyFailed {
                        dataset: dataset.clone(),
                        failure,
                    },
                }),
            }
        }
    }
}

pub(crate) async fn handle_volume_ensure<R>(
    machine_id: MachineId,
    runner: R,
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
{
    let request = match decode_json_request::<MachineVolumeEnsureRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(failure) = validate_volume_target(&machine_id, &request.volume) {
        return machine_domain_error(MachineVolumeEnsureRpcResponse::DomainError {
            machine_id,
            error: failure,
        });
    }
    match runner.ensure_volume(&request.volume).await {
        Ok(()) => machine_success(MachineVolumeEnsureRpcResponse::Ok(
            MachineVolumeEnsureRpcOk { machine_id },
        )),
        Err(failure) => machine_domain_error(MachineVolumeEnsureRpcResponse::DomainError {
            machine_id,
            error: failure,
        }),
    }
}

fn validate_volume_target(
    responder_machine_id: &MachineId,
    volume: &VolumePinState,
) -> Result<(), VolumeEnsureFailure> {
    if volume.machine_id() == responder_machine_id {
        return Ok(());
    }
    Err(VolumeEnsureFailure::MachineMismatch {
        expected_machine_id: volume.machine_id().clone(),
        responder_machine_id: responder_machine_id.clone(),
    })
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
        Err(MachineContainerStopError::Stop {
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
        Err(MachineContainerStopError::ListExisting { message }) => {
            container_list_error(MachineContainerListError::ListExisting { message })
        }
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
        Err(MachineContainerRestartError::Restart {
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
        Err(MachineContainerRestartError::ListExisting { message }) => {
            container_list_error(MachineContainerListError::ListExisting { message })
        }
    }
}

fn service_container_infrastructure_error(
    error: ServiceContainerInfrastructureError,
) -> NatsServiceResponse {
    let message = match error {
        ServiceContainerInfrastructureError::List { message } => {
            format!("container list failed: {message}")
        }
        ServiceContainerInfrastructureError::Create { message } => {
            format!("container create failed: {message}")
        }
        ServiceContainerInfrastructureError::EnsureEndpointNetwork { message } => {
            format!("endpoint network ensure failed: {message}")
        }
        ServiceContainerInfrastructureError::EndpointNetworkSubnetMismatch {
            expected,
            observed,
        } => format!("endpoint network subnet is {observed:?}, expected {expected:?}"),
    };
    NatsServiceResponse::transport_error(NatsServiceError::internal(message))
}

fn hook_container_infrastructure_error(
    error: HookContainerInfrastructureError,
) -> NatsServiceResponse {
    let message = match error {
        HookContainerInfrastructureError::List { message }
        | HookContainerInfrastructureError::TimeoutStopList { message } => {
            format!("container list failed: {message}")
        }
        HookContainerInfrastructureError::ImagePull { message } => {
            format!("image pull failed: {message}")
        }
        HookContainerInfrastructureError::EnsureEndpointNetwork { message } => {
            format!("endpoint network ensure failed: {message}")
        }
        HookContainerInfrastructureError::EndpointNetworkSubnetMismatch { expected, observed } => {
            format!("endpoint network subnet is {observed:?}, expected {expected:?}")
        }
    };
    NatsServiceResponse::transport_error(NatsServiceError::internal(message))
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

#[cfg(test)]
mod volume_ensure_tests {
    use super::validate_volume_target;
    use ployz_core::deploy::VolumeName;
    use ployz_core::ids::{MachineId, NamespaceId};
    use ployz_core::intent::VolumePinState;
    use ployz_core::machine::VolumeEnsureFailure;

    #[test]
    fn volume_ensure_rejects_a_pin_for_another_machine_before_effects() {
        let expected_machine_id = MachineId::try_new("machine-a").expect("machine");
        let responder_machine_id = MachineId::try_new("machine-b").expect("machine");
        let volume = VolumePinState::plain(
            NamespaceId::try_new("default").expect("namespace"),
            VolumeName::try_new("data").expect("volume"),
            expected_machine_id.clone(),
        );

        assert_eq!(
            validate_volume_target(&responder_machine_id, &volume),
            Err(VolumeEnsureFailure::MachineMismatch {
                expected_machine_id,
                responder_machine_id,
            })
        );
    }
}
