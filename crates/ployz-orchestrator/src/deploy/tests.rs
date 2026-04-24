use super::execute::{SessionSet, apply_with_initial_plan, ensure_plan_stable, run_phase_startup};
use super::plan::{deployable_machines, desired_slots, resolve_plan};
use crate::deploy::session::{DeploySession, DeploySessionFactory, StartCandidateRequest};
use crate::error::Result;
use crate::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId,
    MachineLifecycle, MachineRecord, OverlayIp, PublicKey, ServiceRelease, ServiceReleaseRecord,
    ServiceReleaseSlot, ServiceRoutingPolicy, SlotId,
};
use async_trait::async_trait;
use ployz_store_api::memory::{MemoryService, MemoryStore};
use ployz_store_api::{
    DeployStore, InviteStore, MachineStore, MachineSubscription, RoutingInvalidationSubscription,
    RoutingStore, StoreBackend, StoreDriver, StoreRuntimeControl,
};
use ployz_types::Result as PloyzResult;
use ployz_types::spec::{
    ContainerSpec, DeployManifest, Namespace, NetworkMode, Placement, PullPolicy, Resources,
    RestartPolicy, RolloutStrategy, ServiceSpec,
};
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv6Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::sleep;

#[test]
fn deployable_machines_filters_by_participation() {
    let machines = vec![
        test_machine("enabled-a", MachineLifecycle::Active),
        test_machine("enabled-b", MachineLifecycle::Active),
        test_machine("draining", MachineLifecycle::Draining),
    ];

    let deployable = deployable_machines(&machines, &MachineId("local".into()));
    assert_eq!(
        deployable,
        vec![MachineId("enabled-a".into()), MachineId("enabled-b".into())]
    );
}

#[test]
fn deployable_machines_falls_back_to_local_when_none_are_enabled() {
    let machines = vec![test_machine("draining", MachineLifecycle::Draining)];

    let deployable = deployable_machines(&machines, &MachineId("local".into()));
    assert_eq!(deployable, vec![MachineId("local".into())]);
}

#[test]
fn replicated_one_reuses_existing_slot_machine() {
    let spec = test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:latest");
    let machines = vec![MachineId("machine-a".into()), MachineId("machine-b".into())];
    let current_slots = [ServiceReleaseSlot {
        slot_id: SlotId("slot-0001".into()),
        machine_id: MachineId("machine-b".into()),
        active_instance_id: InstanceId("inst-1".into()),
        revision_hash: "rev-1".into(),
    }];

    let machine_map = HashMap::from([
        (
            MachineId("machine-a".into()),
            test_machine("machine-a", MachineLifecycle::Active),
        ),
        (
            MachineId("machine-b".into()),
            test_machine("machine-b", MachineLifecycle::Active),
        ),
    ]);

    let desired =
        desired_slots(&spec, &machines, Some(&current_slots), &machine_map).expect("desired slots");
    let [slot] = desired.as_slice() else {
        panic!("expected one desired slot");
    };
    assert_eq!(slot.slot_id, SlotId("slot-0001".into()));
    assert_eq!(slot.machine_id, MachineId("machine-b".into()));
}

#[tokio::test]
async fn resolve_plan_marks_matching_release_unchanged() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    assert_eq!(
        service_plan.action,
        crate::model::DeployChangeKind::Unchanged
    );
    assert_eq!(service_plan.service, "api");
}

#[tokio::test]
async fn resolve_plan_reuses_slot_machine_when_revision_changes() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.28",
    )]);
    let old_spec = test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:1.27");
    let old_revision_hash = old_spec.revision_hash().expect("old revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &old_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-1",
                &old_revision_hash,
            )],
        ))
        .await
        .expect("seed release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [service_plan] = plan.services() else {
        panic!("expected one service plan");
    };
    let [slot_plan] = service_plan.slots.as_slice() else {
        panic!("expected one slot plan");
    };
    assert_eq!(service_plan.action, crate::model::DeployChangeKind::Replace);
    assert_eq!(slot_plan.machine_id, MachineId("machine-b".into()));
}

