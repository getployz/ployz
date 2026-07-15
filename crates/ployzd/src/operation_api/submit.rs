//! Submit handlers: accept the operation quickly, start the owned worker,
//! and return the operation id + watch subject.

use crate::control::authorization::MintRequest;
use crate::operation_api::admission::{
    CoreReplaceSubmitCommand, CredentialGrantSubmitCommand, DeploySubmitCommand,
    IngressConfigureSubmitCommand, IngressConfigureSubmitError, MachineAddBootstrapMaterial,
    MachineAddBootstrapMaterialError, MachineAddSubmitCommand, MachineLifecycleSubmitCommand,
    MachineUpdateSubmitCommand, NamespaceRemoveSubmitCommand, NetworkRepairSubmitCommand,
    OperationControllers, ServiceRestartSubmitCommand, VolumeRemoveSubmitCommand,
};
use ployz_core::deploy::ImageSource;
use ployz_core::ids::{MachineId, NamespaceId, OperationId, ServiceId};
use ployz_core::internal_dns::InternalServiceName;
use ployz_core::ops::{CredentialGrantAction, EventSequence};
use ployz_core::state::MachineLifecycle;
use ployz_core::subjects::{OperationProgressScope, operation_progress_watch};
use ployz_sdk_types::{
    AcceptedOperation, CoreReplaceError, CoreReplaceRequest, CredentialAddError,
    CredentialAddRequest, CredentialRemoveError, CredentialRemoveRequest, DeployReserveError,
    DeployReserveRequest, DeployReserved, DeploySubmitError, DeploySubmitRequest,
    IngressConfigureError, IngressConfigureRequest, MachineAddAccepted, MachineAddError,
    MachineAddRequest, MachineJoinToken, MachineLifecycleError, MachineLifecycleRequest,
    MachineUpdateError, MachineUpdateRequest, NamespaceRemoveError, NamespaceRemoveRequest,
    NetworkRepairError, NetworkRepairRequest, ServiceRestartError, ServiceRestartRequest,
    VolumeRemoveError, VolumeRemoveRequest,
};

use super::OperationApiHandlers;
use super::error_map::{
    deploy_submit_error_from_submit_error, machine_add_error_from_submit_error,
};

#[must_use]
pub fn owned_operation(
    operation_id: OperationId,
    scope: OperationProgressScope,
    start_sequence: EventSequence,
) -> AcceptedOperation {
    let watch_subject = operation_progress_watch(&scope, &operation_id);
    AcceptedOperation {
        operation_id,
        watch_subject,
        start_sequence,
    }
}

impl From<DeploySubmitRequest> for DeploySubmitCommand {
    fn from(value: DeploySubmitRequest) -> Self {
        let DeploySubmitRequest {
            idempotency_key,
            reservation_id,
            target,
            registry_credentials,
        } = value;
        Self {
            operation_id: mint_deploy_operation_id(),
            idempotency_key,
            reservation_id,
            target,
            registry_credentials,
        }
    }
}

pub async fn deploy_reserve(
    handlers: &OperationApiHandlers,
    request: DeployReserveRequest,
) -> Result<DeployReserved, DeployReserveError> {
    handlers
        .controllers
        .reserve_deploy(&request.namespace_id)
        .await
        .map(|reservation| DeployReserved {
            reservation_id: reservation.reservation_id,
            expires_at: reservation.expires_at,
        })
        .map_err(|error| DeployReserveError::Unavailable {
            message: error.to_string(),
        })
}

pub async fn credential_add(
    handlers: &OperationApiHandlers,
    request: CredentialAddRequest,
) -> Result<AcceptedOperation, CredentialAddError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_credential_grant(CredentialGrantSubmitCommand {
            operation_id: request.operation_id,
            action: CredentialGrantAction::Add {
                grant: request.grant,
            },
        })
        .await
        .map_err(|error| credential_add_submit_error(operation_id.clone(), error))?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Cluster,
        accepted.start_sequence,
    );
    handlers.credential_grant().start(accepted);
    Ok(operation)
}

