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

/// A service's subscriptions have a natural gap while its connection
/// re-establishes them after a reconnect, and the server answers requests
/// in that gap with no-responders. Requests retry briefly across the gap
/// before reporting the service unavailable; every retry is bounded, never
/// a wait-forever.
const NO_RESPONDERS_RETRIES: usize = 4;
const NO_RESPONDERS_RETRY_DELAY: Duration = Duration::from_millis(100);
const SERVICE_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(2);
const SERVICE_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

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
    let mut attempts = 0;
    let response = loop {
        let nats_request = async_nats::Request::new()
            .payload(payload.clone().into())
            .timeout(Some(request_timeout));
        match client.send_request(subject.clone(), nats_request).await {
            Ok(response) => break response,
            Err(error)
                if error.kind() == async_nats::RequestErrorKind::NoResponders
                    && attempts < NO_RESPONDERS_RETRIES =>
            {
                attempts += 1;
                tokio::time::sleep(NO_RESPONDERS_RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(NatsJsonServiceRequestError::Request {
                    failure: request_failure(error),
                });
            }
        }
    };

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsJsonServiceRequestError {
    #[error("failed to encode request: {message}")]
    EncodeRequest { message: String },
    #[error("request failed: {failure}")]
    Request { failure: NatsServiceRequestFailure },
    #[error("service returned an error: {}", failure.message)]
    Service { failure: NatsServiceError },
    #[error("service error header could not be decoded: {error}")]
    ServiceProtocol {
        error: NatsServiceErrorHeaderDecodeError,
    },
    #[error("failed to decode response: {message}")]
    DecodeResponse { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsServiceRequestFailure {
    #[error("timed out")]
    TimedOut,
    #[error("no responders")]
    NoResponders,
    #[error("invalid subject")]
    InvalidSubject,
    #[error("max payload exceeded")]
    MaxPayloadExceeded,
    #[error("{message}")]
    Other { message: String },
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
    name: &'static str,
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
        self.bind_endpoint_inner(endpoint, EndpointExecutionPolicy::default(), handler)
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
        self.bind_endpoint_inner(endpoint, policy, handler).await
    }

    async fn bind_endpoint_inner<H, F>(
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
        if policy.authority.is_expired() {
            return Ok(());
        }
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
        let client = self.client.clone();
        health
            .endpoint_tasks_started
            .fetch_add(1, Ordering::Relaxed);
        let task = tokio::spawn(async move {
            let serve =
                requests.for_each_concurrent(policy.max_concurrent_requests.get(), |request| {
                    let handler = Arc::clone(&handler);
                    let health = Arc::clone(&health);
                    let client = client.clone();
                    async move {
                        let payload = request.message.payload.to_vec();
                        let headers = request.message.headers.clone();
                        let response = match timeout(
                            policy.request_timeout,
                            handler(NatsServiceRequest { payload, headers }),
                        )
                        .await
                        {
                            Ok(response) => {
                                response_within_max_payload(response, client.max_payload())
                            }
                            Err(_) => {
                                health.request_timeouts.fetch_add(1, Ordering::Relaxed);
                                NatsServiceResponse::transport_error(NatsServiceError::timeout(
                                    format!(
                                        "request timed out after {}ms",
                                        policy.request_timeout.as_millis(),
                                    ),
                                ))
                            }
                        };
                        let response = match response {
                            NatsServiceResponse::Ok { payload } => Ok(payload.into()),
                            NatsServiceResponse::DomainError { payload } => {
                                health.domain_failures.fetch_add(1, Ordering::Relaxed);
                                Ok(payload.into())
                            }
                            NatsServiceResponse::TransportError { error } => {
                                health.handler_failures.fetch_add(1, Ordering::Relaxed);
                                Err(error.into_nats_error())
                            }
                        };

                        if request.respond(response).await.is_err() {
                            health.response_failures.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            tokio::pin!(serve);
            match policy.authority {
                EndpointAuthority::Unbounded => serve.await,
                EndpointAuthority::Deadline(deadline) => {
                    tokio::select! {
                        biased;
                        () = tokio::time::sleep_until(deadline) => {}
                        () = &mut serve => {}
                    }
                }
            }
            health
                .endpoint_tasks_finished
                .fetch_add(1, Ordering::Relaxed);
        });
        self.endpoint_tasks.push(task);
        timeout(SERVICE_REGISTRATION_TIMEOUT, self.client.flush())
            .await
            .map_err(|error| NatsServiceRuntimeError::AddEndpoint {
                subject: endpoint.subject.clone(),
                message: format!("subscription flush timed out: {error}"),
            })?
            .map_err(|error| NatsServiceRuntimeError::AddEndpoint {
                subject: endpoint.subject.clone(),
                message: format!("subscription flush failed: {error}"),
            })?;
        Ok(())
    }

    #[must_use]
    pub fn health(&self) -> NatsServiceHealth {
        self.health.snapshot()
    }

    #[must_use]
    pub fn health_reader(&self) -> NatsServiceHealthReader {
        NatsServiceHealthReader {
            health: Arc::clone(&self.health),
        }
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

        let connection_state = self.client.connection_state();
        if connection_state != async_nats::connection::State::Connected {
            eprintln!(
                "warning: forcing NATS service {} shutdown while client is {}",
                self.name, connection_state,
            );
            self.abort_endpoint_tasks();
            return self.await_aborted_endpoint_tasks().await;
        }

        match timeout(SERVICE_SHUTDOWN_GRACE, self.await_endpoint_tasks()).await {
            Ok(result) => result,
            Err(_) => {
                eprintln!(
                    "warning: forcing NATS service {} shutdown after {}ms grace",
                    self.name,
                    SERVICE_SHUTDOWN_GRACE.as_millis(),
                );
                self.abort_endpoint_tasks();
                self.await_aborted_endpoint_tasks().await
            }
        }
    }

    async fn await_endpoint_tasks(&mut self) -> Result<(), NatsServiceShutdownError> {
        while let Some(task) = self.endpoint_tasks.first_mut() {
            task.await
                .map_err(|error| NatsServiceShutdownError::EndpointTaskJoin {
                    message: error.to_string(),
                })?;
            self.endpoint_tasks.remove(0);
        }
        Ok(())
    }

    fn abort_endpoint_tasks(&self) {
        for task in &self.endpoint_tasks {
            task.abort();
        }
    }

    async fn await_aborted_endpoint_tasks(&mut self) -> Result<(), NatsServiceShutdownError> {
        for task in self.endpoint_tasks.drain(..) {
            match task.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    return Err(NatsServiceShutdownError::EndpointTaskJoin {
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointAuthority {
    Unbounded,
    Deadline(tokio::time::Instant),
}

impl EndpointAuthority {
    fn is_expired(self) -> bool {
        matches!(self, Self::Deadline(deadline) if deadline <= tokio::time::Instant::now())
    }
}

fn response_within_max_payload(
    response: NatsServiceResponse,
    max_payload: usize,
) -> NatsServiceResponse {
    match response {
        NatsServiceResponse::Ok { payload } => {
            if payload.len() > max_payload {
                NatsServiceResponse::transport_error(NatsServiceError::response_too_large())
            } else {
                NatsServiceResponse::Ok { payload }
            }
        }
        NatsServiceResponse::DomainError { payload } => {
            if payload.len() > max_payload {
                NatsServiceResponse::transport_error(NatsServiceError::response_too_large())
            } else {
                NatsServiceResponse::DomainError { payload }
            }
        }
        NatsServiceResponse::TransportError { error } => {
            if service_error_wire_len(&error) > max_payload {
                NatsServiceResponse::transport_error(NatsServiceError::response_too_large())
            } else {
                NatsServiceResponse::TransportError { error }
            }
        }
    }
}

fn service_error_wire_len(error: &NatsServiceError) -> usize {
    // Service errors are header-only replies. This mirrors the two headers
    // async-nats adds in Request::respond so an oversized header-only reply is
    // replaced before publish and the requester receives the small typed error.
    b"NATS/1.0\r\n".len()
        + NATS_SERVICE_ERROR_HEADER.len()
        + b": \r\n".len()
        + error.message.len()
        + NATS_SERVICE_ERROR_CODE_HEADER.len()
        + b": \r\n".len()
        + error.code.http_status_code().to_string().len()
        + b"\r\n".len()
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
    authority: EndpointAuthority,
}

impl EndpointExecutionPolicy {
    #[must_use]
    pub const fn new(max_concurrent_requests: NonZeroUsize, request_timeout: Duration) -> Self {
        Self {
            max_concurrent_requests,
            request_timeout,
            authority: EndpointAuthority::Unbounded,
        }
    }

    #[must_use]
    pub const fn with_authority_deadline(mut self, deadline: tokio::time::Instant) -> Self {
        self.authority = EndpointAuthority::Deadline(deadline);
        self
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
            authority: EndpointAuthority::Unbounded,
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

#[derive(Debug, Clone)]
pub struct NatsServiceHealthReader {
    health: Arc<NatsServiceHealthCounters>,
}

impl NatsServiceHealthReader {
    #[must_use]
    pub fn snapshot(&self) -> NatsServiceHealth {
        self.health.snapshot()
    }
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
    pub headers: Option<async_nats::HeaderMap>,
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsServiceRuntimeError {
    #[error("failed to start service {name}: {message}")]
    StartService { name: &'static str, message: String },
    #[error("failed to add endpoint {subject}: {message}")]
    AddEndpoint { subject: String, message: String },
    #[error("service is stopped")]
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NatsServiceShutdownError {
    #[error("failed to stop service: {message}")]
    StopService { message: String },
    #[error("endpoint task failed to join: {message}")]
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
        name: spec.name,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_service_error_is_replaced_with_response_too_large() {
        let response =
            NatsServiceResponse::transport_error(NatsServiceError::internal("x".repeat(256)));

        assert_eq!(
            response_within_max_payload(response, 256),
            NatsServiceResponse::transport_error(NatsServiceError::response_too_large())
        );
    }
}
