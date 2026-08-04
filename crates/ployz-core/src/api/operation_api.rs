//! User-facing operation API contract registry.

use crate::operation::OperationEventReplayPage;
use crate::{
    AcceptedOperation, BuildCancelError, BuildCancelRequest, BuildSubmitError, BuildSubmitRequest,
    BuildTargetCapabilities, BuildTargetCapabilitiesError, BuildTargetCapabilitiesRequest,
    DeployPreview, DeployPreviewError, DeployPreviewRequest, DeployReserveError,
    DeployReserveRequest, DeployReserved, DeploySubmitError, DeploySubmitRequest,
    IngressConfigureError, IngressConfigureRequest, LogsTailError, LogsTailRequest, LogsTailResult,
    MachineBuildCachePruneError, MachineBuildCachePruneRequest, MachineInspectError,
    MachineInspectRequest, MachineLifecycleError, MachineLifecycleRequest, MachineListError,
    MachineListRequest, MachineListResult, MachineSnapshot, MachineStoragePrepareCancelError,
    MachineStoragePrepareCancelRequest, MachineStoragePrepareError, MachineStoragePrepareRequest,
    MachineUpdateError, MachineUpdateRequest, NamespaceRemoveError, NamespaceRemoveRequest,
    NetworkRepairError, NetworkRepairRequest, NetworkResolveError, NetworkResolveRequest,
    NetworkResolveResult, NetworkStatusError, NetworkStatusRequest, NetworkStatusResult,
    OperationStatusSnapshot, OpsListError, OpsListRequest, OpsListResult, OpsStatusError,
    OpsStatusRequest, OpsWatchError, OpsWatchRequest, RuntimeSnapshotError, RuntimeSnapshotRequest,
    RuntimeSnapshotResult, ServiceInspectError, ServiceInspectRequest, ServiceListError,
    ServiceListRequest, ServiceListResult, ServiceRestartError, ServiceRestartRequest,
    ServiceSnapshot, SystemDeployRequest, VolumeCreateError, VolumeCreateRequest, VolumeListError,
    VolumeListRequest, VolumeListResult, VolumeRemoveError, VolumeRemoveRequest,
};

/// Transport-neutral identifier for one public operation API contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationApiEndpoint {
    BuildTargetCapabilities,
    BuildSubmit,
    BuildCancel,
    DeployReserve,
    DeployPreview,
    DeploySubmit,
    SystemDeploy,
    MachineBuildCachePrune,
    MachineUpdate,
    MachineStoragePrepare,
    MachineStoragePrepareCancel,
    MachineDrain,
    MachineResume,
    MachineList,
    MachineInspect,
    NetworkStatus,
    NetworkResolve,
    NetworkRepair,
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
    IngressConfigure,
}

impl OperationApiEndpoint {
    /// Stable logical name of this transport-neutral API endpoint.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::BuildTargetCapabilities => "build.target.capabilities",
            Self::BuildSubmit => "build.submit",
            Self::BuildCancel => "build.cancel",
            Self::DeployReserve => "deploy.reserve",
            Self::DeployPreview => "deploy.preview",
            Self::DeploySubmit => "deploy.submit",
            Self::SystemDeploy => "system.deploy",
            Self::MachineBuildCachePrune => "machine.build_cache_prune",
            Self::MachineUpdate => "machine.update",
            Self::MachineStoragePrepare => "machine.storage_prepare",
            Self::MachineStoragePrepareCancel => "machine.storage_prepare.cancel",
            Self::MachineDrain => "machine.drain",
            Self::MachineResume => "machine.resume",
            Self::MachineList => "machine.list",
            Self::MachineInspect => "machine.inspect",
            Self::NetworkStatus => "network.status",
            Self::NetworkResolve => "network.resolve",
            Self::NetworkRepair => "network.repair",
            Self::ServiceList => "service.list",
            Self::ServiceInspect => "service.inspect",
            Self::ServiceRestart => "service.restart",
            Self::NamespaceRemove => "namespace.remove",
            Self::VolumeList => "volume.list",
            Self::VolumeCreate => "volume.create",
            Self::VolumeRemove => "volume.remove",
            Self::RuntimeSnapshot => "runtime.snapshot",
            Self::LogsTail => "logs.tail",
            Self::OpsList => "ops.list",
            Self::OpsStatus => "ops.status",
            Self::OpsWatch => "ops.watch",
            Self::IngressConfigure => "ingress.configure",
        }
    }
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
            $crate::operation_api::BuildTargetCapabilitiesApi,
            $crate::operation_api::BuildSubmitApi,
            $crate::operation_api::BuildCancelApi,
            $crate::operation_api::DeployReserveApi,
            $crate::operation_api::DeployPreviewApi,
            $crate::operation_api::DeploySubmitApi,
            $crate::operation_api::SystemDeployApi,
            $crate::operation_api::MachineBuildCachePruneApi,
            $crate::operation_api::MachineUpdateApi,
            $crate::operation_api::MachineStoragePrepareApi,
            $crate::operation_api::MachineStoragePrepareCancelApi,
            $crate::operation_api::MachineDrainApi,
            $crate::operation_api::MachineResumeApi,
            $crate::operation_api::ServiceRestartApi,
            $crate::operation_api::NamespaceRemoveApi,
            $crate::operation_api::VolumeCreateApi,
            $crate::operation_api::VolumeRemoveApi,
            $crate::operation_api::IngressConfigureApi,
            $crate::operation_api::MachineListApi,
            $crate::operation_api::MachineInspectApi,
            $crate::operation_api::NetworkStatusApi,
            $crate::operation_api::NetworkResolveApi,
            $crate::operation_api::NetworkRepairApi,
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
pub struct BuildTargetCapabilitiesApi;

impl OperationApiContract for BuildTargetCapabilitiesApi {
    type Request = BuildTargetCapabilitiesRequest;
    type Success = BuildTargetCapabilities;
    type Error = BuildTargetCapabilitiesError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::BuildTargetCapabilities;
    const RESPONSE_ALIAS: &'static str = "BuildTargetCapabilitiesResponse";
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
pub struct SystemDeployApi;

impl OperationApiContract for SystemDeployApi {
    type Request = SystemDeployRequest;
    type Success = AcceptedOperation;
    type Error = DeploySubmitError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::SystemDeploy;
    const RESPONSE_ALIAS: &'static str = "SystemDeployResponse";
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
pub struct MachineStoragePrepareCancelApi;

impl OperationApiContract for MachineStoragePrepareCancelApi {
    type Request = MachineStoragePrepareCancelRequest;
    type Success = AcceptedOperation;
    type Error = MachineStoragePrepareCancelError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineStoragePrepareCancel;
    const RESPONSE_ALIAS: &'static str = "MachineStoragePrepareCancelResponse";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineBuildCachePruneApi;

impl OperationApiContract for MachineBuildCachePruneApi {
    type Request = MachineBuildCachePruneRequest;
    type Success = AcceptedOperation;
    type Error = MachineBuildCachePruneError;

    const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::MachineBuildCachePrune;
    const RESPONSE_ALIAS: &'static str = "MachineBuildCachePruneResponse";
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
