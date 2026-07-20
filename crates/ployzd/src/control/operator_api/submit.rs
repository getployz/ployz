//! Submit handlers: accept the operation quickly, start the owned worker,
//! and return the operation id + watch subject.

use crate::control::authorization::MintRequest;
use crate::control::sequencer::{
    BuildSubmitCommand, CoreReplaceSubmitCommand, CredentialGrantSubmitCommand,
    IngressConfigureSubmitCommand, IngressConfigureSubmitError, MachineAddBootstrapMaterial,
    MachineAddBootstrapMaterialError, MachineAddSubmitCommand, MachineLifecycleSubmitCommand,
    MachineUpdateSubmitCommand, NamespaceRemoveSubmitCommand, NetworkRepairSubmitCommand,
    OperationControllers, ServiceRestartSubmitCommand, VolumeCreateSubmitCommand,
    VolumeRemoveSubmitCommand,
};
use ployz_core::build::{BuildPlatformExecutorAssignment, BuildTarget};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::operation::{CredentialGrantAction, EventSequence, VolumeCreateRequest};
use ployz_nats::subjects::{OperationProgressScope, operation_progress_watch};
use ployz_sdk_types::{
    AcceptedOperation, BuildCancelError, BuildCancelRequest, BuildSubmitError, BuildSubmitRequest,
    BuildTargetCapabilities, BuildTargetCapabilitiesError, CoreReplaceError, CoreReplaceRequest,
    CredentialAddError, CredentialAddRequest, CredentialRemoveError, CredentialRemoveRequest,
    IngressConfigureError, IngressConfigureRequest, MachineAddAccepted, MachineAddError,
    MachineAddRequest, MachineJoinToken, MachineLifecycleError, MachineLifecycleRequest,
    MachineUpdateError, MachineUpdateRequest, NamespaceRemoveError, NamespaceRemoveRequest,
    NetworkRepairError, NetworkRepairRequest, ServiceRestartError, ServiceRestartRequest,
    VolumeCreateError, VolumeRemoveError, VolumeRemoveRequest,
};

pub async fn build_target_capabilities(
    handlers: &OperationApiHandlers,
) -> Result<BuildTargetCapabilities, BuildTargetCapabilitiesError> {
    handlers
        .build_driver()
        .target_capabilities()
        .await
        .map_err(|error| BuildTargetCapabilitiesError::Unavailable {
            message: error.to_string(),
        })
}

pub async fn build_submit(
    handlers: &OperationApiHandlers,
    request: BuildSubmitRequest,
) -> Result<AcceptedOperation, BuildSubmitError> {
    let operation_id = request.operation_id.clone();
    let planned_assignments = external_preflight_for_new_operation(handlers, &request).await?;
    let mut accepted = handlers.controllers().submit_build(BuildSubmitCommand {
        operation_id: request.operation_id,
        target: request.target,
        source: request.source,
        adapter: request.adapter,
        platforms: request.platforms,
    }).await.map_err(|error| match error {
        crate::control::sequencer::SubmitCommandError::Submit(
            crate::control::operation_evidence::SubmitOperationError::DuplicateSequenceMismatch { .. }
        ) => BuildSubmitError::OperationConflict { operation_id: operation_id.clone() },
        error @ (crate::control::sequencer::SubmitCommandError::Clock { .. }
        | crate::control::sequencer::SubmitCommandError::NamespaceBusy { .. }
        | crate::control::sequencer::SubmitCommandError::MachineSubstrateBusy { .. }
        | crate::control::sequencer::SubmitCommandError::IngressBusy { .. }
        | crate::control::sequencer::SubmitCommandError::ReservationNotFound { .. }
        | crate::control::sequencer::SubmitCommandError::ReservationExpired { .. }
        | crate::control::sequencer::SubmitCommandError::Submit(_)) => BuildSubmitError::Unavailable {
            operation_id: operation_id.clone(),
            message: format!("{error:?}"),
        },
    })?;
    if accepted.submission.should_start_execution {
        accepted.planned_assignments = planned_assignments;
    }
    let operation = owned_operation(
        accepted.submission.operation_id.clone(),
        OperationProgressScope::Cluster,
        accepted.submission.start_sequence,
    );
    handlers.build_driver().start(accepted).await;
    Ok(operation)
}

