use crate::acme::AcmeHttp01StatusPayload;
use crate::build::{BuildOperationListPayload, BuildOperationPayload};
use crate::deploy::{
    DeployCandidateStartedPayload, DeployFailurePayload, DeployNamespaceSnapshotPayload,
};
use crate::doctor::DoctorPayload;
use crate::image::{ImageOperationListPayload, ImageOperationPayload, ImageStatusPayload};
use crate::machine::{
    MachineAddPayload, MachineInviteListPayload, MachineListPayload, MachineOperationListPayload,
    MachineOperationPayload, MachineRemovePayload, MachineRttPayload,
    MachineStoragePromotionPayload, MachineUpdatePayload,
};
use crate::mesh::{MeshListPayload, MeshReadyPayload, MeshSelfRecordPayload, MeshStatusPayload};
use crate::runtime::RuntimeStatePayload;
use crate::status::StatusPayload;
use crate::volume::{
    VolumeZfsClonePayload, VolumeZfsInspectPayload, VolumeZfsPeerSendPayload,
    VolumeZfsSnapshotPayload, VolumeZfsTransferListPayload, VolumeZfsTransferPayload,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DaemonPayload {
    Doctor(DoctorPayload),
    Status(StatusPayload),
    MachineList(MachineListPayload),
    MachineRtt(MachineRttPayload),
    MachineAdd(MachineAddPayload),
    MachineStoragePromotion(MachineStoragePromotionPayload),
    MachineUpdate(MachineUpdatePayload),
    MachineRemove(MachineRemovePayload),
    MeshList(MeshListPayload),
    MeshStatus(MeshStatusPayload),
    MeshReady(MeshReadyPayload),
    MeshSelfRecord(MeshSelfRecordPayload),
    MachineInviteList(MachineInviteListPayload),
    MachineOperationList(MachineOperationListPayload),
    MachineOperation(MachineOperationPayload),
    AcmeHttp01Status(AcmeHttp01StatusPayload),
    DeployNamespaceSnapshot(DeployNamespaceSnapshotPayload),
    DeployCandidateStarted(DeployCandidateStartedPayload),
    DeployFailure(DeployFailurePayload),
    ImageStatus(ImageStatusPayload),
    ImageOperation(ImageOperationPayload),
    ImageOperationList(ImageOperationListPayload),
    BuildOperation(BuildOperationPayload),
    BuildOperationList(BuildOperationListPayload),
    VolumeZfsInspect(VolumeZfsInspectPayload),
    VolumeZfsSnapshot(VolumeZfsSnapshotPayload),
    VolumeZfsClone(VolumeZfsClonePayload),
    VolumeZfsPeerSend(VolumeZfsPeerSendPayload),
    VolumeZfsTransfer(VolumeZfsTransferPayload),
    VolumeZfsTransferList(VolumeZfsTransferListPayload),
    RuntimeState(RuntimeStatePayload),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<DaemonPayload>,
}
