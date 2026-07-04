//! Submit handlers: accept the operation quickly, start the owned worker,
//! and return the operation id + watch subject.

use crate::controllers::{
    DeploySubmitCommand, MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError,
    MachineAddSubmitCommand, MachineLifecycleSubmitCommand, MachineUpdateSubmitCommand,
    OperationControllers,
};
use crate::nats_authorization::MintRequest;
use ployz_core::ids::OperationId;
use ployz_core::ops::EventSequence;
use ployz_core::subjects::op_watch;
use ployz_sdk_types::{
    AcceptedOperation, DeploySubmitError, DeploySubmitRequest, MachineAddAccepted, MachineAddError,
    MachineAddRequest, MachineJoinToken, MachineLifecycleError, MachineLifecycleRequest,
    MachineUpdateError, MachineUpdateRequest,
};

use super::OperationApiHandlers;
use super::error_map::{
    deploy_submit_error_from_submit_error, machine_add_error_from_submit_error,
};

#[must_use]
pub fn owned_operation(
    operation_id: OperationId,
    start_sequence: EventSequence,
) -> AcceptedOperation {
    let watch_subject = op_watch(&operation_id);
    AcceptedOperation {
        operation_id,
        watch_subject,
        start_sequence,
    }
}

impl From<DeploySubmitRequest> for DeploySubmitCommand {
    fn from(value: DeploySubmitRequest) -> Self {
        Self {
            operation_id: mint_deploy_operation_id(),
            idempotency_key: value.idempotency_key,
            target: value.target,
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
    let accepted = handlers
        .controllers
        .submit_deploy(command)
        .await
        .map_err(|error| deploy_submit_error_from_submit_error(operation_id, error))?;
    let operation = owned_operation(accepted.operation_id.clone(), accepted.start_sequence);
    handlers.deploy_runtime.start(accepted);

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
        join_bundle: material.join_bundle,
        join_token: material.join_token,
        raw_join_token: material.raw_join_token,
    };

    let accepted = controllers
        .submit_machine_add(command)
        .await
        .map_err(|error| machine_add_error_from_submit_error(operation_id.clone(), error))?;
    let raw_token = MachineJoinToken::try_new(accepted.raw_join_token.as_str()).map_err(|_| {
        MachineAddError::Unavailable {
            operation_id: operation_id.clone(),
            message: "machine-add accepted raw join token is invalid".to_owned(),
        }
    })?;
    handlers.machine_mint.start(MintRequest {
        operation_id: accepted.operation_id.clone(),
        machine_id: accepted.machine_id.clone(),
        idempotency_key,
    });

    Ok(MachineAddAccepted {
        accepted: owned_operation(accepted.operation_id, accepted.start_sequence),
        machine_id: accepted.machine_id,
        bootstrap_url: material.bootstrap_url,
        join_bundle: accepted.join_bundle,
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
        .core_state
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
        .map_err(|error| match super::error_map::submit_failure(error) {
            super::error_map::SubmitFailure::InvalidDeployTarget => {
                unreachable!("machine update submit is not deploy target")
            }
            super::error_map::SubmitFailure::ResourceBusy { .. } => {
                unreachable!("machine update submit has no namespace lock")
            }
            super::error_map::SubmitFailure::Unavailable { message } => {
                MachineUpdateError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                }
            }
            super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
                MachineUpdateError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                }
            }
        })?;
    let operation = owned_operation(accepted.operation_id.clone(), accepted.start_sequence);
    handlers.machine_update_runtime().start(accepted);

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
        .core_state
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
        .map_err(|error| match super::error_map::submit_failure(error) {
            super::error_map::SubmitFailure::InvalidDeployTarget => {
                unreachable!("machine lifecycle submit is not deploy target")
            }
            super::error_map::SubmitFailure::ResourceBusy { .. } => {
                unreachable!("machine lifecycle submit has no namespace lock")
            }
            super::error_map::SubmitFailure::Unavailable { message } => {
                MachineLifecycleError::Unavailable {
                    operation_id: operation_id.clone(),
                    message,
                }
            }
            super::error_map::SubmitFailure::DuplicateSequenceMismatch { sequence } => {
                MachineLifecycleError::DuplicateSequenceMismatch {
                    operation_id: operation_id.clone(),
                    sequence,
                }
            }
        })?;
    let operation = owned_operation(accepted.operation_id.clone(), accepted.start_sequence);
    handlers.machine_lifecycle_runtime().start(accepted);

    Ok(operation)
}

async fn machine_add_bootstrap_material(
    controllers: &OperationControllers,
    request: &MachineAddRequest,
) -> Result<MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError> {
    controllers.issue_machine_add_bootstrap_material(&request.operation_id)
}
