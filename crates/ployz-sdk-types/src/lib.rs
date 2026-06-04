#![forbid(unsafe_code)]

//! Public schema and type export surface for SDK consumers.
//!
//! This crate should expose generated or generation-ready wire types. It should
//! not contain orchestration logic.

pub use ployz_core::deploy::{
    DeployRequest, ImageReference, ImageReferenceError, ReplicaCount, ReplicaCountError,
};
pub use ployz_core::ids::{
    ContainerId, NodeId, OperationId, RevisionId, ServiceId, SubjectTokenError,
};
pub use ployz_core::ops::{
    ArtifactUnavailableReason, CancellationReason, EventSequence, EventSequenceError,
    FailureMessage, HealthCheckFailure, MAX_OPERATION_EVENT_REPLAY_LIMIT, NonEmptyTextError,
    OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayLimitError, OperationEventReplayPage, OperationEventReplayRequest,
    OperationStatus, OperationSubject, OperatorHint, ReplayedOperationEvent, RetainedArtifact,
    RouteCutoverFailureReason, RouteHostname, RouteHostnameError, RoutePort, RoutePortError,
    RouteTarget,
};
pub use ployz_core::ops::{DeployOperationFailure, DeployOperationState, DeployRunningStage};
pub use ployz_core::state::{
    ActiveServiceCommitRequest, ActiveServiceState, ExpectedActiveService,
};
