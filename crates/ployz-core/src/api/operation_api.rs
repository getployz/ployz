//! User-facing operation API contract registry.

use crate::build::BuildTargetCapabilities;
use crate::operation::{OperationEventReplayPage, OperationStatusSnapshot};
use crate::{
    AcceptedOperation, BuildCancelError, BuildCancelRequest, BuildSubmitError, BuildSubmitRequest,
    BuildTargetCapabilitiesError, BuildTargetCapabilitiesRequest, DeployPreview,
    DeployPreviewError, DeployPreviewRequest, DeployReserveError, DeployReserveRequest,
    DeployReserved, DeploySubmitError, DeploySubmitRequest, IngressConfigureError,
    IngressConfigureRequest, LogsTailError, LogsTailRequest, LogsTailResult,
    MachineBuildCachePruneError, MachineBuildCachePruneRequest, MachineInspectError,
    MachineInspectRequest, MachineLifecycleError, MachineLifecycleRequest, MachineListError,
    MachineListRequest, MachineListResult, MachineSnapshot, MachineStoragePrepareCancelError,
    MachineStoragePrepareCancelRequest, MachineStoragePrepareError, MachineStoragePrepareRequest,
    MachineUpdateError, MachineUpdateRequest, NamespaceRemoveError, NamespaceRemoveRequest,
    NetworkRepairError, NetworkRepairRequest, NetworkResolveError, NetworkResolveRequest,
    NetworkResolveResult, NetworkStatusError, NetworkStatusRequest, NetworkStatusResult,
    OpsListError, OpsListRequest, OpsListResult, OpsStatusError, OpsStatusRequest, OpsWatchError,
    OpsWatchRequest, RuntimeSnapshotError, RuntimeSnapshotRequest, RuntimeSnapshotResult,
    ServiceInspectError, ServiceInspectRequest, ServiceListError, ServiceListRequest,
    ServiceListResult, ServiceRestartError, ServiceRestartRequest, ServiceSnapshot,
    SystemDeployRequest, VolumeCreateError, VolumeCreateRequest, VolumeListError,
    VolumeListRequest, VolumeListResult, VolumeRemoveError, VolumeRemoveRequest,
};

pub trait OperationApiContract {
    type Request;
    type Success;
    type Error;

    const ENDPOINT: OperationApiEndpoint;
    const REQUEST_ALIAS: Option<&'static str>;
    const RESPONSE_ALIAS: &'static str;
}

