//! NATS Service API wiring for daemon commands.

use crate::operation_api::{
    OperationApiHandlers, core_replace, core_replace_report, deploy_submit,
    init_first_machine_activate, machine_add, machine_drain, machine_join_redeem,
    machine_join_report, machine_resume, machine_update, namespace_remove, network_repair,
    ops_list, ops_status, ops_watch, service_restart, volume_remove,
};
use crate::service_catalog::{IMPLEMENTED_OPERATION_API_ENDPOINTS, api_endpoint_spec, api_service};
use ployz_core::subjects::OperationApiEndpoint;
use ployz_nats::service_runtime::{
    NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError, RunningNatsService,
    decode_json_request, start_nats_service,
};
use ployz_sdk_types::{
    OperationApiResponse,
    operation_api::{
        CoreReplaceApi, CoreReplaceReportApi, DeploySubmitApi, InitFirstMachineActivateApi,
        LogsTailApi, MachineAddApi, MachineDrainApi, MachineInspectApi, MachineJoinRedeemApi,
        MachineJoinReportApi, MachineListApi, MachineResumeApi, MachineUpdateApi,
        NamespaceRemoveApi, NetworkRepairApi, NetworkResolveApi, OperationApiContract, OpsListApi,
        OpsStatusApi, OpsWatchApi, RuntimeSnapshotApi, ServiceInspectApi, ServiceListApi,
        ServiceRestartApi, VolumeListApi, VolumeRemoveApi,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use std::future::Future;
use std::sync::Arc;

pub async fn start_operation_api_service_with_handlers(
    client: ployz_nats::service_runtime::NatsClient,
    handlers: OperationApiHandlers,
) -> Result<RunningNatsService, ApiServiceError> {
    let spec = api_service();
    let mut runtime = start_nats_service(client, &spec)
        .await
        .map_err(ApiServiceError::Nats)?;
    let handlers = Arc::new(handlers);

    for endpoint in IMPLEMENTED_OPERATION_API_ENDPOINTS.iter().copied() {
        bind_operation_endpoint(&mut runtime, Arc::clone(&handlers), endpoint).await?;
    }

    Ok(runtime)
}

async fn bind_operation_endpoint(
    runtime: &mut RunningNatsService,
    handlers: Arc<OperationApiHandlers>,
    endpoint: OperationApiEndpoint,
) -> Result<(), ApiServiceError> {
    match endpoint {
        OperationApiEndpoint::DeploySubmit => {
            bind_operation_contract::<DeploySubmitApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { deploy_submit(&handlers, request.into()).await },
            )
            .await
        }
        OperationApiEndpoint::ServiceRestart => {
            bind_operation_contract::<ServiceRestartApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { service_restart(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::NamespaceRemove => {
            bind_operation_contract::<NamespaceRemoveApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { namespace_remove(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::VolumeRemove => {
            bind_operation_contract::<VolumeRemoveApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { volume_remove(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::MachineAdd => {
            bind_operation_contract::<MachineAddApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { machine_add(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::MachineUpdate => {
            bind_operation_contract::<MachineUpdateApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { machine_update(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::MachineDrain => {
            bind_operation_contract::<MachineDrainApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { machine_drain(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::MachineResume => {
            bind_operation_contract::<MachineResumeApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { machine_resume(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::CoreReplace => {
            bind_operation_contract::<CoreReplaceApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { core_replace(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::CoreReplaceReport => {
            bind_operation_contract::<CoreReplaceReportApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { core_replace_report(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::InitFirstMachineActivate => bind_operation_contract::<
            InitFirstMachineActivateApi,
            _,
            _,
        >(
            runtime,
            handlers,
            |handlers, request| async move { init_first_machine_activate(&handlers, request).await },
        )
        .await,
        OperationApiEndpoint::MachineList => {
            bind_operation_contract::<MachineListApi, _, _>(
                runtime,
                handlers,
                |handlers, _request| async move { handlers.machine_query().list().await },
            )
            .await
        }
        OperationApiEndpoint::MachineInspect => {
            bind_operation_contract::<MachineInspectApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move {
                    handlers.machine_query().inspect(&request.machine_id).await
                },
            )
            .await
        }
        OperationApiEndpoint::NetworkResolve => {
            bind_operation_contract::<NetworkResolveApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { handlers.network_query().resolve(request).await },
            )
            .await
        }
        OperationApiEndpoint::NetworkRepair => {
            bind_operation_contract::<NetworkRepairApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { network_repair(&handlers, request).await },
            )
            .await
        }
        OperationApiEndpoint::ServiceList => {
            bind_operation_contract::<ServiceListApi, _, _>(
                runtime,
                handlers,
                |handlers, _request| async move { handlers.service_query().list().await },
            )
            .await
        }
        OperationApiEndpoint::VolumeList => {
            bind_operation_contract::<VolumeListApi, _, _>(
                runtime,
                handlers,
                |handlers, _request| async move { handlers.volume_query().list().await },
            )
            .await
        }
        OperationApiEndpoint::ServiceInspect => {
            bind_operation_contract::<ServiceInspectApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move {
                    handlers
                        .service_query()
                        .inspect(&request.namespace_id, &request.service_id)
                        .await
                },
            )
            .await
        }
        OperationApiEndpoint::RuntimeSnapshot => {
            bind_operation_contract::<RuntimeSnapshotApi, _, _>(
                runtime,
                handlers,
                |handlers, _request| async move {
                    handlers.runtime_snapshot_query().snapshot().await
                },
            )
            .await
        }
        OperationApiEndpoint::LogsTail => {
            bind_operation_contract::<LogsTailApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { handlers.logs_query().tail(request).await },
            )
            .await
        }
        OperationApiEndpoint::MachineJoinRedeem => {
            bind_operation_contract::<MachineJoinRedeemApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move {
                    machine_join_redeem(&handlers, request).await
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
        OperationApiEndpoint::OpsList => {
            bind_operation_contract::<OpsListApi, _, _>(
                runtime,
                handlers,
                |handlers, request| async move { ops_list(handlers.controllers(), request).await },
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
) -> Result<(), ApiServiceError>
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
        .map_err(ApiServiceError::Nats)
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiServiceError {
    #[error("machine join template is missing")]
    MissingMachineJoinTemplate,
    #[error("{0}")]
    Nats(NatsServiceRuntimeError),
}

#[cfg(test)]
mod tests {
    use crate::service_catalog::{IMPLEMENTED_OPERATION_API_ENDPOINTS, api_service};

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
