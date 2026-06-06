#![forbid(unsafe_code)]

//! Public schema and type export surface for SDK consumers.
//!
//! This crate should expose generated or generation-ready wire types. It should
//! not contain orchestration logic.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub mod typescript;

pub use ployz_core::cert::{
    AcmeChallengeError, AcmeChallengeToken, AcmeChallengeTtlError, AcmeChallengeTtlSeconds,
    AcmeChallengeValue, AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertTextError,
    CertValidAt, CertValidAtError, CertValidityError, CertValidityWindow,
};
pub use ployz_core::deploy::{
    DeployPlan, DeployPlanStep, DeployRequest, ImageReference, ImageReferenceError, ReplicaCount,
    ReplicaCountError, ReplicaSlot,
};
pub use ployz_core::ids::{
    CertId, ContainerId, NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId,
    SubjectTokenError,
};
pub use ployz_core::ops::{
    ActiveServiceCommitFailure, ArtifactUnavailableReason, CancellationReason, EventSequence,
    EventSequenceError, FailureMessage, HealthCheckFailure, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    NonEmptyTextError, OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayLimitError, OperationEventReplayPage, OperationEventReplayRequest,
    OperationIdempotencyKey, OperationLeaseExpiresAt, OperationLeaseExpiresAtError,
    OperationOwnerLease, OperationOwnershipStatus, OperationStatus, OperationStatusSnapshot,
    OperationSubject, OperatorHint, ReplayedOperationEvent, RetainedArtifact,
    RouteCutoverFailureReason, RouteHostname, RouteHostnameError, RoutePort, RoutePortError,
    RouteTarget,
};
pub use ployz_core::ops::{
    CertOperationFailure, CertOperationState, CertRunningStage, DeployOperationFailure,
    DeployOperationState, DeployRunningStage,
};
pub use ployz_core::state::{
    ActiveServiceCommitRequest, ActiveServiceState, ExpectedActiveService,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploySubmitRequest {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub target: DeployRequest,
}

pub type DeploySubmitResponse = OperationApiResponse<AcceptedOperation, DeploySubmitError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct OpsStatusRequest {
    pub operation_id: OperationId,
}

pub type OpsStatusResponse = OperationApiResponse<OperationStatusSnapshot, OpsStatusError>;

pub type OpsWatchRequest = OperationEventReplayRequest;

pub type OpsWatchResponse = OperationApiResponse<OperationEventReplayPage, OpsWatchError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationApiResponse<T, E> {
    Ok { value: T },
    DomainError { error: E },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct AcceptedOperation {
    pub operation_id: OperationId,
    pub watch_subject: String,
    pub start_sequence: EventSequence,
    pub owner_lease: OperationOwnerLease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploySubmitUnavailableSource {
    StatusStore { failure: DeploySubmitStatusFailure },
    EventLog { failure: DeploySubmitEventFailure },
    Clock { failure: DeploySubmitClockFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DeploySubmitStatusFailure {
    OpenBucket,
    EncodeStatus,
    DecodeStatus,
    EncodeSubmission,
    DecodeSubmission,
    EncodeLease,
    DecodeLease,
    CasConflict,
    GetStatus,
    Clock,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DeploySubmitClockFailure {
    BeforeUnixEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpsStatusUnavailableSource {
    StatusStore { failure: StatusReadFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StatusReadFailure {
    DecodeStatus,
    DecodeLease,
    GetStatus,
    Clock,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpsWatchUnavailableSource {
    StatusStore { failure: StatusReadFailure },
    EventLog { failure: EventReplayFailure },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum EventReplayFailure {
    DecodeEvent,
    ReadEvent,
    Timeout,
    InvalidEventSequence,
    InvalidNextReplaySequence,
}
