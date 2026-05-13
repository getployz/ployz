use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetSpec {
    pub dataset: String,
    pub mountpoint: PathBuf,
    pub quota: String,
    pub mode: String,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneMetadata {
    pub deploy_id: String,
    pub namespace: String,
    pub volume: String,
    pub source_namespace: String,
    pub source_volume: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    pub mountpoint: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetInspection {
    pub dataset: String,
    pub quota: String,
    pub mountpoint: PathBuf,
    pub used_bytes: u64,
    pub snapshots: Vec<SnapshotInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub name: String,
    pub guid: u64,
}
