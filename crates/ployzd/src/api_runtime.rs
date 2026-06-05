//! NATS Service API runtime wiring for daemon commands.

use crate::controllers::OperationControllers;
use crate::operation_api::deploy_submit;
use crate::services::{
    API_SERVICE_DESCRIPTION, API_SERVICE_ID, API_SERVICE_NAME, SERVICE_VERSION,
    api_deploy_submit_endpoint,
};
use ployz_nats::service_runtime::{
    NatsServiceHandlerError, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, start_nats_service,
};
use ployz_nats::services::{NatsServiceSpec, ServiceMetadata};
use ployz_sdk_types::{DeploySubmitRequest, DeploySubmitResponse, OperationApiResponse};
use std::sync::Arc;

pub async fn start_deploy_submit_service(
    client: ployz_nats::service_runtime::NatsClient,
    controllers: OperationControllers,
) -> Result<RunningNatsService, ApiServiceRuntimeError> {
    let spec = deploy_submit_service();
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(ApiServiceRuntimeError::Nats)?;
    let deploy_submit_endpoint = api_deploy_submit_endpoint();

    let controllers = Arc::new(controllers);
    runtime
        .bind_endpoint(&deploy_submit_endpoint, move |request| {
            let controllers = Arc::clone(&controllers);
            async move { handle_deploy_submit(&controllers, request).await }
        })
        .await
        .map_err(ApiServiceRuntimeError::Nats)?;

    Ok(runtime)
}

#[must_use]
pub fn deploy_submit_service() -> NatsServiceSpec {
    NatsServiceSpec::new(
        API_SERVICE_ID,
        API_SERVICE_NAME,
        SERVICE_VERSION,
        API_SERVICE_DESCRIPTION,
        ServiceMetadata::empty(),
        vec![api_deploy_submit_endpoint()],
    )
}

async fn handle_deploy_submit(
    controllers: &OperationControllers,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match serde_json::from_slice::<DeploySubmitRequest>(&request.payload) {
        Ok(request) => request,
        Err(error) => {
            return NatsServiceResponse::transport_error(NatsServiceHandlerError::bad_request(
                error.to_string(),
            ));
        }
    };
    let response = match deploy_submit(controllers, request.into()).await {
        Ok(value) => {
            let response: DeploySubmitResponse = OperationApiResponse::Ok { value };
            encode_api_success(response)
        }
        Err(error) => {
            let response: DeploySubmitResponse = OperationApiResponse::DomainError { error };
            encode_api_domain_error(response)
        }
    };

    match response {
        Ok(response) => response,
        Err(error) => NatsServiceResponse::transport_error(NatsServiceHandlerError::internal(
            error.to_string(),
        )),
    }
}

fn encode_api_success(
    response: DeploySubmitResponse,
) -> Result<NatsServiceResponse, serde_json::Error> {
    serde_json::to_vec(&response).map(NatsServiceResponse::ok)
}

fn encode_api_domain_error(
    response: DeploySubmitResponse,
) -> Result<NatsServiceResponse, serde_json::Error> {
    serde_json::to_vec(&response).map(NatsServiceResponse::domain_error)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiServiceRuntimeError {
    Nats(NatsServiceRuntimeError),
}
