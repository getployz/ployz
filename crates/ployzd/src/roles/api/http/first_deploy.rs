use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{
    ContainerDocument, CorrosionDeployFailure, CorrosionDeployOutcome,
    CorrosionDeployServiceFailure, CorrosionDeployServiceResult, CorrosionDeployTargets,
    CorrosionDeployTransition, CorrosionDocumentVersion, CorrosionPromotionFailure,
    CorrosionTimestamp, OperationDocument, OperationInitiator, OperatorWriteProvenance,
    ServiceDocument, ServicePlacement, ServiceReplicaCount, V2ManagedContainerIdentity,
    fingerprint_env_value,
};
use ployz_core::ids::{ClusterId, ContainerId, MachineRowId, OperationRowId, ServiceRowId};
use ployz_core::network::EndpointBridgeStatus;
use ployz_core::{FirstDeployAccepted, FirstDeployRefusal, FirstDeployRequest};
use ployz_core::{OperationEvidence, deploy::ImageReference};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::watch;

use crate::roles::api::runner::{
    CreateV2ManagedContainer, ExistingManagedContainerState, MachineContainerRunner,
    V2MachineContainerRunner, V2MachineImageRunner,
};

use super::operation_evidence::{
    DurablePromotionProgress, EvidenceIdentity, OperationEvidenceDirectory, OperationEvidenceLog,
    PreparedPromotion,
};
use super::operation_finalizer::{
    PreparedPromotionStore, PromotionFinalizerDecision, PromotionFinalizerState,
    PromotionRequestDisposition, UncertaintyDeadline,
};
use super::operation_store::{ConditionalOperationWrite, OperationStore};
use super::promotion_store::{
    FirstDeployNamespaceResolution, FirstDeployPreflightStore, ResolvedFirstDeployNamespace,
};

const EXTERNAL_EFFECT_TIMEOUT: Duration = Duration::from_secs(60);
const FINALIZER_ATTEMPTS: usize = 3;
const RUNNING_CONFIRMATION_WINDOW: Duration = Duration::from_secs(5);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_PERSISTED_DIAGNOSTIC_BYTES: usize = 4 * 1024;

pub(super) trait FirstDeployStore:
    FirstDeployPreflightStore + PreparedPromotionStore
{
}

impl<T> FirstDeployStore for T where T: FirstDeployPreflightStore + PreparedPromotionStore {}

#[async_trait]
pub(super) trait FirstDeployOperationRows: Send + Sync {
    async fn insert_created(
        &self,
        operation_id: &OperationRowId,
        operation: &OperationDocument,
    ) -> Result<(), String>;

    async fn mark_running(
        &self,
        operation_id: &OperationRowId,
        started_at: CorrosionTimestamp,
    ) -> Result<(), String>;

    async fn prepare_terminal(
        &self,
        operation_id: &OperationRowId,
        completed_at: CorrosionTimestamp,
        outcome: CorrosionDeployOutcome,
    ) -> Result<OperationDocument, String>;

    async fn commit_terminal(
        &self,
        operation_id: &OperationRowId,
        terminal: &OperationDocument,
    ) -> Result<(), String>;
}

