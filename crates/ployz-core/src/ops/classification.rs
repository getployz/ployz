use super::{
    CertId, CertRunningStage, CertTransition, DeployEvidence, DeployTransition,
    MachineAddOperationState, MachineId, MachineUpdateTransition, OperationEvent, OperationId,
};
use crate::ops::CancellationReason;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationSubjectRef {
    Cert(CertId),
    MachineAdd(MachineId),
    MachineUpdate(MachineId),
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
    MachineUpdate {
        operation_id: OperationId,
        subject: OperationSubjectRef,
        event: MachineUpdateEvent,
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
            | Self::MachineUpdate { operation_id, .. }
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
            OperationEvent::DeployDataplanePrepared {
                operation_id,
                report,
                ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::DataplanePrepared { report }),
            },
            OperationEvent::DeployContainerStarted {
                operation_id,
                machine_id,
                container_id,
                ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::ContainerStarted {
                    machine_id,
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
            OperationEvent::DeployCompleted {
                operation_id,
                outcome,
                ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Completed { outcome }),
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
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(machine_id),
                event: MachineAddEvent::Submitted,
            },
            OperationEvent::MachineAddJoined {
                operation_id,
                machine_id,
                joined_at,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(machine_id),
                event: MachineAddEvent::Transition(MachineAddOperationState::Joining { joined_at }),
            },
            OperationEvent::MachineAddCredentialProvisioned {
                operation_id,
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(machine_id),
                event: MachineAddEvent::CredentialProvisioned,
            },
            OperationEvent::MachineAddCompleted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(machine_id),
                event: MachineAddEvent::Transition(MachineAddOperationState::Completed),
            },
            OperationEvent::MachineAddFailed {
                operation_id,
                machine_id,
                failure,
                ..
            } => Self::MachineAdd {
                operation_id,
                subject: OperationSubjectRef::MachineAdd(machine_id),
                event: MachineAddEvent::Transition(MachineAddOperationState::Failed { failure }),
            },
            OperationEvent::MachineUpdateSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineUpdate {
                operation_id,
                subject: OperationSubjectRef::MachineUpdate(machine_id),
                event: MachineUpdateEvent::Submitted,
            },
            OperationEvent::MachineUpdateRunning {
                operation_id,
                machine_id,
            } => Self::MachineUpdate {
                operation_id,
                subject: OperationSubjectRef::MachineUpdate(machine_id),
                event: MachineUpdateEvent::Transition(MachineUpdateTransition::Running),
            },
            OperationEvent::MachineUpdateCompleted {
                operation_id,
                machine_id,
                reported,
            } => Self::MachineUpdate {
                operation_id,
                subject: OperationSubjectRef::MachineUpdate(machine_id),
                event: MachineUpdateEvent::Transition(MachineUpdateTransition::Completed {
                    reported,
                }),
            },
            OperationEvent::MachineUpdateFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineUpdate {
                operation_id,
                subject: OperationSubjectRef::MachineUpdate(machine_id),
                event: MachineUpdateEvent::Transition(MachineUpdateTransition::Failed { failure }),
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
    CredentialProvisioned,
    Transition(MachineAddOperationState),
}

pub(super) enum MachineUpdateEvent {
    Submitted,
    Transition(MachineUpdateTransition),
}
