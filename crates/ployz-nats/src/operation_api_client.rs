//! Typed client for Ployz user-facing NATS services.

use crate::service_protocol::{
    NatsServiceError, NatsServiceErrorHeaderDecodeError, decode_nats_service_error,
};
use ployz_core::ops::{OperationEventReplayPage, OperationStatusSnapshot};
use ployz_core::subjects::OperationApiEndpoint;
use ployz_sdk_types::{
    AcceptedOperation, BackupCreateError, BackupCreateRequest, DeploySubmitError,
    DeploySubmitRequest, InitFirstNodeActivateError, InitFirstNodeActivateRequest,
    InitFirstNodeActivated, LogsTailError, LogsTailRequest, LogsTailResult, MachineAddAccepted,
    MachineAddError, MachineAddRequest, MachineInspectError, MachineInspectRequest,
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinReportError,
    MachineJoinReportRequest, MachineJoinReported, MachineListError, MachineListRequest,
    MachineListResult, MachineSnapshot, OperationApiResponse, OpsStatusError, OpsStatusRequest,
    OpsWatchError, OpsWatchRequest, ServiceInspectError, ServiceInspectRequest, ServiceListError,
    ServiceListRequest, ServiceListResult, ServiceSnapshot,
    operation_api::{
        BackupCreateApi, DeploySubmitApi, InitFirstNodeActivateApi, LogsTailApi, MachineAddApi,
        MachineInspectApi, MachineJoinRedeemApi, MachineJoinReportApi, MachineListApi,
        OperationApiContract, OpsStatusApi, OpsWatchApi, ServiceInspectApi, ServiceListApi,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use std::error::Error;
use std::fmt;
use std::time::Duration;

pub const DEFAULT_OPERATION_API_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct OperationApiClient {
    client: async_nats::Client,
    request_timeout: Duration,
}

impl OperationApiClient {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            client,
            request_timeout: DEFAULT_OPERATION_API_REQUEST_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }

    pub async fn deploy_submit(
        &self,
        request: &DeploySubmitRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<DeploySubmitError>> {
        self.request_api::<DeploySubmitApi>(request).await
    }

    pub async fn backup_create(
        &self,
        request: &BackupCreateRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<BackupCreateError>> {
        self.request_api::<BackupCreateApi>(request).await
    }

    pub async fn init_first_node_activate(
        &self,
        request: &InitFirstNodeActivateRequest,
    ) -> Result<InitFirstNodeActivated, OperationApiClientError<InitFirstNodeActivateError>> {
        self.request_api::<InitFirstNodeActivateApi>(request).await
    }

    pub async fn ops_status(
        &self,
        request: &OpsStatusRequest,
    ) -> Result<OperationStatusSnapshot, OperationApiClientError<OpsStatusError>> {
        self.request_api::<OpsStatusApi>(request).await
    }

    pub async fn machine_add(
        &self,
        request: &MachineAddRequest,
    ) -> Result<MachineAddAccepted, OperationApiClientError<MachineAddError>> {
        self.request_api::<MachineAddApi>(request).await
    }

    pub async fn machine_list(
        &self,
        request: &MachineListRequest,
    ) -> Result<MachineListResult, OperationApiClientError<MachineListError>> {
        self.request_api::<MachineListApi>(request).await
    }

    pub async fn machine_inspect(
        &self,
        request: &MachineInspectRequest,
    ) -> Result<MachineSnapshot, OperationApiClientError<MachineInspectError>> {
        self.request_api::<MachineInspectApi>(request).await
    }

    pub async fn service_list(
        &self,
        request: &ServiceListRequest,
    ) -> Result<ServiceListResult, OperationApiClientError<ServiceListError>> {
        self.request_api::<ServiceListApi>(request).await
    }

    pub async fn service_inspect(
        &self,
        request: &ServiceInspectRequest,
    ) -> Result<ServiceSnapshot, OperationApiClientError<ServiceInspectError>> {
        self.request_api::<ServiceInspectApi>(request).await
    }

    pub async fn logs_tail(
        &self,
        request: &LogsTailRequest,
    ) -> Result<LogsTailResult, OperationApiClientError<LogsTailError>> {
        self.request_api::<LogsTailApi>(request).await
    }

    pub async fn machine_join_redeem(
        &self,
        request: &MachineJoinRedeemRequest,
    ) -> Result<MachineJoinRedeemed, OperationApiClientError<MachineJoinRedeemError>> {
        self.request_api::<MachineJoinRedeemApi>(request).await
    }

    pub async fn machine_join_report(
        &self,
        request: &MachineJoinReportRequest,
    ) -> Result<MachineJoinReported, OperationApiClientError<MachineJoinReportError>> {
        self.request_api::<MachineJoinReportApi>(request).await
    }

    pub async fn ops_watch(
        &self,
        request: &OpsWatchRequest,
    ) -> Result<OperationEventReplayPage, OperationApiClientError<OpsWatchError>> {
        self.request_api::<OpsWatchApi>(request).await
    }

    async fn request_api<C>(
        &self,
        request: &C::Request,
    ) -> Result<C::Success, OperationApiClientError<C::Error>>
    where
        C: OperationApiContract,
        C::Request: Serialize,
        OperationApiResponse<C::Success, C::Error>: DeserializeOwned,
    {
        let payload = serde_json::to_vec(request).map_err(|error| {
            OperationApiClientError::EncodeRequest {
                endpoint: C::ENDPOINT,
                message: error.to_string(),
            }
        })?;
        let nats_request = async_nats::Request::new()
            .payload(payload.into())
            .timeout(Some(self.request_timeout));
        let response = self
            .client
            .send_request(C::ENDPOINT.subject(), nats_request)
            .await
            .map_err(|error| OperationApiClientError::Request {
                endpoint: C::ENDPOINT,
                failure: request_failure(error),
            })?;

        match decode_nats_service_error(response.headers.as_ref()) {
            Ok(Some(failure)) => {
                return Err(OperationApiClientError::Service {
                    endpoint: C::ENDPOINT,
                    failure,
                });
            }
            Ok(None) => {}
            Err(error) => {
                return Err(OperationApiClientError::ServiceProtocol {
                    endpoint: C::ENDPOINT,
                    error,
                });
            }
        }

        match serde_json::from_slice::<OperationApiResponse<C::Success, C::Error>>(
            &response.payload,
        )
        .map_err(|error| OperationApiClientError::DecodeResponse {
            endpoint: C::ENDPOINT,
            message: error.to_string(),
        })? {
            OperationApiResponse::Ok { value } => Ok(value),
            OperationApiResponse::DomainError { error } => Err(OperationApiClientError::Domain {
                endpoint: C::ENDPOINT,
                error,
            }),
        }
    }
}

fn request_failure(error: async_nats::RequestError) -> OperationApiRequestFailure {
    match error.kind() {
        async_nats::RequestErrorKind::TimedOut => OperationApiRequestFailure::TimedOut,
        async_nats::RequestErrorKind::NoResponders => OperationApiRequestFailure::NoResponders,
        async_nats::RequestErrorKind::InvalidSubject => OperationApiRequestFailure::InvalidSubject,
        async_nats::RequestErrorKind::MaxPayloadExceeded => {
            OperationApiRequestFailure::MaxPayloadExceeded
        }
        async_nats::RequestErrorKind::Other => OperationApiRequestFailure::Other {
            message: error.to_string(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationApiRequestFailure {
    TimedOut,
    NoResponders,
    InvalidSubject,
    MaxPayloadExceeded,
    Other { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationApiClientError<E> {
    EncodeRequest {
        endpoint: OperationApiEndpoint,
        message: String,
    },
    Request {
        endpoint: OperationApiEndpoint,
        failure: OperationApiRequestFailure,
    },
    Service {
        endpoint: OperationApiEndpoint,
        failure: NatsServiceError,
    },
    ServiceProtocol {
        endpoint: OperationApiEndpoint,
        error: NatsServiceErrorHeaderDecodeError,
    },
    DecodeResponse {
        endpoint: OperationApiEndpoint,
        message: String,
    },
    Domain {
        endpoint: OperationApiEndpoint,
        error: E,
    },
}

impl<E> fmt::Display for OperationApiClientError<E>
where
    E: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EncodeRequest { endpoint, message } => {
                write!(
                    formatter,
                    "failed to encode {} request: {message}",
                    endpoint.subject()
                )
            }
            Self::Request { endpoint, failure } => {
                write!(
                    formatter,
                    "{} request failed: {failure}",
                    endpoint.subject()
                )
            }
            Self::Service { endpoint, failure } => {
                write!(
                    formatter,
                    "{} returned service error {}: {}",
                    endpoint.subject(),
                    failure.code.http_status_code(),
                    failure.message,
                )
            }
            Self::ServiceProtocol { endpoint, error } => {
                write!(
                    formatter,
                    "{} returned malformed service error headers: {error}",
                    endpoint.subject()
                )
            }
            Self::DecodeResponse { endpoint, message } => {
                write!(
                    formatter,
                    "failed to decode {} response: {message}",
                    endpoint.subject()
                )
            }
            Self::Domain { endpoint, error } => {
                write!(formatter, "{} failed: {error:?}", endpoint.subject())
            }
        }
    }
}

impl<E> Error for OperationApiClientError<E> where E: fmt::Debug {}

impl fmt::Display for OperationApiRequestFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut => write!(formatter, "timed out"),
            Self::NoResponders => write!(formatter, "no responders"),
            Self::InvalidSubject => write!(formatter, "invalid subject"),
            Self::MaxPayloadExceeded => write!(formatter, "max payload exceeded"),
            Self::Other { message } => write!(formatter, "{message}"),
        }
    }
}
