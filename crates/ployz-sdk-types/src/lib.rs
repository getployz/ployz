#![forbid(unsafe_code)]

//! Public schema and type export surface for SDK consumers.
//!
//! This crate should expose generated or generation-ready wire types. It should
//! not contain orchestration logic.
//!
//! Error evidence carries two shapes on purpose: `Unavailable { message }`
//! variants hold a plain rendered `String` — transient plumbing evidence for a
//! reader, never dispatched on — while [`FailureMessage`] fields belong to
//! durable failure records on operations, the typed audience of the Operation
//! Rules. A new error field takes `String` unless it lands on an operation
//! record.

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
    MachineJoinTrustedNats, NatsServerInstallSpec, WrappedCaKey,
};
pub use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, JoinTokenRedeemedAt,
    MachineAddFailure, MachineCredentialProvisioningStep, MachineName, MachineReadinessCheck,
    MachineReadinessEvidence,
};
pub use ployz_core::machine_runtime::{
    ContainerRuntimeState, ManagedContainerIdentity, ManagedContainerKind,
    ManagedContainerObservation,
};
pub use ployz_core::nats_config::{
    NatsAuthorizedUser, NatsCaCertificatePem, NatsUserPublicKey, NatsUserSeed,
};
pub use ployz_core::ops::{
    ArtifactUnavailableReason, CancellationReason, EventSequence, EventSequenceError,
    FailureMessage, HealthCheckFailure, MAX_OPERATION_EVENT_REPLAY_LIMIT, MachineAddOperationState,
    MachineAddOperationStateName, MachineLifecycleFailure, MachineLifecycleOperationState,
    MachineSubstrateVersions, MachineUpdateFailure, MachineUpdateOperationState, NonEmptyTextError,
    OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayLimitError, OperationEventReplayPage, OperationEventReplayRequest,
    OperationIdempotencyKey, OperationKind, OperationStatus, OperationStatusSnapshot,
    OperationSubject, OperatorHint, ReplayedOperationEvent, RetainedArtifact,
    RouteCutoverFailureReason, RouteHostname, RouteHostnameError, RoutePort, RoutePortError,
    RouteTarget, UnusableMachine,
};
pub use ployz_core::ops::{
    CertOperationFailure, CertOperationState, CertRunningStage, ControlPlaneCommitScope,
    DeployCleanupFailure, DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    DeployRunningStage,
};
pub use ployz_core::roles::{DnsRole, GatewayRole, InstallRolePolicy};
pub use ployz_core::security::NatsPrincipal;
pub use ployz_core::state::MachineUsabilityReason;
pub use ployz_core::state::{
    ActiveMachineState, GatewayServingStatus, GatewayStatusObservation, MachineLifecycle,
    MachinePublicIpObservation, RouteBindingState, ServingTargetEntry,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploySubmitRequest {
    pub idempotency_key: OperationIdempotencyKey,
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

/// One request shape for both lifecycle endpoints: the endpoint carries the
/// verb (drain or resume), the body only names the operation and machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineLifecycleRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}

pub type MachineDrainResponse = OperationApiResponse<AcceptedOperation, MachineLifecycleError>;
pub type MachineResumeResponse = OperationApiResponse<AcceptedOperation, MachineLifecycleError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineLifecycleError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("machine lifecycle {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
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
    pub intent: RuntimeProjectionSource,
    pub facts: RuntimeProjectionSource,
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
    /// When this machine last self-reported, as display evidence for the
    /// operator. Never an input to behavior: liveness surfaces at the point
    /// of use (ADR 0027).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_observed_at_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineListError {
    #[error("machine list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineInspectError {
    #[error("no such machine {}", .machine_id.as_str())]
    NoSuchMachine { machine_id: MachineId },
    #[error("machine inspect unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum ServiceListError {
    #[error("service list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum ServiceInspectError {
    #[error("no such service {}", .service_id.as_str())]
    NoSuchService { service_id: ServiceId },
    #[error("service inspect unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum RuntimeSnapshotError {
    #[error("runtime snapshot unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum LogsTailError {
    #[error("no such container {}", .container_id.as_str())]
    NoSuchContainer { container_id: ContainerId },
    #[error("container {} exists on {} machines", .container_id.as_str(), .machine_ids.len())]
    AmbiguousContainer {
        container_id: ContainerId,
        machine_ids: Vec<MachineId>,
    },
    #[error(
        "log read failed on {} for {}: {message}",
        .machine_id.as_str(),
        .container_id.as_str()
    )]
    ReadFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
    },
    #[error("logs tail unavailable: {message}")]
    Unavailable {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine_id: Option<MachineId>,
    },
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
#[derive(thiserror::Error)]
pub enum MachineJoinRedeemError {
    #[error("join token is invalid")]
    InvalidJoinToken,
    #[error("join token is unknown")]
    UnknownJoinToken,
    /// The operation is accepted but its per-machine credential has not
    /// reached `material-ready` yet. The keeper retries redeem boundedly
    /// until the material lands or the join token TTL expires.
    #[error("material for operation {} is not ready yet", .operation_id.as_str())]
    MaterialNotReady { operation_id: OperationId },
    #[error("machine add {} rejected: {failure}", .operation_id.as_str())]
    Rejected {
        operation_id: OperationId,
        failure: MachineAddFailure,
    },
    #[error(
        "operation {} is not pending (currently {})",
        .operation_id.as_str(),
        .current.as_str()
    )]
    OperationNotPending {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    #[error("machine join redeem unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum InitFirstMachineActivateError {
    #[error("first machine activation plan is invalid")]
    InvalidPlan,
    #[error("first machine activation unavailable: {message}")]
    Unavailable { message: String },
    #[error("machine add failed: {failure}")]
    MachineAdd { failure: MachineAddError },
    #[error("join redeem failed: {failure}")]
    JoinRedeem { failure: MachineJoinRedeemError },
    #[error("join report failed: {failure}")]
    JoinReport { failure: MachineJoinReportError },
    /// Control could not write the first machine's `machine.seed` after the
    /// minted material was redeemed.
    #[error("machine seed write failed: {message}")]
    MachineSeedWrite { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineJoinReportError {
    #[error("join token is invalid")]
    InvalidJoinToken,
    #[error("join token is unknown")]
    UnknownJoinToken,
    #[error(
        "operation {} is not joining (currently {})",
        .operation_id.as_str(),
        .current.as_str()
    )]
    OperationNotJoining {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    #[error("machine join report unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineAddError {
    #[error("machine add {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
    #[error("operation {} already exists for this idempotency key", .operation_id.as_str())]
    DuplicateIdempotencyKey { operation_id: OperationId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineUpdateError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error(
        "operation {}: updating the current machine {} is unsupported",
        .operation_id.as_str(),
        .machine_id.as_str()
    )]
    CurrentMachineUnsupported {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("machine update {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
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
#[derive(thiserror::Error)]
pub enum DeploySubmitError {
    #[error("deploy target invalid for operation {}: {message}", .operation_id.as_str())]
    InvalidTarget {
        operation_id: OperationId,
        message: FailureMessage,
    },
    #[error(
        "namespace {} is busy with operation {}",
        .namespace_id.as_str(),
        .owner_operation_id.as_str()
    )]
    ResourceBusy {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        owner_operation_id: OperationId,
    },
    #[error("deploy submit {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum OpsStatusError {
    #[error("no such operation {}", .operation_id.as_str())]
    NoSuchOperation { operation_id: OperationId },
    #[error("operation status for {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum OpsListError {
    #[error("operation list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum OpsWatchError {
    #[error("no such operation {}", .operation_id.as_str())]
    NoSuchOperation { operation_id: OperationId },
    #[error("operation watch for {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
}
