//! Durable operation events: the flat stream shape, its subjects and
//! message ids, and the classification back into per-kind events.

use serde::{Deserialize, Serialize};

use crate::cert::{AcmeHttp01Challenge, ActiveCertState};
use crate::dataplane::PloyzNativeMeshPrepareReport;
use crate::deploy::{DeployCleanupContainer, DeployPlan, DeployRequest};
use crate::ids::{CertId, ContainerId, MachineId, NamespaceId, OperationId, ServiceId};
use crate::install::{InstallArtifactVersion, MachineJoinRuntimeNatsUrl};
use crate::machine::{
    IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineCredentialProvisioningStep,
    MachineName,
};
use crate::roles::InstallRolePolicy;
use crate::state::MachineLifecycle;

use super::cert::{CertEvent, CertTransition};
use super::core_replace::{CoreReplaceEvent, CoreReplaceFailure, CoreReplaceTransition};
use super::deploy::{DeployEvent, DeployEvidence, DeployTransition};
use super::machine_add::MachineAddEvent;
use super::machine_lifecycle::{MachineLifecycleEvent, MachineLifecycleTransition};
use super::machine_update::{MachineUpdateEvent, MachineUpdateTransition};
use super::namespace_remove::{NamespaceRemoveEvent, NamespaceRemoveTransition};
use super::service_restart::{ServiceRestartEvent, ServiceRestartTransition};
use super::text::CancellationReason;
use super::{
    CertOperationFailure, CertRunningStage, DeployCleanupFailure, DeployCompletionOutcome,
    DeployOperationFailure, DeployRunningStage, MachineAddOperationState, MachineLifecycleFailure,
    MachineSubstrateVersions, MachineUpdateFailure, NamespaceRemoveFailure,
    NamespaceRemoveRunningStage, OperationKind, RouteTarget, ServiceRestartFailure,
    ServiceRestartRunningStage,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSubject {
    Deploy { service_id: ServiceId },
    Cert { cert_id: CertId },
    MachineAdd { machine_id: MachineId },
    MachineUpdate { machine_id: MachineId },
    CoreReplace { machine_id: MachineId },
    ServiceRestart { service_id: ServiceId },
    NamespaceRemove { namespace_id: NamespaceId },
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
    CoreReplaceSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        successor_nats_url: MachineJoinRuntimeNatsUrl,
    },
    CoreReplaceCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    CoreReplaceFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: CoreReplaceFailure,
    },
    ServiceRestartSubmitted {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    ServiceRestartRunning {
        operation_id: OperationId,
        stage: ServiceRestartRunningStage,
    },
    ServiceRestartContainerRestarted {
        operation_id: OperationId,
        machine_id: MachineId,
        container_id: ContainerId,
    },
    ServiceRestartCompleted {
        operation_id: OperationId,
    },
    ServiceRestartFailed {
        operation_id: OperationId,
        failure: ServiceRestartFailure,
    },
    NamespaceRemoveSubmitted {
        operation_id: OperationId,
        namespace_id: NamespaceId,
    },
    NamespaceRemoveRunning {
        operation_id: OperationId,
        stage: NamespaceRemoveRunningStage,
    },
    NamespaceRemoveRouteBindingRemoved {
        operation_id: OperationId,
        target: RouteTarget,
    },
    NamespaceRemoveContainerRemoved {
        operation_id: OperationId,
        machine_id: MachineId,
        container_id: ContainerId,
    },
    NamespaceRemoveCompleted {
        operation_id: OperationId,
    },
    NamespaceRemoveFailed {
        operation_id: OperationId,
        failure: NamespaceRemoveFailure,
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
            | Self::CoreReplaceSubmitted { operation_id, .. }
            | Self::CoreReplaceCompleted { operation_id, .. }
            | Self::CoreReplaceFailed { operation_id, .. }
            | Self::ServiceRestartSubmitted { operation_id, .. }
            | Self::ServiceRestartRunning { operation_id, .. }
            | Self::ServiceRestartContainerRestarted { operation_id, .. }
            | Self::ServiceRestartCompleted { operation_id }
            | Self::ServiceRestartFailed { operation_id, .. }
            | Self::NamespaceRemoveSubmitted { operation_id, .. }
            | Self::NamespaceRemoveRunning { operation_id, .. }
            | Self::NamespaceRemoveRouteBindingRemoved { operation_id, .. }
            | Self::NamespaceRemoveContainerRemoved { operation_id, .. }
            | Self::NamespaceRemoveCompleted { operation_id }
            | Self::NamespaceRemoveFailed { operation_id, .. }
            | Self::Cancelled { operation_id, .. } => operation_id,
        }
    }

    /// The subject key of singleton deploy evidence — evidence recorded once
    /// per deploy (plan, dataplane, health-check start, cleanup). `None` for
    /// multi-instance evidence (per-container starts) and every non-evidence
    /// event. The store keys idempotent singleton evidence on this.
    #[must_use]
    pub fn singleton_subject(&self) -> Option<&'static str> {
        match self {
            Self::DeployPlanCreated { .. } => Some("deploy.plan.created"),
            Self::DeployDataplanePrepared { .. } => Some("deploy.dataplane.prepared"),
            Self::DeployHealthCheckStarted { .. } => Some("deploy.health_check.started"),
            Self::DeployCleanupFinished { .. } => Some("deploy.cleanup.finished"),
            Self::DeploySubmitted { .. }
            | Self::DeployPlanningStarted { .. }
            | Self::DeployContainerStarted { .. }
            | Self::DeployRunning { .. }
            | Self::DeployCompleted { .. }
            | Self::DeployFailed { .. }
            | Self::CertRenewalSubmitted { .. }
            | Self::CertChallengePublished { .. }
            | Self::CertValidationStarted { .. }
            | Self::CertCompleted { .. }
            | Self::CertFailed { .. }
            | Self::MachineAddSubmitted { .. }
            | Self::MachineAddJoined { .. }
            | Self::MachineAddCredentialProvisioned { .. }
            | Self::MachineAddCompleted { .. }
            | Self::MachineAddFailed { .. }
            | Self::MachineUpdateSubmitted { .. }
            | Self::MachineUpdateRunning { .. }
            | Self::MachineUpdateCompleted { .. }
            | Self::MachineUpdateFailed { .. }
            | Self::MachineLifecycleSubmitted { .. }
            | Self::MachineLifecycleCompleted { .. }
            | Self::MachineLifecycleFailed { .. }
            | Self::CoreReplaceSubmitted { .. }
            | Self::CoreReplaceCompleted { .. }
            | Self::CoreReplaceFailed { .. }
            | Self::ServiceRestartSubmitted { .. }
            | Self::ServiceRestartRunning { .. }
            | Self::ServiceRestartContainerRestarted { .. }
            | Self::ServiceRestartCompleted { .. }
            | Self::ServiceRestartFailed { .. }
            | Self::NamespaceRemoveSubmitted { .. }
            | Self::NamespaceRemoveRunning { .. }
            | Self::NamespaceRemoveRouteBindingRemoved { .. }
            | Self::NamespaceRemoveContainerRemoved { .. }
            | Self::NamespaceRemoveCompleted { .. }
            | Self::NamespaceRemoveFailed { .. }
            | Self::Cancelled { .. } => None,
        }
    }

    /// The deploy evidence this event carries, if any. `None` for non-evidence
    /// deploy transitions and every other operation kind.
    #[must_use]
    pub fn deploy_evidence(&self) -> Option<DeployEvidence> {
        match self {
            Self::DeployPlanCreated { plan, .. } => {
                Some(DeployEvidence::PlanCreated { plan: plan.clone() })
            }
            Self::DeployDataplanePrepared { report, .. } => {
                Some(DeployEvidence::DataplanePrepared {
                    report: report.clone(),
                })
            }
            Self::DeployContainerStarted {
                machine_id,
                container_id,
                ..
            } => Some(DeployEvidence::ContainerStarted {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
            }),
            Self::DeployHealthCheckStarted { .. } => Some(DeployEvidence::HealthCheckStarted),
            Self::DeployCleanupFinished {
                removed, failed, ..
            } => Some(DeployEvidence::CleanupFinished {
                removed: removed.clone(),
                failed: failed.clone(),
            }),
            Self::DeploySubmitted { .. }
            | Self::DeployPlanningStarted { .. }
            | Self::DeployRunning { .. }
            | Self::DeployCompleted { .. }
            | Self::DeployFailed { .. }
            | Self::CertRenewalSubmitted { .. }
            | Self::CertChallengePublished { .. }
            | Self::CertValidationStarted { .. }
            | Self::CertCompleted { .. }
            | Self::CertFailed { .. }
            | Self::MachineAddSubmitted { .. }
            | Self::MachineAddJoined { .. }
            | Self::MachineAddCredentialProvisioned { .. }
            | Self::MachineAddCompleted { .. }
            | Self::MachineAddFailed { .. }
            | Self::MachineUpdateSubmitted { .. }
            | Self::MachineUpdateRunning { .. }
            | Self::MachineUpdateCompleted { .. }
            | Self::MachineUpdateFailed { .. }
            | Self::MachineLifecycleSubmitted { .. }
            | Self::MachineLifecycleCompleted { .. }
            | Self::MachineLifecycleFailed { .. }
            | Self::CoreReplaceSubmitted { .. }
            | Self::CoreReplaceCompleted { .. }
            | Self::CoreReplaceFailed { .. }
            | Self::ServiceRestartSubmitted { .. }
            | Self::ServiceRestartRunning { .. }
            | Self::ServiceRestartContainerRestarted { .. }
            | Self::ServiceRestartCompleted { .. }
            | Self::ServiceRestartFailed { .. }
            | Self::NamespaceRemoveSubmitted { .. }
            | Self::NamespaceRemoveRunning { .. }
            | Self::NamespaceRemoveRouteBindingRemoved { .. }
            | Self::NamespaceRemoveContainerRemoved { .. }
            | Self::NamespaceRemoveCompleted { .. }
            | Self::NamespaceRemoveFailed { .. }
            | Self::Cancelled { .. } => None,
        }
    }

    /// The stable event token appended under an operation progress scope.
    #[must_use]
    pub fn subject_suffix(&self) -> String {
        match self {
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
            Self::CoreReplaceSubmitted { .. } => "core.replace.submitted".to_owned(),
            Self::CoreReplaceCompleted { .. } => "core.replace.completed".to_owned(),
            Self::CoreReplaceFailed { .. } => "core.replace.failed".to_owned(),
            Self::ServiceRestartSubmitted { .. } => "service.restart.submitted".to_owned(),
            Self::ServiceRestartRunning { stage, .. } => {
                format!("service.restart.running.{}", stage.as_subject())
            }
            Self::ServiceRestartContainerRestarted {
                machine_id,
                container_id,
                ..
            } => format!(
                "service.restart.container.restarted.{}.{}",
                machine_id.as_str(),
                container_id.as_str()
            ),
            Self::ServiceRestartCompleted { .. } => "service.restart.completed".to_owned(),
            Self::ServiceRestartFailed { .. } => "service.restart.failed".to_owned(),
            Self::NamespaceRemoveSubmitted { .. } => "namespace.remove.submitted".to_owned(),
            Self::NamespaceRemoveRunning { stage, .. } => {
                format!("namespace.remove.running.{}", stage.as_subject())
            }
            Self::NamespaceRemoveRouteBindingRemoved { target, .. } => format!(
                "namespace.remove.route_binding.removed.{}.{}",
                target.hostname.as_str(),
                target.port.get()
            ),
            Self::NamespaceRemoveContainerRemoved {
                machine_id,
                container_id,
                ..
            } => format!(
                "namespace.remove.container.removed.{}.{}",
                machine_id.as_str(),
                container_id.as_str()
            ),
            Self::NamespaceRemoveCompleted { .. } => "namespace.remove.completed".to_owned(),
            Self::NamespaceRemoveFailed { .. } => "namespace.remove.failed".to_owned(),
            Self::Cancelled { .. } => "cancelled".to_owned(),
        }
    }

    /// The durable stream subject this event publishes under. Subjects are a
    /// persisted contract: renderings must never change for an existing
    /// variant.
    #[must_use]
    pub fn subject(&self, scope: &crate::subjects::OperationProgressScope) -> String {
        crate::subjects::operation_progress_subject(
            scope,
            self.operation_id(),
            &self.subject_suffix(),
        )
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
    CoreReplace(MachineId),
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
    CoreReplace {
        operation_id: OperationId,
        event: CoreReplaceEvent,
    },
    ServiceRestart {
        operation_id: OperationId,
        event: ServiceRestartEvent,
    },
    NamespaceRemove {
        operation_id: OperationId,
        event: NamespaceRemoveEvent,
    },
}

