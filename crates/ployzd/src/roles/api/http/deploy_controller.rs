//! Admission for one ephemeral preferred-controller deploy attempt.

use std::sync::Arc;

use hyper::{Response, StatusCode};
use ployz_core::corrosion::Principal;
use ployz_core::{DeployAccepted, DeployRefusal, DeployRequest};

use super::server::{ApiService, HttpBody, corrosion_unavailable_response, refusal_response};
use super::simple_deploy::{DeployCommand, DeployStartError};

pub(super) async fn handle(
    service: &ApiService,
    principal: Principal,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let Some(deploy) = &service.simple_deploy else {
        return refusal_response(ployz_core::ApiRefusal::UnsupportedRoute);
    };
    let request: DeployRequest = match super::mutations::decode_request(request.into_body()).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let permit = match Arc::clone(&service.controller_lock).try_lock_owned() {
        Ok(permit) => permit,
        Err(_) => return controller_busy(),
    };
    let operation_id = request.deploy_name.clone();
    let namespace_name = request.namespace_name.clone();
    let command = DeployCommand {
        operation_id: operation_id.clone(),
        request,
        initiator: principal,
    };
    let started = match deploy.start(command).await {
        Ok(started) => started,
        Err(DeployStartError::Refused(refusal)) => return deploy_refusal(refusal),
        Err(DeployStartError::Unavailable(error)) => {
            tracing::warn!(%error, "deploy admission failed");
            return corrosion_unavailable_response();
        }
    };
    let deploy = Arc::clone(deploy);
    let task_operation_id = operation_id.clone();
    tokio::spawn(async move {
        let _permit = permit;
        match deploy.run(started).await {
            Ok(outcome) => {
                tracing::info!(operation_id = %task_operation_id, ?outcome, "deploy attempt finished");
            }
            Err(error) => {
                tracing::warn!(operation_id = %task_operation_id, %error, "deploy attempt ended without a coarse terminal row");
            }
        }
    });
    super::mutations::typed_response(
        StatusCode::ACCEPTED,
        &DeployAccepted {
            namespace_name,
            deploy_name: operation_id,
            controller_machine_name: service.local_machine_id.clone(),
        },
    )
}

fn deploy_refusal(refusal: DeployRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        DeployRefusal::NamespaceNotFound { .. } => StatusCode::NOT_FOUND,
        DeployRefusal::DeployNameAlreadyUsed { .. } | DeployRefusal::HostPortConflict { .. } => {
            StatusCode::CONFLICT
        }
    };
    super::mutations::typed_response(status, &refusal)
}

pub(super) fn controller_busy() -> Response<HttpBody> {
    super::server::json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        b"{\"kind\":\"controller_busy\"}".to_vec(),
    )
}
