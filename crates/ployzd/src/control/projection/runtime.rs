//! Controller-owned passive runtime projection fanout.

use crate::control::intent::service::NatsIntentReader;
use crate::control::projection::runtime_state::{
    RuntimeIngressSources, from_sources as runtime_snapshot_from_sources, load_ingress_sources,
};
use crate::control::role_client::machine::{NatsMachineFactsReader, read_available_machine_facts};
use crate::control::store::CoreStore;
use crate::process_support::BackoffSchedule;
use crate::role_testimony::RoleTestimonyCache;
use crate::service_catalog::{runtime_projection_service, runtime_snapshot_seed_endpoint_spec};
use futures_util::StreamExt;
use ployz_core::intent::IntentSnapshot;
use ployz_nats::service_protocol::NatsServiceError;
use ployz_nats::service_runtime::{
    NatsServiceHealthReader, NatsServiceResponse, NatsServiceShutdownError, RunningNatsService,
    decode_json_request, start_nats_service,
};
use ployz_nats::subjects::{INGRESS_ENDPOINT_CHANGED, INTENT_CHANGED, RUNTIME_SNAPSHOT_STREAM};
use ployz_sdk_types::{
    ControlRuntimeProjectionHealth, ControlRuntimeProjectionLoopHealth,
    ControlRuntimeProjectionServiceHealth, RuntimeSnapshot,
};
use serde::Deserialize;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const NATS_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const STORAGE_ALARM_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
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
    pub(crate) fn health_reader(&self) -> RuntimeProjectionHealthReader {
        RuntimeProjectionHealthReader {
            health: self.health.clone(),
            seed_service: self.seed_service.health_reader(),
        }
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
struct RuntimeProjectionHealthState {
    pub projection: RuntimeProjectionLoopHealth,
    pub publisher: RuntimeProjectionLoopHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuntimeProjectionLoopHealth {
    pub running: bool,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone)]
struct RuntimeProjectionHealth {
    state: Arc<Mutex<RuntimeProjectionHealthState>>,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeProjectionHealthReader {
    health: RuntimeProjectionHealth,
    seed_service: NatsServiceHealthReader,
}

impl RuntimeProjectionHealthReader {
    #[must_use]
    pub(crate) fn snapshot(&self) -> ControlRuntimeProjectionHealth {
        let state = self.health.snapshot();
        let seed = self.seed_service.snapshot();
        ControlRuntimeProjectionHealth {
            projection: ControlRuntimeProjectionLoopHealth {
                running: state.projection.running,
                consecutive_failures: state.projection.consecutive_failures,
            },
            publisher: ControlRuntimeProjectionLoopHealth {
                running: state.publisher.running,
                consecutive_failures: state.publisher.consecutive_failures,
            },
            seed_service: ControlRuntimeProjectionServiceHealth {
                endpoint_tasks_started: seed.endpoint_tasks_started,
                endpoint_tasks_finished: seed.endpoint_tasks_finished,
                request_timeouts: seed.request_timeouts,
                handler_failures: seed.handler_failures,
                domain_failures: seed.domain_failures,
                response_failures: seed.response_failures,
            },
        }
    }
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
    // Alarm liveness comes from the latest point-of-use gather. The cache
    // remains last-known display evidence and never answers whether a machine
    // is currently silent. None means no gather has completed for the current
    // intent generation; Some(empty) means a completed gather had no responders.
    fresh_storage_testimony: Option<Vec<ployz_core::machine::MachineStorageTestimony>>,
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
                fresh_storage_testimony: None,
                snapshots,
            },
            receiver,
        )
    }

    fn replace_sources(
        &mut self,
        intent: IntentSnapshot,
        ingress: RuntimeIngressSources,
        fresh_storage_testimony: Option<Vec<ployz_core::machine::MachineStorageTestimony>>,
    ) {
        self.intent = Some(intent);
        self.ingress = Some(ingress);
        self.fresh_storage_testimony = fresh_storage_testimony;
        self.refresh();
    }

    fn replace_intent_and_ingress(
        &mut self,
        intent: IntentSnapshot,
        ingress: RuntimeIngressSources,
    ) {
        self.replace_sources(intent, ingress, None);
    }

    fn refresh_with_storage_testimony(
        &mut self,
        fresh_storage_testimony: Vec<ployz_core::machine::MachineStorageTestimony>,
    ) {
        self.fresh_storage_testimony = Some(fresh_storage_testimony);
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
            self.fresh_storage_testimony.as_deref(),
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
            facts_reader: NatsMachineFactsReader::new(client.clone()),
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
    facts_reader: NatsMachineFactsReader,
    core_store: CoreStore,
}

type StorageAlarmGatherOutput = Vec<ployz_core::machine::MachineStorageTestimony>;

