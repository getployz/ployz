#![forbid(unsafe_code)]

//! Public schema and type export surface for SDK consumers.
//!
//! This crate should expose generated or generation-ready wire types. It should
//! not contain orchestration logic.

use serde::{Deserialize, Serialize};

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
    OperationIdempotencyKey, OperationStatus, OperationSubject, OperatorHint,
    ReplayedOperationEvent, RetainedArtifact, RouteCutoverFailureReason, RouteHostname,
    RouteHostnameError, RoutePort, RoutePortError, RouteTarget,
};
pub use ployz_core::ops::{DeployOperationFailure, DeployOperationState, DeployRunningStage};
pub use ployz_core::state::{
    ActiveServiceCommitRequest, ActiveServiceState, ExpectedActiveService,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploySubmitRequest {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub service_id: ServiceId,
}

pub type DeploySubmitResponse = OperationApiResponse<AcceptedOperation, DeploySubmitError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpsStatusRequest {
    pub operation_id: OperationId,
}

pub type OpsStatusResponse = OperationApiResponse<OperationStatus, OpsStatusError>;

pub type OpsWatchRequest = OperationEventReplayRequest;

pub type OpsWatchResponse = OperationApiResponse<OperationEventReplayPage, OpsWatchError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationApiResponse<T, E> {
    Ok { value: T },
    DomainError { error: E },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedOperation {
    pub operation_id: OperationId,
    pub dispatch: OperationDispatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationDispatch {
    Queued {
        watch_subject: String,
        start_sequence: EventSequence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploySubmitError {
    Unavailable {
        operation_id: OperationId,
        source: DeploySubmitUnavailableSource,
    },
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploySubmitUnavailableSource {
    StatusStore { failure: DeploySubmitStatusFailure },
    EventLog { failure: DeploySubmitEventFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploySubmitStatusFailure {
    OpenBucket,
    EncodeStatus,
    DecodeStatus,
    EncodeSubmission,
    DecodeSubmission,
    CasConflict,
    GetStatus,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploySubmitEventFailure {
    EncodeEvent,
    DecodeEvent,
    PublishRequest,
    PublishAck,
    ReadEvent,
    Timeout,
    InvalidAckSequence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpsStatusError {
    NoSuchOperation {
        operation_id: OperationId,
    },
    Unavailable {
        operation_id: OperationId,
        source: OpsStatusUnavailableSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpsStatusUnavailableSource {
    StatusStore { failure: StatusReadFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusReadFailure {
    DecodeStatus,
    GetStatus,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpsWatchError {
    NoSuchOperation {
        operation_id: OperationId,
    },
    Unavailable {
        operation_id: OperationId,
        source: OpsWatchUnavailableSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpsWatchUnavailableSource {
    StatusStore { failure: StatusReadFailure },
    EventLog { failure: EventReplayFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventReplayFailure {
    DecodeEvent,
    ReadEvent,
    Timeout,
    InvalidEventSequence,
    InvalidNextReplaySequence,
}
