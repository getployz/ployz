//! Canonical ingress endpoint projection owner.

use std::time::Duration;

use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use ployz_core::ids::MachineId;
use ployz_core::ingress::{
    IngressEndpointProjection, IngressEndpointProjectionIdentity, IngressEndpointProjectionState,
};
use ployz_core::intent::IntentSnapshot;
use ployz_core::intent::recovery::ControlPlaneEpoch;
use ployz_core::machine::GatewayServingStatus;
use ployz_core::network::reachability::is_public;
use ployz_core::roles::GatewayRole;

use ployz_nats::service_runtime::{
    NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError, NatsServiceShutdownError,
    RunningNatsService, decode_json_request, start_nats_service,
};
use ployz_nats::subjects::{INGRESS_ENDPOINT_CHANGED, INTENT_CHANGED};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::control::intent::ingress_intent::{IngressProjectionStore, IngressProjectionWrite};
use crate::control::intent::service::NatsIntentReader;
use crate::control::role_client::gateway::{GatewayStatusReadError, NatsGatewayStatusReader};
use crate::control::role_client::machine::{MachineFactsReadError, NatsMachineFactsReader};
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::service_catalog::{ingress_endpoint_get_spec, ingress_endpoint_service};

mod projection;

use projection::project_refresh;

const REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const GATHER_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionEvidenceRecord {
    pub projection: IngressEndpointProjection,
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
    Store(crate::control::store::CoreStoreError),
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
        if let Err(error) = runtime.refresh().await {
            eprintln!("ployzd ingress endpoint projection warning: {error}");
        }
    }
}

impl ProjectionRuntime {
    async fn refresh(
        &self,
    ) -> Result<Option<IngressEndpointProjectionIdentity>, IngressEndpointRefreshError> {
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
        if next == previous {
            return Ok(None);
        }
        let expected = Some(previous.projection.identity());
        let identity = next.projection.identity();
        match self
            .store
            .compare_and_replace(expected, next)
            .await
            .map_err(IngressEndpointRefreshError::Store)?
        {
            IngressProjectionWrite::Stored => {
                publish_changed(&self.client, identity)
                    .await
                    .map_err(IngressEndpointRefreshError::Publish)?;
                Ok(Some(identity))
            }
            IngressProjectionWrite::Unchanged => Ok(None),
            IngressProjectionWrite::Conflict { .. } => {
                Err(IngressEndpointRefreshError::ConcurrentWriter)
            }
        }
    }

