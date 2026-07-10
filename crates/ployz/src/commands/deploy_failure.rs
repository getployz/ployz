use ployz_core::dataplane::{DataplaneProviderFailure, PloyzNativeMeshComponent};
use ployz_core::ids::{ContainerId, MachineId, ServiceId};
use ployz_core::ops::{
    ArtifactUnavailableReason, ControlPlaneCommitScope, DeployOperationFailure, HealthCheckFailure,
    PreStartHookFailure, RetainedArtifact, RouteCutoverFailureReason, RouteTarget,
};
use ployz_core::state::MachineUsabilityReason;

use super::deploy_render::DeployTree;

pub(crate) struct DeployFailureContainerEvidence<'a> {
    pub machine_id: &'a MachineId,
    pub container_id: &'a ContainerId,
    pub log_command: String,
}

pub(crate) struct DeployFailureView<'a> {
    failure: &'a DeployOperationFailure,
    fallback_service: Option<&'a ServiceId>,
}

impl<'a> DeployFailureView<'a> {
    pub(crate) const fn new(
        failure: &'a DeployOperationFailure,
        fallback_service: Option<&'a ServiceId>,
    ) -> Self {
        Self {
            failure,
            fallback_service,
        }
    }

    pub(crate) fn service(&self) -> String {
        self.service_id()
            .or(self.fallback_service)
            .map(|service_id| service_id.as_str().to_owned())
            .unwrap_or_else(|| "unknown".to_owned())
    }

    pub(crate) fn machines(&self) -> Vec<&'a MachineId> {
        let mut machines = Vec::new();
        match self.failure {
            DeployOperationFailure::NoUsableMachines { reasons } => {
                for reason in reasons {
                    push_unique(&mut machines, &reason.machine_id);
                }
            }
            DeployOperationFailure::DataplaneUnavailable { machine_id, .. }
            | DeployOperationFailure::RuntimeUnavailable { machine_id, .. }
            | DeployOperationFailure::ContainerStartFailed { machine_id, .. }
            | DeployOperationFailure::ImageResolutionFailed { machine_id, .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { machine_id, .. }
            | DeployOperationFailure::PreStartHookFailed { machine_id, .. } => {
                push_unique(&mut machines, machine_id);
            }
            DeployOperationFailure::ImageMissingOnSeed { seed, .. }
            | DeployOperationFailure::ImageDigestMismatch { seed, .. }
            | DeployOperationFailure::SeedUnavailable { seed, .. } => {
                push_unique(&mut machines, seed);
            }
            DeployOperationFailure::ArtifactUnavailable {
                reason: ArtifactUnavailableReason::ImagePullFailed { machine_id, .. },
                ..
            } => push_unique(&mut machines, machine_id),
            DeployOperationFailure::DataplanePrepareTimedOut {
                machines: timed_out,
                ..
            } => {
                for machine_id in timed_out {
                    push_unique(&mut machines, machine_id);
                }
            }
            DeployOperationFailure::HealthCheckFailed { health_check, .. } => match health_check {
                HealthCheckFailure::ProbeFailed { machine_id, .. } => {
                    push_unique(&mut machines, machine_id);
                }
                HealthCheckFailure::TimedOut { .. } => {}
            },
            DeployOperationFailure::RouteCutoverFailed { reason, .. } => match reason {
                RouteCutoverFailureReason::GatewayUnavailable { machine_id } => {
                    push_unique(&mut machines, machine_id);
                }
                RouteCutoverFailureReason::RouteRejected { .. }
                | RouteCutoverFailureReason::StateStoreFailed { .. }
                | RouteCutoverFailureReason::TimedOut { .. } => {}
            },
            DeployOperationFailure::PlanningFailed { .. }
            | DeployOperationFailure::AutoDnsWithoutLease { .. }
            | DeployOperationFailure::ArtifactUnavailable {
                reason:
                    ArtifactUnavailableReason::BundleMissing
                    | ArtifactUnavailableReason::BundleUnreadable { .. },
                ..
            }
            | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
            | DeployOperationFailure::ControlPlaneCommitFailed { .. } => {}
        }

        for artifact in self.failure.retained_artifacts() {
            match artifact {
                RetainedArtifact::CreatedContainer { machine_id, .. }
                | RetainedArtifact::StartedContainer { machine_id, .. }
                | RetainedArtifact::ContainerStopFailed { machine_id, .. } => {
                    push_unique(&mut machines, machine_id);
                }
            }
        }
        machines
    }

