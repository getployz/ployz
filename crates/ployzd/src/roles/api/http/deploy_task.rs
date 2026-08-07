//! The deploy task: one operation's phase execution against the runtime,
//! plus its op-row heartbeat.
//!
//! The task owns Docker phase ordering, cutover strategy, sweep/cleanup
//! evidence, and shutdown handling; row convergence and terminalization stay
//! on the driver it carries.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::{
    ContainerDocument, CorrosionDeployFailure, CorrosionDeployOutcome,
    CorrosionDeployServiceFailure, CorrosionDeployServiceResult, CorrosionDeployTransition,
    CorrosionDeployWarning, CorrosionDocumentVersion, CorrosionServiceName, CorrosionTimestamp,
    DEPLOY_HEARTBEAT_INTERVAL, DeployClaim, DeployTakeover, OperationInitiator,
    OperatorWriteProvenance, ServiceDocument, ServicePlacement, ServiceReplicaCount,
    V2ManagedContainerIdentity, adjudicate_deploy_claim, check_deploy_takeover,
    fingerprint_env_value,
};
use ployz_core::ids::{ContainerId, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId};
use ployz_core::{DeployAccepted, DeployRequest, HealthGatePolicy};
use ployz_core::{OperationEvidence, deploy::ImageReference};
use tokio::sync::{Mutex, watch};
use tokio::time::MissedTickBehavior;

use crate::roles::api::runner::ExistingV2ManagedContainer;

use super::deploy::{
    DeployDriver, DeployDriverError, DeployPath, RedeployFlipEnd, cleanup_incomplete, observed_with,
};
use super::deploy_runtime::bounded_diagnostic;
use super::operation_evidence::{OperationEvidenceLog, PreparedPromotion, PreparedRedeployIntent};
use super::operation_finalizer::PromotionFinalizerState;
use super::operation_store::{ConditionalOperationWrite, HeartbeatWrite, ObservedOperation};
use super::promotion_store::{ObservedContainer, ObservedService, ResolvedNamespace};

/// How the incumbent hands traffic to the replacement, decided once from the
/// observed incumbents and the requested runtime.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CutoverStrategy {
    /// The replacement starts and passes its gate while the incumbent still
    /// serves; the incumbent drains after the flip.
    StartFirst,
    /// The incumbent holds named volumes, so it must stop before its
    /// replacement starts.
    StopFirst,
}

/// Which evidence verbs one best-effort stop/remove pass appends.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CleanupEvidence {
    /// Foreign debris: removed silently, reported once via `DebrisSwept`.
    Debris,
    /// Incumbents the cutover already stopped: remove only, one
    /// `IncumbentRemoved` per container.
    RemoveStopped,
    /// Drained incumbents: `IncumbentStopped` then `IncumbentRemoved` per
    /// container.
    StopThenRemove,
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

