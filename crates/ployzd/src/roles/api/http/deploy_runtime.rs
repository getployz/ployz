//! The Docker-facing runtime seam the deploy driver and the placement
//! responder command.
//!
//! The trait keeps every external container effect behind one narrow surface;
//! the Docker impl adapts the machine runner and owns the health-gate
//! classification for containers this operation just started.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;
use ployz_core::DeployRequest;
use ployz_core::corrosion::{HostPortBindings, V2ManagedContainerIdentity};
use ployz_core::deploy::{ImageReference, RegistryCredential, VolumeName};
use ployz_core::ids::{ContainerId, NamespaceRowId};
use ployz_core::network::EndpointBridgeStatus;
use tokio::sync::watch;

use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, ExistingV2ManagedContainer,
    MachineContainerStopOutcome, V2MachineContainerRunner, V2MachineImageRunner,
};

use super::promotion_store::ResolvedNamespace;

const RUNNING_CONFIRMATION_WINDOW: Duration = Duration::from_secs(5);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_PERSISTED_DIAGNOSTIC_BYTES: usize = 4 * 1024;

#[async_trait]
pub(super) trait DeployRuntime: Send + Sync {
    async fn bridge_ready(&self) -> bool;
    async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String>;
    async fn pull_image(
        &self,
        image: &ImageReference,
        credential: Option<&RegistryCredential>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), String>;
    /// Creates one managed container from an already-assembled command.
    async fn create_container_command(
        &self,
        command: CreateV2ManagedContainer,
    ) -> Result<ContainerId, String>;
    /// Assembles the create command from the deploy request and resolved
    /// namespace, then creates the container.
    async fn create_container(
        &self,
        request: &DeployRequest,
        resolved_image: &ImageReference,
        namespace: &ResolvedNamespace,
        identity: V2ManagedContainerIdentity,
        host_ports: &HostPortBindings,
    ) -> Result<ContainerId, String> {
        let namespace_name = namespace.document.name.clone();
        let dns_search_domain =
            ployz_core::network::internal_dns::InternalDnsSearchDomain::try_from_namespace_label(
                namespace_name.as_str(),
            )
            .map_err(|error| bounded_diagnostic(error.to_string()))?;
        self.create_container_command(CreateV2ManagedContainer {
            image: resolved_image.clone(),
            runtime: request.runtime.clone(),
            dns_search_domain,
            identity,
            host_ports: host_ports.clone(),
        })
        .await
    }
    async fn start_container(&self, container_id: &ContainerId) -> Result<(), String>;
    async fn health_gate(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String>;
    /// The container's endpoint address without any health verdict, for the
    /// skip-gate escape hatch.
    async fn container_ip(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String>;
    /// Every managed container on this machine, across all services.
    async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, String>;
    /// The requested declared volumes this machine holds locally.
    async fn held_volumes(
        &self,
        namespace_id: &NamespaceRowId,
        volumes: &BTreeSet<VolumeName>,
    ) -> Result<BTreeSet<VolumeName>, String>;
    /// The requested namespace's containers; a namespace admits one service,
    /// so this scope also spans a failed first attempt's dead service ids.
    async fn namespace_docker_containers(
        &self,
        namespace_id: &NamespaceRowId,
    ) -> Result<Vec<ExistingV2ManagedContainer>, String> {
        Ok(self
            .managed_containers()
            .await?
            .into_iter()
            .filter(|container| &container.identity.namespace_id == namespace_id)
            .collect())
    }
    async fn stop_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<MachineContainerStopOutcome, String>;
    async fn remove_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String>;
}

#[async_trait]
impl DeployRuntime for DockerManagedContainerRunner {
    async fn bridge_ready(&self) -> bool {
        matches!(
            self.read_endpoint_network_status().await,
            EndpointBridgeStatus::Ready { .. }
        )
    }

    async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String> {
        self.resolve_registry_image(image, None)
            .await
            .and_then(|digest| {
                image.with_digest(&digest).map_err(|error| {
                    crate::roles::api::runner::MachineRegistryImageResolveError::ImagePull {
                        message: error.to_string(),
                    }
                })
            })
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))
    }

    async fn pull_image(
        &self,
        image: &ImageReference,
        credential: Option<&RegistryCredential>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), String> {
        self.pull_v2_registry_image(image, credential, shutdown)
            .await
            .map_err(|error| bounded_diagnostic(error.to_string()))
    }

