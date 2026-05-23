mod command;
mod domain;
mod error;
mod facts;
mod wire;

pub use command::{
    BusVolumeParticipantClient, VolumeFactWriteOutcome, VolumeFactWriter, VolumeParticipantClient,
    VolumeSourceCleanup, VolumeTransferCommand, VolumeTransferExecutor, VolumeTransferResult,
    VolumeTransferTimeouts, decode_payload, encode_payload,
};
pub use domain::{
    VolumeId, VolumeNamespace, VolumeSnapshotGuid, VolumeSnapshotId, VolumeTransferId,
};
pub use error::{VolumeError, VolumeResult};
pub use facts::{
    VolumeOwnershipCandidate, VolumeOwnershipEvidence, VolumeOwnershipFact,
    VolumeOwnershipLeaseEvidence, VolumeOwnershipReadModel, current_volume_owner,
    current_volume_ownership, volume_ownership_fact_key,
};
pub use wire::{
    VolumeReceiveReply, VolumeReceiveRequest, VolumeSnapshotReply, VolumeSnapshotRequest,
    volume_receive_subject, volume_snapshot_subject,
};

#[cfg(test)]
mod tests;