#[tokio::test]
async fn resolve_plan_global_service_targets_enabled_machines_in_order() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);

    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Active))
        .await
        .expect("seed machine-b");
    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-c", MachineLifecycle::Draining))
        .await
        .expect("seed machine-c");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    let [resolution] = plan.services() else {
        panic!("expected one service resolution");
    };
    let desired = resolution
        .slots
        .iter()
        .map(|slot| (slot.slot_id.clone(), slot.machine_id.clone()))
        .collect::<Vec<_>>();

    assert_eq!(
        desired,
        vec![
            (
                SlotId("slot-machine-a".into()),
                MachineId("machine-a".into())
            ),
            (
                SlotId("slot-machine-b".into()),
                MachineId("machine-b".into())
            ),
        ]
    );
}

#[tokio::test]
async fn resolve_plan_includes_removed_service_participants() {
    let store = StoreDriver::memory();
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let [current_spec] = manifest.services.as_slice() else {
        panic!("expected one manifest service");
    };
    let current_revision_hash = current_spec.revision_hash().expect("current revision hash");

    store
        .upsert_self_machine(&test_machine("machine-a", MachineLifecycle::Active))
        .await
        .expect("seed machine-a");
    store
        .upsert_self_machine(&test_machine("machine-b", MachineLifecycle::Draining))
        .await
        .expect("seed machine-b");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "old-api",
            "rev-old",
            vec![test_slot("slot-0001", "machine-b", "inst-old", "rev-old")],
        ))
        .await
        .expect("seed old-api release");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &current_revision_hash,
            vec![test_slot(
                "slot-0001",
                "machine-a",
                "inst-current",
                &current_revision_hash,
            )],
        ))
        .await
        .expect("seed api release");

    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");

    assert!(plan.participants().contains(&MachineId("machine-b".into())));
}

#[tokio::test]
async fn resolve_plan_fingerprint_is_stable_across_release_insert_order() {
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 1 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 1 }, "busybox:1.0"),
    ]);
    let [api_spec, worker_spec] = manifest.services.as_slice() else {
        panic!("expected two services");
    };
    let api_revision = api_spec.revision_hash().expect("api revision");
    let worker_revision = worker_spec.revision_hash().expect("worker revision");

    let store_a = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    store_a
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &api_revision,
            vec![test_slot("slot-0001", "machine-a", "inst-a", &api_revision)],
        ))
        .await
        .expect("api release");
    store_a
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "worker",
            &worker_revision,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-b",
                &worker_revision,
            )],
        ))
        .await
        .expect("worker release");

    let store_b = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    store_b
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "worker",
            &worker_revision,
            vec![test_slot(
                "slot-0001",
                "machine-b",
                "inst-b",
                &worker_revision,
            )],
        ))
        .await
        .expect("worker release");
    store_b
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &api_revision,
            vec![test_slot("slot-0001", "machine-a", "inst-a", &api_revision)],
        ))
        .await
        .expect("api release");

    let plan_a = resolve_plan(&store_a, &local_machine_id, &manifest)
        .await
        .expect("plan a");
    let plan_b = resolve_plan(&store_b, &local_machine_id, &manifest)
        .await
        .expect("plan b");

    assert_eq!(plan_a.fingerprint(), plan_b.fingerprint());
}

