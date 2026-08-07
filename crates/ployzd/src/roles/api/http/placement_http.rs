//! Machine-authority responder seams for placement: the live `/deploy/bid`
//! testimony and the claim-scoped `/deploy/execute` verb handler.
//!
//! Both handlers answer from live local truth — Docker, the local Corrosion
//! replica, and the machine's own capacity — and store nothing. Forwarded
//! registry credentials and environment values pass through to Docker only;
//! they never reach logs or evidence on this machine.

use std::collections::BTreeSet;
use std::future::Future;
use std::net::Ipv4Addr;
use std::time::Duration;

use async_trait::async_trait;
use hyper::{Response, StatusCode};
use ployz_core::corrosion::{
    CorrosionOperation, DeployLiveness, MachineDocument, Principal, V2ManagedContainerIdentity,
};
use ployz_core::deploy::{ImageReference, RegistryCredential, VolumeName};
use ployz_core::ids::{ContainerId, MachineRowId, NamespaceRowId, ServiceRowId};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::internal_dns::InternalDnsSearchDomain;
use ployz_core::{
    ApiRefusal, DeployExecuteOutcome, DeployExecuteRequest, DeployVerb, HealthGatePolicy,
    PlacementBid, PlacementBidRequest, ServiceContainerObservation,
};
use tokio::sync::watch;

use super::deploy_runtime::{DeployRuntime, bounded_diagnostic};
use super::deploy_stores::DeployOperationRows;
use super::mutations::{decode_request, now_timestamp, simple_error, typed_response};
use super::server::{ApiService, HttpBody, corrosion_unavailable_response, refusal_response};
use super::store::read_accepted_roster;
use crate::roles::api::execution::docker::runner::DockerManagedContainerRunner;
use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, ExistingV2ManagedContainer,
    V2MachineContainerRunner, V2MachineImageRunner,
};
use crate::roles::system_observation::SystemObservation;

/// Upper bound on one full bid collection round against local Docker.
const BID_COLLECTION_TIMEOUT: Duration = Duration::from_secs(10);
/// Per-verb budget mirroring the driver-local external effect timeout.
const EXECUTE_EFFECT_TIMEOUT: Duration = Duration::from_secs(60);
/// The long budget for the pull verb, covering the runner's internally
/// bounded pull with its registry retries.
const EXECUTE_PULL_TIMEOUT: Duration = Duration::from_secs(180);
/// How long the responder waits for its replica to converge on the verb's
/// operation row before answering `ClaimNotYetVisible`. Mirrors the
/// operation lens wait.
const CLAIM_VISIBILITY_WAIT: Duration = Duration::from_secs(15);
const CLAIM_VISIBILITY_POLL: Duration = Duration::from_millis(500);

