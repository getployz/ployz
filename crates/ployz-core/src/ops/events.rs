//! Durable operation events: the flat stream shape, its subjects and
//! message ids, and the classification back into per-kind events. Every
//! match that must enumerate all event variants lives in this file.

use serde::{Deserialize, Serialize};

use crate::cert::{AcmeHttp01Challenge, ActiveCertState};
use crate::dataplane::PloyzNativeMeshPrepareReport;
use crate::deploy::{DeployCleanupContainer, DeployPlan, DeployRequest};
use crate::ids::{CertId, ContainerId, MachineId, OperationId, ServiceId};
use crate::install::InstallArtifactVersion;
use crate::machine::{
    IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineCredentialProvisioningStep,
    MachineName,
};
use crate::roles::InstallRolePolicy;
use crate::state::MachineLifecycle;

use super::cert::{CertEvent, CertTransition};
use super::deploy::{DeployEvent, DeployEvidence, DeployTransition};
use super::machine_add::MachineAddEvent;
use super::machine_lifecycle::{MachineLifecycleEvent, MachineLifecycleTransition};
use super::machine_update::{MachineUpdateEvent, MachineUpdateTransition};
use super::text::CancellationReason;
use super::{
    CertOperationFailure, CertRunningStage, DeployCleanupFailure, DeployCompletionOutcome,
    DeployOperationFailure, DeployRunningStage, MachineAddOperationState, MachineLifecycleFailure,
    MachineSubstrateVersions, MachineUpdateFailure, OperationKind,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSubject {
    Deploy { service_id: ServiceId },
    Cert { cert_id: CertId },
    MachineAdd { machine_id: MachineId },
    MachineUpdate { machine_id: MachineId },
}

/// Local operation evidence event and plain NATS progress payload.
///
/// Changing this shape intentionally breaks operation replay/history unless
/// paired with evidence cleanup or migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
    DeployDataplanePrepared {
        operation_id: OperationId,
        report: PloyzNativeMeshPrepareReport,
    },
    DeployContainerStarted {
        operation_id: OperationId,
        machine_id: MachineId,
        container_id: ContainerId,
    },
    DeployHealthCheckStarted {
        operation_id: OperationId,
    },
    DeployCleanupFinished {
        operation_id: OperationId,
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
    },
    DeployCompleted {
        operation_id: OperationId,
        outcome: DeployCompletionOutcome,
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
    MachineAddSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        join_token: IssuedJoinToken,
    },
    MachineAddJoined {
        operation_id: OperationId,
        machine_id: MachineId,
        joined_at: JoinTokenRedeemedAt,
    },
    MachineAddCredentialProvisioned {
        operation_id: OperationId,
        machine_id: MachineId,
        step: MachineCredentialProvisioningStep,
    },
    MachineAddCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineAddFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineAddFailure,
    },
    MachineUpdateSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
    },
    MachineUpdateRunning {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineUpdateCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
        reported: MachineSubstrateVersions,
    },
    MachineUpdateFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineUpdateFailure,
    },
    MachineLifecycleSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        target: MachineLifecycle,
    },
    MachineLifecycleCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineLifecycleFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineLifecycleFailure,
    },
    Cancelled {
        operation_id: OperationId,
        kind: OperationKind,
        reason: CancellationReason,
    },
}

