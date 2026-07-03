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

pub use ployz_core::cert::{
    AcmeChallengeError, AcmeChallengeToken, AcmeChallengeTtlError, AcmeChallengeTtlSeconds,
    AcmeChallengeValue, AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertTextError,
    CertValidAt, CertValidAtError, CertValidityError, CertValidityWindow,
};
pub use ployz_core::dataplane::{
    DataplaneMember, DataplaneProviderFailure, EbpfForwardingReady, EbpfForwardingReadyEvidence,
    MachineEndpointSubnet, PloyzNativeMeshComponent, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
pub use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanStep, DeployRequest, DeployRoute,
    DeployServicePlan, DeployServiceSpec, ImageReference, ImageReferenceError, ReplicaCount,
    ReplicaCountError, ReplicaSlot,
};
pub use ployz_core::ids::{
    CertId, ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    OperationId, ServiceId, StepId, SubjectTokenError,
};
pub use ployz_core::install::{
    AbsoluteInstallPath, FirstMachineInstallArtifacts, FirstMachineInstallSpec,
    InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion, InstallContractError,
    InstallSha256Digest, MachineBootstrapUrl, MachineJoinBundle, MachineJoinClusterName,
    MachineJoinMaterial, MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTemplate,
    MachineJoinTrustedNats, NatsServerInstallSpec,
};
pub use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, JoinTokenRedeemedAt,
    MachineAddFailure, MachineAddOperationState, MachineAddOperationStateName,
    MachineCredentialProvisioningStep, MachineName, MachineReadinessCheck,
    MachineReadinessEvidence,
};
pub use ployz_core::machine_runtime::{
    ContainerRuntimeState, ManagedContainerIdentity, ManagedContainerKind,
    ManagedContainerObservation,
};
pub use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserPublicKey, NatsUserSeed};
pub use ployz_core::ops::{
    ArtifactUnavailableReason, CancellationReason, EventSequence, EventSequenceError,
    FailureMessage, HealthCheckFailure, MAX_OPERATION_EVENT_REPLAY_LIMIT, MachineSubstrateVersions,
    MachineUpdateFailure, MachineUpdateOperationState, NonEmptyTextError, OperationEvent,
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayLimitError,
    OperationEventReplayPage, OperationEventReplayRequest, OperationIdempotencyKey,
    OperationStatus, OperationStatusSnapshot, OperationSubject, OperatorHint,
    ReplayedOperationEvent, RetainedArtifact, RouteCutoverFailureReason, RouteHostname,
    RouteHostnameError, RoutePort, RoutePortError, RouteTarget,
};
pub use ployz_core::ops::{
    CertOperationFailure, CertOperationState, CertRunningStage, ControlPlaneCommitScope,
    DeployCleanupFailure, DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    DeployRunningStage,
};
pub use ployz_core::roles::{DnsRole, GatewayRole, InstallRolePolicy};
pub use ployz_core::state::{
    ActiveMachineState, GatewayServingStatus, GatewayStatusObservation, MachinePublicIpObservation,
    RouteBindingState, ServingTargetEntry,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploySubmitRequest {
    pub operation_id: OperationId,
    pub target: DeployRequest,
}

pub type DeploySubmitResponse = OperationApiResponse<AcceptedOperation, DeploySubmitError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineAddRequest {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
}

pub type MachineAddResponse = OperationApiResponse<MachineAddAccepted, MachineAddError>;

pub type InitFirstMachineActivateResponse =
    OperationApiResponse<InitFirstMachineActivated, InitFirstMachineActivateError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct InitFirstMachineActivateRequest {
    pub machine_id: MachineId,
    pub roles: InstallRolePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct InitFirstMachineActivated {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}

pub type MachineListResponse = OperationApiResponse<MachineListResult, MachineListError>;

pub type MachineInspectResponse = OperationApiResponse<MachineSnapshot, MachineInspectError>;

pub type MachineUpdateResponse = OperationApiResponse<AcceptedOperation, MachineUpdateError>;

pub type ServiceListResponse = OperationApiResponse<ServiceListResult, ServiceListError>;

pub type ServiceInspectResponse = OperationApiResponse<ServiceSnapshot, ServiceInspectError>;

pub type RuntimeSnapshotResponse =
    OperationApiResponse<RuntimeSnapshotResult, RuntimeSnapshotError>;

pub type LogsTailResponse = OperationApiResponse<LogsTailResult, LogsTailError>;

pub type MachineJoinRedeemResponse =
    OperationApiResponse<MachineJoinRedeemed, MachineJoinRedeemError>;

pub type MachineJoinReportResponse =
    OperationApiResponse<MachineJoinReported, MachineJoinReportError>;

pub type OpsListResponse = OperationApiResponse<OpsListResult, OpsListError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineAddAccepted {
    pub accepted: AcceptedOperation,
    pub machine_id: MachineId,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_bundle: MachineJoinBundle,
    pub join_token: MachineJoinToken,
    pub join_secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct OpsListRequest {
    #[serde(default)]
    pub active_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct OpsListResult {
    pub operations: Vec<OperationStatusSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineInspectRequest {
    pub machine_id: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineUpdateRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
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
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct LogsTailRequest {
    pub container_id: ContainerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<LogsTailLines>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct LogsTailResult {
    pub machine_id: MachineId,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LogsTailLinesError {
    #[error("logs tail lines must be greater than zero")]
    Zero,
    #[error("logs tail lines must be at most {MAX_LOGS_TAIL_LINES}")]
    TooLarge { value: u16 },
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
    pub active: ServingTargetEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotResult {
    pub snapshot: RuntimeSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshot {
    pub machines: Vec<MachineSnapshot>,
    pub services: Vec<ServiceSnapshot>,
    pub routes: Vec<RouteBindingState>,
    pub containers: Vec<ManagedContainerObservation>,
    pub revisions: Vec<RuntimeServiceRevision>,
    pub releases: Vec<RuntimeServiceRelease>,
    pub instances: Vec<RuntimeServiceInstance>,
    pub projection_sources: RuntimeProjectionSources,
    pub updated_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceRevision {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceRelease {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub routes: Vec<RouteTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeServiceInstance {
    pub namespace_id: NamespaceId,
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
    pub operation_id: OperationId,
    pub step_id: StepId,
    pub state: ContainerRuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjectionSources {
    pub core_state: RuntimeProjectionSource,
    pub observations: RuntimeProjectionSource,
    pub revisions: RuntimeDerivedCollectionSource,
    pub releases: RuntimeDerivedCollectionSource,
    pub instances: RuntimeDerivedCollectionSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjectionSource {
    pub read_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDerivedCollectionSource {
    pub status: RuntimeDerivedCollectionStatus,
    pub source_count: usize,
    pub missing_link_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDerivedCollectionStatus {
    Complete,
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineSnapshot {
    pub active: ActiveMachineState,
    pub public_ip: Option<MachinePublicIpObservation>,
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
        machine_id: MachineId,
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
pub enum RuntimeSnapshotError {
    Unavailable {
        source: RuntimeSnapshotUnavailableSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeSnapshotUnavailableSource {
    CoreState,
    Observations,
    RuntimeProjection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogsTailError {
    NoSuchContainer {
        container_id: ContainerId,
    },
    AmbiguousContainer {
        container_id: ContainerId,
        machine_ids: Vec<MachineId>,
    },
    ReadFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
    },
    Unavailable {
        source: LogsTailUnavailableSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine_id: Option<MachineId>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogsTailUnavailableSource {
    Observations,
    MachineRpc,
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
    pub machine_id: MachineId,
    pub last_event_sequence: EventSequence,
    pub outcome: MachineJoinReportOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinRedeemed {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
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
    /// The operation is accepted but its per-machine credential has not
    /// reached `material-ready` yet. The keeper retries redeem boundedly
    /// until the material lands or the join token TTL expires.
    MaterialNotReady {
        operation_id: OperationId,
    },
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
pub enum InitFirstMachineActivateError {
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
    /// Control could not write the first machine's `machine.seed` after the
    /// minted material was redeemed.
    MachineSeedWrite {
        message: FailureMessage,
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
    DuplicateIdempotencyKey {
        operation_id: OperationId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpdateError {
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    CurrentMachineUnsupported {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    Unavailable {
        operation_id: OperationId,
        source: MachineUpdateUnavailableSource,
    },
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpdateUnavailableSource {
    OperationSubmit {
        failure: OperationSubmitUnavailableSource,
    },
    CoreState,
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
    BootstrapMaterial {
        failure: BootstrapMaterialFailure,
    },
}

impl From<OperationSubmitUnavailableSource> for MachineAddUnavailableSource {
    fn from(value: OperationSubmitUnavailableSource) -> Self {
        match value {
            OperationSubmitUnavailableSource::StatusStore { failure } => {
                Self::StatusStore { failure }
            }
            OperationSubmitUnavailableSource::EventLog { failure } => Self::EventLog { failure },
        }
    }
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

pub const CLOUD_BOOTSTRAP_PROTOCOL_VERSION: u16 = 1;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapSessionSecret\">")]
#[serde(transparent)]
pub struct CloudBootstrapSessionSecret(String);

impl CloudBootstrapSessionSecret {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapSessionSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudBootstrapSessionSecret([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapCallbackToken\">")]
#[serde(transparent)]
pub struct CloudBootstrapCallbackToken(String);

impl CloudBootstrapCallbackToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapCallbackToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloudBootstrapCallbackToken([redacted])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapRedemptionId\">")]
#[serde(transparent)]
pub struct CloudBootstrapRedemptionId(String);

impl CloudBootstrapRedemptionId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapRedemptionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CloudBootstrapRedemptionId")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"CloudBootstrapAttemptId\">")]
#[serde(transparent)]
pub struct CloudBootstrapAttemptId(String);

impl CloudBootstrapAttemptId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CloudBootstrapSecretError> {
        let value = value.into();
        validate_cloud_bootstrap_secret(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CloudBootstrapAttemptId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CloudBootstrapAttemptId")
            .field(&self.0)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudBootstrapSecretError {
    Empty,
    Invalid,
}

fn validate_cloud_bootstrap_secret(value: &str) -> Result<(), CloudBootstrapSecretError> {
    if value.is_empty() {
        return Err(CloudBootstrapSecretError::Empty);
    }
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(CloudBootstrapSecretError::Invalid);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapClientInfo {
    pub protocol_version: u16,
    pub keeper_version: String,
}

impl CloudBootstrapClientInfo {
    #[must_use]
    pub fn current(keeper_version: impl Into<String>) -> Self {
        Self {
            protocol_version: CLOUD_BOOTSTRAP_PROTOCOL_VERSION,
            keeper_version: keeper_version.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapMachineFacts {
    pub hostname: Option<String>,
    pub os: String,
    pub arch: String,
    pub candidate_runtime_nats_url: Option<MachineJoinRuntimeNatsUrl>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapSessionCreateRequest {
    pub attempt_id: CloudBootstrapAttemptId,
    pub client: CloudBootstrapClientInfo,
    pub machine: CloudBootstrapMachineFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapSessionCreated {
    pub browser_url: String,
    pub user_code: String,
    pub session_secret: CloudBootstrapSessionSecret,
    pub poll_after_seconds: u16,
    pub expires_at_unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapSessionPollRequest {
    pub attempt_id: CloudBootstrapAttemptId,
    pub session_secret: CloudBootstrapSessionSecret,
    pub machine: CloudBootstrapMachineFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapDecision {
    Pending {
        retry_after_seconds: u16,
    },
    Ready {
        envelope: Box<CloudBootstrapEnvelope>,
    },
    Expired,
    Failed {
        failure: CloudBootstrapDecisionFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapDecisionFailure {
    UnsupportedClient {
        message: FailureMessage,
        minimum_protocol_version: u16,
    },
    Unauthorized,
    AlreadyConsumedByPolicy,
    InvalidMachineFacts {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapEnvelope {
    pub attempt_id: CloudBootstrapAttemptId,
    pub redemption_id: CloudBootstrapRedemptionId,
    pub callback_url: String,
    pub callback_token: CloudBootstrapCallbackToken,
    pub intent: CloudBootstrapIntent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapIntent {
    Founder { founder: Box<CloudFounderBootstrap> },
    Joiner { joiner: Box<CloudJoinerBootstrap> },
    WaitForFounder { retry_after_seconds: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudFounderBootstrap {
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub cloud_nats_user_public_key: NatsUserPublicKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudJoinerBootstrap {
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
    pub join_token: MachineJoinToken,
    pub join_secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapCallbackRequest {
    pub attempt_id: CloudBootstrapAttemptId,
    pub redemption_id: CloudBootstrapRedemptionId,
    pub outcome: CloudBootstrapOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapOutcome {
    FounderSucceeded { result: CloudFounderBootstrapResult },
    JoinerSucceeded { result: CloudJoinerBootstrapResult },
    Failed { failure: CloudBootstrapFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudFounderBootstrapResult {
    pub machine_id: MachineId,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudJoinerBootstrapResult {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub last_event_sequence: EventSequence,
    pub result: MachineJoinRedeemResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "failure", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudBootstrapFailure {
    AlreadyBootstrapped,
    EnvelopeInvalid { message: FailureMessage },
    BootstrapFailed { message: FailureMessage },
    CloudReachabilityFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CloudBootstrapCallbackAccepted {
    pub accepted_at_unix_seconds: u64,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeploySubmitError {
    InvalidTarget {
        operation_id: OperationId,
        message: FailureMessage,
    },
    ResourceBusy {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        owner_operation_id: OperationId,
    },
    Unavailable {
        operation_id: OperationId,
        source: OperationSubmitUnavailableSource,
    },
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

/// The shared unavailable source for operation submits (deploy and the
/// submit core of machine-add).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSubmitUnavailableSource {
    StatusStore {
        failure: OperationSubmitStatusFailure,
    },
    EventLog {
        failure: OperationSubmitEventFailure,
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
    CasConflict,
    GetStatus,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpsListError {
    Unavailable { source: OpsStatusUnavailableSource },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StatusReadFailure {
    DecodeStatus,
    GetStatus,
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
