use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{
    ContainerDocument, CorrosionDeployFailure, CorrosionDeployOutcome,
    CorrosionDeployServiceFailure, CorrosionDeployServiceResult, CorrosionDeployTargets,
    CorrosionDeployTransition, CorrosionDeployWarning, CorrosionDocumentVersion,
    CorrosionNamespaceName, CorrosionPromotionFailure, CorrosionServiceName, CorrosionTimestamp,
    DEPLOY_HEARTBEAT_INTERVAL, DeployClaim, DeployTakeover, OperationDocument, OperationInitiator,
    OperatorWriteProvenance, ServiceDocument, ServicePlacement, ServiceReplicaCount,
    V2ManagedContainerIdentity, adjudicate_deploy_claim, check_deploy_takeover,
    fingerprint_env_value,
};
use ployz_core::ids::{ClusterId, ContainerId, MachineRowId, OperationRowId, ServiceRowId};
use ployz_core::network::EndpointBridgeStatus;
use ployz_core::{DeployAccepted, DeployRefusal, DeployRequest, HealthGatePolicy};
use ployz_core::{OperationEvidence, deploy::ImageReference};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::{Mutex, watch};
use tokio::time::MissedTickBehavior;

use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, ExistingV2ManagedContainer,
    MachineContainerRunner, MachineContainerStopOutcome, V2MachineContainerRunner,
    V2MachineImageRunner,
};
use crate::roles::dns::internal::DNS_TTL_SECONDS;

use super::operation_evidence::{
    DurablePromotionProgress, EvidenceIdentity, OperationEvidenceDirectory, OperationEvidenceLog,
    PreparedPromotion, PreparedRedeployIntent,
};
use super::operation_finalizer::{
    PreparedPromotionStore, PromotionFinalizerDecision, PromotionFinalizerState,
    PromotionFinalizerStoreError, PromotionRequestDisposition, PromotionRowsObservation,
    RedeployRowsDecision, UncertaintyDeadline, classify_redeploy_rows,
};
use super::operation_store::{
    ConditionalOperationWrite, HeartbeatWrite, ObservedOperation, OperationStore,
};
use super::promotion_store::{
    ContainerRowCleanup, CorrosionPreparedPromotionStore, DeployAdmission, ObservedContainer,
    ObservedService, PreparedRedeploy, ResolvedFirstDeployNamespace,
};

const EXTERNAL_EFFECT_TIMEOUT: Duration = Duration::from_secs(60);
const FINALIZER_ATTEMPTS: usize = 3;
const RUNNING_CONFIRMATION_WINDOW: Duration = Duration::from_secs(5);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_PERSISTED_DIAGNOSTIC_BYTES: usize = 4 * 1024;

/// Waits for rival claim rows to gossip in before adjudicating the op claim.
const CLAIM_COURTESY_WAIT: Duration = Duration::from_secs(1);

/// Old-revision drain after a stateless flip: one full DNS TTL plus a second
/// of skew, so cached answers stop pointing at the incumbent before it stops.
pub(super) const DEPLOY_DRAIN_WAIT: Duration = Duration::from_secs(DNS_TTL_SECONDS as u64 + 1);
const _: () = assert!(DEPLOY_DRAIN_WAIT.as_secs() >= DNS_TTL_SECONDS as u64);

pub(super) trait DeployStore: PreparedPromotionStore + RedeployStore {}

impl<T> DeployStore for T where T: PreparedPromotionStore + RedeployStore {}

/// Admission triage and exact redeploy row writes, lifted onto the driver seam.
#[async_trait]
pub(super) trait RedeployStore: Send + Sync {
    async fn resolve_deploy_admission(
        &self,
        namespace_name: &CorrosionNamespaceName,
        service_name: &CorrosionServiceName,
    ) -> Result<DeployAdmission, PromotionFinalizerStoreError>;

