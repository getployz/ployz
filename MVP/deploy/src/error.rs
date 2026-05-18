use mvp_bus::{
    BusError, FactContentHash, FactKey, FactKeyParseError, PrincipalId, SubjectParseError,
};
use mvp_identity::NodeId;
use mvp_projection::FactSourceError;
use mvp_routing::RoutingError;
use thiserror::Error;

use crate::{
    CapacityRejectionReason, DeployId, InstanceId, InstanceNotReadyReason, PhaseId, ServingCommitId,
};

pub type DeployResult<T> = Result<T, DeployError>;

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("deploy {deploy_id} has no phases")]
    EmptyManifest { deploy_id: DeployId },
    #[error("phase {phase:?} is not ready")]
    PhaseNotReady { phase: PhaseId },
    #[error("serving commit already exists")]
    ServingCommitAlreadyExists,
    #[error("serving commit is required before drain")]
    ServingCommitRequired,
    #[error("projection catch-up proof is missing")]
    ProjectionCatchUpMissing,
    #[error("projection catch-up proof does not match serving commit {serving_commit_id}")]
    ProjectionCatchUpMismatch { serving_commit_id: ServingCommitId },
    #[error("deploy is blocked after an irreversible phase")]
    BlockedAfterIrreversiblePhase,
    #[error("deploy is still running")]
    DeployStillRunning,
    #[error("serving commit phase must be present and final")]
    ServingCommitPhaseRequired,
    #[error("serving fact already has a conflicting candidate: {key}")]
    ServingFactConflict { key: FactKey },
    #[error("serving fact is missing: {key}")]
    ServingFactMissing { key: FactKey },
    #[error("serving fact payload was not a serving commit: {key}")]
    ServingFactKindMismatch { key: FactKey },
    #[error("serving fact payload does not match serving commit {serving_commit_id}")]
    ServingFactMismatch { serving_commit_id: ServingCommitId },
    #[error(
        "deploy fact already has a conflicting candidate at {key} by {principal} with {content_hash}"
    )]
    DeployFactConflict {
        key: FactKey,
        principal: PrincipalId,
        content_hash: FactContentHash,
    },
    #[error("deploy fact is missing: {key}")]
    DeployFactMissing { key: FactKey },
    #[error("deploy fact payload was not a {expected_kind}: {key}")]
    DeployFactKindMismatch {
        key: FactKey,
        expected_kind: &'static str,
    },
    #[error("deploy fact payload does not match deploy {deploy_id}")]
    DeployFactMismatch { deploy_id: DeployId },
    #[error("planned node was not visible at decision time: {node_id}")]
    PlannedNodeNotVisible { node_id: NodeId },
    #[error("planned node {node_id} did not satisfy capacity requirement: {reason:?}")]
    InsufficientCapacity {
        node_id: NodeId,
        reason: CapacityRejectionReason,
    },
    #[error("capacity reply for {subject_node_id} claimed {payload_node_id}")]
    CapacityReplyNodeMismatch {
        subject_node_id: NodeId,
        payload_node_id: NodeId,
    },
    #[error("instance {instance_id} on {node_id} was not ready: {reason:?}")]
    InstanceNotReady {
        instance_id: InstanceId,
        node_id: NodeId,
        reason: InstanceNotReadyReason,
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
    FactSource(#[from] FactSourceError),
    #[error(transparent)]
    SubjectParse(#[from] SubjectParseError),
    #[error(transparent)]
    FactKeyParse(#[from] FactKeyParseError),
}

impl From<RoutingError> for DeployError {
    fn from(error: RoutingError) -> Self {
        match error {
            RoutingError::ProjectionCatchUpMissing => Self::ProjectionCatchUpMissing,
            RoutingError::ProjectionCatchUpMismatch { serving_commit_id } => {
                Self::ProjectionCatchUpMismatch { serving_commit_id }
            }
            RoutingError::ServingFactConflict { key } => Self::ServingFactConflict { key },
            RoutingError::ServingFactMissing { key } => Self::ServingFactMissing { key },
            RoutingError::ServingFactKindMismatch { key } => Self::ServingFactKindMismatch { key },
            RoutingError::ServingFactMismatch { serving_commit_id } => {
                Self::ServingFactMismatch { serving_commit_id }
            }
            RoutingError::WirePayload { context, source } => Self::WirePayload { context, source },
            RoutingError::Bus(error) => Self::Bus(error),
            RoutingError::FactSource(error) => Self::FactSource(error),
            RoutingError::FactKeyParse(error) => Self::FactKeyParse(error),
        }
    }
}
