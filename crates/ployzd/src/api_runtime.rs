//! NATS Service API runtime wiring for daemon commands.

use crate::controllers::OperationControllers;
use crate::operation_api::{deploy_submit, ops_status, ops_watch};
use crate::services::{api_endpoint_spec, api_service};
use ployz_core::subjects::{OPERATION_API_ENDPOINTS, OperationApiEndpoint};
use ployz_nats::service_runtime::{
    NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError, RunningNatsService,
    decode_json_request, start_nats_service,
};
use ployz_sdk_types::{
    DeploySubmitRequest, OperationApiResponse, OpsStatusRequest, OpsWatchRequest,
};
use serde::{Serialize, de::DeserializeOwned};
use std::future::Future;
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
    operation_api_response::<DeploySubmitRequest, _, _, _>(request, |request| async move {
        deploy_submit(controllers, request.into()).await
    })
    .await
}

async fn handle_ops_status(
    controllers: &OperationControllers,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    operation_api_response::<OpsStatusRequest, _, _, _>(request, |request| async move {
        ops_status(controllers, request.operation_id).await
    })
    .await
}

async fn handle_ops_watch(
    controllers: &OperationControllers,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    operation_api_response::<OpsWatchRequest, _, _, _>(request, |request| async move {
        ops_watch(controllers, request).await
    })
    .await
}

async fn operation_api_response<Request, Value, Error, Fut>(
    request: NatsServiceRequest,
    handler: impl FnOnce(Request) -> Fut,
) -> NatsServiceResponse
where
    Request: DeserializeOwned,
    Value: Serialize,
    Error: Serialize,
    Fut: Future<Output = Result<Value, Error>>,
{
    let request = match decode_json_request::<Request>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match handler(request).await {
        Ok(value) => {
            let response: OperationApiResponse<Value, Error> = OperationApiResponse::Ok { value };
            NatsServiceResponse::json_ok(&response)
        }
        Err(error) => {
            let response: OperationApiResponse<Value, Error> =
                OperationApiResponse::DomainError { error };
            NatsServiceResponse::json_domain_error(&response)
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
