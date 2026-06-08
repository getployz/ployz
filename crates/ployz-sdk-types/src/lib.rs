#![forbid(unsafe_code)]

//! Public schema and type export surface for SDK consumers.
//!
//! This crate should expose generated or generation-ready wire types. It should
//! not contain orchestration logic.

use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;

pub mod operation_api;
pub mod typescript;

pub use ployz_core::backup::{
    BackupArtifact, BackupArtifactKind, BackupBundle, BackupItem, BackupManifest,
    BackupManifestVersion, BackupPolicy, BackupScopeEntry, ControlPlaneKvSnapshot,
    KvBucketSnapshot, KvEntrySnapshot, RestoreStep,
};
pub use ployz_core::cert::{
    AcmeChallengeError, AcmeChallengeToken, AcmeChallengeTtlError, AcmeChallengeTtlSeconds,
    AcmeChallengeValue, AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertTextError,
    CertValidAt, CertValidAtError, CertValidityError, CertValidityWindow,
};
pub use ployz_core::dataplane::WireGuardEbpfComponent;
pub use ployz_core::deploy::{
    DeployPlan, DeployPlanStep, DeployRequest, DeployRoute, ImageReference, ImageReferenceError,
    ReplicaCount, ReplicaCountError, ReplicaSlot,
};
pub use ployz_core::ids::{
    CertId, ContainerId, NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId,
    SubjectTokenError,
};
pub use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    MachineJoinBundle, MachineJoinClusterName, MachineJoinPloyzdArtifact,
};
pub use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, JoinTokenRedeemedAt,
    MachineAddFailure, MachineAddOperationState, MachineAddOperationStateName, MachineName,
    MachineReadinessCheck, MachineReadinessEvidence,
};
pub use ployz_core::ops::{
    ActiveServiceCommitFailure, ArtifactUnavailableReason, BackupOperationFailure,
    BackupOperationState, BackupRunningStage, CancellationReason, EventSequence,
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
pub use ployz_core::roles::FirstNodeGateway;
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
pub struct BackupCreateRequest {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

pub type BackupCreateResponse = OperationApiResponse<AcceptedOperation, BackupCreateError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineAddRequest {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub node_id: NodeId,
    pub name: MachineName,
    pub gateway: MachineAddGateway,
    pub join_bundle: MachineJoinBundle,
}

pub type MachineAddResponse = OperationApiResponse<MachineAddAccepted, MachineAddError>;

pub type MachineJoinRedeemResponse =
    OperationApiResponse<MachineJoinRedeemed, MachineJoinRedeemError>;

pub type MachineJoinReportResponse =
    OperationApiResponse<MachineJoinReported, MachineJoinReportError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MachineAddGateway {
    Install,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineAddAccepted {
    pub accepted: AcceptedOperation,
    pub node_id: NodeId,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_token: MachineJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinRedeemRequest {
    pub join_token: MachineJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinReportRequest {
    pub join_token: MachineJoinToken,
    pub outcome: MachineJoinReportOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportOutcome {
    Completed,
    Failed { failure: MachineJoinReportFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportFailure {
    BootstrapFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinReported {
    pub operation_id: OperationId,
    pub node_id: NodeId,
    pub last_event_sequence: EventSequence,
    pub outcome: MachineJoinReportOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinRedeemed {
    pub operation_id: OperationId,
    pub node_id: NodeId,
    pub name: MachineName,
    pub gateway: FirstNodeGateway,
    pub join_bundle: MachineJoinBundle,
    pub joined_at: JoinTokenRedeemedAt,
    pub last_event_sequence: EventSequence,
    pub result: MachineJoinRedeemResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MachineJoinRedeemResult {
    Joined,
    AlreadyJoined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinRedeemError {
    InvalidJoinToken,
    UnknownJoinToken,
    Rejected {
        operation_id: OperationId,
        failure: MachineAddFailure,
    },
    OperationNotPending {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    Unavailable {
        source: MachineJoinRedeemUnavailableSource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportError {
    InvalidJoinToken,
    UnknownJoinToken,
    OperationNotJoining {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    Unavailable {
        source: MachineJoinReportUnavailableSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportUnavailableSource {
    StatusRead {
        failure: StatusReadFailure,
    },
    StatusWrite {
        failure: OperationSubmitStatusFailure,
    },
    EventLog {
        failure: OperationSubmitEventFailure,
    },
    OperationCorrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinRedeemUnavailableSource {
    StatusRead {
        failure: StatusReadFailure,
    },
    StatusWrite {
        failure: OperationSubmitStatusFailure,
    },
    EventLog {
        failure: OperationSubmitEventFailure,
    },
    Clock {
        failure: OperationSubmitClockFailure,
    },
    OperationCorrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineAddError {
    Unavailable {
        operation_id: OperationId,
        source: MachineAddUnavailableSource,
    },
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineAddUnavailableSource {
    StatusStore {
        failure: OperationSubmitStatusFailure,
    },
    EventLog {
        failure: OperationSubmitEventFailure,
    },
    Clock {
        failure: OperationSubmitClockFailure,
    },
    BootstrapMaterial {
        failure: BootstrapMaterialFailure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapMaterialFailure {
    EncodeJoinBundle,
    IssueJoinToken,
    RenderBootstrapUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"MachineBootstrapUrl\">")]
#[serde(transparent)]
pub struct MachineBootstrapUrl(String);

impl MachineBootstrapUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, BootstrapCommandError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(BootstrapCommandError::EmptyBootstrapUrl);
        }
        if !value.starts_with("https://")
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(BootstrapCommandError::InvalidBootstrapUrl);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"MachineJoinToken\">")]
#[serde(transparent)]
pub struct MachineJoinToken(String);

impl MachineJoinToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, BootstrapCommandError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BootstrapCommandError::EmptyJoinToken);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(BootstrapCommandError::InvalidJoinToken);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MachineJoinToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MachineJoinToken")
            .field(&"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapCommandError {
    EmptyBootstrapUrl,
    InvalidBootstrapUrl,
    EmptyJoinToken,
    InvalidJoinToken,
}

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupCreateError {
    Unavailable {
        operation_id: OperationId,
        source: BackupCreateUnavailableSource,
    },
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupCreateUnavailableSource {
    StatusStore {
        failure: OperationSubmitStatusFailure,
    },
    EventLog {
        failure: OperationSubmitEventFailure,
    },
    Clock {
        failure: OperationSubmitClockFailure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploySubmitUnavailableSource {
    StatusStore {
        failure: OperationSubmitStatusFailure,
    },
    EventLog {
        failure: OperationSubmitEventFailure,
    },
    Clock {
        failure: OperationSubmitClockFailure,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OperationSubmitStatusFailure {
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
pub enum OperationSubmitEventFailure {
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
pub enum OperationSubmitClockFailure {
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
