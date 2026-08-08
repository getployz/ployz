//! The deploy task: one operation's phase execution against its picked
//! target machines, plus its op-row heartbeat.
//!
//! The task owns Docker phase ordering, cutover strategy, sweep/cleanup
//! evidence, and shutdown handling; row convergence and terminalization stay
//! on the driver it carries. Every Docker effect routes through the
//! machine-addressed dispatch: local targets hit the local runner, remote
//! targets get claim-scoped `/deploy/execute` verbs.

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::{
    ContainerDocument, CorrosionDeployFailure, CorrosionDeployOutcome,
    CorrosionDeployServiceFailure, CorrosionDeployServiceResult, CorrosionDeployTransition,
    CorrosionDeployWarning, CorrosionDocumentVersion, CorrosionServiceName, CorrosionTimestamp,
    DEPLOY_HEARTBEAT_INTERVAL, DeployClaim, DeployTakeover, OperationInitiator,
    OperatorWriteProvenance, ServiceDocument, V2ManagedContainerIdentity, adjudicate_deploy_claim,
    check_deploy_takeover, fingerprint_env_value,
};
use ployz_core::deploy::VolumeName;
use ployz_core::ids::{ContainerId, MachineRowId, NamespaceRowId, OperationRowId, ServiceRowId};
use ployz_core::{DeployAccepted, DeployRequest, HealthGatePolicy};
use ployz_core::{OperationEvidence, deploy::ImageReference};
use tokio::sync::{Mutex, watch};
use tokio::time::MissedTickBehavior;

use super::deploy::{
    DeployDriver, DeployDriverError, DeployPath, DeployPlacement, RedeployFlipEnd,
    cleanup_incomplete, observed_with,
};
use super::deploy_dispatch::{
    DISPATCH_EFFECT_BUDGET, DISPATCH_PULL_BUDGET, TargetDispatch, VerbScope,
};
use super::deploy_runtime::bounded_diagnostic;
use super::operation_evidence::{
    OperationEvidenceLog, PreparedDeployContainer, PreparedPromotion, PreparedRedeployIntent,
};
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
/// first-deploy defaults or the incumbent's own fields. Placement and pins
/// come from the deploy's effective placement, not from lineage.
struct ServiceLineage {
    namespace_id: NamespaceRowId,
    name: CorrosionServiceName,
    previous_image: Option<ImageReference>,
}

/// One replacement container this deploy started, bound to the machine that
/// runs it and the endpoint address its gate returned.
struct PlacedContainer {
    machine: MachineRowId,
    container_id: ContainerId,
    ip: Ipv4Addr,
}

/// One live service container observation bound to the machine reporting it.
struct MachineServiceContainer {
    machine: MachineRowId,
    container_id: ContainerId,
    /// The container's own recovered service row id: the incumbent's for a
    /// live container, or a failed first attempt's dead id for debris.
    service_id: ServiceRowId,
    deploy: OperationRowId,
    named_volumes: BTreeSet<VolumeName>,
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
    pub(super) placement: DeployPlacement,
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
        let dispatch = self.dispatch();
        let end = self.run_phases(&mut shutdown, &heartbeat, &dispatch).await;
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

    fn dispatch(&self) -> TargetDispatch {
        TargetDispatch::new(
            self.driver.machine_id.clone(),
            Arc::clone(&self.driver.runtime),
            self.driver.effect_timeout,
            Arc::clone(&self.driver.verbs),
            self.placement.addresses.clone(),
            VerbScope {
                operation_id: self.operation_id.clone(),
                namespace_id: self.namespace_id(),
                service_id: self.service_id.clone(),
            },
        )
    }

    fn namespace_id(&self) -> NamespaceRowId {
        match &self.path {
            DeployPath::First { namespace } | DeployPath::Redeploy { namespace, .. } => {
                namespace.id.clone()
            }
        }
    }

    /// The pick's distinct target machines, in pick order.
    fn distinct_targets(&self) -> Vec<MachineRowId> {
        let mut seen = BTreeSet::new();
        self.placement
            .targets
            .iter()
            .filter(|machine| seen.insert((*machine).clone()))
            .cloned()
            .collect()
    }