    pub(crate) fn containers(&self) -> Vec<DeployFailureContainerEvidence<'a>> {
        let mut containers = Vec::new();
        if let DeployOperationFailure::ContainerStartFailed {
            machine_id,
            container_id,
            ..
        } = self.failure
        {
            containers.push(DeployFailureContainerEvidence {
                machine_id,
                container_id,
                log_command: format!("ployzctl logs {}", container_id.as_str()),
            });
        }

        for artifact in self.failure.retained_artifacts() {
            match artifact {
                RetainedArtifact::CreatedContainer {
                    machine_id,
                    container_id,
                    ..
                }
                | RetainedArtifact::ContainerStopFailed {
                    machine_id,
                    container_id,
                    ..
                } => containers.push(DeployFailureContainerEvidence {
                    machine_id,
                    container_id,
                    log_command: format!("ployzctl logs {}", container_id.as_str()),
                }),
                RetainedArtifact::StartedContainer {
                    machine_id,
                    container_id,
                    log_hint,
                } => containers.push(DeployFailureContainerEvidence {
                    machine_id,
                    container_id,
                    log_command: log_hint.as_str().to_owned(),
                }),
            }
        }
        containers
    }

    pub(crate) fn evidence(&self) -> String {
        let mut container_ids = Vec::new();
        let mut log_commands = Vec::new();
        for container in self.containers() {
            if !container_ids.contains(&container.container_id) {
                container_ids.push(container.container_id);
            }
            if !log_commands.contains(&container.log_command) {
                log_commands.push(container.log_command);
            }
        }

        match container_ids.as_slice() {
            [] => "evidence none".to_owned(),
            ids => format!(
                "evidence {} logs {}",
                ids.iter()
                    .map(|container_id| container_id.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                log_commands.join("; ")
            ),
        }
    }

    pub(crate) fn render_machines(&self) -> String {
        match self.machines().as_slice() {
            [] => "machine unknown".to_owned(),
            [machine_id] => format!("machine {}", machine_id.as_str()),
            many => format!(
                "machines {}",
                many.iter()
                    .map(|machine_id| machine_id.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }

    pub(crate) const fn safety(&self) -> FailureSafety {
        match self.failure {
            DeployOperationFailure::NoUsableMachines { .. }
            | DeployOperationFailure::PlanningFailed { .. }
            | DeployOperationFailure::AutoDnsWithoutLease { .. }
            | DeployOperationFailure::ImageResolutionFailed { .. }
            | DeployOperationFailure::ArtifactUnavailable { .. }
            | DeployOperationFailure::ImageMissingOnSeed { .. }
            | DeployOperationFailure::ImageDigestMismatch { .. }
            | DeployOperationFailure::SeedUnavailable { .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { .. } => {
                FailureSafety::NothingChanged
            }
            DeployOperationFailure::DataplaneUnavailable { .. }
            | DeployOperationFailure::DataplanePrepareTimedOut { .. }
            | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. } => FailureSafety::ServingUnchanged,
            DeployOperationFailure::ControlPlaneCommitFailed { .. }
            | DeployOperationFailure::RouteCutoverFailed { .. } => FailureSafety::NoClaim,
        }
    }

    pub(crate) const fn image_failure_service(&self) -> Option<&'a ServiceId> {
        match self.failure {
            DeployOperationFailure::ImageResolutionFailed { service_id, .. }
            | DeployOperationFailure::ArtifactUnavailable { service_id, .. }
            | DeployOperationFailure::ImageMissingOnSeed { service_id, .. }
            | DeployOperationFailure::ImageDigestMismatch { service_id, .. }
            | DeployOperationFailure::SeedUnavailable { service_id, .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { service_id, .. } => {
                Some(service_id)
            }
            DeployOperationFailure::NoUsableMachines { .. }
            | DeployOperationFailure::PlanningFailed { .. }
            | DeployOperationFailure::AutoDnsWithoutLease { .. }
            | DeployOperationFailure::DataplaneUnavailable { .. }
            | DeployOperationFailure::DataplanePrepareTimedOut { .. }
            | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::ControlPlaneCommitFailed { .. }
            | DeployOperationFailure::RouteCutoverFailed { .. } => None,
        }
    }

    pub(crate) const fn failed_route(&self) -> Option<&'a RouteTarget> {
        match self.failure {
            DeployOperationFailure::RouteCutoverFailed { route, .. } => Some(route),
            DeployOperationFailure::NoUsableMachines { .. }
            | DeployOperationFailure::PlanningFailed { .. }
            | DeployOperationFailure::AutoDnsWithoutLease { .. }
            | DeployOperationFailure::ImageResolutionFailed { .. }
            | DeployOperationFailure::ArtifactUnavailable { .. }
            | DeployOperationFailure::ImageMissingOnSeed { .. }
            | DeployOperationFailure::ImageDigestMismatch { .. }
            | DeployOperationFailure::SeedUnavailable { .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { .. }
            | DeployOperationFailure::DataplaneUnavailable { .. }
            | DeployOperationFailure::DataplanePrepareTimedOut { .. }
            | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::ControlPlaneCommitFailed { .. } => None,
        }
    }

    pub(crate) fn guidance(&self) -> Option<&'a str> {
        match self.failure {
            DeployOperationFailure::AutoDnsWithoutLease { message, .. } => Some(message.as_str()),
            DeployOperationFailure::NoUsableMachines { .. }
            | DeployOperationFailure::PlanningFailed { .. }
            | DeployOperationFailure::ImageResolutionFailed { .. }
            | DeployOperationFailure::ArtifactUnavailable { .. }
            | DeployOperationFailure::ImageMissingOnSeed { .. }
            | DeployOperationFailure::ImageDigestMismatch { .. }
            | DeployOperationFailure::SeedUnavailable { .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { .. }
            | DeployOperationFailure::DataplaneUnavailable { .. }
            | DeployOperationFailure::DataplanePrepareTimedOut { .. }
            | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::ControlPlaneCommitFailed { .. }
            | DeployOperationFailure::RouteCutoverFailed { .. } => None,
        }
    }

    fn service_id(&self) -> Option<&'a ServiceId> {
        match self.failure {
            DeployOperationFailure::PlanningFailed { service_id, .. }
            | DeployOperationFailure::AutoDnsWithoutLease { service_id, .. }
            | DeployOperationFailure::ImageResolutionFailed { service_id, .. }
            | DeployOperationFailure::ArtifactUnavailable { service_id, .. }
            | DeployOperationFailure::ImageMissingOnSeed { service_id, .. }
            | DeployOperationFailure::ImageDigestMismatch { service_id, .. }
            | DeployOperationFailure::SeedUnavailable { service_id, .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { service_id, .. } => {
                Some(service_id)
            }
            DeployOperationFailure::ControlPlaneCommitFailed { scope, .. } => match scope {
                ControlPlaneCommitScope::ServiceEntry { service_id, .. } => Some(service_id),
                ControlPlaneCommitScope::Namespace { .. }
                | ControlPlaneCommitScope::VolumePin { .. } => None,
            },
            DeployOperationFailure::NoUsableMachines { .. }
            | DeployOperationFailure::DataplaneUnavailable { .. }
            | DeployOperationFailure::DataplanePrepareTimedOut { .. }
            | DeployOperationFailure::DataplanePrepareInvalidReport { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::RouteCutoverFailed { .. } => None,
        }
    }
}

/// The strongest safety fact supported by the failure's typed stage.
/// Failures at or after route cutover make no claim about serving state.
pub(crate) enum FailureSafety {
    NothingChanged,
    ServingUnchanged,
    NoClaim,
}

fn push_unique<'a>(machines: &mut Vec<&'a MachineId>, machine_id: &'a MachineId) {
    if !machines.contains(&machine_id) {
        machines.push(machine_id);
    }
}

pub(super) fn failure_cause(tree: &DeployTree, failure: &DeployOperationFailure) -> String {
    match failure {
        DeployOperationFailure::NoUsableMachines { reasons } => {
            let details = reasons
                .iter()
                .map(|reason| match reason.reason {
                    MachineUsabilityReason::Draining => {
                        format!("{} is draining", reason.machine_id.as_str())
                    }
                    MachineUsabilityReason::FactsUnavailable => {
                        format!("{} did not answer with facts", reason.machine_id.as_str())
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            if details.is_empty() {
                "no usable machines were available".to_owned()
            } else {
                format!("no usable machines: {details}")
            }
        }
        DeployOperationFailure::PlanningFailed { message, .. } => {
            format!("deploy planning failed: {}", message.as_str())
        }
        DeployOperationFailure::AutoDnsWithoutLease { message, .. } => message.as_str().to_owned(),
        DeployOperationFailure::ImageResolutionFailed {
            image,
            machine_id,
            message,
            ..
        } => format!(
            "image {} could not be resolved by {}: {}",
            image.as_str(),
            machine_id.as_str(),
            message.as_str()
        ),
        DeployOperationFailure::ArtifactUnavailable {
            service_id, reason, ..
        } => {
            let image = tree
                .requested_image(service_id)
                .unwrap_or("requested image");
            match reason {
                ArtifactUnavailableReason::BundleMissing
                | ArtifactUnavailableReason::BundleUnreadable { .. }
                | ArtifactUnavailableReason::ImagePullFailed { .. } => format!(
                    "image {image} could not be resolved: {}",
                    artifact_unavailable_reason(reason)
                ),
            }
        }
        DeployOperationFailure::ImageMissingOnSeed {
            service_id,
            seed,
            manifest_digest,
        } => format!(
            "image {} manifest {} is missing from seed {}",
            tree.requested_image(service_id)
                .unwrap_or("requested image"),
            manifest_digest.as_str(),
            seed.as_str()
        ),
        DeployOperationFailure::ImageDigestMismatch {
            service_id,
            seed,
            expected,
            actual,
        } => format!(
            "image {} digest mismatch on seed {}: expected {}, got {}",
            tree.requested_image(service_id)
                .unwrap_or("requested image"),
            seed.as_str(),
            expected.as_str(),
            actual.as_str()
        ),
        DeployOperationFailure::SeedUnavailable {
            service_id,
            seed,
            message,
        } => format!(
            "image seed {} unavailable for {}: {}",
            seed.as_str(),
            tree.requested_image(service_id)
                .unwrap_or("requested image"),
            message.as_str()
        ),
        DeployOperationFailure::UnsupportedTargetPlatform {
            service_id,
            machine_id,
            image_platform,
            target_platform,
        } => format!(
            "image {} platform {}/{} is incompatible with {} platform {}/{}",
            tree.requested_image(service_id)
                .unwrap_or("requested image"),
            image_platform.os,
            image_platform.architecture,
            machine_id.as_str(),
            target_platform.os,
            target_platform.architecture
        ),
        DeployOperationFailure::DataplaneUnavailable {
            provider_failure,
            message,
            ..
        } => {
            let component = match provider_failure {
                DataplaneProviderFailure::PloyzNativeMesh { component } => match component {
                    PloyzNativeMeshComponent::WireGuard => "WireGuard",
                    PloyzNativeMeshComponent::EbpfForwarding => "eBPF forwarding",
                },
            };
            format!(
                "dataplane {component} preparation failed: {}",
                message.as_str()
            )
        }
        DeployOperationFailure::DataplanePrepareTimedOut {
            timeout_seconds, ..
        } => format!("dataplane preparation timed out after {timeout_seconds}s"),
        DeployOperationFailure::DataplanePrepareInvalidReport { message, .. } => {
            format!("dataplane returned an invalid report: {}", message.as_str())
        }
        DeployOperationFailure::RuntimeUnavailable { message, .. } => {
            format!("container runtime unavailable: {}", message.as_str())
        }
        DeployOperationFailure::ContainerStartFailed { message, .. } => {
            format!("container failed to start: {}", message.as_str())
        }
        DeployOperationFailure::PreStartHookFailed { failure, .. } => {
            pre_start_hook_failure_cause(failure)
        }
        DeployOperationFailure::HealthCheckFailed { health_check, .. } => match health_check {
            HealthCheckFailure::ProbeFailed { message, .. } => {
                format!("health check failed: {}", message.as_str())
            }
            HealthCheckFailure::TimedOut { timeout_seconds } => {
                format!("health check timed out after {timeout_seconds}s")
            }
        },
        DeployOperationFailure::ControlPlaneCommitFailed { scope, message, .. } => {
            let scope = match scope {
                ControlPlaneCommitScope::ServiceEntry { service_id, .. } => {
                    format!("service {}", service_id.as_str())
                }
                ControlPlaneCommitScope::Namespace {
                    namespace_revision_id,
                } => format!("namespace revision {}", namespace_revision_id.as_str()),
                ControlPlaneCommitScope::VolumePin {
                    namespace_id,
                    volume_name,
                } => format!(
                    "volume {} in namespace {}",
                    volume_name.as_str(),
                    namespace_id.as_str()
                ),
            };
            format!("could not commit {scope}: {}", message.as_str())
        }
        DeployOperationFailure::RouteCutoverFailed { route, reason, .. } => {
            let target = format!("{}:{}", route.hostname.as_str(), route.port.get());
            match reason {
                RouteCutoverFailureReason::GatewayUnavailable { machine_id } => format!(
                    "route {target} gateway unavailable on {}",
                    machine_id.as_str()
                ),
                RouteCutoverFailureReason::RouteRejected { message } => {
                    format!("route {target} rejected: {}", message.as_str())
                }
                RouteCutoverFailureReason::StateStoreFailed { message } => {
                    format!("route {target} state commit failed: {}", message.as_str())
                }
                RouteCutoverFailureReason::TimedOut { timeout_seconds } => {
                    format!("route {target} cutover timed out after {timeout_seconds}s")
                }
            }
        }
    }
}

fn pre_start_hook_failure_cause(failure: &PreStartHookFailure) -> String {
    match failure {
        PreStartHookFailure::RuntimeUnavailable { message } => {
            format!("pre-start hook runtime unavailable: {}", message.as_str())
        }
        PreStartHookFailure::OperationStepAmbiguous {
            operation_id,
            step_id,
            container_ids,
        } => format!(
            "pre-start hook step {} for operation {} matched containers {}",
            step_id.as_str(),
            operation_id.as_str(),
            container_ids
                .iter()
                .map(|container_id| container_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
        PreStartHookFailure::CreateFailed { message } => {
            format!(
                "pre-start hook container creation failed: {}",
                message.as_str()
            )
        }
        PreStartHookFailure::StartFailed { message, .. } => {
            format!("pre-start hook failed to start: {}", message.as_str())
        }
        PreStartHookFailure::WaitFailed { message, .. } => {
            format!("pre-start hook wait failed: {}", message.as_str())
        }
        PreStartHookFailure::TimedOut {
            timeout_millis,
            message,
            ..
        } => format!(
            "pre-start hook timed out after {timeout_millis}ms: {}",
            message.as_str()
        ),
        PreStartHookFailure::Exited {
            exit_code, message, ..
        } => format!(
            "pre-start hook exited with code {exit_code}: {}",
            message.as_str()
        ),
        PreStartHookFailure::CleanupFailed { message, .. } => {
            format!("pre-start hook cleanup failed: {}", message.as_str())
        }
    }
}

pub(super) fn artifact_unavailable_reason(reason: &ArtifactUnavailableReason) -> String {
    match reason {
        ArtifactUnavailableReason::BundleMissing => "deployment bundle is missing".to_owned(),
        ArtifactUnavailableReason::BundleUnreadable { message } => message.as_str().to_owned(),
        ArtifactUnavailableReason::ImagePullFailed {
            machine_id,
            message,
        } => format!(
            "image pull failed on {}: {}",
            machine_id.as_str(),
            message.as_str()
        ),
    }
}