#[async_trait]
impl FirstDeployOperationRows for OperationStore {
    async fn insert_created(
        &self,
        operation_id: &OperationRowId,
        operation: &OperationDocument,
    ) -> Result<(), String> {
        self.insert_created(operation_id, operation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn mark_running(
        &self,
        operation_id: &OperationRowId,
        started_at: CorrosionTimestamp,
    ) -> Result<(), String> {
        let observed = self
            .operation(operation_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "operation row disappeared".to_owned())?;
        match self
            .transition_deploy(observed, CorrosionDeployTransition::Running { started_at })
            .await
            .map_err(|error| error.to_string())?
        {
            ConditionalOperationWrite::Written => Ok(()),
            ConditionalOperationWrite::Stale => {
                Err("operation row changed before running".to_owned())
            }
        }
    }

    async fn prepare_terminal(
        &self,
        operation_id: &OperationRowId,
        completed_at: CorrosionTimestamp,
        outcome: CorrosionDeployOutcome,
    ) -> Result<OperationDocument, String> {
        let observed = self
            .operation(operation_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "operation row disappeared".to_owned())?;
        observed
            .document
            .clone()
            .transition_deploy(CorrosionDeployTransition::Terminal {
                completed_at,
                outcome,
            })
            .map_err(|error| error.to_string())
    }

    async fn commit_terminal(
        &self,
        operation_id: &OperationRowId,
        terminal: &OperationDocument,
    ) -> Result<(), String> {
        let observed = self
            .operation(operation_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "operation row disappeared".to_owned())?;
        match self
            .replace_terminal(&observed, terminal)
            .await
            .map_err(|error| error.to_string())?
        {
            ConditionalOperationWrite::Written => Ok(()),
            ConditionalOperationWrite::Stale => {
                Err("operation row changed before terminal".to_owned())
            }
        }
    }
}

#[async_trait]
pub(super) trait FirstDeployRuntime: Send + Sync {
    async fn bridge_ready(&self) -> bool;
    async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String>;
    async fn pull_image(
        &self,
        image: &ImageReference,
        shutdown: watch::Receiver<bool>,
    ) -> Result<(), String>;
    async fn create_container(
        &self,
        request: &FirstDeployRequest,
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
}

#[async_trait]
impl<Runner> FirstDeployRuntime for Runner
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
        request: &FirstDeployRequest,
        resolved_image: &ImageReference,
        namespace: &ResolvedFirstDeployNamespace,
        identity: V2ManagedContainerIdentity,
    ) -> Result<ContainerId, String> {
        self.create_v2_managed_container(CreateV2ManagedContainer {
            image: resolved_image.clone(),
            runtime: request.runtime.clone(),
            namespace_name: namespace.document.name.clone(),
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
            let containers = self
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
            let ExistingManagedContainerState::Running { ip: Some(ip), .. } = container.state
            else {
                return Err("started container stopped during its health gate".to_owned());
            };
            let ip = match ip {
                IpAddr::V4(ip) => ip,
                IpAddr::V6(_) => {
                    return Err("started container did not have an IPv4 endpoint".to_owned());
                }
            };
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

pub(super) trait FirstDeployClock: Send + Sync {
    fn now(&self) -> Result<CorrosionTimestamp, String>;
}

pub(super) struct SystemFirstDeployClock;

impl FirstDeployClock for SystemFirstDeployClock {
    fn now(&self) -> Result<CorrosionTimestamp, String> {
        let value = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .map_err(|error| error.to_string())?;
        CorrosionTimestamp::try_new(value).map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub(super) struct FirstDeployDriver {
    cluster_id: ClusterId,
    machine_id: MachineRowId,
    evidence: OperationEvidenceDirectory,
    store: Arc<dyn FirstDeployStore>,
    operations: Arc<dyn FirstDeployOperationRows>,
    runtime: Arc<dyn FirstDeployRuntime>,
    clock: Arc<dyn FirstDeployClock>,
    effect_timeout: Duration,
}

impl FirstDeployDriver {
    #[must_use]
    pub(super) fn new(
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        evidence: OperationEvidenceDirectory,
        store: Arc<dyn FirstDeployStore>,
        operations: Arc<dyn FirstDeployOperationRows>,
        runtime: Arc<dyn FirstDeployRuntime>,
        clock: Arc<dyn FirstDeployClock>,
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
        }
    }

    pub(super) async fn admit(
        &self,
        request: FirstDeployRequest,
        initiator: OperationInitiator,
    ) -> Result<Result<AcceptedFirstDeploy, FirstDeployRefusal>, FirstDeployDriverError> {
        let namespace = match self
            .store
            .resolve_empty_namespace(&request.namespace_name)
            .await?
        {
            FirstDeployNamespaceResolution::Missing => {
                return Ok(Err(FirstDeployRefusal::namespace_not_found(
                    request.namespace_name,
                )));
            }
            FirstDeployNamespaceResolution::Ambiguous { namespace_ids } => {
                return Ok(Err(FirstDeployRefusal::NamespaceAmbiguous {
                    namespace_name: request.namespace_name,
                    namespace_ids,
                }));
            }
            FirstDeployNamespaceResolution::NotFirst { namespace_id } => {
                return Ok(Err(FirstDeployRefusal::NotFirstDeploy { namespace_id }));
            }
            FirstDeployNamespaceResolution::Ready(namespace) => namespace,
        };
        if !bounded(self.effect_timeout, self.runtime.bridge_ready())
            .await
            .unwrap_or(false)
        {
            return Ok(Err(FirstDeployRefusal::BridgeUnavailable));
        }

        let operation_id = OperationRowId::generate();
        let service_id = ServiceRowId::generate();
        let created_at = self.clock.now().map_err(FirstDeployDriverError::Clock)?;
        let operation = OperationDocument::deploy_created(
            CorrosionDocumentVersion::V1,
            self.cluster_id.clone(),
            self.machine_id.clone(),
            initiator.clone(),
            namespace.id.clone(),
            CorrosionDeployTargets::try_new(vec![service_id.clone()])
                .map_err(|error| FirstDeployDriverError::Invariant(error.to_string()))?,
            created_at,
        );
        let log = self
            .evidence
            .create(
                EvidenceIdentity::new(operation_id.clone(), self.machine_id.clone()),
                created_at,
            )
            .await?;
        if let Err(error) = self
            .operations
            .insert_created(&operation_id, &operation)
            .await
        {
            return Err(FirstDeployDriverError::Operation(error));
        }
        Ok(Ok(AcceptedFirstDeploy {
            reply: FirstDeployAccepted {
                operation_id: operation_id.clone(),
                driver_machine_id: self.machine_id.clone(),
            },
            task: FirstDeployTask {
                driver: self.clone(),
                operation_id,
                service_id,
                request,
                initiator,
                namespace,
                log,
            },
        }))
    }

    pub(super) async fn resume_promotion(
        &self,
        operation_id: OperationRowId,
        log: OperationEvidenceLog,
        progress: DurablePromotionProgress,
    ) -> Result<(), FirstDeployDriverError> {
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
        };
        self.finish_promotion(&operation_id, &log, state).await
    }

    async fn finish_promotion(
        &self,
        operation_id: &OperationRowId,
        log: &OperationEvidenceLog,
        mut state: PromotionFinalizerState,
    ) -> Result<(), FirstDeployDriverError> {
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
                            super::operation_finalizer::PromotionRowsObservation::ABSENT,
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
                PromotionFinalizerDecision::RetryRows { state: next } => {
                    if attempts >= FINALIZER_ATTEMPTS {
                        return self
                            .terminalize_uncertain_promotion(operation_id, log, next)
                            .await;
                    }
                    state = next;
                }
                PromotionFinalizerDecision::RetryLoserCleanup { state: next } => {
                    if attempts >= FINALIZER_ATTEMPTS {
                        return self
                            .terminalize_uncertain_promotion(operation_id, log, next)
                            .await;
                    }
                    state = next;
                }
                PromotionFinalizerDecision::AppendRowsCommitted { state: next } => {
                    log.append(
                        self.clock.now().map_err(FirstDeployDriverError::Clock)?,
                        OperationEvidence::RowsCommitted,
                    )
                    .await?;
                    state = next;
                    attempts = 0;
                }
                PromotionFinalizerDecision::AppendClaimWon { state: next } => {
                    log.append(
                        self.clock.now().map_err(FirstDeployDriverError::Clock)?,
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
                        self.clock.now().map_err(FirstDeployDriverError::Clock)?,
                        OperationEvidence::ClaimLost { winner },
                    )
                    .await?;
                    state = next;
                    attempts = 0;
                }
                PromotionFinalizerDecision::Succeeded { prepared } => {
                    return self
                        .terminalize(
                            operation_id,
                            log,
                            CorrosionDeployOutcome::completed(vec![prepared.success_result])
                                .map_err(|error| {
                                    FirstDeployDriverError::Invariant(error.to_string())
                                })?,
                        )
                        .await;
                }
                PromotionFinalizerDecision::Failed { prepared, failure } => {
                    return self
                        .terminalize(
                            operation_id,
                            log,
                            CorrosionDeployOutcome::failed(
                                vec![CorrosionDeployServiceResult::skipped(prepared.service_id)],
                                failure,
                            )
                            .map_err(|error| {
                                FirstDeployDriverError::Invariant(error.to_string())
                            })?,
                        )
                        .await;
                }
            }
        }
    }

    async fn terminalize(
        &self,
        operation_id: &OperationRowId,
        log: &OperationEvidenceLog,
        outcome: CorrosionDeployOutcome,
    ) -> Result<(), FirstDeployDriverError> {
        let completed_at = self.clock.now().map_err(FirstDeployDriverError::Clock)?;
        let terminal = self
            .operations
            .prepare_terminal(operation_id, completed_at, outcome)
            .await
            .map_err(FirstDeployDriverError::Operation)?;
        log.append(
            completed_at,
            OperationEvidence::Terminal {
                operation: Box::new(terminal.clone()),
            },
        )
        .await?;
        self.operations
            .commit_terminal(operation_id, &terminal)
            .await
            .map_err(FirstDeployDriverError::Operation)?;
        Ok(())
    }

    async fn terminalize_uncertain_promotion(
        &self,
        operation_id: &OperationRowId,
        log: &OperationEvidenceLog,
        state: PromotionFinalizerState,
    ) -> Result<(), FirstDeployDriverError> {
        let PromotionFinalizerDecision::Failed { prepared, failure } = promotion_uncertain(state)
        else {
            return Err(FirstDeployDriverError::Invariant(
                "uncertain promotion did not produce a terminal failure".to_owned(),
            ));
        };
        self.terminalize(
            operation_id,
            log,
            CorrosionDeployOutcome::failed(
                vec![CorrosionDeployServiceResult::skipped(prepared.service_id)],
                failure,
            )
            .map_err(|error| FirstDeployDriverError::Invariant(error.to_string()))?,
        )
        .await
    }
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

pub(super) struct AcceptedFirstDeploy {
    pub(super) reply: FirstDeployAccepted,
    pub(super) task: FirstDeployTask,
}

impl AcceptedFirstDeploy {
    /// Registers the durable log before spawning so local watchers cannot miss a fast task.
    pub(super) fn operation_log(&self) -> OperationEvidenceLog {
        self.task.log.clone()
    }

    /// Closes the Created operation when task admission shuts between durable acceptance and spawn.
    pub(super) async fn interrupt_unspawned(self) -> Result<(), FirstDeployDriverError> {
        self.task.interrupted().await
    }
}

pub(super) struct FirstDeployTask {
    driver: FirstDeployDriver,
    operation_id: OperationRowId,
    service_id: ServiceRowId,
    request: FirstDeployRequest,
    initiator: OperationInitiator,
    namespace: ResolvedFirstDeployNamespace,
    log: OperationEvidenceLog,
}

impl FirstDeployTask {
    pub(super) async fn run(
        &self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), FirstDeployDriverError> {
        let started_at = self
            .driver
            .clock
            .now()
            .map_err(FirstDeployDriverError::Clock)?;
        self.driver
            .operations
            .mark_running(&self.operation_id, started_at)
            .await
            .map_err(FirstDeployDriverError::Operation)?;
        self.log
            .append(started_at, OperationEvidence::PullingImage)
            .await?;
        if shutdown_requested(&shutdown) {
            return self.interrupted().await;
        }
        let image = select_effect(
            &mut shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.resolve_image(&self.request.image),
        )
        .await;
        let image = match image {
            EffectResult::Completed(Ok(image)) => image,
            EffectResult::Completed(Err(message)) => {
                return self
                    .service_failure(CorrosionDeployServiceFailure::ImagePullFailed { message })
                    .await;
            }
            EffectResult::Shutdown | EffectResult::TimedOut => return self.interrupted().await,
        };
        let pull_shutdown = shutdown.clone();
        match select_effect(
            &mut shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.pull_image(&image, pull_shutdown),
        )
        .await
        {
            EffectResult::Completed(Ok(())) => {}
            EffectResult::Completed(Err(message)) => {
                return self
                    .service_failure(CorrosionDeployServiceFailure::ImagePullFailed { message })
                    .await;
            }
            EffectResult::TimedOut => {
                return self
                    .service_failure(CorrosionDeployServiceFailure::ImagePullFailed {
                        message: "image pull timed out".to_owned(),
                    })
                    .await;
            }
            EffectResult::Shutdown => return self.interrupted().await,
        }
        self.log
            .append(
                self.driver
                    .clock
                    .now()
                    .map_err(FirstDeployDriverError::Clock)?,
                OperationEvidence::ImageResolved,
            )
            .await?;
        let identity = V2ManagedContainerIdentity {
            namespace_id: self.namespace.id.clone(),
            service_id: self.service_id.clone(),
            operation_id: self.operation_id.clone(),
        };
        let created = select_effect(
            &mut shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.create_container(
                &self.request,
                &image,
                &self.namespace,
                identity.clone(),
            ),
        )
        .await;
        let container_id = match created {
            EffectResult::Completed(Ok(container_id)) => container_id,
            EffectResult::Completed(Err(message)) => {
                return self
                    .service_failure(CorrosionDeployServiceFailure::ContainerCreateFailed {
                        message,
                    })
                    .await;
            }
            EffectResult::Shutdown | EffectResult::TimedOut => return self.interrupted().await,
        };
        self.log
            .append(
                self.driver
                    .clock
                    .now()
                    .map_err(FirstDeployDriverError::Clock)?,
                OperationEvidence::ContainerCreated {
                    container_id: container_id.clone(),
                },
            )
            .await?;
        let started = select_effect(
            &mut shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.start_container(&container_id),
        )
        .await;
        match started {
            EffectResult::Completed(Ok(())) => {}
            EffectResult::Completed(Err(message)) => {
                return self
                    .service_failure(CorrosionDeployServiceFailure::ContainerStartFailed {
                        message,
                    })
                    .await;
            }
            EffectResult::Shutdown | EffectResult::TimedOut => return self.interrupted().await,
        }
        self.log
            .append(
                self.driver
                    .clock
                    .now()
                    .map_err(FirstDeployDriverError::Clock)?,
                OperationEvidence::ContainerStarted {
                    container_id: container_id.clone(),
                },
            )
            .await?;
        let ip = match select_effect(
            &mut shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.health_gate(&container_id, &identity),
        )
        .await
        {
            EffectResult::Completed(Ok(ip)) => ip,
            EffectResult::Completed(Err(message)) => {
                return self
                    .service_failure(CorrosionDeployServiceFailure::HealthGateFailed { message })
                    .await;
            }
            EffectResult::TimedOut => {
                return self
                    .service_failure(CorrosionDeployServiceFailure::HealthGateFailed {
                        message: "health gate timed out".to_owned(),
                    })
                    .await;
            }
            EffectResult::Shutdown => return self.interrupted().await,
        };
        let prepared = self.prepared_promotion(container_id, ip, image)?;
        self.log
            .append_promotion_prepared(
                self.driver
                    .clock
                    .now()
                    .map_err(FirstDeployDriverError::Clock)?,
                prepared.clone(),
            )
            .await?;
        self.driver
            .finish_promotion(
                &self.operation_id,
                &self.log,
                PromotionFinalizerState::PromotionPrepared { prepared },
            )
            .await
    }

    pub(super) async fn recover_after_error(&self) -> Result<(), FirstDeployDriverError> {
        let recovery = self.log.recovery_evidence().await?;
        if let Some(terminal) = recovery.terminal {
            return self
                .driver
                .operations
                .commit_terminal(&self.operation_id, &terminal)
                .await
                .map_err(FirstDeployDriverError::Operation);
        }
        if let Some(progress) = recovery.promotion {
            return self
                .driver
                .resume_promotion(self.operation_id.clone(), self.log.clone(), progress)
                .await;
        }
        self.interrupted().await
    }

    fn prepared_promotion(
        &self,
        container_id: ContainerId,
        ip: Ipv4Addr,
        resolved_image: ImageReference,
    ) -> Result<PreparedPromotion, FirstDeployDriverError> {
        let deployed_at = self
            .driver
            .clock
            .now()
            .map_err(FirstDeployDriverError::Clock)?;
        let env_fingerprints = self
            .request
            .runtime
            .environment
            .iter()
            .map(|(name, value)| {
                fingerprint_env_value(value)
                    .map(|fingerprint| (name.as_str().to_owned(), fingerprint))
                    .map_err(|error| FirstDeployDriverError::Invariant(error.to_string()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let service_document = ServiceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.driver.cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: self.initiator.clone(),
                written_at: deployed_at,
            },
            namespace_id: self.namespace.id.clone(),
            name: self.request.service_name.clone(),
            image: resolved_image,
            env_fingerprints,
            placement: ServicePlacement::Replicated {
                replicas: ServiceReplicaCount::try_new(1)
                    .map_err(|error| FirstDeployDriverError::Invariant(error.to_string()))?,
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
            namespace_id: self.namespace.id.clone(),
            ip,
            deploy: self.operation_id.clone(),
        };
        Ok(PreparedPromotion {
            namespace_id: self.namespace.id.clone(),
            exact_namespace_document: self.namespace.exact_document.clone(),
            service_id: self.service_id.clone(),
            service_document,
            container_id,
            container_document,
            success_result: CorrosionDeployServiceResult::completed(self.service_id.clone()),
        })
    }

    async fn service_failure(
        &self,
        failure: CorrosionDeployServiceFailure,
    ) -> Result<(), FirstDeployDriverError> {
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
        .map_err(|error| FirstDeployDriverError::Invariant(error.to_string()))?;
        self.driver
            .terminalize(&self.operation_id, &self.log, outcome)
            .await
    }

    async fn interrupted(&self) -> Result<(), FirstDeployDriverError> {
        let outcome = CorrosionDeployOutcome::failed(
            vec![CorrosionDeployServiceResult::skipped(
                self.service_id.clone(),
            )],
            CorrosionDeployFailure::Interrupted,
        )
        .map_err(|error| FirstDeployDriverError::Invariant(error.to_string()))?;
        self.driver
            .terminalize(&self.operation_id, &self.log, outcome)
            .await
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
pub(super) enum FirstDeployDriverError {
    #[error("operation persistence failed: {0}")]
    Operation(String),
    #[error("operation evidence failed: {0}")]
    Evidence(#[from] super::operation_evidence::OperationEvidenceError),
    #[error("promotion storage failed: {0}")]
    Promotion(#[from] super::operation_finalizer::PromotionFinalizerStoreError),
    #[error("timestamp creation failed: {0}")]
    Clock(String),
    #[error("first deploy invariant failed: {0}")]
    Invariant(String),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use ployz_core::corrosion::{
        CorrosionDeployOutcome, CorrosionDeployTransition, CorrosionNamespaceName,
        CorrosionServiceName, CorrosionTimestamp, NamespaceDocument, OperationDocument, Principal,
    };
    use ployz_core::deploy::{ContainerRuntimeSpec, EnvName, EnvValue, ImageReference};
    use ployz_core::ids::{ClusterId, ContainerId, MachineRowId, NamespaceRowId, PeerId};
    use tempfile::TempDir;
    use tokio::sync::{Mutex, watch};

    use super::{
        FirstDeployClock, FirstDeployDriver, FirstDeployOperationRows, FirstDeployRuntime,
        HEALTH_POLL_INTERVAL, HealthGateObservation, MAX_PERSISTED_DIAGNOSTIC_BYTES,
        RUNNING_CONFIRMATION_WINDOW, bounded_diagnostic, classify_health_observation,
    };
    use crate::roles::api::http::operation_evidence::{
        OperationEvidenceDirectory, PreparedPromotion,
    };
    use crate::roles::api::http::operation_finalizer::{
        PreparedPromotionStore, PromotionClaimOutcome, PromotionFinalizerStoreError,
        PromotionRequestDisposition, PromotionRowsObservation,
    };
    use crate::roles::api::http::promotion_store::{
        FirstDeployNamespaceResolution, FirstDeployPreflightStore, ResolvedFirstDeployNamespace,
    };

    #[derive(Clone, Copy)]
    enum Preflight {
        Missing,
        Ambiguous,
        NotFirst,
        Ready,
    }

    struct FakeStore {
        preflight: Preflight,
        prepared: Mutex<Option<PreparedPromotion>>,
        fail_adjudication: AtomicBool,
        adjudication_attempts: AtomicUsize,
        claim_lost: AtomicBool,
        cleanup_attempts: AtomicUsize,
    }

    #[async_trait]
    impl FirstDeployPreflightStore for FakeStore {
        async fn resolve_empty_namespace(
            &self,
            _name: &CorrosionNamespaceName,
        ) -> Result<FirstDeployNamespaceResolution, PromotionFinalizerStoreError> {
            Ok(match self.preflight {
                Preflight::Missing => FirstDeployNamespaceResolution::Missing,
                Preflight::Ambiguous => FirstDeployNamespaceResolution::Ambiguous {
                    namespace_ids: vec![
                        NamespaceRowId::try_new("01J00000000000000000000013").expect("namespace"),
                        NamespaceRowId::try_new("01J00000000000000000000014").expect("namespace"),
                    ],
                },
                Preflight::NotFirst => FirstDeployNamespaceResolution::NotFirst {
                    namespace_id: NamespaceRowId::try_new("01J00000000000000000000013")
                        .expect("namespace"),
                },
                Preflight::Ready => FirstDeployNamespaceResolution::Ready(resolved_namespace()),
            })
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
                    winner: ployz_core::ids::ServiceRowId::try_new("01J00000000000000000000019")
                        .expect("winner"),
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

    struct FakeOperations {
        writes: AtomicUsize,
        operation: Mutex<Option<OperationDocument>>,
    }

    #[async_trait]
    impl FirstDeployOperationRows for FakeOperations {
        async fn insert_created(
            &self,
            _operation_id: &ployz_core::ids::OperationRowId,
            operation: &OperationDocument,
        ) -> Result<(), String> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            *self.operation.lock().await = Some(operation.clone());
            Ok(())
        }

        async fn mark_running(
            &self,
            _operation_id: &ployz_core::ids::OperationRowId,
            started_at: CorrosionTimestamp,
        ) -> Result<(), String> {
            let mut operation = self.operation.lock().await;
            let current = operation
                .take()
                .ok_or_else(|| "missing operation".to_owned())?;
            *operation = Some(
                current
                    .transition_deploy(CorrosionDeployTransition::Running { started_at })
                    .map_err(|error| error.to_string())?,
            );
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn prepare_terminal(
            &self,
            _operation_id: &ployz_core::ids::OperationRowId,
            completed_at: CorrosionTimestamp,
            outcome: CorrosionDeployOutcome,
        ) -> Result<OperationDocument, String> {
            self.operation
                .lock()
                .await
                .clone()
                .ok_or_else(|| "missing operation".to_owned())?
                .transition_deploy(CorrosionDeployTransition::Terminal {
                    completed_at,
                    outcome,
                })
                .map_err(|error| error.to_string())
        }

        async fn commit_terminal(
            &self,
            _operation_id: &ployz_core::ids::OperationRowId,
            terminal: &OperationDocument,
        ) -> Result<(), String> {
            *self.operation.lock().await = Some(terminal.clone());
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct FakeRuntime {
        bridge_ready: AtomicBool,
        bridge_reads: AtomicUsize,
        created: AtomicUsize,
        started: AtomicUsize,
        health_failure: AtomicBool,
    }

    #[async_trait]
    impl FirstDeployRuntime for FakeRuntime {
        async fn bridge_ready(&self) -> bool {
            self.bridge_reads.fetch_add(1, Ordering::SeqCst);
            self.bridge_ready.load(Ordering::SeqCst)
        }

        async fn resolve_image(&self, image: &ImageReference) -> Result<ImageReference, String> {
            image
                .with_digest(
                    &ployz_core::image::OciDigest::try_new(format!("sha256:{}", "a".repeat(64)))
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
            _request: &ployz_core::FirstDeployRequest,
            _resolved_image: &ImageReference,
            _namespace: &ResolvedFirstDeployNamespace,
            _identity: ployz_core::corrosion::V2ManagedContainerIdentity,
        ) -> Result<ContainerId, String> {
            self.created.fetch_add(1, Ordering::SeqCst);
            ContainerId::try_new("first-deploy-container").map_err(|error| error.to_string())
        }

        async fn start_container(&self, _container_id: &ContainerId) -> Result<(), String> {
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn health_gate(
            &self,
            _container_id: &ContainerId,
            _identity: &ployz_core::corrosion::V2ManagedContainerIdentity,
        ) -> Result<Ipv4Addr, String> {
            if self.health_failure.load(Ordering::SeqCst) {
                return Err("unhealthy".to_owned());
            }
            Ok(Ipv4Addr::new(10, 210, 20, 2))
        }
    }

    struct Clock(AtomicUsize);

    impl FirstDeployClock for Clock {
        fn now(&self) -> Result<CorrosionTimestamp, String> {
            let second = self.0.fetch_add(1, Ordering::SeqCst);
            CorrosionTimestamp::try_new(format!("2026-08-05T10:00:{second:02}Z"))
                .map_err(|error| error.to_string())
        }
    }

    struct Fixture {
        _root: TempDir,
        driver: FirstDeployDriver,
        store: Arc<FakeStore>,
        operations: Arc<FakeOperations>,
        runtime: Arc<FakeRuntime>,
    }

    fn fixture(preflight: Preflight, bridge_ready: bool) -> Fixture {
        let root = tempfile::tempdir().expect("evidence root");
        let store = Arc::new(FakeStore {
            preflight,
            prepared: Mutex::new(None),
            fail_adjudication: AtomicBool::new(false),
            adjudication_attempts: AtomicUsize::new(0),
            claim_lost: AtomicBool::new(false),
            cleanup_attempts: AtomicUsize::new(0),
        });
        let operations = Arc::new(FakeOperations {
            writes: AtomicUsize::new(0),
            operation: Mutex::new(None),
        });
        let runtime = Arc::new(FakeRuntime {
            bridge_ready: AtomicBool::new(bridge_ready),
            bridge_reads: AtomicUsize::new(0),
            created: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            health_failure: AtomicBool::new(false),
        });
        let driver = FirstDeployDriver::new(
            cluster_id(),
            machine_id(),
            OperationEvidenceDirectory::new(root.path().to_owned(), 16 * 1024),
            store.clone(),
            operations.clone(),
            runtime.clone(),
            Arc::new(Clock(AtomicUsize::new(0))),
        );
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
            id: NamespaceRowId::try_new("01J00000000000000000000013").expect("namespace id"),
            exact_document: serde_json::to_string(&document).expect("namespace json"),
            document,
        }
    }

    fn request() -> ployz_core::FirstDeployRequest {
        let mut environment = BTreeMap::new();
        environment.insert(
            EnvName::try_new("DATABASE_PASSWORD").expect("env name"),
            EnvValue::try_new("do-not-persist-this-secret").expect("env value"),
        );
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.environment = environment.into();
        ployz_core::FirstDeployRequest {
            namespace_name: CorrosionNamespaceName::try_new("production").expect("namespace name"),
            service_name: CorrosionServiceName::try_new("api").expect("service name"),
            image: ImageReference::try_new("nginx:1.27-alpine").expect("image"),
            runtime,
        }
    }

    fn initiator() -> Principal {
        Principal::Peer {
            peer_id: PeerId::try_new("01J00000000000000000000015").expect("peer"),
        }
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

    #[tokio::test]
    async fn missing_namespace_refuses_before_bridge_operation_or_docker_effects() {
        let fixture = fixture(Preflight::Missing, true);
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
            ployz_core::FirstDeployRefusal::namespace_not_found(
                CorrosionNamespaceName::try_new("production").expect("name")
            )
        );
        assert_eq!(fixture.runtime.bridge_reads.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.runtime.created.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unavailable_bridge_refuses_before_operation_or_docker_effects() {
        let fixture = fixture(Preflight::Ready, false);
        let admission = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission");
        let Err(refusal) = admission else {
            panic!("unavailable bridge must refuse");
        };
        assert_eq!(refusal, ployz_core::FirstDeployRefusal::BridgeUnavailable);
        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.runtime.created.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn admitted_task_rejected_by_shutdown_terminalizes_without_running() {
        let fixture = fixture(Preflight::Ready, true);
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

        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.runtime.created.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.runtime.started.load(Ordering::SeqCst), 0);
        let operation = fixture
            .operations
            .operation
            .lock()
            .await
            .clone()
            .expect("operation");
        assert!(crate::roles::api::http::operation_lifecycle::operation_is_terminal(&operation));
    }

    #[tokio::test]
    async fn ambiguous_and_nonempty_namespaces_refuse_before_effects() {
        for preflight in [Preflight::Ambiguous, Preflight::NotFirst] {
            let fixture = fixture(preflight, true);
            let admission = fixture
                .driver
                .admit(request(), initiator())
                .await
                .expect("admission");
            assert!(admission.is_err());
            assert_eq!(fixture.runtime.bridge_reads.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 0);
            assert_eq!(fixture.runtime.created.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn successful_first_deploy_uses_three_row_writes_and_persists_no_secret() {
        let fixture = fixture(Preflight::Ready, true);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 1);
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("deploy");
        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 3);
        assert_eq!(fixture.runtime.created.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.runtime.started.load(Ordering::SeqCst), 1);
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
        assert!(
            prepared
                .service_document
                .env_fingerprints
                .contains_key("DATABASE_PASSWORD")
        );
        assert!(prepared.service_document.image.pinned_digest().is_some());
    }

    #[tokio::test]
    async fn exhausted_claim_adjudication_retries_three_times_then_terminalizes() {
        let fixture = fixture(Preflight::Ready, true);
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
        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 3);
        let operation = fixture
            .operations
            .operation
            .lock()
            .await
            .clone()
            .expect("terminal operation");
        assert!(matches!(
            operation.deploy_state(),
            Some(ployz_core::corrosion::CorrosionDeployState::Terminal { .. })
        ));
    }

    #[tokio::test]
    async fn failed_health_gate_retains_the_started_container_as_evidence() {
        let fixture = fixture(Preflight::Ready, true);
        fixture.runtime.health_failure.store(true, Ordering::SeqCst);
        let accepted = fixture
            .driver
            .admit(request(), initiator())
            .await
            .expect("admission")
            .expect("accepted");
        let (_shutdown, shutdown) = watch::channel(false);
        accepted.task.run(shutdown).await.expect("typed failure");
        assert_eq!(fixture.runtime.created.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.runtime.started.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 3);
        assert!(fixture.store.prepared.lock().await.is_none());
    }

    #[tokio::test]
    async fn loser_cleanup_that_never_converges_terminalizes_after_three_attempts() {
        let fixture = fixture(Preflight::Ready, true);
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
        assert_eq!(fixture.operations.writes.load(Ordering::SeqCst), 3);
    }
}
