//! User-facing operation API contract registry.

use crate::{
    AcceptedOperation, BackupCreateError, BackupCreateRequest, DeploySubmitError,
    DeploySubmitRequest, LogsTailError, LogsTailRequest, LogsTailResult, MachineAddAccepted,
    MachineAddError, MachineAddRequest, MachineInspectError, MachineInspectRequest,
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinReportError,
    MachineJoinReportRequest, MachineJoinReported, MachineListError, MachineListRequest,
    MachineListResult, MachineSnapshot, OperationStatusSnapshot, OpsStatusError, OpsStatusRequest,
    OpsWatchError, OpsWatchRequest, ServiceInspectError, ServiceInspectRequest, ServiceListError,
    ServiceListRequest, ServiceListResult, ServiceSnapshot,
};
use ployz_core::ops::OperationEventReplayPage;
use ployz_core::subjects::OperationApiEndpoint;

pub trait OperationApiContract {
    type Request;
    type Success;
    type Error;

    const ENDPOINT: OperationApiEndpoint;
    const REQUEST_ALIAS: Option<&'static str> = None;
    const RESPONSE_ALIAS: &'static str;
}

#[macro_export]
macro_rules! operation_api_contracts {
    ($macro:ident) => {
        $macro!(
            $crate::operation_api::DeploySubmitApi,
            $crate::operation_api::MachineAddApi,
            $crate::operation_api::MachineListApi,
            $crate::operation_api::MachineInspectApi,
            $crate::operation_api::MachineJoinRedeemApi,
            $crate::operation_api::MachineJoinReportApi,
            $crate::operation_api::ServiceListApi,
            $crate::operation_api::ServiceInspectApi,
            $crate::operation_api::LogsTailApi,
            $crate::operation_api::OpsStatusApi,
            $crate::operation_api::OpsWatchApi,
            $crate::operation_api::BackupCreateApi
        );
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploySubmitApi;

impl OperationApiContract for DeploySubmitApi {
    type Request = DeploySubmitRequest;
    type Success = AcceptedOperation;
    type Error = DeploySubmitError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::DeploySubmit;
    const RESPONSE_ALIAS: &'static str = "DeploySubmitResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupCreateApi;

impl OperationApiContract for BackupCreateApi {
    type Request = BackupCreateRequest;
    type Success = AcceptedOperation;
    type Error = BackupCreateError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::BackupCreate;
    const RESPONSE_ALIAS: &'static str = "BackupCreateResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineAddApi;

impl OperationApiContract for MachineAddApi {
    type Request = MachineAddRequest;
    type Success = MachineAddAccepted;
    type Error = MachineAddError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineAdd;
    const RESPONSE_ALIAS: &'static str = "MachineAddResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineListApi;

impl OperationApiContract for MachineListApi {
    type Request = MachineListRequest;
    type Success = MachineListResult;
    type Error = MachineListError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineList;
    const RESPONSE_ALIAS: &'static str = "MachineListResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineInspectApi;

impl OperationApiContract for MachineInspectApi {
    type Request = MachineInspectRequest;
    type Success = MachineSnapshot;
    type Error = MachineInspectError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineInspect;
    const RESPONSE_ALIAS: &'static str = "MachineInspectResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceListApi;

impl OperationApiContract for ServiceListApi {
    type Request = ServiceListRequest;
    type Success = ServiceListResult;
    type Error = ServiceListError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::ServiceList;
    const RESPONSE_ALIAS: &'static str = "ServiceListResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceInspectApi;

impl OperationApiContract for ServiceInspectApi {
    type Request = ServiceInspectRequest;
    type Success = ServiceSnapshot;
    type Error = ServiceInspectError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::ServiceInspect;
    const RESPONSE_ALIAS: &'static str = "ServiceInspectResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogsTailApi;

impl OperationApiContract for LogsTailApi {
    type Request = LogsTailRequest;
    type Success = LogsTailResult;
    type Error = LogsTailError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::LogsTail;
    const RESPONSE_ALIAS: &'static str = "LogsTailResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineJoinRedeemApi;

impl OperationApiContract for MachineJoinRedeemApi {
    type Request = MachineJoinRedeemRequest;
    type Success = MachineJoinRedeemed;
    type Error = MachineJoinRedeemError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineJoinRedeem;
    const RESPONSE_ALIAS: &'static str = "MachineJoinRedeemResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineJoinReportApi;

impl OperationApiContract for MachineJoinReportApi {
    type Request = MachineJoinReportRequest;
    type Success = MachineJoinReported;
    type Error = MachineJoinReportError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineJoinReport;
    const RESPONSE_ALIAS: &'static str = "MachineJoinReportResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpsStatusApi;

impl OperationApiContract for OpsStatusApi {
    type Request = OpsStatusRequest;
    type Success = OperationStatusSnapshot;
    type Error = OpsStatusError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::OpsStatus;
    const RESPONSE_ALIAS: &'static str = "OpsStatusResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpsWatchApi;

impl OperationApiContract for OpsWatchApi {
    type Request = OpsWatchRequest;
    type Success = OperationEventReplayPage;
    type Error = OpsWatchError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::OpsWatch;
    const REQUEST_ALIAS: Option<&'static str> = Some("OpsWatchRequest");
    const RESPONSE_ALIAS: &'static str = "OpsWatchResponse";
}
