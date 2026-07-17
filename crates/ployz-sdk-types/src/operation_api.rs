//! User-facing operation API contract registry.

use crate::{
    AcceptedOperation, BuildCancelError, BuildCancelRequest, BuildSubmitError, BuildSubmitRequest,
    CoreReplaceError, CoreReplaceReportError, CoreReplaceReportRequest, CoreReplaceReported,
    CoreReplaceRequest, CredentialAddError, CredentialAddRequest, CredentialListError,
    CredentialListRequest, CredentialListResult, CredentialRemoveError, CredentialRemoveRequest,
    DeployPreview, DeployPreviewError, DeployPreviewRequest, DeployReserveError,
    DeployReserveRequest, DeployReserved, DeploySubmitError, DeploySubmitRequest,
    IngressConfigureError, IngressConfigureRequest, InitFirstMachineActivateError,
    InitFirstMachineActivateRequest, InitFirstMachineActivated, LogsTailError, LogsTailRequest,
    LogsTailResult, MachineAddAccepted, MachineAddError, MachineAddRequest, MachineInspectError,
    MachineInspectRequest, MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed,
    MachineJoinReportError, MachineJoinReportRequest, MachineJoinReported, MachineLifecycleError,
    MachineLifecycleRequest, MachineListError, MachineListRequest, MachineListResult,
    MachineSnapshot, MachineStoragePrepareError, MachineStoragePrepareRequest, MachineUpdateError,
    MachineUpdateRequest, NamespaceRemoveError, NamespaceRemoveRequest, NetworkRepairError,
    NetworkRepairRequest, NetworkResolveError, NetworkResolveRequest, NetworkResolveResult,
    NetworkStatusError, NetworkStatusRequest, NetworkStatusResult, OperationStatusSnapshot,
    OpsListError, OpsListRequest, OpsListResult, OpsStatusError, OpsStatusRequest, OpsWatchError,
    OpsWatchRequest, RuntimeSnapshotError, RuntimeSnapshotRequest, RuntimeSnapshotResult,
    ServiceInspectError, ServiceInspectRequest, ServiceListError, ServiceListRequest,
    ServiceListResult, ServiceRestartError, ServiceRestartRequest, ServiceSnapshot,
    VolumeCreateError, VolumeCreateRequest, VolumeListError, VolumeListRequest, VolumeListResult,
    VolumeRemoveError, VolumeRemoveRequest,
};
use ployz_core::operation::OperationEventReplayPage;

/// Transport-neutral identifier for one public operation API contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    BuildSubmit,
    BuildCancel,
    DeployReserve,
    DeployPreview,
    DeploySubmit,
    InitFirstMachineActivate,
    MachineAdd,
    MachineUpdate,
    MachineStoragePrepare,
    MachineDrain,
    MachineResume,
    MachineList,
    MachineInspect,
    NetworkStatus,
    NetworkResolve,
    NetworkRepair,
    MachineJoinRedeem,
    MachineJoinReport,
    ServiceList,
    ServiceInspect,
    ServiceRestart,
    NamespaceRemove,
    VolumeList,
    VolumeCreate,
    VolumeRemove,
    RuntimeSnapshot,
    LogsTail,
    OpsList,
    OpsStatus,
    OpsWatch,
    CoreReplace,
    CoreReplaceReport,
    CredentialAdd,
    CredentialList,
    CredentialRemove,
    IngressConfigure,
}

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
            $crate::operation_api::BuildSubmitApi,
            $crate::operation_api::BuildCancelApi,
            $crate::operation_api::DeployReserveApi,
            $crate::operation_api::DeployPreviewApi,
            $crate::operation_api::DeploySubmitApi,
            $crate::operation_api::InitFirstMachineActivateApi,
            $crate::operation_api::MachineAddApi,
            $crate::operation_api::MachineUpdateApi,
            $crate::operation_api::MachineStoragePrepareApi,
            $crate::operation_api::MachineDrainApi,
            $crate::operation_api::MachineResumeApi,
            $crate::operation_api::ServiceRestartApi,
            $crate::operation_api::NamespaceRemoveApi,
            $crate::operation_api::VolumeCreateApi,
            $crate::operation_api::VolumeRemoveApi,
            $crate::operation_api::CoreReplaceApi,
            $crate::operation_api::CoreReplaceReportApi,
            $crate::operation_api::CredentialAddApi,
            $crate::operation_api::CredentialListApi,
            $crate::operation_api::CredentialRemoveApi,
            $crate::operation_api::IngressConfigureApi,
            $crate::operation_api::MachineListApi,
            $crate::operation_api::MachineInspectApi,
            $crate::operation_api::NetworkStatusApi,
            $crate::operation_api::NetworkResolveApi,
            $crate::operation_api::NetworkRepairApi,
            $crate::operation_api::MachineJoinRedeemApi,
            $crate::operation_api::MachineJoinReportApi,
            $crate::operation_api::ServiceListApi,
            $crate::operation_api::VolumeListApi,
            $crate::operation_api::ServiceInspectApi,
            $crate::operation_api::RuntimeSnapshotApi,
            $crate::operation_api::LogsTailApi,
            $crate::operation_api::OpsListApi,
            $crate::operation_api::OpsStatusApi,
            $crate::operation_api::OpsWatchApi
        );
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildSubmitApi;

