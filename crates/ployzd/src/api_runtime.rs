//! NATS Service API runtime wiring for daemon commands.

use crate::controllers::OperationControllers;
use crate::operation_api::{deploy_submit, ops_status, ops_watch};
use crate::services::{api_endpoint_spec, api_service};
use ployz_core::subjects::{OPERATION_API_ENDPOINTS, OperationApiEndpoint};
use ployz_nats::service_runtime::{
    NatsServiceError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, start_nats_service,
};
use ployz_sdk_types::{
    DeploySubmitRequest, DeploySubmitResponse, OperationApiResponse, OpsStatusRequest,
    OpsStatusResponse, OpsWatchRequest, OpsWatchResponse,
};
use serde::Serialize;
use std::sync::Arc;

pub async fn start_operation_api_service(
    client: ployz_nats::service_runtime::NatsClient,
    controllers: OperationControllers,
) -> Result<RunningNatsService, ApiServiceRuntimeError> {
    let spec = api_service();
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(ApiServiceRuntimeError::Nats)?;
    let controllers = Arc::new(controllers);

    for endpoint in OPERATION_API_ENDPOINTS {
        bind_operation_endpoint(&mut runtime, endpoint, Arc::clone(&controllers)).await?;
    }

    Ok(runtime)
}

async fn bind_operation_endpoint(
    runtime: &mut RunningNatsService,
    endpoint: OperationApiEndpoint,
    controllers: Arc<OperationControllers>,
) -> Result<(), ApiServiceRuntimeError> {
    match endpoint {
        OperationApiEndpoint::DeploySubmit => {
            let spec = api_endpoint_spec(endpoint);
            runtime
                .bind_endpoint(&spec, move |request| {
                    let controllers = Arc::clone(&controllers);
                    async move { handle_deploy_submit(&controllers, request).await }
                })
                .await
                .map_err(ApiServiceRuntimeError::Nats)?;
        }
        OperationApiEndpoint::OpsStatus => {
            let spec = api_endpoint_spec(endpoint);
            runtime
                .bind_endpoint(&spec, move |request| {
                    let controllers = Arc::clone(&controllers);
                    async move { handle_ops_status(&controllers, request).await }
                })
                .await
                .map_err(ApiServiceRuntimeError::Nats)?;
        }
        OperationApiEndpoint::OpsWatch => {
            let spec = api_endpoint_spec(endpoint);
            runtime
                .bind_endpoint(&spec, move |request| {
                    let controllers = Arc::clone(&controllers);
                    async move { handle_ops_watch(&controllers, request).await }
                })
                .await
                .map_err(ApiServiceRuntimeError::Nats)?;
        }
    }

    Ok(())
}

async fn handle_deploy_submit(
    controllers: &OperationControllers,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_request::<DeploySubmitRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match deploy_submit(controllers, request.into()).await {
        Ok(value) => {
            let response: DeploySubmitResponse = OperationApiResponse::Ok { value };
            encode_api_success(response)
        }
        Err(error) => {
            let response: DeploySubmitResponse = OperationApiResponse::DomainError { error };
            encode_api_domain_error(response)
        }
    }
}

async fn handle_ops_status(
    controllers: &OperationControllers,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_request::<OpsStatusRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match ops_status(controllers, request.operation_id).await {
        Ok(value) => {
            let response: OpsStatusResponse = OperationApiResponse::Ok { value };
            encode_api_success(response)
        }
        Err(error) => {
            let response: OpsStatusResponse = OperationApiResponse::DomainError { error };
            encode_api_domain_error(response)
        }
    }
}

async fn handle_ops_watch(
    controllers: &OperationControllers,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_request::<OpsWatchRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match ops_watch(controllers, request).await {
        Ok(value) => {
            let response: OpsWatchResponse = OperationApiResponse::Ok { value };
            encode_api_success(response)
        }
        Err(error) => {
            let response: OpsWatchResponse = OperationApiResponse::DomainError { error };
            encode_api_domain_error(response)
        }
    }
}

fn decode_request<T>(request: &NatsServiceRequest) -> Result<T, NatsServiceResponse>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_slice::<T>(&request.payload).map_err(|error| {
        NatsServiceResponse::transport_error(NatsServiceError::bad_request(error.to_string()))
    })
}

fn encode_api_success<T>(response: T) -> NatsServiceResponse
where
    T: Serialize,
{
    encode_response(response, NatsServiceResponse::ok)
}

fn encode_api_domain_error<T>(response: T) -> NatsServiceResponse
where
    T: Serialize,
{
    encode_response(response, NatsServiceResponse::domain_error)
}

fn encode_response<T>(
    response: T,
    output: impl FnOnce(Vec<u8>) -> NatsServiceResponse,
) -> NatsServiceResponse
where
    T: Serialize,
{
    match serde_json::to_vec(&response) {
        Ok(payload) => output(payload),
        Err(error) => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(error.to_string()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiServiceRuntimeError {
    Nats(NatsServiceRuntimeError),
}

#[cfg(test)]
mod tests {
    use crate::services::api_service;
    use ployz_core::subjects::{API_DEPLOY_SUBMIT, API_OPS_STATUS, API_OPS_WATCH};

    #[test]
    fn operation_api_service_advertises_only_bound_endpoints() {
        assert_eq!(
            api_service()
                .endpoints
                .into_iter()
                .map(|endpoint| endpoint.subject)
                .collect::<Vec<_>>(),
            vec![API_DEPLOY_SUBMIT, API_OPS_STATUS, API_OPS_WATCH]
        );
    }
}
