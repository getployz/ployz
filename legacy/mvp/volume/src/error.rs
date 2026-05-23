use mvp_bus::{BusError, FactKey, PrincipalId, SubjectParseError};
use mvp_identity::NodeId;
use mvp_lease::{LeaseContentHash, LeaseEpoch, LeaseHolder, LeaseResource};
use mvp_projection::{CandidateStatus, FactSourceError};
use thiserror::Error;

use crate::{VolumeId, VolumeNamespace, VolumeSnapshotGuid, VolumeSnapshotId, VolumeTransferId};

pub type VolumeResult<T> = Result<T, VolumeError>;

#[derive(Debug, Error)]
pub enum VolumeError {
    #[error("{label} is invalid: {value:?}")]
    InvalidIdentifier { label: &'static str, value: String },
    #[error("volume fact key has the wrong shape: {key}")]
    WrongFactKeyShape { key: FactKey },
    #[error("volume fact key {key} does not match payload volume {namespace}/{volume}")]
    FactKeyPayloadMismatch {
        key: FactKey,
        namespace: VolumeNamespace,
        volume: VolumeId,
    },
    #[error("volume ownership candidate {key} is not readable: {status:?}")]
    UnreadableOwnershipCandidate {
        key: FactKey,
        status: CandidateStatus,
    },
    #[error("volume ownership candidate {key} is missing payload bytes")]
    MissingOwnershipPayload { key: FactKey },
    #[error("volume ownership payload decode failed for {key}: {message}")]
    DecodeOwnershipPayload { key: FactKey, message: String },
    #[error("volume {namespace}/{volume} has no current owner")]
    MissingCurrentOwner {
        namespace: VolumeNamespace,
        volume: VolumeId,
    },
    #[error("volume lease conflict with {holder} at epoch {epoch}")]
    LeaseConflict {
        holder: LeaseHolder,
        epoch: LeaseEpoch,
    },
    #[error("volume lease {resource} is stale for {holder} at epoch {epoch}")]
    StaleLease {
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
    },
    #[error(
        "volume {namespace}/{volume} ownership changed before commit: expected epoch {expected_epoch}, actual epoch {actual_epoch:?}"
    )]
    StaleOwnership {
        namespace: VolumeNamespace,
        volume: VolumeId,
        expected_epoch: u64,
        actual_epoch: Option<u64>,
    },
    #[error("volume lease candidate {key} is not readable: {status:?}")]
    UnreadableLeaseCandidate {
        key: FactKey,
        status: CandidateStatus,
    },
    #[error("volume lease candidate {key} is missing payload bytes")]
    MissingLeasePayload { key: FactKey },
    #[error("volume lease candidate {key} payload does not match its key")]
    MalformedLeaseCandidate { key: FactKey },
    #[error("volume lease epoch overflow")]
    EpochOverflow,
    #[error("volume ownership epoch overflow for fact {key}")]
    OwnershipEpochOverflow { key: FactKey },
    #[error("principal {principal} cannot write fact {key}")]
    UnauthorizedFactWrite {
        principal: PrincipalId,
        key: FactKey,
    },
    #[error("volume fact already has a conflicting candidate: {key}")]
    FactConflict { key: FactKey },
    #[error("snapshot reply came from {actual}, expected {expected}")]
    SnapshotSourceMismatch { expected: NodeId, actual: NodeId },
    #[error("receive reply came from {actual}, expected {expected}")]
    ReceiveTargetMismatch { expected: NodeId, actual: NodeId },
    #[error("snapshot evidence mismatch for transfer {transfer_id}")]
    SnapshotEvidenceMismatch { transfer_id: VolumeTransferId },
    #[error("receive evidence mismatch for transfer {transfer_id}")]
    ReceiveEvidenceMismatch { transfer_id: VolumeTransferId },
    #[error("received snapshot {snapshot_id}/{snapshot_guid} does not match source evidence")]
    ReceivedSnapshotMismatch {
        snapshot_id: VolumeSnapshotId,
        snapshot_guid: VolumeSnapshotGuid,
    },
    #[error("volume fact serialization failed: {message}")]
    Serialization { message: String },
    #[error("volume fact write failed: {message}")]
    FactWrite { message: String },
    #[error("volume command projection payload {operation} failed: {message}")]
    ProjectionPayload {
        operation: &'static str,
        message: String,
    },
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Subject(#[from] SubjectParseError),
    #[error(transparent)]
    FactSource(#[from] FactSourceError),
}
