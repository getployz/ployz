#![forbid(unsafe_code)]

//! Public schema and type export surface for SDK consumers.
//!
//! This crate should expose generated or generation-ready wire types. It should
//! not contain orchestration logic.

use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;

pub const MAX_LOGS_TAIL_LINES: u16 = 1_000;
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
pub use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, WireGuardEbpfComponent,
    WireGuardEbpfNodeReady, WireGuardEbpfPrepareReport, WireGuardEbpfReady, WireGuardPublicKey,
    WireGuardReady, WireGuardReadyEvidence,
};
pub use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanStep, DeployRequest, DeployRoute, ImageReference,
    ImageReferenceError, ReplicaCount, ReplicaCountError, ReplicaSlot,
};
pub use ployz_core::ids::{
    CertId, ContainerId, NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId, StepId,
    SubjectTokenError,
};
pub use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallContractError,
    InstallSha256Digest, MachineBootstrapUrl, MachineJoinArtifact, MachineJoinBundle,
    MachineJoinClusterName, MachineJoinMaterial, MachineJoinNatsCredentials,
    MachineJoinPloyzdArtifact, MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery,
    MachineJoinTemplate, MachineJoinTrustedNats, MachineJoinTrustedNatsServerId,
};
pub use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, JoinTokenRedeemedAt,
    MachineAddFailure, MachineAddOperationState, MachineAddOperationStateName, MachineName,
    MachineReadinessCheck, MachineReadinessEvidence,
};
pub use ployz_core::node::ManagedContainerKind;
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
    CertOperationFailure, CertOperationState, CertRunningStage, DeployCleanupFailure,
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState, DeployRunningStage,
};
pub use ployz_core::roles::FirstNodeGateway;
pub use ployz_core::state::{
    ActiveMachineState, ActiveServiceCommitRequest, ActiveServiceState, ExpectedActiveService,
    GatewayServingStatus, GatewayStatusObservation, NodePublicIpObservation,
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
}

pub type MachineAddResponse = OperationApiResponse<MachineAddAccepted, MachineAddError>;

pub type InitFirstNodeActivateResponse =
    OperationApiResponse<InitFirstNodeActivated, InitFirstNodeActivateError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct InitFirstNodeActivateRequest {
    pub node_id: NodeId,
    pub gateway: MachineAddGateway,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct InitFirstNodeActivated {
    pub operation_id: OperationId,
    pub node_id: NodeId,
}

pub type MachineListResponse = OperationApiResponse<MachineListResult, MachineListError>;

pub type MachineInspectResponse = OperationApiResponse<MachineSnapshot, MachineInspectError>;

pub type ServiceListResponse = OperationApiResponse<ServiceListResult, ServiceListError>;

pub type ServiceInspectResponse = OperationApiResponse<ServiceSnapshot, ServiceInspectError>;

pub type LogsTailResponse = OperationApiResponse<LogsTailResult, LogsTailError>;

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
    pub join_bundle: MachineJoinBundle,
    pub join_token: MachineJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineInspectRequest {
    pub node_id: NodeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineListResult {
    pub machines: Vec<MachineSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceInspectRequest {
    pub service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct LogsTailRequest {
    pub container_id: ContainerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<LogsTailLines>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct LogsTailResult {
    pub node_id: NodeId,
    pub container_id: ContainerId,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(type = "SafeInteger<\"LogsTailLines\">")]
#[serde(transparent)]
pub struct LogsTailLines(u16);

impl LogsTailLines {
    pub fn try_new(value: u16) -> Result<Self, LogsTailLinesError> {
        if value == 0 {
            return Err(LogsTailLinesError::Zero);
        }
        if value > MAX_LOGS_TAIL_LINES {
            return Err(LogsTailLinesError::TooLarge { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogsTailLinesError {
    Zero,
    TooLarge { value: u16 },
}

impl fmt::Display for LogsTailLinesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => write!(formatter, "logs tail lines must be greater than zero"),
            Self::TooLarge { .. } => write!(
                formatter,
                "logs tail lines must be at most {MAX_LOGS_TAIL_LINES}"
            ),
        }
    }
}

impl<'de> Deserialize<'de> for LogsTailLines {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceListResult {
    pub services: Vec<ServiceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceSnapshot {
    pub active: ActiveServiceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineSnapshot {
    pub active: ActiveMachineState,
    pub public_ip: Option<NodePublicIpObservation>,
    pub gateway: Option<GatewayStatusObservation>,
    pub observed_container_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineListError {
    Unavailable {
        source: MachineQueryUnavailableSource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineInspectError {
    NoSuchMachine {
        node_id: NodeId,
    },
    Unavailable {
        source: MachineQueryUnavailableSource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceListError {
    Unavailable {
        source: ServiceQueryUnavailableSource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceInspectError {
    NoSuchService {
        service_id: ServiceId,
    },
    Unavailable {
        source: ServiceQueryUnavailableSource,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogsTailError {
    NoSuchContainer {
        container_id: ContainerId,
    },
    AmbiguousContainer {
        container_id: ContainerId,
        node_ids: Vec<NodeId>,
    },
    ReadFailed {
        node_id: NodeId,
        container_id: ContainerId,
        message: FailureMessage,
    },
    Unavailable {
        source: LogsTailUnavailableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_id: Option<NodeId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogsTailUnavailableSource {
    Observations,
    NodeRpc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceQueryUnavailableSource {
    CoreState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineQueryUnavailableSource {
    CoreState,
    Observations,
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
    pub secret_delivery: MachineJoinSecretDelivery,
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
pub enum InitFirstNodeActivateError {
    InvalidPlan,
    Unavailable {
        source: MachineQueryUnavailableSource,
    },
    MachineAdd {
        failure: MachineAddError,
    },
    JoinRedeem {
        failure: MachineJoinRedeemError,
    },
    JoinReport {
        failure: MachineJoinReportError,
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
    CoreState,
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
    MissingJoinTemplate,
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
