//! Stable NATS subject rendering for durable operation events.

use crate::state::MachineLifecycle;

use super::OperationEvent;

impl OperationEvent {
    /// The stable event token appended under an operation progress scope.
    #[must_use]
    pub fn subject_suffix(&self) -> String {
        match self {
            Self::DeploySubmitted { .. } => "deploy.submitted".to_owned(),
            Self::DeployPlanningStarted { .. } => "deploy.planning.started".to_owned(),
            Self::DeployWaitingForManagedCertificate { .. } => {
                "deploy.managed_certificate.waiting".to_owned()
            }
            Self::DeployImageResolved { service_id, .. } => {
                format!("deploy.image.resolved.{}", service_id.as_str())
            }
            Self::DeployPlanCreated { .. } => "deploy.plan.created".to_owned(),
            Self::DeployRunning { stage, .. } => format!("deploy.running.{}", stage.as_subject()),
            Self::DeployDataplanePrepared { .. } => "deploy.dataplane.prepared".to_owned(),
            Self::DeployImageAvailabilityVerified {
                service_id, seed, ..
            } => format!(
                "deploy.image.availability_verified.{}.{}",
                service_id.as_str(),
                seed.as_str()
            ),
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
            Self::NetworkRepairSubmitted { .. } => "network.repair.submitted".to_owned(),
            Self::NetworkRepairRunning { stage, .. } => {
                format!("network.repair.running.{}", stage.as_subject())
            }
            Self::NetworkRepairDataplanePrepared { .. } => {
                "network.repair.dataplane.prepared".to_owned()
            }
            Self::NetworkRepairMachineFactsRefreshed { .. } => {
                "network.repair.machine_facts.refreshed".to_owned()
            }
            Self::NetworkRepairDnsRefreshConfirmed { .. } => {
                "network.repair.dns_refresh.confirmed".to_owned()
            }
            Self::NetworkRepairCompleted { .. } => "network.repair.completed".to_owned(),
            Self::NetworkRepairFailed { .. } => "network.repair.failed".to_owned(),
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
            Self::ManagedLeaseSubmitted { .. } => "managed.lease.submitted".to_owned(),
            Self::ManagedLeaseCompleted { .. } => "managed.lease.completed".to_owned(),
            Self::ManagedLeaseFailed { .. } => "managed.lease.failed".to_owned(),
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
            Self::VolumeRemoveSubmitted { .. } => "volume.remove.submitted".to_owned(),
            Self::VolumeRemoveRunning { stage, .. } => {
                format!("volume.remove.running.{}", stage.as_subject())
            }
            Self::VolumeRemoveCompleted { .. } => "volume.remove.completed".to_owned(),
            Self::VolumeRemoveFailed { .. } => "volume.remove.failed".to_owned(),
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