/// The local Docker surface both placement handlers command.
///
/// One real implementation adapts the machine's Docker runner; tests fake
/// the seam so no handler behavior depends on a live daemon.
#[async_trait]
pub(super) trait PlacementDockerRunner: Send + Sync {
    async fn managed_containers(&self) -> Result<Vec<ExistingV2ManagedContainer>, String>;
    async fn held_volumes(
        &self,
        namespace_id: &NamespaceRowId,
        volumes: &BTreeSet<VolumeName>,
    ) -> Result<BTreeSet<VolumeName>, String>;
    async fn ensure_volume(
        &self,
        namespace_id: &NamespaceRowId,
        volume: &VolumeName,
    ) -> Result<(), String>;
    async fn pull_image(
        &self,
        image: &ImageReference,
        credential: Option<&RegistryCredential>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), String>;
    async fn create_container(
        &self,
        command: CreateV2ManagedContainer,
    ) -> Result<ContainerId, String>;
    async fn start_container(&self, container_id: &ContainerId) -> Result<(), String>;
    async fn stop_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String>;
    async fn remove_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String>;
    async fn health_gate(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String>;
    async fn container_ip(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String>;
}

#[async_trait]
impl PlacementDockerRunner for DockerManagedContainerRunner {
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
            .map_err(bounded_diagnostic)
    }

    async fn ensure_volume(
        &self,
        namespace_id: &NamespaceRowId,
        volume: &VolumeName,
    ) -> Result<(), String> {
        self.ensure_v2_volume(namespace_id, volume)
            .await
            .map_err(bounded_diagnostic)
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

    async fn create_container(
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

    async fn stop_container(
        &self,
        container_id: &ContainerId,
        expected_identity: &V2ManagedContainerIdentity,
    ) -> Result<(), String> {
        self.stop_v2_managed_container(container_id, expected_identity)
            .await
            .map(|_| ())
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

    async fn health_gate(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        DeployRuntime::health_gate(self, container_id, identity).await
    }

    async fn container_ip(
        &self,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Ipv4Addr, String> {
        DeployRuntime::container_ip(self, container_id, identity).await
    }
}

pub(super) async fn handle_placement_bid(
    service: &ApiService,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    let Some(runner) = service.container_runner.as_ref() else {
        return refusal_response(ApiRefusal::UnsupportedRoute);
    };
    let bid_request: PlacementBidRequest = match decode_request(request.into_body()).await {
        Ok(bid_request) => bid_request,
        Err(response) => return response,
    };
    let machine = match local_machine(service).await {
        Ok(machine) => machine,
        Err(response) => return response,
    };
    let observation = match SystemObservation::read() {
        Ok(observation) => observation,
        Err(error) => return bid_decline(&error.to_string()),
    };
    let collected = tokio::time::timeout(
        BID_COLLECTION_TIMEOUT,
        collect_bid(
            runner.as_ref(),
            BidIdentity {
                machine_id: service.local_machine_id.clone(),
                machine_name: machine.name,
                lifecycle: machine.lifecycle,
            },
            observation,
            &bid_request,
        ),
    )
    .await;
    match collected {
        Ok(Ok(bid)) => typed_response(StatusCode::OK, &bid),
        Ok(Err(diagnostic)) => bid_decline(&diagnostic),
        Err(_) => bid_decline("bid collection timed out"),
    }
}

pub(super) async fn handle_deploy_execute(
    service: &ApiService,
    principal: Principal,
    request: hyper::Request<hyper::body::Incoming>,
    shutdown: watch::Receiver<bool>,
) -> Response<HttpBody> {
    let Some(runner) = service.container_runner.as_ref() else {
        return refusal_response(ApiRefusal::UnsupportedRoute);
    };
    let execute: DeployExecuteRequest = match decode_request(request.into_body()).await {
        Ok(execute) => execute,
        Err(response) => return response,
    };
    let store = service.operations.store();
    let outcome = execute_verb(&store, runner.as_ref(), &principal, execute, shutdown).await;
    typed_response(StatusCode::OK, &outcome)
}

/// A Docker or observation failure declines the bid; the driver classifies
/// the decline as anomalous silence.
fn bid_decline(diagnostic: &str) -> Response<HttpBody> {
    tracing::warn!(diagnostic, "declining placement bid");
    simple_error(StatusCode::SERVICE_UNAVAILABLE, "bid_unavailable")
}

async fn local_machine(service: &ApiService) -> Result<MachineDocument, Response<HttpBody>> {
    let roster = match read_accepted_roster(&service.corrosion, &service.cluster_id).await {
        Ok(roster) => roster,
        Err(error) => {
            tracing::warn!(error = %error, "could not read the roster for a placement bid");
            return Err(corrosion_unavailable_response());
        }
    };
    roster
        .machines
        .into_iter()
        .find_map(|machine| (machine.id == service.local_machine_id).then_some(machine.document))
        .ok_or_else(|| bid_decline("local machine row is not visible in the accepted roster"))
}

struct BidIdentity {
    machine_id: MachineRowId,
    machine_name: MachineName,
    lifecycle: MachineLifecycle,
}

async fn collect_bid(
    runner: &dyn PlacementDockerRunner,
    identity: BidIdentity,
    observation: SystemObservation,
    request: &PlacementBidRequest,
) -> Result<PlacementBid, String> {
    let containers = runner.managed_containers().await?;
    let volumes_held = runner
        .held_volumes(&request.namespace_id, &request.volumes)
        .await?;
    let BidIdentity {
        machine_id,
        machine_name,
        lifecycle,
    } = identity;
    let SystemObservation {
        free_disk_bytes,
        free_memory_bytes,
        load,
    } = observation;
    Ok(PlacementBid {
        machine_id,
        machine_name,
        architecture: std::env::consts::ARCH.to_owned(),
        lifecycle,
        free_disk_bytes,
        free_memory_bytes,
        load,
        total_container_count: containers.len(),
        service_containers: service_observations(
            containers,
            &request.namespace_id,
            &request.service_id,
        ),
        volumes_held,
    })
}

fn service_observations(
    containers: Vec<ExistingV2ManagedContainer>,
    namespace_id: &NamespaceRowId,
    service_id: &ServiceRowId,
) -> Vec<ServiceContainerObservation> {
    containers
        .into_iter()
        .filter(|container| {
            &container.identity.namespace_id == namespace_id
                && &container.identity.service_id == service_id
        })
        .map(|container| ServiceContainerObservation {
            container_id: container.container_id,
            deploy: container.identity.operation_id,
            running: matches!(
                container.state,
                ExistingManagedContainerState::Running { .. }
            ),
            named_volumes: container.named_volume_names,
        })
        .collect()
}

pub(super) async fn execute_verb(
    rows: &dyn DeployOperationRows,
    runner: &dyn PlacementDockerRunner,
    caller: &Principal,
    request: DeployExecuteRequest,
    shutdown: watch::Receiver<bool>,
) -> DeployExecuteOutcome {
    match authorize(rows, caller, &request).await {
        Ok(()) => run_verb(runner, request, shutdown).await,
        Err(outcome) => outcome,
    }
}

/// One local claim reading; `NotYetSettled` means the replica may still be
/// converging and the check is worth repeating.
enum ClaimCheck {
    Authorized,
    NotYetSettled,
}

/// Authorizes the verb against the live deploy claim on the local replica,
/// waiting out replication lag within [`CLAIM_VISIBILITY_WAIT`] before
/// answering [`DeployExecuteOutcome::ClaimNotYetVisible`].
async fn authorize(
    rows: &dyn DeployOperationRows,
    caller: &Principal,
    request: &DeployExecuteRequest,
) -> Result<(), DeployExecuteOutcome> {
    let deadline = tokio::time::Instant::now() + CLAIM_VISIBILITY_WAIT;
    loop {
        match check_claim(rows, caller, request).await? {
            ClaimCheck::Authorized => return Ok(()),
            ClaimCheck::NotYetSettled => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(DeployExecuteOutcome::ClaimNotYetVisible);
                }
                tokio::time::sleep(CLAIM_VISIBILITY_POLL).await;
            }
        }
    }
}

async fn check_claim(
    rows: &dyn DeployOperationRows,
    caller: &Principal,
    request: &DeployExecuteRequest,
) -> Result<ClaimCheck, DeployExecuteOutcome> {
    let now = now_timestamp().map_err(|()| DeployExecuteOutcome::Failed {
        diagnostic: "could not read the local clock".to_owned(),
    })?;
    let candidates = rows
        .deploy_claim_candidates(&request.service_id)
        .await
        .map_err(store_failure)?;
    if let Some((_, document)) = candidates
        .iter()
        .find(|(operation_id, _)| operation_id == &request.operation_id)
    {
        let CorrosionOperation::Deploy { namespace_id, .. } = document.operation() else {
            return Err(DeployExecuteOutcome::ServiceMismatch);
        };
        if namespace_id != &request.namespace_id {
            return Err(DeployExecuteOutcome::ServiceMismatch);
        }
        if caller_machine(caller) != Some(&document.machine_id) {
            return Err(DeployExecuteOutcome::CallerNotDriver {
                driver: document.machine_id.clone(),
            });
        }
        return match document.deploy_liveness(now) {
            DeployLiveness::Live => Ok(ClaimCheck::Authorized),
            // A stale heartbeat here is replication lag of the driver's own
            // refresh: the caller just proved the driver alive. Answer
            // convergence, never refusal.
            DeployLiveness::Stale => Ok(ClaimCheck::NotYetSettled),
        };
    }
    let observed = rows
        .operation(&request.operation_id)
        .await
        .map_err(store_failure)?;
    match observed {
        Some(observed) if observed.document.is_terminal() => {
            Err(DeployExecuteOutcome::ClaimTerminal)
        }
        Some(observed) => {
            // A non-terminal deploy claim for this service that missed the
            // candidate read raced its own insert; re-read before judging.
            let races_candidates = observed
                .document
                .deploy_targets()
                .is_some_and(|targets| targets.as_slice().contains(&request.service_id));
            if races_candidates {
                Ok(ClaimCheck::NotYetSettled)
            } else {
                Err(DeployExecuteOutcome::ServiceMismatch)
            }
        }
        None => Ok(ClaimCheck::NotYetSettled),
    }
}

fn caller_machine(principal: &Principal) -> Option<&MachineRowId> {
    match principal {
        Principal::Machine { machine_id } => Some(machine_id),
        Principal::Peer { .. } | Principal::ApiToken { .. } => None,
    }
}

fn store_failure(error: super::operation_store::OperationStoreError) -> DeployExecuteOutcome {
    DeployExecuteOutcome::Failed {
        diagnostic: bounded_diagnostic(format!("local operation rows unavailable: {error}")),
    }
}

async fn run_verb(
    runner: &dyn PlacementDockerRunner,
    request: DeployExecuteRequest,
    shutdown: watch::Receiver<bool>,
) -> DeployExecuteOutcome {
    let DeployExecuteRequest {
        operation_id,
        namespace_id,
        service_id,
        verb,
    } = request;
    match verb {
        DeployVerb::ListServiceContainers => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                let containers = runner.managed_containers().await?;
                Ok(DeployExecuteOutcome::ServiceContainers {
                    containers: service_observations(containers, &namespace_id, &service_id),
                })
            })
            .await
        }
        DeployVerb::EnsureVolume { volume } => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                runner.ensure_volume(&namespace_id, &volume).await?;
                Ok(DeployExecuteOutcome::VolumeEnsured)
            })
            .await
        }
        DeployVerb::PullImage { image, credential } => {
            bounded(EXECUTE_PULL_TIMEOUT, async {
                runner
                    .pull_image(&image, credential.as_ref(), shutdown)
                    .await?;
                Ok(DeployExecuteOutcome::ImagePulled)
            })
            .await
        }
        DeployVerb::CreateContainer {
            image,
            runtime,
            namespace_name,
        } => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                let dns_search_domain =
                    InternalDnsSearchDomain::try_from_namespace_label(namespace_name.as_str())
                        .map_err(|error| bounded_diagnostic(error.to_string()))?;
                let container_id = runner
                    .create_container(CreateV2ManagedContainer {
                        image,
                        runtime: *runtime,
                        dns_search_domain,
                        identity: V2ManagedContainerIdentity {
                            namespace_id,
                            service_id,
                            operation_id,
                        },
                    })
                    .await?;
                Ok(DeployExecuteOutcome::ContainerCreated { container_id })
            })
            .await
        }
        DeployVerb::StartContainer { container_id } => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                runner.start_container(&container_id).await?;
                Ok(DeployExecuteOutcome::ContainerStarted)
            })
            .await
        }
        DeployVerb::StopContainer { container_id } => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                // An absent container needs no stop; the sweep semantics
                // treat it as already settled.
                if let Some(container) =
                    service_scoped_container(runner, &container_id, &namespace_id, &service_id)
                        .await?
                {
                    runner
                        .stop_container(&container.container_id, &container.identity)
                        .await?;
                }
                Ok(DeployExecuteOutcome::ContainerStopped)
            })
            .await
        }
        DeployVerb::HealthGate {
            container_id,
            policy,
        } => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                let identity = V2ManagedContainerIdentity {
                    namespace_id,
                    service_id,
                    operation_id,
                };
                let ip = match policy {
                    HealthGatePolicy::Enforce => {
                        runner.health_gate(&container_id, &identity).await?
                    }
                    HealthGatePolicy::Skip => runner.container_ip(&container_id, &identity).await?,
                };
                Ok(DeployExecuteOutcome::HealthGated { ip })
            })
            .await
        }
        DeployVerb::RestartContainer { container_id } => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                match service_scoped_container(runner, &container_id, &namespace_id, &service_id)
                    .await?
                {
                    Some(container) => {
                        runner.start_container(&container.container_id).await?;
                        Ok(DeployExecuteOutcome::ContainerRestarted)
                    }
                    None => Err("container to restart was not found in Docker".to_owned()),
                }
            })
            .await
        }
        DeployVerb::RemoveContainer { container_id } => {
            bounded(EXECUTE_EFFECT_TIMEOUT, async {
                // Already absent; removal is settled.
                if let Some(container) =
                    service_scoped_container(runner, &container_id, &namespace_id, &service_id)
                        .await?
                {
                    runner
                        .remove_container(&container.container_id, &container.identity)
                        .await?;
                }
                Ok(DeployExecuteOutcome::ContainerRemoved)
            })
            .await
        }
    }
}

