use serde::{Deserialize, Serialize};

use super::{
    DeployOperationState, DeployRunningStage, MachineUpdateOperationState,
    NamespaceRemoveOperationState, NamespaceRemoveRunningStage, NetworkRepairOperationState,
    NetworkRepairRunningStage, OperationKind, OperationStatus, ServiceRestartOperationState,
    ServiceRestartRunningStage, VolumeRemoveOperationState, VolumeRemoveRunningStage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OperationInterruptionCause {
    ControlShutdown,
    PriorProcessLoss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployInterruptionStage {
    Accepted,
    Planning,
    Running { stage: DeployRunningStage },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationInterruptionStage {
    Deploy { stage: DeployInterruptionStage },
    CredentialGrantAccepted,
    IngressConfigureAccepted,
    MachineUpdateAccepted,
    MachineUpdateRunning,
    MachineLifecycleAccepted,
    NetworkRepairAccepted,
    NetworkRepairRunning { stage: NetworkRepairRunningStage },
    ServiceRestartAccepted,
    ServiceRestartRunning { stage: ServiceRestartRunningStage },
    NamespaceRemoveAccepted,
    NamespaceRemoveRunning { stage: NamespaceRemoveRunningStage },
    VolumeRemoveAccepted,
    VolumeRemoveRunning { stage: VolumeRemoveRunningStage },
}

impl OperationInterruptionStage {
    #[must_use]
    pub const fn kind(self) -> OperationKind {
        match self {
            Self::Deploy { .. } => OperationKind::Deploy,
            Self::CredentialGrantAccepted => OperationKind::CredentialGrant,
            Self::IngressConfigureAccepted => OperationKind::IngressConfigure,
            Self::MachineUpdateAccepted | Self::MachineUpdateRunning => {
                OperationKind::MachineUpdate
            }
            Self::MachineLifecycleAccepted => OperationKind::MachineLifecycle,
            Self::NetworkRepairAccepted | Self::NetworkRepairRunning { .. } => {
                OperationKind::NetworkRepair
            }
            Self::ServiceRestartAccepted | Self::ServiceRestartRunning { .. } => {
                OperationKind::ServiceRestart
            }
            Self::NamespaceRemoveAccepted | Self::NamespaceRemoveRunning { .. } => {
                OperationKind::NamespaceRemove
            }
            Self::VolumeRemoveAccepted | Self::VolumeRemoveRunning { .. } => {
                OperationKind::VolumeRemove
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OperationInterruptionUncertainWork {
    Intent,
    Runtime,
    IntentAndRuntime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OperationInterruptionNextAction {
    RetryFromObservedReality,
    InspectThenResubmit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationInterruptionEvidence {
    pub cause: OperationInterruptionCause,
    pub last_durable_stage: OperationInterruptionStage,
    pub uncertain_work: OperationInterruptionUncertainWork,
    pub next_action: OperationInterruptionNextAction,
}

impl OperationInterruptionEvidence {
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.last_durable_stage.kind()
    }

    #[must_use]
    pub fn matches_status(&self, status: &OperationStatus) -> bool {
        status.interruption_evidence(self.cause).as_ref() == Some(self)
    }
}

impl OperationStatus {
    #[must_use]
    pub fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let (last_durable_stage, uncertain_work, next_action) = match self {
            Self::Deploy { state, .. } => {
                let stage = match state {
                    DeployOperationState::Accepted => DeployInterruptionStage::Accepted,
                    DeployOperationState::Planning => DeployInterruptionStage::Planning,
                    DeployOperationState::Running { stage } => {
                        DeployInterruptionStage::Running { stage: *stage }
                    }
                    DeployOperationState::Completed { .. }
                    | DeployOperationState::Failed { .. }
                    | DeployOperationState::Cancelled { .. }
                    | DeployOperationState::Interrupted { .. } => return None,
                };
                (
                    OperationInterruptionStage::Deploy { stage },
                    OperationInterruptionUncertainWork::IntentAndRuntime,
                    OperationInterruptionNextAction::RetryFromObservedReality,
                )
            }
            Self::CredentialGrant { state, .. } => match state {
                super::CredentialGrantOperationState::Accepted => (
                    OperationInterruptionStage::CredentialGrantAccepted,
                    OperationInterruptionUncertainWork::Intent,
                    OperationInterruptionNextAction::InspectThenResubmit,
                ),
                super::CredentialGrantOperationState::Completed
                | super::CredentialGrantOperationState::Failed { .. }
                | super::CredentialGrantOperationState::Cancelled { .. }
                | super::CredentialGrantOperationState::Interrupted { .. } => return None,
            },
            Self::IngressConfigure { state, .. } => match state {
                super::IngressConfigureOperationState::Accepted => (
                    OperationInterruptionStage::IngressConfigureAccepted,
                    OperationInterruptionUncertainWork::Intent,
                    OperationInterruptionNextAction::InspectThenResubmit,
                ),
                super::IngressConfigureOperationState::Completed
                | super::IngressConfigureOperationState::Failed { .. }
                | super::IngressConfigureOperationState::Interrupted { .. } => return None,
            },
            Self::MachineUpdate { state, .. } => match state {
                MachineUpdateOperationState::Accepted => (
                    OperationInterruptionStage::MachineUpdateAccepted,
                    OperationInterruptionUncertainWork::Runtime,
                    OperationInterruptionNextAction::InspectThenResubmit,
                ),
                MachineUpdateOperationState::Running => (
                    OperationInterruptionStage::MachineUpdateRunning,
                    OperationInterruptionUncertainWork::Runtime,
                    OperationInterruptionNextAction::InspectThenResubmit,
                ),
                MachineUpdateOperationState::Completed { .. }
                | MachineUpdateOperationState::Failed { .. }
                | MachineUpdateOperationState::Cancelled { .. }
                | MachineUpdateOperationState::Interrupted { .. } => return None,
            },
            Self::MachineLifecycle { state, .. } => match state {
                super::MachineLifecycleOperationState::Accepted => (
                    OperationInterruptionStage::MachineLifecycleAccepted,
                    OperationInterruptionUncertainWork::Intent,
                    OperationInterruptionNextAction::InspectThenResubmit,
                ),
                super::MachineLifecycleOperationState::Completed
                | super::MachineLifecycleOperationState::Failed { .. }
                | super::MachineLifecycleOperationState::Cancelled { .. }
                | super::MachineLifecycleOperationState::Interrupted { .. } => return None,
            },
            Self::NetworkRepair { state, .. } => {
                let last_durable_stage = match state {
                    NetworkRepairOperationState::Accepted => {
                        OperationInterruptionStage::NetworkRepairAccepted
                    }
                    NetworkRepairOperationState::Running { stage } => {
                        OperationInterruptionStage::NetworkRepairRunning { stage: *stage }
                    }
                    NetworkRepairOperationState::Completed
                    | NetworkRepairOperationState::Failed { .. }
                    | NetworkRepairOperationState::Cancelled { .. }
                    | NetworkRepairOperationState::Interrupted { .. } => return None,
                };
                (
                    last_durable_stage,
                    OperationInterruptionUncertainWork::IntentAndRuntime,
                    OperationInterruptionNextAction::InspectThenResubmit,
                )
            }
            Self::ServiceRestart { state, .. } => {
                let last_durable_stage = match state {
                    ServiceRestartOperationState::Accepted => {
                        OperationInterruptionStage::ServiceRestartAccepted
                    }
                    ServiceRestartOperationState::Running { stage } => {
                        OperationInterruptionStage::ServiceRestartRunning { stage: *stage }
                    }
                    ServiceRestartOperationState::Completed
                    | ServiceRestartOperationState::Failed { .. }
                    | ServiceRestartOperationState::Cancelled { .. }
                    | ServiceRestartOperationState::Interrupted { .. } => return None,
                };
                (
                    last_durable_stage,
                    OperationInterruptionUncertainWork::Runtime,
                    OperationInterruptionNextAction::InspectThenResubmit,
                )
            }
            Self::NamespaceRemove { state, .. } => {
                let last_durable_stage = match state {
                    NamespaceRemoveOperationState::Accepted => {
                        OperationInterruptionStage::NamespaceRemoveAccepted
                    }
                    NamespaceRemoveOperationState::Running { stage } => {
                        OperationInterruptionStage::NamespaceRemoveRunning { stage: *stage }
                    }
                    NamespaceRemoveOperationState::Completed
                    | NamespaceRemoveOperationState::Failed { .. }
                    | NamespaceRemoveOperationState::Cancelled { .. }
                    | NamespaceRemoveOperationState::Interrupted { .. } => return None,
                };
                (
                    last_durable_stage,
                    OperationInterruptionUncertainWork::IntentAndRuntime,
                    OperationInterruptionNextAction::InspectThenResubmit,
                )
            }
            Self::VolumeRemove { state, .. } => {
                let last_durable_stage = match state {
                    VolumeRemoveOperationState::Accepted => {
                        OperationInterruptionStage::VolumeRemoveAccepted
                    }
                    VolumeRemoveOperationState::Running { stage } => {
                        OperationInterruptionStage::VolumeRemoveRunning { stage: *stage }
                    }
                    VolumeRemoveOperationState::Completed
                    | VolumeRemoveOperationState::Failed { .. }
                    | VolumeRemoveOperationState::Interrupted { .. } => return None,
                };
                (
                    last_durable_stage,
                    OperationInterruptionUncertainWork::IntentAndRuntime,
                    OperationInterruptionNextAction::InspectThenResubmit,
                )
            }
            Self::Cert { .. }
            | Self::MachineAdd { .. }
            | Self::CoreReplace { .. }
            | Self::ManagedDnsReconcile { .. } => return None,
        };
        Some(OperationInterruptionEvidence {
            cause,
            last_durable_stage,
            uncertain_work,
            next_action,
        })
    }
}
