//! Machine-only HTTP adapters for the three coarse deploy host effects.

use std::time::Duration;

use hyper::{Response, StatusCode};
use ployz_core::corrosion::{Principal, is_preferred_controller};
use ployz_core::{
    DeployInspectOutcome, DeployPrepareOutcome, DeployPrepareRequest, DeployRetireOutcome,
    DeployRetireRequest,
};
use serde::de::DeserializeOwned;

use super::server::{ApiService, BoundedBodyError, HttpBody, json_response, read_bounded_body};

const MAX_EFFECT_REQUEST_BYTES: usize = 1_048_576;
const EFFECT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn inspect(
    service: &ApiService,
    _request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let outcome = match &service.deploy_effects {
        Some(effects) => effects.inspect().await,
        None => DeployInspectOutcome::Failed,
    };
    super::mutations::typed_response(StatusCode::OK, &outcome)
}

pub(super) async fn prepare(
    service: &ApiService,
    principal: &Principal,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let request: DeployPrepareRequest = match decode(request).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(response) = authorize(service, principal).await {
        return response;
    }
    let outcome = match &service.node_workflows {
        Some(workflows) => workflows.prepare(request).await,
        None => DeployPrepareOutcome::Failed,
    };
    super::mutations::typed_response(StatusCode::OK, &outcome)
}

pub(super) async fn retire(
    service: &ApiService,
    principal: &Principal,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let request: DeployRetireRequest = match decode(request).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    if let Err(response) = authorize(service, principal).await {
        return response;
    }
    let outcome = match &service.node_workflows {
        Some(workflows) => workflows.retire(request).await,
        None => DeployRetireOutcome::Failed,
    };
    super::mutations::typed_response(StatusCode::OK, &outcome)
}

async fn authorize(service: &ApiService, principal: &Principal) -> Result<(), Response<HttpBody>> {
    let Principal::Machine { machine_id } = principal else {
        return Err(error_response(StatusCode::NOT_FOUND, "unsupported_route"));
    };
    let current =
        service.controller.current().await.map_err(|_| {
            error_response(StatusCode::SERVICE_UNAVAILABLE, "controller_unavailable")
        })?;
    if current
        .as_ref()
        .is_some_and(|controller| is_preferred_controller(controller, machine_id))
    {
        Ok(())
    } else {
        Err(error_response(StatusCode::CONFLICT, "stale_controller"))
    }
}

async fn decode<Request>(
    request: hyper::Request<hyper::body::Incoming>,
) -> Result<Request, Response<HttpBody>>
where
    Request: DeserializeOwned,
{
    let body = read_bounded_body(
        request.into_body(),
        MAX_EFFECT_REQUEST_BYTES,
        EFFECT_REQUEST_TIMEOUT,
    )
    .await
    .map_err(body_error)?;
    serde_json::from_slice(&body)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "invalid_request"))
}

fn body_error(error: BoundedBodyError) -> Response<HttpBody> {
    match error {
        BoundedBodyError::TooLarge => {
            error_response(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large")
        }
        BoundedBodyError::Deadline => {
            error_response(StatusCode::REQUEST_TIMEOUT, "request_timeout")
        }
        BoundedBodyError::Read => error_response(StatusCode::BAD_REQUEST, "invalid_request"),
    }
}

fn error_response(status: StatusCode, kind: &'static str) -> Response<HttpBody> {
    json_response(status, format!("{{\"kind\":\"{kind}\"}}").into_bytes())
}
