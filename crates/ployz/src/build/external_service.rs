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

    fn notify_terminal(&self, completes_once: bool) {
        if !completes_once {
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
    authority_deadline: tokio::time::Instant,
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
    let readiness_runtime = runtime.clone();
    service
        .bind_endpoint_with_policy(
            readiness_endpoint,
            EndpointExecutionPolicy::default().with_authority_deadline(authority_deadline),
            move |request| {
                let identity = readiness_identity.clone();
                let runtime = readiness_runtime.clone();
                async move { handle_readiness(identity, runtime, request).await }
            },
        )
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    let start_runtime = runtime.clone();
    service
        .bind_endpoint_with_policy(
            start_endpoint,
            EndpointExecutionPolicy::new(NonZeroUsize::MIN, BUILD_START_ENDPOINT_TIMEOUT)
                .with_authority_deadline(authority_deadline),
            move |request| {
                let runtime = start_runtime.clone();
                let completion = completion.clone();
                async move {
                    let handled = handle_start(runtime, request).await;
                    completion.notify_terminal(handled.completes_once);
                    handled.response
                }
            },
        )
        .await
        .map_err(|error| executor_error(error.to_string()))?;

    service
        .bind_endpoint_with_policy(
            cancel_endpoint,
            EndpointExecutionPolicy::default().with_authority_deadline(authority_deadline),
            move |request| {
                let runtime = runtime.clone();
                async move { handle_cancel(runtime, request).await }
            },
        )
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
    runtime: ExternalBuildRuntime,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    if !runtime.readiness_authorized_before_expiry().await {
        return std::future::pending().await;
    }
    match decode_json_request::<BuildExecutorReadinessRequest>(&request) {
        Ok(BuildExecutorReadinessRequest {}) => match probe_readiness().await {
            Ok(readiness) => {
                respond_to_readiness_if_authorized(
                    &runtime,
                    NatsServiceResponse::json_ok(&BuildExecutorReadinessAnswer {
                        identity,
                        readiness,
                    }),
                )
                .await
            }
            Err(error) => {
                respond_to_readiness_if_authorized(
                    &runtime,
                    NatsServiceResponse::transport_error(
                        ployz_nats::service_runtime::NatsServiceError::internal(error.to_string()),
                    ),
                )
                .await
            }
        },
        Err(response) => respond_to_readiness_if_authorized(&runtime, response).await,
    }
}

async fn respond_to_readiness_if_authorized(
    runtime: &ExternalBuildRuntime,
    response: NatsServiceResponse,
) -> NatsServiceResponse {
    if runtime.readiness_authorized_before_expiry().await {
        response
    } else {
        std::future::pending().await
    }
}

async fn handle_start(runtime: ExternalBuildRuntime, request: NatsServiceRequest) -> HandledStart {
    let request = match decode_json_request::<BuildExecutorStartRequest>(&request) {
        Ok(request) => request,
        Err(response) => return HandledStart::waiting(response),
    };
    match runtime.start(request).await {
        Ok(ok) => HandledStart::terminal(NatsServiceResponse::json_ok(
            &BuildExecutorStartResponse::Ok(Box::new(ok)),
        )),
        Err(error) => {
            let completes_once = start_error_completes_once(&error);
            HandledStart {
                response: NatsServiceResponse::json_domain_error(
                    &BuildExecutorStartResponse::DomainError { error },
                ),
                completes_once,
            }
        }
    }
}

struct HandledStart {
    response: NatsServiceResponse,
    completes_once: bool,
}

impl HandledStart {
    fn terminal(response: NatsServiceResponse) -> Self {
        Self {
            response,
            completes_once: true,
        }
    }

    fn waiting(response: NatsServiceResponse) -> Self {
        Self {
            response,
            completes_once: false,
        }
    }
}

fn start_error_completes_once(error: &ployz_core::build::BuildExecutorStartDomainError) -> bool {
    match error {
        ployz_core::build::BuildExecutorStartDomainError::OperationIdentityMismatch { .. }
        | ployz_core::build::BuildExecutorStartDomainError::AssignmentMismatch { .. }
        | ployz_core::build::BuildExecutorStartDomainError::ExecutorIdentityMismatch { .. } => {
            false
        }
        ployz_core::build::BuildExecutorStartDomainError::RuntimeUnavailable
        | ployz_core::build::BuildExecutorStartDomainError::RuntimeStopped
        | ployz_core::build::BuildExecutorStartDomainError::PlatformMismatch { .. }
        | ployz_core::build::BuildExecutorStartDomainError::ToolchainUnavailable { .. }
        | ployz_core::build::BuildExecutorStartDomainError::ImageSeedUnavailable { .. }
        | ployz_core::build::BuildExecutorStartDomainError::InvalidTimeout { .. }
        | ployz_core::build::BuildExecutorStartDomainError::AlreadyRunning
        | ployz_core::build::BuildExecutorStartDomainError::PlatformFailed { .. }
        | ployz_core::build::BuildExecutorStartDomainError::Cancelled { .. }
        | ployz_core::build::BuildExecutorStartDomainError::TimedOut { .. } => true,
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
    fn watch_completion_is_a_noop_and_once_waits_through_provenance_rejection() {
        CompletionMode::Watch.notify_terminal(true);

        let (sender, mut receiver) = oneshot::channel();
        let once = CompletionMode::once(sender);
        once.notify_terminal(false);
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        once.notify_terminal(true);
        once.notify_terminal(true);
        assert_eq!(receiver.blocking_recv(), Ok(()));
    }

    #[test]
    fn only_binding_rejections_leave_one_shot_waiting() {
        let expected = ployz_core::ids::OperationId::try_new("op_expected").expect("operation");
        let actual = ployz_core::ids::OperationId::try_new("op_actual").expect("operation");
        assert!(!start_error_completes_once(
            &ployz_core::build::BuildExecutorStartDomainError::OperationIdentityMismatch {
                expected,
                actual,
            }
        ));
        assert!(start_error_completes_once(
            &ployz_core::build::BuildExecutorStartDomainError::RuntimeUnavailable
        ));
    }
}
