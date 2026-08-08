//! Admission for one ephemeral preferred-controller deploy attempt.

use std::sync::Arc;

use hyper::{Response, StatusCode};
use ployz_core::corrosion::{ControllerAppointmentId, Principal};
use ployz_core::ids::OperationRowId;
use ployz_core::{DeployAccepted, DeployRefusal, DeployRequest};

use super::server::{ApiService, HttpBody, corrosion_unavailable_response, refusal_response};
use super::simple_deploy::DeployCommand;

pub(super) async fn handle(
    service: &ApiService,
    principal: Principal,
    appointment_id: ControllerAppointmentId,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let (Some(deploy), Some(store)) = (&service.simple_deploy, &service.simple_deploy_store) else {
        return refusal_response(ployz_core::ApiRefusal::UnsupportedRoute);
    };
    let request: DeployRequest = match super::mutations::decode_request(request.into_body()).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    match store.preflight_namespace(&request).await {
        Ok(Ok(())) => {}
        Ok(Err(refusal)) => return deploy_refusal(refusal),
        Err(error) => {
            tracing::warn!(%error, "deploy namespace preflight failed");
            return corrosion_unavailable_response();
        }
    }
    let permit = match Arc::clone(&service.controller_lock).try_lock_owned() {
        Ok(permit) => permit,
        Err(_) => return controller_busy(),
    };
    let operation_id = OperationRowId::generate();
    let command = DeployCommand {
        operation_id: operation_id.clone(),
        request,
        initiator: principal,
        appointment_id,
    };
    let deploy = Arc::clone(deploy);
    tokio::spawn(async move {
        let _permit = permit;
        match deploy.run(&command).await {
            Ok(outcome) => {
                tracing::info!(operation_id = %command.operation_id, ?outcome, "deploy attempt finished");
            }
            Err(error) => {
                tracing::warn!(operation_id = %command.operation_id, %error, "deploy attempt ended before a coarse terminal row");
            }
        }
    });
    super::mutations::typed_response(
        StatusCode::ACCEPTED,
        &DeployAccepted {
            operation_id,
            driver_machine_id: service.local_machine_id.clone(),
        },
    )
}

fn deploy_refusal(refusal: DeployRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        DeployRefusal::NamespaceNotFound { .. } => StatusCode::NOT_FOUND,
        DeployRefusal::NamespaceAmbiguous { .. } => StatusCode::CONFLICT,
    };
    super::mutations::typed_response(status, &refusal)
}

pub(super) fn controller_busy() -> Response<HttpBody> {
    super::server::json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        b"{\"kind\":\"controller_busy\"}".to_vec(),
    )
}