pub async fn credential_remove(
    handlers: &OperationApiHandlers,
    request: CredentialRemoveRequest,
) -> Result<AcceptedOperation, CredentialRemoveError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_credential_grant(CredentialGrantSubmitCommand {
            operation_id: request.operation_id,
            action: CredentialGrantAction::Remove {
                public_key: request.public_key,
            },
        })
        .await
        .map_err(|error| credential_remove_submit_error(operation_id.clone(), error))?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Cluster,
        accepted.start_sequence,
    );
    handlers.credential_grant().start(accepted);
    Ok(operation)
}

pub async fn ingress_configure(
    handlers: &OperationApiHandlers,
    request: IngressConfigureRequest,
) -> Result<AcceptedOperation, IngressConfigureError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_ingress_configure(
            IngressConfigureSubmitCommand {
                operation_id: request.operation_id,
                configuration: request.configuration,
            },
            handlers.ingress_intent(),
        )
        .await
        .map_err(|error| ingress_configure_submit_error(operation_id.clone(), error))?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Cluster,
        accepted.start_sequence,
    );
    handlers.ingress_configure().start(accepted);
    Ok(operation)
}

fn ingress_configure_submit_error(
    operation_id: OperationId,
    error: IngressConfigureSubmitError,
) -> IngressConfigureError {
    match error {
        IngressConfigureSubmitError::Busy { owner } => {
            IngressConfigureError::ResourceBusy { owner }
        }
        IngressConfigureSubmitError::InvalidConfiguration { message } => {
            IngressConfigureError::InvalidConfiguration { message }
        }
        IngressConfigureSubmitError::Unavailable { message } => {
            IngressConfigureError::Unavailable {
                operation_id,
                message,
            }
        }
        IngressConfigureSubmitError::Submit(error) => {
            match super::error_map::unfenced_submit_failure(
                "ingress configure",
                crate::operation_api::admission::SubmitCommandError::Submit(error),
            ) {
                super::error_map::UnfencedSubmitFailure::Unavailable { message } => {
                    IngressConfigureError::Unavailable {
                        operation_id,
                        message,
                    }
                }
                super::error_map::UnfencedSubmitFailure::DuplicateSequenceMismatch { sequence } => {
                    IngressConfigureError::DuplicateSequenceMismatch {
                        operation_id,
                        sequence,
                    }
                }
            }
        }
    }
}

fn credential_add_submit_error(
    operation_id: OperationId,
    error: crate::operation_api::admission::SubmitCommandError,
) -> CredentialAddError {
    match super::error_map::submit_failure(error) {
        super::error_map::SubmitFailure::Unavailable { message } => {
            CredentialAddError::Unavailable {
                operation_id,
                message,
            }
        }
        super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
            CredentialAddError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
        super::error_map::SubmitFailure::InvalidDeployTarget
        | super::error_map::SubmitFailure::ResourceBusy { .. } => {
            unreachable!("credential grants have no deploy target or namespace fence")
        }
    }
}

fn credential_remove_submit_error(
    operation_id: OperationId,
    error: crate::operation_api::admission::SubmitCommandError,
) -> CredentialRemoveError {
    match super::error_map::submit_failure(error) {
        super::error_map::SubmitFailure::Unavailable { message } => {
            CredentialRemoveError::Unavailable {
                operation_id,
                message,
            }
        }
        super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
            CredentialRemoveError::DuplicateSequenceMismatch {
                operation_id,
                sequence,
            }
        }
        super::error_map::SubmitFailure::InvalidDeployTarget
        | super::error_map::SubmitFailure::ResourceBusy { .. } => {
            unreachable!("credential grants have no deploy target or namespace fence")
        }
    }
}

fn mint_deploy_operation_id() -> OperationId {
    OperationId::try_new(format!("op_deploy_{}", nuid::next()))
        .expect("generated deploy operation id uses subject-token characters")
}

