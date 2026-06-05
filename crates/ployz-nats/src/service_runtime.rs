//! NATS Service API runtime adapter.

pub use crate::service_protocol::{
    NATS_SERVICE_ERROR_CODE_HEADER, NATS_SERVICE_ERROR_HEADER, NatsServiceError,
    NatsServiceErrorCode, NatsServiceErrorHeaderDecodeError, decode_nats_service_error,
};
use crate::services::{NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata};
use async_nats::service::ServiceExt;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::timeout;

pub type NatsClient = async_nats::Client;

#[derive(Debug)]
pub struct RunningNatsService {
    service: Option<async_nats::service::Service>,
    endpoint_tasks: Vec<JoinHandle<()>>,
    health: Arc<NatsServiceHealthCounters>,
}

impl RunningNatsService {
    pub async fn bind_endpoint<H, F>(
        &mut self,
        endpoint: &NatsServiceEndpointSpec,
        handler: H,
    ) -> Result<(), NatsServiceRuntimeError>
    where
        H: Fn(NatsServiceRequest) -> F + Send + Sync + 'static,
        F: Future<Output = NatsServiceResponse> + Send + 'static,
    {
        self.bind_endpoint_with_policy(endpoint, EndpointExecutionPolicy::default(), handler)
            .await
    }

