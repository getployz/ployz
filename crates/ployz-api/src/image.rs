use ployz_types::model::{
    ImageArtifact, ImageAvailabilityRecord, ImageDigest, ImageOperationRecord, MachineId,
};
use serde::{Deserialize, Serialize};

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub machines: Vec<MachineId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePushRequest {
    pub artifact: ImageArtifact,
    pub target_machine: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributeRequest {
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub target_machines: Vec<MachineId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageStatusPayload {
    pub records: Vec<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInspectPayload {
    pub records: Vec<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePushPayload {
    pub operation_id: String,
    pub record: ImageAvailabilityRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageDistributePayload {
    pub operation_id: String,
    pub digest: ImageDigest,
    pub source_machine: MachineId,
    pub targets: Vec<ImageTransferTargetResult>,
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
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageTransferTargetStatus {
    Present,
    SkippedPresent,
    Failed,
}
