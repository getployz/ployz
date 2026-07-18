//! Canonical ingress endpoint projection owner.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use ployz_core::ids::MachineId;
use ployz_core::ingress::{
    IngressEndpointProjection, IngressEndpointProjectionIdentity, IngressEndpointProjectionState,
    IngressEndpointSet,
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
    health: IngressEndpointProjectionHealth,
}

impl RunningIngressEndpointProjection {
    #[must_use]
    pub(crate) fn health_reader(&self) -> IngressEndpointProjectionHealth {
        self.health.clone()
    }

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
    let health = IngressEndpointProjectionHealth::default();
    let task_shutdown = shutdown.subscribe();
    let task = tokio::spawn(run_projection_loop(
        ProjectionRuntime {
            client: client.clone(),
            store,
            intent: NatsIntentReader::new(client.clone()).with_request_timeout(GATHER_DEADLINE),
            gateway: NatsGatewayStatusReader::new(client.clone())
                .with_request_timeout(GATHER_DEADLINE),
            facts: NatsMachineFactsReader::new(client).with_request_timeout(GATHER_DEADLINE),
            pending_invalidation: None,
            health: health.clone(),
        },
        changed,
        task_shutdown,
    ));
    Ok(RunningIngressEndpointProjection {
        service,
        shutdown,
        task,
        health,
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
    pending_invalidation: Option<IngressEndpointProjectionIdentity>,
    health: IngressEndpointProjectionHealth,
}

async fn run_projection_loop(
    mut runtime: ProjectionRuntime,
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
        match runtime.refresh().await {
            Ok(_) => runtime.health.record_success(),
            Err(error) => {
                eprintln!("ployzd ingress endpoint projection warning: {error}");
                runtime.health.record_failure(&error);
            }
        }
    }
}

impl ProjectionRuntime {
    async fn refresh(
        &mut self,
    ) -> Result<Option<IngressEndpointProjectionIdentity>, IngressEndpointRefreshError> {
        self.publish_pending_invalidation().await?;
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
                self.pending_invalidation = Some(identity);
                self.publish_pending_invalidation().await?;
                Ok(Some(identity))
            }
            IngressProjectionWrite::Unchanged => Ok(None),
            IngressProjectionWrite::Conflict { .. } => {
                Err(IngressEndpointRefreshError::ConcurrentWriter)
            }
        }
    }

    async fn publish_pending_invalidation(&mut self) -> Result<(), IngressEndpointRefreshError> {
        let Some(identity) = self.pending_invalidation else {
            return Ok(());
        };
        publish_changed(&self.client, identity)
            .await
            .map_err(IngressEndpointRefreshError::Publish)?;
        self.pending_invalidation = None;
        Ok(())
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
    state: CandidateEndpointState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CandidateEndpointState {
    Indecisive,
    Unavailable,
    Publishable { endpoints: IngressEndpointSet },
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

#[derive(Debug, Clone, Default)]
pub(crate) struct IngressEndpointProjectionHealth {
    state: Arc<Mutex<IngressEndpointProjectionHealthState>>,
}

impl IngressEndpointProjectionHealth {
    fn record_success(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.consecutive_failures = 0;
        state.last_failure = None;
    }

    fn record_failure(&self, error: &IngressEndpointRefreshError) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.last_failure = Some(error.to_string());
    }

    #[must_use]
    pub(crate) fn operational_health(
        &self,
    ) -> ployz_sdk_types::ControlIngressEndpointProjectionHealth {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ployz_sdk_types::ControlIngressEndpointProjectionHealth {
            consecutive_failures: state.consecutive_failures,
            last_failure: state.last_failure.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct IngressEndpointProjectionHealthState {
    consecutive_failures: u64,
    last_failure: Option<String>,
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
    let state = match gateway {
        Err(_) => CandidateEndpointState::Indecisive,
        Ok(status) => match status.serving {
            GatewayServingStatus::Unavailable => CandidateEndpointState::Unavailable,
            GatewayServingStatus::Current | GatewayServingStatus::LastKnownGood => {
                let public_addresses = facts
                    .ok()
                    .and_then(|facts| facts.endpoints().cloned())
                    .map(|endpoints| {
                        endpoints
                            .control_endpoints
                            .into_iter()
                            .filter(|address| is_public(*address))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let ipv4 = public_addresses.iter().filter_map(|address| match address {
                    std::net::IpAddr::V4(address) => Some(*address),
                    std::net::IpAddr::V6(_) => None,
                });
                let ipv6 = public_addresses.iter().filter_map(|address| match address {
                    std::net::IpAddr::V4(_) => None,
                    std::net::IpAddr::V6(address) => Some(*address),
                });
                match IngressEndpointSet::try_new(ipv4, ipv6) {
                    Ok(endpoints) => CandidateEndpointState::Publishable { endpoints },
                    Err(_) => CandidateEndpointState::Indecisive,
                }
            }
        },
    };
    CandidateOutcome { machine_id, state }
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
    use ployz_core::image::OciPlatform;
    use ployz_core::machine::GatewayProcessHealth;
    use ployz_core::machine::runtime::{
        MachineContainerObservationSnapshot, MachineDiskSpace, MachineFactsSnapshot,
    };
    use ployz_core::machine::testimony::MachineEndpointObservation;

    fn machine_id() -> MachineId {
        MachineId::try_new("machine_a").expect("machine id")
    }

    fn gateway(serving: GatewayServingStatus) -> ployz_core::machine::GatewayStatusObservation {
        ployz_core::machine::GatewayStatusObservation {
            machine_id: machine_id(),
            listen_addr: "127.0.0.1:443".parse().expect("listen address"),
            serving,
            route_count: 0,
            process_health: GatewayProcessHealth::default(),
        }
    }

    fn facts(control_endpoints: Option<Vec<std::net::IpAddr>>) -> MachineFactsSnapshot {
        let machine_id = machine_id();
        let endpoints = control_endpoints.map(|control_endpoints| MachineEndpointObservation {
            machine_id: machine_id.clone(),
            control_endpoints,
            mesh_endpoints: vec!["127.0.0.1:7777".parse().expect("mesh endpoint")],
        });
        MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, std::iter::empty())
                .expect("container snapshot"),
            endpoints,
            MachineDiskSpace {
                available_bytes: 1,
                total_bytes: 1,
            },
            None,
            OciPlatform::current(),
            1,
        )
        .expect("machine facts")
    }

    fn facts_unavailable() -> MachineFactsReadError {
        MachineFactsReadError::Unavailable {
            machine_id: machine_id(),
            reason: MachineRuntimeUnavailableReason::RequestTimedOut,
        }
    }

    #[test]
    fn gateway_read_error_is_indecisive() {
        let outcome = candidate_outcome(
            machine_id(),
            Err(GatewayStatusReadError {
                machine_id: machine_id(),
                reason: MachineRuntimeUnavailableReason::RequestTimedOut,
            }),
            Ok(facts(Some(vec!["8.8.8.8".parse().expect("address")]))),
        );

        assert_eq!(outcome.state, CandidateEndpointState::Indecisive);
    }

    #[test]
    fn explicit_gateway_unavailability_is_decisive() {
        let outcome = candidate_outcome(
            machine_id(),
            Ok(gateway(GatewayServingStatus::Unavailable)),
            Err(facts_unavailable()),
        );

        assert_eq!(outcome.state, CandidateEndpointState::Unavailable);
    }

    #[test]
    fn serving_gateway_without_facts_is_indecisive() {
        let outcome = candidate_outcome(
            machine_id(),
            Ok(gateway(GatewayServingStatus::Current)),
            Err(facts_unavailable()),
        );

        assert_eq!(outcome.state, CandidateEndpointState::Indecisive);
    }

    #[test]
    fn serving_gateway_without_public_control_endpoints_is_indecisive() {
        for facts in [
            facts(None),
            facts(Some(Vec::new())),
            facts(Some(vec!["10.0.0.1".parse().expect("address")])),
        ] {
            let outcome = candidate_outcome(
                machine_id(),
                Ok(gateway(GatewayServingStatus::Current)),
                Ok(facts),
            );

            assert_eq!(outcome.state, CandidateEndpointState::Indecisive);
        }
    }

    #[test]
    fn serving_gateway_with_public_control_endpoint_is_publishable() {
        for serving in [
            GatewayServingStatus::Current,
            GatewayServingStatus::LastKnownGood,
        ] {
            let public_address = "8.8.8.8".parse().expect("address");
            let outcome = candidate_outcome(
                machine_id(),
                Ok(gateway(serving)),
                Ok(facts(Some(vec![public_address]))),
            );

            assert_eq!(
                outcome.state,
                CandidateEndpointState::Publishable {
                    endpoints: IngressEndpointSet::try_new(
                        ["8.8.8.8".parse().expect("IPv4 address")],
                        [],
                    )
                    .expect("endpoint set"),
                }
            );
        }
    }

    #[test]
    fn projection_health_records_failure_and_recovery() {
        let health = IngressEndpointProjectionHealth::default();

        health.record_failure(&IngressEndpointRefreshError::ProjectionMissing);
        assert_eq!(health.operational_health().consecutive_failures, 1);
        assert_eq!(
            health.operational_health().last_failure.as_deref(),
            Some("projection is missing")
        );

        health.record_success();
        assert_eq!(health.operational_health().consecutive_failures, 0);
        assert_eq!(health.operational_health().last_failure, None);
    }
}
