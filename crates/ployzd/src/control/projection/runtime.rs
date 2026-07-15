//! Controller-owned passive runtime projection fanout.

use crate::control::intent::service::NatsIntentReader;
use crate::control::projection::runtime_state::{
    RuntimeIngressSources, from_sources as runtime_snapshot_from_sources, load_ingress_sources,
};
use crate::control::store::CoreStore;
use crate::process_support::BackoffSchedule;
use crate::role_testimony::RoleTestimonyCache;
use crate::service_catalog::{runtime_projection_service, runtime_snapshot_seed_endpoint_spec};
use futures_util::StreamExt;
use ployz_core::state::IntentSnapshot;
use ployz_core::subjects::{INGRESS_ENDPOINT_CHANGED, INTENT_CHANGED, RUNTIME_SNAPSHOT_STREAM};
use ployz_nats::service_protocol::NatsServiceError;
use ployz_nats::service_runtime::{
    NatsServiceHealth, NatsServiceResponse, NatsServiceShutdownError, RunningNatsService,
    decode_json_request, start_nats_service,
};
use ployz_sdk_types::RuntimeSnapshot;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const NATS_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const RETRY_SCHEDULE: BackoffSchedule = BackoffSchedule {
    interval: std::time::Duration::from_millis(100),
    cap: std::time::Duration::from_secs(5),
};

pub(crate) struct RunningRuntimeProjection {
    projection_task: JoinHandle<()>,
    publisher_task: JoinHandle<()>,
    seed_service: RunningNatsService,
    health: RuntimeProjectionHealth,
}

impl RunningRuntimeProjection {
    #[must_use]
    pub(crate) fn health(&self) -> RuntimeProjectionHealthState {
        let mut snapshot = self.health.snapshot();
        snapshot.seed = self.seed_service.health();
        snapshot
    }

    pub(crate) async fn shutdown(self) -> Result<(), NatsServiceShutdownError> {
        let Self {
            projection_task,
            publisher_task,
            seed_service,
            health: _,
        } = self;
        let seed_result = seed_service.shutdown().await;
        projection_task.abort();
        publisher_task.abort();
        let _ = projection_task.await;
        let _ = publisher_task.await;
        seed_result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeProjectionHealthState {
    pub projection: RuntimeProjectionLoopHealth,
    pub publisher: RuntimeProjectionLoopHealth,
    pub seed: NatsServiceHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeProjectionLoopHealth {
    pub running: bool,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone)]
struct RuntimeProjectionHealth {
    state: Arc<Mutex<RuntimeProjectionHealthState>>,
}

impl Default for RuntimeProjectionHealth {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(RuntimeProjectionHealthState {
                projection: RuntimeProjectionLoopHealth {
                    running: true,
                    consecutive_failures: 0,
                },
                publisher: RuntimeProjectionLoopHealth {
                    running: true,
                    consecutive_failures: 0,
                },
                seed: NatsServiceHealth {
                    endpoint_tasks_started: 0,
                    endpoint_tasks_finished: 0,
                    request_timeouts: 0,
                    handler_failures: 0,
                    domain_failures: 0,
                    response_failures: 0,
                },
            })),
        }
    }
}

