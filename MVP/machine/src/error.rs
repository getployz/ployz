use mvp_bus::{BusError, FactKey, FactKeyParseError, IslandId, PrincipalId, SubjectParseError};
use mvp_identity::NodeId;
use mvp_mesh::MeshError;
use mvp_routing::{RoutingError, ServingCommitId};
use thiserror::Error;

use crate::wire::PrepareRemoveOutcome;

pub type MachineRemoveResult<T> = Result<T, MachineRemoveError>;

#[derive(Debug, Error)]
pub enum MachineRemoveError {
    #[error("machine remove target is missing: {node_id}")]
    TargetMissing { node_id: NodeId },
    #[error("machine remove target is already tombstoned: {node_id} at epoch {epoch}")]
    TargetTombstoned { node_id: NodeId, epoch: u64 },
    #[error("machine remove target is already being removed: {node_id} at epoch {epoch}")]
    TargetAlreadyRemoving {
        node_id: NodeId,
        epoch: u64,
        reason: String,
    },
    #[error("serving commit {serving_commit_id} is invalid for removing {node_id}: {reason:?}")]
    InvalidServingCommit {
        serving_commit_id: ServingCommitId,
        node_id: NodeId,
        reason: crate::MachineRemoveValidationError,
    },
    #[error("target {node_id} did not finish prepare remove: {outcome:?}")]
    PrepareRemoveRejected {
        node_id: NodeId,
        outcome: PrepareRemoveOutcome,
    },
    #[error("participant {operation} replied for {actual_node_id}, expected {expected_node_id}")]
    ParticipantNodeMismatch {
        operation: &'static str,
        expected_node_id: NodeId,
        actual_node_id: NodeId,
    },
    #[error("machine fact already has a conflicting candidate: {key}")]
    FactConflict { key: FactKey },
    #[error("principal {principal} is not allowed to write machine fact {key} in island {island}")]
    UnauthorizedFactWrite {
        island: IslandId,
        principal: PrincipalId,
        key: FactKey,
    },
    #[error("machine fact author principal {author} does not match session principal {session}")]
    PrincipalMismatch {
        session: PrincipalId,
        author: PrincipalId,
    },
    #[error("principal {principal} in island {island} has no trusted machine fact author key")]
    UntrustedAuthorKey {
        island: IslandId,
        principal: PrincipalId,
    },
    #[error("principal {principal} in island {island} used the wrong machine fact author key")]
    AuthorKeyMismatch {
        island: IslandId,
        principal: PrincipalId,
    },
    #[error("machine fact store {operation} failed: {message}")]
    FactStore {
        operation: &'static str,
        message: String,
    },
    #[error("invalid wire payload: {context}: {source}")]
    WirePayload {
        context: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Mesh(#[from] MeshError),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error(transparent)]
    SubjectParse(#[from] SubjectParseError),
    #[error(transparent)]
    FactKeyParse(#[from] FactKeyParseError),
}
