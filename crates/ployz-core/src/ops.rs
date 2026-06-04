//! Operation state, events, and status models.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::{NonZeroU16, NonZeroU64};

use crate::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployRunningStage {
    RunningPreDeploy,
    StartingContainers,
    WaitingForHealth,
    CuttingOverRoute,
    CleaningUp,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployOperationFailure {
    ArtifactUnavailable {
        service_id: ServiceId,
        revision_id: RevisionId,
        reason: ArtifactUnavailableReason,
    },
    HealthCheckFailed {
        health_check: HealthCheckFailure,
        retained_artifact: RetainedArtifact,
    },
    RouteCutoverFailed {
        route: RouteTarget,
        reason: RouteCutoverFailureReason,
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
    ProbeFailed { message: FailureMessage },
    TimedOut { timeout_seconds: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteCutoverFailureReason {
    GatewayUnavailable { node_id: NodeId },
    RouteRejected { message: FailureMessage },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSubject {
    Deploy { service_id: ServiceId },
    MachineAdd { node_id: NodeId },
    MachineDrain { node_id: NodeId },
    ServiceRemove { service_id: ServiceId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationEvent {
    DeploySubmitted {
        operation_id: OperationId,
        service_id: ServiceId,
    },
    DeployPlanningStarted {
        operation_id: OperationId,
    },
    DeployContainerStarted {
        operation_id: OperationId,
        node_id: NodeId,
        container_id: ContainerId,
    },
    DeployCompleted {
        operation_id: OperationId,
    },
    DeployFailed {
        operation_id: OperationId,
        failure: DeployOperationFailure,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteTarget {
    pub hostname: RouteHostname,
    pub port: RoutePort,
}

impl RouteTarget {
    pub fn try_new(hostname: RouteHostname, port: RoutePort) -> Self {
        Self { hostname, port }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

        Ok(Self(value))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
