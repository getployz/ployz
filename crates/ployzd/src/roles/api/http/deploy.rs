use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ployz_core::corrosion::{
    CorrosionDeployFailure, CorrosionDeployOutcome, CorrosionDeployServiceResult,
    CorrosionDeployTargets, CorrosionDeployTransition, CorrosionDeployWarning,
    CorrosionDocumentVersion, CorrosionPromotionFailure, CorrosionTimestamp, DeployTakeover,
    HostPortBindings, OperationDocument, OperationInitiator, ServicePlacement, ServiceReplicaCount,
    check_deploy_takeover,
};
use ployz_core::deploy::VolumeName;
use ployz_core::ids::{ClusterId, MachineRowId, OperationRowId, ServiceRowId};
use ployz_core::placement::{PlacementPick, PlacementPickInputs, pick_placement};
use ployz_core::{
    DeployAccepted, DeployRefusal, DeployRequest, HealthGatePolicy, OperationEvidence,
    PlacementBidRequest, RequestedPins, RequestedPlacement, ServiceContainerObservation,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Mutex;

use crate::roles::DNS_TTL_SECONDS;

use super::deploy_dispatch::{DeployVerbClient, machine_socket_addr};
use super::deploy_runtime::DeployRuntime;
use super::deploy_stores::{DeployOperationRows, DeployStore};
use super::deploy_task::{AcceptedDeploy, DeployTask};
use super::operation_evidence::{
    DurablePromotionProgress, EvidenceIdentity, OperationEvidenceDirectory, OperationEvidenceLog,
    PreparedRedeployIntent,
};
use super::operation_finalizer::{
    PromotionFinalizerDecision, PromotionFinalizerState, PromotionFinalizerStoreError,
    PromotionRequestDisposition, PromotionRowsObservation, RedeployRowsDecision,
    UncertaintyDeadline, classify_redeploy_rows,
};
use super::operation_store::{ConditionalOperationWrite, ObservedOperation, OperationStoreError};
use super::placement_gather::{PlacementMesh, PlacementPeer, gather_bids};
use super::promotion_store::{DeployAdmission, ObservedService, ResolvedNamespace};

const EXTERNAL_EFFECT_TIMEOUT: Duration = Duration::from_secs(60);
const FINALIZER_ATTEMPTS: usize = 3;

/// Waits for rival claim rows to gossip in before adjudicating the op claim.
const CLAIM_COURTESY_WAIT: Duration = Duration::from_secs(1);

/// Old-revision drain after a stateless flip: one full DNS TTL plus a second
/// of skew, so cached answers stop pointing at the incumbent before it stops.
pub(super) const DEPLOY_DRAIN_WAIT: Duration = Duration::from_secs(DNS_TTL_SECONDS as u64 + 1);

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

pub(super) fn observed_with(
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
    pub(super) cluster_id: ClusterId,
    pub(super) machine_id: MachineRowId,
    evidence: OperationEvidenceDirectory,
    pub(super) store: Arc<dyn DeployStore>,
    pub(super) operations: Arc<dyn DeployOperationRows>,
    pub(super) runtime: Arc<dyn DeployRuntime>,
    pub(super) mesh: Arc<dyn PlacementMesh>,
    pub(super) verbs: Arc<dyn DeployVerbClient>,
    pub(super) api_port: u16,
    pub(super) clock: Arc<dyn DeployClock>,
    pub(super) effect_timeout: Duration,
    pub(super) claim_courtesy_wait: Duration,
    pub(super) drain_wait: Duration,
}

/// The placement a deploy task executes: the effective service intent, the
/// picked target order, and the gather's live picture of the roster.
#[derive(Clone)]
pub(super) struct DeployPlacement {
    /// The effective placement written to the services row at the flip:
    /// the request's, or the incumbent row's when the deploy carried none.
    pub(super) placement: ServicePlacement,
    /// The effective pin set written to the services row at the flip.
    pub(super) pinned_machines: BTreeSet<MachineRowId>,
    /// The pick's target order; a machine appears once per replica it hosts.
    pub(super) targets: Vec<MachineRowId>,
    /// Mesh addresses for every roster machine, from the admission roster.
    pub(super) addresses: BTreeMap<MachineRowId, SocketAddr>,
    /// Service containers each bidding machine reported from live Docker.
    pub(super) bid_service_containers: BTreeMap<MachineRowId, Vec<ServiceContainerObservation>>,
    /// Machines that answered the bid fan-out.
    pub(super) answered: BTreeSet<MachineRowId>,
}

impl DeployPlacement {
    /// Host-published ports for this deploy's containers: the global
    /// variant's bindings, empty for every replicated deploy.
    pub(super) fn host_ports(&self) -> HostPortBindings {
        match &self.placement {
            ServicePlacement::Global { host_ports } => host_ports.clone(),
            ServicePlacement::Replicated { replicas: _ } => HostPortBindings::default(),
        }
    }
}

/// Which admission case this operation is executing.
pub(super) enum DeployPath {
    First {
        namespace: ResolvedNamespace,
    },
    Redeploy {
        namespace: ResolvedNamespace,
        incumbent: Box<ObservedService>,
    },
}

/// The flip verdict shared by the live task and startup resume.
pub(super) enum RedeployFlipEnd {
    Committed,
    Superseded { winner: OperationRowId },
    Failure { failure: CorrosionPromotionFailure },
}

/// The construction-time collaborators of a deploy driver, grouped so the
/// constructor names each seam once.
pub(super) struct DeployDriverSeams {
    pub(super) evidence: OperationEvidenceDirectory,
    pub(super) store: Arc<dyn DeployStore>,
    pub(super) operations: Arc<dyn DeployOperationRows>,
    pub(super) runtime: Arc<dyn DeployRuntime>,
    pub(super) mesh: Arc<dyn PlacementMesh>,
    pub(super) verbs: Arc<dyn DeployVerbClient>,
    pub(super) api_port: u16,
    pub(super) clock: Arc<dyn DeployClock>,
}

impl DeployDriver {
    #[must_use]
    pub(super) fn new(
        cluster_id: ClusterId,
        machine_id: MachineRowId,
        seams: DeployDriverSeams,
    ) -> Self {
        let DeployDriverSeams {
            evidence,
            store,
            operations,
            runtime,
            mesh,
            verbs,
            api_port,
            clock,
        } = seams;
        Self {
            cluster_id,
            machine_id,
            evidence,
            store,
            operations,
            runtime,
            mesh,
            verbs,
            api_port,
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
            } => DeployPath::Redeploy {
                namespace,
                incumbent,
            },
        };
        if !tokio::time::timeout(self.effect_timeout, self.runtime.bridge_ready())
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
        let (placement, gathered, pick) = match self
            .derive_placement(&request, &path, &namespace_id, &service_id)
            .await?
        {
            Ok(derived) => derived,
            Err(refusal) => return Ok(Err(refusal)),
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
        log.append(
            self.clock.now().map_err(DeployDriverError::Clock)?,
            OperationEvidence::PlacementGathered {
                bids: gathered.bids,
                silent: gathered.silent,
            },
        )
        .await?;
        log.append(
            self.clock.now().map_err(DeployDriverError::Clock)?,
            OperationEvidence::PlacementPicked { pick },
        )
        .await?;
        let row = observed_with(&operation_id, operation.clone())?;
        self.operations
            .insert_created(&operation_id, &operation)
            .await?;
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
                placement,
                log,
                row: Arc::new(Mutex::new(row)),
            },
        }))
    }

    /// Gathers live bids over the roster, resolves the request's placement
    /// intent against the incumbent row, and runs the deterministic pick.
    /// Every refusal here happens before any operation or Docker effect.
    async fn derive_placement(
        &self,
        request: &DeployRequest,
        path: &DeployPath,
        namespace_id: &ployz_core::ids::NamespaceRowId,
        service_id: &ServiceRowId,
    ) -> Result<
        Result<
            (
                DeployPlacement,
                super::placement_gather::GatheredBids,
                PlacementPick,
            ),
            DeployRefusal,
        >,
        DeployDriverError,
    > {
        let peers = tokio::time::timeout(self.effect_timeout, self.mesh.roster())
            .await
            .map_err(|_| DeployDriverError::Placement("roster read timed out".to_owned()))?
            .map_err(DeployDriverError::Placement)?;
        let pinned_machines = match resolve_pins(&request.machines, &peers, path) {
            Ok(pins) => pins,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let placement = match effective_placement(request, path)? {
            Ok(placement) => placement,
            Err(refusal) => return Ok(Err(refusal)),
        };
        let volumes: BTreeSet<VolumeName> = request
            .runtime
            .volume_mounts
            .iter()
            .map(|mount| mount.volume_name.clone())
            .collect();
        let active_deploy = match path {
            DeployPath::First { .. } => None,
            DeployPath::Redeploy { incumbent, .. } => {
                Some(incumbent.document.active_deploy.clone())
            }
        };
        let bid_request = PlacementBidRequest {
            namespace_id: namespace_id.clone(),
            service_id: service_id.clone(),
            volumes: volumes.clone(),
            active_deploy: active_deploy.clone(),
        };
        let gathered = tokio::time::timeout(
            self.effect_timeout,
            gather_bids(self.mesh.as_ref(), &peers, &self.machine_id, &bid_request),
        )
        .await
        .map_err(|_| DeployDriverError::Placement("bid gather timed out".to_owned()))?;
        let row_machines = match path {
            DeployPath::First { .. } => BTreeSet::new(),
            DeployPath::Redeploy { .. } => {
                let rows = tokio::time::timeout(
                    self.effect_timeout,
                    self.store.service_containers(service_id),
                )
                .await
                .map_err(|_| {
                    DeployDriverError::Placement("service container rows timed out".to_owned())
                })??;
                rows.into_iter()
                    .map(|row| row.document.machine_id)
                    .collect()
            }
        };
        let inputs = PlacementPickInputs {
            placement: placement.clone(),
            pinned_machines: pinned_machines.clone(),
            volumes,
            plausible_volume_holders: row_machines.union(&pinned_machines).cloned().collect(),
            active_deploy,
            bids: gathered.bids.clone(),
            silent_machines: gathered
                .silent
                .iter()
                .filter_map(|silent| {
                    peers
                        .iter()
                        .find(|peer| peer.id == silent.machine_id)
                        .map(|peer| (silent.machine_id.clone(), peer.name.clone()))
                })
                .collect(),
        };
        let pick = match pick_placement(&inputs) {
            Ok(pick) => pick,
            Err(refusal) => return Ok(Err(DeployRefusal::Placement { refusal })),
        };
        let addresses = peers
            .iter()
            .map(|peer| {
                (
                    peer.id.clone(),
                    machine_socket_addr(&peer.transport, self.api_port),
                )
            })
            .collect();
        let bid_service_containers = gathered
            .bids
            .iter()
            .map(|bid| (bid.machine_id.clone(), bid.service_containers.clone()))
            .collect();
        let answered = gathered
            .bids
            .iter()
            .map(|bid| bid.machine_id.clone())
            .collect();
        Ok(Ok((
            DeployPlacement {
                placement,
                pinned_machines,
                targets: pick.targets.clone(),
                addresses,
                bid_service_containers,
                answered,
            },
            gathered,
            pick,
        )))
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
            .await?
            .ok_or_else(|| DeployDriverError::Invariant("operation row disappeared".to_owned()))?;
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
                // next deploy's sweep collects whatever this one left behind,
                // and the outcome carries the cleanup warning.
                let outcome = redeploy_completed_outcome(
                    &prepared,
                    vec![cleanup_incomplete(
                        "resumed after the committed flip; drain and cleanup were skipped",
                    )],
                )?;
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
                let outcome = redeploy_completed_outcome(
                    &prepared,
                    vec![cleanup_incomplete(
                        "resumed after the committed flip; drain and cleanup were skipped",
                    )],
                )?;
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
    pub(super) async fn converge_redeploy(
        &self,
        prepared: &PreparedRedeployIntent,
        operation_id: &OperationRowId,
        service_id: &ServiceRowId,
    ) -> Result<RedeployFlipEnd, DeployDriverError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let decision = match tokio::time::timeout(
                self.effect_timeout,
                self.store.converge_redeploy_rows(prepared),
            )
            .await
            {
                Ok(Ok((disposition, rows))) => {
                    classify_redeploy_rows(disposition, rows, finalizer_deadline(attempts))
                }
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
                    let newer = match tokio::time::timeout(
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

    pub(super) async fn finish_promotion(
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
                    match tokio::time::timeout(
                        self.effect_timeout,
                        self.store.converge_rows(prepared),
                    )
                    .await
                    {
                        Ok(Ok((disposition, rows))) => state.clone().observe_prepared_rows(
                            disposition,
                            rows,
                            finalizer_deadline(attempts),
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
                    match tokio::time::timeout(
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
                    match tokio::time::timeout(
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
                        OperationEvidence::ServiceClaimWon,
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
                        OperationEvidence::ServiceClaimLost { winner },
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
    pub(super) async fn terminalize(
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
            .transition_deploy(row.clone(), transition)
            .await?
        {
            ConditionalOperationWrite::Written => {
                *row = observed_with(&row.id, terminal)?;
                Ok(())
            }
            ConditionalOperationWrite::Stale => Err(DeployDriverError::Invariant(
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

/// The effective placement: the request's intent resolved against the
/// incumbent row, or the incumbent's placement unchanged when the deploy
/// carried none. A first deploy without intent runs one replica.
fn effective_placement(
    request: &DeployRequest,
    path: &DeployPath,
) -> Result<Result<ServicePlacement, DeployRefusal>, DeployDriverError> {
    let one_replica = || {
        ServiceReplicaCount::try_new(1)
            .map_err(|error| DeployDriverError::Invariant(error.to_string()))
    };
    match (&request.placement, path) {
        // The mode-preserving count change: legal against a replicated
        // incumbent or a first deploy, refused against a global incumbent
        // instead of silently unpublishing its host ports.
        (
            Some(RequestedPlacement::Replicas { replicas }),
            DeployPath::Redeploy { incumbent, .. },
        ) => match &incumbent.document.placement {
            ServicePlacement::Replicated { replicas: _ } => Ok(Ok(ServicePlacement::Replicated {
                replicas: *replicas,
            })),
            ServicePlacement::Global { host_ports: _ } => {
                Ok(Err(DeployRefusal::ReplicasOnGlobalService))
            }
        },
        (Some(RequestedPlacement::Replicas { replicas }), DeployPath::First { .. }) => {
            Ok(Ok(ServicePlacement::Replicated {
                replicas: *replicas,
            }))
        }
        (
            Some(RequestedPlacement::Replicated {
                replicas: Some(replicas),
            }),
            _,
        ) => Ok(Ok(ServicePlacement::Replicated {
            replicas: *replicas,
        })),
        // A count-free replicated intent keeps a replicated incumbent's
        // count; a first deploy or a global conversion defaults to one.
        (
            Some(RequestedPlacement::Replicated { replicas: None }),
            DeployPath::Redeploy { incumbent, .. },
        ) => match &incumbent.document.placement {
            ServicePlacement::Replicated { replicas } => Ok(Ok(ServicePlacement::Replicated {
                replicas: *replicas,
            })),
            ServicePlacement::Global { host_ports: _ } => Ok(Ok(ServicePlacement::Replicated {
                replicas: one_replica()?,
            })),
        },
        (Some(RequestedPlacement::Replicated { replicas: None }), DeployPath::First { .. }) => {
            Ok(Ok(ServicePlacement::Replicated {
                replicas: one_replica()?,
            }))
        }
        (Some(RequestedPlacement::Global { host_ports }), _) => Ok(Ok(ServicePlacement::Global {
            host_ports: host_ports.clone(),
        })),
        (None, DeployPath::Redeploy { incumbent, .. }) => {
            Ok(Ok(incumbent.document.placement.clone()))
        }
        (None, DeployPath::First { .. }) => Ok(Ok(ServicePlacement::Replicated {
            replicas: one_replica()?,
        })),
    }
}

/// The effective pin set: requested names resolved to roster row ids,
/// `--machine any` clearing the set, or the incumbent row's pins unchanged.
fn resolve_pins(
    machines: &Option<RequestedPins>,
    peers: &[PlacementPeer],
    path: &DeployPath,
) -> Result<BTreeSet<MachineRowId>, DeployRefusal> {
    match machines {
        Some(RequestedPins::Machines { names }) => names
            .iter()
            .map(|name| {
                peers
                    .iter()
                    .find(|peer| &peer.name == name)
                    .map(|peer| peer.id.clone())
                    .ok_or_else(|| DeployRefusal::UnknownPinnedMachine {
                        machine_name: name.clone(),
                    })
            })
            .collect(),
        Some(RequestedPins::Any) => Ok(BTreeSet::new()),
        None => match path {
            DeployPath::First { .. } => Ok(BTreeSet::new()),
            DeployPath::Redeploy { incumbent, .. } => {
                Ok(incumbent.document.pinned_machines.clone())
            }
        },
    }
}

/// One shared deadline convention for every bounded finalizer loop.
fn finalizer_deadline(attempts: usize) -> UncertaintyDeadline {
    if attempts >= FINALIZER_ATTEMPTS {
        UncertaintyDeadline::Reached
    } else {
        UncertaintyDeadline::Open
    }
}

pub(super) fn cleanup_incomplete(detail: &str) -> CorrosionDeployWarning {
    CorrosionDeployWarning::CleanupIncomplete {
        detail: detail.to_owned(),
    }
}

fn redeploy_completed_outcome(
    prepared: &PreparedRedeployIntent,
    mut warnings: Vec<CorrosionDeployWarning>,
) -> Result<CorrosionDeployOutcome, DeployDriverError> {
    let results = vec![CorrosionDeployServiceResult::completed(
        prepared.service_id.clone(),
    )];
    match prepared.health_gate {
        HealthGatePolicy::Enforce => {}
        HealthGatePolicy::Skip => warnings.insert(
            0,
            CorrosionDeployWarning::HealthGateSkipped {
                service_id: prepared.service_id.clone(),
            },
        ),
    }
    if warnings.is_empty() {
        CorrosionDeployOutcome::completed(results)
    } else {
        CorrosionDeployOutcome::completed_with_warnings(results, warnings)
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

#[derive(Debug, thiserror::Error)]
pub(super) enum DeployDriverError {
    #[error("operation persistence failed: {0}")]
    Operation(#[from] OperationStoreError),
    #[error("operation evidence failed: {0}")]
    Evidence(#[from] super::operation_evidence::OperationEvidenceError),
    #[error("promotion storage failed: {0}")]
    Promotion(#[from] PromotionFinalizerStoreError),
    #[error("timestamp creation failed: {0}")]
    Clock(String),
    #[error("placement derivation failed: {0}")]
    Placement(String),
    #[error("deploy invariant failed: {0}")]
    Invariant(String),
}

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;
