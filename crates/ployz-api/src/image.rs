use ployz_types::model::{
    ImageArtifact, ImageAvailabilityRecord, ImageDigest, ImageOperationRecord, ImagePlatform,
    MachineId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStatusRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<ImageDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInspectRequest {
    pub digest: ImageDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub machines: Vec<MachineId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePushRequest {
    pub source_image: String,
    pub target_machines: Vec<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_digest: Option<ImageDigest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributeRequest {
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub target_machines: Vec<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceiveSessionRequest {
    pub operation_id: String,
    pub source_machine: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceivedImportRequest {
    pub operation_id: String,
    pub source_machine: MachineId,
    pub repository: String,
    pub reference: String,
    pub expected_digest: ImageDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repo_tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStatusPayload {
    pub records: Vec<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInspectPayload {
    pub operation_id: String,
    pub records: Vec<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePushPayload {
    pub operation_id: String,
    pub artifact: ImageArtifact,
    pub targets: Vec<ImageTransferTargetResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributePayload {
    pub operation_id: String,
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub targets: Vec<ImageTransferTargetResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributeValidationPayload {
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub target_machines: Vec<MachineId>,
    pub failure: ImageDistributeValidationFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageDistributeValidationFailure {
    TargetRequired {
        target_count: usize,
    },
    DuplicateTarget {
        duplicate_target: MachineId,
    },
    SourceNotLocal {
        source_machine: MachineId,
        local_machine: MachineId,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceiveSessionPayload {
    pub target_machine: MachineId,
    pub endpoint: String,
    pub token: String,
    pub expires_at_unix_secs: u64,
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageReceivedImportPayload {
    pub target_machine: MachineId,
    pub record: ImageAvailabilityRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOperationPayload {
    pub operation: ImageOperationRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOperationListPayload {
    pub operations: Vec<ImageOperationRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageTransferTargetResult {
    pub machine_id: MachineId,
    pub status: ImageTransferTargetStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<ImageAvailabilityRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ImageTransferFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageTransferFailure {
    pub code: String,
    pub stage: ImageTransferFailureStage,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTransferFailureStage {
    AvailabilityRead,
    SourceVerify,
    SourceExport,
    ArchiveParse,
    LocalAvailability,
    ReceiveSession,
    Upload,
    Import,
    DistributingPushedImage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTransferTargetStatus {
    Present,
    SkippedPresent,
    Failed,
}