/// The identity a prepared deploy's service document descends from: the
/// first-deploy defaults or the incumbent's own fields.
struct ServiceLineage {
    namespace_id: NamespaceRowId,
    name: CorrosionServiceName,
    placement: ServicePlacement,
    pinned_machines: BTreeSet<MachineRowId>,
    previous_image: Option<ImageReference>,
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
pub(super) struct DeployHeartbeat {
    stop: watch::Sender<bool>,
    superseded: watch::Receiver<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl DeployHeartbeat {
    pub(super) fn spawn(driver: DeployDriver, row: Arc<Mutex<ObservedOperation>>) -> Self {
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
                match tokio::time::timeout(
                    driver.effect_timeout,
                    driver.operations.refresh_heartbeat(&row, now),
                )
                .await
                {
                    Ok(Ok(HeartbeatWrite::Written(refreshed))) => *row = *refreshed,
                    Ok(Ok(HeartbeatWrite::Stale)) => {
                        match tokio::time::timeout(
                            driver.effect_timeout,
                            driver.operations.operation(&row.id),
                        )
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

    pub(super) fn superseded(&self) -> bool {
        *self.superseded.borrow()
    }

    /// Stops the refresher and waits it out so no heartbeat write can land
    /// after the terminal write.
    pub(super) async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

pub(super) struct DeployTask {
    pub(super) driver: DeployDriver,
    pub(super) operation_id: OperationRowId,
    pub(super) service_id: ServiceRowId,
    pub(super) request: DeployRequest,
    pub(super) initiator: OperationInitiator,
    pub(super) path: DeployPath,
    pub(super) log: OperationEvidenceLog,
    pub(super) row: Arc<Mutex<ObservedOperation>>,
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
        namespace: &ResolvedNamespace,
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
        namespace: &ResolvedNamespace,
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
        let strategy = if incumbents
            .iter()
            .any(|container| !container.named_volume_names.is_empty())
            || !self.request.runtime.volume_mounts.is_empty()
        {
            CutoverStrategy::StopFirst
        } else {
            CutoverStrategy::StartFirst
        };

        // Sweep is best-effort: debris that refuses to die stays for the next
        // deploy's sweep and never fails this operation.
        let removed = self
            .stop_then_remove(&debris, CleanupEvidence::Debris)
            .await?;
        if !removed.is_empty() {
            self.delete_container_rows(&removed).await;
            self.log
                .append(
                    self.now()?,
                    OperationEvidence::DebrisSwept {
                        removed,
                        machine: None,
                    },
                )
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

        let cutover = match strategy {
            CutoverStrategy::StopFirst => {
                self.stop_first_cutover(
                    shutdown,
                    heartbeat,
                    &incumbents,
                    &container_id,
                    &identity,
                    &mut warnings,
                )
                .await?
            }
            CutoverStrategy::StartFirst => {
                self.start_first_cutover(shutdown, &container_id, &identity, &mut warnings)
                    .await?
            }
        };
        let ip = match cutover {
            Ok(ip) => ip,
            Err(end) => return Ok(end),
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
        // operation; leftovers are the next deploy's sweep, and skipping or
        // failing them is surfaced as a cleanup warning on the outcome.
        match strategy {
            CutoverStrategy::StopFirst => {
                let removed = self
                    .stop_then_remove(&incumbents, CleanupEvidence::RemoveStopped)
                    .await?;
                if removed.len() < incumbents.len() {
                    warnings.push(cleanup_incomplete(
                        "some old-revision containers could not be removed; the next deploy's sweep collects them",
                    ));
                }
            }
            CutoverStrategy::StartFirst => {
                tokio::select! {
                    _ = shutdown.changed() => {
                        warnings.push(cleanup_incomplete(
                            "shutdown before the old revision drained; its containers were left running",
                        ));
                        return Ok(DeployTaskEnd::Completed { warnings });
                    }
                    () = tokio::time::sleep(self.driver.drain_wait) => {}
                }
                self.log
                    .append(self.now()?, OperationEvidence::Drained)
                    .await?;
                let removed = self
                    .stop_then_remove(&incumbents, CleanupEvidence::StopThenRemove)
                    .await?;
                if removed.len() < incumbents.len() {
                    warnings.push(cleanup_incomplete(
                        "some old-revision containers could not be stopped or removed; the next deploy's sweep collects them",
                    ));
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

    /// Stop-first cutover for a volume-holding incumbent: stop it, start the
    /// replacement, and gate it.
    ///
    /// The incumbent restart runs only for service failures. A shutdown
    /// interruption after the incumbent stop leaves the service down until
    /// the deploy is re-run: a restart launched during shutdown could not be
    /// awaited to completion, so the interrupted terminal outcome is the
    /// evidence instead.
    async fn stop_first_cutover(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        heartbeat: &DeployHeartbeat,
        incumbents: &[ExistingV2ManagedContainer],
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
        warnings: &mut Vec<CorrosionDeployWarning>,
    ) -> Result<Result<Ipv4Addr, DeployTaskEnd>, DeployDriverError> {
        if let Some(end) = self.takeover_boundary(shutdown, heartbeat).await? {
            return Ok(Err(end));
        }
        for container in incumbents {
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
                                machine: None,
                            },
                        )
                        .await?;
                }
                EffectResult::Completed(Err(message)) => {
                    return Ok(Err(DeployTaskEnd::ServiceFailure {
                        failure: CorrosionDeployServiceFailure::IncumbentStopFailed {
                            message: bounded_diagnostic(message),
                        },
                    }));
                }
                EffectResult::TimedOut => {
                    return Ok(Err(DeployTaskEnd::ServiceFailure {
                        failure: CorrosionDeployServiceFailure::IncumbentStopFailed {
                            message: "incumbent stop timed out".to_owned(),
                        },
                    }));
                }
                EffectResult::Shutdown => return Ok(Err(DeployTaskEnd::Interrupted)),
            }
        }
        match self.start_new_container(shutdown, container_id).await? {
            Ok(()) => {}
            Err(end) => {
                self.restart_incumbents(incumbents).await?;
                return Ok(Err(end));
            }
        }
        match self
            .health_gate_or_skip(shutdown, container_id, identity, warnings)
            .await?
        {
            Ok(ip) => Ok(Ok(ip)),
            Err(end) => {
                // The failed replacement is retained for inspection; only
                // the incumbent is brought back.
                if matches!(end, DeployTaskEnd::ServiceFailure { .. }) {
                    self.restart_incumbents(incumbents).await?;
                }
                Ok(Err(end))
            }
        }
    }

    /// Start-first cutover: the incumbent keeps serving while the replacement
    /// starts and passes its gate.
    async fn start_first_cutover(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        container_id: &ContainerId,
        identity: &V2ManagedContainerIdentity,
        warnings: &mut Vec<CorrosionDeployWarning>,
    ) -> Result<Result<Ipv4Addr, DeployTaskEnd>, DeployDriverError> {
        if let Err(end) = self.start_new_container(shutdown, container_id).await? {
            return Ok(Err(end));
        }
        self.health_gate_or_skip(shutdown, container_id, identity, warnings)
            .await
    }

    /// One best-effort stop/remove pass over `containers`, appending the
    /// evidence verbs the phase calls for. Containers that refuse to die stay
    /// for the next deploy's sweep and never fail the operation. Returns the
    /// ids removed from Docker.
    async fn stop_then_remove(
        &self,
        containers: &[ExistingV2ManagedContainer],
        evidence: CleanupEvidence,
    ) -> Result<Vec<ContainerId>, DeployDriverError> {
        let mut removed = Vec::new();
        for container in containers {
            match evidence {
                CleanupEvidence::RemoveStopped => {}
                CleanupEvidence::Debris | CleanupEvidence::StopThenRemove => {
                    let stopped = tokio::time::timeout(
                        self.driver.effect_timeout,
                        self.driver
                            .runtime
                            .stop_container(&container.container_id, &container.identity),
                    )
                    .await;
                    if !matches!(stopped, Ok(Ok(_))) {
                        continue;
                    }
                    if matches!(evidence, CleanupEvidence::StopThenRemove) {
                        self.log
                            .append(
                                self.now()?,
                                OperationEvidence::IncumbentStopped {
                                    container_id: container.container_id.clone(),
                                    machine: None,
                                },
                            )
                            .await?;
                    }
                }
            }
            let removal = tokio::time::timeout(
                self.driver.effect_timeout,
                self.driver
                    .runtime
                    .remove_container(&container.container_id, &container.identity),
            )
            .await;
            if !matches!(removal, Ok(Ok(()))) {
                continue;
            }
            if matches!(
                evidence,
                CleanupEvidence::RemoveStopped | CleanupEvidence::StopThenRemove
            ) {
                self.log
                    .append(
                        self.now()?,
                        OperationEvidence::IncumbentRemoved {
                            container_id: container.container_id.clone(),
                            machine: None,
                        },
                    )
                    .await?;
            }
            removed.push(container.container_id.clone());
        }
        Ok(removed)
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
        match tokio::time::timeout(
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
        namespace: &ResolvedNamespace,
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
                    machine: None,
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
                    machine: None,
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
            let restarted = tokio::time::timeout(
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
                            machine: None,
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
        let rows = match tokio::time::timeout(
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
        let _ = tokio::time::timeout(
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
                .await?
                .ok_or_else(|| {
                    DeployDriverError::Invariant("operation row disappeared".to_owned())
                })?;
            if observed.document == terminal {
                return Ok(());
            }
            return match self
                .driver
                .operations
                .replace_terminal(&observed, &terminal)
                .await?
            {
                ConditionalOperationWrite::Written => Ok(()),
                ConditionalOperationWrite::Stale => Err(DeployDriverError::Invariant(
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

    fn identity(&self, namespace_id: &NamespaceRowId) -> V2ManagedContainerIdentity {
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

    /// The service and container documents every prepared deploy intent
    /// shares, written from this operation's identity at one timestamp.
    fn deploy_documents(
        &self,
        lineage: ServiceLineage,
        resolved_image: ImageReference,
        ip: Ipv4Addr,
    ) -> Result<(ServiceDocument, ContainerDocument), DeployDriverError> {
        let deployed_at = self.now()?;
        let service_document = ServiceDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.driver.cluster_id.clone(),
            provenance: OperatorWriteProvenance {
                written_by: self.initiator.clone(),
                written_at: deployed_at,
            },
            namespace_id: lineage.namespace_id.clone(),
            name: lineage.name,
            image: resolved_image,
            env_fingerprints: self.env_fingerprints()?,
            placement: lineage.placement,
            pinned_machines: lineage.pinned_machines,
            active_deploy: self.operation_id.clone(),
            previous_image: lineage.previous_image,
            deployed_at,
            operation_id: self.operation_id.clone(),
        };
        let container_document = ContainerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: self.driver.cluster_id.clone(),
            machine_id: self.driver.machine_id.clone(),
            service_id: self.service_id.clone(),
            namespace_id: lineage.namespace_id,
            ip,
            deploy: self.operation_id.clone(),
        };
        Ok((service_document, container_document))
    }

    fn prepared_promotion(
        &self,
        namespace: &ResolvedNamespace,
        container_id: ContainerId,
        ip: Ipv4Addr,
        resolved_image: ImageReference,
    ) -> Result<PreparedPromotion, DeployDriverError> {
        let (service_document, container_document) = self.deploy_documents(
            ServiceLineage {
                namespace_id: namespace.id.clone(),
                name: self.request.service_name.clone(),
                placement: ServicePlacement::Replicated {
                    replicas: ServiceReplicaCount::try_new(1)
                        .map_err(|error| DeployDriverError::Invariant(error.to_string()))?,
                },
                pinned_machines: BTreeSet::from([self.driver.machine_id.clone()]),
                previous_image: None,
            },
            resolved_image,
            ip,
        )?;
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
        let (service_document, container_document) = self.deploy_documents(
            ServiceLineage {
                namespace_id: incumbent.document.namespace_id.clone(),
                name: incumbent.document.name.clone(),
                placement: incumbent.document.placement.clone(),
                pinned_machines: incumbent.document.pinned_machines.clone(),
                previous_image: Some(incumbent.document.image.clone()),
            },
            resolved_image,
            ip,
        )?;
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
            .transition_deploy(row.clone(), transition)
            .await?
        {
            ConditionalOperationWrite::Written => {
                *row = observed_with(&row.id, replacement)?;
                Ok(())
            }
            ConditionalOperationWrite::Stale => Err(DeployDriverError::Invariant(
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

    pub(super) async fn interrupted(&self) -> Result<(), DeployDriverError> {
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

fn shutdown_requested(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}