impl OperationEvent {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        match self {
            Self::DeploySubmitted { operation_id, .. }
            | Self::DeployPlanningStarted { operation_id }
            | Self::DeployPlanCreated { operation_id, .. }
            | Self::DeployRunning { operation_id, .. }
            | Self::DeployDataplanePrepared { operation_id, .. }
            | Self::DeployContainerStarted { operation_id, .. }
            | Self::DeployHealthCheckStarted { operation_id }
            | Self::DeployCleanupFinished { operation_id, .. }
            | Self::DeployCompleted { operation_id, .. }
            | Self::DeployFailed { operation_id, .. }
            | Self::CertRenewalSubmitted { operation_id, .. }
            | Self::CertChallengePublished { operation_id, .. }
            | Self::CertValidationStarted { operation_id, .. }
            | Self::CertCompleted { operation_id, .. }
            | Self::CertFailed { operation_id, .. }
            | Self::MachineAddSubmitted { operation_id, .. }
            | Self::MachineAddJoined { operation_id, .. }
            | Self::MachineAddCredentialProvisioned { operation_id, .. }
            | Self::MachineAddCompleted { operation_id, .. }
            | Self::MachineAddFailed { operation_id, .. }
            | Self::MachineUpdateSubmitted { operation_id, .. }
            | Self::MachineUpdateRunning { operation_id, .. }
            | Self::MachineUpdateCompleted { operation_id, .. }
            | Self::MachineUpdateFailed { operation_id, .. }
            | Self::MachineLifecycleSubmitted { operation_id, .. }
            | Self::MachineLifecycleCompleted { operation_id, .. }
            | Self::MachineLifecycleFailed { operation_id, .. }
            | Self::Cancelled { operation_id, .. } => operation_id,
        }
    }

    /// The durable stream subject this event publishes under. Subjects are a
    /// persisted contract: renderings must never change for an existing
    /// variant.
    #[must_use]
    pub fn subject(&self) -> String {
        let suffix = match self {
            Self::DeploySubmitted { .. } => "deploy.submitted".to_owned(),
            Self::DeployPlanningStarted { .. } => "deploy.planning.started".to_owned(),
            Self::DeployPlanCreated { .. } => "deploy.plan.created".to_owned(),
            Self::DeployRunning { stage, .. } => format!("deploy.running.{}", stage.as_subject()),
            Self::DeployDataplanePrepared { .. } => "deploy.dataplane.prepared".to_owned(),
            Self::DeployContainerStarted {
                machine_id,
                container_id,
                ..
            } => format!(
                "deploy.container.started.{}.{}",
                machine_id.as_str(),
                container_id.as_str()
            ),
            Self::DeployHealthCheckStarted { .. } => "deploy.health_check.started".to_owned(),
            Self::DeployCleanupFinished { .. } => "deploy.cleanup.finished".to_owned(),
            Self::DeployCompleted { .. } => "deploy.completed".to_owned(),
            Self::DeployFailed { .. } => "deploy.failed".to_owned(),
            Self::CertRenewalSubmitted { .. } => "cert.submitted".to_owned(),
            Self::CertChallengePublished { .. } => "cert.challenge.published".to_owned(),
            Self::CertValidationStarted { .. } => "cert.validation.started".to_owned(),
            Self::CertCompleted { .. } => "cert.completed".to_owned(),
            Self::CertFailed { .. } => "cert.failed".to_owned(),
            Self::MachineAddSubmitted { .. } => "machine.add.submitted".to_owned(),
            Self::MachineAddJoined { .. } => "machine.add.joined".to_owned(),
            Self::MachineAddCredentialProvisioned { step, .. } => {
                format!("machine.add.credential.{}", step.as_subject_token())
            }
            Self::MachineAddCompleted { .. } => "machine.add.completed".to_owned(),
            Self::MachineAddFailed { .. } => "machine.add.failed".to_owned(),
            Self::MachineUpdateSubmitted { .. } => "machine.update.submitted".to_owned(),
            Self::MachineUpdateRunning { .. } => "machine.update.running".to_owned(),
            Self::MachineUpdateCompleted { .. } => "machine.update.completed".to_owned(),
            Self::MachineUpdateFailed { .. } => "machine.update.failed".to_owned(),
            Self::MachineLifecycleSubmitted { target, .. } => match target {
                MachineLifecycle::Active => "machine.lifecycle.resume.submitted".to_owned(),
                MachineLifecycle::Draining => "machine.lifecycle.drain.submitted".to_owned(),
            },
            Self::MachineLifecycleCompleted { .. } => "machine.lifecycle.completed".to_owned(),
            Self::MachineLifecycleFailed { .. } => "machine.lifecycle.failed".to_owned(),
            Self::Cancelled { .. } => "cancelled".to_owned(),
        };
        crate::subjects::op_event_subject(self.operation_id(), &suffix)
    }

    /// The idempotent JetStream message id for this event. Message ids are a
    /// persisted dedup contract: renderings must never change for an existing
    /// variant. Every terminal event of one operation kind shares one id, so
    /// stream dedup enforces "terminal states are final" - a retried terminal
    /// write after a different terminal landed is dropped by the stream, not
    /// by application code.
    #[must_use]
    pub fn message_id(&self) -> String {
        let operation_id = self.operation_id().as_str();
        match self {
            Self::DeploySubmitted { .. }
            | Self::CertRenewalSubmitted { .. }
            | Self::MachineAddSubmitted { .. }
            | Self::MachineUpdateSubmitted { .. }
            | Self::MachineLifecycleSubmitted { .. } => {
                format!("operation.submit.{operation_id}")
            }
            Self::DeployPlanningStarted { .. } => {
                format!("deploy.event.{operation_id}.planning.started")
            }
            Self::DeployRunning { stage, .. } => {
                format!("deploy.event.{operation_id}.running.{}", stage.as_subject())
            }
            Self::DeployPlanCreated { .. } => format!("deploy.plan.created.{operation_id}"),
            Self::DeployDataplanePrepared { .. } => {
                format!("deploy.dataplane.prepared.{operation_id}")
            }
            Self::DeployContainerStarted {
                machine_id,
                container_id,
                ..
            } => format!(
                "deploy.container.started.{operation_id}.{}.{}",
                machine_id.as_str(),
                container_id.as_str()
            ),
            Self::DeployHealthCheckStarted { .. } => {
                format!("deploy.health_check.started.{operation_id}")
            }
            Self::DeployCleanupFinished { .. } => {
                format!("deploy.cleanup.finished.{operation_id}")
            }
            Self::DeployCompleted { .. } | Self::DeployFailed { .. } => {
                format!("deploy.terminal.{operation_id}")
            }
            // A cancel shares the terminal id of its operation kind, so a
            // cancel racing another terminal write dedups at the stream.
            Self::Cancelled { kind, .. } => match kind {
                OperationKind::Deploy => format!("deploy.terminal.{operation_id}"),
                OperationKind::Cert => format!("cert.terminal.{operation_id}"),
                OperationKind::MachineAdd => format!("machine.add.terminal.{operation_id}"),
                OperationKind::MachineUpdate => {
                    format!("machine.update.terminal.{operation_id}")
                }
                OperationKind::MachineLifecycle => {
                    format!("machine.lifecycle.terminal.{operation_id}")
                }
            },
            Self::CertChallengePublished { .. } => {
                format!("cert.challenge.published.{operation_id}")
            }
            Self::CertValidationStarted { .. } => {
                format!("cert.validation.started.{operation_id}")
            }
            Self::CertCompleted { .. } | Self::CertFailed { .. } => {
                format!("cert.terminal.{operation_id}")
            }
            Self::MachineAddJoined { .. } => format!("machine.add.joined.{operation_id}"),
            Self::MachineAddCredentialProvisioned { step, .. } => format!(
                "machine.add.credential.{}.{operation_id}",
                step.as_subject_token()
            ),
            Self::MachineAddCompleted { .. } | Self::MachineAddFailed { .. } => {
                format!("machine.add.terminal.{operation_id}")
            }
            Self::MachineUpdateRunning { .. } => format!("machine.update.running.{operation_id}"),
            Self::MachineUpdateCompleted { .. } | Self::MachineUpdateFailed { .. } => {
                format!("machine.update.terminal.{operation_id}")
            }
            Self::MachineLifecycleCompleted { .. } | Self::MachineLifecycleFailed { .. } => {
                format!("machine.lifecycle.terminal.{operation_id}")
            }
        }
    }
}

