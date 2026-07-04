//! Durable operation events and their subjects.

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

use super::text::CancellationReason;
use super::{
    CertOperationFailure, DeployCleanupFailure, DeployCompletionOutcome, DeployOperationFailure,
    DeployRunningStage, MachineSubstrateVersions, MachineUpdateFailure,
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

/// Persisted `PLZ_OPS` stream payload.
///
/// Changing this shape intentionally breaks operation replay/history unless
/// paired with stream cleanup or migration.
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
    Cancelled {
        operation_id: OperationId,
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
            | Self::MachineUpdateSubmitted { .. } => format!("operation.submit.{operation_id}"),
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
            // Cancellation is recorded only by the deploy path; when another
            // operation kind gains cancellation this event must learn its
            // kind so terminal dedup stays per-kind-accurate.
            Self::DeployCompleted { .. } | Self::DeployFailed { .. } | Self::Cancelled { .. } => {
                format!("deploy.terminal.{operation_id}")
            }
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
        }
    }
}
