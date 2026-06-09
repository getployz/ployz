use super::backup::BackupEvent;
use super::{
    BackupTransition, CertId, CertRunningStage, CertTransition, DeployEvidence, DeployTransition,
    MachineAddOperationState, NodeId, OperationEvent, OperationId,
};
use crate::ops::CancellationReason;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationSubjectRef {
    Cert(CertId),
    MachineAdd(NodeId),
}

pub(super) enum ClassifiedOperationEvent {
    Deploy {
        operation_id: OperationId,
        event: DeployEvent,
    },
    Cert {
        operation_id: OperationId,
        subject: OperationSubjectRef,
        event: CertEvent,
    },
    MachineAdd {
        operation_id: OperationId,
        subject: OperationSubjectRef,
        event: MachineAddEvent,
    },
    Backup {
        operation_id: OperationId,
        event: BackupEvent,
    },
    Cancelled {
        operation_id: OperationId,
        reason: CancellationReason,
    },
}

impl ClassifiedOperationEvent {
    pub(super) fn operation_id(&self) -> &OperationId {
        match self {
            Self::Deploy { operation_id, .. }
            | Self::Cert { operation_id, .. }
            | Self::MachineAdd { operation_id, .. }
            | Self::Backup { operation_id, .. }
            | Self::Cancelled { operation_id, .. } => operation_id,
        }
    }
}

impl From<OperationEvent> for ClassifiedOperationEvent {
    fn from(event: OperationEvent) -> Self {
        match event {
            OperationEvent::DeploySubmitted { operation_id, .. } => Self::Deploy {
                operation_id,
                event: DeployEvent::Submitted,
            },
            OperationEvent::DeployPlanningStarted { operation_id, .. } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Planning),
            },
            OperationEvent::DeployPlanCreated {
                operation_id, plan, ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::PlanCreated { plan }),
            },
            OperationEvent::DeployRunning {
                operation_id,
                stage,
                ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Running { stage }),
            },
            OperationEvent::DeployContainerStarted {
                operation_id,
                node_id,
                container_id,
                ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::ContainerStarted {
                    node_id,
                    container_id,
                }),
            },
            OperationEvent::DeployHealthCheckStarted { operation_id, .. } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::HealthCheckStarted),
            },
            OperationEvent::DeployCleanupFinished {
                operation_id,
                removed,
                failed,
                ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::CleanupFinished { removed, failed }),
            },
            OperationEvent::DeployCompleted { operation_id, .. } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Completed),
            },
            OperationEvent::DeployFailed {
                operation_id,
                failure,
                ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Failed { failure }),
            },
            OperationEvent::CertRenewalSubmitted {
                operation_id,
                cert_id,
                ..
            } => Self::Cert {
                operation_id,
                subject: OperationSubjectRef::Cert(cert_id),
                event: CertEvent::Submitted,
            },
            OperationEvent::CertChallengePublished {
                operation_id,
                cert_id,
                ..
            } => Self::Cert {
                operation_id,
                subject: OperationSubjectRef::Cert(cert_id),
                event: CertEvent::Transition(CertTransition::Running {
                    stage: CertRunningStage::ChallengePublished,
                }),
            },
            OperationEvent::CertValidationStarted {
                operation_id,
                cert_id,
                ..
            } => Self::Cert {
                operation_id,
                subject: OperationSubjectRef::Cert(cert_id),
                event: CertEvent::Transition(CertTransition::Running {
                    stage: CertRunningStage::ValidationStarted,
                }),
            },
            OperationEvent::CertCompleted {
                operation_id,
                active_cert,
                ..
            } => Self::Cert {
                operation_id,
                subject: OperationSubjectRef::Cert(active_cert.cert_id.clone()),
                event: CertEvent::Transition(CertTransition::Completed),
            },
            OperationEvent::CertFailed {
                operation_id,
                failure,
                ..
            } => Self::Cert {
                operation_id,
                subject: OperationSubjectRef::Cert(failure.cert_id().clone()),
                event: CertEvent::Transition(CertTransition::Failed { failure }),
            },
            OperationEvent::MachineAddSubmitted {
                operation_id,
                node_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(node_id),
                event: MachineAddEvent::Submitted,
            },
            OperationEvent::MachineAddJoined {
                operation_id,
                node_id,
                joined_at,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(node_id),
                event: MachineAddEvent::Transition(MachineAddOperationState::Joining { joined_at }),
            },
            OperationEvent::MachineAddCompleted {
                operation_id,
                node_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(node_id),
                event: MachineAddEvent::Transition(MachineAddOperationState::Completed),
            },
            OperationEvent::MachineAddFailed {
                operation_id,
                node_id,
                failure,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(node_id),
                event: MachineAddEvent::Transition(MachineAddOperationState::Failed { failure }),
            },
            OperationEvent::BackupCreateSubmitted { operation_id } => Self::Backup {
                operation_id,
                event: BackupEvent::Submitted,
            },
            OperationEvent::BackupRunning {
                operation_id,
                stage,
                ..
            } => Self::Backup {
                operation_id,
                event: BackupEvent::Transition(BackupTransition::Running { stage }),
            },
            OperationEvent::BackupCompleted {
                operation_id,
                manifest,
                ..
            } => Self::Backup {
                operation_id,
                event: BackupEvent::Transition(BackupTransition::Completed { manifest }),
            },
            OperationEvent::BackupFailed {
                operation_id,
                failure,
                ..
            } => Self::Backup {
                operation_id,
                event: BackupEvent::Transition(BackupTransition::Failed { failure }),
            },
            OperationEvent::Cancelled {
                operation_id,
                reason,
                ..
            } => Self::Cancelled {
                operation_id,
                reason,
            },
        }
    }
}

pub(super) enum DeployEvent {
    Submitted,
    Evidence(DeployEvidence),
    Transition(DeployTransition),
}

pub(super) enum CertEvent {
    Submitted,
    Transition(CertTransition),
}

pub(super) enum MachineAddEvent {
    Submitted,
    Transition(MachineAddOperationState),
}