#[tokio::test]
async fn session_set_opens_sessions_in_parallel_for_noop_plan() {
    let store = seeded_store_with_machines(&[
        "machine-a",
        "machine-b",
        "machine-c",
        "machine-d",
        "machine-e",
    ])
    .await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &revision_hash,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a", &revision_hash),
                test_slot("slot-machine-b", "machine-b", "inst-b", &revision_hash),
                test_slot("slot-machine-c", "machine-c", "inst-c", &revision_hash),
                test_slot("slot-machine-d", "machine-d", "inst-d", &revision_hash),
                test_slot("slot-machine-e", "machine-e", "inst-e", &revision_hash),
            ],
        ))
        .await
        .expect("seed release");

    let controller = FakeController {
        open_delay: Duration::from_millis(25),
        start_delay: Duration::from_millis(5),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeSessionFactory::new(controller.clone());
    let deploy_id = DeployId("deploy-open".into());

    let (sessions, _events) = SessionSet::open(&factory, &plan, &local_machine_id, &deploy_id)
        .await
        .expect("open sessions");
    sessions.close_all().await;

    assert_eq!(controller.max_open_seen(), 5);
    assert_eq!(controller.start_count(), 0);
}

#[tokio::test]
async fn phase_startup_uses_one_worker_per_machine_but_parallel_across_machines() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 2 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 2 }, "busybox:1.0"),
    ]);

    let controller = FakeController {
        start_delay: Duration::from_millis(40),
        ..Default::default()
    };
    let plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let factory = FakeSessionFactory::new(controller.clone());
    let deploy_id = DeployId("deploy-phase".into());
    let (sessions, _events) = SessionSet::open(&factory, &plan, &local_machine_id, &deploy_id)
        .await
        .expect("open sessions");
    let startup = run_phase_startup(&store, &sessions, &plan, &deploy_id)
        .await
        .expect("run startup");
    sessions.close_all().await;

    assert_eq!(startup.started.len(), 4);
    assert_eq!(controller.start_count(), 4);
    assert!(controller.max_global_start_seen() >= 2);
    assert_eq!(controller.max_machine_start_seen("machine-a"), 1);
    assert_eq!(controller.max_machine_start_seen("machine-b"), 1);
}

#[tokio::test]
async fn run_phase_startup_waits_for_previous_phase_before_next_phase() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 2 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 2 }, "busybox:1.0"),
    ]);
    let mut plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("resolve plan");
    let [first, second] = plan.services_mut() else {
        panic!("expected two planned services");
    };
    first.phase = Some(0);
    second.phase = Some(1);

    let controller = FakeController {
        start_delay: Duration::from_millis(20),
        ..Default::default()
    };
    let factory = FakeSessionFactory::new(controller.clone());
    let deploy_id = DeployId("deploy-test".into());
    let (sessions, _events) = SessionSet::open(&factory, &plan, &local_machine_id, &deploy_id)
        .await
        .expect("open sessions");

    let startup = run_phase_startup(&store, &sessions, &plan, &deploy_id)
        .await
        .expect("run phases");
    sessions.close_all().await;

    assert_eq!(startup.started.len(), 4);
    let log = controller.start_log().await;
    let first_worker = log
        .iter()
        .position(|entry| entry.contains("worker"))
        .expect("worker start present");
    let last_api = log
        .iter()
        .rposition(|entry| entry.contains("api"))
        .expect("api start present");
    assert!(last_api < first_worker);
}

#[tokio::test]
async fn ensure_plan_stable_rejects_post_lock_drift() {
    let store = seeded_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.27",
    )]);
    let drift_manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Global,
        "nginx:1.28",
    )]);
    let [current_spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let current_revision = current_spec.revision_hash().expect("current revision");
    let [drift_spec] = drift_manifest.services.as_slice() else {
        panic!("expected one drift service");
    };
    let drift_revision = drift_spec.revision_hash().expect("drift revision");

    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &current_revision,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a", &current_revision),
                test_slot("slot-machine-b", "machine-b", "inst-b", &current_revision),
            ],
        ))
        .await
        .expect("seed release");

    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    store
        .upsert_service_release(&test_release(
            &manifest.namespace,
            "api",
            &drift_revision,
            vec![
                test_slot("slot-machine-a", "machine-a", "inst-a2", &drift_revision),
                test_slot("slot-machine-b", "machine-b", "inst-b2", &drift_revision),
            ],
        ))
        .await
        .expect("drift release");
    let final_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("final plan");

    let error = ensure_plan_stable(&initial_plan.fingerprint(), &final_plan.fingerprint())
        .expect_err("plan drift should fail");
    assert!(
        error
            .to_string()
            .contains("resolved execution plan changed after lock acquisition")
    );
}

