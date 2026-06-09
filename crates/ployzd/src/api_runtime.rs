//! NATS Service API runtime wiring for daemon commands.

use crate::controllers::OperationControllers;
use crate::operation_api::{
    OperationApiHandlers, backup_create, deploy_submit, logs_tail, machine_add, machine_inspect,
    machine_join_redeem, machine_join_report, machine_list, ops_status, ops_watch, service_inspect,
    service_list,
};
use crate::services::{IMPLEMENTED_OPERATION_API_ENDPOINTS, api_endpoint_spec, api_service};
use ployz_core::subjects::OperationApiEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError, RunningNatsService,
    decode_json_request, start_nats_service,
};
use ployz_sdk_types::{
    OperationApiResponse,
    operation_api::{
        BackupCreateApi, DeploySubmitApi, LogsTailApi, MachineAddApi, MachineInspectApi,
        MachineJoinRedeemApi, MachineJoinReportApi, MachineListApi, OperationApiContract,
        OpsStatusApi, OpsWatchApi, ServiceInspectApi, ServiceListApi,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use std::future::Future;
use std::sync::Arc;

pub async fn start_operation_api_service(
    client: ployz_nats::service_runtime::NatsClient,
    controllers: OperationControllers,
) -> Result<RunningNatsService, ApiServiceRuntimeError> {
    start_operation_api_service_with_handlers(
        client,
        OperationApiHandlers::accept_only(controllers),
    )
    .await
}

pub async fn start_operation_api_service_with_handlers(
    client: ployz_nats::service_runtime::NatsClient,
    handlers: OperationApiHandlers,
) -> Result<RunningNatsService, ApiServiceRuntimeError> {
    if !handlers.controllers().has_machine_join_template() {
        return Err(ApiServiceRuntimeError::MissingMachineJoinTemplate);
    }
    let spec = api_service();
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(ApiServiceRuntimeError::Nats)?;
    let handlers = Arc::new(handlers);

    for endpoint in IMPLEMENTED_OPERATION_API_ENDPOINTS {
        bind_operation_endpoint(&mut runtime, Arc::clone(&handlers), endpoint).await?;
    }

    Ok(runtime)
}

async fn bind_operation_endpoint(
    runtime: &mut RunningNatsService,
    handlers: Arc<OperationApiHandlers>,
    endpoint: OperationApiEndpoint,
) -> Result<(), ApiServiceRuntimeError> {
    match endpoint {
        OperationApiEndpoint::DeploySubmit => {
            bind_operation_contract::<DeploySubmitApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { deploy_submit(&handlers, request.into()).await },
            )
            .await
        }
        OperationApiEndpoint::MachineAdd => bind_operation_contract::<MachineAddApi, _, _>(
            runtime,
            handlers,
            |handlers, request| async move { machine_add(handlers.controllers(), request).await },
        )
        .await,
        OperationApiEndpoint::MachineList => {
            bind_operation_contract::<MachineListApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { machine_list(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::MachineInspect => {
            bind_operation_contract::<MachineInspectApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { machine_inspect(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::ServiceList => {
            bind_operation_contract::<ServiceListApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { service_list(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::ServiceInspect => {
            bind_operation_contract::<ServiceInspectApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { service_inspect(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::LogsTail => {
            bind_operation_contract::<LogsTailApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { logs_tail(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::BackupCreate => {
            bind_operation_contract::<BackupCreateApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { backup_create(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::MachineJoinRedeem => {
            bind_operation_contract::<MachineJoinRedeemApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move {
                    machine_join_redeem(handlers.controllers(), request).await
                },
            )
            .await
        }
        OperationApiEndpoint::MachineJoinReport => {
            bind_operation_contract::<MachineJoinReportApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { machine_join_report(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::OpsStatus => {
            bind_operation_contract::<OpsStatusApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move {
                    ops_status(handlers.controllers(), request.operation_id).await
                },
            )
            .await
        }
        OperationApiEndpoint::OpsWatch => {
            bind_operation_contract::<OpsWatchApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { ops_watch(handlers.controllers(), request).await },
            )
            .await
        }
    }
}

async fn bind_operation_contract<C, H, F>(
    runtime: &mut RunningNatsService,
    handlers: Arc<OperationApiHandlers>,
    handler: H,
) -> Result<(), ApiServiceRuntimeError>
where
    C: OperationApiContract + 'static,
    C::Request: DeserializeOwned + 'static,
    C::Success: Serialize + 'static,
    C::Error: Serialize + 'static,
    H: Fn(Arc<OperationApiHandlers>, C::Request) -> F + Send + Sync + 'static,
    F: Future<Output = Result<C::Success, C::Error>> + Send + 'static,
{
    let spec = api_endpoint_spec(C::ENDPOINT);
    let handler = Arc::new(handler);
    runtime
        .bind_endpoint(&spec, move |request| {
            let handlers = Arc::clone(&handlers);
            let handler = Arc::clone(&handler);
            operation_api_response::<C, _, _>(request, {
                move |request| handler(handlers, request)
            })
        })
        .await
        .map_err(ApiServiceRuntimeError::Nats)
}

async fn operation_api_response<C, H, Fut>(
    request: NatsServiceRequest,
    handler: H,
) -> NatsServiceResponse
where
    C: OperationApiContract,
    C::Request: DeserializeOwned,
    C::Success: Serialize,
    C::Error: Serialize,
    H: FnOnce(C::Request) -> Fut,
    Fut: Future<Output = Result<C::Success, C::Error>>,
{
    let request = match decode_json_request::<C::Request>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    match handler(request).await {
        Ok(value) => {
            let response: OperationApiResponse<C::Success, C::Error> =
                OperationApiResponse::Ok { value };
            NatsServiceResponse::json_ok(&response)
        }
        Err(error) => {
            let response: OperationApiResponse<C::Success, C::Error> =
                OperationApiResponse::DomainError { error };
            NatsServiceResponse::json_domain_error(&response)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiServiceRuntimeError {
    MissingMachineJoinTemplate,
    Nats(NatsServiceRuntimeError),
}

#[cfg(test)]
mod tests {
    use crate::services::{IMPLEMENTED_OPERATION_API_ENDPOINTS, api_service};

    #[test]
    fn operation_api_service_advertises_only_bound_endpoints() {
        assert_eq!(
            api_service()
                .endpoints
                .into_iter()
                .map(|endpoint| endpoint.subject)
                .collect::<Vec<_>>(),
            IMPLEMENTED_OPERATION_API_ENDPOINTS
                .iter()
                .map(|endpoint| endpoint.subject().to_owned())
                .collect::<Vec<_>>()
        );
    }
}