struct StorageAlarmGather {
    intent_generation: u64,
    task: Option<JoinHandle<(u64, StorageAlarmGatherOutput)>>,
}

impl StorageAlarmGather {
    fn new() -> Self {
        Self {
            intent_generation: 0,
            task: None,
        }
    }

    fn replace(&mut self, gather: impl Future<Output = StorageAlarmGatherOutput> + Send + 'static) {
        self.intent_generation = self.intent_generation.wrapping_add(1);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.start(gather);
    }

    fn start_if_idle<Gather, GatherFuture>(&mut self, gather: Gather) -> bool
    where
        Gather: FnOnce() -> GatherFuture,
        GatherFuture: Future<Output = StorageAlarmGatherOutput> + Send + 'static,
    {
        if self.task.is_some() {
            return false;
        }
        self.start(gather());
        true
    }

    fn start(&mut self, gather: impl Future<Output = StorageAlarmGatherOutput> + Send + 'static) {
        let generation = self.intent_generation;
        self.task = Some(tokio::spawn(async move { (generation, gather.await) }));
    }

    fn is_running(&self) -> bool {
        self.task.is_some()
    }

    async fn join(&mut self) -> Option<StorageAlarmGatherOutput> {
        let result = {
            let task = self.task.as_mut()?;
            task.await
        };
        self.task = None;
        let Ok((generation, testimony)) = result else {
            return None;
        };
        self.accept(generation, testimony)
    }

    fn accept(
        &self,
        generation: u64,
        testimony: StorageAlarmGatherOutput,
    ) -> Option<StorageAlarmGatherOutput> {
        (generation == self.intent_generation).then_some(testimony)
    }
}

impl Drop for StorageAlarmGather {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
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
        facts_reader,
        core_store,
    } = sources;
    let intent = retry_intent_read(&intent_reader, &health).await;
    let ingress = retry_ingress_read(&core_store, &health).await;
    let machine_ids = storage_testimony_machine_ids(&intent);
    projection.replace_sources(intent, ingress, None);
    let mut storage_alarm_gather = StorageAlarmGather::new();
    storage_alarm_gather.replace(gather_storage_testimony(facts_reader.clone(), machine_ids));
    let mut storage_alarm_refresh = tokio::time::interval(STORAGE_ALARM_REFRESH_INTERVAL);
    storage_alarm_refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    storage_alarm_refresh.tick().await;
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
                }
                let intent = retry_intent_read(&intent_reader, &health).await;
                let ingress = retry_ingress_read(&core_store, &health).await;
                let machine_ids = storage_testimony_machine_ids(&intent);
                projection.replace_intent_and_ingress(intent, ingress);
                storage_alarm_gather.replace(gather_storage_testimony(
                    facts_reader.clone(),
                    machine_ids,
                ));
            }
            message = ingress_changes.next() => {
                if message.is_none() {
                    health.projection_failed();
                    ingress_changes = retry_subscription(&client, INGRESS_ENDPOINT_CHANGED, &health).await;
                }
                let ingress = retry_ingress_read(&core_store, &health).await;
                projection.ingress = Some(ingress);
                projection.refresh();
            }
            testimony = storage_alarm_gather.join(), if storage_alarm_gather.is_running() => {
                if let Some(testimony) = testimony {
                    projection.refresh_with_storage_testimony(testimony);
                }
            }
            _ = storage_alarm_refresh.tick() => {
                let Some(intent) = projection.intent.as_ref() else {
                    continue;
                };
                let machine_ids = storage_testimony_machine_ids(intent);
                let facts_reader = facts_reader.clone();
                storage_alarm_gather.start_if_idle(move || {
                    gather_storage_testimony(facts_reader, machine_ids)
                });
            }
        }
    }
}

fn storage_testimony_machine_ids(intent: &IntentSnapshot) -> Vec<ployz_core::ids::MachineId> {
    intent
        .active_machines
        .iter()
        .map(|machine| machine.machine_id.clone())
        .collect()
}

