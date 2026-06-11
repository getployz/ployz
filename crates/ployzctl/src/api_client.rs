//! Re-export the shared NATS operation API client for CLI callers.

pub use ployz_nats::operation_api_client::{
    DEFAULT_OPERATION_API_REQUEST_TIMEOUT, OperationApiClient, OperationApiClientError,
};
pub use ployz_nats::service_runtime::NatsServiceRequestFailure;