impl From<MachineUpdateRequest> for MachineUpdateSubmitCommand {
    fn from(value: MachineUpdateRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            machine_id: value.machine_id,
            target_version: value.target_version,
        }
    }
}

pub async fn deploy_submit(
    handlers: &OperationApiHandlers,
    command: DeploySubmitCommand,
) -> Result<AcceptedOperation, DeploySubmitError> {
    let operation_id = command.operation_id.clone();
    for service in &command.target.services {
        validate_internal_dns_name(&command.target.namespace_id, &service.service_id).map_err(
            |message| DeploySubmitError::InvalidTarget {
                operation_id: operation_id.clone(),
                message,
            },
        )?;
    }
    validate_registry_credentials(&command)?;
    validate_pushed_image_seeds(handlers, &command).await?;
    let accepted_execution = handlers
        .controllers
        .submit_deploy(command)
        .await
        .map_err(|error| deploy_submit_error_from_submit_error(operation_id, error))?;
    let accepted = &accepted_execution.submission;
    let scope = OperationProgressScope::Namespace {
        namespace_id: accepted.target.namespace_id.clone(),
    };
    let operation = owned_operation(
        accepted.operation_id.clone(),
        scope,
        accepted.start_sequence,
    );
    handlers.deploy_driver.start(accepted_execution);

    Ok(operation)
}

fn validate_internal_dns_name(
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
) -> Result<(), ployz_core::ops::FailureMessage> {
    InternalServiceName::try_from_ids(service_id, namespace_id)
        .map(|_| ())
        .map_err(|_| {
            ployz_core::ops::FailureMessage::try_new(format!(
                "service {} in namespace {} cannot form internal DNS name because each label is limited to 63 bytes",
                service_id.as_str(),
                namespace_id.as_str()
            ))
            .expect("generated internal DNS validation message is non-empty")
        })
}

fn validate_registry_credentials(command: &DeploySubmitCommand) -> Result<(), DeploySubmitError> {
    for service_id in command.registry_credentials.keys() {
        let Some(service) = command
            .target
            .services
            .iter()
            .find(|service| service.service_id == *service_id)
        else {
            return Err(invalid_registry_credential(
                command,
                service_id,
                "does not name a service in the deploy target",
            ));
        };
        if !matches!(service.image_source, ImageSource::Registry) {
            return Err(invalid_registry_credential(
                command,
                service_id,
                "belongs to a pushed image",
            ));
        }
    }

    for service in &command.target.services {
        let ImageSource::PushedToSeed {
            manifest_digest, ..
        } = &service.image_source
        else {
            continue;
        };
        let Some(pinned_digest) = service.image.pinned_digest() else {
            return Err(invalid_pushed_image(
                command,
                service,
                "must be digest-pinned",
            ));
        };
        if &pinned_digest != manifest_digest {
            return Err(invalid_pushed_image(
                command,
                service,
                "digest must match its pushed manifest digest",
            ));
        }
    }
    Ok(())
}

fn invalid_pushed_image(
    command: &DeploySubmitCommand,
    service: &ployz_core::deploy::DeployServiceSpec,
    reason: &str,
) -> DeploySubmitError {
    DeploySubmitError::InvalidTarget {
        operation_id: command.operation_id.clone(),
        message: ployz_core::ops::FailureMessage::try_new(format!(
            "pushed image for service {} {reason}",
            service.service_id.as_str()
        ))
        .expect("generated pushed image failure message is non-empty"),
    }
}

fn invalid_registry_credential(
    command: &DeploySubmitCommand,
    service_id: &ployz_core::ids::ServiceId,
    reason: &str,
) -> DeploySubmitError {
    DeploySubmitError::InvalidTarget {
        operation_id: command.operation_id.clone(),
        message: ployz_core::ops::FailureMessage::try_new(format!(
            "registry credential for service {} {reason}",
            service_id.as_str()
        ))
        .expect("generated registry credential failure message is non-empty"),
    }
}

