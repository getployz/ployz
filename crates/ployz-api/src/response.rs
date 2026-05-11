use crate::acme::AcmeHttp01StatusPayload;
use crate::build::{BuildOperationListPayload, BuildOperationPayload, BuildResultPayload};
use crate::deploy::{
    BranchEnvironmentListPayload, BranchEnvironmentPayload, DeployCandidateStartedPayload,
    DeployFailurePayload, DeployNamespaceSnapshotPayload, DeployPreparePayload,
};
use crate::doctor::DoctorPayload;
use crate::image::{
    ImageDistributePayload, ImageDistributeValidationPayload, ImageInspectPayload,
    ImageOperationListPayload, ImageOperationPayload, ImagePushPayload, ImageReceiveSessionPayload,
    ImageReceivedImportPayload, ImageStatusPayload,
};
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
    DeployPrepare(DeployPreparePayload),
    BranchEnvironment(BranchEnvironmentPayload),
    BranchEnvironmentList(BranchEnvironmentListPayload),
    DeployCandidateStarted(DeployCandidateStartedPayload),
    DeployFailure(DeployFailurePayload),
    ImageStatus(ImageStatusPayload),
    ImageInspect(ImageInspectPayload),
    ImagePush(ImagePushPayload),
    ImageDistribute(ImageDistributePayload),
    ImageDistributeValidation(ImageDistributeValidationPayload),
    ImageReceiveSession(ImageReceiveSessionPayload),
    ImageReceivedImport(ImageReceivedImportPayload),
    ImageOperation(ImageOperationPayload),
    ImageOperationList(ImageOperationListPayload),
    BuildResult(BuildResultPayload),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{
        ImageDistributeValidationFailure, ImageDistributeValidationPayload, ImageTransferFailure,
        ImageTransferFailureStage, ImageTransferTargetResult,
    };
    use ployz_types::model::{ImageDigest, MachineId};
    use std::collections::BTreeMap;

    #[test]
    fn image_receive_session_payload_preserves_endpoint_and_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("x-ployz-image-operation".into(), "image-push-1".into());
        headers.insert("x-ployz-source-machine".into(), "machine-a".into());
        headers.insert("x-ployz-image-session".into(), "token-1".into());
        let response = DaemonResponse {
            ok: true,
            code: "OK".into(),
            message: "created".into(),
            payload: Some(DaemonPayload::ImageReceiveSession(
                ImageReceiveSessionPayload {
                    target_machine: MachineId::new("machine-b"),
                    endpoint: "http://127.0.0.1:4320/v2/ployz/image-push-1".into(),
                    token: "token-1".into(),
                    expires_at_unix_secs: 1_777_646_000,
                    headers,
                },
            )),
        };

        let json = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(
            json["payload"],
            serde_json::json!({
                "kind": "image-receive-session",
                "target_machine": "machine-b",
                "endpoint": "http://127.0.0.1:4320/v2/ployz/image-push-1",
                "token": "token-1",
                "expires_at_unix_secs": 1777646000_u64,
                "headers": {
                    "x-ployz-image-operation": "image-push-1",
                    "x-ployz-source-machine": "machine-a",
                    "x-ployz-image-session": "token-1"
                }
            })
        );
    }

    #[test]
    fn image_distribute_validation_payload_serializes_branchable_context() {
        let digest =
            ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid digest");
        let response = DaemonResponse {
            ok: false,
            code: "IMAGE_DISTRIBUTE_DUPLICATE_TARGET".into(),
            message: "duplicate".into(),
            payload: Some(DaemonPayload::ImageDistributeValidation(
                ImageDistributeValidationPayload {
                    request: crate::image::ImageDistributeRequest {
                        digest: digest.clone(),
                        source_machine: MachineId::new("founder"),
                        target_machines: vec![MachineId::new("peer"), MachineId::new("peer")],
                        platform: None,
                    },
                    failure: ImageDistributeValidationFailure::DuplicateTarget {
                        duplicate_target: MachineId::new("peer"),
                    },
                },
            )),
        };

        let json = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(
            json["payload"],
            serde_json::json!({
                "kind": "image-distribute-validation",
                "digest": digest.as_str(),
                "source_machine": "founder",
                "target_machines": ["peer", "peer"],
                "failure": {
                    "type": "duplicate_target",
                    "duplicate_target": "peer"
                }
            })
        );
    }

    #[test]
    fn image_distribute_payload_serializes_target_failure_context() {
        let digest =
            ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid digest");
        let response = DaemonResponse {
            ok: false,
            code: "IMAGE_DISTRIBUTE_FAILED".into(),
            message: "failed".into(),
            payload: Some(DaemonPayload::ImageDistribute(
                crate::image::ImageDistributePayload {
                    operation_id: "image-distribute-1".into(),
                    digest: digest.clone(),
                    source_machine: MachineId::new("founder"),
                    targets: vec![ImageTransferTargetResult::failed(
                        MachineId::new("peer"),
                        ImageTransferFailure {
                            code: "IMAGE_DISTRIBUTE_SOURCE_EXPORT_FAILED".into(),
                            stage: ImageTransferFailureStage::SourceExport,
                            message: "export failed".into(),
                        },
                    )],
                },
            )),
        };

        let json = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(
            json["payload"]["targets"][0]["failure"],
            serde_json::json!({
                "code": "IMAGE_DISTRIBUTE_SOURCE_EXPORT_FAILED",
                "stage": "source_export",
                "message": "export failed"
            })
        );
    }
}