    async fn create_container_command(
        &self,
        command: CreateV2ManagedContainer,
    ) -> Result<ContainerId, String> {
        self.create_v2_managed_container(command)
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))
    }

    async fn start_container(&self, container_id: &ContainerId) -> Result<(), String> {
        self.start_v2_managed_container(container_id)
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))
    }

    async fn health_gate(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        let confirmation_started = tokio::time::Instant::now();
        loop {
            let container = observe_running(self, container_id, identity).await?;
            let ExistingManagedContainerState::Running { ip: Some(ip), .. } = container.state
            else {
                return Err("started container stopped during its health gate".to_owned());
            };
            let ip = require_ipv4(ip)?;
            match classify_health_observation(
                container.health_status,
                confirmation_started.elapsed(),
            ) {
                HealthGateObservation::Ready => return Ok(ip),
                HealthGateObservation::Continue => {}
                HealthGateObservation::Failed => {
                    return Err("container healthcheck reported unhealthy".to_owned());
                }
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    async fn container_ip(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        loop {
            let container = observe_running(self, container_id, identity).await?;
            let ExistingManagedContainerState::Running { ip, .. } = container.state else {
                return Err("started container stopped before exposing an endpoint".to_owned());
            };
            if let Some(ip) = ip {
                return require_ipv4(ip);
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, String> {
        self.existing_v2_managed_containers()
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))
    }

    async fn held_volumes(
        &self,
        namespace_id: &NamespaceRowId,
        volumes: &BTreeSet<VolumeName>,
    ) -> Result<BTreeSet<VolumeName>, String> {
        self.held_v2_volumes(namespace_id, volumes)
            .await
            .map_err(|error| bounded_diagnostic(error.to_string()))
    }

    async fn stop_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<MachineContainerStopOutcome, String> {
        self.stop_v2_managed_container(container_id, expected_identity)
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))
    }

    async fn remove_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String> {
        self.remove_v2_managed_container(container_id, expected_identity)
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))
    }
}

/// One Docker listing round for a container this operation just started.
async fn observe_running<Runner>(
    runner: &Runner,
    container_id: &ContainerId,
    identity: &V2ManagedContainerIdentity,
) -> Result<ExistingV2ManagedContainer, String>
where
    Runner: V2MachineContainerRunner + Send + Sync,
{
    let containers = runner
        .existing_v2_managed_containers()
        .await
        .map_err(|error| bounded_diagnostic(format!("{error:?}")))?;
    let Some(container) = containers
        .into_iter()
        .find(|container| &container.container_id == container_id)
    else {
        return Err("started container was not visible in Docker".to_owned());
    };
    if &container.identity != identity {
        return Err("started container identity did not match its operation".to_owned());
    }
    Ok(container)
}

fn require_ipv4(ip: IpAddr) -> Result<Ipv4Addr, String> {
    match ip {
        IpAddr::V4(ip) => Ok(ip),
        IpAddr::V6(_) => Err("started container did not have an IPv4 endpoint".to_owned()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthGateObservation {
    Continue,
    Ready,
    Failed,
}

fn classify_health_observation(
    health: Option<ployz_core::machine::runtime::ManagedContainerHealthStatus>,
    continuously_running_for: Duration,
) -> HealthGateObservation {
    match health {
        Some(ployz_core::machine::runtime::ManagedContainerHealthStatus::Healthy) => {
            HealthGateObservation::Ready
        }
        Some(ployz_core::machine::runtime::ManagedContainerHealthStatus::Unhealthy) => {
            HealthGateObservation::Failed
        }
        Some(ployz_core::machine::runtime::ManagedContainerHealthStatus::Starting) => {
            HealthGateObservation::Continue
        }
        None if continuously_running_for >= RUNNING_CONFIRMATION_WINDOW => {
            HealthGateObservation::Ready
        }
        None => HealthGateObservation::Continue,
    }
}

/// Truncates a persisted diagnostic to its byte bound on a char boundary.
pub(super) fn bounded_diagnostic(mut message: String) -> String {
    if message.len() <= MAX_PERSISTED_DIAGNOSTIC_BYTES {
        return message;
    }
    let mut boundary = MAX_PERSISTED_DIAGNOSTIC_BYTES;
    while !message.is_char_boundary(boundary) {
        boundary -= 1;
    }
    message.truncate(boundary);
    message
}

#[cfg(test)]
mod tests {
    use super::{
        HEALTH_POLL_INTERVAL, HealthGateObservation, MAX_PERSISTED_DIAGNOSTIC_BYTES,
        RUNNING_CONFIRMATION_WINDOW, bounded_diagnostic, classify_health_observation,
    };

    #[test]
    fn inherited_healthcheck_waits_for_healthy_and_fails_unhealthy() {
        use ployz_core::machine::runtime::ManagedContainerHealthStatus;

        assert_eq!(
            classify_health_observation(
                Some(ManagedContainerHealthStatus::Starting),
                RUNNING_CONFIRMATION_WINDOW + HEALTH_POLL_INTERVAL,
            ),
            HealthGateObservation::Continue
        );
        assert_eq!(
            classify_health_observation(
                Some(ManagedContainerHealthStatus::Healthy),
                HEALTH_POLL_INTERVAL,
            ),
            HealthGateObservation::Ready
        );
        assert_eq!(
            classify_health_observation(
                Some(ManagedContainerHealthStatus::Unhealthy),
                HEALTH_POLL_INTERVAL,
            ),
            HealthGateObservation::Failed
        );
        assert_eq!(
            classify_health_observation(None, RUNNING_CONFIRMATION_WINDOW),
            HealthGateObservation::Ready
        );
    }

    #[test]
    fn persisted_diagnostics_are_utf8_safely_bounded() {
        let sentinel = "🔒".repeat(MAX_PERSISTED_DIAGNOSTIC_BYTES);
        let bounded = bounded_diagnostic(sentinel);
        assert!(bounded.len() <= MAX_PERSISTED_DIAGNOSTIC_BYTES);
        assert!(std::str::from_utf8(bounded.as_bytes()).is_ok());
    }
}