async fn validate_pushed_image_seeds(
    handlers: &OperationApiHandlers,
    command: &DeploySubmitCommand,
) -> Result<(), DeploySubmitError> {
    let seeds = command.target.services.iter().filter_map(|service| {
        let ImageSource::PushedToSeed { seed, .. } = &service.image_source else {
            return None;
        };
        Some(seed)
    });

    for seed in seeds {
        let active = handlers
            .machine_roster
            .active_machine(seed)
            .await
            .map_err(|error| DeploySubmitError::Unavailable {
                operation_id: command.operation_id.clone(),
                message: error.to_string(),
            })?;
        let Some(active) = active else {
            return Err(invalid_image_seed(
                command,
                seed,
                "is not in the active roster",
            ));
        };
        if !matches!(active.lifecycle, MachineLifecycle::Active) {
            return Err(invalid_image_seed(
                command,
                seed,
                "is not in the active lifecycle",
            ));
        }
    }

    Ok(())
}

fn invalid_image_seed(
    command: &DeploySubmitCommand,
    seed: &MachineId,
    reason: &str,
) -> DeploySubmitError {
    DeploySubmitError::InvalidTarget {
        operation_id: command.operation_id.clone(),
        message: ployz_core::ops::FailureMessage::try_new(format!(
            "pushed image seed {} {reason}",
            seed.as_str()
        ))
        .expect("generated pushed image seed failure message is non-empty"),
    }
}

pub async fn service_restart(
    handlers: &OperationApiHandlers,
    request: ServiceRestartRequest,
) -> Result<AcceptedOperation, ServiceRestartError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_service_restart(ServiceRestartSubmitCommand {
            operation_id: request.operation_id,
            namespace_id: request.namespace_id,
            service_id: request.service_id,
        })
        .await
        .map_err(|error| match super::error_map::submit_failure(error) {
            super::error_map::SubmitFailure::InvalidDeployTarget => {
                unreachable!("service restart submit is not deploy target")
            }
            super::error_map::SubmitFailure::ResourceBusy {
                namespace_id,
                owner,
            } => ServiceRestartError::ResourceBusy {
                operation_id: operation_id.clone(),
                namespace_id,
                owner_operation_id: owner,
            },
            super::error_map::SubmitFailure::Unavailable { message } => {
                ServiceRestartError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                }
            }
            super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
                ServiceRestartError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                }
            }
        })?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Namespace {
            namespace_id: accepted.namespace_id.clone(),
        },
        accepted.start_sequence,
    );
    handlers.service_restart().start(accepted);
    Ok(operation)
}

pub async fn network_repair(
    handlers: &OperationApiHandlers,
    request: NetworkRepairRequest,
) -> Result<AcceptedOperation, NetworkRepairError> {
    let existing = handlers
        .controllers()
        .repository()
        .get(&request.operation_id)
        .await
        .map_err(|error| NetworkRepairError::Unavailable {
            operation_id: request.operation_id.clone(),
            message: error.to_string(),
        })?;
    if existing.is_none() {
        let active_machine_ids = handlers
            .intent_reader()
            .intent()
            .await
            .map_err(|error| NetworkRepairError::Unavailable {
                operation_id: request.operation_id.clone(),
                message: error.to_string(),
            })?
            .active_machines
            .into_iter()
            .map(|machine| machine.machine_id)
            .collect::<Vec<_>>();
        validate_network_repair_preconditions(
            &request.operation_id,
            request.machine_id.as_ref(),
            &active_machine_ids,
        )?;
    }
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_network_repair(NetworkRepairSubmitCommand {
            operation_id: request.operation_id,
            target_machine_id: request.machine_id,
        })
        .await
        .map_err(|error| {
            match super::error_map::unfenced_submit_failure("network-repair", error) {
                super::error_map::UnfencedSubmitFailure::Unavailable { message } => {
                    NetworkRepairError::Unavailable {
                        operation_id: operation_id.clone(),
                        message,
                    }
                }
                super::error_map::UnfencedSubmitFailure::DuplicateSequenceMismatch { sequence } => {
                    NetworkRepairError::DuplicateSequenceMismatch {
                        operation_id: operation_id.clone(),
                        sequence,
                    }
                }
            }
        })?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Cluster,
        accepted.start_sequence,
    );
    handlers.network_repair().start(accepted);
    Ok(operation)
}