    async fn run_phases(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        heartbeat: &DeployHeartbeat,
        dispatch: &TargetDispatch,
    ) -> Result<DeployTaskEnd, DeployDriverError> {
        let end = match &self.path {
            DeployPath::First { namespace } => self.run_first(shutdown, dispatch, namespace).await,
            DeployPath::Redeploy {
                namespace,
                incumbent,
            } => {
                self.run_redeploy(shutdown, heartbeat, dispatch, namespace, incumbent)
                    .await
            }
        };
        match end {
            Ok(end) | Err(PhaseStop::End(end)) => Ok(end),
            Err(PhaseStop::Driver(error)) => Err(error),
        }
    }

    async fn run_first(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
        namespace: &ResolvedNamespace,
    ) -> Result<DeployTaskEnd, PhaseStop> {
        let mut warnings = Vec::new();
        // A first deploy has no incumbent: any existing container of this
        // service is a failed earlier attempt's debris, swept before new
        // work begins. Retained-for-inspection only shields a failing
        // operation's own containers until its next attempt starts.
        let containers = self.observe_service_containers(shutdown, dispatch).await?;
        let debris: Vec<MachineServiceContainer> = containers
            .into_iter()
            .filter(|container| container.deploy != self.operation_id)
            .collect();
        self.sweep_debris(shutdown, dispatch, &debris).await?;
        let image = self.acquire_image(shutdown, dispatch).await?;
        let identity = self.identity(&namespace.id);
        let created = self
            .create_targets(shutdown, dispatch, &image, namespace, &identity)
            .await?;
        let placed = self
            .start_and_gate(shutdown, dispatch, &created, &identity, &mut warnings)
            .await?;
        if let Err(error) = self
            .driver
            .routes
            .check(&namespace.id, &self.service_id)
            .await
        {
            return Err(PhaseStop::End(DeployTaskEnd::Failure {
                failure: error.into_deploy_failure(self.service_id.clone()),
            }));
        }
        let prepared = self.prepared_promotion(namespace, &placed, image)?;
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
        dispatch: &TargetDispatch,
        namespace: &ResolvedNamespace,
        incumbent: &ObservedService,
    ) -> Result<DeployTaskEnd, PhaseStop> {
        let mut warnings = Vec::new();
        self.takeover_boundary(shutdown, heartbeat).await?;
        let containers = self.observe_service_containers(shutdown, dispatch).await?;
        let (incumbents, debris): (Vec<_>, Vec<_>) = containers
            .into_iter()
            .partition(|container| container.deploy == incumbent.document.active_deploy);
        let debris: Vec<MachineServiceContainer> = debris
            .into_iter()
            .filter(|container| container.deploy != self.operation_id)
            .collect();
        let strategy = if incumbents
            .iter()
            .any(|container| !container.named_volumes.is_empty())
            || !self.request.runtime.volume_mounts.is_empty()
        {
            CutoverStrategy::StopFirst
        } else {
            CutoverStrategy::StartFirst
        };

        // Sweep is best-effort: debris that refuses to die stays for the next
        // deploy's sweep and never fails this operation.
        self.sweep_debris(shutdown, dispatch, &debris).await?;

        // The replacements are pulled and created before any incumbent is
        // touched.
        let image = self.acquire_image(shutdown, dispatch).await?;
        let identity = self.identity(&namespace.id);
        let created = self
            .create_targets(shutdown, dispatch, &image, namespace, &identity)
            .await?;

        let placed = match strategy {
            CutoverStrategy::StopFirst => {
                self.stop_first_cutover(
                    shutdown,
                    heartbeat,
                    dispatch,
                    &incumbents,
                    &created,
                    &identity,
                    &mut warnings,
                )
                .await?
            }
            CutoverStrategy::StartFirst => {
                self.start_and_gate(shutdown, dispatch, &created, &identity, &mut warnings)
                    .await?
            }
        };

        // The takeover check always runs immediately before the flip.
        self.takeover_boundary(shutdown, heartbeat).await?;
        let intent = self.prepared_redeploy_intent(incumbent, &placed, image)?;
        if let Err(error) = self
            .driver
            .routes
            .ensure(
                &intent.service_document.namespace_id,
                &self.service_id,
                intent.service_document.provenance.clone(),
            )
            .await
        {
            return Ok(DeployTaskEnd::Failure {
                failure: error.into_deploy_failure(self.service_id.clone()),
            });
        }
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
                    .stop_then_remove(
                        shutdown,
                        dispatch,
                        &incumbents,
                        CleanupEvidence::RemoveStopped,
                    )
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
                    .stop_then_remove(
                        shutdown,
                        dispatch,
                        &incumbents,
                        CleanupEvidence::StopThenRemove,
                    )
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

    /// Lists this service's live containers on every machine that reported
    /// them in a bid, plus every answering machine named by a container row.
    async fn observe_service_containers(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
    ) -> Result<Vec<MachineServiceContainer>, PhaseStop> {
        let mut machines: BTreeSet<MachineRowId> = self
            .placement
            .bid_service_containers
            .iter()
            .filter(|(_, containers)| !containers.is_empty())
            .map(|(machine, _)| machine.clone())
            .collect();
        match select_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver.store.service_containers(&self.service_id),
        )
        .await
        {
            EffectResult::Completed(Ok(rows)) => {
                for row in rows {
                    if self.placement.answered.contains(&row.document.machine_id) {
                        machines.insert(row.document.machine_id);
                    }
                }
            }
            EffectResult::Completed(Err(error)) => {
                return Err(PhaseStop::Driver(DeployDriverError::Promotion(error)));
            }
            EffectResult::Shutdown | EffectResult::TimedOut => {
                return Err(PhaseStop::End(DeployTaskEnd::Interrupted));
            }
        }
        let mut observed = Vec::new();
        for machine in machines {
            let containers = phase_effect(
                shutdown,
                DISPATCH_EFFECT_BUDGET,
                dispatch.service_containers(&machine),
                |message| CorrosionDeployServiceFailure::ContainerCreateFailed {
                    message: bounded_diagnostic(format!(
                        "could not list service containers on machine {machine}: {message}"
                    )),
                },
                PhaseTimeout::Interrupts,
            )
            .await?;
            observed.extend(
                containers
                    .into_iter()
                    .map(|container| MachineServiceContainer {
                        machine: machine.clone(),
                        container_id: container.container_id,
                        service_id: container.service_id,
                        deploy: container.deploy,
                        named_volumes: container.named_volumes,
                    }),
            );
        }
        Ok(observed)
    }