/// Recovers the container's own Docker identity and refuses containers that
/// do not belong to the verb's service, so a claim never touches another
/// service's containers.
async fn service_scoped_container(
    runner: &dyn PlacementDockerRunner,
    container_id: &ContainerId,
    namespace_id: &NamespaceRowId,
    service_id: &ServiceRowId,
) -> Result<Option<ExistingV2ManagedContainer>, String> {
    let containers = runner.managed_containers().await?;
    let Some(container) = containers
        .into_iter()
        .find(|container| &container.container_id == container_id)
    else {
        return Ok(None);
    };
    if &container.identity.namespace_id != namespace_id
        || &container.identity.service_id != service_id
    {
        return Err("container does not belong to the verb's service".to_owned());
    }
    Ok(Some(container))
}

async fn bounded(
    budget: Duration,
    effect: impl Future<Output = Result<DeployExecuteOutcome, String>> + Send,
) -> DeployExecuteOutcome {
    match tokio::time::timeout(budget, effect).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(diagnostic)) => DeployExecuteOutcome::Failed {
            diagnostic: bounded_diagnostic(diagnostic),
        },
        Err(_) => DeployExecuteOutcome::Failed {
            diagnostic: "deploy verb timed out".to_owned(),
        },
    }
}

#[cfg(test)]
#[path = "placement_tests.rs"]
mod tests;