/// Applies a macro to the one canonical declaration of every public API contract.
#[macro_export]
macro_rules! operation_api_contracts {
    ($macro:ident) => {
        $macro! {
            BuildTargetCapabilitiesApi => {
                endpoint: BuildTargetCapabilities,
                name: "build.target.capabilities",
                request: BuildTargetCapabilitiesRequest,
                success: BuildTargetCapabilities,
                error: BuildTargetCapabilitiesError,
                request_alias: None,
                response_alias: "BuildTargetCapabilitiesResponse"
            },
            BuildSubmitApi => {
                endpoint: BuildSubmit,
                name: "build.submit",
                request: BuildSubmitRequest,
                success: AcceptedOperation,
                error: BuildSubmitError,
                request_alias: None,
                response_alias: "BuildSubmitResponse"
            },
            BuildCancelApi => {
                endpoint: BuildCancel,
                name: "build.cancel",
                request: BuildCancelRequest,
                success: AcceptedOperation,
                error: BuildCancelError,
                request_alias: None,
                response_alias: "BuildCancelResponse"
            },
            DeployReserveApi => {
                endpoint: DeployReserve,
                name: "deploy.reserve",
                request: DeployReserveRequest,
                success: DeployReserved,
                error: DeployReserveError,
                request_alias: None,
                response_alias: "DeployReserveResponse"
            },
            DeployPreviewApi => {
                endpoint: DeployPreview,
                name: "deploy.preview",
                request: DeployPreviewRequest,
                success: DeployPreview,
                error: DeployPreviewError,
                request_alias: None,
                response_alias: "DeployPreviewResponse"
            },
            DeploySubmitApi => {
                endpoint: DeploySubmit,
                name: "deploy.submit",
                request: DeploySubmitRequest,
                success: AcceptedOperation,
                error: DeploySubmitError,
                request_alias: None,
                response_alias: "DeploySubmitResponse"
            },
            SystemDeployApi => {
                endpoint: SystemDeploy,
                name: "system.deploy",
                request: SystemDeployRequest,
                success: AcceptedOperation,
                error: DeploySubmitError,
                request_alias: None,
                response_alias: "SystemDeployResponse"
            },
            MachineBuildCachePruneApi => {
                endpoint: MachineBuildCachePrune,
                name: "machine.build_cache_prune",
                request: MachineBuildCachePruneRequest,
                success: AcceptedOperation,
                error: MachineBuildCachePruneError,
                request_alias: None,
                response_alias: "MachineBuildCachePruneResponse"
            },
            MachineUpdateApi => {
                endpoint: MachineUpdate,
                name: "machine.update",
                request: MachineUpdateRequest,
                success: AcceptedOperation,
                error: MachineUpdateError,
                request_alias: None,
                response_alias: "MachineUpdateResponse"
            },
            MachineStoragePrepareApi => {
                endpoint: MachineStoragePrepare,
                name: "machine.storage_prepare",
                request: MachineStoragePrepareRequest,
                success: AcceptedOperation,
                error: MachineStoragePrepareError,
                request_alias: None,
                response_alias: "MachineStoragePrepareResponse"
            },
            MachineStoragePrepareCancelApi => {
                endpoint: MachineStoragePrepareCancel,
                name: "machine.storage_prepare.cancel",
                request: MachineStoragePrepareCancelRequest,
                success: AcceptedOperation,
                error: MachineStoragePrepareCancelError,
                request_alias: None,
                response_alias: "MachineStoragePrepareCancelResponse"
            },
            MachineDrainApi => {
                endpoint: MachineDrain,
                name: "machine.drain",
                request: MachineLifecycleRequest,
                success: AcceptedOperation,
                error: MachineLifecycleError,
                request_alias: None,
                response_alias: "MachineDrainResponse"
            },
            MachineResumeApi => {
                endpoint: MachineResume,
                name: "machine.resume",
                request: MachineLifecycleRequest,
                success: AcceptedOperation,
                error: MachineLifecycleError,
                request_alias: None,
                response_alias: "MachineResumeResponse"
            },
            ServiceRestartApi => {
                endpoint: ServiceRestart,
                name: "service.restart",
                request: ServiceRestartRequest,
                success: AcceptedOperation,
                error: ServiceRestartError,
                request_alias: None,
                response_alias: "ServiceRestartResponse"
            },
            NamespaceRemoveApi => {
                endpoint: NamespaceRemove,
                name: "namespace.remove",
                request: NamespaceRemoveRequest,
                success: AcceptedOperation,
                error: NamespaceRemoveError,
                request_alias: None,
                response_alias: "NamespaceRemoveResponse"
            },
            VolumeCreateApi => {
                endpoint: VolumeCreate,
                name: "volume.create",
                request: VolumeCreateRequest,
                success: AcceptedOperation,
                error: VolumeCreateError,
                request_alias: None,
                response_alias: "VolumeCreateResponse"
            },
            VolumeRemoveApi => {
                endpoint: VolumeRemove,
                name: "volume.remove",
                request: VolumeRemoveRequest,
                success: AcceptedOperation,
                error: VolumeRemoveError,
                request_alias: None,
                response_alias: "VolumeRemoveResponse"
            },
            IngressConfigureApi => {
                endpoint: IngressConfigure,
                name: "ingress.configure",
                request: IngressConfigureRequest,
                success: AcceptedOperation,
                error: IngressConfigureError,
                request_alias: None,
                response_alias: "IngressConfigureResponse"
            },
            MachineListApi => {
                endpoint: MachineList,
                name: "machine.list",
                request: MachineListRequest,
                success: MachineListResult,
                error: MachineListError,
                request_alias: None,
                response_alias: "MachineListResponse"
            },
            MachineInspectApi => {
                endpoint: MachineInspect,
                name: "machine.inspect",
                request: MachineInspectRequest,
                success: MachineSnapshot,
                error: MachineInspectError,
                request_alias: None,
                response_alias: "MachineInspectResponse"
            },
            NetworkStatusApi => {
                endpoint: NetworkStatus,
                name: "network.status",
                request: NetworkStatusRequest,
                success: NetworkStatusResult,
                error: NetworkStatusError,
                request_alias: None,
                response_alias: "NetworkStatusResponse"
            },
            NetworkResolveApi => {
                endpoint: NetworkResolve,
                name: "network.resolve",
                request: NetworkResolveRequest,
                success: NetworkResolveResult,
                error: NetworkResolveError,
                request_alias: None,
                response_alias: "NetworkResolveResponse"
            },
            NetworkRepairApi => {
                endpoint: NetworkRepair,
                name: "network.repair",
                request: NetworkRepairRequest,
                success: AcceptedOperation,
                error: NetworkRepairError,
                request_alias: None,
                response_alias: "NetworkRepairResponse"
            },
            ServiceListApi => {
                endpoint: ServiceList,
                name: "service.list",
                request: ServiceListRequest,
                success: ServiceListResult,
                error: ServiceListError,
                request_alias: None,
                response_alias: "ServiceListResponse"
            },
            VolumeListApi => {
                endpoint: VolumeList,
                name: "volume.list",
                request: VolumeListRequest,
                success: VolumeListResult,
                error: VolumeListError,
                request_alias: None,
                response_alias: "VolumeListResponse"
            },
            ServiceInspectApi => {
                endpoint: ServiceInspect,
                name: "service.inspect",
                request: ServiceInspectRequest,
                success: ServiceSnapshot,
                error: ServiceInspectError,
                request_alias: None,
                response_alias: "ServiceInspectResponse"
            },
            RuntimeSnapshotApi => {
                endpoint: RuntimeSnapshot,
                name: "runtime.snapshot",
                request: RuntimeSnapshotRequest,
                success: RuntimeSnapshotResult,
                error: RuntimeSnapshotError,
                request_alias: None,
                response_alias: "RuntimeSnapshotResponse"
            },
            LogsTailApi => {
                endpoint: LogsTail,
                name: "logs.tail",
                request: LogsTailRequest,
                success: LogsTailResult,
                error: LogsTailError,
                request_alias: None,
                response_alias: "LogsTailResponse"
            },
            OpsListApi => {
                endpoint: OpsList,
                name: "ops.list",
                request: OpsListRequest,
                success: OpsListResult,
                error: OpsListError,
                request_alias: None,
                response_alias: "OpsListResponse"
            },
            OpsStatusApi => {
                endpoint: OpsStatus,
                name: "ops.status",
                request: OpsStatusRequest,
                success: OperationStatusSnapshot,
                error: OpsStatusError,
                request_alias: None,
                response_alias: "OpsStatusResponse"
            },
            OpsWatchApi => {
                endpoint: OpsWatch,
                name: "ops.watch",
                request: OpsWatchRequest,
                success: OperationEventReplayPage,
                error: OpsWatchError,
                request_alias: Some("OpsWatchRequest"),
                response_alias: "OpsWatchResponse"
            }
        }
    };
}