    /// Removes foreign debris wherever it was observed, deleting the matching
    /// rows and appending one `DebrisSwept` per machine that shed containers.
    async fn sweep_debris(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
        debris: &[MachineServiceContainer],
    ) -> Result<(), DeployDriverError> {
        let removed = self
            .stop_then_remove(shutdown, dispatch, debris, CleanupEvidence::Debris)
            .await?;
        if removed.is_empty() {
            return Ok(());
        }
        self.delete_container_rows(&removed).await;
        let mut by_machine: BTreeMap<MachineRowId, Vec<ContainerId>> = BTreeMap::new();
        for container in debris {
            if removed.contains(&container.container_id) {
                by_machine
                    .entry(container.machine.clone())
                    .or_default()
                    .push(container.container_id.clone());
            }
        }
        for (machine, removed) in by_machine {
            self.log
                .append_on(
                    self.now()?,
                    machine,
                    OperationEvidence::DebrisSwept { removed },
                )
                .await?;
        }
        Ok(())
    }

    /// Stop-first cutover for volume-holding incumbents: stop them, start the
    /// replacements, and gate them.
    ///
    /// The incumbent restart runs only for service failures. A shutdown
    /// interruption after the incumbent stop leaves the service down until
    /// the deploy is re-run: a restart launched during shutdown could not be
    /// awaited to completion, so the interrupted terminal outcome is the
    /// evidence instead.
    #[expect(
        clippy::too_many_arguments,
        reason = "the cutover names every phase collaborator it threads"
    )]
    async fn stop_first_cutover(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        heartbeat: &DeployHeartbeat,
        dispatch: &TargetDispatch,
        incumbents: &[MachineServiceContainer],
        created: &[(MachineRowId, ContainerId)],
        identity: &V2ManagedContainerIdentity,
        warnings: &mut Vec<CorrosionDeployWarning>,
    ) -> Result<Vec<PlacedContainer>, PhaseStop> {
        self.takeover_boundary(shutdown, heartbeat).await?;
        for container in incumbents {
            phase_effect(
                shutdown,
                DISPATCH_EFFECT_BUDGET,
                dispatch.stop_container(
                    &container.machine,
                    &container.container_id,
                    &self.identity_for(container),
                ),
                |message| CorrosionDeployServiceFailure::IncumbentStopFailed {
                    message: bounded_diagnostic(message),
                },
                PhaseTimeout::Fails(CorrosionDeployServiceFailure::IncumbentStopFailed {
                    message: "incumbent stop timed out".to_owned(),
                }),
            )
            .await?;
            self.log
                .append_on(
                    self.now()?,
                    container.machine.clone(),
                    OperationEvidence::IncumbentStopped {
                        container_id: container.container_id.clone(),
                    },
                )
                .await?;
        }
        match self
            .start_and_gate(shutdown, dispatch, created, identity, warnings)
            .await
        {
            Ok(placed) => Ok(placed),
            Err(PhaseStop::End(end)) => {
                // The failed replacement is retained for inspection; only
                // the incumbents are brought back.
                if matches!(end, DeployTaskEnd::ServiceFailure { .. }) {
                    self.restart_incumbents(shutdown, dispatch, incumbents)
                        .await?;
                }
                Err(PhaseStop::End(end))
            }
            Err(PhaseStop::Driver(error)) => Err(PhaseStop::Driver(error)),
        }
    }

    /// One best-effort stop/remove pass over `containers`, appending the
    /// evidence verbs the phase calls for. Containers that refuse to die stay
    /// for the next deploy's sweep and never fail the operation. Returns the
    /// ids removed from Docker.
    async fn stop_then_remove(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
        containers: &[MachineServiceContainer],
        evidence: CleanupEvidence,
    ) -> Result<Vec<ContainerId>, DeployDriverError> {
        let mut removed = Vec::new();
        for container in containers {
            let identity = self.identity_for(container);
            match evidence {
                CleanupEvidence::RemoveStopped => {}
                CleanupEvidence::Debris | CleanupEvidence::StopThenRemove => {
                    match select_effect(
                        shutdown,
                        DISPATCH_EFFECT_BUDGET,
                        dispatch.stop_container(
                            &container.machine,
                            &container.container_id,
                            &identity,
                        ),
                    )
                    .await
                    {
                        EffectResult::Completed(Ok(())) => {}
                        EffectResult::Completed(Err(_)) | EffectResult::TimedOut => continue,
                        // Cleanup is best-effort; shutdown keeps what was
                        // already removed and leaves the rest to the next
                        // deploy's sweep.
                        EffectResult::Shutdown => return Ok(removed),
                    }
                    if matches!(evidence, CleanupEvidence::StopThenRemove) {
                        self.log
                            .append_on(
                                self.now()?,
                                container.machine.clone(),
                                OperationEvidence::IncumbentStopped {
                                    container_id: container.container_id.clone(),
                                },
                            )
                            .await?;
                    }
                }
            }
            match select_effect(
                shutdown,
                DISPATCH_EFFECT_BUDGET,
                dispatch.remove_container(&container.machine, &container.container_id, &identity),
            )
            .await
            {
                EffectResult::Completed(Ok(())) => {}
                EffectResult::Completed(Err(_)) | EffectResult::TimedOut => continue,
                EffectResult::Shutdown => return Ok(removed),
            }
            if matches!(
                evidence,
                CleanupEvidence::RemoveStopped | CleanupEvidence::StopThenRemove
            ) {
                self.log
                    .append_on(
                        self.now()?,
                        container.machine.clone(),
                        OperationEvidence::IncumbentRemoved {
                            container_id: container.container_id.clone(),
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
    ) -> Result<(), PhaseStop> {
        if heartbeat.superseded() {
            return Err(PhaseStop::End(DeployTaskEnd::StopSilently));
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
            EffectResult::Completed(Err(error)) => {
                return Err(PhaseStop::Driver(DeployDriverError::Operation(error)));
            }
            EffectResult::Shutdown | EffectResult::TimedOut => {
                return Err(PhaseStop::End(DeployTaskEnd::Interrupted));
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
            return Err(PhaseStop::End(DeployTaskEnd::Failure {
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
                    return Err(PhaseStop::End(DeployTaskEnd::StopSilently));
                }
                *self.row.lock().await = current;
                Ok(())
            }
            Ok(Ok(None)) => Err(PhaseStop::End(DeployTaskEnd::StopSilently)),
            // A transient read failure never blocks a boundary; the flip CAS
            // remains the hard gate.
            Ok(Err(_)) | Err(_) => Ok(()),
        }
    }

    /// Resolves the image locally, then pulls it on every distinct target.
    async fn acquire_image(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
    ) -> Result<ImageReference, PhaseStop> {
        self.log
            .append(self.now()?, OperationEvidence::PullingImage)
            .await?;
        let image = phase_effect(
            shutdown,
            self.driver.effect_timeout,
            self.driver.runtime.resolve_image(&self.request.image),
            |message| CorrosionDeployServiceFailure::ImagePullFailed { message },
            PhaseTimeout::Interrupts,
        )
        .await?;
        for machine in self.distinct_targets() {
            let pull_shutdown = shutdown.clone();
            phase_effect(
                shutdown,
                DISPATCH_PULL_BUDGET,
                dispatch.pull_image(&machine, &image, pull_shutdown),
                |message| CorrosionDeployServiceFailure::ImagePullFailed {
                    message: bounded_diagnostic(format!(
                        "pull on machine {machine} failed: {message}"
                    )),
                },
                PhaseTimeout::Fails(CorrosionDeployServiceFailure::ImagePullFailed {
                    message: format!("image pull timed out on machine {machine}"),
                }),
            )
            .await?;
        }
        self.log
            .append(self.now()?, OperationEvidence::ImageResolved)
            .await?;
        Ok(image)
    }

    /// Creates one container per pick target entry; a machine picked for N
    /// replicas gets N containers.
    async fn create_targets(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
        image: &ImageReference,
        namespace: &ResolvedNamespace,
        identity: &V2ManagedContainerIdentity,
    ) -> Result<Vec<(MachineRowId, ContainerId)>, PhaseStop> {
        let host_ports = self.placement.host_ports();
        let mut created = Vec::new();
        for machine in &self.placement.targets {
            let container_id = phase_effect(
                shutdown,
                DISPATCH_EFFECT_BUDGET,
                dispatch.create_container(
                    machine,
                    &self.request,
                    image,
                    namespace,
                    identity.clone(),
                    &host_ports,
                ),
                |message| CorrosionDeployServiceFailure::ContainerCreateFailed {
                    message: bounded_diagnostic(format!(
                        "create on machine {machine} failed: {message}"
                    )),
                },
                PhaseTimeout::Interrupts,
            )
            .await?;
            self.log
                .append_on(
                    self.now()?,
                    machine.clone(),
                    OperationEvidence::ContainerCreated {
                        container_id: container_id.clone(),
                    },
                )
                .await?;
            created.push((machine.clone(), container_id));
        }
        Ok(created)
    }

    /// Starts and gates every created replacement, in creation order. A
    /// failed gate on any target fails the operation before the flip; the
    /// failed container is retained for inspection.
    async fn start_and_gate(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
        created: &[(MachineRowId, ContainerId)],
        identity: &V2ManagedContainerIdentity,
        warnings: &mut Vec<CorrosionDeployWarning>,
    ) -> Result<Vec<PlacedContainer>, PhaseStop> {
        let mut placed = Vec::new();
        for (machine, container_id) in created {
            phase_effect(
                shutdown,
                DISPATCH_EFFECT_BUDGET,
                dispatch.start_container(machine, container_id),
                |message| CorrosionDeployServiceFailure::ContainerStartFailed {
                    message: bounded_diagnostic(format!(
                        "start on machine {machine} failed: {message}"
                    )),
                },
                PhaseTimeout::Fails(CorrosionDeployServiceFailure::ContainerStartFailed {
                    message: format!("container start timed out on machine {machine}"),
                }),
            )
            .await?;
            self.log
                .append_on(
                    self.now()?,
                    machine.clone(),
                    OperationEvidence::ContainerStarted {
                        container_id: container_id.clone(),
                    },
                )
                .await?;
            let gate =
                dispatch.health_gate(machine, container_id, identity, self.request.health_gate);
            let ip = match self.request.health_gate {
                HealthGatePolicy::Enforce => {
                    phase_effect(
                        shutdown,
                        DISPATCH_EFFECT_BUDGET,
                        gate,
                        |message| CorrosionDeployServiceFailure::HealthGateFailed {
                            message: bounded_diagnostic(format!(
                                "health gate on machine {machine} failed: {message}"
                            )),
                        },
                        PhaseTimeout::Fails(CorrosionDeployServiceFailure::HealthGateFailed {
                            message: format!("health gate timed out on machine {machine}"),
                        }),
                    )
                    .await?
                }
                HealthGatePolicy::Skip => {
                    phase_effect(
                        shutdown,
                        DISPATCH_EFFECT_BUDGET,
                        gate,
                        |message| CorrosionDeployServiceFailure::ContainerStartFailed {
                            message: bounded_diagnostic(format!(
                                "endpoint lookup on machine {machine} failed: {message}"
                            )),
                        },
                        PhaseTimeout::Fails(CorrosionDeployServiceFailure::ContainerStartFailed {
                            message: format!(
                                "container endpoint lookup timed out on machine {machine}"
                            ),
                        }),
                    )
                    .await?
                }
            };
            placed.push(PlacedContainer {
                machine: machine.clone(),
                container_id: container_id.clone(),
                ip,
            });
        }
        match self.request.health_gate {
            HealthGatePolicy::Enforce => {}
            HealthGatePolicy::Skip => {
                self.log
                    .append(self.now()?, OperationEvidence::HealthGateSkipped)
                    .await?;
                warnings.push(CorrosionDeployWarning::HealthGateSkipped {
                    service_id: self.service_id.clone(),
                });
            }
        }
        Ok(placed)
    }

    async fn restart_incumbents(
        &self,
        shutdown: &mut watch::Receiver<bool>,
        dispatch: &TargetDispatch,
        incumbents: &[MachineServiceContainer],
    ) -> Result<(), DeployDriverError> {
        for container in incumbents {
            match select_effect(
                shutdown,
                DISPATCH_EFFECT_BUDGET,
                dispatch.start_container(&container.machine, &container.container_id),
            )
            .await
            {
                EffectResult::Completed(Ok(())) => {}
                // Restart is best-effort: the interrupted or failed terminal
                // outcome is the evidence for whatever stayed down.
                EffectResult::Completed(Err(_)) | EffectResult::TimedOut => continue,
                EffectResult::Shutdown => return Ok(()),
            }
            self.log
                .append_on(
                    self.now()?,
                    container.machine.clone(),
                    OperationEvidence::IncumbentRestarted {
                        container_id: container.container_id.clone(),
                    },
                )
                .await?;
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

    /// The Docker identity of an observed container, recovered from its own
    /// reported service row id and deploy operation so identity guards match
    /// exactly what was observed.
    fn identity_for(&self, container: &MachineServiceContainer) -> V2ManagedContainerIdentity {
        V2ManagedContainerIdentity {
            namespace_id: self.namespace_id(),
            service_id: container.service_id.clone(),
            operation_id: container.deploy.clone(),
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
    /// shares, written from this operation's identity at one timestamp. The
    /// service row carries the effective placement and pins; each container
    /// row names the machine it runs on and the address its gate returned.
    fn deploy_documents(
        &self,
        lineage: ServiceLineage,
        resolved_image: ImageReference,
        placed: &[PlacedContainer],
    ) -> Result<(ServiceDocument, Vec<PreparedDeployContainer>), DeployDriverError> {
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
            placement: self.placement.placement.clone(),
            pinned_machines: self.placement.pinned_machines.clone(),
            active_deploy: self.operation_id.clone(),
            previous_image: lineage.previous_image,
            deployed_at,
            operation_id: self.operation_id.clone(),
        };
        let containers = placed
            .iter()
            .map(|container| PreparedDeployContainer {
                id: container.container_id.clone(),
                document: ContainerDocument {
                    v: CorrosionDocumentVersion::V1,
                    cluster_id: self.driver.cluster_id.clone(),
                    machine_id: container.machine.clone(),
                    service_id: self.service_id.clone(),
                    namespace_id: lineage.namespace_id.clone(),
                    ip: container.ip,
                    deploy: self.operation_id.clone(),
                },
            })
            .collect();
        Ok((service_document, containers))
    }

    fn prepared_promotion(
        &self,
        namespace: &ResolvedNamespace,
        placed: &[PlacedContainer],
        resolved_image: ImageReference,
    ) -> Result<PreparedPromotion, DeployDriverError> {
        let (service_document, containers) = self.deploy_documents(
            ServiceLineage {
                namespace_id: namespace.id.clone(),
                name: self.request.service_name.clone(),
                previous_image: None,
            },
            resolved_image,
            placed,
        )?;
        Ok(PreparedPromotion {
            namespace_id: namespace.id.clone(),
            exact_namespace_document: namespace.exact_document.clone(),
            service_id: self.service_id.clone(),
            service_document,
            containers,
            success_result: CorrosionDeployServiceResult::completed(self.service_id.clone()),
        })
    }

    fn prepared_redeploy_intent(
        &self,
        incumbent: &ObservedService,
        placed: &[PlacedContainer],
        resolved_image: ImageReference,
    ) -> Result<PreparedRedeployIntent, DeployDriverError> {
        let (service_document, containers) = self.deploy_documents(
            ServiceLineage {
                namespace_id: incumbent.document.namespace_id.clone(),
                name: incumbent.document.name.clone(),
                previous_image: Some(incumbent.document.image.clone()),
            },
            resolved_image,
            placed,
        )?;
        Ok(PreparedRedeployIntent {
            service_id: self.service_id.clone(),
            exact_incumbent_document: incumbent.exact_document.clone(),
            service_document,
            containers,
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

/// Why a phase chain stopped early: a task-level end (failure, interruption,
/// supersession) that terminalizes normally, or a driver error that aborts
/// the task.
enum PhaseStop {
    End(DeployTaskEnd),
    Driver(DeployDriverError),
}

impl<Error> From<Error> for PhaseStop
where
    DeployDriverError: From<Error>,
{
    fn from(error: Error) -> Self {
        Self::Driver(DeployDriverError::from(error))
    }
}

/// How one dispatched phase effect classifies its timeout.
enum PhaseTimeout {
    /// The timeout ends the task as interrupted, like shutdown.
    Interrupts,
    /// The timeout fails the phase with this prebuilt failure.
    Fails(CorrosionDeployServiceFailure),
}

/// One dispatched Docker effect inside a phase: shutdown interrupts the
/// task, an error fails the phase with the caller's failure shape, and a
/// timeout is classified by `on_timeout`.
async fn phase_effect<T>(
    shutdown: &mut watch::Receiver<bool>,
    budget: Duration,
    effect: impl std::future::Future<Output = Result<T, String>>,
    on_error: impl FnOnce(String) -> CorrosionDeployServiceFailure,
    on_timeout: PhaseTimeout,
) -> Result<T, PhaseStop> {
    match select_effect(shutdown, budget, effect).await {
        EffectResult::Completed(Ok(value)) => Ok(value),
        EffectResult::Completed(Err(message)) => {
            Err(PhaseStop::End(DeployTaskEnd::ServiceFailure {
                failure: on_error(message),
            }))
        }
        EffectResult::TimedOut => match on_timeout {
            PhaseTimeout::Fails(failure) => {
                Err(PhaseStop::End(DeployTaskEnd::ServiceFailure { failure }))
            }
            PhaseTimeout::Interrupts => Err(PhaseStop::End(DeployTaskEnd::Interrupted)),
        },
        EffectResult::Shutdown => Err(PhaseStop::End(DeployTaskEnd::Interrupted)),
    }
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
