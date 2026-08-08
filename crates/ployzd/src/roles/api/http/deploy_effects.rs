//! Coarse, idempotent deploy effects owned by one target host.

use std::collections::{BTreeSet, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::V2ManagedContainerIdentity;
use ployz_core::deploy::{ImageReference, RegistryCredential, VolumeName};
use ployz_core::ids::ContainerId;
use ployz_core::network::EndpointBridgeStatus;
use ployz_core::{
    DeployContainerObservation, DeployContainerState, DeployDesiredReplica, DeployInspectOutcome,
    DeployInspectRequest, DeployPrepareOutcome, DeployPrepareRequest, DeployPreparedReplica,
    DeployRetireOutcome, DeployRetireRequest, HealthGatePolicy,
};
use tokio::sync::watch;

use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, ExistingV2ManagedContainer,
    V2MachineContainerRunner, V2MachineImageRunner,
};

const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);
const RUNNING_CONFIRMATION_WINDOW: Duration = Duration::from_secs(5);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_PERSISTED_DIAGNOSTIC_BYTES: usize = 4 * 1024;

/// The local implementation behind the three machine-only deploy endpoints.
/// Mutations are serialized by the node workflow's single activity worker.
pub(super) struct DeployHostEffects {
    runtime: Arc<DockerManagedContainerRunner>,
}

impl DeployHostEffects {
    #[must_use]
    pub(super) fn new(runtime: Arc<DockerManagedContainerRunner>) -> Self {
        Self { runtime }
    }