impl OperationApiContract for BuildSubmitApi {
    type Request = BuildSubmitRequest;
    type Success = AcceptedOperation;
    type Error = BuildSubmitError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::BuildSubmit;
    const RESPONSE_ALIAS: &'static str = "BuildSubmitResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildCancelApi;

impl OperationApiContract for BuildCancelApi {
    type Request = BuildCancelRequest;
    type Success = AcceptedOperation;
    type Error = BuildCancelError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::BuildCancel;
    const RESPONSE_ALIAS: &'static str = "BuildCancelResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeployReserveApi;

impl OperationApiContract for DeployReserveApi {
    type Request = DeployReserveRequest;
    type Success = DeployReserved;
    type Error = DeployReserveError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::DeployReserve;
    const RESPONSE_ALIAS: &'static str = "DeployReserveResponse";
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
pub struct DeployPreviewApi;

impl OperationApiContract for DeployPreviewApi {
    type Request = DeployPreviewRequest;
    type Success = DeployPreview;
    type Error = DeployPreviewError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::DeployPreview;
    const RESPONSE_ALIAS: &'static str = "DeployPreviewResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitFirstMachineActivateApi;

impl OperationApiContract for InitFirstMachineActivateApi {
    type Request = InitFirstMachineActivateRequest;
    type Success = InitFirstMachineActivated;
    type Error = InitFirstMachineActivateError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::InitFirstMachineActivate;
    const RESPONSE_ALIAS: &'static str = "InitFirstMachineActivateResponse";
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
pub struct MachineUpdateApi;

impl OperationApiContract for MachineUpdateApi {
    type Request = MachineUpdateRequest;
    type Success = AcceptedOperation;
    type Error = MachineUpdateError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineUpdate;
    const RESPONSE_ALIAS: &'static str = "MachineUpdateResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineStoragePrepareApi;

impl OperationApiContract for MachineStoragePrepareApi {
    type Request = MachineStoragePrepareRequest;
    type Success = AcceptedOperation;
    type Error = MachineStoragePrepareError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineStoragePrepare;
    const RESPONSE_ALIAS: &'static str = "MachineStoragePrepareResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineDrainApi;

impl OperationApiContract for MachineDrainApi {
    type Request = MachineLifecycleRequest;
    type Success = AcceptedOperation;
    type Error = MachineLifecycleError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineDrain;
    const RESPONSE_ALIAS: &'static str = "MachineDrainResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineResumeApi;

impl OperationApiContract for MachineResumeApi {
    type Request = MachineLifecycleRequest;
    type Success = AcceptedOperation;
    type Error = MachineLifecycleError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineResume;
    const RESPONSE_ALIAS: &'static str = "MachineResumeResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreReplaceApi;

impl OperationApiContract for CoreReplaceApi {
    type Request = CoreReplaceRequest;
    type Success = AcceptedOperation;
    type Error = CoreReplaceError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::CoreReplace;
    const RESPONSE_ALIAS: &'static str = "CoreReplaceResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialAddApi;

impl OperationApiContract for CredentialAddApi {
    type Request = CredentialAddRequest;
    type Success = AcceptedOperation;
    type Error = CredentialAddError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::CredentialAdd;
    const RESPONSE_ALIAS: &'static str = "CredentialAddResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialListApi;

impl OperationApiContract for CredentialListApi {
    type Request = CredentialListRequest;
    type Success = CredentialListResult;
    type Error = CredentialListError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::CredentialList;
    const RESPONSE_ALIAS: &'static str = "CredentialListResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialRemoveApi;

impl OperationApiContract for CredentialRemoveApi {
    type Request = CredentialRemoveRequest;
    type Success = AcceptedOperation;
    type Error = CredentialRemoveError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::CredentialRemove;
    const RESPONSE_ALIAS: &'static str = "CredentialRemoveResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressConfigureApi;

impl OperationApiContract for IngressConfigureApi {
    type Request = IngressConfigureRequest;
    type Success = AcceptedOperation;
    type Error = IngressConfigureError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::IngressConfigure;
    const RESPONSE_ALIAS: &'static str = "IngressConfigureResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceRestartApi;

impl OperationApiContract for ServiceRestartApi {
    type Request = ServiceRestartRequest;
    type Success = AcceptedOperation;
    type Error = ServiceRestartError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::ServiceRestart;
    const RESPONSE_ALIAS: &'static str = "ServiceRestartResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceRemoveApi;

impl OperationApiContract for NamespaceRemoveApi {
    type Request = NamespaceRemoveRequest;
    type Success = AcceptedOperation;
    type Error = NamespaceRemoveError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::NamespaceRemove;
    const RESPONSE_ALIAS: &'static str = "NamespaceRemoveResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeRemoveApi;

impl OperationApiContract for VolumeRemoveApi {
    type Request = VolumeRemoveRequest;
    type Success = AcceptedOperation;
    type Error = VolumeRemoveError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::VolumeRemove;
    const RESPONSE_ALIAS: &'static str = "VolumeRemoveResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeCreateApi;

impl OperationApiContract for VolumeCreateApi {
    type Request = VolumeCreateRequest;
    type Success = AcceptedOperation;
    type Error = VolumeCreateError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::VolumeCreate;
    const RESPONSE_ALIAS: &'static str = "VolumeCreateResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreReplaceReportApi;

impl OperationApiContract for CoreReplaceReportApi {
    type Request = CoreReplaceReportRequest;
    type Success = CoreReplaceReported;
    type Error = CoreReplaceReportError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::CoreReplaceReport;
    const RESPONSE_ALIAS: &'static str = "CoreReplaceReportResponse";
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
pub struct NetworkResolveApi;

impl OperationApiContract for NetworkResolveApi {
    type Request = NetworkResolveRequest;
    type Success = NetworkResolveResult;
    type Error = NetworkResolveError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::NetworkResolve;
    const RESPONSE_ALIAS: &'static str = "NetworkResolveResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkStatusApi;

impl OperationApiContract for NetworkStatusApi {
    type Request = NetworkStatusRequest;
    type Success = NetworkStatusResult;
    type Error = NetworkStatusError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::NetworkStatus;
    const RESPONSE_ALIAS: &'static str = "NetworkStatusResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkRepairApi;

impl OperationApiContract for NetworkRepairApi {
    type Request = NetworkRepairRequest;
    type Success = AcceptedOperation;
    type Error = NetworkRepairError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::NetworkRepair;
    const RESPONSE_ALIAS: &'static str = "NetworkRepairResponse";
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
pub struct VolumeListApi;

impl OperationApiContract for VolumeListApi {
    type Request = VolumeListRequest;
    type Success = VolumeListResult;
    type Error = VolumeListError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::VolumeList;
    const RESPONSE_ALIAS: &'static str = "VolumeListResponse";
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
pub struct RuntimeSnapshotApi;

impl OperationApiContract for RuntimeSnapshotApi {
    type Request = RuntimeSnapshotRequest;
    type Success = RuntimeSnapshotResult;
    type Error = RuntimeSnapshotError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::RuntimeSnapshot;
    const RESPONSE_ALIAS: &'static str = "RuntimeSnapshotResponse";
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
pub struct OpsListApi;

impl OperationApiContract for OpsListApi {
    type Request = OpsListRequest;
    type Success = OpsListResult;
    type Error = OpsListError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::OpsList;
    const RESPONSE_ALIAS: &'static str = "OpsListResponse";
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
