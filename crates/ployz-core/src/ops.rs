//! Operation state, events, and status models.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::{NonZeroU16, NonZeroU64};

use crate::cert::{AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertValidityWindow};
use crate::deploy::{DeployPlan, DeployRequest};
use crate::ids::{
    CertId, ContainerId, NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId,
    SubjectToken, SubjectTokenError,
};
use crate::state::ExpectedActiveService;

mod projection;

pub use projection::{
    CertProjection, DeployProjection, OperationEventProjection, OperationSubjectRef,
    ProjectionOperationState, StatusProjectionError, project_cert_transition,
    project_deploy_transition, project_operation_event, validate_cert_transition,
    validate_deploy_transition, validate_fresh_deploy_evidence,
};

pub const MAX_OPERATION_EVENT_REPLAY_LIMIT: u16 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployRunningStage {
    StartingContainers,
    WaitingForHealth,
    ActiveServiceCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertRunningStage {
    ChallengePublished,
    ValidationStarted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployOperationState {
    Accepted,
    Planning,
    Running { stage: DeployRunningStage },
    Completed,
    Failed { failure: DeployOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl DeployOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Planning | Self::Running { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertOperationState {
    Accepted,
    Running { stage: CertRunningStage },
    Completed,
    Failed { failure: CertOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl CertOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Running { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployOperationFailure {
    PlanningFailed {
        service_id: ServiceId,
        revision_id: RevisionId,
        message: FailureMessage,
    },
    ArtifactUnavailable {
        service_id: ServiceId,
        revision_id: RevisionId,
        reason: ArtifactUnavailableReason,
    },
    RuntimeUnavailable {
        node_id: NodeId,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    HealthCheckFailed {
        health_check: HealthCheckFailure,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    ControlPlaneCommitFailed {
        service_id: ServiceId,
        revision_id: RevisionId,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    ActiveServiceCommitRejected {
        service_id: ServiceId,
        revision_id: RevisionId,
        reason: ActiveServiceCommitFailure,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    RouteCutoverFailed {
        route: RouteTarget,
        reason: RouteCutoverFailureReason,
        retained_artifacts: Vec<RetainedArtifact>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertOperationFailure {
    ChallengePublishFailed {
        cert_id: CertId,
        message: FailureMessage,
    },
    AcmeValidationFailed {
        cert_id: CertId,
        message: FailureMessage,
        retained_active_cert: Option<ActiveCertState>,
    },
    ActiveCertCommitFailed {
        cert_id: CertId,
        bundle_ref: CertBundleRef,
        validity: CertValidityWindow,
        message: FailureMessage,
        retained_active_cert: Option<ActiveCertState>,
    },
}

impl CertOperationFailure {
    #[must_use]
    pub const fn cert_id(&self) -> &CertId {
        match self {
            Self::ChallengePublishFailed { cert_id, .. }
            | Self::AcmeValidationFailed { cert_id, .. }
            | Self::ActiveCertCommitFailed { cert_id, .. } => cert_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActiveServiceCommitFailure {
    ActiveServiceChanged {
        expected_current: ExpectedActiveService,
        current_revision: Option<RevisionId>,
        attempted_revision: RevisionId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactUnavailableReason {
    BundleMissing,
    BundleUnreadable { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum HealthCheckFailure {
    ProbeFailed {
        node_id: NodeId,
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    TimedOut {
        timeout_seconds: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteCutoverFailureReason {
    GatewayUnavailable { node_id: NodeId },
    RouteRejected { message: FailureMessage },
    TimedOut { timeout_seconds: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetainedArtifact {
    StartedContainer {
        node_id: NodeId,
        container_id: ContainerId,
        log_hint: OperatorHint,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationStatus {
    Deploy {
        id: OperationId,
        service_id: ServiceId,
        state: DeployOperationState,
        last_event_sequence: EventSequence,
    },
    Cert {
        id: OperationId,
        cert_id: CertId,
        state: CertOperationState,
        last_event_sequence: EventSequence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationStatusSnapshot {
    pub status: OperationStatus,
    pub ownership: OperationOwnershipStatus,
}

impl OperationStatusSnapshot {
    #[must_use]
    pub fn new(status: OperationStatus, ownership: OperationOwnershipStatus) -> Self {
        Self { status, ownership }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationOwnerLease {
    pub operation_id: OperationId,
    pub owner_id: OperationOwnerId,
    pub expires_at: OperationLeaseExpiresAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct OperationLeaseDurationSeconds(NonZeroU64);

impl OperationLeaseDurationSeconds {
    pub fn try_new(seconds: u64) -> Result<Self, OperationLeaseDurationError> {
        let Some(value) = NonZeroU64::new(seconds) else {
            return Err(OperationLeaseDurationError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for OperationLeaseDurationSeconds {
    type Error = OperationLeaseDurationError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperationLeaseDurationSeconds> for u64 {
    fn from(value: OperationLeaseDurationSeconds) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationLeaseDurationError {
    Zero,
}

impl fmt::Display for OperationLeaseDurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("operation lease duration must be greater than zero"),
        }
    }
}

impl OperationOwnerLease {
    #[must_use]
    pub fn new(
        operation_id: OperationId,
        owner_id: OperationOwnerId,
        expires_at: OperationLeaseExpiresAt,
    ) -> Self {
        Self {
            operation_id,
            owner_id,
            expires_at,
        }
    }

    #[must_use]
    pub fn is_expired_at(&self, now: OperationLeaseExpiresAt) -> bool {
        self.expires_at <= now
    }

    #[must_use]
    pub fn renew_until(&self, expires_at: OperationLeaseExpiresAt) -> Self {
        Self {
            operation_id: self.operation_id.clone(),
            owner_id: self.owner_id.clone(),
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationOwnershipStatus {
    Unclaimed,
    Owned { lease: OperationOwnerLease },
    Expired { lease: OperationOwnerLease },
}

impl OperationOwnershipStatus {
    #[must_use]
    pub fn from_lease_at(lease: OperationOwnerLease, now: OperationLeaseExpiresAt) -> Self {
        if lease.is_expired_at(now) {
            Self::Expired { lease }
        } else {
            Self::Owned { lease }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct OperationLeaseExpiresAt(NonZeroU64);

impl OperationLeaseExpiresAt {
    pub fn try_new(unix_seconds: u64) -> Result<Self, OperationLeaseExpiresAtError> {
        let Some(value) = NonZeroU64::new(unix_seconds) else {
            return Err(OperationLeaseExpiresAtError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn unix_seconds(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for OperationLeaseExpiresAt {
    type Error = OperationLeaseExpiresAtError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperationLeaseExpiresAt> for u64 {
    fn from(value: OperationLeaseExpiresAt) -> Self {
        value.unix_seconds()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationLeaseExpiresAtError {
    Zero,
}

impl fmt::Display for OperationLeaseExpiresAtError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("operation lease expiry must be greater than zero"),
        }
    }
}

impl OperationStatus {
    #[must_use]
    pub fn deploy_accepted(
        id: OperationId,
        service_id: ServiceId,
        event_sequence: EventSequence,
    ) -> Self {
        Self::Deploy {
            id,
            service_id,
            state: DeployOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn cert_accepted(id: OperationId, cert_id: CertId, event_sequence: EventSequence) -> Self {
        Self::Cert {
            id,
            cert_id,
            state: CertOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Deploy { state, .. } => state.is_terminal(),
            Self::Cert { state, .. } => state.is_terminal(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTransition {
    Planning,
    Running { stage: DeployRunningStage },
    Completed,
    Failed { failure: DeployOperationFailure },
    Cancelled { reason: CancellationReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployEvidence {
    PlanCreated {
        plan: DeployPlan,
    },
    ContainerStarted {
        node_id: NodeId,
        container_id: ContainerId,
    },
    HealthCheckStarted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTransition {
    Running { stage: CertRunningStage },
    Completed,
    Failed { failure: CertOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl CertTransition {
    #[must_use]
    pub fn state(&self) -> CertOperationState {
        match self {
            Self::Running { stage } => CertOperationState::Running { stage: *stage },
            Self::Completed => CertOperationState::Completed,
            Self::Failed { failure } => CertOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => CertOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

impl DeployEvidence {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::PlanCreated { plan } => OperationEvent::DeployPlanCreated {
                operation_id: operation_id.clone(),
                plan: plan.clone(),
            },
            Self::ContainerStarted {
                node_id,
                container_id,
            } => OperationEvent::DeployContainerStarted {
                operation_id: operation_id.clone(),
                node_id: node_id.clone(),
                container_id: container_id.clone(),
            },
            Self::HealthCheckStarted => OperationEvent::DeployHealthCheckStarted {
                operation_id: operation_id.clone(),
            },
        }
    }
}

impl DeployTransition {
    #[must_use]
    pub fn state(&self) -> DeployOperationState {
        match self {
            Self::Planning => DeployOperationState::Planning,
            Self::Running { stage } => DeployOperationState::Running { stage: *stage },
            Self::Completed => DeployOperationState::Completed,
            Self::Failed { failure } => DeployOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => DeployOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationIdempotencyKey(SubjectToken);

impl OperationIdempotencyKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct EventSequence(NonZeroU64);

impl EventSequence {
    pub fn try_new(value: u64) -> Result<Self, EventSequenceError> {
        let Some(value) = NonZeroU64::new(value) else {
            return Err(EventSequenceError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<u64> for EventSequence {
    type Error = EventSequenceError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<EventSequence> for u64 {
    fn from(value: EventSequence) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventSequenceError {
    Zero,
}

impl fmt::Display for EventSequenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("event sequence must be greater than zero"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct OperationEventReplayLimit(NonZeroU16);

impl OperationEventReplayLimit {
    pub fn try_new(value: u16) -> Result<Self, OperationEventReplayLimitError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(OperationEventReplayLimitError::Zero);
        };
        if value.get() > MAX_OPERATION_EVENT_REPLAY_LIMIT {
            return Err(OperationEventReplayLimitError::TooLarge {
                value: value.get(),
                max: MAX_OPERATION_EVENT_REPLAY_LIMIT,
            });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }

    #[must_use]
    pub fn as_usize(self) -> usize {
        usize::from(self.get())
    }
}

impl TryFrom<u16> for OperationEventReplayLimit {
    type Error = OperationEventReplayLimitError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperationEventReplayLimit> for u16 {
    fn from(value: OperationEventReplayLimit) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationEventReplayLimitError {
    Zero,
    TooLarge { value: u16, max: u16 },
}

impl fmt::Display for OperationEventReplayLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => {
                formatter.write_str("operation event replay limit must be greater than zero")
            }
            Self::TooLarge { value, max } => {
                write!(
                    formatter,
                    "operation event replay limit {value} exceeds maximum {max}"
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationEventReplayRequest {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub limit: OperationEventReplayLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationEventReplayPage {
    pub events: Vec<ReplayedOperationEvent>,
    pub cursor: OperationEventReplayCursor,
}

impl OperationEventReplayPage {
    #[must_use]
    pub fn caught_up(events: Vec<ReplayedOperationEvent>) -> Self {
        Self {
            events,
            cursor: OperationEventReplayCursor::CaughtUp,
        }
    }

    #[must_use]
    pub fn more(events: Vec<ReplayedOperationEvent>, next_start_sequence: EventSequence) -> Self {
        Self {
            events,
            cursor: OperationEventReplayCursor::More {
                next_start_sequence,
            },
        }
    }

    #[must_use]
    pub fn terminal(events: Vec<ReplayedOperationEvent>) -> Self {
        Self {
            events,
            cursor: OperationEventReplayCursor::Terminal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationEventReplayCursor {
    CaughtUp,
    Terminal,
    More { next_start_sequence: EventSequence },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayedOperationEvent {
    pub sequence: EventSequence,
    pub event: OperationEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSubject {
    Deploy { service_id: ServiceId },
    Cert { cert_id: CertId },
    MachineAdd { node_id: NodeId },
    MachineDrain { node_id: NodeId },
    ServiceRemove { service_id: ServiceId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationEvent {
    DeploySubmitted {
        operation_id: OperationId,
        target: DeployRequest,
    },
    DeployPlanningStarted {
        operation_id: OperationId,
    },
    DeployPlanCreated {
        operation_id: OperationId,
        plan: DeployPlan,
    },
    DeployRunning {
        operation_id: OperationId,
        stage: DeployRunningStage,
    },
    DeployContainerStarted {
        operation_id: OperationId,
        node_id: NodeId,
        container_id: ContainerId,
    },
    DeployHealthCheckStarted {
        operation_id: OperationId,
    },
    DeployCompleted {
        operation_id: OperationId,
    },
    DeployFailed {
        operation_id: OperationId,
        failure: DeployOperationFailure,
    },
    CertRenewalSubmitted {
        operation_id: OperationId,
        cert_id: CertId,
    },
    CertChallengePublished {
        operation_id: OperationId,
        cert_id: CertId,
        challenge: AcmeHttp01Challenge,
    },
    CertValidationStarted {
        operation_id: OperationId,
        cert_id: CertId,
    },
    CertCompleted {
        operation_id: OperationId,
        active_cert: ActiveCertState,
    },
    CertFailed {
        operation_id: OperationId,
        failure: CertOperationFailure,
    },
    Cancelled {
        operation_id: OperationId,
        reason: CancellationReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FailureMessage(String);

impl FailureMessage {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NonEmptyTextError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(NonEmptyTextError::Empty);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FailureMessage {
    type Error = NonEmptyTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<FailureMessage> for String {
    fn from(value: FailureMessage) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CancellationReason(String);

impl CancellationReason {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NonEmptyTextError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(NonEmptyTextError::Empty);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CancellationReason {
    type Error = NonEmptyTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CancellationReason> for String {
    fn from(value: CancellationReason) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OperatorHint(String);

impl OperatorHint {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NonEmptyTextError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(NonEmptyTextError::Empty);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OperatorHint {
    type Error = NonEmptyTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperatorHint> for String {
    fn from(value: OperatorHint) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteTarget {
    pub hostname: RouteHostname,
    pub port: RoutePort,
}

impl RouteTarget {
    #[must_use]
    pub fn try_new(hostname: RouteHostname, port: RoutePort) -> Self {
        Self { hostname, port }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RouteHostname(String);

impl RouteHostname {
    pub fn try_new(value: impl Into<String>) -> Result<Self, RouteHostnameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RouteHostnameError::Empty);
        }

        if value
            .split('.')
            .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
        {
            return Err(RouteHostnameError::Invalid { value });
        }

        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return Err(RouteHostnameError::Invalid { value });
        }

        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RouteHostname {
    type Error = RouteHostnameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RouteHostname> for String {
    fn from(value: RouteHostname) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteHostnameError {
    Empty,
    Invalid { value: String },
}

impl fmt::Display for RouteHostnameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("route hostname is empty"),
            Self::Invalid { value } => write!(formatter, "route hostname is invalid: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct RoutePort(NonZeroU16);

impl RoutePort {
    pub fn try_new(value: u16) -> Result<Self, RoutePortError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(RoutePortError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for RoutePort {
    type Error = RoutePortError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RoutePort> for u16 {
    fn from(value: RoutePort) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutePortError {
    Zero,
}

impl fmt::Display for RoutePortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("route port must be greater than zero"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonEmptyTextError {
    Empty,
}

impl fmt::Display for NonEmptyTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("text must not be empty"),
        }
    }
}
