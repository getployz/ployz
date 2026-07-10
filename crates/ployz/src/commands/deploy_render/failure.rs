use super::*;

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
        DeployOperationFailure::ArtifactUnavailable {
            service_id, reason, ..
        } => {
            let image = tree
                .deploy
                .as_ref()
                .and_then(|deploy| {
                    deploy
                        .target
                        .services
                        .iter()
                        .find(|service| &service.service_id == service_id)
                })
                .map_or("requested image", |service| service.image.as_str());
            match reason {
                ArtifactUnavailableReason::BundleMissing
                | ArtifactUnavailableReason::BundleUnreadable { .. } => format!(
                    "image {image} could not be resolved: {}",
                    artifact_unavailable_reason(reason)
                ),
            }
        }
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

pub(super) fn artifact_unavailable_reason(reason: &ArtifactUnavailableReason) -> String {
    match reason {
        ArtifactUnavailableReason::BundleMissing => "deployment bundle is missing".to_owned(),
        ArtifactUnavailableReason::BundleUnreadable { message } => message.as_str().to_owned(),
    }
}