impl RuntimeProjectionHealth {
    fn snapshot(&self) -> RuntimeProjectionHealthState {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn projection_succeeded(&self) {
        self.update(|state| state.projection.consecutive_failures = 0);
    }

    fn projection_failed(&self) {
        self.update(|state| {
            state.projection.consecutive_failures =
                state.projection.consecutive_failures.saturating_add(1);
        });
    }

    fn projection_stopped(&self) {
        self.update(|state| {
            state.projection.running = false;
            state.projection.consecutive_failures =
                state.projection.consecutive_failures.saturating_add(1);
        });
    }

    fn publisher_succeeded(&self) {
        self.update(|state| state.publisher.consecutive_failures = 0);
    }

    fn publisher_failed(&self) {
        self.update(|state| {
            state.publisher.consecutive_failures =
                state.publisher.consecutive_failures.saturating_add(1);
        });
    }

    fn publisher_stopped(&self) {
        self.update(|state| {
            state.publisher.running = false;
            state.publisher.consecutive_failures =
                state.publisher.consecutive_failures.saturating_add(1);
        });
    }

    fn update(&self, update: impl FnOnce(&mut RuntimeProjectionHealthState)) {
        update(
            &mut self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshotSeedRequest {}

struct PassiveRuntimeProjection {
    intent: Option<IntentSnapshot>,
    ingress: Option<RuntimeIngressSources>,
    facts: RoleTestimonyCache,
    snapshots: watch::Sender<Option<RuntimeSnapshot>>,
}

impl PassiveRuntimeProjection {
    fn new(facts: RoleTestimonyCache) -> (Self, watch::Receiver<Option<RuntimeSnapshot>>) {
        let (snapshots, receiver) = watch::channel(None);
        (
            Self {
                intent: None,
                ingress: None,
                facts,
                snapshots,
            },
            receiver,
        )
    }

    fn replace_sources(&mut self, intent: IntentSnapshot, ingress: RuntimeIngressSources) {
        self.intent = Some(intent);
        self.ingress = Some(ingress);
        self.refresh();
    }

    fn refresh(&self) {
        let (Some(intent), Some(ingress)) = (&self.intent, &self.ingress) else {
            return;
        };
        self.snapshots.send_replace(Some(assemble_snapshot(
            intent.clone(),
            ingress.clone(),
            &self.facts,
        )));
    }
}

pub(crate) async fn start_runtime_projection(
    client: async_nats::Client,
    intent_reader: NatsIntentReader,
    facts: RoleTestimonyCache,
    core_store: CoreStore,
) -> Result<RunningRuntimeProjection, RuntimeProjectionStartError> {
    let intent_changes = subscribe(&client, INTENT_CHANGED)
        .await
        .map_err(|message| RuntimeProjectionStartError::SubscribeIntent { message })?;
    let ingress_changes = subscribe(&client, INGRESS_ENDPOINT_CHANGED)
        .await
        .map_err(|message| RuntimeProjectionStartError::SubscribeIngress { message })?;
    let fact_changes = facts.subscribe_changes();
    let (projection, snapshots) = PassiveRuntimeProjection::new(facts);
    let seed_snapshots = snapshots.clone();
    let mut seed_service = start_nats_service(client.clone(), &runtime_projection_service())
        .await
        .map_err(RuntimeProjectionStartError::StartSeedService)?;
    if let Err(error) = seed_service
        .bind_endpoint(&runtime_snapshot_seed_endpoint_spec(), move |request| {
            let snapshots = seed_snapshots.clone();
            async move {
                if let Err(response) = decode_json_request::<RuntimeSnapshotSeedRequest>(&request) {
                    return response;
                }
                let Some(snapshot) = snapshots.borrow().clone() else {
                    return NatsServiceResponse::transport_error(NatsServiceError::unavailable(
                        "runtime snapshot is not initialized",
                    ));
                };
                NatsServiceResponse::json_ok(&snapshot)
            }
        })
        .await
    {
        let _ = seed_service.shutdown().await;
        return Err(RuntimeProjectionStartError::StartSeedService(error));
    }
    let health = RuntimeProjectionHealth::default();
    let projection_task = tokio::spawn(run_projection(
        projection,
        RuntimeProjectionSources {
            client: client.clone(),
            intent_reader,
            intent_changes,
            ingress_changes,
            fact_changes,
            core_store,
        },
        health.clone(),
    ));
    let publisher_task = tokio::spawn(publish_snapshots(client, snapshots, health.clone()));

    Ok(RunningRuntimeProjection {
        projection_task,
        publisher_task,
        seed_service,
        health,
    })
}

struct RuntimeProjectionSources {
    client: async_nats::Client,
    intent_reader: NatsIntentReader,
    intent_changes: async_nats::Subscriber,
    ingress_changes: async_nats::Subscriber,
    fact_changes: watch::Receiver<u64>,
    core_store: CoreStore,
}

async fn run_projection(
    mut projection: PassiveRuntimeProjection,
    sources: RuntimeProjectionSources,
    health: RuntimeProjectionHealth,
) {
    let RuntimeProjectionSources {
        client,
        intent_reader,
        mut intent_changes,
        mut ingress_changes,
        mut fact_changes,
        core_store,
    } = sources;
    let intent = retry_intent_read(&intent_reader, &health).await;
    let ingress = retry_ingress_read(&core_store, &health).await;
    projection.replace_sources(intent, ingress);
    loop {
        tokio::select! {
            result = fact_changes.changed() => {
                if result.is_err() {
                    health.projection_stopped();
                    break;
                }
                projection.refresh();
            }
            message = intent_changes.next() => {
                if message.is_none() {
                    health.projection_failed();
                    intent_changes = retry_subscription(&client, INTENT_CHANGED, &health).await;
                    continue;
                }
                let intent = retry_intent_read(&intent_reader, &health).await;
                let ingress = retry_ingress_read(&core_store, &health).await;
                projection.replace_sources(intent, ingress);
            }
            message = ingress_changes.next() => {
                if message.is_none() {
                    health.projection_failed();
                    ingress_changes = retry_subscription(&client, INGRESS_ENDPOINT_CHANGED, &health).await;
                    continue;
                }
                let ingress = retry_ingress_read(&core_store, &health).await;
                projection.ingress = Some(ingress);
                projection.refresh();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::dataplane::{DataplaneProjection, MachineEndpointSubnet, WireGuardPublicKey};
    use ployz_core::machine::MachineName;
    use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_core::state::{
        ActiveMachineState, ControlPlaneEpoch, GatewayServingStatus, GatewayStatusObservation,
        MachineLifecycle,
    };
    use ployz_sdk_types::{MachineTestimony, RuntimeDerivedCollectionStatus};
    use ployz_test_support::containers;
    use ployz_test_support::fixtures::{serving_target_entry, test_disk_space};
    use ployz_test_support::ids::{machine_id, operation_id};

    #[test]
    fn unconfigured_projection_withholds_a_runtime_snapshot() {
        let (_projection, snapshots) = PassiveRuntimeProjection::new(RoleTestimonyCache::default());

        assert!(snapshots.borrow().is_none());
    }

    #[test]
    fn initial_passive_projection_is_complete() {
        let facts = RoleTestimonyCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (mut projection, snapshots) = PassiveRuntimeProjection::new(facts);
        projection.replace_sources(
            intent(vec![serving_target_entry("svc_api", "entry_2")]),
            ingress(),
        );
        let snapshot = snapshots.borrow();
        let snapshot = snapshot.as_ref().expect("initialized snapshot");

        assert_eq!(snapshot.machines.len(), 1);
        let [container] = snapshot.containers.as_slice() else {
            panic!("one projected container");
        };
        assert_eq!(container.container_id.as_str(), "ctr_initial");
        assert_eq!(
            snapshot.projection_sources.revisions.status,
            RuntimeDerivedCollectionStatus::Complete
        );
    }

    #[tokio::test]
    async fn intent_machine_and_gateway_replacements_emit_full_snapshots() {
        let facts = RoleTestimonyCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (mut projection, mut snapshots) = PassiveRuntimeProjection::new(facts.clone());
        projection.replace_sources(intent(Vec::new()), ingress());
        let _ = snapshots.borrow_and_update();

        projection.replace_sources(
            intent(vec![serving_target_entry("svc_api", "entry_2")]),
            ingress(),
        );
        snapshots.changed().await.expect("intent replacement");
        assert_eq!(
            snapshots
                .borrow_and_update()
                .as_ref()
                .expect("initialized snapshot")
                .services
                .len(),
            1
        );

        facts.record_machine_facts(machine_facts("machine_a", "ctr_replacement"));
        projection.refresh();
        snapshots.changed().await.expect("machine replacement");
        {
            let replacement = snapshots.borrow_and_update();
            let replacement = replacement.as_ref().expect("initialized snapshot");
            let [container] = replacement.containers.as_slice() else {
                panic!("one replacement container");
            };
            assert_eq!(container.container_id.as_str(), "ctr_replacement");
        }

        facts.record_gateway_status(GatewayStatusObservation {
            machine_id: machine_id("machine_a"),
            listen_addr: "127.0.0.1:443".parse().expect("socket address"),
            serving: GatewayServingStatus::Current,
            route_count: 3,
        });
        projection.refresh();
        snapshots.changed().await.expect("gateway replacement");
        let replacement = snapshots.borrow_and_update();
        let replacement = replacement.as_ref().expect("initialized snapshot");
        let [machine] = replacement.machines.as_slice() else {
            panic!("one projected machine");
        };
        let MachineTestimony::Answered { gateway, .. } = &machine.testimony else {
            panic!("machine testimony answered");
        };
        assert_eq!(gateway.as_ref().expect("gateway testimony").route_count, 3);
    }

    #[test]
    fn health_records_degradation_and_recovery() {
        let health = RuntimeProjectionHealth::default();
        assert_eq!(health.snapshot().projection.consecutive_failures, 0);

        health.projection_failed();
        health.publisher_failed();
        let degraded = health.snapshot();
        assert_eq!(degraded.projection.consecutive_failures, 1);
        assert_eq!(degraded.publisher.consecutive_failures, 1);

        health.projection_succeeded();
        health.publisher_succeeded();
        let healthy = health.snapshot();
        assert_eq!(healthy.projection.consecutive_failures, 0);
        assert_eq!(healthy.publisher.consecutive_failures, 0);
    }

    fn intent(
        serving_target_entries: Vec<ployz_core::state::ServingTargetEntry>,
    ) -> IntentSnapshot {
        let machine = active_machine();
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: machine.machine_id.clone(),
            dataplane_projection: DataplaneProjection::try_new(Vec::new(), None)
                .expect("dataplane projection"),
            active_machines: vec![machine],
            route_bindings: Vec::new(),
            serving_target_entries,
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration:
                ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
            active_certificates: Vec::new(),
        }
    }

    fn ingress() -> RuntimeIngressSources {
        RuntimeIngressSources {
            ployz_dns_target_allocation: None,
            ployz_dns_target_checkpoint: None,
            ingress_endpoint_projection: ployz_core::ingress::IngressEndpointProjection {
                control_plane_epoch: ControlPlaneEpoch::initial(),
                revision: 0,
                state: ployz_core::ingress::IngressEndpointProjectionState::Pending,
            },
        }
    }

    fn active_machine() -> ActiveMachineState {
        ActiveMachineState {
            machine_id: machine_id("machine_a"),
            name: MachineName::try_new("machine-a").expect("machine name"),
            activated_by: operation_id("op_machine_add"),
            roles: InstallRolePolicy::install_all(),
            lifecycle: MachineLifecycle::Active,
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            endpoint_subnet: MachineEndpointSubnet::try_new("10.198.0.0/24")
                .expect("endpoint subnet"),
            wireguard_public_key: WireGuardPublicKey::try_new("public-machine-a")
                .expect("public key"),
        }
    }

    fn machine_facts(machine: &str, container: &str) -> MachineFactsSnapshot {
        let machine_id = machine_id(machine);
        let observation = containers::observation(machine, container)
            .with(containers::identity("svc_api").entry("entry_2"))
            .running_unroutable()
            .build();
        MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, [observation])
                .expect("container snapshot"),
            None,
            test_disk_space(),
            ployz_core::image::OciPlatform::current(),
            1_000,
        )
        .expect("machine facts")
    }
}

async fn publish_snapshots(
    client: async_nats::Client,
    mut snapshots: watch::Receiver<Option<RuntimeSnapshot>>,
    health: RuntimeProjectionHealth,
) {
    let mut retry_delay = RETRY_SCHEDULE.interval;
    loop {
        let snapshot = { snapshots.borrow_and_update().clone() };
        let Some(snapshot) = snapshot else {
            if snapshots.changed().await.is_err() {
                health.publisher_stopped();
                return;
            }
            continue;
        };
        let payload = match serde_json::to_vec(&snapshot) {
            Ok(payload) => payload,
            Err(error) => {
                health.publisher_failed();
                warn_failure("encode", &health, &error);
                if wait_for_retry_or_replacement(retry_delay, &mut snapshots).await {
                    health.publisher_stopped();
                    return;
                }
                retry_delay = RETRY_SCHEDULE.next_after_failure(retry_delay);
                continue;
            }
        };
        match timeout(
            NATS_IO_TIMEOUT,
            client.publish(RUNTIME_SNAPSHOT_STREAM, payload.into()),
        )
        .await
        {
            Ok(Ok(())) => {
                health.publisher_succeeded();
                retry_delay = RETRY_SCHEDULE.interval;
            }
            Ok(Err(error)) => {
                health.publisher_failed();
                warn_failure("publish", &health, &error);
                if wait_for_retry_or_replacement(retry_delay, &mut snapshots).await {
                    health.publisher_stopped();
                    return;
                }
                retry_delay = RETRY_SCHEDULE.next_after_failure(retry_delay);
                continue;
            }
            Err(error) => {
                health.publisher_failed();
                warn_failure("publish-timeout", &health, &error);
                if wait_for_retry_or_replacement(retry_delay, &mut snapshots).await {
                    health.publisher_stopped();
                    return;
                }
                retry_delay = RETRY_SCHEDULE.next_after_failure(retry_delay);
                continue;
            }
        }
        if snapshots.changed().await.is_err() {
            health.publisher_stopped();
            return;
        }
    }
}

async fn wait_for_retry_or_replacement(
    delay: std::time::Duration,
    snapshots: &mut watch::Receiver<Option<RuntimeSnapshot>>,
) -> bool {
    tokio::select! {
        () = tokio::time::sleep(delay) => false,
        result = snapshots.changed() => result.is_err(),
    }
}

async fn subscribe(
    client: &async_nats::Client,
    subject: &'static str,
) -> Result<async_nats::Subscriber, String> {
    let subscription = timeout(NATS_IO_TIMEOUT, client.subscribe(subject))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    timeout(NATS_IO_TIMEOUT, client.flush())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    Ok(subscription)
}

async fn retry_subscription(
    client: &async_nats::Client,
    subject: &'static str,
    health: &RuntimeProjectionHealth,
) -> async_nats::Subscriber {
    let mut delay = RETRY_SCHEDULE.interval;
    loop {
        match subscribe(client, subject).await {
            Ok(subscription) => return subscription,
            Err(error) => {
                health.projection_failed();
                warn_failure("subscribe", health, &error);
                tokio::time::sleep(delay).await;
                delay = RETRY_SCHEDULE.next_after_failure(delay);
            }
        }
    }
}

async fn retry_intent_read(
    intent_reader: &NatsIntentReader,
    health: &RuntimeProjectionHealth,
) -> IntentSnapshot {
    let mut delay = RETRY_SCHEDULE.interval;
    loop {
        match intent_reader.intent().await {
            Ok(intent) => {
                health.projection_succeeded();
                return intent;
            }
            Err(error) => {
                health.projection_failed();
                warn_failure("load-intent", health, &error);
                tokio::time::sleep(delay).await;
                delay = RETRY_SCHEDULE.next_after_failure(delay);
            }
        }
    }
}

async fn retry_ingress_read(
    store: &CoreStore,
    health: &RuntimeProjectionHealth,
) -> RuntimeIngressSources {
    let mut delay = RETRY_SCHEDULE.interval;
    loop {
        match load_ingress_sources(store).await {
            Ok(ingress) => {
                health.projection_succeeded();
                return ingress;
            }
            Err(error) => {
                health.projection_failed();
                warn_failure("load-ingress", health, &error);
                tokio::time::sleep(delay).await;
                delay = RETRY_SCHEDULE.next_after_failure(delay);
            }
        }
    }
}

fn assemble_snapshot(
    intent: IntentSnapshot,
    ingress: RuntimeIngressSources,
    facts: &RoleTestimonyCache,
) -> RuntimeSnapshot {
    let (machine_facts, gateway_statuses) = facts.runtime_projection_facts();
    runtime_snapshot_from_sources(
        intent,
        &machine_facts,
        &gateway_statuses,
        ingress,
        current_unix_seconds(),
    )
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn warn_failure(phase: &str, health: &RuntimeProjectionHealth, error: &impl std::fmt::Display) {
    let snapshot = health.snapshot();
    eprintln!(
        "ployzd runtime projection warning: phase={phase} projection_failures={} publisher_failures={} error={error}",
        snapshot.projection.consecutive_failures, snapshot.publisher.consecutive_failures,
    );
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeProjectionStartError {
    #[error("failed to subscribe to intent changes: {message}")]
    SubscribeIntent { message: String },
    #[error("failed to subscribe to ingress endpoint changes: {message}")]
    SubscribeIngress { message: String },
    #[error("failed to start runtime snapshot seed service: {0}")]
    StartSeedService(ployz_nats::service_runtime::NatsServiceRuntimeError),
}