async fn gather_storage_testimony(
    facts_reader: NatsMachineFactsReader,
    machine_ids: Vec<ployz_core::ids::MachineId>,
) -> StorageAlarmGatherOutput {
    read_available_machine_facts(&facts_reader, machine_ids)
        .await
        .into_iter()
        .map(|facts| {
            ployz_core::machine::MachineStorageTestimony::new(
                facts.machine_id().clone(),
                facts.storage().cloned(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{DatasetName, VolumeMaxSizeBytes, VolumeName, ZfsPoolName};
    use ployz_core::intent::ActiveMachineState;
    use ployz_core::intent::recovery::ControlPlaneEpoch;
    use ployz_core::intent::{VolumeKind, VolumePinState};
    use ployz_core::machine::GatewayServingStatus;
    use ployz_core::machine::GatewayStatusObservation;
    use ployz_core::machine::MachineLifecycle;
    use ployz_core::machine::MachineName;
    use ployz_core::machine::runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_core::machine::{MachineStorageTestimony, StorageCapability, StrandedVolumeReason};
    use ployz_core::network::{DataplaneProjection, MachineEndpointSubnet, WireGuardPublicKey};
    use ployz_core::roles::InstallRolePolicy;

    use ployz_sdk_types::{MachineTestimony, RuntimeDerivedCollectionStatus};
    use ployz_test_support::containers;
    use ployz_test_support::fixtures::{serving_target_entry, test_disk_space};
    use ployz_test_support::ids::{machine_id, operation_id};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

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
            None,
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

    #[test]
    fn passive_projection_replaces_last_known_ready_with_fresh_silence_alarm() {
        let facts = RoleTestimonyCache::default();
        facts.record_machine_facts(machine_facts_with_storage(
            "machine_a",
            "ctr_initial",
            Some(StorageCapability::Ready {
                pool: ZfsPoolName::try_new("tank").expect("valid pool"),
                capacity: ployz_core::machine::PoolCapacityFacts {
                    available_bytes: 1024 * 1024,
                    child_quotas: Vec::new(),
                },
            }),
        ));
        let (mut projection, snapshots) = PassiveRuntimeProjection::new(facts);
        let mut cluster_intent = intent(Vec::new());
        cluster_intent.volume_pins = vec![provisioned_pin("data", "machine_a", "tank")];
        projection.replace_sources(
            cluster_intent,
            ingress(),
            Some(vec![MachineStorageTestimony::new(
                machine_id("machine_a"),
                Some(StorageCapability::Ready {
                    pool: ZfsPoolName::try_new("tank").expect("valid pool"),
                    capacity: ployz_core::machine::PoolCapacityFacts {
                        available_bytes: 1024 * 1024,
                        child_quotas: Vec::new(),
                    },
                }),
            )]),
        );
        {
            let snapshot = snapshots.borrow();
            let [machine] = snapshot
                .as_ref()
                .expect("initialized snapshot")
                .machines
                .as_slice()
            else {
                panic!("one projected machine");
            };
            assert!(machine.storage_alarms.is_empty());
        }

        projection.refresh_with_storage_testimony(Vec::new());

        let snapshot = snapshots.borrow();
        let [machine] = snapshot
            .as_ref()
            .expect("initialized snapshot")
            .machines
            .as_slice()
        else {
            panic!("one projected machine");
        };
        let [alarm] = machine.storage_alarms.as_slice() else {
            panic!("one stranded-volume alarm");
        };
        assert_eq!(alarm.namespace_id.as_str(), "default");
        assert_eq!(alarm.volume_name.as_str(), "data");
        assert_eq!(alarm.reason, StrandedVolumeReason::MachineSilent);
        let MachineTestimony::Answered { storage, .. } = &machine.testimony else {
            panic!("last-known display testimony remains available");
        };
        assert!(matches!(storage, Some(StorageCapability::Ready { .. })));
    }

    #[test]
    fn initial_unknown_storage_testimony_suppresses_stranded_alarms() {
        let facts = RoleTestimonyCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (mut projection, snapshots) = PassiveRuntimeProjection::new(facts);
        let mut cluster_intent = intent(vec![serving_target_entry("svc_api", "entry_2")]);
        cluster_intent.volume_pins = vec![provisioned_pin("data", "machine_a", "tank")];

        projection.replace_sources(cluster_intent, ingress(), None);

        let snapshot = snapshots.borrow();
        let snapshot = snapshot.as_ref().expect("initialized snapshot");
        let [machine] = snapshot.machines.as_slice() else {
            panic!("one projected machine");
        };
        assert!(machine.storage_alarms.is_empty());
        assert_eq!(snapshot.services.len(), 1);
        assert_eq!(snapshot.containers.len(), 1);
    }

    #[test]
    fn intent_replacement_invalidates_storage_testimony_before_publishing() {
        let facts = RoleTestimonyCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (mut projection, snapshots) = PassiveRuntimeProjection::new(facts);
        let mut previous_intent = intent(Vec::new());
        previous_intent.volume_pins = vec![provisioned_pin("data", "machine_a", "tank")];
        projection.replace_sources(
            previous_intent,
            ingress(),
            Some(vec![MachineStorageTestimony::new(
                machine_id("machine_a"),
                None,
            )]),
        );

        let mut replacement_intent = intent(vec![serving_target_entry("svc_api", "entry_2")]);
        replacement_intent.volume_pins = vec![provisioned_pin("data", "machine_a", "tank")];
        projection.replace_intent_and_ingress(replacement_intent, ingress());

        let snapshot = snapshots.borrow();
        let snapshot = snapshot.as_ref().expect("replacement snapshot");
        let [machine] = snapshot.machines.as_slice() else {
            panic!("one projected machine");
        };
        assert!(machine.storage_alarms.is_empty());
        assert_eq!(snapshot.services.len(), 1);
    }

    #[tokio::test]
    async fn pending_storage_alarm_gather_does_not_block_projection_signals() {
        let mut gather = StorageAlarmGather::new();
        gather.replace(std::future::pending());
        let (signal, mut signals) = watch::channel(0_u8);
        signal.send_replace(1);

        tokio::select! {
            testimony = gather.join() => panic!("pending gather completed: {testimony:?}"),
            changed = signals.changed() => changed.expect("projection signal remains live"),
        }

        assert_eq!(*signals.borrow_and_update(), 1);
    }

    #[test]
    fn storage_alarm_gather_rejects_a_stale_intent_generation() {
        let gather = StorageAlarmGather {
            intent_generation: 2,
            task: None,
        };

        assert!(gather.accept(1, Vec::new()).is_none());
        assert_eq!(gather.accept(2, Vec::new()), Some(Vec::new()));
    }

    #[tokio::test]
    async fn replacing_storage_alarm_gather_cancels_the_previous_intent() {
        struct NotifyOnDrop(Option<oneshot::Sender<()>>);

        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                if let Some(notify) = self.0.take() {
                    let _ = notify.send(());
                }
            }
        }

        let (dropped, previous_dropped) = oneshot::channel();
        let mut gather = StorageAlarmGather::new();
        gather.replace(async move {
            let _notify = NotifyOnDrop(Some(dropped));
            std::future::pending().await
        });
        tokio::task::yield_now().await;

        gather.replace(async { Vec::new() });

        timeout(std::time::Duration::from_secs(1), previous_dropped)
            .await
            .expect("previous gather cancellation is bounded")
            .expect("previous gather reports cancellation");
        assert_eq!(gather.join().await, Some(Vec::new()));
    }

    #[tokio::test]
    async fn periodic_storage_alarm_gathers_do_not_overlap() {
        let starts = AtomicUsize::new(0);
        let mut gather = StorageAlarmGather::new();

        assert!(gather.start_if_idle(|| {
            starts.fetch_add(1, Ordering::Relaxed);
            std::future::pending()
        }));
        assert!(!gather.start_if_idle(|| {
            starts.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Vec::new())
        }));
        assert_eq!(starts.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn intent_machine_and_gateway_replacements_emit_full_snapshots() {
        let facts = RoleTestimonyCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (mut projection, mut snapshots) = PassiveRuntimeProjection::new(facts.clone());
        projection.replace_sources(intent(Vec::new()), ingress(), Some(Vec::new()));
        let _ = snapshots.borrow_and_update();

        projection.replace_sources(
            intent(vec![serving_target_entry("svc_api", "entry_2")]),
            ingress(),
            Some(Vec::new()),
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
            process_health: ployz_core::machine::GatewayProcessHealth::default(),
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
        serving_target_entries: Vec<ployz_core::intent::ServingTargetEntry>,
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
        machine_facts_with_storage(machine, container, None)
    }

    fn machine_facts_with_storage(
        machine: &str,
        container: &str,
        storage: Option<StorageCapability>,
    ) -> MachineFactsSnapshot {
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
            storage,
            ployz_core::image::OciPlatform::current(),
            1_000,
        )
        .expect("machine facts")
    }

    fn provisioned_pin(volume: &str, machine: &str, pool: &str) -> VolumePinState {
        let namespace_id = ployz_test_support::ids::namespace_id("default");
        let volume_name = VolumeName::try_new(volume).expect("valid volume name");
        let pool = ZfsPoolName::try_new(pool).expect("valid pool");
        VolumePinState::try_new(
            namespace_id.clone(),
            volume_name.clone(),
            machine_id(machine),
            VolumeKind::Provisioned {
                dataset: DatasetName::for_volume(&pool, &namespace_id, &volume_name)
                    .expect("canonical dataset"),
                max_size_bytes: VolumeMaxSizeBytes::try_new(1024).expect("non-zero size"),
            },
        )
        .expect("valid provisioned pin")
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
    fresh_storage_testimony: Option<&[ployz_core::machine::MachineStorageTestimony]>,
) -> RuntimeSnapshot {
    let (machine_facts, gateway_statuses) = facts.runtime_projection_facts();
    runtime_snapshot_from_sources(
        intent,
        &machine_facts,
        fresh_storage_testimony,
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