    pub(super) async fn inspect(&self, request: DeployInspectRequest) -> DeployInspectOutcome {
        match tokio::time::timeout(INSPECT_TIMEOUT, self.inspect_inner(request)).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) | Err(_) => DeployInspectOutcome::Failed {},
        }
    }

    /// Converges one target-host prepare from current Docker reality.
    pub(super) async fn prepare(
        &self,
        mut request: DeployPrepareRequest,
        shutdown: watch::Receiver<bool>,
    ) -> Result<DeployPrepareOutcome, String> {
        let volumes = requested_volumes(&request);
        let target = validate_prepare_request(&request, &volumes).map_err(bounded_diagnostic)?;
        let image = self
            .resolve_image(&request.image, request.credential.as_ref())
            .await?;
        if image.pinned_digest().is_none() {
            return Err("target image resolution did not return a digest-pinned image".to_owned());
        }
        request.image = image;
        self.runtime
            .pull_v2_registry_image(&request.image, request.credential.as_ref(), shutdown)
            .await
            .map_err(|error| registry_diagnostic(error.to_string(), request.credential.as_ref()))?;
        request.credential = None;
        let image = request.image.clone();
        match self.prepare_inner(request, &target, &volumes).await {
            Ok(replicas) => Ok(DeployPrepareOutcome::Prepared { image, replicas }),
            Err(EffectError::Refused) => Ok(DeployPrepareOutcome::Refused {}),
            Err(EffectError::Failed(diagnostic)) => Err(diagnostic),
        }
    }

    pub(super) async fn retire(
        &self,
        request: DeployRetireRequest,
    ) -> Result<DeployRetireOutcome, String> {
        match self.retire_inner(request).await {
            Ok(()) => Ok(DeployRetireOutcome::Retired),
            Err(EffectError::Refused) => Ok(DeployRetireOutcome::Refused {}),
            Err(EffectError::Failed(diagnostic)) => Err(diagnostic),
        }
    }

    async fn resolve_image(
        &self,
        image: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> Result<ImageReference, String> {
        let digest = self
            .runtime
            .resolve_registry_image(image, credential)
            .await
            .map_err(|error| registry_diagnostic(format!("{error:?}"), credential))?;
        image
            .with_digest(&digest)
            .map_err(|error| bounded_diagnostic(error.to_string()))
    }

    async fn inspect_inner(
        &self,
        request: DeployInspectRequest,
    ) -> Result<DeployInspectOutcome, String> {
        let bridge_ready = matches!(
            self.runtime.read_endpoint_network_status().await,
            EndpointBridgeStatus::Ready { .. }
        );
        let containers = self
            .runtime
            .existing_v2_managed_containers()
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))?
            .into_iter()
            .map(normalize_container)
            .collect();
        let volumes_held = self
            .runtime
            .held_v2_volumes(&request.namespace_id, &request.volumes)
            .await
            .map_err(|error| bounded_diagnostic(error.to_string()))?;
        Ok(DeployInspectOutcome::Inspected {
            bridge_ready,
            containers,
            volumes_held,
        })
    }

    async fn prepare_inner(
        &self,
        request: DeployPrepareRequest,
        target: &V2ManagedContainerIdentity,
        volumes: &BTreeSet<VolumeName>,
    ) -> Result<Vec<DeployPreparedReplica>, EffectError> {
        let mut observed = self.managed_containers().await.map_err(failed)?;
        for replica in &request.replicas {
            if matching_container(&observed, replica)?.is_some() {
                continue;
            }
            let command = create_command(&request, replica)?;
            self.runtime
                .create_v2_managed_container(command)
                .await
                .map_err(|error| failed(format!("{error:?}")))?;
        }

        observed = self.managed_containers().await.map_err(failed)?;
        let candidates = candidate_containers(&observed, &request.replicas)?;
        if !volumes.is_empty() {
            if let Some(predecessor_deploy) = &request.predecessor_deploy {
                for predecessor in observed.iter().filter(|container| {
                    container.identity.namespace_id == target.namespace_id
                        && container.identity.service_id == target.service_id
                        && &container.identity.operation_id == predecessor_deploy
                        && !candidates.contains(&container.container_id)
                }) {
                    self.runtime
                        .stop_v2_managed_container(&predecessor.container_id, &predecessor.identity)
                        .await
                        .map_err(|error| failed(format!("{error:?}")))?;
                }
            }

            let active = self
                .runtime
                .active_v2_volume_users(&target.namespace_id, volumes)
                .await
                .map_err(bounded_diagnostic)
                .map_err(failed)?;
            reject_foreign_volume_user(&active, &candidates)?;
        }

        let mut prepared = Vec::with_capacity(request.replicas.len());
        for replica in &request.replicas {
            let observed = self.managed_containers().await.map_err(failed)?;
            let candidate = matching_container(&observed, replica)?.ok_or_else(|| {
                EffectError::Failed("prepared candidate disappeared before start".to_owned())
            })?;
            match candidate.state {
                ExistingManagedContainerState::Running { .. } => {}
                ExistingManagedContainerState::StartableStopped => self
                    .runtime
                    .start_v2_managed_container(&candidate.container_id)
                    .await
                    .map_err(|error| bounded_diagnostic(format!("{error:?}")))
                    .map_err(failed)?,
                ExistingManagedContainerState::NotStartable { .. } => {
                    return Err(EffectError::Failed(
                        "prepared candidate was not safely startable".to_owned(),
                    ));
                }
            }
            let gated = self
                .gate_container(
                    &candidate.container_id,
                    &candidate.identity,
                    request.health_gate,
                )
                .await;
            let ip = match gated {
                Ok(ip) => ip,
                Err(diagnostic) => {
                    let _ = self
                        .runtime
                        .stop_v2_managed_container(&candidate.container_id, &candidate.identity)
                        .await;
                    return Err(EffectError::Failed(diagnostic));
                }
            };
            prepared.push(DeployPreparedReplica {
                container_id: candidate.container_id.clone(),
                identity: candidate.identity.clone(),
                ip,
            });
        }

        Ok(prepared)
    }

    async fn retire_inner(&self, request: DeployRetireRequest) -> Result<(), EffectError> {
        let observed = self.managed_containers().await.map_err(failed)?;
        for target in &request.containers {
            let Some(actual) = observed
                .iter()
                .find(|container| container.container_id == target.container_id)
            else {
                continue;
            };
            if actual.identity != target.identity {
                return Err(EffectError::Refused);
            }
        }
        for target in request.containers {
            self.runtime
                .stop_v2_managed_container(&target.container_id, &target.identity)
                .await
                .map_err(|error| failed(format!("{error:?}")))?;
            self.runtime
                .remove_v2_managed_container(&target.container_id, &target.identity)
                .await
                .map_err(|error| failed(format!("{error:?}")))?;
        }
        Ok(())
    }

    async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, String> {
        self.runtime
            .existing_v2_managed_containers()
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))
    }

    async fn gate_container(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
        policy: HealthGatePolicy,
    ) -> Result<Ipv4Addr, String> {
        let confirmation_started = tokio::time::Instant::now();
        loop {
            let container = observe_running(&self.runtime, container_id, identity).await?;
            let ExistingManagedContainerState::Running { ip, .. } = container.state else {
                return Err("started container stopped before passing its creation gate".to_owned());
            };
            let Some(ip) = ip else {
                tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
                continue;
            };
            let ip = require_ipv4(ip)?;
            if matches!(policy, HealthGatePolicy::Skip) {
                return Ok(ip);
            }
            use ployz_core::machine::runtime::ManagedContainerHealthStatus;
            match container.health_status {
                Some(ManagedContainerHealthStatus::Healthy) => return Ok(ip),
                Some(ManagedContainerHealthStatus::Unhealthy) => {
                    return Err("container healthcheck reported unhealthy".to_owned());
                }
                None if confirmation_started.elapsed() >= RUNNING_CONFIRMATION_WINDOW => {
                    return Ok(ip);
                }
                Some(ManagedContainerHealthStatus::Starting) | None => {}
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }
}

fn requested_volumes(request: &DeployPrepareRequest) -> BTreeSet<VolumeName> {
    request
        .runtime
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect()
}

fn validate_prepare_request(
    request: &DeployPrepareRequest,
    volumes: &BTreeSet<VolumeName>,
) -> Result<V2ManagedContainerIdentity, String> {
    let Some(first) = request.replicas.first() else {
        return Err("target preparation requires at least one replica".to_owned());
    };
    let identity = &first.identity;
    if identity.operation_id != request.operation_id
        || request.replicas.iter().any(|replica| {
            replica.identity.namespace_id != identity.namespace_id
                || replica.identity.service_id != identity.service_id
                || replica.identity.operation_id != identity.operation_id
        })
    {
        return Err("target preparation mixed service identities".to_owned());
    }
    if !volumes.is_empty() && request.replicas.len() != 1 {
        return Err("named-volume preparation requires exactly one replica".to_owned());
    }
    let unique = request
        .replicas
        .iter()
        .map(|replica| replica.identity.clone())
        .collect::<HashSet<_>>();
    if unique.len() != request.replicas.len() {
        return Err("target preparation repeated a replica identity".to_owned());
    }
    Ok(identity.clone())
}

fn create_command(
    request: &DeployPrepareRequest,
    replica: &DeployDesiredReplica,
) -> Result<CreateV2ManagedContainer, EffectError> {
    let dns_search_domain =
        ployz_core::network::internal_dns::InternalDnsSearchDomain::try_from_namespace_label(
            request.namespace_name.as_str(),
        )
        .map_err(|error| failed(error.to_string()))?;
    Ok(CreateV2ManagedContainer {
        image: request.image.clone(),
        runtime: request.runtime.clone(),
        dns_search_domain,
        identity: replica.identity.clone(),
        host_ports: replica.host_ports.clone(),
    })
}

fn matching_container<'a>(
    observed: &'a [ExistingV2ManagedContainer],
    replica: &DeployDesiredReplica,
) -> Result<Option<&'a ExistingV2ManagedContainer>, EffectError> {
    let mut matching = observed
        .iter()
        .filter(|container| container.identity == replica.identity);
    let first = matching.next();
    if matching.next().is_some() {
        return Err(EffectError::Failed(
            "multiple containers carried one replica identity".to_owned(),
        ));
    }
    Ok(first)
}