fn validate_network_repair_preconditions(
    operation_id: &OperationId,
    target_machine_id: Option<&MachineId>,
    active_machine_ids: &[MachineId],
) -> Result<(), NetworkRepairError> {
    if active_machine_ids.is_empty() {
        return Err(NetworkRepairError::NoActiveMachines {
            operation_id: operation_id.clone(),
        });
    }
    if let Some(machine_id) = target_machine_id
        && !active_machine_ids.contains(machine_id)
    {
        return Err(NetworkRepairError::TargetMachineNotFound {
            operation_id: operation_id.clone(),
            machine_id: machine_id.clone(),
        });
    }
    Ok(())
}

pub async fn namespace_remove(
    handlers: &OperationApiHandlers,
    request: NamespaceRemoveRequest,
) -> Result<AcceptedOperation, NamespaceRemoveError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_namespace_remove(NamespaceRemoveSubmitCommand {
            operation_id: request.operation_id,
            namespace_id: request.namespace_id,
        })
        .await
        .map_err(|error| match super::error_map::submit_failure(error) {
            super::error_map::SubmitFailure::InvalidDeployTarget => {
                unreachable!("namespace remove submit is not deploy target")
            }
            super::error_map::SubmitFailure::ResourceBusy {
                namespace_id,
                owner,
            } => NamespaceRemoveError::ResourceBusy {
                operation_id: operation_id.clone(),
                namespace_id,
                owner_operation_id: owner,
            },
            super::error_map::SubmitFailure::Unavailable { message } => {
                NamespaceRemoveError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                }
            }
            super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
                NamespaceRemoveError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                }
            }
        })?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Namespace {
            namespace_id: accepted.namespace_id.clone(),
        },
        accepted.start_sequence,
    );
    handlers.namespace_remove().start(accepted);
    Ok(operation)
}

pub async fn volume_remove(
    handlers: &OperationApiHandlers,
    request: VolumeRemoveRequest,
) -> Result<AcceptedOperation, VolumeRemoveError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_volume_remove(VolumeRemoveSubmitCommand {
            operation_id: request.operation_id,
            namespace_id: request.namespace_id,
            volume_name: request.volume_name,
        })
        .await
        .map_err(|error| match super::error_map::submit_failure(error) {
            super::error_map::SubmitFailure::InvalidDeployTarget => {
                unreachable!("volume remove submit is not deploy target")
            }
            super::error_map::SubmitFailure::ResourceBusy {
                namespace_id,
                owner,
            } => VolumeRemoveError::ResourceBusy {
                operation_id: operation_id.clone(),
                namespace_id,
                owner_operation_id: owner,
            },
            super::error_map::SubmitFailure::Unavailable { message } => {
                VolumeRemoveError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                }
            }
            super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
                VolumeRemoveError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                }
            }
        })?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Namespace {
            namespace_id: accepted.namespace_id.clone(),
        },
        accepted.start_sequence,
    );
    handlers.volume_remove().start(accepted);
    Ok(operation)
}