#[tokio::test]
async fn apply_with_initial_plan_does_not_commit_when_session_open_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 2 },
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeSessionFactory::new(FakeController {
        fail_open_machine: Some("machine-b".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("apply should fail");

    assert!(error.to_string().contains("injected open failure"));
    assert_eq!(backend.commit_count(), 0);
}

#[tokio::test]
async fn apply_with_initial_plan_does_not_commit_when_start_candidate_fails() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 2 },
        "nginx:1.27",
    )]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeSessionFactory::new(FakeController {
        fail_start_service: Some("api".into()),
        ..Default::default()
    });

    let error =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect_err("apply should fail");

    assert!(error.to_string().contains("injected start failure"));
    assert_eq!(backend.commit_count(), 0);
    let releases = store
        .list_service_releases(&manifest.namespace)
        .await
        .expect("list releases");
    assert!(releases.is_empty());
}

#[tokio::test]
async fn apply_with_initial_plan_sets_cleanup_pending_after_cleanup_failure() {
    let (store, backend) = counting_store_with_machines(&["machine-a"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![test_service_spec(
        "api",
        Placement::Replicated { count: 1 },
        "nginx:1.27",
    )]);
    let [spec] = manifest.services.as_slice() else {
        panic!("expected one service");
    };
    let revision_hash = spec.revision_hash().expect("revision hash");
    let old_instance = test_instance_status(
        &manifest.namespace,
        "api",
        "slot-0001",
        "machine-a",
        "old-instance",
        &revision_hash,
    );
    store
        .upsert_instance_status(&old_instance)
        .await
        .expect("seed old instance");
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeSessionFactory::new(FakeController {
        fail_remove_instance: Some("old-instance".into()),
        ..Default::default()
    });

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, crate::model::DeployState::CleanupPending);
    assert_eq!(backend.commit_count(), 1);
    assert_eq!(backend.upsert_deploy_count(), 2);
    let commit_index = result
        .events
        .iter()
        .position(|event| event.step == "commit")
        .expect("commit event");
    let cleanup_pending_index = result
        .events
        .iter()
        .position(|event| event.step == "cleanup_pending")
        .expect("cleanup pending event");
    assert!(commit_index < cleanup_pending_index);
    assert!(
        result
            .events
            .iter()
            .filter(|event| event.step == "commit")
            .count()
            == 1
    );
    assert_eq!(factory.controller.drain_count(), 1);
    assert_eq!(factory.controller.remove_count(), 1);
}

#[tokio::test]
async fn apply_with_initial_plan_commits_once_after_all_starts_finish() {
    let (store, backend) = counting_store_with_machines(&["machine-a", "machine-b"]).await;
    let local_machine_id = MachineId("local".into());
    let manifest = test_manifest(vec![
        test_service_spec("api", Placement::Replicated { count: 2 }, "nginx:1.27"),
        test_service_spec("worker", Placement::Replicated { count: 2 }, "busybox:1.0"),
    ]);
    let initial_plan = resolve_plan(&store, &local_machine_id, &manifest)
        .await
        .expect("initial plan");
    let factory = FakeSessionFactory::new(FakeController {
        start_delay: Duration::from_millis(10),
        ..Default::default()
    });

    let result =
        apply_with_initial_plan(&store, &factory, &local_machine_id, &manifest, initial_plan)
            .await
            .expect("apply");

    assert_eq!(result.state, crate::model::DeployState::Committed);
    assert_eq!(backend.commit_count(), 1);
    let commit_index = result
        .events
        .iter()
        .position(|event| event.step == "commit")
        .expect("commit event");
    let last_start_index = result
        .events
        .iter()
        .rposition(|event| event.step == "start_candidate")
        .expect("start events");
    assert!(last_start_index < commit_index);
    assert!(
        result
            .events
            .iter()
            .enumerate()
            .skip(commit_index + 1)
            .all(|(_, event)| event.step != "start_candidate")
    );
}