/// The subject an event claims to be about, checked against the status
/// record during projection so a misrouted event surfaces as typed
/// evidence instead of silently mutating the wrong operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationSubjectRef {
    Cert(CertId),
    MachineAdd(MachineId),
    MachineUpdate(MachineId),
    MachineLifecycle(MachineId),
}

pub(super) enum ClassifiedOperationEvent {
    Deploy {
        operation_id: OperationId,
        event: DeployEvent,
    },
    Cert {
        operation_id: OperationId,
        event: CertEvent,
    },
    MachineAdd {
        operation_id: OperationId,
        event: MachineAddEvent,
    },
    MachineUpdate {
        operation_id: OperationId,
        event: MachineUpdateEvent,
    },
    MachineLifecycle {
        operation_id: OperationId,
        event: MachineLifecycleEvent,
    },
}

impl ClassifiedOperationEvent {
    pub(super) fn operation_id(&self) -> &OperationId {
        match self {
            Self::Deploy { operation_id, .. }
            | Self::Cert { operation_id, .. }
            | Self::MachineAdd { operation_id, .. }
            | Self::MachineUpdate { operation_id, .. }
            | Self::MachineLifecycle { operation_id, .. } => operation_id,
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
                event: CertEvent::Submitted { cert_id },
            },
            OperationEvent::CertChallengePublished {
                operation_id,
                cert_id,
                ..
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id,
                    transition: CertTransition::Running {
                        stage: CertRunningStage::ChallengePublished,
                    },
                },
            },
            OperationEvent::CertValidationStarted {
                operation_id,
                cert_id,
                ..
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id,
                    transition: CertTransition::Running {
                        stage: CertRunningStage::ValidationStarted,
                    },
                },
            },
            OperationEvent::CertCompleted {
                operation_id,
                active_cert,
                ..
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id: active_cert.cert_id.clone(),
                    transition: CertTransition::Completed,
                },
            },
            OperationEvent::CertFailed {
                operation_id,
                failure,
                ..
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id: failure.cert_id().clone(),
                    transition: CertTransition::Failed { failure },
                },
            },
            OperationEvent::MachineAddSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Submitted { machine_id },
            },
            OperationEvent::MachineAddJoined {
                operation_id,
                machine_id,
                joined_at,
                ..
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Transition {
                    machine_id,
                    state: MachineAddOperationState::Joining { joined_at },
                },
            },
            OperationEvent::MachineAddCredentialProvisioned {
                operation_id,
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::CredentialProvisioned { machine_id },
            },
            OperationEvent::MachineAddCompleted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Transition {
                    machine_id,
                    state: MachineAddOperationState::Completed,
                },
            },
            OperationEvent::MachineAddFailed {
                operation_id,
                machine_id,
                failure,
                ..
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Transition {
                    machine_id,
                    state: MachineAddOperationState::Failed { failure },
                },
            },
            OperationEvent::MachineUpdateSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Submitted { machine_id },
            },
            OperationEvent::MachineUpdateRunning {
                operation_id,
                machine_id,
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Transition {
                    machine_id,
                    transition: MachineUpdateTransition::Running,
                },
            },
            OperationEvent::MachineUpdateCompleted {
                operation_id,
                machine_id,
                reported,
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Transition {
                    machine_id,
                    transition: MachineUpdateTransition::Completed { reported },
                },
            },
            OperationEvent::MachineUpdateFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Transition {
                    machine_id,
                    transition: MachineUpdateTransition::Failed { failure },
                },
            },
            OperationEvent::MachineLifecycleSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineLifecycle {
                operation_id,
                event: MachineLifecycleEvent::Submitted { machine_id },
            },
            OperationEvent::MachineLifecycleCompleted {
                operation_id,
                machine_id,
            } => Self::MachineLifecycle {
                operation_id,
                event: MachineLifecycleEvent::Transition {
                    machine_id,
                    transition: MachineLifecycleTransition::Completed,
                },
            },
            OperationEvent::MachineLifecycleFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineLifecycle {
                operation_id,
                event: MachineLifecycleEvent::Transition {
                    machine_id,
                    transition: MachineLifecycleTransition::Failed { failure },
                },
            },
            // A cancel classifies by the kind it claims, so a cancel whose
            // kind disagrees with the status record surfaces as a kind
            // mismatch in projection instead of silently cancelling.
            OperationEvent::Cancelled {
                operation_id,
                kind,
                reason,
            } => match kind {
                OperationKind::Deploy => Self::Deploy {
                    operation_id,
                    event: DeployEvent::Transition(DeployTransition::Cancelled { reason }),
                },
                OperationKind::Cert => Self::Cert {
                    operation_id,
                    event: CertEvent::Cancelled(reason),
                },
                OperationKind::MachineAdd => Self::MachineAdd {
                    operation_id,
                    event: MachineAddEvent::Cancelled(reason),
                },
                OperationKind::MachineUpdate => Self::MachineUpdate {
                    operation_id,
                    event: MachineUpdateEvent::Cancelled(reason),
                },
                OperationKind::MachineLifecycle => Self::MachineLifecycle {
                    operation_id,
                    event: MachineLifecycleEvent::Cancelled(reason),
                },
            },
        }
    }
}