/// Accepts a machine-add: validates, persists the operation, and returns
/// the operation id + join token + join bundle quickly. It does **not**
/// mint, render, reload, or test-connect — credential minting runs as
/// owned operation work spawned after acceptance.
pub async fn machine_add(
    handlers: &OperationApiHandlers,
    request: MachineAddRequest,
) -> Result<MachineAddAccepted, MachineAddError> {
    let controllers = handlers.controllers();
    let operation_id = request.operation_id.clone();
    let idempotency_key = request.idempotency_key.clone();
    let material = machine_add_bootstrap_material(controllers, &request)
        .await
        .map_err(|error| MachineAddError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })?;
    let command = MachineAddSubmitCommand {
        operation_id: request.operation_id,
        idempotency_key: request.idempotency_key,
        machine_id: request.machine_id,
        name: request.name,
        roles: request.roles,
        host_port_assurance: request.host_port_assurance,
        endpoint_subnet: super::admission::MachineAddEndpointSubnet::Allocate,
        join_bundle: material.join_bundle,
        join_token: material.join_token,
        raw_join_token: material.raw_join_token,
    };

    let accepted = controllers
        .submit_machine_add(
            command,
            handlers.dataplane_endpoint_supernet(),
            &handlers.machine_roster,
        )
        .await
        .map_err(|error| machine_add_error_from_submit_error(operation_id.clone(), error))?;
    let raw_token =
        MachineJoinToken::try_new(accepted.identity.raw_join_token.as_str()).map_err(|_| {
            MachineAddError::Unavailable {
                operation_id: operation_id.clone(),
                message: "machine-add accepted raw join token is invalid".to_owned(),
            }
        })?;
    handlers.machine_mint.start(MintRequest {
        operation_id: accepted.operation_id.clone(),
        machine_id: accepted.identity.machine_id.clone(),
        idempotency_key,
    });
    tokio::spawn({
        let handlers = handlers.clone();
        async move {
            handlers.publish_pending_machine_joins().await;
        }
    });

    Ok(MachineAddAccepted {
        accepted: owned_machine_operation(
            accepted.operation_id,
            &accepted.identity.machine_id,
            accepted.start_sequence,
        ),
        machine_id: accepted.identity.machine_id,
        bootstrap_url: material.bootstrap_url,
        join_bundle: accepted.identity.join_bundle,
        join_token: raw_token,
        join_secret_delivery: material.join_secret_delivery,
    })
}

pub async fn machine_update(
    handlers: &OperationApiHandlers,
    request: MachineUpdateRequest,
) -> Result<AcceptedOperation, MachineUpdateError> {
    let operation_id = request.operation_id.clone();
    if &request.machine_id == handlers.local_machine_id() {
        return Err(MachineUpdateError::CurrentMachineUnsupported {
            operation_id,
            machine_id: request.machine_id,
        });
    }
    let operation_id = request.operation_id.clone();
    let target_machine = handlers
        .machine_roster
        .active_machine(&request.machine_id)
        .await
        .map_err(|error| MachineUpdateError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })?;
    if target_machine.is_none() {
        return Err(MachineUpdateError::NoSuchMachine {
            operation_id,
            machine_id: request.machine_id,
        });
    }
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_machine_update(request.into())
        .await
        .map_err(|error| {
            match super::error_map::unfenced_submit_failure("machine-update", error) {
                super::error_map::UnfencedSubmitFailure::Unavailable { message } => {
                    MachineUpdateError::Unavailable {
                        operation_id: operation_id.clone(),
                        message,
                    }
                }
                super::error_map::UnfencedSubmitFailure::DuplicateSequenceMismatch { sequence } => {
                    MachineUpdateError::DuplicateSequenceMismatch {
                        operation_id: operation_id.clone(),
                        sequence,
                    }
                }
            }
        })?;
    let operation = owned_machine_operation(
        accepted.operation_id.clone(),
        &accepted.machine_id,
        accepted.start_sequence,
    );
    handlers.machine_update().start(accepted);

    Ok(operation)
}

pub async fn machine_drain(
    handlers: &OperationApiHandlers,
    request: MachineLifecycleRequest,
) -> Result<AcceptedOperation, MachineLifecycleError> {
    machine_lifecycle(
        handlers,
        request.operation_id,
        request.machine_id,
        ployz_core::state::MachineLifecycle::Draining,
    )
    .await
}

