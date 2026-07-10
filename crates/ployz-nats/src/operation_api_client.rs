//! Typed client for Ployz user-facing NATS services.

use crate::service_protocol::{NatsServiceError, NatsServiceErrorHeaderDecodeError};
use crate::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, request_json,
};
use ployz_core::ops::{OperationEventReplayPage, OperationStatusSnapshot};
use ployz_core::subjects::OperationApiEndpoint;
use ployz_sdk_types::{
    AcceptedOperation, CoreReplaceError, CoreReplaceReportError, CoreReplaceReportRequest,
    CoreReplaceReported, CoreReplaceRequest, DeploySubmitError, DeploySubmitRequest,
    InitFirstMachineActivateError, InitFirstMachineActivateRequest, InitFirstMachineActivated,
    LogsTailError, LogsTailRequest, LogsTailResult, MachineAddAccepted, MachineAddError,
    MachineAddRequest, MachineInspectError, MachineInspectRequest, MachineJoinRedeemError,
    MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinReportError,
    MachineJoinReportRequest, MachineJoinReported, MachineLifecycleError, MachineLifecycleRequest,
    MachineListError, MachineListRequest, MachineListResult, MachineSnapshot, MachineUpdateError,
    MachineUpdateRequest, NamespaceRemoveError, NamespaceRemoveRequest, OperationApiResponse,
    OpsListError, OpsListRequest, OpsListResult, OpsStatusError, OpsStatusRequest, OpsWatchError,
    OpsWatchRequest, RuntimeSnapshotError, RuntimeSnapshotRequest, RuntimeSnapshotResult,
    ServiceInspectError, ServiceInspectRequest, ServiceListError, ServiceListRequest,
    ServiceListResult, ServiceRestartError, ServiceRestartRequest, ServiceSnapshot,
    VolumeListError, VolumeListRequest, VolumeListResult, VolumeRemoveError, VolumeRemoveRequest,
    operation_api::{
        CoreReplaceApi, CoreReplaceReportApi, DeploySubmitApi, InitFirstMachineActivateApi,
        LogsTailApi, MachineAddApi, MachineDrainApi, MachineInspectApi, MachineJoinRedeemApi,
        MachineJoinReportApi, MachineListApi, MachineResumeApi, MachineUpdateApi,
        NamespaceRemoveApi, OperationApiContract, OpsListApi, OpsStatusApi, OpsWatchApi,
        RuntimeSnapshotApi, ServiceInspectApi, ServiceListApi, ServiceRestartApi, VolumeListApi,
        VolumeRemoveApi,
    },
};
use serde::{Serialize, de::DeserializeOwned};
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

    pub async fn init_first_machine_activate(
        &self,
        request: &InitFirstMachineActivateRequest,
    ) -> Result<InitFirstMachineActivated, OperationApiClientError<InitFirstMachineActivateError>>
    {
        self.request_api::<InitFirstMachineActivateApi>(request)
            .await
    }

    pub async fn ops_status(
        &self,
        request: &OpsStatusRequest,
    ) -> Result<OperationStatusSnapshot, OperationApiClientError<OpsStatusError>> {
        self.request_api::<OpsStatusApi>(request).await
    }

    pub async fn ops_list(
        &self,
        request: &OpsListRequest,
    ) -> Result<OpsListResult, OperationApiClientError<OpsListError>> {
        self.request_api::<OpsListApi>(request).await
    }

    pub async fn machine_add(
        &self,
        request: &MachineAddRequest,
    ) -> Result<MachineAddAccepted, OperationApiClientError<MachineAddError>> {
        self.request_api::<MachineAddApi>(request).await
    }

    pub async fn machine_update(
        &self,
        request: &MachineUpdateRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<MachineUpdateError>> {
        self.request_api::<MachineUpdateApi>(request).await
    }

    pub async fn machine_drain(
        &self,
        request: &MachineLifecycleRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<MachineLifecycleError>> {
        self.request_api::<MachineDrainApi>(request).await
    }

    pub async fn machine_resume(
        &self,
        request: &MachineLifecycleRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<MachineLifecycleError>> {
        self.request_api::<MachineResumeApi>(request).await
    }

    pub async fn core_replace(
        &self,
        request: &CoreReplaceRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<CoreReplaceError>> {
        self.request_api::<CoreReplaceApi>(request).await
    }

    pub async fn core_replace_report(
        &self,
        request: &CoreReplaceReportRequest,
    ) -> Result<CoreReplaceReported, OperationApiClientError<CoreReplaceReportError>> {
        self.request_api::<CoreReplaceReportApi>(request).await
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

    pub async fn service_restart(
        &self,
        request: &ServiceRestartRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<ServiceRestartError>> {
        self.request_api::<ServiceRestartApi>(request).await
    }

    pub async fn namespace_remove(
        &self,
        request: &NamespaceRemoveRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<NamespaceRemoveError>> {
        self.request_api::<NamespaceRemoveApi>(request).await
    }

    pub async fn volume_list(
        &self,
        request: &VolumeListRequest,
    ) -> Result<VolumeListResult, OperationApiClientError<VolumeListError>> {
        self.request_api::<VolumeListApi>(request).await
    }

    pub async fn volume_remove(
        &self,
        request: &VolumeRemoveRequest,
    ) -> Result<AcceptedOperation, OperationApiClientError<VolumeRemoveError>> {
        self.request_api::<VolumeRemoveApi>(request).await
    }

    pub async fn runtime_snapshot(
        &self,
        request: &RuntimeSnapshotRequest,
    ) -> Result<RuntimeSnapshotResult, OperationApiClientError<RuntimeSnapshotError>> {
        self.request_api::<RuntimeSnapshotApi>(request).await
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
        let response = request_json::<_, OperationApiResponse<C::Success, C::Error>>(
            &self.client,
            C::ENDPOINT.subject().to_owned(),
            request,
            self.request_timeout,
        )
        .await
        .map_err(|error| match error {
            NatsJsonServiceRequestError::EncodeRequest { message } => {
                OperationApiClientError::EncodeRequest {
                    endpoint: C::ENDPOINT,
                    message,
                }
            }
            NatsJsonServiceRequestError::Request { failure } => OperationApiClientError::Request {
                endpoint: C::ENDPOINT,
                failure,
            },
            NatsJsonServiceRequestError::Service { failure } => OperationApiClientError::Service {
                endpoint: C::ENDPOINT,
                failure,
            },
            NatsJsonServiceRequestError::ServiceProtocol { error } => {
                OperationApiClientError::ServiceProtocol {
                    endpoint: C::ENDPOINT,
                    error,
                }
            }
            NatsJsonServiceRequestError::DecodeResponse { message } => {
                OperationApiClientError::DecodeResponse {
                    endpoint: C::ENDPOINT,
                    message,
                }
            }
        })?;

        match response {
            OperationApiResponse::Ok { value } => Ok(value),
            OperationApiResponse::DomainError { error } => Err(OperationApiClientError::Domain {
                endpoint: C::ENDPOINT,
                error,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OperationApiClientError<E> {
    #[error("failed to encode {} request: {message}", endpoint.subject())]
    EncodeRequest {
        endpoint: OperationApiEndpoint,
        message: String,
    },
    #[error("{} request failed: {failure}", endpoint.subject())]
    Request {
        endpoint: OperationApiEndpoint,
        failure: NatsServiceRequestFailure,
    },
    #[error(
        "{} returned service error {}: {}",
        endpoint.subject(),
        failure.code.http_status_code(),
        failure.message
    )]
    Service {
        endpoint: OperationApiEndpoint,
        failure: NatsServiceError,
    },
    #[error("{} returned malformed service error headers: {error}", endpoint.subject())]
    ServiceProtocol {
        endpoint: OperationApiEndpoint,
        error: NatsServiceErrorHeaderDecodeError,
    },
    #[error("failed to decode {} response: {message}", endpoint.subject())]
    DecodeResponse {
        endpoint: OperationApiEndpoint,
        message: String,
    },
    #[error("{} failed: {error}", endpoint.subject())]
    Domain {
        endpoint: OperationApiEndpoint,
        error: E,
    },
}
