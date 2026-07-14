//! Canonical ingress endpoint projection owner.

use std::net::IpAddr;
use std::time::Duration;

use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use ployz_core::ids::MachineId;
use ployz_core::ingress::{
    IngressEndpointProjection, IngressEndpointProjectionIdentity, IngressEndpointProjectionState,
    IngressEndpointSet, IngressEndpointUnavailableReason,
};
use ployz_core::ops::{
    FailureMessage, IngressRefreshCandidateEvidence, IngressRefreshCandidatePublication,
    IngressRefreshEvidence, IngressRefreshExclusionReason, IngressRefreshFactsOutcome,
    IngressRefreshFailure, IngressRefreshGatewayOutcome, IngressRefreshInvalidationEvidence,
    IngressRefreshOperationState, IngressRefreshTransition, OperationStatus,
};
use ployz_core::reachability::is_public;
use ployz_core::roles::GatewayRole;
use ployz_core::state::{ControlPlaneEpoch, GatewayServingStatus, IntentSnapshot};
use ployz_core::subjects::{INGRESS_ENDPOINT_CHANGED, INTENT_CHANGED};
use ployz_nats::service_runtime::{
    NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError, NatsServiceShutdownError,
    RunningNatsService, decode_json_request, start_nats_service,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::intent::ingress_intent::{IngressProjectionStore, IngressProjectionWrite};
use crate::intent::service::NatsIntentReader;
use crate::machine_runtime::MachineRuntimeUnavailableReason;
use crate::operations::log::{IngressRefreshOperationSubmission, OperationRepository};
use crate::roles::gateway::client::{GatewayStatusReadError, NatsGatewayStatusReader};
use crate::roles::machine::client::{MachineFactsReadError, NatsMachineFactsReader};
use crate::service_catalog::{ingress_endpoint_get_spec, ingress_endpoint_service};

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const GATHER_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionEvidenceRecord {
    pub projection: IngressEndpointProjection,
    pub candidate_outcomes: Vec<IngressRefreshCandidateEvidence>,
    pub publishable_gateway_ids: Vec<MachineId>,
}

impl ProjectionEvidenceRecord {
    fn pending(control_plane_epoch: ControlPlaneEpoch) -> Self {
        Self {
            projection: IngressEndpointProjection {
                control_plane_epoch,
                revision: 0,
                state: IngressEndpointProjectionState::Pending,
            },
            candidate_outcomes: Vec::new(),
            publishable_gateway_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IngressEndpointGetRequest {}

pub struct RunningIngressEndpointProjection {
    service: RunningNatsService,
    shutdown: broadcast::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningIngressEndpointProjection {
    pub async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
        self.service.shutdown().await
    }
}

pub async fn start_ingress_endpoint_projection(
    client: async_nats::Client,
    store: IngressProjectionStore,
    control_plane_epoch: ControlPlaneEpoch,
    operations: OperationRepository,
) -> Result<RunningIngressEndpointProjection, IngressEndpointStartError> {
    ensure_epoch_projection(&store, control_plane_epoch).await?;
    recover_unfinished_refreshes(&operations).await?;
    let mut service = start_nats_service(client.clone(), &ingress_endpoint_service())
        .await
        .map_err(IngressEndpointStartError::Service)?;
    let endpoint = ingress_endpoint_get_spec();
    let query_store = store.clone();
    service
        .bind_endpoint(&endpoint, move |request| {
            let store = query_store.clone();
            async move { handle_get(store, request).await }
        })
        .await
        .map_err(IngressEndpointStartError::Service)?;
    let changed = client.subscribe(INTENT_CHANGED).await.map_err(|error| {
        IngressEndpointStartError::Subscribe {
            message: error.to_string(),
        }
    })?;
    let (shutdown, _) = broadcast::channel(1);
    let task_shutdown = shutdown.subscribe();
    let task = tokio::spawn(run_projection_loop(
        ProjectionRuntime {
            client: client.clone(),
            store,
            intent: NatsIntentReader::new(client.clone()).with_request_timeout(GATHER_DEADLINE),
            gateway: NatsGatewayStatusReader::new(client.clone())
                .with_request_timeout(GATHER_DEADLINE),
            facts: NatsMachineFactsReader::new(client).with_request_timeout(GATHER_DEADLINE),
            operations,
        },
        changed,
        task_shutdown,
    ));
    Ok(RunningIngressEndpointProjection {
        service,
        shutdown,
        task,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum IngressEndpointStartError {
    #[error("initialize ingress endpoint projection: {0}")]
    Store(crate::core_store::CoreStoreError),
    #[error("start ingress endpoint service: {0}")]
    Service(NatsServiceRuntimeError),
    #[error("subscribe ingress endpoint intent changes: {message}")]
    Subscribe { message: String },
    #[error("recover interrupted ingress endpoint refreshes: {message}")]
    Recovery { message: String },
}

async fn recover_unfinished_refreshes(
    operations: &OperationRepository,
) -> Result<(), IngressEndpointStartError> {
    let statuses = operations.operation_statuses().await.map_err(|error| {
        IngressEndpointStartError::Recovery {
            message: error.to_string(),
        }
    })?;
    for id in unfinished_refresh_ids(statuses) {
        operations
            .record_ingress_refresh_transition(
                &id,
                IngressRefreshTransition::Failed {
                    failure: IngressRefreshFailure::Interrupted {
                        message: failure_message(
                            "ingress endpoint task restarted before terminal evidence",
                        ),
                    },
                },
            )
            .await
            .map_err(|error| IngressEndpointStartError::Recovery {
                message: error.to_string(),
            })?;
    }
    Ok(())
}

fn unfinished_refresh_ids(statuses: Vec<OperationStatus>) -> Vec<ployz_core::ids::OperationId> {
    statuses
        .into_iter()
        .filter_map(|status| match status {
            OperationStatus::IngressRefresh {
                id,
                state: IngressRefreshOperationState::Accepted,
                ..
            } => Some(id),
            OperationStatus::IngressRefresh {
                state:
                    IngressRefreshOperationState::Completed { .. }
                    | IngressRefreshOperationState::Failed { .. },
                ..
            }
            | OperationStatus::Deploy { .. }
            | OperationStatus::Cert { .. }
            | OperationStatus::MachineAdd { .. }
            | OperationStatus::MachineUpdate { .. }
            | OperationStatus::MachineLifecycle { .. }
            | OperationStatus::CoreReplace { .. }
            | OperationStatus::CredentialGrant { .. }
            | OperationStatus::NetworkRepair { .. }
            | OperationStatus::ServiceRestart { .. }
            | OperationStatus::ManagedDnsReconcile { .. }
            | OperationStatus::IngressConfigure { .. }
            | OperationStatus::NamespaceRemove { .. }
            | OperationStatus::VolumeRemove { .. } => None,
        })
        .collect()
}

async fn ensure_epoch_projection(
    store: &IngressProjectionStore,
    epoch: ControlPlaneEpoch,
) -> Result<(), IngressEndpointStartError> {
    let current = store
        .load()
        .await
        .map_err(IngressEndpointStartError::Store)?;
    if current
        .as_ref()
        .is_some_and(|record| record.projection.control_plane_epoch == epoch)
    {
        return Ok(());
    }
    let expected = current.as_ref().map(|record| record.projection.identity());
    let _ = store
        .compare_and_replace(expected, ProjectionEvidenceRecord::pending(epoch))
        .await
        .map_err(IngressEndpointStartError::Store)?;
    Ok(())
}

async fn handle_get(
    store: IngressProjectionStore,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    if let Err(response) = decode_json_request::<IngressEndpointGetRequest>(&request) {
        return response;
    }
    match store.load().await {
        Ok(Some(record)) => NatsServiceResponse::json_ok(&record.projection),
        Ok(None) => NatsServiceResponse::transport_error(
            ployz_nats::service_protocol::NatsServiceError::unavailable(
                "ingress endpoint projection is not initialized",
            ),
        ),
        Err(error) => NatsServiceResponse::transport_error(
            ployz_nats::service_protocol::NatsServiceError::internal(error.to_string()),
        ),
    }
}

struct ProjectionRuntime {
    client: async_nats::Client,
    store: IngressProjectionStore,
    intent: NatsIntentReader,
    gateway: NatsGatewayStatusReader,
    facts: NatsMachineFactsReader,
    operations: OperationRepository,
}

async fn run_projection_loop(
    runtime: ProjectionRuntime,
    mut intent_changed: async_nats::Subscriber,
    mut shutdown: broadcast::Receiver<()>,
) {
    let mut interval = tokio::time::interval(REFRESH_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            changed = intent_changed.next() => {
                if changed.is_none() { break; }
            }
            _ = shutdown.recv() => break,
        }
        while intent_changed.next().now_or_never().flatten().is_some() {}
        let _ = runtime.refresh().await;
    }
}

impl ProjectionRuntime {
    async fn refresh(
        &self,
    ) -> Result<Option<IngressEndpointProjectionIdentity>, IngressEndpointRefreshError> {
        let operation_id =
            ployz_core::ids::OperationId::try_new(format!("op_ingress_refresh_{}", nuid::next()))
                .expect("NUID operation id is valid");
        self.operations
            .submit_ingress_refresh(IngressRefreshOperationSubmission {
                operation_id: operation_id.clone(),
            })
            .await
            .map_err(|_| IngressEndpointRefreshError::OperationStore)?;
        let result = self.refresh_inner().await;
        let (identity, transition) = match result {
            Ok((identity, mut evidence)) => {
                // Projection invalidation follows the projection commit and precedes terminal
                // evidence, so an operation-log outage cannot hide the committed identity.
                evidence.invalidation = match publish_changed(&self.client, identity).await {
                    Ok(()) => IngressRefreshInvalidationEvidence::Published,
                    Err(error) => IngressRefreshInvalidationEvidence::Failed {
                        message: bounded_failure_message(
                            "publish ingress.endpoint.changed",
                            &error,
                        ),
                    },
                };
                (
                    Some(identity),
                    IngressRefreshTransition::Completed { evidence },
                )
            }
            Err(error) => (
                None,
                IngressRefreshTransition::Failed {
                    failure: error.operation_failure(),
                },
            ),
        };
        self.operations
            .record_ingress_refresh_transition(&operation_id, transition)
            .await
            .map_err(|_| IngressEndpointRefreshError::OperationStore)?;
        Ok(identity)
    }

    async fn refresh_inner(
        &self,
    ) -> Result<
        (IngressEndpointProjectionIdentity, IngressRefreshEvidence),
        IngressEndpointRefreshError,
    > {
        let intent = self
            .intent
            .intent()
            .await
            .map_err(|_| IngressEndpointRefreshError::IntentUnavailable)?;
        let candidates = gateway_candidates(&intent);
        let outcomes = self.gather(candidates).await;
        let gathered_evidence = outcomes.clone();
        let previous = self
            .store
            .load()
            .await
            .map_err(|source| IngressEndpointRefreshError::Store {
                source,
                candidates: gathered_evidence.clone(),
            })?
            .ok_or_else(|| IngressEndpointRefreshError::ProjectionMissing {
                candidates: gathered_evidence,
            })?;
        let next = project_refresh(&previous, outcomes);
        let evidence = refresh_evidence(&previous, &next);
        let expected = Some(previous.projection.identity());
        let identity = next.projection.identity();
        match self
            .store
            .compare_and_replace(expected, next)
            .await
            .map_err(|source| IngressEndpointRefreshError::Store {
                source,
                candidates: evidence.candidates.clone(),
            })? {
            IngressProjectionWrite::Stored | IngressProjectionWrite::Unchanged => {
                Ok((identity, evidence))
            }
            IngressProjectionWrite::Conflict { .. } => {
                Err(IngressEndpointRefreshError::ConcurrentWriter {
                    candidates: evidence.candidates,
                })
            }
        }
    }

    async fn gather(&self, candidates: Vec<MachineId>) -> Vec<IngressRefreshCandidateEvidence> {
        let mut gathered = candidates
            .iter()
            .cloned()
            .map(|machine_id| (machine_id, (None, None)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut requests = FuturesUnordered::new();
        for machine_id in candidates {
            let gateway_machine_id = machine_id.clone();
            requests.push(
                async move {
                    let result = self.gateway.read(&gateway_machine_id).await;
                    GatherReply::Gateway {
                        machine_id: gateway_machine_id,
                        result,
                    }
                }
                .boxed(),
            );
            requests.push(
                async move {
                    let result = self.facts.machine_facts(&machine_id).await;
                    GatherReply::Facts { machine_id, result }
                }
                .boxed(),
            );
        }
        let deadline = tokio::time::sleep(GATHER_DEADLINE);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                reply = requests.next(), if !requests.is_empty() => {
                    let Some(reply) = reply else { break; };
                    match reply {
                        GatherReply::Gateway { machine_id, result } => {
                            if let Some((gateway, _)) = gathered.get_mut(&machine_id) {
                                *gateway = Some(result);
                            }
                        }
                        GatherReply::Facts { machine_id, result } => {
                            if let Some((_, facts)) = gathered.get_mut(&machine_id) {
                                *facts = Some(result);
                            }
                        }
                    }
                }
                _ = &mut deadline => break,
            }
        }
        gathered
            .into_iter()
            .map(|(machine_id, (gateway, facts))| {
                candidate_outcome(
                    machine_id.clone(),
                    gateway.unwrap_or_else(|| {
                        Err(GatewayStatusReadError {
                            machine_id: machine_id.clone(),
                            reason: MachineRuntimeUnavailableReason::RequestTimedOut,
                        })
                    }),
                    facts.unwrap_or({
                        Err(MachineFactsReadError::Unavailable {
                            machine_id,
                            reason: MachineRuntimeUnavailableReason::RequestTimedOut,
                        })
                    }),
                )
            })
            .collect()
    }
}

enum GatherReply {
    Gateway {
        machine_id: MachineId,
        result: Result<ployz_core::state::GatewayStatusObservation, GatewayStatusReadError>,
    },
    Facts {
        machine_id: MachineId,
        result: Result<ployz_core::machine_runtime::MachineFactsSnapshot, MachineFactsReadError>,
    },
}

#[derive(Debug, thiserror::Error)]
enum IngressEndpointRefreshError {
    #[error("intent unavailable")]
    IntentUnavailable,
    #[error("projection store: {source}")]
    Store {
        source: crate::core_store::CoreStoreError,
        candidates: Vec<IngressRefreshCandidateEvidence>,
    },
    #[error("projection is missing")]
    ProjectionMissing {
        candidates: Vec<IngressRefreshCandidateEvidence>,
    },
    #[error("concurrent projection writer")]
    ConcurrentWriter {
        candidates: Vec<IngressRefreshCandidateEvidence>,
    },
    #[error("operation evidence store failed")]
    OperationStore,
}

impl IngressEndpointRefreshError {
    fn operation_failure(&self) -> IngressRefreshFailure {
        match self {
            Self::IntentUnavailable => IngressRefreshFailure::IntentUnavailable {
                message: failure_message("intent.get was unavailable"),
            },
            Self::Store { source, candidates } => IngressRefreshFailure::StorageFailed {
                message: bounded_failure_message("projection store failed", source),
                candidates: candidates.clone(),
            },
            Self::ProjectionMissing { candidates } => IngressRefreshFailure::StorageFailed {
                message: failure_message("ingress endpoint projection is missing"),
                candidates: candidates.clone(),
            },
            Self::OperationStore => IngressRefreshFailure::StorageFailed {
                message: failure_message("operation evidence store failed"),
                candidates: Vec::new(),
            },
            Self::ConcurrentWriter { candidates } => IngressRefreshFailure::ConcurrentWriter {
                message: failure_message("ingress endpoint projection changed during refresh"),
                candidates: candidates.clone(),
            },
        }
    }
}

fn refresh_evidence(
    previous: &ProjectionEvidenceRecord,
    next: &ProjectionEvidenceRecord,
) -> IngressRefreshEvidence {
    IngressRefreshEvidence {
        candidates: next.candidate_outcomes.clone(),
        publishable_gateway_ids: next.publishable_gateway_ids.clone(),
        deadline_seconds: GATHER_DEADLINE.as_secs(),
        before: previous.projection.identity(),
        after: next.projection.identity(),
        invalidation: IngressRefreshInvalidationEvidence::Published,
    }
}

fn candidate_publication(
    gateway: IngressRefreshGatewayOutcome,
    facts: &IngressRefreshFactsOutcome,
) -> IngressRefreshCandidatePublication {
    if gateway_may_publish(gateway)
        && let IngressRefreshFactsOutcome::Responded {
            public_control_endpoints,
        } = facts
        && !public_control_endpoints.is_empty()
    {
        return IngressRefreshCandidatePublication::Published {
            addresses: public_control_endpoints.clone(),
        };
    }
    let reason = match (gateway, facts) {
        (IngressRefreshGatewayOutcome::Unavailable, _) => {
            IngressRefreshExclusionReason::GatewayUnavailable
        }
        (
            IngressRefreshGatewayOutcome::TimedOut
            | IngressRefreshGatewayOutcome::NoResponder
            | IngressRefreshGatewayOutcome::WrongResponder
            | IngressRefreshGatewayOutcome::Rejected
            | IngressRefreshGatewayOutcome::Malformed
            | IngressRefreshGatewayOutcome::Transport,
            _,
        ) => IngressRefreshExclusionReason::GatewayTestimonyFailed,
        (
            _,
            IngressRefreshFactsOutcome::TimedOut
            | IngressRefreshFactsOutcome::NoResponder
            | IngressRefreshFactsOutcome::WrongResponder
            | IngressRefreshFactsOutcome::Rejected
            | IngressRefreshFactsOutcome::Malformed
            | IngressRefreshFactsOutcome::Transport,
        ) => IngressRefreshExclusionReason::FactsTestimonyFailed,
        (_, IngressRefreshFactsOutcome::Missing) => IngressRefreshExclusionReason::MissingFacts,
        (_, IngressRefreshFactsOutcome::Addressless) => IngressRefreshExclusionReason::Addressless,
        (_, IngressRefreshFactsOutcome::NonPublic) => {
            IngressRefreshExclusionReason::NonPublicAddresses
        }
        (_, IngressRefreshFactsOutcome::Responded { .. }) => {
            IngressRefreshExclusionReason::Addressless
        }
    };
    IngressRefreshCandidatePublication::Excluded { reason }
}

fn gateway_candidates(intent: &IntentSnapshot) -> Vec<MachineId> {
    let mut candidates = intent
        .active_machines
        .iter()
        .filter(|machine| machine.roles.gateway == GatewayRole::Install)
        .map(|machine| machine.machine_id.clone())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
}

fn candidate_outcome(
    machine_id: MachineId,
    gateway: Result<ployz_core::state::GatewayStatusObservation, GatewayStatusReadError>,
    facts: Result<ployz_core::machine_runtime::MachineFactsSnapshot, MachineFactsReadError>,
) -> IngressRefreshCandidateEvidence {
    let gateway = match gateway {
        Ok(status) => match status.serving {
            GatewayServingStatus::Current => IngressRefreshGatewayOutcome::Current,
            GatewayServingStatus::LastKnownGood => IngressRefreshGatewayOutcome::LastKnownGood,
            GatewayServingStatus::Unavailable => IngressRefreshGatewayOutcome::Unavailable,
        },
        Err(error) => gateway_failure(&error.reason),
    };
    let facts = match facts {
        Ok(facts) => match facts.endpoints() {
            None => IngressRefreshFactsOutcome::Missing,
            Some(endpoints) if endpoints.control_endpoints.is_empty() => {
                IngressRefreshFactsOutcome::Addressless
            }
            Some(endpoints) => {
                let public_control_endpoints = endpoints
                    .control_endpoints
                    .iter()
                    .copied()
                    .filter(|address| is_public(*address))
                    .collect::<Vec<_>>();
                if public_control_endpoints.is_empty() {
                    IngressRefreshFactsOutcome::NonPublic
                } else {
                    IngressRefreshFactsOutcome::Responded {
                        public_control_endpoints,
                    }
                }
            }
        },
        Err(MachineFactsReadError::Unavailable { reason, .. }) => facts_failure(&reason),
        Err(MachineFactsReadError::GatherFailed { .. }) => IngressRefreshFactsOutcome::Rejected,
    };
    let publication = candidate_publication(gateway, &facts);
    IngressRefreshCandidateEvidence {
        machine_id,
        gateway,
        facts,
        publication,
    }
}

fn failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("static ingress refresh failure is non-empty")
}

fn bounded_failure_message(context: &str, error: &impl std::fmt::Display) -> FailureMessage {
    const MAX_CHARS: usize = 512;
    let rendered = format!("{context}: {error}");
    let bounded = rendered.chars().take(MAX_CHARS).collect::<String>();
    FailureMessage::try_new(bounded).expect("ingress refresh failure context is non-empty")
}

const fn gateway_failure(reason: &MachineRuntimeUnavailableReason) -> IngressRefreshGatewayOutcome {
    match reason {
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. } => {
            IngressRefreshGatewayOutcome::TimedOut
        }
        MachineRuntimeUnavailableReason::NoResponders => IngressRefreshGatewayOutcome::NoResponder,
        MachineRuntimeUnavailableReason::WrongResponder { .. } => {
            IngressRefreshGatewayOutcome::WrongResponder
        }
        MachineRuntimeUnavailableReason::DecodeResponse { .. }
        | MachineRuntimeUnavailableReason::MalformedServiceError { .. } => {
            IngressRefreshGatewayOutcome::Malformed
        }
        MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. } => {
            IngressRefreshGatewayOutcome::Rejected
        }
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => {
            IngressRefreshGatewayOutcome::Transport
        }
    }
}

const fn facts_failure(reason: &MachineRuntimeUnavailableReason) -> IngressRefreshFactsOutcome {
    match reason {
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. } => {
            IngressRefreshFactsOutcome::TimedOut
        }
        MachineRuntimeUnavailableReason::NoResponders => IngressRefreshFactsOutcome::NoResponder,
        MachineRuntimeUnavailableReason::WrongResponder { .. } => {
            IngressRefreshFactsOutcome::WrongResponder
        }
        MachineRuntimeUnavailableReason::DecodeResponse { .. }
        | MachineRuntimeUnavailableReason::MalformedServiceError { .. } => {
            IngressRefreshFactsOutcome::Malformed
        }
        MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. } => {
            IngressRefreshFactsOutcome::Rejected
        }
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => {
            IngressRefreshFactsOutcome::Transport
        }
    }
}

const fn gateway_is_valid_reply(outcome: IngressRefreshGatewayOutcome) -> bool {
    matches!(
        outcome,
        IngressRefreshGatewayOutcome::Current
            | IngressRefreshGatewayOutcome::LastKnownGood
            | IngressRefreshGatewayOutcome::Unavailable
    )
}

const fn gateway_may_publish(outcome: IngressRefreshGatewayOutcome) -> bool {
    matches!(
        outcome,
        IngressRefreshGatewayOutcome::Current | IngressRefreshGatewayOutcome::LastKnownGood
    )
}

fn project_refresh(
    previous: &ProjectionEvidenceRecord,
    mut outcomes: Vec<IngressRefreshCandidateEvidence>,
) -> ProjectionEvidenceRecord {
    outcomes.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    let next_state = if outcomes.is_empty() {
        IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoDeclaredGateways,
        }
    } else if !outcomes
        .iter()
        .any(|outcome| gateway_is_valid_reply(outcome.gateway))
    {
        retained_after_total_silence(&previous.projection.state)
    } else {
        let addresses = outcomes
            .iter()
            .flat_map(|outcome| match &outcome.publication {
                IngressRefreshCandidatePublication::Published { addresses } => addresses.as_slice(),
                IngressRefreshCandidatePublication::Excluded { .. } => &[],
            })
            .copied()
            .collect::<Vec<_>>();
        endpoint_state(addresses)
    };
    let revision =
        previous.projection.revision + u64::from(next_state != previous.projection.state);
    let publishable_gateway_ids = outcomes
        .iter()
        .filter(|outcome| {
            matches!(
                outcome.publication,
                IngressRefreshCandidatePublication::Published { .. }
            )
        })
        .map(|outcome| outcome.machine_id.clone())
        .collect();
    ProjectionEvidenceRecord {
        projection: IngressEndpointProjection {
            control_plane_epoch: previous.projection.control_plane_epoch,
            revision,
            state: next_state,
        },
        candidate_outcomes: outcomes,
        publishable_gateway_ids,
    }
}