fn candidate_containers(
    observed: &[ExistingV2ManagedContainer],
    replicas: &[DeployDesiredReplica],
) -> Result<BTreeSet<ContainerId>, EffectError> {
    let mut candidates = BTreeSet::new();
    for replica in replicas {
        let candidate = matching_container(observed, replica)?.ok_or_else(|| {
            EffectError::Failed("created candidate was not visible in Docker".to_owned())
        })?;
        candidates.insert(candidate.container_id.clone());
    }
    Ok(candidates)
}

fn reject_foreign_volume_user(
    active: &BTreeSet<ContainerId>,
    candidates: &BTreeSet<ContainerId>,
) -> Result<(), EffectError> {
    active
        .is_subset(candidates)
        .then_some(())
        .ok_or(EffectError::Refused)
}

fn normalize_container(container: ExistingV2ManagedContainer) -> DeployContainerObservation {
    let state = match container.state {
        ExistingManagedContainerState::Running { ip, .. } => DeployContainerState::Running {
            ip: ip.and_then(|ip| match ip {
                IpAddr::V4(ip) => Some(ip),
                IpAddr::V6(_) => None,
            }),
        },
        ExistingManagedContainerState::StartableStopped => DeployContainerState::Stopped,
        ExistingManagedContainerState::NotStartable { .. } => DeployContainerState::Indeterminate,
    };
    DeployContainerObservation {
        container_id: container.container_id,
        identity: container.identity,
        state,
        health: container.health_status,
        resolved_image_identity: container.resolved_image_identity,
        named_volumes: container.named_volume_names,
    }
}

fn failed(message: String) -> EffectError {
    EffectError::Failed(bounded_diagnostic(message))
}

enum EffectError {
    Refused,
    Failed(String),
}

async fn observe_running(
    runner: &DockerManagedContainerRunner,
    container_id: &ContainerId,
    identity: &V2ManagedContainerIdentity,
) -> Result<ExistingV2ManagedContainer, String> {
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

fn registry_diagnostic(message: String, credential: Option<&RegistryCredential>) -> String {
    bounded_diagnostic(match credential {
        Some(credential) => credential.redact_secret_in(message),
        None => message,
    })
}

fn bounded_diagnostic(mut message: String) -> String {
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
    use super::*;

    #[test]
    fn persisted_diagnostics_stay_bounded_utf8() {
        let bounded = bounded_diagnostic("🔒".repeat(MAX_PERSISTED_DIAGNOSTIC_BYTES));
        assert!(bounded.len() <= MAX_PERSISTED_DIAGNOSTIC_BYTES);
        assert!(std::str::from_utf8(bounded.as_bytes()).is_ok());
    }

    #[test]
    fn foreign_volume_users_are_still_refused() {
        let candidate = ContainerId::try_new("candidate").expect("container id");
        let foreign = ContainerId::try_new("foreign").expect("container id");

        assert!(matches!(
            reject_foreign_volume_user(&BTreeSet::from([foreign]), &BTreeSet::from([candidate]),),
            Err(EffectError::Refused)
        ));
    }
}