async fn external_preflight_for_new_operation(
    handlers: &OperationApiHandlers,
    request: &BuildSubmitRequest,
) -> Result<Vec<BuildPlatformExecutorAssignment>, BuildSubmitError> {
    let BuildTarget::External { pool_id } = &request.target else {
        return Ok(Vec::new());
    };
    let operation_id = request.operation_id.clone();
    let existing = handlers
        .controllers()
        .repository()
        .get(&operation_id)
        .await
        .map_err(|error| BuildSubmitError::Unavailable {
            operation_id: operation_id.clone(),
            message: error.to_string(),
        })?;
    if existing.is_some() {
        return Ok(Vec::new());
    }
    handlers
        .build_driver()
        .preflight_external(pool_id, &request.platforms, &request.adapter)
        .await
        .map_err(|failure| {
            match failure {
            crate::control::operations::build::ExternalBuildAdmissionError::NoCapableExecutor {
                pool_id,
                platform,
            } => BuildSubmitError::NoCapableExternalExecutor {
                operation_id,
                pool_id,
                platform,
            },
            crate::control::operations::build::ExternalBuildAdmissionError::NoReachableImageSeed {
                pool_id,
            } => BuildSubmitError::NoReachableImageSeed {
                operation_id,
                pool_id,
            },
            crate::control::operations::build::ExternalBuildAdmissionError::Unavailable {
                message,
            } => BuildSubmitError::Unavailable {
                operation_id,
                message,
            },
        }
        })
}

pub async fn build_cancel(
    handlers: &OperationApiHandlers,
    request: BuildCancelRequest,
) -> Result<AcceptedOperation, BuildCancelError> {
    let operation_id = request.operation_id;
    let disposition = handlers
        .build_driver()
        .cancel(&operation_id, request.reason)
        .await
        .map_err(|message| BuildCancelError::Unavailable {
            operation_id: operation_id.clone(),
            message,
        })?;
    let watch_start_sequence = match disposition {
        crate::control::operations::build::BuildCancelDisposition::NoSuchOperation => {
            return Err(BuildCancelError::NoSuchOperation { operation_id });
        }
        crate::control::operations::build::BuildCancelDisposition::AlreadyTerminal => {
            return Err(BuildCancelError::AlreadyTerminal { operation_id });
        }
        crate::control::operations::build::BuildCancelDisposition::Accepted {
            watch_start_sequence,
        } => watch_start_sequence,
    };
    Ok(owned_operation(
        operation_id,
        OperationProgressScope::Cluster,
        watch_start_sequence,
    ))
}

use super::OperationApiHandlers;
#[cfg(test)]
use super::deploy::{normalize_deploy_submit, normalize_system_deploy};
use super::error_map::machine_add_error_from_submit_error;

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
    handlers.credential_grant().start(accepted).await;
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
    handlers.credential_grant().start(accepted).await;
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
    handlers.ingress_configure().start(accepted).await;
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
                crate::control::sequencer::SubmitCommandError::Submit(error),
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
    error: crate::control::sequencer::SubmitCommandError,
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
        super::error_map::SubmitFailure::ResourceBusy { .. } => {
            unreachable!("credential grants have no deploy target or namespace fence")
        }
    }
}

fn credential_remove_submit_error(
    operation_id: OperationId,
    error: crate::control::sequencer::SubmitCommandError,
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
        super::error_map::SubmitFailure::ResourceBusy { .. } => {
            unreachable!("credential grants have no deploy target or namespace fence")
        }
    }
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
        .map_err(
            |error| match super::error_map::ordinary_namespace_submit_failure(error) {
                super::error_map::OrdinaryNamespaceSubmitFailure::ReservedSystemNamespace {
                    namespace_id,
                } => ServiceRestartError::ReservedSystemNamespace {
                    operation_id: operation_id.clone(),
                    namespace_id,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::ResourceBusy {
                        namespace_id,
                        owner,
                    },
                ) => ServiceRestartError::ResourceBusy {
                    operation_id: operation_id.clone(),
                    namespace_id,
                    owner_operation_id: owner,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::Unavailable { message },
                ) => ServiceRestartError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence },
                ) => ServiceRestartError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                },
            },
        )?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Namespace {
            namespace_id: accepted.namespace_id.clone(),
        },
        accepted.start_sequence,
    );
    handlers.service_restart().start(accepted).await;
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
    handlers.network_repair().start(accepted).await;
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
        .map_err(
            |error| match super::error_map::ordinary_namespace_submit_failure(error) {
                super::error_map::OrdinaryNamespaceSubmitFailure::ReservedSystemNamespace {
                    namespace_id,
                } => NamespaceRemoveError::ReservedSystemNamespace {
                    operation_id: operation_id.clone(),
                    namespace_id,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::ResourceBusy {
                        namespace_id,
                        owner,
                    },
                ) => NamespaceRemoveError::ResourceBusy {
                    operation_id: operation_id.clone(),
                    namespace_id,
                    owner_operation_id: owner,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::Unavailable { message },
                ) => NamespaceRemoveError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence },
                ) => NamespaceRemoveError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                },
            },
        )?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Namespace {
            namespace_id: accepted.namespace_id.clone(),
        },
        accepted.start_sequence,
    );
    handlers.namespace_remove().start(accepted).await;
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
        .map_err(
            |error| match super::error_map::ordinary_namespace_submit_failure(error) {
                super::error_map::OrdinaryNamespaceSubmitFailure::ReservedSystemNamespace {
                    namespace_id,
                } => VolumeRemoveError::ReservedSystemNamespace {
                    operation_id: operation_id.clone(),
                    namespace_id,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::ResourceBusy {
                        namespace_id,
                        owner,
                    },
                ) => VolumeRemoveError::ResourceBusy {
                    operation_id: operation_id.clone(),
                    namespace_id,
                    owner_operation_id: owner,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::Unavailable { message },
                ) => VolumeRemoveError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence },
                ) => VolumeRemoveError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                },
            },
        )?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        OperationProgressScope::Namespace {
            namespace_id: accepted.namespace_id.clone(),
        },
        accepted.start_sequence,
    );
    handlers.volume_remove().start(accepted).await;
    Ok(operation)
}

