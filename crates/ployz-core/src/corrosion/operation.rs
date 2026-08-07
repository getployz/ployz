//! Construction and transition policy for Corrosion operation summaries.

use serde::{Deserialize, Serialize};

use super::document::{CorrosionDocumentVersion, CorrosionTimestamp, OperationDocument};
use super::principal::OperationInitiator;
use crate::ids::{ClusterId, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId};

/// Operation kind, target, and the only state type legal for that kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionOperation {
    Build {
        service_id: ServiceRowId,
        #[serde(flatten)]
        #[cfg_attr(feature = "ts", ts(flatten))]
        state: CorrosionOperationState,
    },
    Deploy {
        namespace_id: NamespaceRowId,
        service_ids: CorrosionDeployTargets,
        #[serde(flatten)]
        #[cfg_attr(feature = "ts", ts(flatten))]
        state: CorrosionDeployState,
    },
    MachineAdd {
        target_machine_id: MachineRowId,
        #[serde(flatten)]
        #[cfg_attr(feature = "ts", ts(flatten))]
        state: CorrosionOperationState,
    },
    MachineRemove {
        target_machine_id: MachineRowId,
        #[serde(flatten)]
        #[cfg_attr(feature = "ts", ts(flatten))]
        state: CorrosionOperationState,
    },
    Recovery {
        target_machine_id: MachineRowId,
        #[serde(flatten)]
        #[cfg_attr(feature = "ts", ts(flatten))]
        state: CorrosionOperationState,
    },
}

/// Basic state for operation kinds that do not own a deploy outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CorrosionOperationState {
    Created {
        created_at: CorrosionTimestamp,
    },
    Running {
        created_at: CorrosionTimestamp,
        started_at: CorrosionTimestamp,
    },
    Succeeded {
        created_at: CorrosionTimestamp,
        started_at: Option<CorrosionTimestamp>,
        completed_at: CorrosionTimestamp,
    },
    Failed {
        created_at: CorrosionTimestamp,
        started_at: Option<CorrosionTimestamp>,
        completed_at: CorrosionTimestamp,
        failure: CorrosionOperationFailure,
    },
}

