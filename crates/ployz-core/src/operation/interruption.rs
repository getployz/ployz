use serde::{Deserialize, Serialize};

use super::{
    BuildOperationState, CredentialGrantOperationState, DeployOperationState, DeployRunningStage,
    IngressConfigureOperationState, MachineLifecycleOperationState,
    MachineStoragePrepareOperationState, MachineUpdateOperationState,
    NamespaceRemoveOperationState, NamespaceRemoveRunningStage, NetworkRepairOperationState,
    NetworkRepairRunningStage, OperationKind, OperationStatus, ServiceRestartOperationState,
    ServiceRestartRunningStage, VolumeCreateOperationState, VolumeCreateRunningStage,
    VolumeRemoveOperationState, VolumeRemoveRunningStage,
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
#[serde(rename_all = "snake_case")]
pub enum BuildInterruptionStage {
    Accepted,
    Placing,
    Building,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationInterruptionStage {
    Build { stage: BuildInterruptionStage },
    Deploy { stage: DeployInterruptionStage },
    CredentialGrantAccepted,
    IngressConfigureAccepted,
    MachineUpdateAccepted,
    MachineUpdateRunning,
    MachineStoragePrepareAccepted,
    MachineStoragePreparePreparing,
    MachineLifecycleAccepted,
    NetworkRepairAccepted,
    NetworkRepairRunning { stage: NetworkRepairRunningStage },
    ServiceRestartAccepted,
    ServiceRestartRunning { stage: ServiceRestartRunningStage },
    NamespaceRemoveAccepted,
    NamespaceRemoveRunning { stage: NamespaceRemoveRunningStage },
    VolumeRemoveAccepted,
    VolumeRemoveRunning { stage: VolumeRemoveRunningStage },
    VolumeCreateAccepted,
    VolumeCreatePlanning,
    VolumeCreateRunning { stage: VolumeCreateRunningStage },
}

impl OperationInterruptionStage {
    #[must_use]
    pub const fn kind(self) -> OperationKind {
        match self {
            Self::Build { .. } => OperationKind::Build,
            Self::Deploy { .. } => OperationKind::Deploy,
            Self::CredentialGrantAccepted => OperationKind::CredentialGrant,
            Self::IngressConfigureAccepted => OperationKind::IngressConfigure,
            Self::MachineUpdateAccepted | Self::MachineUpdateRunning => {
                OperationKind::MachineUpdate
            }
            Self::MachineStoragePrepareAccepted | Self::MachineStoragePreparePreparing => {
                OperationKind::MachineStoragePrepare
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
            Self::VolumeCreateAccepted
            | Self::VolumeCreatePlanning
            | Self::VolumeCreateRunning { .. } => OperationKind::VolumeCreate,
        }
    }

    #[must_use]
    pub const fn uncertain_work(self) -> OperationInterruptionUncertainWork {
        match self {
            Self::Build { .. } => OperationInterruptionUncertainWork::Runtime,
            Self::CredentialGrantAccepted
            | Self::IngressConfigureAccepted
            | Self::MachineLifecycleAccepted => OperationInterruptionUncertainWork::Intent,
            Self::MachineUpdateAccepted
            | Self::MachineUpdateRunning
            | Self::MachineStoragePrepareAccepted
            | Self::MachineStoragePreparePreparing
            | Self::ServiceRestartAccepted
            | Self::ServiceRestartRunning { .. } => OperationInterruptionUncertainWork::Runtime,
            Self::Deploy { .. }
            | Self::NetworkRepairAccepted
            | Self::NetworkRepairRunning { .. }
            | Self::NamespaceRemoveAccepted
            | Self::NamespaceRemoveRunning { .. }
            | Self::VolumeRemoveAccepted
            | Self::VolumeRemoveRunning { .. }
            | Self::VolumeCreateAccepted
            | Self::VolumeCreatePlanning
            | Self::VolumeCreateRunning { .. } => {
                OperationInterruptionUncertainWork::IntentAndRuntime
            }
        }
    }

    #[must_use]
    pub const fn next_action(self) -> OperationInterruptionNextAction {
        match self {
            Self::Build { .. } => OperationInterruptionNextAction::InspectThenResubmit,
            Self::Deploy { .. } => OperationInterruptionNextAction::RetryFromObservedReality,
            Self::CredentialGrantAccepted
            | Self::IngressConfigureAccepted
            | Self::MachineUpdateAccepted
            | Self::MachineUpdateRunning
            | Self::MachineStoragePrepareAccepted
            | Self::MachineStoragePreparePreparing
            | Self::MachineLifecycleAccepted
            | Self::NetworkRepairAccepted
            | Self::NetworkRepairRunning { .. }
            | Self::ServiceRestartAccepted
            | Self::ServiceRestartRunning { .. }
            | Self::NamespaceRemoveAccepted
            | Self::NamespaceRemoveRunning { .. }
            | Self::VolumeRemoveAccepted
            | Self::VolumeRemoveRunning { .. }
            | Self::VolumeCreateAccepted
            | Self::VolumeCreatePlanning
            | Self::VolumeCreateRunning { .. } => {
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
    from = "OperationInterruptionEvidenceInput",
    into = "OperationInterruptionEvidenceOutput"
)]
pub struct OperationInterruptionEvidence {
    cause: OperationInterruptionCause,
    last_durable_stage: OperationInterruptionStage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationInterruptionEvidenceInput {
    cause: OperationInterruptionCause,
    last_durable_stage: OperationInterruptionStage,
    #[serde(default, rename = "kind")]
    _kind: Option<serde::de::IgnoredAny>,
    #[serde(default, rename = "uncertain_work")]
    _uncertain_work: Option<serde::de::IgnoredAny>,
    #[serde(default, rename = "next_action")]
    _next_action: Option<serde::de::IgnoredAny>,
}

#[derive(Serialize)]
struct OperationInterruptionEvidenceOutput {
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

impl From<OperationInterruptionEvidenceInput> for OperationInterruptionEvidence {
    fn from(wire: OperationInterruptionEvidenceInput) -> Self {
        let OperationInterruptionEvidenceInput {
            cause,
            last_durable_stage,
            _kind: _,
            _uncertain_work: _,
            _next_action: _,
        } = wire;
        Self::new(cause, last_durable_stage)
    }
}

impl From<OperationInterruptionEvidence> for OperationInterruptionEvidenceOutput {
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

impl OperationStatus {
    #[must_use]
    pub fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        match self {
            Self::Build { state, .. } => state.interruption_evidence(cause),
            Self::Deploy { state, .. } => state.interruption_evidence(cause),
            Self::CredentialGrant { state, .. } => state.interruption_evidence(cause),
            Self::IngressConfigure { state, .. } => state.interruption_evidence(cause),
            Self::MachineUpdate { state, .. } => state.interruption_evidence(cause),
            Self::MachineStoragePrepare { state, .. } => state.interruption_evidence(cause),
            Self::MachineLifecycle { state, .. } => state.interruption_evidence(cause),
            Self::NetworkRepair { state, .. } => state.interruption_evidence(cause),
            Self::ServiceRestart { state, .. } => state.interruption_evidence(cause),
            Self::NamespaceRemove { state, .. } => state.interruption_evidence(cause),
            Self::VolumeRemove { state, .. } => state.interruption_evidence(cause),
            Self::VolumeCreate { state, .. } => state.interruption_evidence(cause),
            Self::Cert { .. }
            | Self::MachineAdd { .. }
            | Self::CoreReplace { .. }
            | Self::ManagedDnsReconcile { .. } => None,
        }
    }

    #[must_use]
    pub const fn terminal_interruption_evidence(&self) -> Option<&OperationInterruptionEvidence> {
        match self {
            Self::Build {
                state: BuildOperationState::Interrupted { evidence },
                ..
            }
            | Self::Deploy {
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
            | Self::MachineStoragePrepare {
                state: MachineStoragePrepareOperationState::Interrupted { evidence },
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
            }
            | Self::VolumeCreate {
                state: VolumeCreateOperationState::Interrupted { evidence },
                ..
            } => Some(evidence),
            Self::Build { .. }
            | Self::Deploy { .. }
            | Self::CredentialGrant { .. }
            | Self::IngressConfigure { .. }
            | Self::MachineUpdate { .. }
            | Self::MachineStoragePrepare { .. }
            | Self::MachineLifecycle { .. }
            | Self::NetworkRepair { .. }
            | Self::ServiceRestart { .. }
            | Self::NamespaceRemove { .. }
            | Self::VolumeRemove { .. }
            | Self::VolumeCreate { .. }
            | Self::Cert { .. }
            | Self::MachineAdd { .. }
            | Self::CoreReplace { .. }
            | Self::ManagedDnsReconcile { .. } => None,
        }
    }
}