/// Accepts a volume-create request and starts its owned operation work.
pub async fn submit_volume_create(
    handlers: &OperationApiHandlers,
    request: VolumeCreateRequest,
) -> Result<AcceptedOperation, VolumeCreateError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers()
        .submit_volume_create(VolumeCreateSubmitCommand { request })
        .await
        .map_err(
            |error| match super::error_map::ordinary_namespace_submit_failure(error) {
                super::error_map::OrdinaryNamespaceSubmitFailure::ReservedSystemNamespace {
                    namespace_id,
                } => VolumeCreateError::ReservedSystemNamespace {
                    operation_id: operation_id.clone(),
                    namespace_id,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::ResourceBusy {
                        namespace_id,
                        owner,
                    },
                ) => VolumeCreateError::ResourceBusy {
                    operation_id: operation_id.clone(),
                    namespace_id,
                    owner_operation_id: owner,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::Unavailable { message },
                ) => VolumeCreateError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                },
                super::error_map::OrdinaryNamespaceSubmitFailure::Submit(
                    super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence },
                ) => VolumeCreateError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                },
            },
        )?;
    let operation = owned_operation(
        accepted.request.operation_id.clone(),
        OperationProgressScope::Namespace {
            namespace_id: accepted.request.namespace_id.clone(),
        },
        accepted.start_sequence,
    );
    handlers.volume_create().start(accepted).await;
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
        endpoint_subnet: crate::control::sequencer::MachineAddEndpointSubnet::Allocate,
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
    handlers
        .machine_mint
        .start(MintRequest {
            operation_id: accepted.operation_id.clone(),
            machine_id: accepted.identity.machine_id.clone(),
            idempotency_key,
        })
        .await;
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
            let error = match error {
                crate::control::sequencer::SubmitCommandError::MachineSubstrateBusy {
                    machine_id,
                    owner,
                } => {
                    return MachineUpdateError::MachineSubstrateBusy {
                        operation_id: operation_id.clone(),
                        machine_id,
                        owner_operation_id: owner,
                    };
                }
                error @ crate::control::sequencer::SubmitCommandError::Clock { .. }
                | error @ crate::control::sequencer::SubmitCommandError::NamespaceBusy { .. }
                | error @ crate::control::sequencer::SubmitCommandError::IngressBusy { .. }
                | error @ crate::control::sequencer::SubmitCommandError::ReservationNotFound {
                    ..
                }
                | error @ crate::control::sequencer::SubmitCommandError::ReservationExpired {
                    ..
                }
                | error @ crate::control::sequencer::SubmitCommandError::Submit(_) => error,
            };
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
    handlers.machine_update().start(accepted).await;

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
        ployz_core::machine::MachineLifecycle::Draining,
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
        ployz_core::machine::MachineLifecycle::Active,
    )
    .await
}

async fn machine_lifecycle(
    handlers: &OperationApiHandlers,
    operation_id: ployz_core::ids::OperationId,
    machine_id: ployz_core::ids::MachineId,
    target: ployz_core::machine::MachineLifecycle,
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
    handlers.machine_lifecycle().start(accepted).await;

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
pub(super) fn owned_machine_operation(
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