#[derive(Clone, Default)]
struct FakeController {
    open_delay: Duration,
    start_delay: Duration,
    fail_open_machine: Option<String>,
    fail_start_service: Option<String>,
    fail_remove_instance: Option<String>,
    open_active: Arc<AtomicUsize>,
    max_open: Arc<AtomicUsize>,
    start_count: Arc<AtomicUsize>,
    start_active: Arc<AtomicUsize>,
    max_global_start: Arc<AtomicUsize>,
    drain_count: Arc<AtomicUsize>,
    remove_count: Arc<AtomicUsize>,
    machine_state: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    machine_max: Arc<std::sync::Mutex<HashMap<String, usize>>>,
    start_log_entries: Arc<Mutex<Vec<String>>>,
}

impl FakeController {
    fn max_open_seen(&self) -> usize {
        self.max_open.load(Ordering::SeqCst)
    }

    fn start_count(&self) -> usize {
        self.start_count.load(Ordering::SeqCst)
    }

    fn max_global_start_seen(&self) -> usize {
        self.max_global_start.load(Ordering::SeqCst)
    }

    fn drain_count(&self) -> usize {
        self.drain_count.load(Ordering::SeqCst)
    }

    fn remove_count(&self) -> usize {
        self.remove_count.load(Ordering::SeqCst)
    }

    fn max_machine_start_seen(&self, machine_id: &str) -> usize {
        self.machine_max
            .lock()
            .expect("machine max lock")
            .get(machine_id)
            .copied()
            .unwrap_or_default()
    }

    async fn start_log(&self) -> Vec<String> {
        self.start_log_entries.lock().await.clone()
    }

    async fn on_open_start(&self) {
        let current = self.open_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_open.fetch_max(current, Ordering::SeqCst);
        sleep(self.open_delay).await;
        self.open_active.fetch_sub(1, Ordering::SeqCst);
    }

    fn should_fail_open(&self, machine_id: &MachineId) -> bool {
        self.fail_open_machine.as_deref() == Some(machine_id.0.as_str())
    }

    async fn on_start_begin(&self, machine_id: &MachineId, service: &str, slot_id: &SlotId) {
        self.start_count.fetch_add(1, Ordering::SeqCst);
        let global = self.start_active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_global_start.fetch_max(global, Ordering::SeqCst);
        {
            let mut machine_state = self.machine_state.lock().expect("machine state lock");
            let current = machine_state.entry(machine_id.0.clone()).or_default();
            *current += 1;
            let mut machine_max = self.machine_max.lock().expect("machine max lock");
            let max = machine_max.entry(machine_id.0.clone()).or_default();
            *max = (*max).max(*current);
        }
        self.start_log_entries
            .lock()
            .await
            .push(format!("{service}:{machine_id}:{slot_id}"));
    }

    fn should_fail_start(&self, service: &str) -> bool {
        self.fail_start_service.as_deref() == Some(service)
    }

    async fn on_start_end(&self, machine_id: &MachineId) {
        self.start_active.fetch_sub(1, Ordering::SeqCst);
        let mut machine_state = self.machine_state.lock().expect("machine state lock");
        let Some(current) = machine_state.get_mut(&machine_id.0) else {
            return;
        };
        *current -= 1;
    }

    fn on_drain(&self) {
        self.drain_count.fetch_add(1, Ordering::SeqCst);
    }

    fn on_remove(&self) {
        self.remove_count.fetch_add(1, Ordering::SeqCst);
    }

    fn should_fail_remove(&self, instance_id: &InstanceId) -> bool {
        self.fail_remove_instance.as_deref() == Some(instance_id.0.as_str())
    }
}

