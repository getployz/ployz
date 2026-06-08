//! User-facing operation API contract registry.

use crate::{
    AcceptedOperation, BackupCreateError, BackupCreateRequest, DeploySubmitError,
    DeploySubmitRequest, MachineAddAccepted, MachineAddError, MachineAddRequest,
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinReportError,
    MachineJoinReportRequest, MachineJoinReported, OperationStatusSnapshot, OpsStatusError,
    OpsStatusRequest, OpsWatchError, OpsWatchRequest,
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
            $crate::operation_api::MachineJoinRedeemApi,
            $crate::operation_api::MachineJoinReportApi,
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
