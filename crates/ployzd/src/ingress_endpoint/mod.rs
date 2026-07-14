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
    IngressRefreshCandidateEvidence, IngressRefreshEvidence, IngressRefreshFactsOutcome,
    IngressRefreshFailure, IngressRefreshGatewayOutcome, IngressRefreshTransition,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TestimonyFailure {
    TimedOut,
    NoResponder,
    WrongResponder,
    Rejected,
    Malformed,
    Transport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum GatewayTestimonyOutcome {
    Current,
    LastKnownGood,
    Unavailable,
    Failed { failure: TestimonyFailure },
}

impl GatewayTestimonyOutcome {
    const fn is_valid_reply(self) -> bool {
        matches!(
            self,
            Self::Current | Self::LastKnownGood | Self::Unavailable
        )
    }

    const fn may_publish(self) -> bool {
        matches!(self, Self::Current | Self::LastKnownGood)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum MachineFactsOutcome {
    Responded {
        public_control_endpoints: Vec<IpAddr>,
    },
    Failed {
        failure: TestimonyFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateProjectionOutcome {
    pub machine_id: MachineId,
    pub gateway: GatewayTestimonyOutcome,
    pub facts: MachineFactsOutcome,
    pub contribution: Vec<IpAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionEvidenceRecord {
    pub projection: IngressEndpointProjection,
    pub candidate_outcomes: Vec<CandidateProjectionOutcome>,
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
        let periodic = tokio::select! {
            _ = interval.tick() => true,
            changed = intent_changed.next() => {
                if changed.is_none() { break; }
                false
            }
            _ = shutdown.recv() => break,
        };
        while intent_changed.next().now_or_never().flatten().is_some() {}
        let identity = match runtime.refresh().await {
            Ok(identity) => identity,
            Err(_) if periodic => runtime
                .store
                .load()
                .await
                .ok()
                .flatten()
                .map(|record| record.projection.identity()),
            Err(_) => None,
        };
        if let Some(identity) = identity
            && let Err(error) = publish_changed(&runtime.client, identity).await
        {
            eprintln!("ployzd control warning: phase=ingress-endpoint-signal message={error}");
        }
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
        let transition = match &result {
            Ok((_, evidence)) => IngressRefreshTransition::Completed {
                evidence: evidence.clone(),
            },
            Err(error) => IngressRefreshTransition::Failed {
                failure: error.operation_failure(),
            },
        };
        self.operations
            .record_ingress_refresh_transition(&operation_id, transition)
            .await
            .map_err(|_| IngressEndpointRefreshError::OperationStore)?;
        result.map(|(identity, _)| Some(identity))
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
        let previous = self
            .store
            .load()
            .await
            .map_err(IngressEndpointRefreshError::Store)?
            .ok_or(IngressEndpointRefreshError::ProjectionMissing)?;
        let next = project_refresh(&previous, outcomes);
        let evidence = refresh_evidence(&previous, &next);
        let expected = Some(previous.projection.identity());
        let identity = next.projection.identity();
        match self
            .store
            .compare_and_replace(expected, next)
            .await
            .map_err(IngressEndpointRefreshError::Store)?
        {
            IngressProjectionWrite::Stored | IngressProjectionWrite::Unchanged => {
                Ok((identity, evidence))
            }
            IngressProjectionWrite::Conflict { .. } => {
                Err(IngressEndpointRefreshError::ConcurrentWriter)
            }
        }
    }

    async fn gather(&self, candidates: Vec<MachineId>) -> Vec<CandidateProjectionOutcome> {
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
                    facts.unwrap_or_else(|| {
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
    #[error("projection store: {0}")]
    Store(crate::core_store::CoreStoreError),
    #[error("projection is missing")]
    ProjectionMissing,
    #[error("concurrent projection writer")]
    ConcurrentWriter,
    #[error("operation evidence store failed")]
    OperationStore,
}

impl IngressEndpointRefreshError {
    const fn operation_failure(&self) -> IngressRefreshFailure {
        match self {
            Self::IntentUnavailable => IngressRefreshFailure::IntentUnavailable,
            Self::Store(_) | Self::ProjectionMissing | Self::OperationStore => {
                IngressRefreshFailure::StorageFailed
            }
            Self::ConcurrentWriter => IngressRefreshFailure::ConcurrentWriter,
        }
    }
}

fn refresh_evidence(
    previous: &ProjectionEvidenceRecord,
    next: &ProjectionEvidenceRecord,
) -> IngressRefreshEvidence {
    IngressRefreshEvidence {
        candidates: next
            .candidate_outcomes
            .iter()
            .map(candidate_evidence)
            .collect(),
        publishable_gateway_ids: next.publishable_gateway_ids.clone(),
        deadline_seconds: GATHER_DEADLINE.as_secs(),
        before: previous.projection.identity(),
        after: next.projection.identity(),
    }
}

fn candidate_evidence(outcome: &CandidateProjectionOutcome) -> IngressRefreshCandidateEvidence {
    IngressRefreshCandidateEvidence {
        machine_id: outcome.machine_id.clone(),
        gateway: match outcome.gateway {
            GatewayTestimonyOutcome::Current => IngressRefreshGatewayOutcome::Current,
            GatewayTestimonyOutcome::LastKnownGood => IngressRefreshGatewayOutcome::LastKnownGood,
            GatewayTestimonyOutcome::Unavailable => IngressRefreshGatewayOutcome::Unavailable,
            GatewayTestimonyOutcome::Failed { failure } => gateway_failure_evidence(failure),
        },
        facts: match &outcome.facts {
            MachineFactsOutcome::Responded {
                public_control_endpoints,
            } => IngressRefreshFactsOutcome::Responded {
                public_control_endpoints: public_control_endpoints.clone(),
            },
            MachineFactsOutcome::Failed { failure } => facts_failure_evidence(*failure),
        },
        contribution: outcome.contribution.clone(),
    }
}

const fn gateway_failure_evidence(failure: TestimonyFailure) -> IngressRefreshGatewayOutcome {
    match failure {
        TestimonyFailure::TimedOut => IngressRefreshGatewayOutcome::TimedOut,
        TestimonyFailure::NoResponder => IngressRefreshGatewayOutcome::NoResponder,
        TestimonyFailure::WrongResponder => IngressRefreshGatewayOutcome::WrongResponder,
        TestimonyFailure::Rejected => IngressRefreshGatewayOutcome::Rejected,
        TestimonyFailure::Malformed => IngressRefreshGatewayOutcome::Malformed,
        TestimonyFailure::Transport => IngressRefreshGatewayOutcome::Transport,
    }
}

const fn facts_failure_evidence(failure: TestimonyFailure) -> IngressRefreshFactsOutcome {
    match failure {
        TestimonyFailure::TimedOut => IngressRefreshFactsOutcome::TimedOut,
        TestimonyFailure::NoResponder => IngressRefreshFactsOutcome::NoResponder,
        TestimonyFailure::WrongResponder => IngressRefreshFactsOutcome::WrongResponder,
        TestimonyFailure::Rejected => IngressRefreshFactsOutcome::Rejected,
        TestimonyFailure::Malformed => IngressRefreshFactsOutcome::Malformed,
        TestimonyFailure::Transport => IngressRefreshFactsOutcome::Transport,
    }
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
) -> CandidateProjectionOutcome {
    let gateway = match gateway {
        Ok(status) => match status.serving {
            GatewayServingStatus::Current => GatewayTestimonyOutcome::Current,
            GatewayServingStatus::LastKnownGood => GatewayTestimonyOutcome::LastKnownGood,
            GatewayServingStatus::Unavailable => GatewayTestimonyOutcome::Unavailable,
        },
        Err(error) => GatewayTestimonyOutcome::Failed {
            failure: classify_machine_failure(&error.reason),
        },
    };
    let facts = match facts {
        Ok(facts) => MachineFactsOutcome::Responded {
            public_control_endpoints: facts
                .endpoints()
                .map(|endpoints| {
                    endpoints
                        .control_endpoints
                        .iter()
                        .copied()
                        .filter(|address| is_public(*address))
                        .collect()
                })
                .unwrap_or_default(),
        },
        Err(MachineFactsReadError::Unavailable { reason, .. }) => MachineFactsOutcome::Failed {
            failure: classify_machine_failure(&reason),
        },
        Err(MachineFactsReadError::GatherFailed { .. }) => MachineFactsOutcome::Failed {
            failure: TestimonyFailure::Rejected,
        },
    };
    let contribution = match (&gateway, &facts) {
        (
            gateway,
            MachineFactsOutcome::Responded {
                public_control_endpoints,
            },
        ) if gateway.may_publish() => public_control_endpoints.clone(),
        _ => Vec::new(),
    };
    CandidateProjectionOutcome {
        machine_id,
        gateway,
        facts,
        contribution,
    }
}

fn classify_machine_failure(reason: &MachineRuntimeUnavailableReason) -> TestimonyFailure {
    match reason {
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. } => TestimonyFailure::TimedOut,
        MachineRuntimeUnavailableReason::NoResponders => TestimonyFailure::NoResponder,
        MachineRuntimeUnavailableReason::WrongResponder { .. } => TestimonyFailure::WrongResponder,
        MachineRuntimeUnavailableReason::DecodeResponse { .. }
        | MachineRuntimeUnavailableReason::MalformedServiceError { .. } => {
            TestimonyFailure::Malformed
        }
        MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. } => TestimonyFailure::Rejected,
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => TestimonyFailure::Transport,
    }
}

fn project_refresh(
    previous: &ProjectionEvidenceRecord,
    mut outcomes: Vec<CandidateProjectionOutcome>,
) -> ProjectionEvidenceRecord {
    outcomes.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
    let next_state = if outcomes.is_empty() {
        IngressEndpointProjectionState::Unavailable {
            reason: IngressEndpointUnavailableReason::NoDeclaredGateways,
        }
    } else if !outcomes
        .iter()
        .any(|outcome| outcome.gateway.is_valid_reply())
    {
        retained_after_total_silence(&previous.projection.state)
    } else {
        let addresses = outcomes
            .iter()
            .flat_map(|outcome| outcome.contribution.iter().copied())
            .collect::<Vec<_>>();
        endpoint_state(addresses)
    };
    let revision =
        previous.projection.revision + u64::from(next_state != previous.projection.state);
    let publishable_gateway_ids = outcomes
        .iter()
        .filter(|outcome| !outcome.contribution.is_empty())
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

    fn pending() -> ProjectionEvidenceRecord {
        ProjectionEvidenceRecord::pending(ControlPlaneEpoch::initial())
    }

    fn outcome(
        machine: &str,
        gateway: GatewayTestimonyOutcome,
        addresses: &[&str],
    ) -> CandidateProjectionOutcome {
        let addresses = addresses
            .iter()
            .map(|address| address.parse().expect("address"))
            .collect::<Vec<_>>();
        CandidateProjectionOutcome {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            gateway,
            facts: MachineFactsOutcome::Responded {
                public_control_endpoints: addresses.clone(),
            },
            contribution: gateway
                .may_publish()
                .then_some(addresses)
                .unwrap_or_default(),
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
                outcome("machine_a", GatewayTestimonyOutcome::Current, &["8.8.8.8"]),
                CandidateProjectionOutcome {
                    machine_id: MachineId::try_new("machine_b").expect("machine id"),
                    gateway: GatewayTestimonyOutcome::Failed {
                        failure: TestimonyFailure::TimedOut,
                    },
                    facts: MachineFactsOutcome::Failed {
                        failure: TestimonyFailure::TimedOut,
                    },
                    contribution: Vec::new(),
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
                GatewayTestimonyOutcome::Current,
                &["1.1.1.1"],
            )],
        );
        let silent = CandidateProjectionOutcome {
            machine_id: MachineId::try_new("machine_a").expect("machine id"),
            gateway: GatewayTestimonyOutcome::Failed {
                failure: TestimonyFailure::TimedOut,
            },
            facts: MachineFactsOutcome::Failed {
                failure: TestimonyFailure::TimedOut,
            },
            contribution: Vec::new(),
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
            vec![CandidateProjectionOutcome {
                machine_id: MachineId::try_new("machine_a").expect("machine id"),
                gateway: GatewayTestimonyOutcome::Current,
                facts: MachineFactsOutcome::Failed {
                    failure: TestimonyFailure::TimedOut,
                },
                contribution: Vec::new(),
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
                GatewayTestimonyOutcome::Current,
                &["1.1.1.1"],
            )],
        );
        let repeated = project_refresh(
            &first,
            vec![outcome(
                "machine_a",
                GatewayTestimonyOutcome::Current,
                &["1.1.1.1"],
            )],
        );

        assert_eq!(repeated.projection.revision, first.projection.revision);
    }
}