macro_rules! define_operation_api_contracts {
    (
        $(
            $marker:ident => {
                endpoint: $endpoint:ident,
                name: $name:literal,
                request: $request:ty,
                success: $success:ty,
                error: $error:ty,
                request_alias: $request_alias:expr,
                response_alias: $response_alias:literal
            }
        ),+ $(,)?
    ) => {
        /// Transport-neutral identifier for one public operation API contract.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum OperationApiEndpoint {
            $($endpoint),+
        }

        impl OperationApiEndpoint {
            /// Every declared endpoint in stable registry order.
            pub const ALL: &'static [Self] = &[$(Self::$endpoint),+];

            /// Stable logical name of this transport-neutral API endpoint.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$endpoint => $name),+
                }
            }
        }

        $(
            #[derive(Debug, Clone, Copy, PartialEq, Eq)]
            pub struct $marker;

            impl OperationApiContract for $marker {
                type Request = $request;
                type Success = $success;
                type Error = $error;

                const ENDPOINT: OperationApiEndpoint = OperationApiEndpoint::$endpoint;
                const REQUEST_ALIAS: Option<&'static str> = $request_alias;
                const RESPONSE_ALIAS: &'static str = $response_alias;
            }
        )+
    };
}

operation_api_contracts!(define_operation_api_contracts);

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_declared_endpoint_has_one_marker_contract_and_unique_name() {
        let mut marker_endpoints = Vec::new();
        macro_rules! collect_marker_endpoints {
            (
                $(
                    $marker:ident => {
                        endpoint: $endpoint:ident,
                        name: $name:literal,
                        request: $request:ty,
                        success: $success:ty,
                        error: $error:ty,
                        request_alias: $request_alias:expr,
                        response_alias: $response_alias:literal
                    }
                ),+ $(,)?
            ) => {
                $(marker_endpoints.push(<$marker as OperationApiContract>::ENDPOINT);)+
            };
        }
        crate::operation_api_contracts!(collect_marker_endpoints);

        assert_eq!(marker_endpoints, OperationApiEndpoint::ALL);
        assert_eq!(
            OperationApiEndpoint::ALL
                .iter()
                .map(|endpoint| endpoint.name())
                .collect::<BTreeSet<_>>()
                .len(),
            OperationApiEndpoint::ALL.len()
        );
    }
}
