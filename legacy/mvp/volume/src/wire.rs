use mvp_bus::Subject;
use mvp_identity::NodeId;
use serde::{Deserialize, Serialize};

use crate::{
    VolumeError, VolumeId, VolumeNamespace, VolumeResult, VolumeSnapshotGuid, VolumeSnapshotId,
    VolumeTransferId,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSnapshotRequest {
    pub source: NodeId,
    pub target: NodeId,
    pub namespace: VolumeNamespace,
    pub volume: VolumeId,
    pub transfer_id: VolumeTransferId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeSnapshotReply {
    pub source: NodeId,
    pub transfer_id: VolumeTransferId,
    pub namespace: VolumeNamespace,
    pub volume: VolumeId,
    pub snapshot_id: VolumeSnapshotId,
    pub snapshot_guid: VolumeSnapshotGuid,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeReceiveRequest {
    pub source: NodeId,
    pub target: NodeId,
    pub namespace: VolumeNamespace,
    pub volume: VolumeId,
    pub transfer_id: VolumeTransferId,
    pub snapshot_id: VolumeSnapshotId,
    pub snapshot_guid: VolumeSnapshotGuid,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeReceiveReply {
    pub source: NodeId,
    pub target: NodeId,
    pub namespace: VolumeNamespace,
    pub volume: VolumeId,
    pub transfer_id: VolumeTransferId,
    pub snapshot_id: VolumeSnapshotId,
    pub snapshot_guid: VolumeSnapshotGuid,
    pub bytes: u64,
}

pub fn volume_snapshot_subject(node_id: &NodeId) -> VolumeResult<Subject> {
    Subject::parse(format!("node.{}.rpc.volume.snapshot", node_id.as_str()))
        .map_err(VolumeError::from)
}

pub fn volume_receive_subject(node_id: &NodeId) -> VolumeResult<Subject> {
    Subject::parse(format!("node.{}.rpc.volume.receive", node_id.as_str()))
        .map_err(VolumeError::from)
}