    async fn converge_redeploy_rows(
        &self,
        prepared: &PreparedRedeployIntent,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>;

    async fn service_containers(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<ObservedContainer>, PromotionFinalizerStoreError>;

    async fn delete_exact_container_rows(
        &self,
        rows: &[ObservedContainer],
    ) -> Result<Vec<(ContainerId, ContainerRowCleanup)>, PromotionFinalizerStoreError>;
}

#[async_trait]
impl RedeployStore for CorrosionPreparedPromotionStore {
    async fn resolve_deploy_admission(
        &self,
        namespace_name: &CorrosionNamespaceName,
        service_name: &CorrosionServiceName,
    ) -> Result<DeployAdmission, PromotionFinalizerStoreError> {
        CorrosionPreparedPromotionStore::resolve_deploy_admission(
            self,
            namespace_name,
            service_name,
        )
        .await
    }

    async fn converge_redeploy_rows(
        &self,
        prepared: &PreparedRedeployIntent,
    ) -> Result<(PromotionRequestDisposition, PromotionRowsObservation), PromotionFinalizerStoreError>
    {
        let prepared = PreparedRedeploy {
            service_id: prepared.service_id.clone(),
            exact_incumbent_document: prepared.exact_incumbent_document.clone(),
            service_document: prepared.service_document.clone(),
            container_id: prepared.container_id.clone(),
            container_document: prepared.container_document.clone(),
        };
        CorrosionPreparedPromotionStore::converge_redeploy_rows(self, &prepared).await
    }

    async fn service_containers(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<ObservedContainer>, PromotionFinalizerStoreError> {
        CorrosionPreparedPromotionStore::service_containers(self, service_id).await
    }

    async fn delete_exact_container_rows(
        &self,
        rows: &[ObservedContainer],
    ) -> Result<Vec<(ContainerId, ContainerRowCleanup)>, PromotionFinalizerStoreError> {
        CorrosionPreparedPromotionStore::delete_exact_container_rows(self, rows).await
    }
}

#[async_trait]
pub(super) trait DeployOperationRows: Send + Sync {
    async fn insert_created(
        &self,
        operation_id: &OperationRowId,
        operation: &OperationDocument,
    ) -> Result<(), String>;

    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, String>;

    async fn transition_deploy(
        &self,
        observed: ObservedOperation,
        transition: CorrosionDeployTransition,
    ) -> Result<ConditionalOperationWrite, String>;

    async fn replace_terminal(
        &self,
        observed: &ObservedOperation,
        terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, String>;

    async fn refresh_heartbeat(
        &self,
        observed: &ObservedOperation,
        now: CorrosionTimestamp,
    ) -> Result<HeartbeatWrite, String>;

    async fn deploy_claim_candidates(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, String>;

    async fn deploy_takeover_candidates(
        &self,
        operation_id: &OperationRowId,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, String>;
}

#[async_trait]
impl DeployOperationRows for OperationStore {
    async fn insert_created(
        &self,
        operation_id: &OperationRowId,
        operation: &OperationDocument,
    ) -> Result<(), String> {
        OperationStore::insert_created(self, operation_id, operation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn operation(
        &self,
        operation_id: &OperationRowId,
    ) -> Result<Option<ObservedOperation>, String> {
        OperationStore::operation(self, operation_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn transition_deploy(
        &self,
        observed: ObservedOperation,
        transition: CorrosionDeployTransition,
    ) -> Result<ConditionalOperationWrite, String> {
        OperationStore::transition_deploy(self, observed, transition)
            .await
            .map_err(|error| error.to_string())
    }

    async fn replace_terminal(
        &self,
        observed: &ObservedOperation,
        terminal: &OperationDocument,
    ) -> Result<ConditionalOperationWrite, String> {
        OperationStore::replace_terminal(self, observed, terminal)
            .await
            .map_err(|error| error.to_string())
    }

    async fn refresh_heartbeat(
        &self,
        observed: &ObservedOperation,
        now: CorrosionTimestamp,
    ) -> Result<HeartbeatWrite, String> {
        OperationStore::refresh_heartbeat(self, observed, now)
            .await
            .map_err(|error| error.to_string())
    }

    async fn deploy_claim_candidates(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, String> {
        OperationStore::deploy_claim_candidates(self, service_id)
            .await
            .map_err(|error| error.to_string())
    }

    async fn deploy_takeover_candidates(
        &self,
        operation_id: &OperationRowId,
        service_id: &ServiceRowId,
    ) -> Result<Vec<(OperationRowId, OperationDocument)>, String> {
        OperationStore::deploy_takeover_candidates(self, operation_id, service_id)
            .await
            .map_err(|error| error.to_string())
    }
}

#[async_trait]
pub(super) trait DeployRuntime: Send + Sync {
    async fn bridge_ready(&self) -> bool;
    async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String>;
    async fn pull_image(
        &self,
        image: &ImageReference,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), String>;
    async fn create_container(
        &self,
        request: &DeployRequest,
        resolved_image: &ImageReference,
        namespace: &ResolvedFirstDeployNamespace,
        identity: V2ManagedContainerIdentity,
    ) -> Result<ContainerId, String>;
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
    async fn service_docker_containers(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<ExistingV2ManagedContainer>, String>;
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
impl<Runner> DeployRuntime for Runner
where
    Runner: MachineContainerRunner + V2MachineContainerRunner + V2MachineImageRunner + Send + Sync,
{
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
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), String> {
        self.pull_v2_registry_image(image, None, shutdown)
            .await
            .map_err(|error| bounded_diagnostic(error.to_string()))
    }

    async fn create_container(
        &self,
        request: &DeployRequest,
        resolved_image: &ImageReference,
        namespace: &ResolvedFirstDeployNamespace,
        identity: V2ManagedContainerIdentity,
    ) -> Result<ContainerId, String> {
        let namespace_name = namespace.document.name.clone();
        let dns_search_domain =
            ployz_core::network::internal_dns::InternalDnsSearchDomain::try_from_namespace_label(
                namespace_name.as_str(),
            )
            .map_err(|error| bounded_diagnostic(error.to_string()))?;
        self.create_v2_managed_container(CreateV2ManagedContainer {
            image: resolved_image.clone(),
            runtime: request.runtime.clone(),
            dns_search_domain,
            identity,
        })
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

    async fn service_docker_containers(
        &self,
        service_id: &ServiceRowId,
    ) -> Result<Vec<ExistingV2ManagedContainer>, String> {
        let containers = self
            .existing_v2_managed_containers()
            .await
            .map_err(|error| bounded_diagnostic(format!("{error:?}")))?;
        Ok(containers
            .into_iter()
            .filter(|container| &container.identity.service_id == service_id)
            .collect())
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

pub(super) trait DeployClock: Send + Sync {
    fn now(&self) -> Result<CorrosionTimestamp, String>;
}

pub(super) struct SystemDeployClock;

impl DeployClock for SystemDeployClock {
    fn now(&self) -> Result<CorrosionTimestamp, String> {
        let value = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| error.to_string())?;
        CorrosionTimestamp::try_new(value).map_err(|error| error.to_string())
    }
}

fn clone_observed(observed: &ObservedOperation) -> ObservedOperation {
    ObservedOperation {
        id: observed.id.clone(),
        exact_document: observed.exact_document.clone(),
        document: observed.document.clone(),
    }
}

fn observed_with(
    id: &OperationRowId,
    document: OperationDocument,
) -> Result<ObservedOperation, DeployDriverError> {
    let exact_document = serde_json::to_string(&document)
        .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
    Ok(ObservedOperation {
        id: id.clone(),
        exact_document,
        document,
    })
}

#[derive(Clone)]
pub(super) struct DeployDriver {
    cluster_id: ClusterId,
    machine_id: MachineRowId,
    evidence: OperationEvidenceDirectory,
    store: Arc<dyn DeployStore>,
    operations: Arc<dyn DeployOperationRows>,
    runtime: Arc<dyn DeployRuntime>,
    clock: Arc<dyn DeployClock>,
    effect_timeout: Duration,
    claim_courtesy_wait: Duration,
    drain_wait: Duration,
}

/// Which admission case this operation is executing.
enum DeployPath {
    First {
        namespace: ResolvedFirstDeployNamespace,
    },
    Redeploy {
        namespace: ResolvedFirstDeployNamespace,
        incumbent: Box<ObservedService>,
    },
}

/// How one deploy task's phases ended; terminalization happens only after the
/// heartbeat task has stopped, so terminal-is-final holds for the op row.
enum DeployTaskEnd {
    FinishFirstDeploy {
        prepared: Box<PreparedPromotion>,
        warnings: Vec<CorrosionDeployWarning>,
    },
    Completed {
        warnings: Vec<CorrosionDeployWarning>,
    },
    ServiceFailure {
        failure: CorrosionDeployServiceFailure,
    },
    Failure {
        failure: CorrosionDeployFailure,
    },
    Interrupted,
    /// The own row is already terminal; some earlier write settled this
    /// operation, so the task stops without another terminal write.
    StopSilently,
}

/// The flip verdict shared by the live task and startup resume.
enum RedeployFlipEnd {
    Committed,
    Superseded { winner: OperationRowId },
    Failure { failure: CorrosionPromotionFailure },
}

impl DeployDriver {
    #[must_use]
    pub(super) fn new(
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        evidence: OperationEvidenceDirectory,
        store: Arc<dyn DeployStore>,
        operations: Arc<dyn DeployOperationRows>,
        runtime: Arc<dyn DeployRuntime>,
        clock: Arc<dyn DeployClock>,
    ) -> Self {
        Self {
            cluster_id,
            machine_id,
            evidence,
            store,
            operations,
            runtime,
            clock,
            effect_timeout: EXTERNAL_EFFECT_TIMEOUT,
            claim_courtesy_wait: CLAIM_COURTESY_WAIT,
            drain_wait: DEPLOY_DRAIN_WAIT,
        }
    }

    #[cfg(test)]
    #[must_use]
    fn with_waits(mut self, claim_courtesy_wait: Duration, drain_wait: Duration) -> Self {
        self.claim_courtesy_wait = claim_courtesy_wait;
        self.drain_wait = drain_wait;
        self
    }

    pub(super) async fn admit(
        &self,
        request: DeployRequest,
        initiator: OperationInitiator,
    ) -> Result<Result<AcceptedDeploy, DeployRefusal>, DeployDriverError> {
        let admission = self
            .store
            .resolve_deploy_admission(&request.namespace_name, &request.service_name)
            .await?;
        let path = match admission {
            DeployAdmission::NamespaceMissing => {
                return Ok(Err(DeployRefusal::namespace_not_found(
                    request.namespace_name,
                )));
            }
            DeployAdmission::NamespaceAmbiguous { namespace_ids } => {
                return Ok(Err(DeployRefusal::NamespaceAmbiguous {
                    namespace_name: request.namespace_name,
                    namespace_ids,
                }));
            }
            DeployAdmission::DifferentService {
                namespace_id,
                incumbent_name,
            } => {
                return Ok(Err(DeployRefusal::DifferentService {
                    namespace_id,
                    incumbent_service_name: incumbent_name,
                }));
            }
            DeployAdmission::MultipleServices {
                namespace_id,
                service_ids,
            } => {
                return Ok(Err(DeployRefusal::MultipleServices {
                    namespace_id,
                    service_ids,
                }));
            }
            DeployAdmission::RoutesWithoutServices { namespace_id } => {
                return Ok(Err(DeployRefusal::RoutesWithoutServices { namespace_id }));
            }
            DeployAdmission::FirstDeploy { namespace } => DeployPath::First { namespace },
            DeployAdmission::Redeploy {
                namespace,
                incumbent,
            } => {
                // This slice commands only the incumbent's own machine.
                let Some(pinned) = incumbent.document.pinned_machines.iter().next().cloned() else {
                    return Err(DeployDriverError::Invariant(
                        "incumbent service pins no machine".to_owned(),
                    ));
                };
                if !incumbent
                    .document
                    .pinned_machines
                    .contains(&self.machine_id)
                {
                    return Ok(Err(DeployRefusal::IncumbentOnAnotherMachine {
                        machine_id: pinned,
                    }));
                }
                DeployPath::Redeploy {
                    namespace,
                    incumbent,
                }
            }
        };
        if !bounded(self.effect_timeout, self.runtime.bridge_ready())
            .await
            .unwrap_or(false)
        {
            return Ok(Err(DeployRefusal::BridgeUnavailable));
        }

        let operation_id = OperationRowId::generate();
        let service_id = match &path {
            DeployPath::First { .. } => ServiceRowId::generate(),
            DeployPath::Redeploy { incumbent, .. } => incumbent.id.clone(),
        };
        let namespace_id = match &path {
            DeployPath::First { namespace } | DeployPath::Redeploy { namespace, .. } => {
                namespace.id.clone()
            }
        };
        let created_at = self.clock.now().map_err(DeployDriverError::Clock)?;
        let operation = OperationDocument::deploy_created(
            CorrosionDocumentVersion::V1,
            self.cluster_id.clone(),
            self.machine_id.clone(),
            initiator.clone(),
            namespace_id,
            CorrosionDeployTargets::try_new(vec![service_id.clone()])
                .map_err(|error| DeployDriverError::Invariant(error.to_string()))?,
            created_at,
        );
        let log = self
            .evidence
            .create(
                EvidenceIdentity::new(operation_id.clone(), self.machine_id.clone()),
                created_at,
            )
            .await?;
        let row = observed_with(&operation_id, operation.clone())?;
        if let Err(error) = self
            .operations
            .insert_created(&operation_id, &operation)
            .await
        {
            return Err(DeployDriverError::Operation(error));
        }
        Ok(Ok(AcceptedDeploy {
            reply: DeployAccepted {
                operation_id: operation_id.clone(),
                driver_machine_id: self.machine_id.clone(),
            },
            task: DeployTask {
                driver: self.clone(),
                operation_id,
                service_id,
                request,
                initiator,
                path,
                log,
                row: Arc::new(Mutex::new(row)),
            },
        }))
    }

    pub(super) async fn resume_promotion(
        &self,
        operation_id: OperationRowId,
        log: OperationEvidenceLog,
        progress: DurablePromotionProgress,
    ) -> Result<(), DeployDriverError> {
        let observed = self
            .operations
            .operation(&operation_id)
            .await
            .map_err(DeployDriverError::Operation)?
            .ok_or_else(|| DeployDriverError::Operation("operation row disappeared".to_owned()))?;
        if observed.document.is_terminal() {
            return Ok(());
        }
        let row = Arc::new(Mutex::new(observed));
        let state = match progress {
            DurablePromotionProgress::PromotionPrepared { prepared } => {
                PromotionFinalizerState::PromotionPrepared { prepared }
            }
            DurablePromotionProgress::RowsCommitted { prepared } => {
                PromotionFinalizerState::RowsCommitted { prepared }
            }
            DurablePromotionProgress::ClaimWon { prepared } => {
                PromotionFinalizerState::ClaimWon { prepared }
            }
            DurablePromotionProgress::ClaimLost { prepared, winner } => {
                PromotionFinalizerState::ClaimLost { prepared, winner }
            }
            DurablePromotionProgress::RedeployPrepared { prepared } => {
                return self
                    .resume_redeploy_flip(&operation_id, &log, &row, prepared)
                    .await;
            }
            DurablePromotionProgress::RedeployRowsCommitted { prepared } => {
                // The flip is durable; drain and clean are never resumed — the
                // next deploy's sweep collects whatever this one left behind.
                let outcome = redeploy_completed_outcome(&prepared)?;
                return self.terminalize(&log, &row, outcome).await;
            }
        };
        self.finish_promotion(&log, state, &row, Vec::new()).await
    }

    async fn resume_redeploy_flip(
        &self,
        operation_id: &OperationRowId,
        log: &OperationEvidenceLog,
        row: &Arc<Mutex<ObservedOperation>>,
        prepared: PreparedRedeployIntent,
    ) -> Result<(), DeployDriverError> {
        let service_id = prepared.service_id.clone();
        match self
            .converge_redeploy(&prepared, operation_id, &service_id)
            .await?
        {
            RedeployFlipEnd::Committed => {
                log.append(
                    self.clock.now().map_err(DeployDriverError::Clock)?,
                    OperationEvidence::RowsCommitted,
                )
                .await?;
                let outcome = redeploy_completed_outcome(&prepared)?;
                self.terminalize(log, row, outcome).await
            }
            RedeployFlipEnd::Superseded { winner } => {
                let outcome = CorrosionDeployOutcome::failed(
                    vec![CorrosionDeployServiceResult::skipped(service_id)],
                    CorrosionDeployFailure::SupersededByOperation { winner },
                )
                .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
                self.terminalize(log, row, outcome).await
            }
            RedeployFlipEnd::Failure { failure } => {
                let outcome = CorrosionDeployOutcome::failed(
                    vec![CorrosionDeployServiceResult::skipped(service_id.clone())],
                    CorrosionDeployFailure::Promotion {
                        service_id,
                        failure,
                    },
                )
                .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
                self.terminalize(log, row, outcome).await
            }
        }
    }

    /// Applies the incumbent CAS until it lands, is definitively lost, or the
    /// uncertainty deadline is reached. A CAS miss names the takeover winner
    /// when one is visible; otherwise it stays a typed rejection.
    async fn converge_redeploy(
        &self,
        prepared: &PreparedRedeployIntent,
        operation_id: &OperationRowId,
        service_id: &ServiceRowId,
    ) -> Result<RedeployFlipEnd, DeployDriverError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let deadline = if attempts >= FINALIZER_ATTEMPTS {
                UncertaintyDeadline::Reached
            } else {
                UncertaintyDeadline::Open
            };
            let decision = match bounded(
                self.effect_timeout,
                self.store.converge_redeploy_rows(prepared),
            )
            .await
            {
                Ok(Ok((disposition, rows))) => classify_redeploy_rows(disposition, rows, deadline),
                Ok(Err(_)) | Err(_) if attempts < FINALIZER_ATTEMPTS => RedeployRowsDecision::Retry,
                Ok(Err(_)) | Err(_) => RedeployRowsDecision::Failure {
                    failure: CorrosionPromotionFailure::OutcomeUncertain,
                },
            };
            match decision {
                RedeployRowsDecision::Committed => return Ok(RedeployFlipEnd::Committed),
                RedeployRowsDecision::Retry if attempts < FINALIZER_ATTEMPTS => {}
                RedeployRowsDecision::Retry => {
                    return Ok(RedeployFlipEnd::Failure {
                        failure: CorrosionPromotionFailure::OutcomeUncertain,
                    });
                }
                RedeployRowsDecision::SupersededByCasMiss => {
                    let newer = match bounded(
                        self.effect_timeout,
                        self.operations
                            .deploy_takeover_candidates(operation_id, service_id),
                    )
                    .await
                    {
                        Ok(Ok(newer)) => newer,
                        Ok(Err(_)) | Err(_) => Vec::new(),
                    };
                    return match check_deploy_takeover(operation_id, &newer) {
                        DeployTakeover::TakenOver { winner } => {
                            Ok(RedeployFlipEnd::Superseded { winner })
                        }
                        DeployTakeover::Clear => Ok(RedeployFlipEnd::Failure {
                            failure: CorrosionPromotionFailure::Rejected,
                        }),
                    };
                }
                RedeployRowsDecision::Failure { failure } => {
                    return Ok(RedeployFlipEnd::Failure { failure });
                }
            }
        }
    }

    async fn finish_promotion(
        &self,
        log: &OperationEvidenceLog,
        mut state: PromotionFinalizerState,
        row: &Arc<Mutex<ObservedOperation>>,
        warnings: Vec<CorrosionDeployWarning>,
    ) -> Result<(), DeployDriverError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let decision = match state.clone() {
                PromotionFinalizerState::PromotionPrepared { ref prepared } => {
                    match bounded(self.effect_timeout, self.store.converge_rows(prepared)).await {
                        Ok(Ok((disposition, rows))) => state.clone().observe_prepared_rows(
                            disposition,
                            rows,
                            if attempts == FINALIZER_ATTEMPTS {
                                UncertaintyDeadline::Reached
                            } else {
                                UncertaintyDeadline::Open
                            },
                        ),
                        Ok(Err(_)) | Err(_) if attempts < FINALIZER_ATTEMPTS => {
                            PromotionFinalizerDecision::RetryRows {
                                state: state.clone(),
                            }
                        }
                        Ok(Err(_)) | Err(_) => state.clone().observe_prepared_rows(
                            PromotionRequestDisposition::Uncertain,
                            PromotionRowsObservation::ABSENT,
                            UncertaintyDeadline::Reached,
                        ),
                    }
                }
                PromotionFinalizerState::RowsCommitted { ref prepared } => {
                    match bounded(
                        self.effect_timeout,
                        self.store.adjudicate_service_claim(prepared),
                    )
                    .await
                    {
                        Ok(Ok(outcome)) => state.clone().observe_claim(outcome),
                        Ok(Err(_)) | Err(_) if attempts < FINALIZER_ATTEMPTS => {
                            continue;
                        }
                        Ok(Err(_)) | Err(_) => promotion_uncertain(state.clone()),
                    }
                }
                PromotionFinalizerState::ClaimWon { .. } => state.clone().finish_claim_won(),
                PromotionFinalizerState::ClaimLost { ref prepared, .. } => {
                    match bounded(
                        self.effect_timeout,
                        self.store.delete_exact_losing_rows(prepared),
                    )
                    .await
                    {
                        Ok(Ok(rows)) => state.clone().observe_loser_cleanup(rows),
                        Ok(Err(_)) | Err(_) if attempts < FINALIZER_ATTEMPTS => {
                            continue;
                        }
                        Ok(Err(_)) | Err(_) => promotion_uncertain(state.clone()),
                    }
                }
            };
            match decision {
                PromotionFinalizerDecision::RetryRows { state: next }
                | PromotionFinalizerDecision::RetryLoserCleanup { state: next } => {
                    if attempts >= FINALIZER_ATTEMPTS {
                        return self.terminalize_uncertain_promotion(log, row, next).await;
                    }
                    state = next;
                }
                PromotionFinalizerDecision::AppendRowsCommitted { state: next } => {
                    log.append(
                        self.clock.now().map_err(DeployDriverError::Clock)?,
                        OperationEvidence::RowsCommitted,
                    )
                    .await?;
                    state = next;
                    attempts = 0;
                }
                PromotionFinalizerDecision::AppendClaimWon { state: next } => {
                    log.append(
                        self.clock.now().map_err(DeployDriverError::Clock)?,
                        OperationEvidence::ClaimWon,
                    )
                    .await?;
                    state = next;
                    attempts = 0;
                }
                PromotionFinalizerDecision::AppendClaimLost {
                    state: next,
                    winner,
                } => {
                    log.append(
                        self.clock.now().map_err(DeployDriverError::Clock)?,
                        OperationEvidence::ClaimLost { winner },
                    )
                    .await?;
                    state = next;
                    attempts = 0;
                }
                PromotionFinalizerDecision::Succeeded { prepared } => {
                    let results = vec![prepared.success_result];
                    let outcome = if warnings.is_empty() {
                        CorrosionDeployOutcome::completed(results)
                    } else {
                        CorrosionDeployOutcome::completed_with_warnings(results, warnings.clone())
                    }
                    .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
                    return self.terminalize(log, row, outcome).await;
                }
                PromotionFinalizerDecision::Failed { prepared, failure } => {
                    let outcome = CorrosionDeployOutcome::failed(
                        vec![CorrosionDeployServiceResult::skipped(prepared.service_id)],
                        failure,
                    )
                    .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
                    return self.terminalize(log, row, outcome).await;
                }
            }
        }
    }

    /// Appends the durable terminal evidence, then commits the terminal row
    /// through the shared handle. Callers stop the heartbeat task first.
    async fn terminalize(
        &self,
        log: &OperationEvidenceLog,
        row: &Arc<Mutex<ObservedOperation>>,
        outcome: CorrosionDeployOutcome,
    ) -> Result<(), DeployDriverError> {
        let completed_at = self.clock.now().map_err(DeployDriverError::Clock)?;
        let mut row = row.lock().await;
        let transition = CorrosionDeployTransition::Terminal {
            completed_at,
            outcome,
        };
        let terminal = row
            .document
            .clone()
            .transition_deploy(transition.clone())
            .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
        log.append(
            completed_at,
            OperationEvidence::Terminal {
                operation: Box::new(terminal.clone()),
            },
        )
        .await?;
        match self
            .operations
            .transition_deploy(clone_observed(&row), transition)
            .await
            .map_err(DeployDriverError::Operation)?
        {
            ConditionalOperationWrite::Written => {
                *row = observed_with(&row.id, terminal)?;
                Ok(())
            }
            ConditionalOperationWrite::Stale => Err(DeployDriverError::Operation(
                "operation row changed before terminal".to_owned(),
            )),
        }
    }

    async fn terminalize_uncertain_promotion(
        &self,
        log: &OperationEvidenceLog,
        row: &Arc<Mutex<ObservedOperation>>,
        state: PromotionFinalizerState,
    ) -> Result<(), DeployDriverError> {
        let PromotionFinalizerDecision::Failed { prepared, failure } = promotion_uncertain(state)
        else {
            return Err(DeployDriverError::Invariant(
                "uncertain promotion did not produce a terminal failure".to_owned(),
            ));
        };
        let outcome = CorrosionDeployOutcome::failed(
            vec![CorrosionDeployServiceResult::skipped(prepared.service_id)],
            failure,
        )
        .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
        self.terminalize(log, row, outcome).await
    }
}

fn redeploy_completed_outcome(
    prepared: &PreparedRedeployIntent,
) -> Result<CorrosionDeployOutcome, DeployDriverError> {
    let results = vec![CorrosionDeployServiceResult::completed(
        prepared.service_id.clone(),
    )];
    match prepared.health_gate {
        HealthGatePolicy::Enforce => CorrosionDeployOutcome::completed(results),
        HealthGatePolicy::Skip => CorrosionDeployOutcome::completed_with_warnings(
            results,
            vec![CorrosionDeployWarning::HealthGateSkipped {
                service_id: prepared.service_id.clone(),
            }],
        ),
    }
    .map_err(|error| DeployDriverError::Invariant(error.to_string()))
}

fn promotion_uncertain(state: PromotionFinalizerState) -> PromotionFinalizerDecision {
    let prepared = match state {
        PromotionFinalizerState::PromotionPrepared { prepared }
        | PromotionFinalizerState::RowsCommitted { prepared }
        | PromotionFinalizerState::ClaimWon { prepared }
        | PromotionFinalizerState::ClaimLost { prepared, .. } => prepared,
    };
    PromotionFinalizerDecision::Failed {
        failure: CorrosionDeployFailure::Promotion {
            service_id: prepared.service_id.clone(),
            failure: CorrosionPromotionFailure::OutcomeUncertain,
        },
        prepared,
    }
}

pub(super) struct AcceptedDeploy {
    pub(super) reply: DeployAccepted,
    pub(super) task: DeployTask,
}

impl AcceptedDeploy {
    /// Registers the durable log before spawning so local watchers cannot miss a fast task.
    pub(super) fn operation_log(&self) -> OperationEvidenceLog {
        self.task.log.clone()
    }

    /// Closes the Created operation when task admission shuts between durable acceptance and spawn.
    pub(super) async fn interrupt_unspawned(self) -> Result<(), DeployDriverError> {
        self.task.interrupted().await
    }
}

/// A driver-owned task that refreshes the op-row heartbeat while phases run.
///
/// Every write goes through the shared row handle, so heartbeats and phase
/// transitions always CAS against the true latest document. A stale CAS
/// re-reads the row; a terminal or missing row raises the superseded flag.
struct DeployHeartbeat {
    stop: watch::Sender<bool>,
    superseded: watch::Receiver<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl DeployHeartbeat {
    fn spawn(driver: DeployDriver, row: Arc<Mutex<ObservedOperation>>) -> Self {
        let (stop, mut stop_rx) = watch::channel(false);
        let (superseded_tx, superseded) = watch::channel(false);
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(DEPLOY_HEARTBEAT_INTERVAL);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    _ = stop_rx.changed() => return,
                    _ = ticker.tick() => {}
                }
                let Ok(now) = driver.clock.now() else {
                    continue;
                };
                let mut row = row.lock().await;
                match bounded(
                    driver.effect_timeout,
                    driver.operations.refresh_heartbeat(&row, now),
                )
                .await
                {
                    Ok(Ok(HeartbeatWrite::Written(refreshed))) => *row = *refreshed,
                    Ok(Ok(HeartbeatWrite::Stale)) => {
                        match bounded(driver.effect_timeout, driver.operations.operation(&row.id))
                            .await
                        {
                            Ok(Ok(Some(current))) => {
                                if current.document.is_terminal() {
                                    let _ = superseded_tx.send(true);
                                    return;
                                }
                                *row = current;
                            }
                            Ok(Ok(None)) => {
                                let _ = superseded_tx.send(true);
                                return;
                            }
                            Ok(Err(_)) | Err(_) => {}
                        }
                    }
                    Ok(Err(_)) | Err(_) => {}
                }
            }
        });
        Self {
            stop,
            superseded,
            task,
        }
    }

    fn superseded(&self) -> bool {
        *self.superseded.borrow()
    }

    /// Stops the refresher and waits it out so no heartbeat write can land
    /// after the terminal write.
    async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

pub(super) struct DeployTask {
    driver: DeployDriver,
    operation_id: OperationRowId,
    service_id: ServiceRowId,
    request: DeployRequest,
    initiator: OperationInitiator,
    path: DeployPath,
    log: OperationEvidenceLog,
    row: Arc<Mutex<ObservedOperation>>,
}

impl DeployTask {
    pub(super) async fn run(
        &self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), DeployDriverError> {
        if shutdown_requested(&shutdown) {
            return self.interrupted().await;
        }
        // Claim phase: courtesy beat, adjudicate, and only then mark running.
        tokio::select! {
            _ = shutdown.changed() => return self.interrupted().await,
            () = tokio::time::sleep(self.driver.claim_courtesy_wait) => {}
        }
        let adjudicated_at = self.now()?;
        let candidates = match select_effect(
            &mut shutdown,
            self.driver.effect_timeout,
            self.driver
                .operations
                .deploy_claim_candidates(&self.service_id),
        )
        .await
        {
            EffectResult::Completed(Ok(candidates)) => candidates,
            EffectResult::Completed(Err(error)) => return Err(DeployDriverError::Operation(error)),
            EffectResult::Shutdown | EffectResult::TimedOut => return self.interrupted().await,
        };
        match adjudicate_deploy_claim(&self.operation_id, &candidates, adjudicated_at) {
            DeployClaim::Lost { winner } => {
                self.log
                    .append(
                        self.now()?,
                        OperationEvidence::OpClaimLost {
                            winner: winner.clone(),
                        },
                    )
                    .await?;
                return self.superseded(winner).await;
            }
            DeployClaim::Won => {
                self.log
                    .append(self.now()?, OperationEvidence::OpClaimWon)
                    .await?;
            }
        }
        let started_at = self.now()?;
        self.transition_row(CorrosionDeployTransition::Running { started_at })
            .await?;
        let heartbeat = DeployHeartbeat::spawn(self.driver.clone(), Arc::clone(&self.row));
        let end = self.run_phases(&mut shutdown, &heartbeat).await;
        heartbeat.stop().await;
        match end? {
            DeployTaskEnd::FinishFirstDeploy { prepared, warnings } => {
                self.driver
                    .finish_promotion(
                        &self.log,
                        PromotionFinalizerState::PromotionPrepared {
                            prepared: *prepared,
                        },
                        &self.row,
                        warnings,
                    )
                    .await
            }
            DeployTaskEnd::Completed { warnings } => self.completed(warnings).await,
            DeployTaskEnd::ServiceFailure { failure } => self.service_failure(failure).await,
            DeployTaskEnd::Failure { failure } => self.deploy_failure(failure).await,
            DeployTaskEnd::Interrupted => self.interrupted().await,
            DeployTaskEnd::StopSilently => Ok(()),
        }
    }

    async fn run_phases(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        heartbeat: &DeployHeartbeat,
    ) -> Result<DeployTaskEnd, DeployDriverError> {
        match &self.path {
            DeployPath::First { namespace } => self.run_first(shutdown, namespace).await,
            DeployPath::Redeploy {
                namespace,
                incumbent,
            } => {
                self.run_redeploy(shutdown, heartbeat, namespace, incumbent)
                    .await
            }
        }
    }

    async fn run_first(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        namespace: &ResolvedFirstDeployNamespace,
    ) -> Result<DeployTaskEnd, DeployDriverError> {
        let mut warnings = Vec::new();
        let image = match self.acquire_image(shutdown).await? {
            Ok(image) => image,
            Err(end) => return Ok(end),
        };
        let identity = self.identity(&namespace.id);
        let container_id = match self
            .create_container(shutdown, &image, namespace, identity.clone())
            .await?
        {
            Ok(container_id) => container_id,
            Err(end) => return Ok(end),
        };
        if let Err(end) = self.start_new_container(shutdown, &container_id).await? {
            return Ok(end);
        }
        let ip = match self
            .health_gate_or_skip(shutdown, &container_id, &identity, &mut warnings)
            .await?
        {
            Ok(ip) => ip,
            Err(end) => return Ok(end),
        };
        let prepared = self.prepared_promotion(namespace, container_id, ip, image)?;
        self.log
            .append_promotion_prepared(self.now()?, prepared.clone())
            .await?;
        Ok(DeployTaskEnd::FinishFirstDeploy {
            prepared: Box::new(prepared),
            warnings,
        })
    }

    async fn run_redeploy(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        heartbeat: &DeployHeartbeat,
        namespace: &ResolvedFirstDeployNamespace,
        incumbent: &ObservedService,
    ) -> Result<DeployTaskEnd, DeployDriverError> {
        let mut warnings = Vec::new();
        if let Some(end) = self.takeover_boundary(shutdown, heartbeat).await? {
            return Ok(end);
        }
        let containers = match select_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver
                .runtime
                .service_docker_containers(&self.service_id),
        )
        .await
        {
            EffectResult::Completed(Ok(containers)) => containers,
            EffectResult::Completed(Err(message)) => {
                return Ok(DeployTaskEnd::ServiceFailure {
                    failure: CorrosionDeployServiceFailure::ContainerCreateFailed {
                        message: bounded_diagnostic(format!(
                            "could not list service containers: {message}"
                        )),
                    },
                });
            }
            EffectResult::Shutdown | EffectResult::TimedOut => {
                return Ok(DeployTaskEnd::Interrupted);
            }
        };
        let (incumbents, debris): (Vec<_>, Vec<_>) =
            containers.into_iter().partition(|container| {
                container.identity.operation_id == incumbent.document.active_deploy
            });
        let debris: Vec<ExistingV2ManagedContainer> = debris
            .into_iter()
            .filter(|container| container.identity.operation_id != self.operation_id)
            .collect();
        let volume_service = incumbents
            .iter()
            .any(|container| !container.named_volume_names.is_empty())
            || !self.request.runtime.volume_mounts.is_empty();

        // Sweep is best-effort: debris that refuses to die stays for the next
        // deploy's sweep and never fails this operation.
        let mut removed = Vec::new();
        for container in &debris {
            let stopped = bounded(
                self.driver.effect_timeout,
                self.driver
                    .runtime
                    .stop_container(&container.container_id, &container.identity),
            )
            .await;
            if !matches!(stopped, Ok(Ok(_))) {
                continue;
            }
            let removal = bounded(
                self.driver.effect_timeout,
                self.driver
                    .runtime
                    .remove_container(&container.container_id, &container.identity),
            )
            .await;
            if matches!(removal, Ok(Ok(()))) {
                removed.push(container.container_id.clone());
            }
        }
        if !removed.is_empty() {
            self.delete_container_rows(&removed).await;
            self.log
                .append(self.now()?, OperationEvidence::DebrisSwept { removed })
                .await?;
        }

        // The replacement is pulled and created before the incumbent is touched.
        let image = match self.acquire_image(shutdown).await? {
            Ok(image) => image,
            Err(end) => return Ok(end),
        };
        let identity = self.identity(&namespace.id);
        let container_id = match self
            .create_container(shutdown, &image, namespace, identity.clone())
            .await?
        {
            Ok(container_id) => container_id,
            Err(end) => return Ok(end),
        };

        let ip = if volume_service {
            if let Some(end) = self.takeover_boundary(shutdown, heartbeat).await? {
                return Ok(end);
            }
            // Stop-first cutover: the incumbent holds the named volumes.
            for container in &incumbents {
                match select_effect(
                    shutdown,
                    self.driver.effect_timeout,
                    self.driver
                        .runtime
                        .stop_container(&container.container_id, &container.identity),
                )
                .await
                {
                    EffectResult::Completed(Ok(_)) => {
                        self.log
                            .append(
                                self.now()?,
                                OperationEvidence::IncumbentStopped {
                                    container_id: container.container_id.clone(),
                                },
                            )
                            .await?;
                    }
                    EffectResult::Completed(Err(message)) => {
                        return Ok(DeployTaskEnd::ServiceFailure {
                            failure: CorrosionDeployServiceFailure::IncumbentStopFailed {
                                message: bounded_diagnostic(message),
                            },
                        });
                    }
                    EffectResult::TimedOut => {
                        return Ok(DeployTaskEnd::ServiceFailure {
                            failure: CorrosionDeployServiceFailure::IncumbentStopFailed {
                                message: "incumbent stop timed out".to_owned(),
                            },
                        });
                    }
                    EffectResult::Shutdown => return Ok(DeployTaskEnd::Interrupted),
                }
            }
            match self.start_new_container(shutdown, &container_id).await? {
                Ok(()) => {}
                Err(end) => {
                    self.restart_incumbents(&incumbents).await?;
                    return Ok(end);
                }
            }
            match self
                .health_gate_or_skip(shutdown, &container_id, &identity, &mut warnings)
                .await?
            {
                Ok(ip) => ip,
                Err(end) => {
                    // The failed replacement is retained for inspection; only
                    // the incumbent is brought back.
                    if matches!(end, DeployTaskEnd::ServiceFailure { .. }) {
                        self.restart_incumbents(&incumbents).await?;
                    }
                    return Ok(end);
                }
            }
        } else {
            if let Err(end) = self.start_new_container(shutdown, &container_id).await? {
                return Ok(end);
            }
            match self
                .health_gate_or_skip(shutdown, &container_id, &identity, &mut warnings)
                .await?
            {
                Ok(ip) => ip,
                Err(end) => return Ok(end),
            }
        };

        // The takeover check always runs immediately before the flip.
        if let Some(end) = self.takeover_boundary(shutdown, heartbeat).await? {
            return Ok(end);
        }
        let intent = self.prepared_redeploy_intent(incumbent, container_id, ip, image)?;
        self.log
            .append_redeploy_prepared(self.now()?, intent.clone())
            .await?;
        match self
            .driver
            .converge_redeploy(&intent, &self.operation_id, &self.service_id)
            .await?
        {
            RedeployFlipEnd::Committed => {
                self.log
                    .append(self.now()?, OperationEvidence::RowsCommitted)
                    .await?;
            }
            RedeployFlipEnd::Superseded { winner } => {
                return Ok(DeployTaskEnd::Failure {
                    failure: CorrosionDeployFailure::SupersededByOperation { winner },
                });
            }
            RedeployFlipEnd::Failure { failure } => {
                return Ok(DeployTaskEnd::Failure {
                    failure: CorrosionDeployFailure::Promotion {
                        service_id: self.service_id.clone(),
                        failure,
                    },
                });
            }
        }

        // Post-flip: the flip is committed, so nothing below fails the
        // operation; leftovers are the next deploy's sweep.
        if volume_service {
            for container in &incumbents {
                let removal = bounded(
                    self.driver.effect_timeout,
                    self.driver
                        .runtime
                        .remove_container(&container.container_id, &container.identity),
                )
                .await;
                if matches!(removal, Ok(Ok(()))) {
                    self.log
                        .append(
                            self.now()?,
                            OperationEvidence::IncumbentRemoved {
                                container_id: container.container_id.clone(),
                            },
                        )
                        .await?;
                }
            }
        } else {
            tokio::select! {
                _ = shutdown.changed() => return Ok(DeployTaskEnd::Completed { warnings }),
                () = tokio::time::sleep(self.driver.drain_wait) => {}
            }
            self.log
                .append(self.now()?, OperationEvidence::Drained)
                .await?;
            for container in &incumbents {
                let stopped = bounded(
                    self.driver.effect_timeout,
                    self.driver
                        .runtime
                        .stop_container(&container.container_id, &container.identity),
                )
                .await;
                if !matches!(stopped, Ok(Ok(_))) {
                    continue;
                }
                self.log
                    .append(
                        self.now()?,
                        OperationEvidence::IncumbentStopped {
                            container_id: container.container_id.clone(),
                        },
                    )
                    .await?;
                let removal = bounded(
                    self.driver.effect_timeout,
                    self.driver
                        .runtime
                        .remove_container(&container.container_id, &container.identity),
                )
                .await;
                if matches!(removal, Ok(Ok(()))) {
                    self.log
                        .append(
                            self.now()?,
                            OperationEvidence::IncumbentRemoved {
                                container_id: container.container_id.clone(),
                            },
                        )
                        .await?;
                }
            }
        }
        let old_ids: Vec<ContainerId> = incumbents
            .iter()
            .map(|container| container.container_id.clone())
            .collect();
        self.delete_container_rows(&old_ids).await;
        Ok(DeployTaskEnd::Completed { warnings })
    }

    /// A cheap pre-phase check: a live newer op owns the service now, and a
    /// terminal own row means some earlier write already settled this op.
    async fn takeover_boundary(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        heartbeat: &DeployHeartbeat,
    ) -> Result<Option<DeployTaskEnd>, DeployDriverError> {
        if heartbeat.superseded() {
            return Ok(Some(DeployTaskEnd::StopSilently));
        }
        let newer = match select_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver
                .operations
                .deploy_takeover_candidates(&self.operation_id, &self.service_id),
        )
        .await
        {
            EffectResult::Completed(Ok(newer)) => newer,
            EffectResult::Completed(Err(error)) => return Err(DeployDriverError::Operation(error)),
            EffectResult::Shutdown | EffectResult::TimedOut => {
                return Ok(Some(DeployTaskEnd::Interrupted));
            }
        };
        if let DeployTakeover::TakenOver { winner } =
            check_deploy_takeover(&self.operation_id, &newer)
        {
            self.log
                .append(
                    self.now()?,
                    OperationEvidence::OpClaimLost {
                        winner: winner.clone(),
                    },
                )
                .await?;
            return Ok(Some(DeployTaskEnd::Failure {
                failure: CorrosionDeployFailure::SupersededByOperation { winner },
            }));
        }
        match bounded(
            self.driver.effect_timeout,
            self.driver.operations.operation(&self.operation_id),
        )
        .await
        {
            Ok(Ok(Some(current))) => {
                if current.document.is_terminal() {
                    return Ok(Some(DeployTaskEnd::StopSilently));
                }
                *self.row.lock().await = current;
                Ok(None)
            }
            Ok(Ok(None)) => Ok(Some(DeployTaskEnd::StopSilently)),
            // A transient read failure never blocks a boundary; the flip CAS
            // remains the hard gate.
            Ok(Err(_)) | Err(_) => Ok(None),
        }
    }

    async fn acquire_image(
        &self,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<Result<ImageReference, DeployTaskEnd>, DeployDriverError> {
        self.log
            .append(self.now()?, OperationEvidence::PullingImage)
            .await?;
        let image = match select_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.resolve_image(&self.request.image),
        )
        .await
        {
            EffectResult::Completed(Ok(image)) => image,
            EffectResult::Completed(Err(message)) => {
                return Ok(Err(DeployTaskEnd::ServiceFailure {
                    failure: CorrosionDeployServiceFailure::ImagePullFailed { message },
                }));
            }
            EffectResult::Shutdown | EffectResult::TimedOut => {
                return Ok(Err(DeployTaskEnd::Interrupted));
            }
        };
        let pull_shutdown = shutdown.clone();
        match select_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.pull_image(&image, pull_shutdown),
        )
        .await
        {
            EffectResult::Completed(Ok(())) => {}
            EffectResult::Completed(Err(message)) => {
                return Ok(Err(DeployTaskEnd::ServiceFailure {
                    failure: CorrosionDeployServiceFailure::ImagePullFailed { message },
                }));
            }
            EffectResult::TimedOut => {
                return Ok(Err(DeployTaskEnd::ServiceFailure {
                    failure: CorrosionDeployServiceFailure::ImagePullFailed {
                        message: "image pull timed out".to_owned(),
                    },
                }));
            }
            EffectResult::Shutdown => return Ok(Err(DeployTaskEnd::Interrupted)),
        }
        self.log
            .append(self.now()?, OperationEvidence::ImageResolved)
            .await?;
        Ok(Ok(image))
    }

    async fn create_container(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        image: &ImageReference,
        namespace: &ResolvedFirstDeployNamespace,
        identity: V2ManagedContainerIdentity,
    ) -> Result<Result<ContainerId, DeployTaskEnd>, DeployDriverError> {
        let created = select_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver
                .runtime
                .create_container(&self.request, image, namespace, identity),
        )
        .await;
        let container_id = match created {
            EffectResult::Completed(Ok(container_id)) => container_id,
            EffectResult::Completed(Err(message)) => {
                return Ok(Err(DeployTaskEnd::ServiceFailure {
                    failure: CorrosionDeployServiceFailure::ContainerCreateFailed { message },
                }));
            }
            EffectResult::Shutdown | EffectResult::TimedOut => {
                return Ok(Err(DeployTaskEnd::Interrupted));
            }
        };
        self.log
            .append(
                self.now()?,
                OperationEvidence::ContainerCreated {
                    container_id: container_id.clone(),
                },
            )
            .await?;
        Ok(Ok(container_id))
    }

    async fn start_new_container(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        container_id: &ContainerId,
    ) -> Result<Result<(), DeployTaskEnd>, DeployDriverError> {
        match select_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.start_container(container_id),
        )
        .await
        {
            EffectResult::Completed(Ok(())) => {}
            EffectResult::Completed(Err(message)) => {
                return Ok(Err(DeployTaskEnd::ServiceFailure {
                    failure: CorrosionDeployServiceFailure::ContainerStartFailed { message },
                }));
            }
            EffectResult::TimedOut => {
                return Ok(Err(DeployTaskEnd::ServiceFailure {
                    failure: CorrosionDeployServiceFailure::ContainerStartFailed {
                        message: "container start timed out".to_owned(),
                    },
                }));
            }
            EffectResult::Shutdown => return Ok(Err(DeployTaskEnd::Interrupted)),
        }
        self.log
            .append(
                self.now()?,
                OperationEvidence::ContainerStarted {
                    container_id: container_id.clone(),
                },
            )
            .await?;
        Ok(Ok(()))
    }

    async fn health_gate_or_skip(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
        warnings: &mut Vec<CorrosionDeployWarning>,
    ) -> Result<Result<Ipv4Addr, DeployTaskEnd>, DeployDriverError> {
        match self.request.health_gate {
            HealthGatePolicy::Enforce => {
                match select_effect(
                    shutdown,
                    self.driver.effect_timeout,
                    self.driver.runtime.health_gate(container_id, identity),
                )
                .await
                {
                    EffectResult::Completed(Ok(ip)) => Ok(Ok(ip)),
                    EffectResult::Completed(Err(message)) => {
                        Ok(Err(DeployTaskEnd::ServiceFailure {
                            failure: CorrosionDeployServiceFailure::HealthGateFailed { message },
                        }))
                    }
                    EffectResult::TimedOut => Ok(Err(DeployTaskEnd::ServiceFailure {
                        failure: CorrosionDeployServiceFailure::HealthGateFailed {
                            message: "health gate timed out".to_owned(),
                        },
                    })),
                    EffectResult::Shutdown => Ok(Err(DeployTaskEnd::Interrupted)),
                }
            }
            HealthGatePolicy::Skip => {
                match select_effect(
                    shutdown,
                    self.driver.effect_timeout,
                    self.driver.runtime.container_ip(container_id, identity),
                )
                .await
                {
                    EffectResult::Completed(Ok(ip)) => {
                        self.log
                            .append(self.now()?, OperationEvidence::HealthGateSkipped)
                            .await?;
                        warnings.push(CorrosionDeployWarning::HealthGateSkipped {
                            service_id: self.service_id.clone(),
                        });
                        Ok(Ok(ip))
                    }
                    EffectResult::Completed(Err(message)) => {
                        Ok(Err(DeployTaskEnd::ServiceFailure {
                            failure: CorrosionDeployServiceFailure::ContainerStartFailed {
                                message,
                            },
                        }))
                    }
                    EffectResult::TimedOut => Ok(Err(DeployTaskEnd::ServiceFailure {
                        failure: CorrosionDeployServiceFailure::ContainerStartFailed {
                            message: "container endpoint lookup timed out".to_owned(),
                        },
                    })),
                    EffectResult::Shutdown => Ok(Err(DeployTaskEnd::Interrupted)),
                }
            }
        }
    }

    async fn restart_incumbents(
        &self,
        incumbents: &[ExistingV2ManagedContainer],
    ) -> Result<(), DeployDriverError> {
        for container in incumbents {
            let restarted = bounded(
                self.driver.effect_timeout,
                self.driver.runtime.start_container(&container.container_id),
            )
            .await;
            if matches!(restarted, Ok(Ok(()))) {
                self.log
                    .append(
                        self.now()?,
                        OperationEvidence::IncumbentRestarted {
                            container_id: container.container_id.clone(),
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Deletes the container rows matching the given Docker containers, each
    /// exactly as observed. Best-effort: the next deploy's sweep retries.
    async fn delete_container_rows(&self, container_ids: &[ContainerId]) {
        if container_ids.is_empty() {
            return;
        }
        let rows = match bounded(
            self.driver.effect_timeout,
            self.driver.store.service_containers(&self.service_id),
        )
        .await
        {
            Ok(Ok(rows)) => rows,
            Ok(Err(_)) | Err(_) => return,
        };
        let matched: Vec<ObservedContainer> = rows
            .into_iter()
            .filter(|row| container_ids.contains(&row.id))
            .collect();
        if matched.is_empty() {
            return;
        }
        let _ = bounded(
            self.driver.effect_timeout,
            self.driver.store.delete_exact_container_rows(&matched),
        )
        .await;
    }

    pub(super) async fn recover_after_error(&self) -> Result<(), DeployDriverError> {
        let recovery = self.log.recovery_evidence().await?;
        if let Some(terminal) = recovery.terminal {
            let observed = self
                .driver
                .operations
                .operation(&self.operation_id)
                .await
                .map_err(DeployDriverError::Operation)?
                .ok_or_else(|| {
                    DeployDriverError::Operation("operation row disappeared".to_owned())
                })?;
            if observed.document == terminal {
                return Ok(());
            }
            return match self
                .driver
                .operations
                .replace_terminal(&observed, &terminal)
                .await
                .map_err(DeployDriverError::Operation)?
            {
                ConditionalOperationWrite::Written => Ok(()),
                ConditionalOperationWrite::Stale => Err(DeployDriverError::Operation(
                    "operation row changed before terminal".to_owned(),
                )),
            };
        }
        if let Some(progress) = recovery.promotion {
            return self
                .driver
                .resume_promotion(self.operation_id.clone(), self.log.clone(), progress)
                .await;
        }
        self.interrupted().await
    }

    fn identity(
        &self,
        namespace_id: &ployz_core::ids::NamespaceRowId,
    ) -> V2ManagedContainerIdentity {
        V2ManagedContainerIdentity {
            namespace_id: namespace_id.clone(),
            service_id: self.service_id.clone(),
            operation_id: self.operation_id.clone(),
        }
    }

    fn env_fingerprints(
        &self,
    ) -> Result<BTreeMap<String, ployz_core::corrosion::Sha256Hex>, DeployDriverError> {
        self.request
            .runtime
            .environment
            .iter()
            .map(|(name, value)| {
                fingerprint_env_value(value)
                    .map(|fingerprint| (name.as_str().to_owned(), fingerprint))
                    .map_err(|error| DeployDriverError::Invariant(error.to_string()))
            })
            .collect()
    }

    fn prepared_promotion(
        &self,
        namespace: &ResolvedFirstDeployNamespace,
        container_id: ContainerId,
        ip: Ipv4Addr,
        resolved_image: ImageReference,
    ) -> Result<PreparedPromotion, DeployDriverError> {
        let deployed_at = self.now()?;
        let service_document = ServiceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.driver.cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: self.initiator.clone(),
                written_at: deployed_at,
            },
            namespace_id: namespace.id.clone(),
            name: self.request.service_name.clone(),
            image: resolved_image,
            env_fingerprints: self.env_fingerprints()?,
            placement: ServicePlacement::Replicated {
                replicas: ServiceReplicaCount::try_new(1)
                    .map_err(|error| DeployDriverError::Invariant(error.to_string()))?,
            },
            pinned_machines: BTreeSet::from([self.driver.machine_id.clone()]),
            active_deploy: self.operation_id.clone(),
            previous_image: None,
            deployed_at,
            operation_id: self.operation_id.clone(),
        };
        let container_document = ContainerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.driver.cluster_id.clone(),
            machine_id: self.driver.machine_id.clone(),
            service_id: self.service_id.clone(),
            namespace_id: namespace.id.clone(),
            ip,
            deploy: self.operation_id.clone(),
        };
        Ok(PreparedPromotion {
            namespace_id: namespace.id.clone(),
            exact_namespace_document: namespace.exact_document.clone(),
            service_id: self.service_id.clone(),
            service_document,
            container_id,
            container_document,
            success_result: CorrosionDeployServiceResult::completed(self.service_id.clone()),
        })
    }

    fn prepared_redeploy_intent(
        &self,
        incumbent: &ObservedService,
        container_id: ContainerId,
        ip: Ipv4Addr,
        resolved_image: ImageReference,
    ) -> Result<PreparedRedeployIntent, DeployDriverError> {
        let deployed_at = self.now()?;
        let service_document = ServiceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.driver.cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: self.initiator.clone(),
                written_at: deployed_at,
            },
            namespace_id: incumbent.document.namespace_id.clone(),
            name: incumbent.document.name.clone(),
            image: resolved_image,
            env_fingerprints: self.env_fingerprints()?,
            placement: incumbent.document.placement,
            pinned_machines: incumbent.document.pinned_machines.clone(),
            active_deploy: self.operation_id.clone(),
            previous_image: Some(incumbent.document.image.clone()),
            deployed_at,
            operation_id: self.operation_id.clone(),
        };
        let container_document = ContainerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.driver.cluster_id.clone(),
            machine_id: self.driver.machine_id.clone(),
            service_id: self.service_id.clone(),
            namespace_id: incumbent.document.namespace_id.clone(),
            ip,
            deploy: self.operation_id.clone(),
        };
        Ok(PreparedRedeployIntent {
            service_id: self.service_id.clone(),
            exact_incumbent_document: incumbent.exact_document.clone(),
            service_document,
            container_id,
            container_document,
            health_gate: self.request.health_gate,
        })
    }

    async fn transition_row(
        &self,
        transition: CorrosionDeployTransition,
    ) -> Result<(), DeployDriverError> {
        let mut row = self.row.lock().await;
        let replacement = row
            .document
            .clone()
            .transition_deploy(transition.clone())
            .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
        match self
            .driver
            .operations
            .transition_deploy(clone_observed(&row), transition)
            .await
            .map_err(DeployDriverError::Operation)?
        {
            ConditionalOperationWrite::Written => {
                *row = observed_with(&row.id, replacement)?;
                Ok(())
            }
            ConditionalOperationWrite::Stale => Err(DeployDriverError::Operation(
                "operation row changed underneath its driver".to_owned(),
            )),
        }
    }

    async fn completed(
        &self,
        warnings: Vec<CorrosionDeployWarning>,
    ) -> Result<(), DeployDriverError> {
        let results = vec![CorrosionDeployServiceResult::completed(
            self.service_id.clone(),
        )];
        let outcome = if warnings.is_empty() {
            CorrosionDeployOutcome::completed(results)
        } else {
            CorrosionDeployOutcome::completed_with_warnings(results, warnings)
        }
        .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
        self.driver.terminalize(&self.log, &self.row, outcome).await
    }

    async fn superseded(&self, winner: OperationRowId) -> Result<(), DeployDriverError> {
        self.deploy_failure(CorrosionDeployFailure::SupersededByOperation { winner })
            .await
    }

    async fn deploy_failure(
        &self,
        failure: CorrosionDeployFailure,
    ) -> Result<(), DeployDriverError> {
        let outcome = CorrosionDeployOutcome::failed(
            vec![CorrosionDeployServiceResult::skipped(
                self.service_id.clone(),
            )],
            failure,
        )
        .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
        self.driver.terminalize(&self.log, &self.row, outcome).await
    }

    async fn service_failure(
        &self,
        failure: CorrosionDeployServiceFailure,
    ) -> Result<(), DeployDriverError> {
        let failure = bound_service_failure(failure);
        let outcome = CorrosionDeployOutcome::failed(
            vec![CorrosionDeployServiceResult::failed(
                self.service_id.clone(),
                failure.clone(),
            )],
            CorrosionDeployFailure::ServiceFailed {
                service_id: self.service_id.clone(),
                failure,
            },
        )
        .map_err(|error| DeployDriverError::Invariant(error.to_string()))?;
        self.driver.terminalize(&self.log, &self.row, outcome).await
    }

    async fn interrupted(&self) -> Result<(), DeployDriverError> {
        self.deploy_failure(CorrosionDeployFailure::Interrupted)
            .await
    }

    fn now(&self) -> Result<CorrosionTimestamp, DeployDriverError> {
        self.driver.clock.now().map_err(DeployDriverError::Clock)
    }
}

fn bound_service_failure(failure: CorrosionDeployServiceFailure) -> CorrosionDeployServiceFailure {
    match failure {
        CorrosionDeployServiceFailure::ImagePullFailed { message } => {
            CorrosionDeployServiceFailure::ImagePullFailed {
                message: bounded_diagnostic(message),
            }
        }
        CorrosionDeployServiceFailure::ContainerCreateFailed { message } => {
            CorrosionDeployServiceFailure::ContainerCreateFailed {
                message: bounded_diagnostic(message),
            }
        }
        CorrosionDeployServiceFailure::ContainerStartFailed { message } => {
            CorrosionDeployServiceFailure::ContainerStartFailed {
                message: bounded_diagnostic(message),
            }
        }
        CorrosionDeployServiceFailure::IncumbentStopFailed { message } => {
            CorrosionDeployServiceFailure::IncumbentStopFailed {
                message: bounded_diagnostic(message),
            }
        }
        CorrosionDeployServiceFailure::HealthGateFailed { message } => {
            CorrosionDeployServiceFailure::HealthGateFailed {
                message: bounded_diagnostic(message),
            }
        }
    }
}

fn bounded_diagnostic(mut message: String) -> String {
    if message.len() <= MAX_PERSISTED_DIAGNOSTIC_BYTES {
        return message;
    }
    let boundary = message
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX_PERSISTED_DIAGNOSTIC_BYTES)
        .last()
        .unwrap_or(0);
    message.truncate(boundary);
    message
}

enum EffectResult<T> {
    Completed(T),
    Shutdown,
    TimedOut,
}

async fn select_effect<Future, Output>(
    shutdown: &mut watch::Receiver<bool>,
    timeout: Duration,
    future: Future,
) -> EffectResult<Output>
where
    Future: std::future::Future<Output = Output>,
{
    if shutdown_requested(shutdown) {
        return EffectResult::Shutdown;
    }
    tokio::select! {
        _ = shutdown.changed() => EffectResult::Shutdown,
        outcome = tokio::time::timeout(timeout, future) => match outcome {
            Ok(output) => EffectResult::Completed(output),
            Err(_) => EffectResult::TimedOut,
        },
    }
}

async fn bounded<Future, Output>(
    timeout: Duration,
    future: Future,
) -> Result<Output, tokio::time::error::Elapsed>
where
    Future: std::future::Future<Output = Output>,
{
    tokio::time::timeout(timeout, future).await
}

fn shutdown_requested(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

#[derive(Debug, thiserror::Error)]
pub(super) enum DeployDriverError {
    #[error("operation persistence failed: {0}")]
    Operation(String),
    #[error("operation evidence failed: {0}")]
    Evidence(#[from] super::operation_evidence::OperationEvidenceError),
    #[error("promotion storage failed: {0}")]
    Promotion(#[from] PromotionFinalizerStoreError),
    #[error("timestamp creation failed: {0}")]
    Clock(String),
    #[error("deploy invariant failed: {0}")]
    Invariant(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use ployz_core::corrosion::{
        ContainerDocument, CorrosionDeployOutcome, CorrosionDeployState, CorrosionDeployWarning,
        CorrosionNamespaceName, CorrosionServiceName, CorrosionTimestamp, NamespaceDocument,
        OperationDocument, Principal, ServiceDocument, V2ManagedContainerIdentity,
    };
    use ployz_core::deploy::{ContainerRuntimeSpec, EnvName, EnvValue, ImageReference, VolumeName};
    use ployz_core::ids::{
        ClusterId, ContainerId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, ServiceRowId,
    };
    use ployz_core::machine::runtime::ContainerHealth;
    use ployz_core::{DeployRefusal, HealthGatePolicy, OperationEvidence};
    use tempfile::TempDir;
    use tokio::sync::{Mutex, watch};

    use super::{
        DEPLOY_DRAIN_WAIT, DNS_TTL_SECONDS, DeployClock, DeployDriver, DeployHeartbeat,
        DeployOperationRows, DeployRuntime, HEALTH_POLL_INTERVAL, HealthGateObservation,
        MAX_PERSISTED_DIAGNOSTIC_BYTES, RUNNING_CONFIRMATION_WINDOW, RedeployStore,
        bounded_diagnostic, classify_health_observation, observed_with,
    };
    use crate::roles::api::http::operation_evidence::{
        OperationEvidenceDirectory, PreparedPromotion, PreparedRedeployIntent,
    };
    use crate::roles::api::http::operation_finalizer::{
        PreparedPromotionStore, PromotionClaimOutcome, PromotionFinalizerStoreError,
        PromotionRequestDisposition, PromotionRowsObservation,
    };
    use crate::roles::api::http::operation_store::{
        ConditionalOperationWrite, HeartbeatWrite, ObservedOperation,
    };
    use crate::roles::api::http::promotion_store::{
        ContainerRowCleanup, DeployAdmission, ObservedContainer, ObservedService,
        ResolvedFirstDeployNamespace,
    };
    use crate::roles::api::runner::{
        ExistingManagedContainerState, ExistingV2ManagedContainer, MachineContainerStopOutcome,
    };

    #[derive(Clone, Copy)]
    enum Admission {
        Missing,
        Ambiguous,
        DifferentService,
        MultipleServices,
        RoutesWithoutServices,
        First,
        Redeploy,
        RedeployElsewhere,
    }

    struct FakeStore {
        admission: Admission,
        prepared: Mutex<Option<PreparedPromotion>>,
        fail_adjudication: AtomicBool,
        adjudication_attempts: AtomicUsize,
        claim_lost: AtomicBool,
        cleanup_attempts: AtomicUsize,
        redeploy_prepared: Mutex<Option<PreparedRedeployIntent>>,
        converge_calls: AtomicUsize,
        rows: Mutex<Vec<ObservedContainer>>,
        deleted: Mutex<Vec<Vec<ContainerId>>>,
    }

    fn cloned_row(row: &ObservedContainer) -> ObservedContainer {
        ObservedContainer {
            id: row.id.clone(),
            exact_document: row.exact_document.clone(),
            document: row.document.clone(),
        }
    }

    #[async_trait]
    impl RedeployStore for FakeStore {
        async fn resolve_deploy_admission(
            &self,
            _namespace_name: &CorrosionNamespaceName,
            _service_name: &CorrosionServiceName,
        ) -> Result<DeployAdmission, PromotionFinalizerStoreError> {
            Ok(match self.admission {
                Admission::Missing => DeployAdmission::NamespaceMissing,
                Admission::Ambiguous => DeployAdmission::NamespaceAmbiguous {
                    namespace_ids: vec![namespace_id()],
                },
                Admission::DifferentService => DeployAdmission::DifferentService {
                    namespace_id: namespace_id(),
                    incumbent_name: CorrosionServiceName::try_new("web").expect("name"),
                },
                Admission::MultipleServices => DeployAdmission::MultipleServices {
                    namespace_id: namespace_id(),
                    service_ids: vec![service_id()],
                },
                Admission::RoutesWithoutServices => DeployAdmission::RoutesWithoutServices {
                    namespace_id: namespace_id(),
                },
                Admission::First => DeployAdmission::FirstDeploy {
                    namespace: resolved_namespace(),
                },
                Admission::Redeploy => DeployAdmission::Redeploy {
                    namespace: resolved_namespace(),
                    incumbent: Box::new(incumbent_service(machine_id())),
                },
                Admission::RedeployElsewhere => DeployAdmission::Redeploy {
                    namespace: resolved_namespace(),
                    incumbent: Box::new(incumbent_service(other_machine_id())),
                },
            })
        }

        async fn converge_redeploy_rows(
            &self,
            prepared: &PreparedRedeployIntent,
        ) -> Result<
            (PromotionRequestDisposition, PromotionRowsObservation),
            PromotionFinalizerStoreError,
        > {
            self.converge_calls.fetch_add(1, Ordering::SeqCst);
            *self.redeploy_prepared.lock().await = Some(prepared.clone());
            Ok((
                PromotionRequestDisposition::Accepted,
                PromotionRowsObservation::EXACT,
            ))
        }

        async fn service_containers(
            &self,
            _service_id: &ServiceRowId,
        ) -> Result<Vec<ObservedContainer>, PromotionFinalizerStoreError> {
            Ok(self.rows.lock().await.iter().map(cloned_row).collect())
        }

        async fn delete_exact_container_rows(
            &self,
            rows: &[ObservedContainer],
        ) -> Result<Vec<(ContainerId, ContainerRowCleanup)>, PromotionFinalizerStoreError> {
            self.deleted
                .lock()
                .await
                .push(rows.iter().map(|row| row.id.clone()).collect());
            Ok(rows
                .iter()
                .map(|row| (row.id.clone(), ContainerRowCleanup::Removed))
                .collect())
        }
    }

    #[async_trait]
    impl PreparedPromotionStore for FakeStore {
        async fn converge_rows(
            &self,
            prepared: &PreparedPromotion,
        ) -> Result<
            (PromotionRequestDisposition, PromotionRowsObservation),
            PromotionFinalizerStoreError,
        > {
            *self.prepared.lock().await = Some(prepared.clone());
            Ok((
                PromotionRequestDisposition::Accepted,
                PromotionRowsObservation::EXACT,
            ))
        }

        async fn adjudicate_service_claim(
            &self,
            _prepared: &PreparedPromotion,
        ) -> Result<PromotionClaimOutcome, PromotionFinalizerStoreError> {
            self.adjudication_attempts.fetch_add(1, Ordering::SeqCst);
            if self.fail_adjudication.load(Ordering::SeqCst) {
                return Err(PromotionFinalizerStoreError::Transport(
                    "test outage".to_owned(),
                ));
            }
            if self.claim_lost.load(Ordering::SeqCst) {
                return Ok(PromotionClaimOutcome::Lost {
                    winner: ServiceRowId::try_new("01J00000000000000000000019").expect("winner"),
                });
            }
            Ok(PromotionClaimOutcome::Won)
        }

        async fn delete_exact_losing_rows(
            &self,
            _prepared: &PreparedPromotion,
        ) -> Result<PromotionRowsObservation, PromotionFinalizerStoreError> {
            self.cleanup_attempts.fetch_add(1, Ordering::SeqCst);
            Ok(PromotionRowsObservation::EXACT)
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RowWrite {
        Created,
        Transition,
        Heartbeat,
    }

    struct FakeOperations {
        operation: Mutex<Option<OperationDocument>>,
        writes: Mutex<Vec<RowWrite>>,
        claim_candidates: Mutex<Vec<(OperationRowId, OperationDocument)>>,
        takeover_candidates: Mutex<Vec<(OperationRowId, OperationDocument)>>,
        takeover_visible_after: AtomicUsize,
        takeover_calls: AtomicUsize,
        stale_heartbeat: AtomicBool,
    }

    impl FakeOperations {
        fn new() -> Self {
            Self {
                operation: Mutex::new(None),
                writes: Mutex::new(Vec::new()),
                claim_candidates: Mutex::new(Vec::new()),
                takeover_candidates: Mutex::new(Vec::new()),
                takeover_visible_after: AtomicUsize::new(0),
                takeover_calls: AtomicUsize::new(0),
                stale_heartbeat: AtomicBool::new(false),
            }
        }

        async fn terminal_state(&self) -> CorrosionDeployState {
            let operation = self.operation.lock().await.clone().expect("operation");
            operation.deploy_state().expect("deploy state").clone()
        }
    }

    fn cloned_candidates(
        candidates: &[(OperationRowId, OperationDocument)],
    ) -> Vec<(OperationRowId, OperationDocument)> {
        candidates
            .iter()
            .map(|(id, document)| (id.clone(), document.clone()))
            .collect()
    }

    #[async_trait]
    impl DeployOperationRows for FakeOperations {
        async fn insert_created(
            &self,
            _operation_id: &OperationRowId,
            operation: &OperationDocument,
        ) -> Result<(), String> {
            *self.operation.lock().await = Some(operation.clone());
            self.writes.lock().await.push(RowWrite::Created);
            Ok(())
        }

        async fn operation(
            &self,
            operation_id: &OperationRowId,
        ) -> Result<Option<ObservedOperation>, String> {
            let Some(document) = self.operation.lock().await.clone() else {
                return Ok(None);
            };
            observed_with(operation_id, document)
                .map(Some)
                .map_err(|error| error.to_string())
        }

        async fn transition_deploy(
            &self,
            _observed: ObservedOperation,
            transition: ployz_core::corrosion::CorrosionDeployTransition,
        ) -> Result<ConditionalOperationWrite, String> {
            let mut operation = self.operation.lock().await;
            let current = operation
                .take()
                .ok_or_else(|| "missing operation".to_owned())?;
            *operation = Some(
                current
                    .transition_deploy(transition)
                    .map_err(|error| error.to_string())?,
            );
            self.writes.lock().await.push(RowWrite::Transition);
            Ok(ConditionalOperationWrite::Written)
        }

        async fn replace_terminal(
            &self,
            _observed: &ObservedOperation,
            terminal: &OperationDocument,
        ) -> Result<ConditionalOperationWrite, String> {
            *self.operation.lock().await = Some(terminal.clone());
            self.writes.lock().await.push(RowWrite::Transition);
            Ok(ConditionalOperationWrite::Written)
        }

        async fn refresh_heartbeat(
            &self,
            observed: &ObservedOperation,
            now: CorrosionTimestamp,
        ) -> Result<HeartbeatWrite, String> {
            if self.stale_heartbeat.load(Ordering::SeqCst) {
                return Ok(HeartbeatWrite::Stale);
            }
            let refreshed = observed
                .document
                .clone()
                .refresh_heartbeat(now)
                .map_err(|error| error.to_string())?;
            *self.operation.lock().await = Some(refreshed.clone());
            self.writes.lock().await.push(RowWrite::Heartbeat);
            observed_with(&observed.id, refreshed)
                .map(|observed| HeartbeatWrite::Written(Box::new(observed)))
                .map_err(|error| error.to_string())
        }

        async fn deploy_claim_candidates(
            &self,
            _service_id: &ServiceRowId,
        ) -> Result<Vec<(OperationRowId, OperationDocument)>, String> {
            Ok(cloned_candidates(&self.claim_candidates.lock().await))
        }

        async fn deploy_takeover_candidates(
            &self,
            _operation_id: &OperationRowId,
            _service_id: &ServiceRowId,
        ) -> Result<Vec<(OperationRowId, OperationDocument)>, String> {
            let calls = self.takeover_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if calls <= self.takeover_visible_after.load(Ordering::SeqCst) {
                return Ok(Vec::new());
            }
            Ok(cloned_candidates(&self.takeover_candidates.lock().await))
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RuntimeCall {
        Create,
        Start(String),
        Stop(String),
        Remove(String),
    }

    struct FakeRuntime {
        bridge_ready: AtomicBool,
        bridge_reads: AtomicUsize,
        containers: Mutex<Vec<ExistingV2ManagedContainer>>,
        calls: Mutex<Vec<RuntimeCall>>,
        health_failure: AtomicBool,
    }

    impl FakeRuntime {
        async fn call_log(&self) -> Vec<RuntimeCall> {
            self.calls.lock().await.clone()
        }

        async fn created(&self) -> usize {
            self.calls
                .lock()
                .await
                .iter()
                .filter(|call| matches!(call, RuntimeCall::Create))
                .count()
        }
    }

    #[async_trait]
    impl DeployRuntime for FakeRuntime {
        async fn bridge_ready(&self) -> bool {
            self.bridge_reads.fetch_add(1, Ordering::SeqCst);
            self.bridge_ready.load(Ordering::SeqCst)
        }

        async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String> {
            image
                .with_digest(
                    &ployz_core::image::OciDigest::try_new(format!("sha256:{}", "c".repeat(64)))
                        .expect("digest"),
                )
                .map_err(|error| error.to_string())
        }

        async fn pull_image(
            &self,
            _image: &ImageReference,
            _shutdown: watch::Receiver<bool>,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn create_container(
            &self,
            _request: &ployz_core::DeployRequest,
            _resolved_image: &ImageReference,
            _namespace: &ResolvedFirstDeployNamespace,
            _identity: V2ManagedContainerIdentity,
        ) -> Result<ContainerId, String> {
            self.calls.lock().await.push(RuntimeCall::Create);
            ContainerId::try_new("new-container").map_err(|error| error.to_string())
        }

        async fn start_container(&self, container_id: &ContainerId) -> Result<(), String> {
            self.calls
                .lock()
                .await
                .push(RuntimeCall::Start(container_id.as_str().to_owned()));
            Ok(())
        }

        async fn health_gate(
            &self,
            _container_id: &ContainerId,
            _identity: &V2ManagedContainerIdentity,
        ) -> Result<Ipv4Addr, String> {
            if self.health_failure.load(Ordering::SeqCst) {
                return Err("unhealthy".to_owned());
            }
            Ok(Ipv4Addr::new(10, 210, 20, 2))
        }

        async fn container_ip(
            &self,
            _container_id: &ContainerId,
            _identity: &V2ManagedContainerIdentity,
        ) -> Result<Ipv4Addr, String> {
            Ok(Ipv4Addr::new(10, 210, 20, 2))
        }

        async fn service_docker_containers(
            &self,
            _service_id: &ServiceRowId,
        ) -> Result<Vec<ExistingV2ManagedContainer>, String> {
            Ok(self.containers.lock().await.clone())
        }

        async fn stop_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &V2ManagedContainerIdentity,
        ) -> Result<MachineContainerStopOutcome, String> {
            self.calls
                .lock()
                .await
                .push(RuntimeCall::Stop(container_id.as_str().to_owned()));
            Ok(MachineContainerStopOutcome::StoppedRunning)
        }

        async fn remove_container(
            &self,
            container_id: &ContainerId,
            _expected_identity: &V2ManagedContainerIdentity,
        ) -> Result<(), String> {
            self.calls
                .lock()
                .await
                .push(RuntimeCall::Remove(container_id.as_str().to_owned()));
            Ok(())
        }
    }

    struct Clock(AtomicUsize);

    impl DeployClock for Clock {
        fn now(&self) -> Result<CorrosionTimestamp, String> {
            let tick = self.0.fetch_add(1, Ordering::SeqCst);
            let minute = tick / 60;
            let second = tick % 60;
            CorrosionTimestamp::try_new(format!("2026-08-05T10:{minute:02}:{second:02}Z"))
                .map_err(|error| error.to_string())
        }
    }

    struct Fixture {
        _root: TempDir,
        driver: DeployDriver,
        store: Arc<FakeStore>,
        operations: Arc<FakeOperations>,
        runtime: Arc<FakeRuntime>,
    }

    fn fixture(admission: Admission, bridge_ready: bool) -> Fixture {
        let root = tempfile::tempdir().expect("evidence root");
        let store = Arc::new(FakeStore {
            admission,
            prepared: Mutex::new(None),
            fail_adjudication: AtomicBool::new(false),
            adjudication_attempts: AtomicUsize::new(0),
            claim_lost: AtomicBool::new(false),
            cleanup_attempts: AtomicUsize::new(0),
            redeploy_prepared: Mutex::new(None),
            converge_calls: AtomicUsize::new(0),
            rows: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
        });
        let operations = Arc::new(FakeOperations::new());
        let runtime = Arc::new(FakeRuntime {
            bridge_ready: AtomicBool::new(bridge_ready),
            bridge_reads: AtomicUsize::new(0),
            containers: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            health_failure: AtomicBool::new(false),
        });
        let driver = DeployDriver::new(
            cluster_id(),
            machine_id(),
            OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024),
            store.clone(),
            operations.clone(),
            runtime.clone(),
            Arc::new(Clock(AtomicUsize::new(0))),
        )
        .with_waits(Duration::ZERO, Duration::ZERO);
        Fixture {
            _root: root,
            driver,
            store,
            operations,
            runtime,
        }
    }

    fn cluster_id() -> ClusterId {
        ClusterId::try_new("01J00000000000000000000010").expect("cluster")
    }

    fn machine_id() -> MachineRowId {
        MachineRowId::try_new("01J00000000000000000000012").expect("machine")
    }

    fn other_machine_id() -> MachineRowId {
        MachineRowId::try_new("01J00000000000000000000016").expect("machine")
    }

    fn namespace_id() -> NamespaceRowId {
        NamespaceRowId::try_new("01J00000000000000000000013").expect("namespace")
    }

    fn service_id() -> ServiceRowId {
        ServiceRowId::try_new("01J00000000000000000000014").expect("service")
    }

    fn incumbent_op() -> OperationRowId {
        OperationRowId::try_new("01J00000000000000000000008").expect("operation")
    }

    fn debris_op() -> OperationRowId {
        OperationRowId::try_new("01J00000000000000000000007").expect("operation")
    }

    fn resolved_namespace() -> ResolvedFirstDeployNamespace {
        let document: NamespaceDocument = serde_json::from_value(serde_json::json!({
            "v": 1,
            "cluster_id": "01J00000000000000000000010",
            "written_by": { "kind": "peer", "peer_id": "01J00000000000000000000015" },
            "written_at": "2026-08-05T09:00:00Z",
            "name": "production"
        }))
        .expect("namespace");
        ResolvedFirstDeployNamespace {
            id: namespace_id(),
            exact_document: serde_json::to_string(&document).expect("namespace json"),
            document,
        }
    }

    fn incumbent_document(pinned: MachineRowId) -> ServiceDocument {
        serde_json::from_value(serde_json::json!({
            "v": 1,
            "cluster_id": "01J00000000000000000000010",
            "written_by": { "kind": "peer", "peer_id": "01J00000000000000000000015" },
            "written_at": "2026-08-05T09:00:00Z",
            "namespace_id": namespace_id(),
            "name": "api",
            "image": "ghcr.io/acme/api@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "env_fingerprints": {},
            "mode": "replicated",
            "replicas": 1,
            "pinned_machines": [pinned],
            "active_deploy": incumbent_op(),
            "previous_image": null,
            "deployed_at": "2026-08-05T09:00:02Z",
            "operation_id": incumbent_op()
        }))
        .expect("incumbent document")
    }

    fn incumbent_service(pinned: MachineRowId) -> ObservedService {
        let document = incumbent_document(pinned);
        ObservedService {
            id: service_id(),
            exact_document: serde_json::to_string(&document).expect("incumbent json"),
            document,
        }
    }

    fn docker_container(
        container_id: &str,
        operation_id: OperationRowId,
        volumes: &[&str],
    ) -> ExistingV2ManagedContainer {
        ExistingV2ManagedContainer {
            container_id: ContainerId::try_new(container_id).expect("container id"),
            identity: V2ManagedContainerIdentity {
                namespace_id: namespace_id(),
                service_id: service_id(),
                operation_id,
            },
            state: ExistingManagedContainerState::Running {
                ip: Some(IpAddr::V4(Ipv4Addr::new(10, 210, 20, 9))),
                health: ContainerHealth::None,
                started_at_unix_ms: None,
            },
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
            named_volume_names: volumes
                .iter()
                .map(|name| VolumeName::try_new(*name).expect("volume name"))
                .collect(),
        }
    }

    fn container_row(container_id: &str, operation_id: OperationRowId) -> ObservedContainer {
        let document: ContainerDocument = serde_json::from_value(serde_json::json!({
            "v": 1,
            "cluster_id": "01J00000000000000000000010",
            "machine_id": "01J00000000000000000000012",
            "service_id": service_id(),
            "namespace_id": namespace_id(),
            "ip": "10.210.20.9",
            "deploy": operation_id
        }))
        .expect("container document");
        ObservedContainer {
            id: ContainerId::try_new(container_id).expect("container id"),
            exact_document: serde_json::to_string(&document).expect("container json"),
            document,
        }
    }

    fn claim_candidate(id: &str, created_at: &str) -> (OperationRowId, OperationDocument) {
        let document = OperationDocument::deploy_created(
            ployz_core::corrosion::CorrosionDocumentVersion::V1,
            cluster_id(),
            machine_id(),
            Principal::Peer {
                peer_id: PeerId::try_new("01J00000000000000000000015").expect("peer"),
            },
            namespace_id(),
            ployz_core::corrosion::CorrosionDeployTargets::try_new(vec![service_id()])
                .expect("targets"),
            CorrosionTimestamp::try_new(created_at).expect("timestamp"),
        );
        (OperationRowId::try_new(id).expect("operation"), document)
    }

    fn request() -> ployz_core::DeployRequest {
        request_with(HealthGatePolicy::Enforce)
    }

    fn request_with(health_gate: HealthGatePolicy) -> ployz_core::DeployRequest {
        let mut environment = BTreeMap::new();
        environment.insert(
            EnvName::try_new("DATABASE_PASSWORD").expect("env name"),
            EnvValue::try_new("do-not-persist-this-secret").expect("env value"),
        );
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.environment = environment.into();
        ployz_core::DeployRequest {
            namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace name"),
            service_name: CorrosionServiceName::try_new("api").expect("service name"),
            image: ImageReference::try_new("nginx:1.27-alpine").expect("image"),
            runtime,
            health_gate,
        }
    }

    fn initiator() -> Principal {
        Principal::Peer {
            peer_id: PeerId::try_new("01J00000000000000000000015").expect("peer"),
        }
    }

    async fn evidence_kinds(
        log: &crate::roles::api::http::operation_evidence::OperationEvidenceLog,
    ) -> Vec<OperationEvidence> {
        log.replay_after(0)
            .await
            .expect("replay")
            .into_iter()
            .map(|event| event.evidence)
            .collect()
    }

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

    #[test]
    fn drain_covers_the_dns_ttl() {
        assert!(DEPLOY_DRAIN_WAIT >= Duration::from_secs(DNS_TTL_SECONDS as u64));
    }

    #[tokio::test]
    async fn missing_namespace_refuses_before_bridge_operation_or_docker_effects() {
        let fixture = fixture(Admission::Missing, true);
        let admission = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission");
        let Err(refusal) = admission else {
            panic!("missing namespace must refuse");
        };
        assert_eq!(
            refusal,
            DeployRefusal::namespace_not_found(
                CorrosionNamespaceName::try_new("production").expect("name")
            )
        );
        assert_eq!(fixture.runtime.bridge_reads.load(Ordering::SeqCst), 0);
        assert!(fixture.operations.writes.lock().await.is_empty());
        assert_eq!(fixture.runtime.created().await, 0);
    }

    #[tokio::test]
    async fn populated_namespace_shapes_refuse_before_effects() {
        for (admission, expected) in [
            (
                Admission::Ambiguous,
                DeployRefusal::NamespaceAmbiguous {
                    namespace_name: CorrosionNamespaceName::try_new("production").expect("name"),
                    namespace_ids: vec![namespace_id()],
                },
            ),
            (
                Admission::DifferentService,
                DeployRefusal::DifferentService {
                    namespace_id: namespace_id(),
                    incumbent_service_name: CorrosionServiceName::try_new("web").expect("name"),
                },
            ),
            (
                Admission::MultipleServices,
                DeployRefusal::MultipleServices {
                    namespace_id: namespace_id(),
                    service_ids: vec![service_id()],
                },
            ),
            (
                Admission::RoutesWithoutServices,
                DeployRefusal::RoutesWithoutServices {
                    namespace_id: namespace_id(),
                },
            ),
            (
                Admission::RedeployElsewhere,
                DeployRefusal::IncumbentOnAnotherMachine {
                    machine_id: other_machine_id(),
                },
            ),
        ] {
            let fixture = fixture(admission, true);
            let admission = fixture
                .driver
                .admit(request(), initiator())
                .await
                .expect("admission");
            let Err(refusal) = admission else {
                panic!("populated namespace must refuse");
            };
            assert_eq!(refusal, expected);
            assert!(fixture.operations.writes.lock().await.is_empty());
            assert_eq!(fixture.runtime.created().await, 0);
        }
    }

    #[tokio::test]
    async fn unavailable_bridge_refuses_before_operation_or_docker_effects() {
        let fixture = fixture(Admission::First, false);
        let admission = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission");
        let Err(refusal) = admission else {
            panic!("unavailable bridge must refuse");
        };
        assert_eq!(refusal, DeployRefusal::BridgeUnavailable);
        assert!(fixture.operations.writes.lock().await.is_empty());
        assert_eq!(fixture.runtime.created().await, 0);
    }

    #[tokio::test]
    async fn admitted_task_rejected_by_shutdown_terminalizes_without_running() {
        let fixture = fixture(Admission::First, true);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");

        accepted
            .interrupt_unspawned()
            .await
            .expect("interrupted terminal");

        assert_eq!(
            fixture.operations.writes.lock().await.as_slice(),
            [RowWrite::Created, RowWrite::Transition]
        );
        assert_eq!(fixture.runtime.created().await, 0);
        let operation = fixture
            .operations
            .operation
            .lock()
            .await
            .clone()
            .expect("operation");
        assert!(operation.is_terminal());
    }

    #[tokio::test]
    async fn successful_first_deploy_uses_three_row_writes_and_persists_no_secret() {
        let fixture = fixture(Admission::First, true);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("deploy");
        let writes: Vec<RowWrite> = fixture
            .operations
            .writes
            .lock()
            .await
            .iter()
            .copied()
            .filter(|write| *write != RowWrite::Heartbeat)
            .collect();
        assert_eq!(
            writes,
            [
                RowWrite::Created,
                RowWrite::Transition,
                RowWrite::Transition
            ]
        );
        assert_eq!(fixture.runtime.created().await, 1);
        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::OpClaimWon));
        let prepared = fixture
            .store
            .prepared
            .lock()
            .await
            .clone()
            .expect("prepared promotion");
        let durable = serde_json::to_string(&prepared).expect("durable promotion json");
        assert!(!durable.contains("do-not-persist-this-secret"));
        assert!(durable.contains("DATABASE_PASSWORD"));
        assert!(prepared.service_document.image.pinned_digest().is_some());
    }

    #[tokio::test]
    async fn exhausted_claim_adjudication_retries_three_times_then_terminalizes() {
        let fixture = fixture(Admission::First, true);
        fixture
            .store
            .fail_adjudication
            .store(true, Ordering::SeqCst);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed terminal");
        assert_eq!(
            fixture.store.adjudication_attempts.load(Ordering::SeqCst),
            3
        );
        assert!(matches!(
            fixture.operations.terminal_state().await,
            CorrosionDeployState::Terminal { .. }
        ));
    }

    #[tokio::test]
    async fn failed_health_gate_retains_the_started_container_as_evidence() {
        let fixture = fixture(Admission::First, true);
        fixture.runtime.health_failure.store(true, Ordering::SeqCst);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed failure");
        assert_eq!(fixture.runtime.created().await, 1);
        let calls = fixture.runtime.call_log().await;
        assert!(
            !calls
                .iter()
                .any(|call| matches!(call, RuntimeCall::Remove(_)))
        );
        assert!(fixture.store.prepared.lock().await.is_none());
    }

    #[tokio::test]
    async fn loser_cleanup_that_never_converges_terminalizes_after_three_attempts() {
        let fixture = fixture(Admission::First, true);
        fixture.store.claim_lost.store(true, Ordering::SeqCst);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed terminal");
        assert_eq!(fixture.store.cleanup_attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn claim_loss_terminalizes_superseded_with_zero_docker_effects() {
        let fixture = fixture(Admission::Redeploy, true);
        let (winner_id, winner_doc) =
            claim_candidate("01J00000000000000000000001", "2026-08-05T10:00:00Z");
        *fixture.operations.claim_candidates.lock().await = vec![(winner_id.clone(), winner_doc)];
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed terminal");

        assert!(fixture.runtime.call_log().await.is_empty());
        // The claim was lost before mark_running: Created then Terminal only.
        assert_eq!(
            fixture.operations.writes.lock().await.as_slice(),
            [RowWrite::Created, RowWrite::Transition]
        );
        let CorrosionDeployState::Terminal { outcome, .. } =
            fixture.operations.terminal_state().await
        else {
            panic!("terminal state expected");
        };
        assert!(matches!(
            outcome,
            CorrosionDeployOutcome::Failed {
                failure: ployz_core::corrosion::CorrosionDeployFailure::SupersededByOperation {
                    winner,
                },
                ..
            } if winner == winner_id
        ));
        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::OpClaimLost { winner: winner_id }));
    }

    #[tokio::test]
    async fn stale_lower_claim_does_not_block() {
        let fixture = fixture(Admission::First, true);
        let stale = claim_candidate("01J00000000000000000000001", "2026-08-05T08:00:00Z");
        *fixture.operations.claim_candidates.lock().await = vec![stale];
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("deploy");
        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::OpClaimWon));
        assert!(matches!(
            fixture.operations.terminal_state().await,
            CorrosionDeployState::Terminal {
                outcome: CorrosionDeployOutcome::Completed { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn second_deploy_flips_atomically_with_previous_image() {
        let fixture = fixture(Admission::Redeploy, true);
        *fixture.runtime.containers.lock().await =
            vec![docker_container("incumbent-1", incumbent_op(), &[])];
        *fixture.store.rows.lock().await = vec![container_row("incumbent-1", incumbent_op())];
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let operation_id = accepted.reply.operation_id.clone();
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("redeploy");

        let intent = fixture
            .store
            .redeploy_prepared
            .lock()
            .await
            .clone()
            .expect("flip intent");
        assert_eq!(
            intent.exact_incumbent_document,
            incumbent_service(machine_id()).exact_document
        );
        assert_eq!(intent.service_document.active_deploy, operation_id);
        assert_eq!(intent.service_document.operation_id, operation_id);
        assert_eq!(
            intent.service_document.previous_image,
            Some(incumbent_document(machine_id()).image)
        );
        assert_eq!(intent.container_document.deploy, operation_id);
        assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 1);
        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::RowsCommitted));
        assert!(events.contains(&OperationEvidence::Drained));
        assert!(events.contains(&OperationEvidence::IncumbentStopped {
            container_id: ContainerId::try_new("incumbent-1").expect("container"),
        }));
        assert!(events.contains(&OperationEvidence::IncumbentRemoved {
            container_id: ContainerId::try_new("incumbent-1").expect("container"),
        }));
        assert!(matches!(
            fixture.operations.terminal_state().await,
            CorrosionDeployState::Terminal {
                outcome: CorrosionDeployOutcome::Completed { .. },
                ..
            }
        ));
        // The old revision's row is deleted exactly as observed.
        let deleted = fixture.store.deleted.lock().await.clone();
        assert!(deleted.iter().any(|rows| {
            rows == &vec![ContainerId::try_new("incumbent-1").expect("container")]
        }));
    }

    #[tokio::test]
    async fn pre_flip_failure_never_flips_and_leaves_the_incumbent_serving() {
        let fixture = fixture(Admission::Redeploy, true);
        fixture.runtime.health_failure.store(true, Ordering::SeqCst);
        *fixture.runtime.containers.lock().await =
            vec![docker_container("incumbent-1", incumbent_op(), &[])];
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed failure");

        assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
        assert!(fixture.store.redeploy_prepared.lock().await.is_none());
        let calls = fixture.runtime.call_log().await;
        assert!(!calls.contains(&RuntimeCall::Stop("incumbent-1".to_owned())));
        assert!(
            !calls
                .iter()
                .any(|call| matches!(call, RuntimeCall::Remove(_)))
        );
        assert_eq!(fixture.runtime.created().await, 1);
        assert!(matches!(
            fixture.operations.terminal_state().await,
            CorrosionDeployState::Terminal {
                outcome: CorrosionDeployOutcome::Failed { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn volume_service_stops_incumbent_only_after_create_and_restarts_on_gate_failure() {
        let fixture = fixture(Admission::Redeploy, true);
        fixture.runtime.health_failure.store(true, Ordering::SeqCst);
        *fixture.runtime.containers.lock().await =
            vec![docker_container("incumbent-1", incumbent_op(), &["data"])];
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed failure");

        let calls = fixture.runtime.call_log().await;
        let create_at = calls
            .iter()
            .position(|call| matches!(call, RuntimeCall::Create))
            .expect("create call");
        let stop_at = calls
            .iter()
            .position(|call| call == &RuntimeCall::Stop("incumbent-1".to_owned()))
            .expect("incumbent stop");
        assert!(
            create_at < stop_at,
            "pull/create must precede incumbent stop"
        );
        // The incumbent is restarted after the failed gate; the failed new
        // container is retained.
        assert!(calls.contains(&RuntimeCall::Start("incumbent-1".to_owned())));
        assert!(
            !calls
                .iter()
                .any(|call| matches!(call, RuntimeCall::Remove(_)))
        );
        assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::IncumbentStopped {
            container_id: ContainerId::try_new("incumbent-1").expect("container"),
        }));
        assert!(events.contains(&OperationEvidence::IncumbentRestarted {
            container_id: ContainerId::try_new("incumbent-1").expect("container"),
        }));
    }

    #[tokio::test]
    async fn takeover_abort_before_flip_mutates_nothing_further() {
        let fixture = fixture(Admission::Redeploy, true);
        *fixture.runtime.containers.lock().await =
            vec![docker_container("incumbent-1", incumbent_op(), &[])];
        let rival = claim_candidate("7ZZZZZZZZZZZZZZZZZZZZZZZZZ", "2026-08-05T10:00:00Z");
        *fixture.operations.takeover_candidates.lock().await = vec![rival.clone()];
        // The rival becomes visible only at the pre-flip boundary.
        fixture
            .operations
            .takeover_visible_after
            .store(1, Ordering::SeqCst);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed terminal");

        assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 0);
        let calls = fixture.runtime.call_log().await;
        assert!(!calls.contains(&RuntimeCall::Stop("incumbent-1".to_owned())));
        assert!(
            !calls
                .iter()
                .any(|call| matches!(call, RuntimeCall::Remove(_)))
        );
        let CorrosionDeployState::Terminal { outcome, .. } =
            fixture.operations.terminal_state().await
        else {
            panic!("terminal state expected");
        };
        assert!(matches!(
            outcome,
            CorrosionDeployOutcome::Failed {
                failure: ployz_core::corrosion::CorrosionDeployFailure::SupersededByOperation {
                    winner,
                },
                ..
            } if winner == rival.0
        ));
        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::OpClaimLost { winner: rival.0 }));
    }

    #[tokio::test]
    async fn sweep_removes_only_foreign_debris_and_their_rows() {
        let fixture = fixture(Admission::Redeploy, true);
        *fixture.runtime.containers.lock().await = vec![
            docker_container("debris-1", debris_op(), &[]),
            docker_container("incumbent-1", incumbent_op(), &[]),
        ];
        *fixture.store.rows.lock().await = vec![
            container_row("debris-1", debris_op()),
            container_row("incumbent-1", incumbent_op()),
        ];
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("redeploy");

        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::DebrisSwept {
            removed: vec![ContainerId::try_new("debris-1").expect("container")],
        }));
        // The sweep's exact row delete names only the debris row.
        let deleted = fixture.store.deleted.lock().await.clone();
        let [first_delete, ..] = deleted.as_slice() else {
            panic!("sweep must delete debris rows");
        };
        assert_eq!(
            first_delete,
            &vec![ContainerId::try_new("debris-1").expect("container")]
        );
        let calls = fixture.runtime.call_log().await;
        let debris_remove = calls
            .iter()
            .position(|call| call == &RuntimeCall::Remove("debris-1".to_owned()))
            .expect("debris removed");
        let create_at = calls
            .iter()
            .position(|call| matches!(call, RuntimeCall::Create))
            .expect("create call");
        assert!(debris_remove < create_at, "sweep precedes pull/create");
    }

    #[tokio::test]
    async fn skip_gate_completes_with_the_health_gate_warning() {
        let fixture = fixture(Admission::Redeploy, true);
        // The gate would fail; skip must bypass it entirely.
        fixture.runtime.health_failure.store(true, Ordering::SeqCst);
        *fixture.runtime.containers.lock().await =
            vec![docker_container("incumbent-1", incumbent_op(), &[])];
        let accepted = fixture
            .driver
            .admit(request_with(HealthGatePolicy::Skip), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let log = accepted.operation_log();
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("redeploy");

        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::HealthGateSkipped));
        let CorrosionDeployState::Terminal { outcome, .. } =
            fixture.operations.terminal_state().await
        else {
            panic!("terminal state expected");
        };
        let CorrosionDeployOutcome::CompletedWithWarnings { warnings, .. } = outcome else {
            panic!("skip-gate must complete with warnings");
        };
        assert_eq!(
            warnings,
            vec![CorrosionDeployWarning::HealthGateSkipped {
                service_id: service_id(),
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_threads_through_the_shared_handle_and_stops_before_terminal() {
        let fixture = fixture(Admission::Redeploy, true);
        let fixture = Fixture {
            driver: fixture
                .driver
                .clone()
                .with_waits(Duration::ZERO, Duration::from_secs(20)),
            ..fixture
        };
        *fixture.runtime.containers.lock().await =
            vec![docker_container("incumbent-1", incumbent_op(), &[])];
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("redeploy");

        // The 20s drain outlives the 15s heartbeat interval: at least one
        // refresh lands, and none may follow the terminal transition.
        let writes = fixture.operations.writes.lock().await.clone();
        assert!(writes.contains(&RowWrite::Heartbeat));
        assert_eq!(writes.last(), Some(&RowWrite::Transition));
        let after_last_transition = writes
            .iter()
            .rposition(|write| *write == RowWrite::Transition)
            .expect("terminal transition");
        assert!(
            writes
                .iter()
                .skip(after_last_transition)
                .all(|write| *write != RowWrite::Heartbeat),
            "no heartbeat may land after the terminal write"
        );
        assert!(matches!(
            fixture.operations.terminal_state().await,
            CorrosionDeployState::Terminal {
                outcome: CorrosionDeployOutcome::Completed { .. },
                ..
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn stale_heartbeat_over_a_terminal_row_raises_the_superseded_flag() {
        let fixture = fixture(Admission::First, true);
        fixture
            .operations
            .stale_heartbeat
            .store(true, Ordering::SeqCst);
        let operation_id = OperationRowId::try_new("01J00000000000000000000011").expect("op");
        let (_, document) = claim_candidate("01J00000000000000000000011", "2026-08-05T10:00:00Z");
        let terminal = document
            .clone()
            .transition_deploy(ployz_core::corrosion::CorrosionDeployTransition::Terminal {
                completed_at: CorrosionTimestamp::try_new("2026-08-05T10:00:05Z")
                    .expect("timestamp"),
                outcome: CorrosionDeployOutcome::failed(
                    vec![
                        ployz_core::corrosion::CorrosionDeployServiceResult::skipped(service_id()),
                    ],
                    ployz_core::corrosion::CorrosionDeployFailure::Interrupted,
                )
                .expect("outcome"),
            })
            .expect("terminal");
        *fixture.operations.operation.lock().await = Some(terminal);
        let row = Arc::new(Mutex::new(
            observed_with(&operation_id, document).expect("observed"),
        ));
        let heartbeat = DeployHeartbeat::spawn(fixture.driver.clone(), row);

        tokio::time::sleep(super::DEPLOY_HEARTBEAT_INTERVAL * 2).await;
        assert!(heartbeat.superseded());
        heartbeat.stop().await;
    }

    #[tokio::test]
    async fn resumed_redeploy_never_enters_the_first_deploy_claim_branch() {
        let fixture = fixture(Admission::Redeploy, true);
        let intent =
            crate::roles::api::http::operation_evidence::prepared_redeploy_intent_fixture();
        let operation_id = OperationRowId::try_new("01J00000000000000000000011").expect("op");
        let (_, document) = claim_candidate("01J00000000000000000000011", "2026-08-05T10:00:00Z");
        *fixture.operations.operation.lock().await = Some(document);
        let directory =
            OperationEvidenceDirectory::new(fixture._root.path().join("resume"), 16 * 1024);
        let log = directory
            .create(
                crate::roles::api::http::operation_evidence::EvidenceIdentity::new(
                    operation_id.clone(),
                    machine_id(),
                ),
                CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
            )
            .await
            .expect("create evidence");
        log.append(
            CorrosionTimestamp::try_new("2026-08-05T10:00:00Z").expect("timestamp"),
            OperationEvidence::OpClaimWon,
        )
        .await
        .expect("claim won");
        log.append_redeploy_prepared(
            CorrosionTimestamp::try_new("2026-08-05T10:00:01Z").expect("timestamp"),
            intent.clone(),
        )
        .await
        .expect("redeploy prepared");

        fixture
            .driver
            .resume_promotion(
                operation_id,
                log.clone(),
                crate::roles::api::http::operation_evidence::DurablePromotionProgress::RedeployPrepared {
                    prepared: intent,
                },
            )
            .await
            .expect("resume");

        // The redeploy resume converges the flip and never touches the
        // first-deploy service-name claim machinery.
        assert_eq!(fixture.store.converge_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.store.adjudication_attempts.load(Ordering::SeqCst),
            0
        );
        assert_eq!(fixture.store.cleanup_attempts.load(Ordering::SeqCst), 0);
        let events = evidence_kinds(&log).await;
        assert!(events.contains(&OperationEvidence::RowsCommitted));
        assert!(matches!(
            fixture.operations.terminal_state().await,
            CorrosionDeployState::Terminal {
                outcome: CorrosionDeployOutcome::Completed { .. },
                ..
            }
        ));
    }
}
