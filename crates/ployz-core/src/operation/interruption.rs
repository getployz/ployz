use serde::{Deserialize, Serialize};

use super::{
    CredentialGrantOperationState, DeployOperationState, DeployRunningStage,
    IngressConfigureOperationState, MachineLifecycleOperationState, MachineUpdateOperationState,
    NamespaceRemoveOperationState, NamespaceRemoveRunningStage, NetworkRepairOperationState,
    NetworkRepairRunningStage, OperationKind, OperationStatus, ServiceRestartOperationState,
    ServiceRestartRunningStage, VolumeRemoveOperationState, VolumeRemoveRunningStage,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OperationInterruptionCause {
    CoreShutdown,
    PriorCoreProcessLoss,
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

    #[must_use]
    pub const fn uncertain_work(self) -> OperationInterruptionUncertainWork {
        match self {
            Self::CredentialGrantAccepted
            | Self::IngressConfigureAccepted
            | Self::MachineLifecycleAccepted => OperationInterruptionUncertainWork::Intent,
            Self::MachineUpdateAccepted
            | Self::MachineUpdateRunning
            | Self::ServiceRestartAccepted
            | Self::ServiceRestartRunning { .. } => OperationInterruptionUncertainWork::Runtime,
            Self::Deploy { .. }
            | Self::NetworkRepairAccepted
            | Self::NetworkRepairRunning { .. }
            | Self::NamespaceRemoveAccepted
            | Self::NamespaceRemoveRunning { .. }
            | Self::VolumeRemoveAccepted
            | Self::VolumeRemoveRunning { .. } => {
                OperationInterruptionUncertainWork::IntentAndRuntime
            }
        }
    }

    #[must_use]
    pub const fn next_action(self) -> OperationInterruptionNextAction {
        match self {
            Self::Deploy { .. } => OperationInterruptionNextAction::RetryFromObservedReality,
            Self::CredentialGrantAccepted
            | Self::IngressConfigureAccepted
            | Self::MachineUpdateAccepted
            | Self::MachineUpdateRunning
            | Self::MachineLifecycleAccepted
            | Self::NetworkRepairAccepted
            | Self::NetworkRepairRunning { .. }
            | Self::ServiceRestartAccepted
            | Self::ServiceRestartRunning { .. }
            | Self::NamespaceRemoveAccepted
            | Self::NamespaceRemoveRunning { .. }
            | Self::VolumeRemoveAccepted
            | Self::VolumeRemoveRunning { .. } => {
                OperationInterruptionNextAction::InspectThenResubmit
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
#[cfg_attr(
    feature = "typescript",
    ts(
        type = "{ cause: OperationInterruptionCause, last_durable_stage: OperationInterruptionStage, kind: OperationKind, uncertain_work: OperationInterruptionUncertainWork, next_action: OperationInterruptionNextAction }"
    )
)]
#[serde(
    try_from = "OperationInterruptionEvidenceWire",
    into = "OperationInterruptionEvidenceWire"
)]
pub struct OperationInterruptionEvidence {
    cause: OperationInterruptionCause,
    last_durable_stage: OperationInterruptionStage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationInterruptionEvidenceWire {
    cause: OperationInterruptionCause,
    last_durable_stage: OperationInterruptionStage,
    kind: OperationKind,
    uncertain_work: OperationInterruptionUncertainWork,
    next_action: OperationInterruptionNextAction,
}

impl OperationInterruptionEvidence {
    pub(super) const fn new(
        cause: OperationInterruptionCause,
        last_durable_stage: OperationInterruptionStage,
    ) -> Self {
        Self {
            cause,
            last_durable_stage,
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
        self.last_durable_stage.uncertain_work()
    }

    #[must_use]
    pub const fn next_action(&self) -> OperationInterruptionNextAction {
        self.last_durable_stage.next_action()
    }

    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.last_durable_stage.kind()
    }
}

impl TryFrom<OperationInterruptionEvidenceWire> for OperationInterruptionEvidence {
    type Error = OperationInterruptionEvidenceWireError;

    fn try_from(wire: OperationInterruptionEvidenceWire) -> Result<Self, Self::Error> {
        let evidence = Self::new(wire.cause, wire.last_durable_stage);
        if wire.kind != evidence.kind()
            || wire.uncertain_work != evidence.uncertain_work()
            || wire.next_action != evidence.next_action()
        {
            return Err(OperationInterruptionEvidenceWireError);
        }
        Ok(evidence)
    }
}

impl From<OperationInterruptionEvidence> for OperationInterruptionEvidenceWire {
    fn from(evidence: OperationInterruptionEvidence) -> Self {
        Self {
            cause: evidence.cause(),
            last_durable_stage: evidence.last_durable_stage(),
            kind: evidence.kind(),
            uncertain_work: evidence.uncertain_work(),
            next_action: evidence.next_action(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("interruption evidence derived fields do not match its durable stage")]
pub struct OperationInterruptionEvidenceWireError;

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
            Self::Deploy {
                state: DeployOperationState::Interrupted { evidence },
                ..
            }
            | Self::CredentialGrant {
                state: CredentialGrantOperationState::Interrupted { evidence },
                ..
            }
            | Self::IngressConfigure {
                state: IngressConfigureOperationState::Interrupted { evidence },
                ..
            }
            | Self::MachineUpdate {
                state: MachineUpdateOperationState::Interrupted { evidence },
                ..
            }
            | Self::MachineLifecycle {
                state: MachineLifecycleOperationState::Interrupted { evidence },
                ..
            }
            | Self::NetworkRepair {
                state: NetworkRepairOperationState::Interrupted { evidence },
                ..
            }
            | Self::ServiceRestart {
                state: ServiceRestartOperationState::Interrupted { evidence },
                ..
            }
            | Self::NamespaceRemove {
                state: NamespaceRemoveOperationState::Interrupted { evidence },
                ..
            }
            | Self::VolumeRemove {
                state: VolumeRemoveOperationState::Interrupted { evidence },
                ..
            } => Some(evidence),
            Self::Deploy { .. }
            | Self::CredentialGrant { .. }
            | Self::IngressConfigure { .. }
            | Self::MachineUpdate { .. }
            | Self::MachineLifecycle { .. }
            | Self::NetworkRepair { .. }
            | Self::ServiceRestart { .. }
            | Self::NamespaceRemove { .. }
            | Self::VolumeRemove { .. }
            | Self::Cert { .. }
            | Self::MachineAdd { .. }
            | Self::CoreReplace { .. }
            | Self::ManagedDnsReconcile { .. } => None,
        }
    }
}
