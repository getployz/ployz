//! Coarse, idempotent deploy effects owned by one target host.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::V2ManagedContainerIdentity;
use ployz_core::deploy::{ImageReference, RegistryCredential};
use ployz_core::ids::ContainerId;
use ployz_core::network::EndpointBridgeStatus;
use ployz_core::{
    DeployDesiredReplica, DeployInspectOutcome, DeployInspectRequest, DeployObservedContainer,
    DeployPrepareOutcome, DeployPrepareRequest, DeployPreparedReplica, DeployRetireOutcome,
    DeployRetireRequest, HealthGatePolicy,
};
use tokio::sync::watch;

use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, ExistingV2ManagedContainer,
    MachineContainerStopOutcome, V2MachineContainerRunner, V2MachineImageRunner,
};

const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);

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
        let has_named_volumes = !request.runtime.volume_mounts.is_empty();
        let target = validate_prepare_request(&request, has_named_volumes)
            .map_err(|()| "prepare failed".to_owned())?;
        let image = self
            .resolve_image(&request.image, request.credential.as_ref())
            .await
            .map_err(|()| "prepare failed".to_owned())?;
        if image.pinned_digest().is_none() {
            return Err("prepare failed".to_owned());
        }
        request.image = image;
        self.runtime
            .pull_v2_registry_image(&request.image, request.credential.as_ref(), shutdown)
            .await
            .map_err(|_| "prepare failed".to_owned())?;
        request.credential = None;
        let image = request.image.clone();
        let controller_machine_id = request.controller_machine_id.clone();
        let appointment_id = request.appointment_id;
        match self
            .prepare_inner(request, &target, has_named_volumes)
            .await
        {
            Ok((replicas, displaced_incumbents)) => Ok(DeployPrepareOutcome::Prepared {
                controller_machine_id,
                appointment_id,
                image,
                replicas,
                displaced_incumbents,
            }),
            Err(EffectError::Refused) => Ok(DeployPrepareOutcome::Refused {}),
            Err(EffectError::Failed) => Err("prepare failed".to_owned()),
        }
    }

    pub(super) async fn retire(
        &self,
        request: DeployRetireRequest,
    ) -> Result<DeployRetireOutcome, String> {
        match self.retire_inner(request).await {
            Ok(()) => Ok(DeployRetireOutcome::Retired),
            Err(EffectError::Refused) => Ok(DeployRetireOutcome::Refused {}),
            Err(EffectError::Failed) => Err("retire failed".to_owned()),
        }
    }

    async fn resolve_image(
        &self,
        image: &ImageReference,
        credential: Option<&RegistryCredential>,
    ) -> Result<ImageReference, ()> {
        let digest = self
            .runtime
            .resolve_registry_image(image, credential)
            .await
            .map_err(|_| ())?;
        image.with_digest(&digest).map_err(|_| ())
    }

    async fn inspect_inner(
        &self,
        _request: DeployInspectRequest,
    ) -> Result<DeployInspectOutcome, ()> {
        let bridge_ready = matches!(
            self.runtime.read_endpoint_network_status().await,
            EndpointBridgeStatus::Ready { .. }
        );
        let containers = self
            .runtime
            .existing_v2_managed_containers()
            .await
            .map_err(|_| ())?
            .into_iter()
            .map(|container| DeployObservedContainer {
                container_id: container.container_id,
                identity: container.identity,
                running: matches!(
                    container.state,
                    ExistingManagedContainerState::Running { .. }
                ),
                host_ports: container.host_ports,
            })
            .collect();
        Ok(DeployInspectOutcome::Inspected {
            bridge_ready,
            containers,
        })
    }

    async fn prepare_inner(
        &self,
        request: DeployPrepareRequest,
        target: &V2ManagedContainerIdentity,
        has_named_volumes: bool,
    ) -> Result<(Vec<DeployPreparedReplica>, Vec<DeployObservedContainer>), EffectError> {
        let observed = self.managed_containers().await?;
        if has_named_volumes
            && observed.iter().any(|container| {
                container.identity.namespace_id == target.namespace_id
                    && container.identity.operation_id != request.operation_id
            })
        {
            return Err(EffectError::Refused);
        }
        for replica in &request.replicas {
            if matching_container(&observed, replica)?.is_some() {
                continue;
            }
            let command = create_command(&request, replica)?;
            self.runtime
                .create_v2_managed_container(command)
                .await
                .map_err(|_| EffectError::Failed)?;
        }

        let stopped_incumbents = self
            .stop_conflicting_incumbents(&request.stop_before_start)
            .await?;

        let result = self.start_and_gate_replicas(&request).await;
        if result.is_err() {
            self.restart_incumbents(&stopped_incumbents).await;
        }
        result.map(|replicas| (replicas, stopped_incumbents))
    }

    async fn start_and_gate_replicas(
        &self,
        request: &DeployPrepareRequest,
    ) -> Result<Vec<DeployPreparedReplica>, EffectError> {
        let mut prepared = Vec::with_capacity(request.replicas.len());
        for replica in &request.replicas {
            let observed = self.managed_containers().await?;
            let candidate = matching_container(&observed, replica)?.ok_or(EffectError::Failed)?;
            match candidate.state {
                ExistingManagedContainerState::Running { .. } => {}
                ExistingManagedContainerState::StartableStopped => self
                    .runtime
                    .start_v2_managed_container(&candidate.container_id)
                    .await
                    .map_err(|_| EffectError::Failed)?,
                ExistingManagedContainerState::NotStartable { .. } => {
                    return Err(EffectError::Failed);
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
                Err(error) => {
                    let _ = self
                        .runtime
                        .stop_v2_managed_container(&candidate.container_id, &candidate.identity)
                        .await;
                    return Err(error);
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

    async fn stop_conflicting_incumbents(
        &self,
        targets: &[DeployObservedContainer],
    ) -> Result<Vec<DeployObservedContainer>, EffectError> {
        let observed = self.managed_containers().await?;
        for target in targets {
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
        let mut stopped = Vec::new();
        for target in targets {
            let outcome = self
                .runtime
                .stop_v2_managed_container(&target.container_id, &target.identity)
                .await;
            match outcome {
                Ok(
                    outcome @ (MachineContainerStopOutcome::StoppedRunning
                    | MachineContainerStopOutcome::AlreadyStopped),
                ) => {
                    if restart_required(target.running, outcome) {
                        stopped.push(target.clone());
                    }
                }
                Ok(MachineContainerStopOutcome::Missing) => {
                    self.restart_incumbents(&stopped).await;
                    return Err(EffectError::Refused);
                }
                Err(_) => {
                    self.restart_incumbents(&stopped).await;
                    return Err(EffectError::Failed);
                }
            }
        }
        Ok(stopped)
    }

    async fn restart_incumbents(&self, stopped: &[DeployObservedContainer]) {
        for incumbent in stopped {
            let _ = self
                .runtime
                .start_v2_managed_container(&incumbent.container_id)
                .await;
        }
    }

    async fn retire_inner(&self, request: DeployRetireRequest) -> Result<(), EffectError> {
        if !request.rollback_services.is_empty() {
            return Err(EffectError::Refused);
        }
        let is_rollback = !request.restart_after_retire.is_empty();
        if request.containers.iter().any(|container| {
            container.identity.namespace_id != request.namespace_name
                || is_rollback && container.identity.operation_id != request.operation_id
        }) || request.restart_after_retire.iter().any(|incumbent| {
            incumbent.identity.namespace_id != request.namespace_name
                || incumbent.identity.operation_id == request.operation_id
                || !incumbent.running
        }) {
            return Err(EffectError::Refused);
        }
        let observed = self.managed_containers().await?;
        if request.containers.iter().any(|container| {
            request
                .restart_after_retire
                .iter()
                .any(|incumbent| incumbent.container_id == container.container_id)
        }) {
            return Err(EffectError::Refused);
        }
        for target in request
            .containers
            .iter()
            .chain(&request.restart_after_retire)
        {
            let Some(actual) = observed
                .iter()
                .find(|container| container.container_id == target.container_id)
            else {
                if request
                    .restart_after_retire
                    .iter()
                    .any(|restart| restart.container_id == target.container_id)
                {
                    return Err(EffectError::Refused);
                }
                continue;
            };
            if actual.identity != target.identity {
                return Err(EffectError::Refused);
            }
        }
        let mut failed = false;
        for target in request.containers {
            if self
                .runtime
                .stop_v2_managed_container(&target.container_id, &target.identity)
                .await
                .is_err()
            {
                failed = true;
                continue;
            }
            if self
                .runtime
                .remove_v2_managed_container(&target.container_id, &target.identity)
                .await
                .is_err()
            {
                failed = true;
            }
        }
        for incumbent in request.restart_after_retire {
            let state = observed
                .iter()
                .find(|container| container.container_id == incumbent.container_id)
                .map(|container| &container.state);
            match state {
                Some(ExistingManagedContainerState::Running { .. }) => {}
                Some(ExistingManagedContainerState::StartableStopped) => {
                    if self
                        .runtime
                        .start_v2_managed_container(&incumbent.container_id)
                        .await
                        .is_err()
                    {
                        failed = true;
                    }
                }
                Some(ExistingManagedContainerState::NotStartable { .. }) | None => failed = true,
            }
        }
        if failed {
            Err(EffectError::Failed)
        } else {
            Ok(())
        }
    }

    async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, EffectError> {
        self.runtime
            .existing_v2_managed_containers()
            .await
            .map_err(|_| EffectError::Failed)
    }

    async fn gate_container(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
        policy: HealthGatePolicy,
    ) -> Result<Ipv4Addr, EffectError> {
        loop {
            let container = observe_running(&self.runtime, container_id, identity).await?;
            let ExistingManagedContainerState::Running { ip, .. } = container.state else {
                return Err(EffectError::Failed);
            };
            let Some(ip) = ip else {
                tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
                continue;
            };
            let ip = require_ipv4(ip)?;
            if health_gate_ready(policy, container.health_status)? {
                return Ok(ip);
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }
}

fn restart_required(was_running_at_inspection: bool, outcome: MachineContainerStopOutcome) -> bool {
    matches!(outcome, MachineContainerStopOutcome::StoppedRunning)
        || was_running_at_inspection
            && matches!(outcome, MachineContainerStopOutcome::AlreadyStopped)
}

fn health_gate_ready(
    policy: HealthGatePolicy,
    status: Option<ployz_core::machine::runtime::ManagedContainerHealthStatus>,
) -> Result<bool, EffectError> {
    use ployz_core::machine::runtime::ManagedContainerHealthStatus;

    if matches!(policy, HealthGatePolicy::Skip) {
        return Ok(true);
    }
    match status {
        None | Some(ManagedContainerHealthStatus::Healthy) => Ok(true),
        Some(ManagedContainerHealthStatus::Starting) => Ok(false),
        Some(ManagedContainerHealthStatus::Unhealthy) => Err(EffectError::Failed),
    }
}

fn validate_prepare_request(
    request: &DeployPrepareRequest,
    has_named_volumes: bool,
) -> Result<V2ManagedContainerIdentity, ()> {
    let Some(first) = request.replicas.first() else {
        return Err(());
    };
    let identity = &first.identity;
    if identity.operation_id != request.operation_id
        || request.replicas.iter().any(|replica| {
            replica.identity.namespace_id != identity.namespace_id
                || replica.identity.operation_id != identity.operation_id
        })
    {
        return Err(());
    }
    if has_named_volumes && request.replicas.len() != 1 {
        return Err(());
    }
    let unique = request
        .replicas
        .iter()
        .map(|replica| replica.identity.clone())
        .collect::<HashSet<_>>();
    if unique.len() != request.replicas.len() {
        return Err(());
    }
    let stop_ids = request
        .stop_before_start
        .iter()
        .map(|container| container.container_id.clone())
        .collect::<HashSet<_>>();
    if stop_ids.len() != request.stop_before_start.len()
        || request.stop_before_start.iter().any(|container| {
            container.identity.namespace_id != identity.namespace_id
                || container.identity.operation_id == request.operation_id
                || !request.replicas.iter().any(|replica| {
                    replica.host_ports.iter().any(|desired| {
                        container.host_ports.iter().any(|incumbent| {
                            desired.protocol == incumbent.protocol
                                && desired.host_port == incumbent.host_port
                        })
                    })
                })
        })
    {
        return Err(());
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
        .map_err(|_| EffectError::Failed)?;
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
        return Err(EffectError::Failed);
    }
    Ok(first)
}

#[derive(Debug)]
enum EffectError {
    Refused,
    Failed,
}

async fn observe_running(
    runner: &DockerManagedContainerRunner,
    container_id: &ContainerId,
    identity: &V2ManagedContainerIdentity,
) -> Result<ExistingV2ManagedContainer, EffectError> {
    let containers = runner
        .existing_v2_managed_containers()
        .await
        .map_err(|_| EffectError::Failed)?;
    let Some(container) = containers
        .into_iter()
        .find(|container| &container.container_id == container_id)
    else {
        return Err(EffectError::Failed);
    };
    if &container.identity != identity {
        return Err(EffectError::Failed);
    }
    Ok(container)
}

fn require_ipv4(ip: IpAddr) -> Result<Ipv4Addr, EffectError> {
    match ip {
        IpAddr::V4(ip) => Ok(ip),
        IpAddr::V6(_) => Err(EffectError::Failed),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::machine::runtime::ManagedContainerHealthStatus;

    use super::*;

    #[test]
    fn enforced_gate_waits_only_when_docker_reports_a_healthcheck() {
        assert!(health_gate_ready(HealthGatePolicy::Enforce, None).expect("no healthcheck"));
        assert!(
            !health_gate_ready(
                HealthGatePolicy::Enforce,
                Some(ManagedContainerHealthStatus::Starting),
            )
            .expect("starting healthcheck")
        );
        assert!(
            health_gate_ready(
                HealthGatePolicy::Enforce,
                Some(ManagedContainerHealthStatus::Healthy),
            )
            .expect("healthy healthcheck")
        );
        assert!(
            health_gate_ready(
                HealthGatePolicy::Enforce,
                Some(ManagedContainerHealthStatus::Unhealthy),
            )
            .is_err()
        );
    }

    #[test]
    fn replay_restarts_an_incumbent_that_the_first_attempt_already_stopped() {
        assert!(restart_required(
            true,
            MachineContainerStopOutcome::AlreadyStopped
        ));
        assert!(!restart_required(
            false,
            MachineContainerStopOutcome::AlreadyStopped
        ));
    }
}
