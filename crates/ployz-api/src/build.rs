use ployz_types::model::{
    BuildMethod, BuildOperationRecord, ImageArtifact, ImageAvailabilityRecord, ImagePlatform,
    MachineId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildLocalRequest {
    pub method: BuildMethod,
    pub context_dir: String,
    pub image_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_target: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub distribute_targets: Vec<MachineId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildMachineRequest {
    pub method: BuildMethod,
    pub context_path: String,
    pub image_name: String,
    pub machine_id: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<ImagePlatform>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub build_args: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResultPayload {
    pub operation_id: String,
    pub artifact: ImageArtifact,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<ImageAvailabilityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOperationPayload {
    pub operation: BuildOperationRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOperationListPayload {
    pub operations: Vec<BuildOperationRecord>,
}