    pub async fn bind_endpoint_with_policy<H, F>(
        &mut self,
        endpoint: &NatsServiceEndpointSpec,
        policy: EndpointExecutionPolicy,
        handler: H,
    ) -> Result<(), NatsServiceRuntimeError>
    where
        H: Fn(NatsServiceRequest) -> F + Send + Sync + 'static,
        F: Future<Output = NatsServiceResponse> + Send + 'static,
    {
        let Some(service) = self.service.as_ref() else {
            return Err(NatsServiceRuntimeError::Stopped);
        };
        let requests = service
            .endpoint_builder()
            .name(endpoint.name)
            .add(endpoint.subject.clone())
            .await
            .map_err(|error| NatsServiceRuntimeError::AddEndpoint {
                subject: endpoint.subject.clone(),
                message: error.to_string(),
            })?;

        let handler = Arc::new(handler);
        let health = Arc::clone(&self.health);
        health
            .endpoint_tasks_started
            .fetch_add(1, Ordering::Relaxed);
        let task = tokio::spawn(async move {
            requests
                .for_each_concurrent(policy.max_concurrent_requests.get(), |request| {
                    let handler = Arc::clone(&handler);
                    let health = Arc::clone(&health);
                    async move {
                        let payload = request.message.payload.to_vec();
                        let response = match timeout(
                            policy.request_timeout,
                            handler(NatsServiceRequest { payload }),
                        )
                        .await
                        {
                            Ok(NatsServiceResponse::Ok { payload }) => Ok(payload.into()),
                            Ok(NatsServiceResponse::DomainError { payload }) => {
                                health.domain_failures.fetch_add(1, Ordering::Relaxed);
                                Ok(payload.into())
                            }
                            Ok(NatsServiceResponse::TransportError { error }) => {
                                health.handler_failures.fetch_add(1, Ordering::Relaxed);
                                Err(error.into_nats_error())
                            }
                            Err(_) => {
                                health.request_timeouts.fetch_add(1, Ordering::Relaxed);
                                Err(NatsServiceError::timeout(format!(
                                    "request timed out after {}ms",
                                    policy.request_timeout.as_millis(),
                                ))
                                .into_nats_error())
                            }
                        };

                        if request.respond(response).await.is_err() {
                            health.response_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
                .await;
            health
                .endpoint_tasks_finished
                .fetch_add(1, Ordering::Relaxed);
        });
        self.endpoint_tasks.push(task);
        Ok(())
    }

    #[must_use]
    pub fn health(&self) -> NatsServiceHealth {
        self.health.snapshot()
    }

    pub async fn shutdown(mut self) -> Result<(), NatsServiceShutdownError> {
        if let Some(service) = self.service.take() {
            service
                .stop()
                .await
                .map_err(|error| NatsServiceShutdownError::StopService {
                    message: error.to_string(),
                })?;
        }

        for task in self.endpoint_tasks.drain(..) {
            task.await
                .map_err(|error| NatsServiceShutdownError::EndpointTaskJoin {
                    message: error.to_string(),
                })?;
        }

        Ok(())
    }
}

impl Drop for RunningNatsService {
    fn drop(&mut self) {
        for task in &self.endpoint_tasks {
            task.abort();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointExecutionPolicy {
    pub max_concurrent_requests: NonZeroUsize,
    pub request_timeout: Duration,
}

impl EndpointExecutionPolicy {
    #[must_use]
    pub const fn new(max_concurrent_requests: NonZeroUsize, request_timeout: Duration) -> Self {
        Self {
            max_concurrent_requests,
            request_timeout,
        }
    }
}

impl Default for EndpointExecutionPolicy {
    fn default() -> Self {
        let Some(max_concurrent_requests) = NonZeroUsize::new(32) else {
            unreachable!("default concurrency is non-zero");
        };
        Self {
            max_concurrent_requests,
            request_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServiceHealth {
    pub endpoint_tasks_started: usize,
    pub endpoint_tasks_finished: usize,
    pub request_timeouts: usize,
    pub handler_failures: usize,
    pub domain_failures: usize,
    pub response_failures: usize,
}

#[derive(Debug, Default)]
struct NatsServiceHealthCounters {
    endpoint_tasks_started: AtomicUsize,
    endpoint_tasks_finished: AtomicUsize,
    request_timeouts: AtomicUsize,
    handler_failures: AtomicUsize,
    domain_failures: AtomicUsize,
    response_failures: AtomicUsize,
}

impl NatsServiceHealthCounters {
    fn snapshot(&self) -> NatsServiceHealth {
        NatsServiceHealth {
            endpoint_tasks_started: self.endpoint_tasks_started.load(Ordering::Relaxed),
            endpoint_tasks_finished: self.endpoint_tasks_finished.load(Ordering::Relaxed),
            request_timeouts: self.request_timeouts.load(Ordering::Relaxed),
            handler_failures: self.handler_failures.load(Ordering::Relaxed),
            domain_failures: self.domain_failures.load(Ordering::Relaxed),
            response_failures: self.response_failures.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsServiceRequest {
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServiceResponse {
    Ok { payload: Vec<u8> },
    DomainError { payload: Vec<u8> },
    TransportError { error: NatsServiceError },
}

impl NatsServiceResponse {
    #[must_use]
    pub fn ok(payload: impl Into<Vec<u8>>) -> Self {
        Self::Ok {
            payload: payload.into(),
        }
    }

    #[must_use]
    pub fn domain_error(payload: impl Into<Vec<u8>>) -> Self {
        Self::DomainError {
            payload: payload.into(),
        }
    }

    #[must_use]
    pub fn transport_error(error: NatsServiceError) -> Self {
        Self::TransportError { error }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServiceRuntimeError {
    StartService { name: &'static str, message: String },
    AddEndpoint { subject: String, message: String },
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServiceShutdownError {
    StopService { message: String },
    EndpointTaskJoin { message: String },
}

pub async fn start_nats_service(
    client: async_nats::Client,
    spec: &NatsServiceSpec,
) -> Result<RunningNatsService, NatsServiceRuntimeError> {
    let metadata = service_metadata_map(&spec.metadata);
    let service = client
        .service_builder()
        .description(spec.description)
        .metadata(metadata)
        .start(spec.name, spec.version.to_string())
        .await
        .map_err(|error| NatsServiceRuntimeError::StartService {
            name: spec.name,
            message: error.to_string(),
        })?;

    Ok(RunningNatsService {
        service: Some(service),
        endpoint_tasks: Vec::new(),
        health: Arc::new(NatsServiceHealthCounters::default()),
    })
}

fn service_metadata_map(metadata: &ServiceMetadata) -> HashMap<String, String> {
    metadata
        .entries()
        .iter()
        .map(|entry| (entry.key.to_owned(), entry.value.clone()))
        .collect()
}
