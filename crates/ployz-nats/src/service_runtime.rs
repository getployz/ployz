//! NATS Service API runtime adapter.

pub use crate::service_protocol::{
    NATS_SERVICE_ERROR_CODE_HEADER, NATS_SERVICE_ERROR_HEADER, NatsServiceError,
    NatsServiceErrorCode, NatsServiceErrorHeaderDecodeError, decode_nats_service_error,
};
use crate::services::{NatsServiceEndpointSpec, NatsServiceSpec, ServiceMetadata};
use async_nats::service::ServiceExt;
use futures_util::StreamExt;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::future::Future;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::timeout;

pub type NatsClient = async_nats::Client;

pub async fn request_json<Request, Response>(
    client: &async_nats::Client,
    subject: String,
    request: &Request,
    request_timeout: Duration,
) -> Result<Response, NatsJsonServiceRequestError>
where
    Request: Serialize + ?Sized,
    Response: DeserializeOwned,
{
    let payload = serde_json::to_vec(request).map_err(|error| {
        NatsJsonServiceRequestError::EncodeRequest {
            message: error.to_string(),
        }
    })?;
    let nats_request = async_nats::Request::new()
        .payload(payload.into())
        .timeout(Some(request_timeout));
    let response = client
        .send_request(subject, nats_request)
        .await
        .map_err(|error| NatsJsonServiceRequestError::Request {
            failure: request_failure(error),
        })?;

    match decode_nats_service_error(response.headers.as_ref()) {
        Ok(Some(failure)) => return Err(NatsJsonServiceRequestError::Service { failure }),
        Ok(None) => {}
        Err(error) => return Err(NatsJsonServiceRequestError::ServiceProtocol { error }),
    }

    serde_json::from_slice::<Response>(&response.payload).map_err(|error| {
        NatsJsonServiceRequestError::DecodeResponse {
            message: error.to_string(),
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsJsonServiceRequestError {
    EncodeRequest {
        message: String,
    },
    Request {
        failure: NatsServiceRequestFailure,
    },
    Service {
        failure: NatsServiceError,
    },
    ServiceProtocol {
        error: NatsServiceErrorHeaderDecodeError,
    },
    DecodeResponse {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsServiceRequestFailure {
    TimedOut,
    NoResponders,
    InvalidSubject,
    MaxPayloadExceeded,
    Other { message: String },
}

impl std::fmt::Display for NatsServiceRequestFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => write!(formatter, "timed out"),
            Self::NoResponders => write!(formatter, "no responders"),
            Self::InvalidSubject => write!(formatter, "invalid subject"),
            Self::MaxPayloadExceeded => write!(formatter, "max payload exceeded"),
            Self::Other { message } => write!(formatter, "{message}"),
        }
    }
}

fn request_failure(error: async_nats::RequestError) -> NatsServiceRequestFailure {
    match error.kind() {
        async_nats::RequestErrorKind::TimedOut => NatsServiceRequestFailure::TimedOut,
        async_nats::RequestErrorKind::NoResponders => NatsServiceRequestFailure::NoResponders,
        async_nats::RequestErrorKind::InvalidSubject => NatsServiceRequestFailure::InvalidSubject,
        async_nats::RequestErrorKind::MaxPayloadExceeded => {
            NatsServiceRequestFailure::MaxPayloadExceeded
        }
        async_nats::RequestErrorKind::Other => NatsServiceRequestFailure::Other {
            message: error.to_string(),
        },
    }
}

#[derive(Debug)]
pub struct RunningNatsService {
    client: async_nats::Client,
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
        self.client
            .flush()
            .await
            .map_err(|error| NatsServiceRuntimeError::AddEndpoint {
                subject: endpoint.subject.clone(),
                message: format!("subscription flush failed: {error}"),
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
        tokio::task::yield_now().await;
        self.client
            .flush()
            .await
            .map_err(|error| NatsServiceRuntimeError::AddEndpoint {
                subject: endpoint.subject.clone(),
                message: format!("endpoint task flush failed: {error}"),
            })?;
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

    #[must_use]
    pub fn json_ok<T>(response: &T) -> Self
    where
        T: Serialize,
    {
        json_response(response, Self::ok)
    }

    #[must_use]
    pub fn json_domain_error<T>(response: &T) -> Self
    where
        T: Serialize,
    {
        json_response(response, Self::domain_error)
    }
}

pub fn decode_json_request<T>(request: &NatsServiceRequest) -> Result<T, NatsServiceResponse>
where
    T: DeserializeOwned,
{
    serde_json::from_slice::<T>(&request.payload).map_err(|error| {
        NatsServiceResponse::transport_error(NatsServiceError::bad_request(error.to_string()))
    })
}

fn json_response<T>(
    response: &T,
    output: impl FnOnce(Vec<u8>) -> NatsServiceResponse,
) -> NatsServiceResponse
where
    T: Serialize,
{
    match serde_json::to_vec(response) {
        Ok(payload) => output(payload),
        Err(error) => {
            NatsServiceResponse::transport_error(NatsServiceError::internal(error.to_string()))
        }
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
        client,
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