fn retained_after_total_silence(
    previous: &IngressEndpointProjectionState,
) -> IngressEndpointProjectionState {
    match previous {
        IngressEndpointProjectionState::Current { endpoints }
        | IngressEndpointProjectionState::Retained { endpoints } => {
            IngressEndpointProjectionState::Retained {
                endpoints: endpoints.clone(),
            }
        }
        IngressEndpointProjectionState::Pending => IngressEndpointProjectionState::Pending,
        IngressEndpointProjectionState::Unavailable { reason } => {
            IngressEndpointProjectionState::Unavailable { reason: *reason }
        }
    }
}

fn endpoint_state(addresses: Vec<IpAddr>) -> IngressEndpointProjectionState {
    let ipv4 = addresses.iter().filter_map(|address| match address {
        IpAddr::V4(address) => Some(*address),
        IpAddr::V6(_) => None,
    });
    let ipv6 = addresses.iter().filter_map(|address| match address {
        IpAddr::V4(_) => None,
        IpAddr::V6(address) => Some(*address),
    });
    match IngressEndpointSet::try_new(ipv4, ipv6) {
        Ok(endpoints) => IngressEndpointProjectionState::Current { endpoints },
        Err(_) => IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoPublishableEndpoints,
        },
    }
}

async fn publish_changed(
    client: &async_nats::Client,
    identity: IngressEndpointProjectionIdentity,
) -> Result<(), async_nats::PublishError> {
    let payload = serde_json::to_vec(&identity).expect("projection identity serializes");
    client
        .publish(INGRESS_ENDPOINT_CHANGED, payload.into())
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ops::EventSequence;

    fn pending() -> ProjectionEvidenceRecord {
        ProjectionEvidenceRecord::pending(ControlPlaneEpoch::initial())
    }

    fn outcome(
        machine: &str,
        gateway: IngressRefreshGatewayOutcome,
        addresses: &[&str],
    ) -> IngressRefreshCandidateEvidence {
        let addresses = addresses
            .iter()
            .map(|address| address.parse().expect("address"))
            .collect::<Vec<_>>();
        IngressRefreshCandidateEvidence {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            gateway,
            facts: IngressRefreshFactsOutcome::Responded {
                public_control_endpoints: addresses.clone(),
            },
            publication: if gateway_may_publish(gateway) {
                IngressRefreshCandidatePublication::Published { addresses }
            } else {
                IngressRefreshCandidatePublication::Excluded {
                    reason: IngressRefreshExclusionReason::GatewayUnavailable,
                }
            },
        }
    }

    #[test]
    fn zero_candidates_is_decisive_withdrawal() {
        let projected = project_refresh(&pending(), Vec::new());
        assert_eq!(
            projected.projection.state,
            IngressEndpointProjectionState::Unavailable {
                reason: IngressEndpointUnavailableReason::NoDeclaredGateways,
            }
        );
    }

    #[test]
    fn one_valid_responder_removes_silent_peers() {
        let projected = project_refresh(
            &pending(),
            vec![
                outcome(
                    "machine_a",
                    IngressRefreshGatewayOutcome::Current,
                    &["8.8.8.8"],
                ),
                IngressRefreshCandidateEvidence {
                    machine_id: MachineId::try_new("machine_b").expect("machine id"),
                    gateway: IngressRefreshGatewayOutcome::TimedOut,
                    facts: IngressRefreshFactsOutcome::TimedOut,
                    publication: IngressRefreshCandidatePublication::Excluded {
                        reason: IngressRefreshExclusionReason::GatewayTestimonyFailed,
                    },
                },
            ],
        );
        let IngressEndpointProjectionState::Current { endpoints } = projected.projection.state
        else {
            panic!("current projection")
        };
        assert_eq!(
            endpoints.ipv4().iter().copied().collect::<Vec<_>>(),
            vec!["8.8.8.8".parse::<std::net::Ipv4Addr>().expect("address")]
        );
    }

    #[test]
    fn total_silence_retains_current_complete_set() {
        let current = project_refresh(
            &pending(),
            vec![outcome(
                "machine_a",
                IngressRefreshGatewayOutcome::Current,
                &["1.1.1.1"],
            )],
        );
        let silent = IngressRefreshCandidateEvidence {
            machine_id: MachineId::try_new("machine_a").expect("machine id"),
            gateway: IngressRefreshGatewayOutcome::TimedOut,
            facts: IngressRefreshFactsOutcome::TimedOut,
            publication: IngressRefreshCandidatePublication::Excluded {
                reason: IngressRefreshExclusionReason::GatewayTestimonyFailed,
            },
        };
        let retained = project_refresh(&current, vec![silent]);
        assert!(matches!(
            retained.projection.state,
            IngressEndpointProjectionState::Retained { .. }
        ));
        assert_eq!(
            retained.projection.revision,
            current.projection.revision + 1
        );
    }

    #[test]
    fn valid_gateway_without_facts_is_decisive_empty_union() {
        let projected = project_refresh(
            &pending(),
            vec![IngressRefreshCandidateEvidence {
                machine_id: MachineId::try_new("machine_a").expect("machine id"),
                gateway: IngressRefreshGatewayOutcome::Current,
                facts: IngressRefreshFactsOutcome::TimedOut,
                publication: IngressRefreshCandidatePublication::Excluded {
                    reason: IngressRefreshExclusionReason::FactsTestimonyFailed,
                },
            }],
        );

        assert_eq!(
            projected.projection.state,
            IngressEndpointProjectionState::Unavailable {
                reason: IngressEndpointUnavailableReason::NoPublishableEndpoints,
            }
        );
    }

    #[test]
    fn identical_refresh_keeps_revision_stable() {
        let first = project_refresh(
            &pending(),
            vec![outcome(
                "machine_a",
                IngressRefreshGatewayOutcome::Current,
                &["1.1.1.1"],
            )],
        );
        let repeated = project_refresh(
            &first,
            vec![outcome(
                "machine_a",
                IngressRefreshGatewayOutcome::Current,
                &["1.1.1.1"],
            )],
        );

        assert_eq!(repeated.projection.revision, first.projection.revision);
    }

    #[test]
    fn candidate_evidence_names_each_non_publishable_reason() {
        let machine_id = MachineId::try_new("machine_a").expect("machine id");
        let cases = [
            (
                IngressRefreshGatewayOutcome::Unavailable,
                IngressRefreshFactsOutcome::Addressless,
                IngressRefreshExclusionReason::GatewayUnavailable,
            ),
            (
                IngressRefreshGatewayOutcome::Current,
                IngressRefreshFactsOutcome::Missing,
                IngressRefreshExclusionReason::MissingFacts,
            ),
            (
                IngressRefreshGatewayOutcome::Current,
                IngressRefreshFactsOutcome::Addressless,
                IngressRefreshExclusionReason::Addressless,
            ),
            (
                IngressRefreshGatewayOutcome::Current,
                IngressRefreshFactsOutcome::NonPublic,
                IngressRefreshExclusionReason::NonPublicAddresses,
            ),
            (
                IngressRefreshGatewayOutcome::TimedOut,
                IngressRefreshFactsOutcome::Addressless,
                IngressRefreshExclusionReason::GatewayTestimonyFailed,
            ),
            (
                IngressRefreshGatewayOutcome::Current,
                IngressRefreshFactsOutcome::Malformed,
                IngressRefreshExclusionReason::FactsTestimonyFailed,
            ),
        ];

        for (gateway, facts, expected) in cases {
            let publication = candidate_publication(gateway, &facts);
            let evidence = IngressRefreshCandidateEvidence {
                machine_id: machine_id.clone(),
                gateway,
                facts,
                publication,
            };
            assert_eq!(
                evidence.publication,
                IngressRefreshCandidatePublication::Excluded { reason: expected }
            );
        }
    }

    #[test]
    fn startup_recovery_selects_only_accepted_refreshes() {
        let accepted_id =
            ployz_core::ids::OperationId::try_new("op_refresh_accepted").expect("operation id");
        let completed_id =
            ployz_core::ids::OperationId::try_new("op_refresh_completed").expect("operation id");
        let sequence = EventSequence::try_new(1).expect("sequence");
        let completed = OperationStatus::IngressRefresh {
            id: completed_id,
            state: IngressRefreshOperationState::Completed {
                evidence: IngressRefreshEvidence {
                    candidates: Vec::new(),
                    publishable_gateway_ids: Vec::new(),
                    deadline_seconds: 30,
                    before: pending().projection.identity(),
                    after: pending().projection.identity(),
                    invalidation: IngressRefreshInvalidationEvidence::Published,
                },
            },
            last_event_sequence: sequence,
        };

        assert_eq!(
            unfinished_refresh_ids(vec![
                OperationStatus::ingress_refresh_accepted(accepted_id.clone(), sequence),
                completed,
            ]),
            vec![accepted_id]
        );
    }

    #[test]
    fn publish_failure_message_is_bounded() {
        let message = bounded_failure_message("publish", &"x".repeat(1_000));
        assert_eq!(message.as_str().chars().count(), 512);
    }
}