    async fn gather(&self, candidates: Vec<MachineId>) -> Vec<CandidateOutcome> {
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
            if requests.is_empty() {
                break;
            }
            tokio::select! {
                reply = requests.next() => {
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
        result: Result<ployz_core::machine::GatewayStatusObservation, GatewayStatusReadError>,
    },
    Facts {
        machine_id: MachineId,
        result: Result<ployz_core::machine::runtime::MachineFactsSnapshot, MachineFactsReadError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CandidateOutcome {
    machine_id: MachineId,
    gateway: GatewayOutcome,
    facts: FactsOutcome,
    publication: CandidatePublication,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CandidatePublication {
    Published { addresses: Vec<std::net::IpAddr> },
    Excluded { reason: ExclusionReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExclusionReason {
    GatewayUnavailable,
    GatewayTestimonyFailed,
    FactsTestimonyFailed,
    MissingFacts,
    Addressless,
    NonPublicAddresses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayOutcome {
    Current,
    LastKnownGood,
    Unavailable,
    TimedOut,
    NoResponder,
    WrongResponder,
    Rejected,
    Malformed,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FactsOutcome {
    Responded {
        public_control_endpoints: Vec<std::net::IpAddr>,
    },
    Missing,
    Addressless,
    NonPublic,
    TimedOut,
    NoResponder,
    WrongResponder,
    Rejected,
    Malformed,
    Transport,
}

#[derive(Debug, thiserror::Error)]
enum IngressEndpointRefreshError {
    #[error("intent unavailable")]
    IntentUnavailable,
    #[error("projection store: {0}")]
    Store(crate::control::store::CoreStoreError),
    #[error("projection is missing")]
    ProjectionMissing,
    #[error("concurrent projection writer")]
    ConcurrentWriter,
    #[error("publish ingress endpoint invalidation: {0}")]
    Publish(async_nats::PublishError),
}

fn candidate_publication(gateway: GatewayOutcome, facts: &FactsOutcome) -> CandidatePublication {
    if gateway_may_publish(gateway)
        && let FactsOutcome::Responded {
            public_control_endpoints,
        } = facts
        && !public_control_endpoints.is_empty()
    {
        return CandidatePublication::Published {
            addresses: public_control_endpoints.clone(),
        };
    }
    let reason = match (gateway, facts) {
        (GatewayOutcome::Unavailable, _) => ExclusionReason::GatewayUnavailable,
        (
            GatewayOutcome::TimedOut
            | GatewayOutcome::NoResponder
            | GatewayOutcome::WrongResponder
            | GatewayOutcome::Rejected
            | GatewayOutcome::Malformed
            | GatewayOutcome::Transport,
            _,
        ) => ExclusionReason::GatewayTestimonyFailed,
        (
            _,
            FactsOutcome::TimedOut
            | FactsOutcome::NoResponder
            | FactsOutcome::WrongResponder
            | FactsOutcome::Rejected
            | FactsOutcome::Malformed
            | FactsOutcome::Transport,
        ) => ExclusionReason::FactsTestimonyFailed,
        (_, FactsOutcome::Missing) => ExclusionReason::MissingFacts,
        (_, FactsOutcome::Addressless) => ExclusionReason::Addressless,
        (_, FactsOutcome::NonPublic) => ExclusionReason::NonPublicAddresses,
        (_, FactsOutcome::Responded { .. }) => ExclusionReason::Addressless,
    };
    CandidatePublication::Excluded { reason }
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
    gateway: Result<ployz_core::machine::GatewayStatusObservation, GatewayStatusReadError>,
    facts: Result<ployz_core::machine::runtime::MachineFactsSnapshot, MachineFactsReadError>,
) -> CandidateOutcome {
    let gateway = match gateway {
        Ok(status) => match status.serving {
            GatewayServingStatus::Current => GatewayOutcome::Current,
            GatewayServingStatus::LastKnownGood => GatewayOutcome::LastKnownGood,
            GatewayServingStatus::Unavailable => GatewayOutcome::Unavailable,
        },
        Err(error) => gateway_failure(&error.reason),
    };
    let facts = match facts {
        Ok(facts) => match facts.endpoints() {
            None => FactsOutcome::Missing,
            Some(endpoints) if endpoints.control_endpoints.is_empty() => FactsOutcome::Addressless,
            Some(endpoints) => {
                let public_control_endpoints = endpoints
                    .control_endpoints
                    .iter()
                    .copied()
                    .filter(|address| is_public(*address))
                    .collect::<Vec<_>>();
                if public_control_endpoints.is_empty() {
                    FactsOutcome::NonPublic
                } else {
                    FactsOutcome::Responded {
                        public_control_endpoints,
                    }
                }
            }
        },
        Err(MachineFactsReadError::Unavailable { reason, .. }) => facts_failure(&reason),
        Err(MachineFactsReadError::GatherFailed { .. }) => FactsOutcome::Rejected,
    };
    let publication = candidate_publication(gateway, &facts);
    CandidateOutcome {
        machine_id,
        gateway,
        facts,
        publication,
    }
}

const fn gateway_failure(reason: &MachineRuntimeUnavailableReason) -> GatewayOutcome {
    match reason {
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. } => GatewayOutcome::TimedOut,
        MachineRuntimeUnavailableReason::NoResponders => GatewayOutcome::NoResponder,
        MachineRuntimeUnavailableReason::WrongResponder { .. } => GatewayOutcome::WrongResponder,
        MachineRuntimeUnavailableReason::DecodeResponse { .. }
        | MachineRuntimeUnavailableReason::MalformedServiceError { .. } => {
            GatewayOutcome::Malformed
        }
        MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. } => GatewayOutcome::Rejected,
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceResponseTooLarge
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => GatewayOutcome::Transport,
    }
}

const fn facts_failure(reason: &MachineRuntimeUnavailableReason) -> FactsOutcome {
    match reason {
        MachineRuntimeUnavailableReason::RequestTimedOut
        | MachineRuntimeUnavailableReason::ServiceTimedOut { .. } => FactsOutcome::TimedOut,
        MachineRuntimeUnavailableReason::NoResponders => FactsOutcome::NoResponder,
        MachineRuntimeUnavailableReason::WrongResponder { .. } => FactsOutcome::WrongResponder,
        MachineRuntimeUnavailableReason::DecodeResponse { .. }
        | MachineRuntimeUnavailableReason::MalformedServiceError { .. } => FactsOutcome::Malformed,
        MachineRuntimeUnavailableReason::ServiceBadRequest { .. }
        | MachineRuntimeUnavailableReason::ServiceConflict { .. } => FactsOutcome::Rejected,
        MachineRuntimeUnavailableReason::EncodeRequest { .. }
        | MachineRuntimeUnavailableReason::InvalidSubject
        | MachineRuntimeUnavailableReason::MaxPayloadExceeded
        | MachineRuntimeUnavailableReason::RequestFailed { .. }
        | MachineRuntimeUnavailableReason::ServiceResponseTooLarge
        | MachineRuntimeUnavailableReason::ServiceUnavailable { .. }
        | MachineRuntimeUnavailableReason::ServiceInternal { .. } => FactsOutcome::Transport,
    }
}

const fn gateway_may_publish(outcome: GatewayOutcome) -> bool {
    matches!(
        outcome,
        GatewayOutcome::Current | GatewayOutcome::LastKnownGood
    )
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

    #[test]
    fn candidate_evidence_names_each_non_publishable_reason() {
        let machine_id = MachineId::try_new("machine_a").expect("machine id");
        let cases = [
            (
                GatewayOutcome::Unavailable,
                FactsOutcome::Addressless,
                ExclusionReason::GatewayUnavailable,
            ),
            (
                GatewayOutcome::Current,
                FactsOutcome::Missing,
                ExclusionReason::MissingFacts,
            ),
            (
                GatewayOutcome::Current,
                FactsOutcome::Addressless,
                ExclusionReason::Addressless,
            ),
            (
                GatewayOutcome::Current,
                FactsOutcome::NonPublic,
                ExclusionReason::NonPublicAddresses,
            ),
            (
                GatewayOutcome::TimedOut,
                FactsOutcome::Addressless,
                ExclusionReason::GatewayTestimonyFailed,
            ),
            (
                GatewayOutcome::Current,
                FactsOutcome::Malformed,
                ExclusionReason::FactsTestimonyFailed,
            ),
        ];

        for (gateway, facts, expected) in cases {
            let publication = candidate_publication(gateway, &facts);
            let evidence = CandidateOutcome {
                machine_id: machine_id.clone(),
                gateway,
                facts,
                publication,
            };
            assert_eq!(
                evidence.publication,
                CandidatePublication::Excluded { reason: expected }
            );
        }
    }
}
