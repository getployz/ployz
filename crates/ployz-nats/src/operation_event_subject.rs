//! Stable NATS subject suffix rendering for durable operation events.

use ployz_core::machine::{MachineCredentialProvisioningStep, MachineLifecycle};
use ployz_core::operation::OperationEvent;

use crate::subjects::{
    deploy_running_stage, namespace_remove_running_stage, network_repair_running_stage,
    service_restart_running_stage, volume_remove_running_stage,
};

/// The stable event token appended under an operation progress scope.
#[must_use]
pub fn operation_event_subject_suffix(event: &OperationEvent) -> String {
    match event {
        OperationEvent::BuildSubmitted { .. } => "build.submitted".to_owned(),
        OperationEvent::BuildPlacementStarted { .. } => "build.placement.started".to_owned(),
        OperationEvent::BuildPlatformPlaced { .. } => "build.platform.placed".to_owned(),
        OperationEvent::BuildCommitVerified { .. } => "build.platform.commit_verified".to_owned(),
        OperationEvent::BuildPlatformToolchainVerified { .. } => {
            "build.platform.toolchain_verified".to_owned()
        }
        OperationEvent::BuildRunning { .. } => "build.running".to_owned(),
        OperationEvent::BuildPlatformLog { .. } => "build.platform.log".to_owned(),
        OperationEvent::BuildPlatformLogTruncated { .. } => {
            "build.platform.log_truncated".to_owned()
        }
        OperationEvent::BuildPlatformLogGap { .. } => "build.platform.log_gap".to_owned(),
        OperationEvent::BuildPlatformCompleted { .. } => "build.platform.completed".to_owned(),
        OperationEvent::BuildPlatformFailed { .. } => "build.platform.failed".to_owned(),
        OperationEvent::BuildCompleted { .. } => "build.completed".to_owned(),
        OperationEvent::BuildFailed { .. } => "build.failed".to_owned(),
        OperationEvent::BuildCancelled { .. } => "build.cancelled".to_owned(),
        OperationEvent::BuildTimedOut { .. } => "build.timed_out".to_owned(),
        OperationEvent::DeploySubmitted { .. } => "deploy.submitted".to_owned(),
        OperationEvent::DeployPlanningStarted { .. } => "deploy.planning.started".to_owned(),
        OperationEvent::DeployImageResolved { service_id, .. } => {
            format!("deploy.image.resolved.{}", service_id.as_str())
        }
        OperationEvent::DeployPlanCreated { .. } => "deploy.plan.created".to_owned(),
        OperationEvent::DeployRunning { stage, .. } => {
            format!("deploy.running.{}", deploy_running_stage(stage))
        }
        OperationEvent::DeployImageAvailabilityVerified {
            service_id, seed, ..
        } => format!(
            "deploy.image.availability_verified.{}.{}",
            service_id.as_str(),
            seed.as_str()
        ),
        OperationEvent::DeployContainerStarted {
            machine_id,
            container_id,
            ..
        } => format!(
            "deploy.container.started.{}.{}",
            machine_id.as_str(),
            container_id.as_str()
        ),
        OperationEvent::DeployHealthCheckStarted { .. } => "deploy.health_check.started".to_owned(),
        OperationEvent::DeployPhaseStarted { phase, .. } => format!("deploy.phase.{phase}.started"),
        OperationEvent::DeployPhaseFinished { phase, .. } => {
            format!("deploy.phase.{phase}.finished")
        }
        OperationEvent::DeployCleanupFinished { .. } => "deploy.cleanup.finished".to_owned(),
        OperationEvent::DeployCompleted { .. } => "deploy.completed".to_owned(),
        OperationEvent::DeployFailed { .. } => "deploy.failed".to_owned(),
        OperationEvent::CertProvisionSubmitted { .. } => "cert.submitted".to_owned(),
        OperationEvent::CertChallengePublished { .. } => "cert.challenge.published".to_owned(),
        OperationEvent::CertValidationStarted { .. } => "cert.validation.started".to_owned(),
        OperationEvent::CertWarning { .. } => "cert.warning".to_owned(),
        OperationEvent::CertCompleted { .. } => "cert.completed".to_owned(),
        OperationEvent::CertFailed { .. } => "cert.failed".to_owned(),
        OperationEvent::MachineAddSubmitted { .. } => "machine.add.submitted".to_owned(),
        OperationEvent::MachineAddJoined { .. } => "machine.add.joined".to_owned(),
        OperationEvent::MachineAddCredentialProvisioned { step, .. } => {
            format!(
                "machine.add.credential.{}",
                machine_credential_provisioning_step_token(*step)
            )
        }
        OperationEvent::MachineAddCompleted { .. } => "machine.add.completed".to_owned(),
        OperationEvent::MachineAddFailed { .. } => "machine.add.failed".to_owned(),
        OperationEvent::MachineUpdateSubmitted { .. } => "machine.update.submitted".to_owned(),
        OperationEvent::MachineUpdateRunning { .. } => "machine.update.running".to_owned(),
        OperationEvent::MachineUpdateCompleted { .. } => "machine.update.completed".to_owned(),
        OperationEvent::MachineUpdateFailed { .. } => "machine.update.failed".to_owned(),
        OperationEvent::MachineStoragePrepareSubmitted { .. } => {
            "machine.storage_prepare.submitted".to_owned()
        }
        OperationEvent::MachineStoragePreparePreparing { .. } => {
            "machine.storage_prepare.preparing".to_owned()
        }
        OperationEvent::MachineStoragePrepareCompleted { .. } => {
            "machine.storage_prepare.completed".to_owned()
        }
        OperationEvent::MachineStoragePrepareFailed { .. } => {
            "machine.storage_prepare.failed".to_owned()
        }
        OperationEvent::MachineLifecycleSubmitted { target, .. } => match target {
            MachineLifecycle::Active => "machine.lifecycle.resume.submitted".to_owned(),
            MachineLifecycle::Draining => "machine.lifecycle.drain.submitted".to_owned(),
        },
        OperationEvent::MachineLifecycleCompleted { .. } => {
            "machine.lifecycle.completed".to_owned()
        }
        OperationEvent::MachineLifecycleFailed { .. } => "machine.lifecycle.failed".to_owned(),
        OperationEvent::CoreReplaceSubmitted { .. } => "core.replace.submitted".to_owned(),
        OperationEvent::CoreReplaceCompleted { .. } => "core.replace.completed".to_owned(),
        OperationEvent::CoreReplaceFailed { .. } => "core.replace.failed".to_owned(),
        OperationEvent::CredentialGrantSubmitted { action, .. } => match action {
            ployz_core::operation::CredentialGrantAction::Add { .. } => {
                "credential.add.submitted".to_owned()
            }
            ployz_core::operation::CredentialGrantAction::Remove { .. } => {
                "credential.remove.submitted".to_owned()
            }
        },
        OperationEvent::CredentialGrantCompleted { .. } => "credential.grant.completed".to_owned(),
        OperationEvent::CredentialGrantFailed { .. } => "credential.grant.failed".to_owned(),
        OperationEvent::NetworkRepairSubmitted { .. } => "network.repair.submitted".to_owned(),
        OperationEvent::NetworkRepairRunning { stage, .. } => {
            format!(
                "network.repair.running.{}",
                network_repair_running_stage(stage)
            )
        }
        OperationEvent::NetworkRepairDataplaneConverged { .. } => {
            "network.repair.dataplane.converged".to_owned()
        }
        OperationEvent::NetworkRepairMachineFactsRefreshed { .. } => {
            "network.repair.machine_facts.refreshed".to_owned()
        }
        OperationEvent::NetworkRepairDnsRefreshConfirmed { .. } => {
            "network.repair.dns_refresh.confirmed".to_owned()
        }
        OperationEvent::NetworkRepairCompleted { .. } => "network.repair.completed".to_owned(),
        OperationEvent::NetworkRepairFailed { .. } => "network.repair.failed".to_owned(),
        OperationEvent::ServiceRestartSubmitted { .. } => "service.restart.submitted".to_owned(),
        OperationEvent::ServiceRestartRunning { stage, .. } => {
            format!(
                "service.restart.running.{}",
                service_restart_running_stage(stage)
            )
        }
        OperationEvent::ServiceRestartContainerRestarted {
            machine_id,
            container_id,
            ..
        } => format!(
            "service.restart.container.restarted.{}.{}",
            machine_id.as_str(),
            container_id.as_str()
        ),
        OperationEvent::ServiceRestartCompleted { .. } => "service.restart.completed".to_owned(),
        OperationEvent::ServiceRestartFailed { .. } => "service.restart.failed".to_owned(),
        OperationEvent::ManagedDnsReconcileSubmitted { .. } => {
            "managed.dns.reconcile.submitted".to_owned()
        }
        OperationEvent::ManagedDnsReconcileCompleted { .. } => {
            "managed.dns.reconcile.completed".to_owned()
        }
        OperationEvent::ManagedDnsReconcileFailed { .. } => {
            "managed.dns.reconcile.failed".to_owned()
        }
        OperationEvent::IngressConfigureSubmitted { .. } => {
            "ingress.configure.submitted".to_owned()
        }
        OperationEvent::IngressConfigureCompleted { .. } => {
            "ingress.configure.completed".to_owned()
        }
        OperationEvent::IngressConfigureFailed { .. } => "ingress.configure.failed".to_owned(),
        OperationEvent::NamespaceRemoveSubmitted { .. } => "namespace.remove.submitted".to_owned(),
        OperationEvent::NamespaceRemoveRunning { stage, .. } => {
            format!(
                "namespace.remove.running.{}",
                namespace_remove_running_stage(stage)
            )
        }
        OperationEvent::NamespaceRemoveRouteBindingRemoved { target, .. } => format!(
            "namespace.remove.route_binding.removed.{}",
            target.hostname.as_str()
        ),
        OperationEvent::NamespaceRemoveContainerRemoved {
            machine_id,
            container_id,
            ..
        } => format!(
            "namespace.remove.container.removed.{}.{}",
            machine_id.as_str(),
            container_id.as_str()
        ),
        OperationEvent::NamespaceRemoveCompleted { .. } => "namespace.remove.completed".to_owned(),
        OperationEvent::NamespaceRemoveFailed { .. } => "namespace.remove.failed".to_owned(),
        OperationEvent::VolumeRemoveSubmitted { .. } => "volume.remove.submitted".to_owned(),
        OperationEvent::VolumeRemoveRunning { stage, .. } => {
            format!(
                "volume.remove.running.{}",
                volume_remove_running_stage(stage)
            )
        }
        OperationEvent::VolumeRemoveCompleted { .. } => "volume.remove.completed".to_owned(),
        OperationEvent::VolumeRemoveFailed { .. } => "volume.remove.failed".to_owned(),
        OperationEvent::VolumeCreateSubmitted { .. } => "volume.create.submitted".to_owned(),
        OperationEvent::VolumeCreatePlanningStarted { .. } => {
            "volume.create.planning.started".to_owned()
        }
        OperationEvent::VolumeCreateRunning { stage, .. } => {
            let stage = match stage {
                ployz_core::operation::VolumeCreateRunningStage::CommittingPin => "committing_pin",
                ployz_core::operation::VolumeCreateRunningStage::EnsuringVolume => {
                    "ensuring_volume"
                }
            };
            format!("volume.create.running.{stage}")
        }
        OperationEvent::VolumeCreateCompleted { .. } => "volume.create.completed".to_owned(),
        OperationEvent::VolumeCreateFailed { .. } => "volume.create.failed".to_owned(),
        OperationEvent::OperationInterrupted { .. } => "operation.interrupted".to_owned(),
        OperationEvent::Cancelled { .. } => "cancelled".to_owned(),
    }
}