struct FakeSessionFactory {
    controller: FakeController,
}

impl FakeSessionFactory {
    fn new(controller: FakeController) -> Self {
        Self { controller }
    }
}

#[async_trait::async_trait]
impl DeploySessionFactory for FakeSessionFactory {
    async fn open(
        &self,
        machine: &MachineRecord,
        namespace: &Namespace,
        deploy_id: &DeployId,
        _coordinator_id: &MachineId,
    ) -> Result<(Box<dyn DeploySession>, Vec<InstanceStatusRecord>)> {
        self.controller.on_open_start().await;
        if self.controller.should_fail_open(&machine.id) {
            return Err(ployz_types::error::Error::operation(
                "fake_open",
                format!("injected open failure for '{}'", machine.id),
            ));
        }
        Ok((
            Box::new(FakeSession {
                controller: self.controller.clone(),
                machine_id: machine.id.clone(),
                namespace: namespace.clone(),
                deploy_id: deploy_id.clone(),
            }),
            Vec::new(),
        ))
    }
}

struct FakeSession {
    controller: FakeController,
    machine_id: MachineId,
    namespace: Namespace,
    deploy_id: DeployId,
}

#[async_trait::async_trait]
impl DeploySession for FakeSession {
    fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    async fn inspect_namespace(&mut self) -> Result<Vec<InstanceStatusRecord>> {
        Ok(Vec::new())
    }

    async fn start_candidate(
        &mut self,
        req: StartCandidateRequest,
    ) -> Result<InstanceStatusRecord> {
        self.controller
            .on_start_begin(&self.machine_id, &req.service, &req.slot_id)
            .await;
        if self.controller.should_fail_start(&req.service) {
            self.controller.on_start_end(&self.machine_id).await;
            return Err(ployz_types::error::Error::operation(
                "fake_start",
                format!("injected start failure for '{}'", req.service),
            ));
        }
        sleep(self.controller.start_delay).await;
        self.controller.on_start_end(&self.machine_id).await;
        Ok(InstanceStatusRecord {
            instance_id: req.instance_id.clone(),
            namespace: self.namespace.clone(),
            service: req.service,
            slot_id: req.slot_id,
            machine_id: self.machine_id.clone(),
            revision_hash: "fake-revision".into(),
            deploy_id: self.deploy_id.clone(),
            docker_container_id: format!("container-{}", req.instance_id.0),
            overlay_ip: None,
            backend_ports: BTreeMap::new(),
            phase: InstancePhase::Ready,
            ready: true,
            drain_state: DrainState::None,
            error: None,
            started_at: 0,
            updated_at: 0,
        })
    }

    async fn drain_instance(&mut self, _instance_id: &InstanceId) -> Result<()> {
        self.controller.on_drain();
        Ok(())
    }

