//! Network repair operation: re-apply the cluster dataplane projection to
//! every active machine through one bounded operation.

use serde::{Deserialize, Serialize};

use crate::dataplane::{PloyzNativeMeshComponent, PloyzNativeMeshPrepareReport};
use crate::ids::{MachineId, OperationId};
use crate::internal_dns::InternalDnsFactWatermark;

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
    project_transition,
};
use super::text::{CancellationReason, FailureMessage};
use super::{EventSequence, OperationKind, OperationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NetworkRepairRunningStage {
    PreparingDataplane,
    RefreshingMachineFacts,
    ConfirmingDnsRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairOperationState {
    Accepted,
    Running { stage: NetworkRepairRunningStage },
    Completed,
    Failed { failure: NetworkRepairFailure },
    Cancelled { reason: CancellationReason },
}

impl NetworkRepairOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Running { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairFailure {
    NoActiveMachines,
    TargetMachineNotFound {
        machine_id: MachineId,
    },
    IntentReadFailed {
        message: FailureMessage,
    },
    DataplaneConvergenceFailed {
        machine_id: MachineId,
        component: PloyzNativeMeshComponent,
        message: FailureMessage,
    },
    DataplaneReportInvalid {
        message: FailureMessage,
    },
    MachineFactsRefreshFailed {
        outcomes: Vec<NetworkRepairMachineFactsRefreshOutcome>,
    },
    DnsRefreshFailed {
        confirmed_machine_ids: Vec<MachineId>,
        problems: Vec<NetworkRepairDnsRefreshProblem>,
    },
    ProgressRecordFailed {
        phase: NetworkRepairProgressPhase,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NetworkRepairProgressPhase {
    Starting,
    RecordingDataplaneEvidence,
    AdvancingMachineFacts,
    RecordingMachineFactsEvidence,
    AdvancingDnsRefresh,
    RecordingDnsRefreshEvidence,
    Completing,
    RecordingTerminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairMachineFactsRefreshOutcome {
    Refreshed {
        machine_id: MachineId,
        observed_at_unix_ms: u64,
    },
    Unavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    Failed {
        machine_id: MachineId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "problem", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairDnsRefreshProblem {
    Unavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    Stale {
        machine_id: MachineId,
        stale_machine_ids: Vec<MachineId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkRepairTransition {
    Running { stage: NetworkRepairRunningStage },
    Completed,
    Failed { failure: NetworkRepairFailure },
    Cancelled { reason: CancellationReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkRepairEvidence {
    DataplanePrepared {
        report: PloyzNativeMeshPrepareReport,
    },
    MachineFactsRefreshed {
        watermarks: Vec<InternalDnsFactWatermark>,
    },
    DnsRefreshConfirmed {
        machine_ids: Vec<MachineId>,
    },
}

impl NetworkRepairEvidence {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::DataplanePrepared { report } => OperationEvent::NetworkRepairDataplanePrepared {
                operation_id: operation_id.clone(),
                report: report.clone(),
            },
            Self::MachineFactsRefreshed { watermarks } => {
                OperationEvent::NetworkRepairMachineFactsRefreshed {
                    operation_id: operation_id.clone(),
                    watermarks: watermarks.clone(),
                }
            }
            Self::DnsRefreshConfirmed { machine_ids } => {
                OperationEvent::NetworkRepairDnsRefreshConfirmed {
                    operation_id: operation_id.clone(),
                    machine_ids: machine_ids.clone(),
                }
            }
        }
    }
}

impl NetworkRepairTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Running { stage } => OperationEvent::NetworkRepairRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed => OperationEvent::NetworkRepairCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::NetworkRepairFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::NetworkRepair,
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> NetworkRepairOperationState {
        match self {
            Self::Running { stage } => NetworkRepairOperationState::Running { stage: *stage },
            Self::Completed => NetworkRepairOperationState::Completed,
            Self::Failed { failure } => NetworkRepairOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => NetworkRepairOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

pub(super) enum NetworkRepairEvent {
    Submitted,
    Evidence(NetworkRepairEvidence),
    Transition(NetworkRepairTransition),
}

pub(super) fn project_event(
    id: &OperationId,
    target_machine_id: &Option<MachineId>,
    state: &NetworkRepairOperationState,
    event: NetworkRepairEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        NetworkRepairEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
        NetworkRepairEvent::Evidence(evidence) => {
            let expected_stage = match evidence {
                NetworkRepairEvidence::DataplanePrepared { .. } => {
                    NetworkRepairRunningStage::PreparingDataplane
                }
                NetworkRepairEvidence::MachineFactsRefreshed { .. } => {
                    NetworkRepairRunningStage::RefreshingMachineFacts
                }
                NetworkRepairEvidence::DnsRefreshConfirmed { .. } => {
                    NetworkRepairRunningStage::ConfirmingDnsRefresh
                }
            };
            if !matches!(state, NetworkRepairOperationState::Running { stage } if *stage == expected_stage)
            {
                return Ok(OperationProjection::AlreadySatisfied);
            }
            Ok(OperationProjection::StatusChanged {
                status: Box::new(OperationStatus::NetworkRepair {
                    id: id.clone(),
                    target_machine_id: target_machine_id.clone(),
                    state: state.clone(),
                    last_event_sequence: event_sequence,
                }),
            })
        }
        NetworkRepairEvent::Transition(transition) => project_state(
            id,
            target_machine_id,
            state,
            transition.state(),
            event_sequence,
        ),
    }
}

fn project_state(
    id: &OperationId,
    target_machine_id: &Option<MachineId>,
    current: &NetworkRepairOperationState,
    attempted: NetworkRepairOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        current,
        attempted,
        NetworkRepairOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::NetworkRepair,
        |state| OperationStatus::NetworkRepair {
            id: id.clone(),
            target_machine_id: target_machine_id.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &NetworkRepairOperationState,
    attempted: &NetworkRepairOperationState,
) -> bool {
    match (current, attempted) {
        (
            NetworkRepairOperationState::Accepted,
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::PreparingDataplane,
            }
            | NetworkRepairOperationState::Cancelled { .. },
        )
        | (
            NetworkRepairOperationState::Accepted,
            NetworkRepairOperationState::Failed {
                failure: NetworkRepairFailure::ProgressRecordFailed { .. },
            },
        )
        | (
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::PreparingDataplane,
            },
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::RefreshingMachineFacts,
            }
            | NetworkRepairOperationState::Failed { .. }
            | NetworkRepairOperationState::Cancelled { .. },
        )
        | (
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::RefreshingMachineFacts,
            },
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::ConfirmingDnsRefresh,
            }
            | NetworkRepairOperationState::Failed { .. }
            | NetworkRepairOperationState::Cancelled { .. },
        )
        | (
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::ConfirmingDnsRefresh,
            },
            NetworkRepairOperationState::Completed
            | NetworkRepairOperationState::Failed { .. }
            | NetworkRepairOperationState::Cancelled { .. },
        ) => true,
        (
            NetworkRepairOperationState::Accepted
            | NetworkRepairOperationState::Completed
            | NetworkRepairOperationState::Failed { .. }
            | NetworkRepairOperationState::Cancelled { .. },
            _,
        )
        | (
            NetworkRepairOperationState::Running {
                stage:
                    NetworkRepairRunningStage::PreparingDataplane
                    | NetworkRepairRunningStage::RefreshingMachineFacts
                    | NetworkRepairRunningStage::ConfirmingDnsRefresh,
            },
            NetworkRepairOperationState::Accepted
            | NetworkRepairOperationState::Running {
                stage:
                    NetworkRepairRunningStage::PreparingDataplane
                    | NetworkRepairRunningStage::RefreshingMachineFacts
                    | NetworkRepairRunningStage::ConfirmingDnsRefresh,
            },
        )
        | (
            NetworkRepairOperationState::Running {
                stage:
                    NetworkRepairRunningStage::PreparingDataplane
                    | NetworkRepairRunningStage::RefreshingMachineFacts,
            },
            NetworkRepairOperationState::Completed,
        ) => false,
    }
}

pub fn project_network_repair_transition(
    current: &OperationStatus,
    transition: NetworkRepairTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::NetworkRepair {
        id,
        target_machine_id,
        state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::NetworkRepair));
    };
    project_state(
        id,
        target_machine_id,
        state,
        transition.state(),
        event_sequence,
    )
}