#[must_use]
pub const fn machine_credential_provisioning_step_token(
    step: MachineCredentialProvisioningStep,
) -> &'static str {
    match step {
        MachineCredentialProvisioningStep::Minted => "minted",
        MachineCredentialProvisioningStep::Rendered => "rendered",
        MachineCredentialProvisioningStep::Reloaded => "reloaded",
        MachineCredentialProvisioningStep::Verified => "verified",
        MachineCredentialProvisioningStep::MaterialReady => "material_ready",
    }
}

#[cfg(test)]
mod build_subject_tests {
    use ployz_core::ids::{MachineId, OperationId};
    use ployz_core::image::OciPlatform;
    use ployz_core::operation::{BuildLogChunk, OperationEvent};

    use super::operation_event_subject_suffix;

    #[test]
    fn build_event_subjects_are_stable() {
        let operation_id = OperationId::try_new("build-subject-test").expect("operation id");
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let platform = OciPlatform::try_new("linux", "arm64").expect("platform");

        assert_eq!(
            operation_event_subject_suffix(&OperationEvent::BuildPlacementStarted {
                operation_id: operation_id.clone(),
            }),
            "build.placement.started"
        );
        assert_eq!(
            operation_event_subject_suffix(&OperationEvent::BuildPlatformLog {
                operation_id,
                platform,
                machine_id,
                chunk: BuildLogChunk::try_new("step 1").expect("chunk"),
            }),
            "build.platform.log"
        );
    }
}