    async fn remove_instance(&mut self, instance_id: &InstanceId) -> Result<()> {
        self.controller.on_remove();
        if self.controller.should_fail_remove(instance_id) {
            return Err(ployz_types::error::Error::operation(
                "fake_remove",
                format!("injected remove failure for '{}'", instance_id),
            ));
        }
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

async fn seeded_store_with_machines(machine_ids: &[&str]) -> StoreDriver {
    let store = StoreDriver::memory();
    for machine_id in machine_ids {
        store
            .upsert_self_machine(&test_machine(machine_id, MachineLifecycle::Active))
            .await
            .expect("seed machine");
    }
    store
}

async fn counting_store_with_machines(machine_ids: &[&str]) -> (StoreDriver, Arc<CountingBackend>) {
    let backend = Arc::new(CountingBackend::new());
    let store = StoreDriver::from_backend(
        backend.clone() as Arc<dyn StoreBackend>,
        backend.clone() as Arc<dyn StoreRuntimeControl>,
    );
    for machine_id in machine_ids {
        store
            .upsert_self_machine(&test_machine(machine_id, MachineLifecycle::Active))
            .await
            .expect("seed machine");
    }
    (store, backend)
}

fn test_manifest(services: Vec<ServiceSpec>) -> DeployManifest {
    DeployManifest {
        namespace: Namespace("test".into()),
        services,
    }
}

fn test_service_spec(name: &str, placement: Placement, image: &str) -> ServiceSpec {
    ServiceSpec {
        name: name.into(),
        placement,
        template: ContainerSpec {
            image: image.into(),
            command: None,
            entrypoint: None,
            env: BTreeMap::new(),
            volumes: Vec::new(),
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            user: None,
            stop_grace_period: None,
            pid_mode: None,
            pull_policy: PullPolicy::IfNotPresent,
            resources: Resources::empty(),
            sysctls: BTreeMap::new(),
        },
        network: NetworkMode::Overlay,
        service_ports: Vec::new(),
        publish: Vec::new(),
        routes: Vec::new(),
        readiness: None,
        rollout: RolloutStrategy::Recreate,
        labels: BTreeMap::new(),
        restart: RestartPolicy::UnlessStopped,
    }
}

fn test_release(
    namespace: &Namespace,
    service: &str,
    revision_hash: &str,
    slots: Vec<ServiceReleaseSlot>,
) -> ServiceReleaseRecord {
    ServiceReleaseRecord {
        namespace: namespace.clone(),
        service: service.into(),
        release: ServiceRelease {
            primary_revision_hash: revision_hash.into(),
            referenced_revision_hashes: vec![revision_hash.into()],
            routing: ServiceRoutingPolicy::Direct {
                revision_hash: revision_hash.into(),
            },
            slots,
            updated_by_deploy_id: DeployId("deploy-1".into()),
            updated_at: 0,
        },
    }
}

fn test_slot(
    slot_id: &str,
    machine_id: &str,
    instance_id: &str,
    revision_hash: &str,
) -> ServiceReleaseSlot {
    ServiceReleaseSlot {
        slot_id: SlotId(slot_id.into()),
        machine_id: MachineId(machine_id.into()),
        active_instance_id: InstanceId(instance_id.into()),
        revision_hash: revision_hash.into(),
    }
}

fn test_machine(id: &str, lifecycle: MachineLifecycle) -> MachineRecord {
    MachineRecord {
        id: MachineId(id.into()),
        public_key: PublicKey([7; 32]),
        overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
        control_target: None,
        subnet: None,
        bridge_ip: None,
        endpoints: vec!["127.0.0.1:51820".into()],
        lifecycle,
        created_at: 0,
        updated_at: 0,
        labels: BTreeMap::new(),
    }
}

fn test_instance_status(
    namespace: &Namespace,
    service: &str,
    slot_id: &str,
    machine_id: &str,
    instance_id: &str,
    revision_hash: &str,
) -> InstanceStatusRecord {
    InstanceStatusRecord {
        instance_id: InstanceId(instance_id.into()),
        namespace: namespace.clone(),
        service: service.into(),
        slot_id: SlotId(slot_id.into()),
        machine_id: MachineId(machine_id.into()),
        revision_hash: revision_hash.into(),
        deploy_id: DeployId("previous-deploy".into()),
        docker_container_id: format!("container-{instance_id}"),
        overlay_ip: None,
        backend_ports: BTreeMap::new(),
        phase: InstancePhase::Ready,
        ready: true,
        drain_state: DrainState::None,
        error: None,
        started_at: 0,
        updated_at: 0,
    }
}

struct CountingBackend {
    store: Arc<MemoryStore>,
    service: Arc<MemoryService>,
    commit_calls: AtomicUsize,
    upsert_deploy_calls: AtomicUsize,
}

impl CountingBackend {
    fn new() -> Self {
        Self {
            store: Arc::new(MemoryStore::new()),
            service: Arc::new(MemoryService::new()),
            commit_calls: AtomicUsize::new(0),
            upsert_deploy_calls: AtomicUsize::new(0),
        }
    }

    fn commit_count(&self) -> usize {
        self.commit_calls.load(Ordering::SeqCst)
    }

    fn upsert_deploy_count(&self) -> usize {
        self.upsert_deploy_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl StoreBackend for CountingBackend {
    async fn init(&self) -> PloyzResult<()> {
        self.store.init().await
    }

    async fn list_machines(&self) -> PloyzResult<Vec<MachineRecord>> {
        self.store.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineRecord) -> PloyzResult<()> {
        self.store.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> PloyzResult<()> {
        self.store.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> PloyzResult<MachineSubscription> {
        self.store.subscribe_machines().await
    }

    async fn create_invite(&self, invite: &ployz_types::model::InviteRecord) -> PloyzResult<()> {
        self.store.create_invite(invite).await
    }

    async fn get_invite(
        &self,
        invite_id: &str,
    ) -> PloyzResult<Option<ployz_types::model::InviteRecord>> {
        self.store.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> PloyzResult<Vec<ployz_types::model::InviteRecord>> {
        self.store.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> PloyzResult<ployz_types::model::InviteRecord> {
        self.store
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(
        &self,
        invite_id: &str,
        now_unix_secs: u64,
    ) -> PloyzResult<ployz_types::model::InviteRecord> {
        self.store.revoke_invite(invite_id, now_unix_secs).await
    }

    async fn load_routing_state(&self) -> PloyzResult<crate::model::RoutingState> {
        self.store.load_routing_state().await
    }

    async fn subscribe_routing_invalidations(
        &self,
    ) -> PloyzResult<RoutingInvalidationSubscription> {
        self.store.subscribe_routing_invalidations().await
    }

    async fn list_service_revisions(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<crate::model::ServiceRevisionRecord>> {
        self.store.list_service_revisions(namespace).await
    }

    async fn list_service_releases(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<ServiceReleaseRecord>> {
        self.store.list_service_releases(namespace).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> PloyzResult<Vec<InstanceStatusRecord>> {
        self.store.list_instance_status(namespace).await
    }

    async fn upsert_service_revision(
        &self,
        record: &crate::model::ServiceRevisionRecord,
    ) -> PloyzResult<()> {
        self.store.upsert_service_revision(record).await
    }

    async fn upsert_service_release(&self, record: &ServiceReleaseRecord) -> PloyzResult<()> {
        self.store.upsert_service_release(record).await
    }

    async fn delete_service_release(
        &self,
        namespace: &Namespace,
        service: &str,
    ) -> PloyzResult<()> {
        self.store.delete_service_release(namespace, service).await
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> PloyzResult<()> {
        self.store.upsert_instance_status(record).await
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> PloyzResult<()> {
        self.store.delete_instance_status(instance_id).await
    }

    async fn upsert_deploy(&self, record: &crate::model::DeployRecord) -> PloyzResult<()> {
        self.upsert_deploy_calls.fetch_add(1, Ordering::SeqCst);
        self.store.upsert_deploy(record).await
    }

    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        releases: &[ServiceReleaseRecord],
        deploy: &crate::model::DeployRecord,
    ) -> PloyzResult<()> {
        self.commit_calls.fetch_add(1, Ordering::SeqCst);
        self.store
            .commit_deploy(namespace, removed_services, releases, deploy)
            .await
    }

    async fn get_deploy(
        &self,
        deploy_id: &DeployId,
    ) -> PloyzResult<Option<crate::model::DeployRecord>> {
        self.store.get_deploy(deploy_id).await
    }
}

#[async_trait]
impl StoreRuntimeControl for CountingBackend {
    async fn start(&self) -> PloyzResult<()> {
        self.service.start().await
    }

    async fn stop(&self) -> PloyzResult<()> {
        self.service.stop().await
    }

    async fn healthy(&self) -> bool {
        self.service.healthy().await
    }
}
