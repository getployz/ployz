use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{ContainerId, MachineId, NamespaceRevisionId, ServiceId};
use ployz_core::ops::{
    ArtifactUnavailableReason, CertificateProvisionFailure, ControlPlaneCommitScope,
    DeployOperationFailure, HealthCheckFailure, PreStartHookFailure, RetainedArtifact,
    RouteCutoverFailureReason, RouteHostname, RouteTarget,
};
use ployz_core::state::MachineUsabilityReason;

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
            DeployOperationFailure::RuntimeUnavailable { machine_id, .. }
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
            DeployOperationFailure::HealthCheckFailed { health_check, .. } => match health_check {
                HealthCheckFailure::ProbeFailed { machine_id, .. } => {
                    push_unique(&mut machines, machine_id);
                }
                HealthCheckFailure::TimedOut { .. } => {}
            },
            DeployOperationFailure::CertificateProvisionFailed { failure, .. } => match failure {
                CertificateProvisionFailure::ChallengeReadiness {
                    missing_machine_ids,
                } => {
                    for machine_id in missing_machine_ids {
                        push_unique(&mut machines, machine_id);
                    }
                }
                CertificateProvisionFailure::GatewayArtifactPush { machine_id, .. } => {
                    push_unique(&mut machines, machine_id);
                }
                CertificateProvisionFailure::OperationEvidenceWrite { .. }
                | CertificateProvisionFailure::DnsPreflight { .. }
                | CertificateProvisionFailure::ChallengePublish { .. }
                | CertificateProvisionFailure::AcmeValidation { .. }
                | CertificateProvisionFailure::ActiveCertCommit { .. } => {}
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
            | DeployOperationFailure::CertificatePending { .. }
            | DeployOperationFailure::ArtifactUnavailable {
                reason:
                    ArtifactUnavailableReason::BundleMissing
                    | ArtifactUnavailableReason::BundleUnreadable { .. },
                ..
            }
            | DeployOperationFailure::CertificateProvisionTimedOut { .. }
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
            | DeployOperationFailure::CertificatePending { .. }
            | DeployOperationFailure::ImageResolutionFailed { .. }
            | DeployOperationFailure::ArtifactUnavailable { .. }
            | DeployOperationFailure::ImageMissingOnSeed { .. }
            | DeployOperationFailure::ImageDigestMismatch { .. }
            | DeployOperationFailure::SeedUnavailable { .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { .. } => {
                FailureSafety::NothingChanged
            }
            DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::CertificateProvisionFailed { .. }
            | DeployOperationFailure::CertificateProvisionTimedOut { .. } => {
                FailureSafety::ServingUnchanged
            }
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
            | DeployOperationFailure::CertificatePending { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::CertificateProvisionFailed { .. }
            | DeployOperationFailure::CertificateProvisionTimedOut { .. }
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
            | DeployOperationFailure::CertificatePending { .. }
            | DeployOperationFailure::ImageResolutionFailed { .. }
            | DeployOperationFailure::ArtifactUnavailable { .. }
            | DeployOperationFailure::ImageMissingOnSeed { .. }
            | DeployOperationFailure::ImageDigestMismatch { .. }
            | DeployOperationFailure::SeedUnavailable { .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::CertificateProvisionFailed { .. }
            | DeployOperationFailure::CertificateProvisionTimedOut { .. }
            | DeployOperationFailure::ControlPlaneCommitFailed { .. } => None,
        }
    }

    pub(crate) fn guidance(&self) -> Option<String> {
        match self.failure {
            DeployOperationFailure::AutoDnsWithoutLease { message, .. } => {
                Some(message.as_str().to_owned())
            }
            DeployOperationFailure::CertificateProvisionFailed {
                hostname,
                namespace_revision_id,
                failure,
                ..
            } => Some(certificate_provision_failure_cause(
                hostname,
                namespace_revision_id,
                failure,
            )),
            DeployOperationFailure::CertificateProvisionTimedOut {
                hostname,
                namespace_revision_id,
                timeout_seconds,
                ..
            } => Some(certificate_provision_timeout_cause(
                hostname,
                namespace_revision_id,
                *timeout_seconds,
            )),
            DeployOperationFailure::NoUsableMachines { .. }
            | DeployOperationFailure::PlanningFailed { .. }
            | DeployOperationFailure::CertificatePending { .. }
            | DeployOperationFailure::ImageResolutionFailed { .. }
            | DeployOperationFailure::ArtifactUnavailable { .. }
            | DeployOperationFailure::ImageMissingOnSeed { .. }
            | DeployOperationFailure::ImageDigestMismatch { .. }
            | DeployOperationFailure::SeedUnavailable { .. }
            | DeployOperationFailure::UnsupportedTargetPlatform { .. }
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
                ControlPlaneCommitScope::DeployPhase { .. }
                | ControlPlaneCommitScope::Namespace { .. }
                | ControlPlaneCommitScope::VolumePin { .. } => None,
            },
            DeployOperationFailure::NoUsableMachines { .. }
            | DeployOperationFailure::CertificatePending { .. }
            | DeployOperationFailure::RuntimeUnavailable { .. }
            | DeployOperationFailure::ContainerStartFailed { .. }
            | DeployOperationFailure::PreStartHookFailed { .. }
            | DeployOperationFailure::HealthCheckFailed { .. }
            | DeployOperationFailure::CertificateProvisionFailed { .. }
            | DeployOperationFailure::CertificateProvisionTimedOut { .. }
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

fn requested_image<'a>(target: &'a DeployRequest, service_id: &ServiceId) -> Option<&'a str> {
    target
        .services
        .iter()
        .find(|service| &service.service_id == service_id)
        .map(|service| service.image.as_str())
}

pub(super) fn failure_cause(target: &DeployRequest, failure: &DeployOperationFailure) -> String {
    match failure {
        DeployOperationFailure::NoUsableMachines { reasons } => {
            let details = reasons
                .iter()
                .map(|reason| match &reason.reason {
                    MachineUsabilityReason::Draining => {
                        format!("{} is draining", reason.machine_id.as_str())
                    }
                    MachineUsabilityReason::FactsUnavailable => {
                        format!("{} did not answer with facts", reason.machine_id.as_str())
                    }
                    MachineUsabilityReason::DataplaneUnavailable {
                        reason: unavailable,
                    } => {
                        format!(
                            "{} dataplane is unavailable: {unavailable:?}",
                            reason.machine_id.as_str()
                        )
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
        DeployOperationFailure::CertificatePending { last_error } => last_error.map_or_else(
            || "managed certificate is still pending".to_owned(),
            |last_error| format!("managed certificate is still pending: {last_error:?}"),
        ),
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
            let image = requested_image(target, service_id).unwrap_or("requested image");
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
            requested_image(target, service_id).unwrap_or("requested image"),
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
            requested_image(target, service_id).unwrap_or("requested image"),
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
            requested_image(target, service_id).unwrap_or("requested image"),
            message.as_str()
        ),
        DeployOperationFailure::UnsupportedTargetPlatform {
            service_id,
            machine_id,
            image_platform,
            target_platform,
        } => format!(
            "image {} platform {}/{} is incompatible with {} platform {}/{}",
            requested_image(target, service_id).unwrap_or("requested image"),
            image_platform.os,
            image_platform.architecture,
            machine_id.as_str(),
            target_platform.os,
            target_platform.architecture
        ),
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
        DeployOperationFailure::CertificateProvisionFailed {
            hostname,
            namespace_revision_id,
            failure,
            ..
        } => certificate_provision_failure_cause(hostname, namespace_revision_id, failure),
        DeployOperationFailure::CertificateProvisionTimedOut {
            hostname,
            namespace_revision_id,
            timeout_seconds,
            ..
        } => certificate_provision_timeout_cause(hostname, namespace_revision_id, *timeout_seconds),
        DeployOperationFailure::ControlPlaneCommitFailed { scope, message, .. } => {
            let scope = match scope {
                ControlPlaneCommitScope::DeployPhase {
                    namespace_revision_id,
                    phase,
                } => format!(
                    "phase {phase} of namespace revision {}",
                    namespace_revision_id.as_str()
                ),
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

fn certificate_provision_failure_cause(
    hostname: &RouteHostname,
    namespace_revision_id: &NamespaceRevisionId,
    failure: &CertificateProvisionFailure,
) -> String {
    certificate_provision_failure_detail(
        failure,
        Some(&format!(
            "for {} (namespace revision {})",
            hostname.as_str(),
            namespace_revision_id.as_str(),
        )),
    )
}

pub(crate) fn certificate_provision_failure_detail(
    failure: &CertificateProvisionFailure,
    scope: Option<&str>,
) -> String {
    let scope = scope.map_or_else(String::new, |scope| format!(" {scope}"));
    match failure {
        CertificateProvisionFailure::OperationEvidenceWrite { message } => format!(
            "certificate operation evidence write failed{scope}: {}",
            message.as_str()
        ),
        CertificateProvisionFailure::DnsPreflight { message } => {
            format!(
                "certificate DNS preflight failed{scope}: {}",
                message.as_str()
            )
        }
        CertificateProvisionFailure::ChallengePublish { message } => {
            format!(
                "certificate HTTP-01 challenge publish failed{scope}: {}",
                message.as_str()
            )
        }
        CertificateProvisionFailure::ChallengeReadiness {
            missing_machine_ids,
        } => {
            let machines = missing_machine_ids
                .iter()
                .map(|machine_id| machine_id.as_str())
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "certificate HTTP-01 challenge readiness failed{scope}: missing gateway acknowledgements from {machines}"
            )
        }
        CertificateProvisionFailure::AcmeValidation { message } => {
            format!(
                "certificate ACME validation failed{scope}: {}",
                message.as_str()
            )
        }
        CertificateProvisionFailure::GatewayArtifactPush {
            machine_id,
            message,
        } => format!(
            "certificate gateway artifact push failed on {}{scope}: {}",
            machine_id.as_str(),
            message.as_str()
        ),
        CertificateProvisionFailure::ActiveCertCommit {
            attempted_active_cert,
            message,
        } => format!(
            "active certificate commit failed for {}{scope}: {}",
            attempted_active_cert.cert_id.as_str(),
            message.as_str()
        ),
    }
}

fn certificate_provision_timeout_cause(
    hostname: &RouteHostname,
    namespace_revision_id: &NamespaceRevisionId,
    timeout_seconds: u32,
) -> String {
    format!(
        "certificate provisioning timed out after {timeout_seconds}s for {} (namespace revision {})",
        hostname.as_str(),
        namespace_revision_id.as_str()
    )
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
