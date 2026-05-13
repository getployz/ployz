use crate::ipc::listener::IncomingRequest;
use ployz_api::DaemonRequest;
use ployz_model::MachineId;
use ployz_node_api::NodeRequest;

use super::super::DaemonState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLane {
    Shared,
    Exclusive,
}

impl DaemonState {
    #[must_use]
    pub fn request_lane(req: &DaemonRequest) -> RequestLane {
        match req {
            DaemonRequest::DebugTick { .. }
            | DaemonRequest::MeshJoin { .. }
            | DaemonRequest::MeshBootstrap { .. }
            | DaemonRequest::MeshInit { .. }
            | DaemonRequest::MeshStart { .. }
            | DaemonRequest::MeshStop { .. }
            | DaemonRequest::MeshDestroy { .. }
            | DaemonRequest::MachineUpdate { .. }
            | DaemonRequest::MachineStoragePromote { .. }
            | DaemonRequest::MachineRemove { .. } => RequestLane::Exclusive,
            DaemonRequest::Ping
            | DaemonRequest::Status
            | DaemonRequest::Doctor
            | DaemonRequest::DeployPreview { .. }
            | DaemonRequest::DeployPrepare { .. }
            | DaemonRequest::DeployApply { .. }
            | DaemonRequest::DeployApplyPrepared { .. }
            | DaemonRequest::DeployExport { .. }
            | DaemonRequest::BranchNamespace { .. }
            | DaemonRequest::BranchApplyPrepared { .. }
            | DaemonRequest::BranchEnvironmentStatus { .. }
            | DaemonRequest::BranchEnvironmentList
            | DaemonRequest::MigrateService { .. }
            | DaemonRequest::ImageStatus { .. }
            | DaemonRequest::ImageInspect { .. }
            | DaemonRequest::ImagePush { .. }
            | DaemonRequest::ImageDistribute { .. }
            | DaemonRequest::ImageReceiveSession { .. }
            | DaemonRequest::ImageReceivedImport { .. }
            | DaemonRequest::ImageOperationGet { .. }
            | DaemonRequest::ImageOperationList
            | DaemonRequest::BuildLocal { .. }
            | DaemonRequest::BuildMachine { .. }
            | DaemonRequest::BuildOperationGet { .. }
            | DaemonRequest::BuildOperationList
            | DaemonRequest::RuntimeSubscribe
            | DaemonRequest::VolumeZfsInspect { .. }
            | DaemonRequest::VolumeZfsSnapshot { .. }
            | DaemonRequest::VolumeZfsSend { .. }
            | DaemonRequest::VolumeZfsTransferGet { .. }
            | DaemonRequest::VolumeZfsTransferList
            | DaemonRequest::MeshList
            | DaemonRequest::MeshStatus { .. }
            | DaemonRequest::MeshReady { .. }
            | DaemonRequest::MeshCreate { .. }
            | DaemonRequest::MachineList
            | DaemonRequest::MachineRtt
            | DaemonRequest::MeshPeerRttSnapshot
            | DaemonRequest::MachineInit { .. }
            | DaemonRequest::MachineAdd { .. }
            | DaemonRequest::MachineActivate { .. }
            | DaemonRequest::MachineDrain { .. }
            | DaemonRequest::MachineStandby { .. }
            | DaemonRequest::MachineOperationList
            | DaemonRequest::MachineOperationGet { .. }
            | DaemonRequest::MachineInviteCreate { .. }
            | DaemonRequest::MachineInviteRevoke { .. }
            | DaemonRequest::MachineInviteList
            | DaemonRequest::MachineInviteImport { .. }
            | DaemonRequest::AcmeChallengeReady { .. }
            | DaemonRequest::AcmeHttp01Status { .. }
            | DaemonRequest::MeshSelfRecord => RequestLane::Shared,
        }
    }

    #[must_use]
    pub fn request_lane_for_state(&self, req: &DaemonRequest) -> RequestLane {
        match req {
            DaemonRequest::MachineDrain { target }
            | DaemonRequest::MachineStandby { target, .. }
                if MachineId::try_new(target.as_str())
                    .map(|target| target == self.identity.machine_id)
                    .unwrap_or(false) =>
            {
                RequestLane::Exclusive
            }
            _ => Self::request_lane(req),
        }
    }

    #[must_use]
    pub fn node_request_lane(req: &NodeRequest) -> RequestLane {
        match req {
            NodeRequest::MeshPeerPrepareDestroy { .. }
            | NodeRequest::MeshPeerCancelDestroy { .. }
            | NodeRequest::MeshPeerExecuteDestroy { .. }
            | NodeRequest::MeshPeerPrepareUpdate { .. }
            | NodeRequest::MeshPeerExecuteUpdate { .. }
            | NodeRequest::MeshPeerRemoveMachine { .. }
            | NodeRequest::MachineTransitionSelf { .. }
            | NodeRequest::MachineStoragePromoteSelf { .. }
            | NodeRequest::MachineStorageRestoreSelf { .. } => RequestLane::Exclusive,
            NodeRequest::Ping
            | NodeRequest::Status
            | NodeRequest::MeshReady { .. }
            | NodeRequest::MeshSelfRecord
            | NodeRequest::MachineOperationGet { .. }
            | NodeRequest::DeployNodeInspectNamespace { .. }
            | NodeRequest::DeployNodeStartCandidate { .. }
            | NodeRequest::DeployNodeDrainInstance { .. }
            | NodeRequest::DeployNodeRemoveInstance { .. }
            | NodeRequest::DeployNodeCloneVolume { .. }
            | NodeRequest::DeployNodeCleanupUncommittedVolumeClone { .. }
            | NodeRequest::VolumeZfsInspect { .. }
            | NodeRequest::VolumeZfsSnapshot { .. }
            | NodeRequest::VolumeZfsSend { .. }
            | NodeRequest::VolumeZfsPeerSnapshot { .. }
            | NodeRequest::VolumeZfsPeerSnapshotGuid { .. }
            | NodeRequest::VolumeZfsPeerStartSend { .. }
            | NodeRequest::VolumeZfsTransferGet { .. }
            | NodeRequest::ImageDistribute { .. }
            | NodeRequest::ImageReceiveSession { .. }
            | NodeRequest::ImageReceivedImport { .. } => RequestLane::Shared,
        }
    }

    #[must_use]
    pub fn request_lane_for_command(&self, req: &IncomingRequest) -> RequestLane {
        match req {
            IncomingRequest::Control(request) => self.request_lane_for_state(request),
            IncomingRequest::Node(request) => Self::node_request_lane(request),
        }
    }
}
