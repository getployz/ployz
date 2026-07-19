//! Deploy admission and submission handlers.

use std::collections::BTreeMap;

use crate::control::intent::ingress_intent::PloyzDnsTargetStore;
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::operations::deploy::validate_deploy_route_admission;
use crate::control::sequencer::{AcceptedDeployExecution, DeploySubmitCommand};
use ployz_core::deploy::DeployReservationId;
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::image::RegistryCredential;
use ployz_core::operation::{FailureMessage, OperationIdempotencyKey};
use ployz_nats::subjects::OperationProgressScope;
use ployz_sdk_types::{
    AcceptedOperation, DeployRequest, DeployReserveError, DeployReserveRequest, DeployReserved,
    DeploySubmitError, DeploySubmitRequest, SystemDeployRequest, SystemDeployTarget,
};

use super::OperationApiHandlers;
use super::error_map::{ordinary_deploy_submit_error, system_deploy_submit_error};
use super::submit::owned_operation;

pub(super) fn normalize_deploy_submit(
    value: DeploySubmitRequest,
) -> Result<DeploySubmitCommand, DeploySubmitError> {
    let operation_id = mint_deploy_operation_id();
    let DeploySubmitRequest {
        idempotency_key,
        reservation_id,
        target,
        registry_credentials,
    } = value;
    ployz_core::namespace::ensure_ordinary_namespace(&target.namespace_id).map_err(|error| {
        DeploySubmitError::ReservedSystemNamespace {
            operation_id: operation_id.clone(),
            namespace_id: error.namespace_id,
        }
    })?;
    normalize_deploy_command(
        operation_id,
        idempotency_key,
        reservation_id,
        target,
        registry_credentials,
    )
}

pub(super) fn normalize_system_deploy(
    value: SystemDeployRequest,
) -> Result<DeploySubmitCommand, DeploySubmitError> {
    let operation_id = mint_deploy_operation_id();
    let SystemDeployRequest {
        idempotency_key,
        reservation_id,
        target: SystemDeployTarget { origin, services },
        registry_credentials,
    } = value;
    normalize_deploy_command(
        operation_id,
        idempotency_key,
        reservation_id,
        DeployRequest {
            namespace_id: ployz_core::namespace::reserved_system_namespace(),
            origin,
            volumes: BTreeMap::new(),
            services,
        },
        registry_credentials,
    )
}

fn normalize_deploy_command(
    operation_id: OperationId,
    idempotency_key: OperationIdempotencyKey,
    reservation_id: DeployReservationId,
    target: DeployRequest,
    registry_credentials: BTreeMap<ServiceId, RegistryCredential>,
) -> Result<DeploySubmitCommand, DeploySubmitError> {
    let planning_target = ployz_core::deploy::DeployPlanningTarget::try_from_deploy(&target)
        .map_err(|error| DeploySubmitError::InvalidTarget {
            operation_id: operation_id.clone(),
            message: FailureMessage::try_new(error.to_string())
                .expect("deploy target validation error is non-empty"),
        })?;
    planning_target
        .validate_registry_credential_service_ids(registry_credentials.keys())
        .map_err(|error| DeploySubmitError::InvalidTarget {
            operation_id: operation_id.clone(),
            message: FailureMessage::try_new(error.to_string())
                .expect("registry credential target validation error is non-empty"),
        })?;
    Ok(DeploySubmitCommand {
        operation_id,
        idempotency_key,
        reservation_id,
        target,
        registry_credentials,
    })
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

pub async fn deploy_submit(
    handlers: &OperationApiHandlers,
    request: DeploySubmitRequest,
) -> Result<AcceptedOperation, DeploySubmitError> {
    let command = normalize_deploy_submit(request)?;
    let operation_id = command.operation_id.clone();
    validate_deploy_command(handlers, &command).await?;
    let accepted_execution = handlers
        .controllers
        .submit_deploy(command)
        .await
        .map_err(|error| ordinary_deploy_submit_error(operation_id, error))?;
    finish_deploy(handlers, accepted_execution).await
}

pub async fn system_deploy(
    handlers: &OperationApiHandlers,
    request: SystemDeployRequest,
) -> Result<AcceptedOperation, DeploySubmitError> {
    let command = normalize_system_deploy(request)?;
    let operation_id = command.operation_id.clone();
    validate_deploy_command(handlers, &command).await?;
    let accepted_execution = handlers
        .controllers
        .submit_system_deploy(command)
        .await
        .map_err(|error| system_deploy_submit_error(operation_id, error))?;
    finish_deploy(handlers, accepted_execution).await
}

async fn validate_deploy_command(
    handlers: &OperationApiHandlers,
    command: &DeploySubmitCommand,
) -> Result<(), DeploySubmitError> {
    validate_deploy_route_admission(
        &command.target,
        &handlers.ingress_intent,
        &PloyzDnsTargetStore::new(handlers.core_store.clone()),
        &NamespaceIntentStore::new(handlers.core_store.clone()),
    )
    .await
    .map_err(|error| DeploySubmitError::InvalidTarget {
        operation_id: command.operation_id.clone(),
        message: FailureMessage::try_new(error.to_string())
            .expect("route admission validation error is non-empty"),
    })
}

async fn finish_deploy(
    handlers: &OperationApiHandlers,
    accepted_execution: AcceptedDeployExecution,
) -> Result<AcceptedOperation, DeploySubmitError> {
    let accepted = &accepted_execution.submission;
    let scope = OperationProgressScope::Namespace {
        namespace_id: accepted.target.namespace_id.clone(),
    };
    let operation = owned_operation(
        accepted.operation_id.clone(),
        scope,
        accepted.start_sequence,
    );
    handlers.deploy_driver.start(accepted_execution).await;

    Ok(operation)
}

fn mint_deploy_operation_id() -> OperationId {
    OperationId::try_new(format!("op_deploy_{}", nuid::next()))
        .expect("generated deploy operation id uses subject-token characters")
}
