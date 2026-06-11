//! Submit handlers: accept the operation quickly, start the owned worker,
//! and return the operation id + watch subject.

use crate::controllers::{
    BackupCreateCommand, DeploySubmitCommand, MachineAddBootstrapMaterial,
    MachineAddBootstrapMaterialError, MachineAddSubmitCommand, OperationControllers,
};
use crate::nats_authorization::MintRequest;
use ployz_core::ids::OperationId;
use ployz_core::ops::{EventSequence, OperationOwnerLease};
use ployz_core::roles::FirstNodeGateway;
use ployz_core::subjects::op_watch;
use ployz_sdk_types::{
    AcceptedOperation, BackupCreateError, BackupCreateRequest, BootstrapMaterialFailure,
    DeploySubmitError, DeploySubmitRequest, MachineAddAccepted, MachineAddError, MachineAddRequest,
    MachineAddUnavailableSource, MachineJoinToken,
};

use super::OperationApiHandlers;
use super::error_map::{
    backup_create_error_from_submit_error, bootstrap_material_failure,
    deploy_submit_error_from_submit_error, machine_add_error_from_submit_error,
};

#[must_use]
pub fn owned_operation(
    operation_id: OperationId,
    start_sequence: EventSequence,
    lease: OperationOwnerLease,
) -> AcceptedOperation {
    let watch_subject = op_watch(&operation_id);
    AcceptedOperation {
        operation_id,
        watch_subject,
        start_sequence,
        owner_lease: lease,
    }
}

impl From<DeploySubmitRequest> for DeploySubmitCommand {
    fn from(value: DeploySubmitRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            idempotency_key: value.idempotency_key,
            target: value.target,
        }
    }
}

impl From<BackupCreateRequest> for BackupCreateCommand {
    fn from(value: BackupCreateRequest) -> Self {
        Self {
            operation_id: value.operation_id,
            idempotency_key: value.idempotency_key,
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
    let operation = owned_operation(
        accepted.operation_id.clone(),
        accepted.start_sequence,
        accepted.lease.clone(),
    );
    handlers.deploy_runtime.start(accepted);

    Ok(operation)
}

pub async fn backup_create(
    handlers: &OperationApiHandlers,
    request: BackupCreateRequest,
) -> Result<AcceptedOperation, BackupCreateError> {
    let operation_id = request.operation_id.clone();
    let accepted = handlers
        .controllers
        .submit_backup(request.into())
        .await
        .map_err(|error| backup_create_error_from_submit_error(operation_id, error))?;
    let operation = owned_operation(
        accepted.operation_id.clone(),
        accepted.start_sequence,
        accepted.lease.clone(),
    );
    handlers.backup_runtime.start(accepted);

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
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: bootstrap_material_failure(error),
            },
        })?;
    let command = MachineAddSubmitCommand {
        operation_id: request.operation_id,
        idempotency_key: request.idempotency_key,
        node_id: request.node_id,
        name: request.name,
        gateway: first_node_gateway(request.gateway),
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
            source: MachineAddUnavailableSource::BootstrapMaterial {
                failure: BootstrapMaterialFailure::IssueJoinToken,
            },
        }
    })?;
    handlers.machine_mint.start(MintRequest {
        operation_id: accepted.operation_id.clone(),
        node_id: accepted.node_id.clone(),
        idempotency_key,
    });

    Ok(MachineAddAccepted {
        accepted: owned_operation(
            accepted.operation_id,
            accepted.start_sequence,
            accepted.lease,
        ),
        node_id: accepted.node_id,
        bootstrap_url: material.bootstrap_url,
        join_bundle: accepted.join_bundle,
        join_token: raw_token,
    })
}

async fn machine_add_bootstrap_material(
    controllers: &OperationControllers,
    request: &MachineAddRequest,
) -> Result<MachineAddBootstrapMaterial, MachineAddBootstrapMaterialError> {
    let Some(existing) = controllers
        .submitted_machine_add_bootstrap_material(&request.idempotency_key)
        .await?
    else {
        return controllers.issue_machine_add_bootstrap_material(&request.operation_id);
    };
    Ok(MachineAddBootstrapMaterial {
        raw_join_token: existing.raw_join_token,
        join_token: existing.join_token,
        bootstrap_url: controllers.machine_bootstrap_url().clone(),
        join_bundle: existing.join_bundle,
    })
}

fn first_node_gateway(gateway: ployz_sdk_types::MachineAddGateway) -> FirstNodeGateway {
    match gateway {
        ployz_sdk_types::MachineAddGateway::Install => FirstNodeGateway::Install,
        ployz_sdk_types::MachineAddGateway::Skip => FirstNodeGateway::Skip,
    }
}
