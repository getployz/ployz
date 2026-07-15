use serde::{Deserialize, Serialize};

use super::{
    DeployRunningStage, NamespaceRemoveRunningStage, NetworkRepairRunningStage, OperationKind,
    OperationStatus, ServiceRestartRunningStage, VolumeRemoveRunningStage,
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
    cause: OperationInterruptionCause,
    last_durable_stage: OperationInterruptionStage,
    uncertain_work: OperationInterruptionUncertainWork,
    next_action: OperationInterruptionNextAction,
}

impl OperationInterruptionEvidence {
    pub(super) const fn new(
        cause: OperationInterruptionCause,
        last_durable_stage: OperationInterruptionStage,
        uncertain_work: OperationInterruptionUncertainWork,
        next_action: OperationInterruptionNextAction,
    ) -> Self {
        Self {
            cause,
            last_durable_stage,
            uncertain_work,
            next_action,
        }
    }

    #[must_use]
    pub const fn cause(&self) -> OperationInterruptionCause {
        self.cause
    }

    #[must_use]
    pub const fn last_durable_stage(&self) -> OperationInterruptionStage {
        self.last_durable_stage
    }

    #[must_use]
    pub const fn uncertain_work(&self) -> OperationInterruptionUncertainWork {
        self.uncertain_work
    }

    #[must_use]
    pub const fn next_action(&self) -> OperationInterruptionNextAction {
        self.next_action
    }

    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.last_durable_stage.kind()
    }
}

impl OperationStatus {
    #[must_use]
    pub fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        match self {
            Self::Deploy { state, .. } => state.interruption_evidence(cause),
            Self::CredentialGrant { state, .. } => state.interruption_evidence(cause),
            Self::IngressConfigure { state, .. } => state.interruption_evidence(cause),
            Self::MachineUpdate { state, .. } => state.interruption_evidence(cause),
            Self::MachineLifecycle { state, .. } => state.interruption_evidence(cause),
            Self::NetworkRepair { state, .. } => state.interruption_evidence(cause),
            Self::ServiceRestart { state, .. } => state.interruption_evidence(cause),
            Self::NamespaceRemove { state, .. } => state.interruption_evidence(cause),
            Self::VolumeRemove { state, .. } => state.interruption_evidence(cause),
            Self::Cert { .. }
            | Self::MachineAdd { .. }
            | Self::CoreReplace { .. }
            | Self::ManagedDnsReconcile { .. } => None,
        }
    }

    #[must_use]
    pub const fn terminal_interruption_evidence(&self) -> Option<&OperationInterruptionEvidence> {
        match self {
            Self::Deploy { state, .. } => state.terminal_interruption_evidence(),
            Self::CredentialGrant { state, .. } => state.terminal_interruption_evidence(),
            Self::IngressConfigure { state, .. } => state.terminal_interruption_evidence(),
            Self::MachineUpdate { state, .. } => state.terminal_interruption_evidence(),
            Self::MachineLifecycle { state, .. } => state.terminal_interruption_evidence(),
            Self::NetworkRepair { state, .. } => state.terminal_interruption_evidence(),
            Self::ServiceRestart { state, .. } => state.terminal_interruption_evidence(),
            Self::NamespaceRemove { state, .. } => state.terminal_interruption_evidence(),
            Self::VolumeRemove { state, .. } => state.terminal_interruption_evidence(),
            Self::Cert { .. }
            | Self::MachineAdd { .. }
            | Self::CoreReplace { .. }
            | Self::ManagedDnsReconcile { .. } => None,
        }
    }
}