/// A terminal failure for a non-deploy operation summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionOperationFailure {
    Precondition {
        message: String,
    },
    MachineUnavailable {
        machine_id: MachineRowId,
        message: String,
    },
    Timeout {
        stage: CorrosionOperationTimeoutStage,
        message: String,
    },
    Execution {
        class: CorrosionExecutionFailureClass,
        message: String,
    },
    Interrupted {
        message: String,
    },
    Superseded {
        winner: OperationRowId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CorrosionOperationTimeoutStage {
    Admission,
    Execution,
    Finalization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CorrosionExecutionFailureClass {
    BuildFailed,
    ImagePullFailed,
    ContainerStartFailed,
    HealthGateFailed,
    StorageFailed,
    NetworkFailed,
    Internal,
}

/// The immutable, ordered, nonempty service target of one deploy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Array<ServiceRowId>"))]
#[serde(try_from = "Vec<ServiceRowId>", into = "Vec<ServiceRowId>")]
pub struct CorrosionDeployTargets(Vec<ServiceRowId>);

impl CorrosionDeployTargets {
    pub fn try_new(service_ids: Vec<ServiceRowId>) -> Result<Self, CorrosionDeployTargetsError> {
        if service_ids.is_empty() {
            return Err(CorrosionDeployTargetsError::Empty);
        }
        for (index, service_id) in service_ids.iter().enumerate() {
            if service_ids
                .iter()
                .take(index)
                .any(|prior| prior == service_id)
            {
                return Err(CorrosionDeployTargetsError::Duplicate {
                    service_id: service_id.clone(),
                });
            }
        }
        Ok(Self(service_ids))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[ServiceRowId] {
        &self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// A valid target set is never empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl TryFrom<Vec<ServiceRowId>> for CorrosionDeployTargets {
    type Error = CorrosionDeployTargetsError;

    fn try_from(value: Vec<ServiceRowId>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CorrosionDeployTargets> for Vec<ServiceRowId> {
    fn from(value: CorrosionDeployTargets) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionDeployTargetsError {
    #[error("a deploy requires at least one target service")]
    Empty,
    #[error("deploy target service {service_id} appears more than once")]
    Duplicate { service_id: ServiceRowId },
}

/// One target service's terminal deploy result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CorrosionDeployServiceResult {
    pub service_id: ServiceRowId,
    #[serde(flatten)]
    #[cfg_attr(feature = "ts", ts(flatten))]
    pub result: CorrosionDeployServiceResultKind,
}

impl CorrosionDeployServiceResult {
    #[must_use]
    pub fn completed(service_id: ServiceRowId) -> Self {
        Self {
            service_id,
            result: CorrosionDeployServiceResultKind::Completed,
        }
    }

    #[must_use]
    pub fn failed(service_id: ServiceRowId, failure: CorrosionDeployServiceFailure) -> Self {
        Self {
            service_id,
            result: CorrosionDeployServiceResultKind::Failed { failure },
        }
    }

    #[must_use]
    pub fn skipped(service_id: ServiceRowId) -> Self {
        Self {
            service_id,
            result: CorrosionDeployServiceResultKind::Skipped,
        }
    }

    #[must_use]
    pub fn unchanged(service_id: ServiceRowId) -> Self {
        Self {
            service_id,
            result: CorrosionDeployServiceResultKind::Unchanged,
        }
    }

    #[must_use]
    pub fn removed(service_id: ServiceRowId) -> Self {
        Self {
            service_id,
            result: CorrosionDeployServiceResultKind::Removed,
        }
    }

    const fn is_useful(&self) -> bool {
        matches!(
            self.result,
            CorrosionDeployServiceResultKind::Completed
                | CorrosionDeployServiceResultKind::Unchanged
                | CorrosionDeployServiceResultKind::Removed
        )
    }

    const fn is_incomplete(&self) -> bool {
        matches!(
            self.result,
            CorrosionDeployServiceResultKind::Failed { .. }
                | CorrosionDeployServiceResultKind::Skipped
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CorrosionDeployServiceResultKind {
    Completed,
    Failed {
        failure: CorrosionDeployServiceFailure,
    },
    Skipped,
    Unchanged,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionDeployServiceFailure {
    ImagePullFailed { message: String },
    ContainerCreateFailed { message: String },
    ContainerStartFailed { message: String },
    IncumbentStopFailed { message: String },
    HealthGateFailed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionDeployFailure {
    BridgeUnavailable {
        machine_id: MachineRowId,
    },
    ServiceFailed {
        service_id: ServiceRowId,
        failure: CorrosionDeployServiceFailure,
    },
    NamespaceChanged {
        namespace_id: NamespaceRowId,
    },
    EvidenceLost {
        reason: CorrosionEvidenceLossReason,
    },
    Promotion {
        service_id: ServiceRowId,
        failure: CorrosionPromotionFailure,
    },
    Superseded {
        service_id: ServiceRowId,
        winner: ServiceRowId,
    },
    SupersededByOperation {
        winner: OperationRowId,
    },
    Interrupted,
}

/// Why driver-local detail could not safely resume a nonterminal operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CorrosionEvidenceLossReason {
    MissingFile,
    MalformedFile,
    IdentityMismatch,
}

/// A terminal classification from exact promotion readback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionPromotionFailure {
    Rejected,
    OutcomeUncertain,
    InvariantViolation {
        service_row: CorrosionPromotionRowObservation,
        container_row: CorrosionPromotionRowObservation,
    },
}

/// How one prepared promotion row compared with its durable intended document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CorrosionPromotionRowObservation {
    Exact,
    Absent,
    Mismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorrosionDeployWarning {
    HealthGateSkipped { service_id: ServiceRowId },
    RoleObservationIncomplete { machine_ids: Vec<MachineRowId> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CorrosionDeployCancellationReason {
    OperatorRequested,
    Shutdown,
}

/// The namespace-level terminal outcome with its legal evidence shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CorrosionDeployOutcome {
    Completed {
        results: Vec<CorrosionDeployServiceResult>,
    },
    CompletedWithWarnings {
        results: Vec<CorrosionDeployServiceResult>,
        warnings: Vec<CorrosionDeployWarning>,
    },
    PartiallyCompleted {
        results: Vec<CorrosionDeployServiceResult>,
        failure: CorrosionDeployFailure,
    },
    PartiallyCompletedWithWarnings {
        results: Vec<CorrosionDeployServiceResult>,
        warnings: Vec<CorrosionDeployWarning>,
        failure: CorrosionDeployFailure,
    },
    Failed {
        results: Vec<CorrosionDeployServiceResult>,
        failure: CorrosionDeployFailure,
    },
    Cancelled {
        results: Vec<CorrosionDeployServiceResult>,
        reason: CorrosionDeployCancellationReason,
    },
}

impl CorrosionDeployOutcome {
    pub fn completed(
        results: Vec<CorrosionDeployServiceResult>,
    ) -> Result<Self, CorrosionDeployOutcomeError> {
        let outcome = Self::Completed { results };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn completed_with_warnings(
        results: Vec<CorrosionDeployServiceResult>,
        warnings: Vec<CorrosionDeployWarning>,
    ) -> Result<Self, CorrosionDeployOutcomeError> {
        let outcome = Self::CompletedWithWarnings { results, warnings };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn partially_completed(
        results: Vec<CorrosionDeployServiceResult>,
        failure: CorrosionDeployFailure,
    ) -> Result<Self, CorrosionDeployOutcomeError> {
        let outcome = Self::PartiallyCompleted { results, failure };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn partially_completed_with_warnings(
        results: Vec<CorrosionDeployServiceResult>,
        warnings: Vec<CorrosionDeployWarning>,
        failure: CorrosionDeployFailure,
    ) -> Result<Self, CorrosionDeployOutcomeError> {
        let outcome = Self::PartiallyCompletedWithWarnings {
            results,
            warnings,
            failure,
        };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn failed(
        results: Vec<CorrosionDeployServiceResult>,
        failure: CorrosionDeployFailure,
    ) -> Result<Self, CorrosionDeployOutcomeError> {
        let outcome = Self::Failed { results, failure };
        outcome.validate()?;
        Ok(outcome)
    }

    pub fn cancelled(
        results: Vec<CorrosionDeployServiceResult>,
        reason: CorrosionDeployCancellationReason,
    ) -> Result<Self, CorrosionDeployOutcomeError> {
        let outcome = Self::Cancelled { results, reason };
        outcome.validate()?;
        Ok(outcome)
    }

    fn results(&self) -> &[CorrosionDeployServiceResult] {
        match self {
            Self::Completed { results }
            | Self::CompletedWithWarnings { results, .. }
            | Self::PartiallyCompleted { results, .. }
            | Self::PartiallyCompletedWithWarnings { results, .. }
            | Self::Failed { results, .. }
            | Self::Cancelled { results, .. } => results,
        }
    }

    fn validate(&self) -> Result<(), CorrosionDeployOutcomeError> {
        let results = self.results();
        if results.is_empty() {
            return Err(CorrosionDeployOutcomeError::EmptyResults);
        }
        for (index, result) in results.iter().enumerate() {
            if results
                .iter()
                .take(index)
                .any(|prior| prior.service_id == result.service_id)
            {
                return Err(CorrosionDeployOutcomeError::DuplicateResult {
                    service_id: result.service_id.clone(),
                });
            }
        }

        match self {
            Self::Completed { results } => require_all_useful(results),
            Self::CompletedWithWarnings { results, warnings } => {
                require_warnings(warnings)?;
                require_all_useful(results)
            }
            Self::PartiallyCompleted { results, .. } => require_partial(results),
            Self::PartiallyCompletedWithWarnings {
                results, warnings, ..
            } => {
                require_warnings(warnings)?;
                require_partial(results)
            }
            Self::Failed { results, .. } => {
                if results
                    .iter()
                    .all(CorrosionDeployServiceResult::is_incomplete)
                {
                    Ok(())
                } else {
                    Err(CorrosionDeployOutcomeError::IllegalFailedResult)
                }
            }
            Self::Cancelled { results, .. } => {
                if results.iter().all(|result| {
                    result.is_useful()
                        || matches!(result.result, CorrosionDeployServiceResultKind::Skipped)
                }) {
                    Ok(())
                } else {
                    Err(CorrosionDeployOutcomeError::IllegalCancelledResult)
                }
            }
        }
    }
}

fn require_all_useful(
    results: &[CorrosionDeployServiceResult],
) -> Result<(), CorrosionDeployOutcomeError> {
    if results.iter().all(CorrosionDeployServiceResult::is_useful) {
        Ok(())
    } else {
        Err(CorrosionDeployOutcomeError::IllegalCompletedResult)
    }
}

fn require_partial(
    results: &[CorrosionDeployServiceResult],
) -> Result<(), CorrosionDeployOutcomeError> {
    if results.iter().any(CorrosionDeployServiceResult::is_useful)
        && results
            .iter()
            .any(CorrosionDeployServiceResult::is_incomplete)
    {
        Ok(())
    } else {
        Err(CorrosionDeployOutcomeError::NotPartial)
    }
}

fn require_warnings(
    warnings: &[CorrosionDeployWarning],
) -> Result<(), CorrosionDeployOutcomeError> {
    if warnings.is_empty() {
        Err(CorrosionDeployOutcomeError::EmptyWarnings)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionDeployOutcomeError {
    #[error("a deploy outcome requires at least one service result")]
    EmptyResults,
    #[error("deploy result for service {service_id} appears more than once")]
    DuplicateResult { service_id: ServiceRowId },
    #[error("completed deploy outcomes may contain only completed, unchanged, or removed results")]
    IllegalCompletedResult,
    #[error("a partially completed deploy requires useful and incomplete service results")]
    NotPartial,
    #[error("failed deploy outcomes may contain only failed or skipped results")]
    IllegalFailedResult,
    #[error("cancelled deploy outcomes cannot contain failed service results")]
    IllegalCancelledResult,
    #[error("a with-warnings deploy outcome requires at least one warning")]
    EmptyWarnings,
}

/// Created, running, or terminal state for a deploy summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CorrosionDeployState {
    Created {
        created_at: CorrosionTimestamp,
    },
    Running {
        created_at: CorrosionTimestamp,
        started_at: CorrosionTimestamp,
    },
    Terminal {
        created_at: CorrosionTimestamp,
        started_at: Option<CorrosionTimestamp>,
        completed_at: CorrosionTimestamp,
        #[serde(flatten)]
        #[cfg_attr(feature = "ts", ts(flatten))]
        outcome: CorrosionDeployOutcome,
    },
}

impl CorrosionDeployState {
    fn created_at(&self) -> CorrosionTimestamp {
        match self {
            Self::Created { created_at }
            | Self::Running { created_at, .. }
            | Self::Terminal { created_at, .. } => *created_at,
        }
    }

    fn validate(&self, targets: &CorrosionDeployTargets) -> Result<(), CorrosionDeployStateError> {
        match self {
            Self::Created { .. } => Ok(()),
            Self::Running {
                created_at,
                started_at,
            } => validate_started(*created_at, *started_at),
            Self::Terminal {
                created_at,
                started_at,
                completed_at,
                outcome,
            } => {
                outcome.validate()?;
                validate_completed(*created_at, *started_at, *completed_at)?;
                let result_ids = outcome
                    .results()
                    .iter()
                    .map(|result| &result.service_id)
                    .collect::<Vec<_>>();
                if targets.as_slice().iter().eq(result_ids) {
                    Ok(())
                } else {
                    Err(CorrosionDeployStateError::TargetResultsMismatch)
                }
            }
        }
    }
}

fn validate_started(
    created_at: CorrosionTimestamp,
    started_at: CorrosionTimestamp,
) -> Result<(), CorrosionDeployStateError> {
    if created_at <= started_at {
        Ok(())
    } else {
        Err(CorrosionDeployStateError::StartedBeforeCreated)
    }
}

fn validate_completed(
    created_at: CorrosionTimestamp,
    started_at: Option<CorrosionTimestamp>,
    completed_at: CorrosionTimestamp,
) -> Result<(), CorrosionDeployStateError> {
    if created_at > completed_at {
        return Err(CorrosionDeployStateError::CompletedBeforeCreated);
    }
    if started_at.is_some_and(|started_at| started_at < created_at || started_at > completed_at) {
        return Err(CorrosionDeployStateError::InvalidStartedAt);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionDeployStateError {
    #[error("deploy start precedes creation")]
    StartedBeforeCreated,
    #[error("deploy completion precedes creation")]
    CompletedBeforeCreated,
    #[error("deploy start is outside the creation-to-completion interval")]
    InvalidStartedAt,
    #[error("deploy results do not match the ordered target service set")]
    TargetResultsMismatch,
    #[error(transparent)]
    InvalidOutcome(#[from] CorrosionDeployOutcomeError),
}

/// One of the only two writes legal after a created deploy summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrosionDeployTransition {
    Running {
        started_at: CorrosionTimestamp,
    },
    Terminal {
        completed_at: CorrosionTimestamp,
        outcome: CorrosionDeployOutcome,
    },
}

/// One of the only two writes legal after a created non-deploy summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrosionBasicTransition {
    Running {
        started_at: CorrosionTimestamp,
    },
    Succeeded {
        completed_at: CorrosionTimestamp,
    },
    Failed {
        completed_at: CorrosionTimestamp,
        failure: CorrosionOperationFailure,
    },
}

impl OperationDocument {
    #[must_use]
    pub fn deploy_created(
        v: CorrosionDocumentVersion,
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        initiator: OperationInitiator,
        namespace_id: NamespaceRowId,
        service_ids: CorrosionDeployTargets,
        created_at: CorrosionTimestamp,
    ) -> Self {
        Self::from_parts(
            v,
            cluster_id,
            machine_id,
            initiator,
            CorrosionOperation::Deploy {
                namespace_id,
                service_ids,
                state: CorrosionDeployState::Created { created_at },
            },
            Some(created_at),
        )
    }

    #[must_use]
    pub fn basic_created(
        v: CorrosionDocumentVersion,
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        initiator: OperationInitiator,
        operation: CorrosionBasicOperation,
        created_at: CorrosionTimestamp,
    ) -> Self {
        let state = CorrosionOperationState::Created { created_at };
        let operation = match operation {
            CorrosionBasicOperation::Build { service_id } => {
                CorrosionOperation::Build { service_id, state }
            }
            CorrosionBasicOperation::MachineAdd { target_machine_id } => {
                CorrosionOperation::MachineAdd {
                    target_machine_id,
                    state,
                }
            }
            CorrosionBasicOperation::MachineRemove { target_machine_id } => {
                CorrosionOperation::MachineRemove {
                    target_machine_id,
                    state,
                }
            }
            CorrosionBasicOperation::Recovery { target_machine_id } => {
                CorrosionOperation::Recovery {
                    target_machine_id,
                    state,
                }
            }
        };
        Self::from_parts(
            v,
            cluster_id,
            machine_id,
            initiator,
            operation,
            Some(created_at),
        )
    }

    #[must_use]
    pub fn deploy_state(&self) -> Option<&CorrosionDeployState> {
        match self.operation() {
            CorrosionOperation::Deploy { state, .. } => Some(state),
            CorrosionOperation::Build { .. }
            | CorrosionOperation::MachineAdd { .. }
            | CorrosionOperation::MachineRemove { .. }
            | CorrosionOperation::Recovery { .. } => None,
        }
    }

    #[must_use]
    pub fn deploy_targets(&self) -> Option<&CorrosionDeployTargets> {
        match self.operation() {
            CorrosionOperation::Deploy { service_ids, .. } => Some(service_ids),
            CorrosionOperation::Build { .. }
            | CorrosionOperation::MachineAdd { .. }
            | CorrosionOperation::MachineRemove { .. }
            | CorrosionOperation::Recovery { .. } => None,
        }
    }

    pub fn transition_deploy(
        self,
        transition: CorrosionDeployTransition,
    ) -> Result<Self, CorrosionOperationTransitionError> {
        let original = self;
        let CorrosionOperation::Deploy {
            namespace_id,
            service_ids,
            state,
        } = original.operation().clone()
        else {
            return Err(CorrosionOperationTransitionError::NotDeploy {
                document: Box::new(original),
            });
        };

        let created_at = state.created_at();
        let next_state = match (state, transition) {
            (
                CorrosionDeployState::Created { .. },
                CorrosionDeployTransition::Running { started_at },
            ) => CorrosionDeployState::Running {
                created_at,
                started_at,
            },
            (
                CorrosionDeployState::Created { .. },
                CorrosionDeployTransition::Terminal {
                    completed_at,
                    outcome,
                },
            ) => CorrosionDeployState::Terminal {
                created_at,
                started_at: None,
                completed_at,
                outcome,
            },
            (
                CorrosionDeployState::Running { started_at, .. },
                CorrosionDeployTransition::Terminal {
                    completed_at,
                    outcome,
                },
            ) => CorrosionDeployState::Terminal {
                created_at,
                started_at: Some(started_at),
                completed_at,
                outcome,
            },
            (CorrosionDeployState::Running { .. }, CorrosionDeployTransition::Running { .. }) => {
                return Err(CorrosionOperationTransitionError::RunningRewrite {
                    document: Box::new(original),
                });
            }
            (CorrosionDeployState::Terminal { .. }, _) => {
                return Err(CorrosionOperationTransitionError::TerminalFinal {
                    document: Box::new(original),
                });
            }
        };

        if let Err(error) = next_state.validate(&service_ids) {
            return Err(CorrosionOperationTransitionError::InvalidDeployState {
                document: Box::new(original),
                error,
            });
        }
        let heartbeat_at = original.heartbeat_at();
        Ok(Self::from_parts(
            original.v,
            original.cluster_id.clone(),
            original.machine_id.clone(),
            original.initiator.clone(),
            CorrosionOperation::Deploy {
                namespace_id,
                service_ids,
                state: next_state,
            },
            heartbeat_at,
        ))
    }

    pub fn transition_basic(
        self,
        transition: CorrosionBasicTransition,
    ) -> Result<Self, CorrosionOperationTransitionError> {
        let original = self;
        let operation = original.operation().clone();
        let state = match &operation {
            CorrosionOperation::Build { state, .. }
            | CorrosionOperation::MachineAdd { state, .. }
            | CorrosionOperation::MachineRemove { state, .. }
            | CorrosionOperation::Recovery { state, .. } => state,
            CorrosionOperation::Deploy { .. } => {
                return Err(CorrosionOperationTransitionError::NotBasic {
                    document: Box::new(original),
                });
            }
        };

        let (created_at, started_at) = match state {
            CorrosionOperationState::Created { created_at } => (*created_at, None),
            CorrosionOperationState::Running {
                created_at,
                started_at,
            } => (*created_at, Some(*started_at)),
            CorrosionOperationState::Succeeded { .. } | CorrosionOperationState::Failed { .. } => {
                return Err(CorrosionOperationTransitionError::TerminalFinal {
                    document: Box::new(original),
                });
            }
        };

        let next_state = match transition {
            CorrosionBasicTransition::Running { started_at } => {
                if state.is_running() {
                    return Err(CorrosionOperationTransitionError::RunningRewrite {
                        document: Box::new(original),
                    });
                }
                CorrosionOperationState::Running {
                    created_at,
                    started_at,
                }
            }
            CorrosionBasicTransition::Succeeded { completed_at } => {
                CorrosionOperationState::Succeeded {
                    created_at,
                    started_at,
                    completed_at,
                }
            }
            CorrosionBasicTransition::Failed {
                completed_at,
                failure,
            } => CorrosionOperationState::Failed {
                created_at,
                started_at,
                completed_at,
                failure,
            },
        };

        if let Err(error) = validate_basic_state_value(&next_state) {
            return Err(CorrosionOperationTransitionError::InvalidBasicState {
                document: Box::new(original),
                error,
            });
        }

        let operation = match operation {
            CorrosionOperation::Build { service_id, .. } => CorrosionOperation::Build {
                service_id,
                state: next_state,
            },
            CorrosionOperation::MachineAdd {
                target_machine_id, ..
            } => CorrosionOperation::MachineAdd {
                target_machine_id,
                state: next_state,
            },
            CorrosionOperation::MachineRemove {
                target_machine_id, ..
            } => CorrosionOperation::MachineRemove {
                target_machine_id,
                state: next_state,
            },
            CorrosionOperation::Recovery {
                target_machine_id, ..
            } => CorrosionOperation::Recovery {
                target_machine_id,
                state: next_state,
            },
            CorrosionOperation::Deploy { .. } => unreachable!("deploy rejected above"),
        };

        let heartbeat_at = original.heartbeat_at();
        Ok(Self::from_parts(
            original.v,
            original.cluster_id.clone(),
            original.machine_id.clone(),
            original.initiator.clone(),
            operation,
            heartbeat_at,
        ))
    }

    /// Whether this summary has reached a final state that no write may follow.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        match self.operation() {
            CorrosionOperation::Deploy { state, .. } => {
                matches!(state, CorrosionDeployState::Terminal { .. })
            }
            CorrosionOperation::Build { state, .. }
            | CorrosionOperation::MachineAdd { state, .. }
            | CorrosionOperation::MachineRemove { state, .. }
            | CorrosionOperation::Recovery { state, .. } => matches!(
                state,
                CorrosionOperationState::Succeeded { .. } | CorrosionOperationState::Failed { .. }
            ),
        }
    }

    /// The creation instant carried by every state of every operation kind.
    #[must_use]
    pub fn created_at(&self) -> CorrosionTimestamp {
        match self.operation() {
            CorrosionOperation::Deploy { state, .. } => state.created_at(),
            CorrosionOperation::Build { state, .. }
            | CorrosionOperation::MachineAdd { state, .. }
            | CorrosionOperation::MachineRemove { state, .. }
            | CorrosionOperation::Recovery { state, .. } => state.created_at(),
        }
    }

    /// Returns a copy with only `heartbeat_at` advanced to `now`.
    ///
    /// The operation kind, state, and outcome are carried over untouched, so
    /// this write can never race a state transition into a different truth.
    /// A terminal summary is final and refuses the refresh.
    pub fn refresh_heartbeat(
        self,
        now: CorrosionTimestamp,
    ) -> Result<Self, CorrosionOperationTransitionError> {
        if self.is_terminal() {
            return Err(CorrosionOperationTransitionError::TerminalFinal {
                document: Box::new(self),
            });
        }
        Ok(Self::from_parts(
            self.v,
            self.cluster_id.clone(),
            self.machine_id.clone(),
            self.initiator.clone(),
            self.operation().clone(),
            Some(now),
        ))
    }
}

impl CorrosionOperationState {
    const fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }

    fn created_at(&self) -> CorrosionTimestamp {
        match self {
            Self::Created { created_at }
            | Self::Running { created_at, .. }
            | Self::Succeeded { created_at, .. }
            | Self::Failed { created_at, .. } => *created_at,
        }
    }
}

/// The cadence at which a live deploy driver rewrites its heartbeat.
pub const DEPLOY_HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// The heartbeat age at which a deploy op stops blocking rival claims.
pub const DEPLOY_CLAIM_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a deploy driver's heartbeat is fresh enough to hold its claim.
///
/// Liveness is meaningful only for non-terminal summaries; a terminal
/// summary's claim is settled by its outcome, not its heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployLiveness {
    Live,
    Stale,
}

impl OperationDocument {
    /// Judges heartbeat freshness against [`DEPLOY_CLAIM_STALE_AFTER`],
    /// falling back to the state's `created_at` for rows without a heartbeat.
    #[must_use]
    pub fn deploy_liveness(&self, now: CorrosionTimestamp) -> DeployLiveness {
        let beat = self.heartbeat_at().unwrap_or_else(|| self.created_at());
        if now.saturating_since(beat) < DEPLOY_CLAIM_STALE_AFTER {
            DeployLiveness::Live
        } else {
            DeployLiveness::Stale
        }
    }

    /// A non-terminal summary whose driver heartbeat has gone stale.
    #[must_use]
    pub fn is_stalled(&self, now: CorrosionTimestamp) -> bool {
        !self.is_terminal() && matches!(self.deploy_liveness(now), DeployLiveness::Stale)
    }
}

/// The admission-time adjudication of one service's optimistic op-row claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployClaim {
    Won,
    Lost { winner: OperationRowId },
}

/// Adjudicates the op-row claim among one service's non-terminal deploy ops.
///
/// The lowest ULID among live candidates wins. Terminal, non-deploy, and
/// stale candidates never block, so only a live rival below `my_operation_id`
/// can turn the claim into a loss.
#[must_use]
pub fn adjudicate_deploy_claim(
    my_operation_id: &OperationRowId,
    candidates: &[(OperationRowId, OperationDocument)],
    now: CorrosionTimestamp,
) -> DeployClaim {
    let winner = candidates
        .iter()
        .filter(|(id, document)| {
            id < my_operation_id
                && document.deploy_state().is_some()
                && !document.is_terminal()
                && matches!(document.deploy_liveness(now), DeployLiveness::Live)
        })
        .map(|(id, _)| id)
        .min();
    match winner {
        Some(winner) => DeployClaim::Lost {
            winner: winner.clone(),
        },
        None => DeployClaim::Won,
    }
}

/// A phase-boundary check for newer deploy ops that may own the service now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTakeover {
    Clear,
    TakenOver { winner: OperationRowId },
}

/// Checks whether any deploy op newer than `my_operation_id` has taken over.
///
/// A newer op is harmless only when it terminally recorded itself superseded
/// by `my_operation_id`: losers mutate nothing. Any other newer op — running,
/// failed, interrupted, or stalled — may already have swept this op's
/// containers, so staleness never clears a takeover.
#[must_use]
pub fn check_deploy_takeover(
    my_operation_id: &OperationRowId,
    newer: &[(OperationRowId, OperationDocument)],
) -> DeployTakeover {
    let winner = newer
        .iter()
        .filter(|(id, document)| {
            id > my_operation_id && !is_terminal_superseded_by(document, my_operation_id)
        })
        .map(|(id, _)| id)
        .min();
    match winner {
        Some(winner) => DeployTakeover::TakenOver {
            winner: winner.clone(),
        },
        None => DeployTakeover::Clear,
    }
}

fn is_terminal_superseded_by(document: &OperationDocument, winner: &OperationRowId) -> bool {
    let Some(CorrosionDeployState::Terminal { outcome, .. }) = document.deploy_state() else {
        return false;
    };
    match outcome {
        CorrosionDeployOutcome::Failed { failure, .. } => match failure {
            CorrosionDeployFailure::SupersededByOperation { winner: recorded } => {
                recorded == winner
            }
            CorrosionDeployFailure::BridgeUnavailable { .. }
            | CorrosionDeployFailure::ServiceFailed { .. }
            | CorrosionDeployFailure::NamespaceChanged { .. }
            | CorrosionDeployFailure::EvidenceLost { .. }
            | CorrosionDeployFailure::Promotion { .. }
            | CorrosionDeployFailure::Superseded { .. }
            | CorrosionDeployFailure::Interrupted => false,
        },
        CorrosionDeployOutcome::Completed { .. }
        | CorrosionDeployOutcome::CompletedWithWarnings { .. }
        | CorrosionDeployOutcome::PartiallyCompleted { .. }
        | CorrosionDeployOutcome::PartiallyCompletedWithWarnings { .. }
        | CorrosionDeployOutcome::Cancelled { .. } => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrosionBasicOperation {
    Build { service_id: ServiceRowId },
    MachineAdd { target_machine_id: MachineRowId },
    MachineRemove { target_machine_id: MachineRowId },
    Recovery { target_machine_id: MachineRowId },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionOperationTransitionError {
    #[error("operation is not a deploy")]
    NotDeploy { document: Box<OperationDocument> },
    #[error("operation is a deploy, not a basic operation")]
    NotBasic { document: Box<OperationDocument> },
    #[error("a running operation summary cannot be rewritten")]
    RunningRewrite { document: Box<OperationDocument> },
    #[error("a terminal operation summary is final")]
    TerminalFinal { document: Box<OperationDocument> },
    #[error("invalid deploy state: {error}")]
    InvalidDeployState {
        document: Box<OperationDocument>,
        error: CorrosionDeployStateError,
    },
    #[error("invalid basic operation state: {error}")]
    InvalidBasicState {
        document: Box<OperationDocument>,
        error: CorrosionBasicStateError,
    },
}

impl CorrosionOperationTransitionError {
    #[must_use]
    pub fn document(&self) -> &OperationDocument {
        match self {
            Self::NotDeploy { document }
            | Self::NotBasic { document }
            | Self::RunningRewrite { document }
            | Self::TerminalFinal { document }
            | Self::InvalidDeployState { document, .. }
            | Self::InvalidBasicState { document, .. } => document,
        }
    }
}

pub(super) fn validate_operation_document(
    operation: &CorrosionOperation,
) -> Result<(), CorrosionOperationDocumentError> {
    match operation {
        CorrosionOperation::Deploy {
            service_ids, state, ..
        } => state
            .validate(service_ids)
            .map_err(CorrosionOperationDocumentError::InvalidDeployState),
        CorrosionOperation::Build { state, .. }
        | CorrosionOperation::MachineAdd { state, .. }
        | CorrosionOperation::MachineRemove { state, .. }
        | CorrosionOperation::Recovery { state, .. } => validate_basic_state_value(state)
            .map_err(CorrosionOperationDocumentError::InvalidBasicState),
    }
}

fn validate_basic_state_value(
    state: &CorrosionOperationState,
) -> Result<(), CorrosionBasicStateError> {
    match state {
        CorrosionOperationState::Created { .. } => Ok(()),
        CorrosionOperationState::Running {
            created_at,
            started_at,
        } => {
            if created_at <= started_at {
                Ok(())
            } else {
                Err(CorrosionBasicStateError::StartedBeforeCreated)
            }
        }
        CorrosionOperationState::Succeeded {
            created_at,
            started_at,
            completed_at,
        }
        | CorrosionOperationState::Failed {
            created_at,
            started_at,
            completed_at,
            ..
        } => {
            if created_at > completed_at {
                return Err(CorrosionBasicStateError::CompletedBeforeCreated);
            }
            if started_at
                .is_some_and(|started_at| started_at < *created_at || started_at > *completed_at)
            {
                return Err(CorrosionBasicStateError::InvalidStartedAt);
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionBasicStateError {
    #[error("operation start precedes creation")]
    StartedBeforeCreated,
    #[error("operation completion precedes creation")]
    CompletedBeforeCreated,
    #[error("operation start is outside the creation-to-completion interval")]
    InvalidStartedAt,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionOperationDocumentError {
    #[error("invalid operation summary: {0}")]
    InvalidDeployState(CorrosionDeployStateError),
    #[error("invalid basic operation summary: {0}")]
    InvalidBasicState(CorrosionBasicStateError),
}
