//! Controller-owned passive runtime projection fanout.

use crate::fact_cache::FactCache;
use crate::intent::service::{IntentReadError, NatsIntentReader};
use crate::process_support::BackoffSchedule;
use crate::runtime_snapshot::from_sources as runtime_snapshot_from_sources;
use crate::service_catalog::{runtime_projection_service, runtime_snapshot_seed_endpoint_spec};
use futures_util::StreamExt;
use ployz_core::state::IntentSnapshot;
use ployz_core::subjects::{INTENT_CHANGED, RUNTIME_SNAPSHOT_STREAM};
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
    intent: IntentSnapshot,
    facts: FactCache,
    snapshots: watch::Sender<RuntimeSnapshot>,
}

impl PassiveRuntimeProjection {
    fn new(intent: IntentSnapshot, facts: FactCache) -> (Self, watch::Receiver<RuntimeSnapshot>) {
        let snapshot = assemble_snapshot(intent.clone(), &facts);
        let (snapshots, receiver) = watch::channel(snapshot);
        (
            Self {
                intent,
                facts,
                snapshots,
            },
            receiver,
        )
    }

    fn replace_intent(&mut self, intent: IntentSnapshot) {
        self.intent = intent;
        self.refresh();
    }

    fn refresh(&self) {
        self.snapshots
            .send_replace(assemble_snapshot(self.intent.clone(), &self.facts));
    }
}

pub(crate) async fn start_runtime_projection(
    client: async_nats::Client,
    intent_reader: NatsIntentReader,
    facts: FactCache,
) -> Result<RunningRuntimeProjection, RuntimeProjectionStartError> {
    let intent_changes = subscribe_intent(&client)
        .await
        .map_err(|message| RuntimeProjectionStartError::SubscribeIntent { message })?;
    let initial_intent = intent_reader
        .intent()
        .await
        .map_err(RuntimeProjectionStartError::LoadInitialIntent)?;
    let fact_changes = facts.subscribe_changes();
    let (projection, snapshots) = PassiveRuntimeProjection::new(initial_intent, facts);
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
                NatsServiceResponse::json_ok(&snapshots.borrow().clone())
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
        client.clone(),
        intent_reader,
        intent_changes,
        fact_changes,
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

async fn run_projection(
    mut projection: PassiveRuntimeProjection,
    client: async_nats::Client,
    intent_reader: NatsIntentReader,
    mut intent_changes: async_nats::Subscriber,
    mut fact_changes: watch::Receiver<u64>,
    health: RuntimeProjectionHealth,
) {
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
                    intent_changes = retry_intent_subscription(&client, &health).await;
                    continue;
                }
                let intent = retry_intent_read(&intent_reader, &health).await;
                projection.replace_intent(intent);
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
        MachineLifecycle, ManagedLeaseProjection,
    };
    use ployz_sdk_types::{MachineTestimony, RuntimeDerivedCollectionStatus};
    use ployz_test_support::containers;
    use ployz_test_support::fixtures::{serving_target_entry, test_disk_space};
    use ployz_test_support::ids::{machine_id, operation_id};

    #[test]
    fn initial_passive_projection_is_complete() {
        let facts = FactCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (_projection, snapshots) = PassiveRuntimeProjection::new(
            intent(vec![serving_target_entry("svc_api", "entry_2")]),
            facts,
        );
        let snapshot = snapshots.borrow();

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
        let facts = FactCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (mut projection, mut snapshots) =
            PassiveRuntimeProjection::new(intent(Vec::new()), facts.clone());

        projection.replace_intent(intent(vec![serving_target_entry("svc_api", "entry_2")]));
        snapshots.changed().await.expect("intent replacement");
        assert_eq!(snapshots.borrow_and_update().services.len(), 1);

        facts.record_machine_facts(machine_facts("machine_a", "ctr_replacement"));
        projection.refresh();
        snapshots.changed().await.expect("machine replacement");
        {
            let replacement = snapshots.borrow_and_update();
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
            public_url_mode: ployz_core::cert::PublicUrlMode::Auto,
            managed_lease: ManagedLeaseProjection::Unacquired,
            custom_certificates: Vec::new(),
            acme_http01_challenges: Vec::new(),
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
    mut snapshots: watch::Receiver<RuntimeSnapshot>,
    health: RuntimeProjectionHealth,
) {
    let mut retry_delay = RETRY_SCHEDULE.interval;
    loop {
        let snapshot = snapshots.borrow_and_update().clone();
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
    snapshots: &mut watch::Receiver<RuntimeSnapshot>,
) -> bool {
    tokio::select! {
        () = tokio::time::sleep(delay) => false,
        result = snapshots.changed() => result.is_err(),
    }
}

async fn subscribe_intent(client: &async_nats::Client) -> Result<async_nats::Subscriber, String> {
    let subscription = timeout(NATS_IO_TIMEOUT, client.subscribe(INTENT_CHANGED))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    timeout(NATS_IO_TIMEOUT, client.flush())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    Ok(subscription)
}

async fn retry_intent_subscription(
    client: &async_nats::Client,
    health: &RuntimeProjectionHealth,
) -> async_nats::Subscriber {
    let mut delay = RETRY_SCHEDULE.interval;
    loop {
        match subscribe_intent(client).await {
            Ok(subscription) => return subscription,
            Err(error) => {
                health.projection_failed();
                warn_failure("subscribe-intent", health, &error);
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

fn assemble_snapshot(intent: IntentSnapshot, facts: &FactCache) -> RuntimeSnapshot {
    let (machine_facts, gateway_statuses) = facts.runtime_projection_facts();
    runtime_snapshot_from_sources(
        intent,
        &machine_facts,
        &gateway_statuses,
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
    #[error("failed to load initial intent: {0}")]
    LoadInitialIntent(IntentReadError),
    #[error("failed to start runtime snapshot seed service: {0}")]
    StartSeedService(ployz_nats::service_runtime::NatsServiceRuntimeError),
}