impl ClassifiedOperationEvent {
    pub(super) fn operation_id(&self) -> &OperationId {
        match self {
            Self::Deploy { operation_id, .. }
            | Self::Cert { operation_id, .. }
            | Self::MachineAdd { operation_id, .. }
            | Self::MachineUpdate { operation_id, .. }
            | Self::MachineLifecycle { operation_id, .. }
            | Self::CoreReplace { operation_id, .. }
            | Self::ServiceRestart { operation_id, .. }
            | Self::NamespaceRemove { operation_id, .. } => operation_id,
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
            OperationEvent::DeployPlanningStarted { operation_id } => Self::Deploy {
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
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Running { stage }),
            },
            OperationEvent::DeployDataplanePrepared {
                operation_id,
                report,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::DataplanePrepared { report }),
            },
            OperationEvent::DeployContainerStarted {
                operation_id,
                machine_id,
                container_id,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::ContainerStarted {
                    machine_id,
                    container_id,
                }),
            },
            OperationEvent::DeployHealthCheckStarted { operation_id } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::HealthCheckStarted),
            },
            OperationEvent::DeployCleanupFinished {
                operation_id,
                removed,
                failed,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::CleanupFinished { removed, failed }),
            },
            OperationEvent::DeployCompleted {
                operation_id,
                outcome,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Completed { outcome }),
            },
            OperationEvent::DeployFailed {
                operation_id,
                failure,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Failed { failure }),
            },
            OperationEvent::CertRenewalSubmitted {
                operation_id,
                cert_id,
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
            OperationEvent::CoreReplaceSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::CoreReplace {
                operation_id,
                event: CoreReplaceEvent::Submitted { machine_id },
            },
            OperationEvent::CoreReplaceCompleted {
                operation_id,
                machine_id,
            } => Self::CoreReplace {
                operation_id,
                event: CoreReplaceEvent::Transition {
                    machine_id,
                    transition: CoreReplaceTransition::Completed,
                },
            },
            OperationEvent::CoreReplaceFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::CoreReplace {
                operation_id,
                event: CoreReplaceEvent::Transition {
                    machine_id,
                    transition: CoreReplaceTransition::Failed { failure },
                },
            },
            OperationEvent::ServiceRestartSubmitted {
                operation_id,
                namespace_id,
                service_id,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Submitted {
                    namespace_id,
                    service_id,
                },
            },
            OperationEvent::ServiceRestartRunning {
                operation_id,
                stage,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Transition(ServiceRestartTransition::Running { stage }),
            },
            OperationEvent::ServiceRestartContainerRestarted {
                operation_id,
                machine_id,
                container_id,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::ContainerRestarted {
                    machine_id,
                    container_id,
                },
            },
            OperationEvent::ServiceRestartCompleted { operation_id } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Transition(ServiceRestartTransition::Completed),
            },
            OperationEvent::ServiceRestartFailed {
                operation_id,
                failure,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Transition(ServiceRestartTransition::Failed {
                    failure,
                }),
            },
            OperationEvent::NamespaceRemoveSubmitted {
                operation_id,
                namespace_id,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Submitted { namespace_id },
            },
            OperationEvent::NamespaceRemoveRunning {
                operation_id,
                stage,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Running {
                    stage,
                }),
            },
            OperationEvent::NamespaceRemoveRouteBindingRemoved {
                operation_id,
                target,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::RouteBindingRemoved { target },
            },
            OperationEvent::NamespaceRemoveContainerRemoved {
                operation_id,
                machine_id,
                container_id,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::ContainerRemoved {
                    machine_id,
                    container_id,
                },
            },
            OperationEvent::NamespaceRemoveCompleted { operation_id } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Completed),
            },
            OperationEvent::NamespaceRemoveFailed {
                operation_id,
                failure,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Failed {
                    failure,
                }),
            },
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
                OperationKind::CoreReplace => Self::CoreReplace {
                    operation_id,
                    event: CoreReplaceEvent::Cancelled(reason),
                },
                OperationKind::ServiceRestart => Self::ServiceRestart {
                    operation_id,
                    event: ServiceRestartEvent::Transition(ServiceRestartTransition::Cancelled {
                        reason,
                    }),
                },
                OperationKind::NamespaceRemove => Self::NamespaceRemove {
                    operation_id,
                    event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Cancelled {
                        reason,
                    }),
                },
            },
        }
    }
}
