use ployz_types::model::MachineId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsInspectPayload {
    pub namespace: String,
    pub volume: String,
    pub machine_id: MachineId,
    pub dataset: String,
    pub mountpoint: String,
    pub quota: String,
    pub used_bytes: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub snapshots: Vec<VolumeZfsSnapshotInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsSnapshotPayload {
    pub namespace: String,
    pub volume: String,
    pub machine_id: MachineId,
    pub dataset: String,
    pub snapshot: String,
    pub guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsClonePayload {
    pub namespace: String,
    pub volume: String,
    pub source_namespace: String,
    pub source_volume: String,
    pub machine_id: MachineId,
    pub source_dataset: String,
    pub target_dataset: String,
    pub snapshot: String,
    pub guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsSnapshotInfo {
    pub name: String,
    pub guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsPeerSendPayload {
    pub bytes_transferred: u64,
    pub snapshot_guid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsTransferPayload {
    pub transfer: VolumeZfsTransferInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsTransferListPayload {
    pub transfers: Vec<VolumeZfsTransferInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeZfsTransferInfo {
    pub id: String,
    pub namespace: String,
    pub volume: String,
    pub source_machine: MachineId,
    pub target_machine: MachineId,
    pub status: String,
    pub stage: String,
    pub snapshot_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_guid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_snapshot_guid: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_transferred: Option<u64>,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}
