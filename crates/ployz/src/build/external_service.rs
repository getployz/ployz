//! NATS service surface for an external Build Executor.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use ployz_core::build::{
    BUILD_START_ENDPOINT_TIMEOUT, BuildExecutorCancelRequest, BuildExecutorCancelResponse,
    BuildExecutorIdentity, BuildExecutorReadinessAnswer, BuildExecutorReadinessRequest,
    BuildExecutorStartRequest, BuildExecutorStartResponse,
};
use ployz_nats::service_runtime::{
    EndpointExecutionPolicy, NatsServiceRequest, NatsServiceResponse, RunningNatsService,
    decode_json_request, start_nats_service,
};
use ployz_nats::services::{
    EndpointExecution, NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata,
    ServiceMetadataEntry, ServiceVersion,
};
use ployz_nats::subjects::{
    BUILD_EXECUTOR_SERVICE_NAME, BuildExecutorServiceEndpoint, build_executor_service,
};
use tokio::sync::oneshot;

use super::external_runtime::{ExternalBuildRuntime, executor_error, probe_readiness};
use super::runtime::BuildExecutionError;

#[derive(Clone)]
pub(super) enum CompletionMode {
    Once(Arc<Mutex<Option<oneshot::Sender<()>>>>),
    Watch,
}

impl CompletionMode {
    #[must_use]
    pub fn once(sender: oneshot::Sender<()>) -> Self {
        Self::Once(Arc::new(Mutex::new(Some(sender))))
    }

    fn notify_terminal(&self, response: &NatsServiceResponse) {
        if !matches!(
            response,
            NatsServiceResponse::Ok { .. } | NatsServiceResponse::DomainError { .. }
        ) {
            return;
        }
        let Self::Once(sender) = self else {
            return;
        };
        let Some(sender) = sender.lock().expect("completion mutex poisoned").take() else {
            return;
        };
        let _ = sender.send(());
    }
}

pub(super) async fn start_executor_service(
    client: async_nats::Client,
    identity: BuildExecutorIdentity,
    runtime: ExternalBuildRuntime,
    completion: CompletionMode,
) -> Result<RunningNatsService, BuildExecutionError> {
    let endpoints = executor_endpoints(&identity);
    let [readiness_endpoint, start_endpoint, cancel_endpoint] = &endpoints;
    let spec = NatsServiceSpec::new(
        format!(
            "{BUILD_EXECUTOR_SERVICE_NAME}.{}.{}",
            identity.pool_id.as_str(),
            identity.executor_id.as_str()
        ),
        BUILD_EXECUTOR_SERVICE_NAME,
        ServiceVersion::new(1, 0, 0),
        "External Dockerfile and Railpack build executor",
        ServiceMetadata::from_entries(vec![
            ServiceMetadataEntry::new("pool_id", identity.pool_id.as_str()),
            ServiceMetadataEntry::new("executor_id", identity.executor_id.as_str()),
        ]),
        endpoints.to_vec(),
    );
    let mut service = start_nats_service(client, &spec)
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    let readiness_identity = identity.clone();
    service
        .bind_endpoint(readiness_endpoint, move |request| {
            let identity = readiness_identity.clone();
            async move { handle_readiness(identity, request).await }
        })
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    let start_runtime = runtime.clone();
    service
        .bind_endpoint_with_policy(
            start_endpoint,
            EndpointExecutionPolicy::new(NonZeroUsize::MIN, BUILD_START_ENDPOINT_TIMEOUT),
            move |request| {
                let runtime = start_runtime.clone();
                let completion = completion.clone();
                async move {
                    let response = handle_start(runtime, request).await;
                    completion.notify_terminal(&response);
                    response
                }
            },
        )
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    service
        .bind_endpoint(cancel_endpoint, move |request| {
            let runtime = runtime.clone();
            async move { handle_cancel(runtime, request).await }
        })
        .await
        .map_err(|error| executor_error(error.to_string()))?;
    Ok(service)
}

pub(super) fn executor_endpoints(identity: &BuildExecutorIdentity) -> [NatsServiceEndpointSpec; 3] {
    [
        executor_endpoint(identity, BuildExecutorServiceEndpoint::ReadinessGet),
        executor_endpoint(identity, BuildExecutorServiceEndpoint::BuildStart),
        executor_endpoint(identity, BuildExecutorServiceEndpoint::BuildCancel),
    ]
}

fn executor_endpoint(
    identity: &BuildExecutorIdentity,
    endpoint: BuildExecutorServiceEndpoint,
) -> NatsServiceEndpointSpec {
    let (name, execution) = match endpoint {
        BuildExecutorServiceEndpoint::ReadinessGet => ("readiness.get", EndpointExecution::Query),
        BuildExecutorServiceEndpoint::BuildStart => ("build.start", EndpointExecution::MachineRpc),
        BuildExecutorServiceEndpoint::BuildCancel => {
            ("build.cancel", EndpointExecution::MachineRpc)
        }
    };
    NatsServiceEndpointSpec::new(
        name,
        build_executor_service(&identity.pool_id, &identity.executor_id, endpoint),
        execution,
    )
}

async fn handle_readiness(
    identity: BuildExecutorIdentity,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    match decode_json_request::<BuildExecutorReadinessRequest>(&request) {
        Ok(BuildExecutorReadinessRequest {}) => match probe_readiness().await {
            Ok(readiness) => NatsServiceResponse::json_ok(&BuildExecutorReadinessAnswer {
                identity,
                readiness,
            }),
            Err(error) => NatsServiceResponse::transport_error(
                ployz_nats::service_runtime::NatsServiceError::internal(error.to_string()),
            ),
        },
        Err(response) => response,
    }
}

async fn handle_start(
    runtime: ExternalBuildRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<BuildExecutorStartRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match runtime.start(request).await {
        Ok(ok) => NatsServiceResponse::json_ok(&BuildExecutorStartResponse::Ok(Box::new(ok))),
        Err(error) => {
            NatsServiceResponse::json_domain_error(&BuildExecutorStartResponse::DomainError {
                error,
            })
        }
    }
}

async fn handle_cancel(
    runtime: ExternalBuildRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<BuildExecutorCancelRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match runtime.cancel(request).await {
        Ok(ok) => NatsServiceResponse::json_ok(&BuildExecutorCancelResponse::Ok(ok)),
        Err(error) => {
            NatsServiceResponse::json_domain_error(&BuildExecutorCancelResponse::DomainError {
                error,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_completion_is_a_noop_and_once_is_consumed() {
        let terminal = NatsServiceResponse::ok(Vec::new());
        CompletionMode::Watch.notify_terminal(&terminal);

        let (sender, receiver) = oneshot::channel();
        let once = CompletionMode::once(sender);
        once.notify_terminal(&terminal);
        once.notify_terminal(&terminal);
        assert_eq!(receiver.blocking_recv(), Ok(()));
    }
}