pub async fn machine_resume(
    handlers: &OperationApiHandlers,
    request: MachineLifecycleRequest,
) -> Result<AcceptedOperation, MachineLifecycleError> {
    machine_lifecycle(
        handlers,
        request.operation_id,
        request.machine_id,
        ployz_core::state::MachineLifecycle::Active,
    )
    .await
}

async fn machine_lifecycle(
    handlers: &OperationApiHandlers,
    operation_id: ployz_core::ids::OperationId,
    machine_id: ployz_core::ids::MachineId,
    target: ployz_core::state::MachineLifecycle,
) -> Result<AcceptedOperation, MachineLifecycleError> {
    let target_machine = handlers
        .machine_roster
        .active_machine(&machine_id)
        .await
        .map_err(|error| MachineLifecycleError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })?;
    if target_machine.is_none() {
        return Err(MachineLifecycleError::NoSuchMachine {
            operation_id,
            machine_id,
        });
    }
    let accepted = handlers
        .controllers()
        .submit_machine_lifecycle(MachineLifecycleSubmitCommand {
            operation_id: operation_id.clone(),
            machine_id,
            target,
        })
        .await
        .map_err(|error| {
            match super::error_map::unfenced_submit_failure("machine-lifecycle", error) {
                super::error_map::UnfencedSubmitFailure::Unavailable { message } => {
                    MachineLifecycleError::Unavailable {
                        operation_id: operation_id.clone(),
                        message,
                    }
                }
                super::error_map::UnfencedSubmitFailure::DuplicateSequenceMismatch { sequence } => {
                    MachineLifecycleError::DuplicateSequenceMismatch {
                        operation_id: operation_id.clone(),
                        sequence,
                    }
                }
            }
        })?;
    let operation = owned_machine_operation(
        accepted.operation_id.clone(),
        &accepted.machine_id,
        accepted.start_sequence,
    );
    handlers.machine_lifecycle().start(accepted);

    Ok(operation)
}

pub async fn core_replace(
    handlers: &OperationApiHandlers,
    request: CoreReplaceRequest,
) -> Result<AcceptedOperation, CoreReplaceError> {
    let target_machine = handlers
        .machine_roster
        .active_machine(&request.machine_id)
        .await
        .map_err(|error| CoreReplaceError::Unavailable {
            operation_id: request.operation_id.clone(),
            message: error.to_string(),
        })?;
    if target_machine.is_none() {
        return Err(CoreReplaceError::NoSuchMachine {
            operation_id: request.operation_id,
            machine_id: request.machine_id,
        });
    }

    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_core_replace(CoreReplaceSubmitCommand {
            operation_id: request.operation_id,
            machine_id: request.machine_id,
            successor_nats_url: request.successor_nats_url,
        })
        .await
        .map_err(|error| match super::error_map::submit_failure(error) {
            super::error_map::SubmitFailure::InvalidDeployTarget => {
                unreachable!("core replace submit is not deploy target")
            }
            super::error_map::SubmitFailure::ResourceBusy { .. } => {
                unreachable!("core replace submit has no namespace lock")
            }
            super::error_map::SubmitFailure::Unavailable { message } => {
                CoreReplaceError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                }
            }
            super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
                CoreReplaceError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                }
            }
        })?;
    Ok(owned_machine_operation(
        accepted.operation_id,
        &accepted.machine_id,
        accepted.start_sequence,
    ))
}

#[must_use]
fn owned_machine_operation(
    operation_id: OperationId,
    machine_id: &MachineId,
    start_sequence: EventSequence,
) -> AcceptedOperation {
    owned_operation(
        operation_id,
        OperationProgressScope::Machine {
            machine_id: machine_id.clone(),
        },
        start_sequence,
    )
}

async fn machine_add_bootstrap_material(
    controllers: &OperationControllers,
    request: &MachineAddRequest,
) -> Result<MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError> {
    controllers.issue_machine_add_bootstrap_material(&request.operation_id)
}

#[cfg(test)]
#[path = "submit_tests.rs"]
mod tests;
