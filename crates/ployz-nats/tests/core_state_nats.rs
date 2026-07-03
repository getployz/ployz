use async_nats::jetstream;
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    ActiveMachineState, ActiveMachineStateKey, ActiveRouteState, ActiveRouteStateKey,
    ActiveServiceState, ActiveServiceStateKey,
};
use ployz_nats::core_state::{
    ActiveRouteStoreError, AsyncNatsCoreStateStore, CoreStateStoreError, NamespaceLockAcquire,
    NamespaceLockRenew,
};
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_test_support::ids::{
    machine_id, machine_name, namespace_id, operation_id, revision_id, route_hostname, route_port,
    service_id,
};

#[tokio::test]
async fn active_service_state_round_trips_through_kv_core() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let state = active_service_state("svc_api", "rev_1");

    store
        .replace_active_service(&state)
        .await
        .expect("active state stores");

    assert_eq!(
        store
            .active_service(&namespace_id("default"), &service_id("svc_api"))
            .await
            .expect("active state loads"),
        Some(state)
    );
}

#[tokio::test]
async fn active_service_replace_overwrites_current_revision() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let first = active_service_state("svc_api", "rev_1");
    let second = active_service_state("svc_api", "rev_2");

    store
        .replace_active_service(&first)
        .await
        .expect("first active state stores");
    store
        .replace_active_service(&second)
        .await
        .expect("second active state stores");

    assert_eq!(
        store
            .active_service(&namespace_id("default"), &service_id("svc_api"))
            .await
            .expect("active state loads"),
        Some(second)
    );
}

#[tokio::test]
async fn active_service_replace_succeeds_after_delete_marker() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let state = active_service_state("svc_api", "rev_1");

    store
        .replace_active_service(&state)
        .await
        .expect("active state stores");
    store
        .remove_active_service(&state.namespace_id, &state.service_id)
        .await
        .expect("active state removes");
    store
        .replace_active_service(&state)
        .await
        .expect("active state replaces after delete");

    assert_eq!(
        store
            .active_service(&state.namespace_id, &state.service_id)
            .await
            .expect("active state loads"),
        Some(state)
    );
}

#[tokio::test]
async fn active_services_list_skips_deleted_keys() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let api = active_service_state("svc_api", "rev_2");
    let worker = active_service_state("svc_worker", "rev_1");

    store
        .replace_active_service(&api)
        .await
        .expect("api service stores");
    store
        .replace_active_service(&worker)
        .await
        .expect("worker service stores");
    store
        .remove_active_service(&worker.namespace_id, &worker.service_id)
        .await
        .expect("worker service removes");

    assert_eq!(
        store.active_services().await.expect("services list"),
        vec![api]
    );
}

#[tokio::test]
async fn active_service_state_rejects_payload_for_wrong_service_key() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let target_service_id = service_id("svc_api");
    let other_service_id = service_id("svc_other");
    let target_namespace_id = namespace_id("default");
    let key =
        ActiveServiceStateKey::from_namespace_service(&target_namespace_id, &target_service_id);
    let bucket = nats
        .jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open test KV_CORE bucket");

    let wrong_payload = serde_json::to_vec(&ActiveServiceState {
        namespace_id: namespace_id("default"),
        service_id: other_service_id.clone(),
        active_revision: revision_id("rev_1"),
    })
    .expect("encode wrong active state");
    bucket
        .put(key.as_str(), wrong_payload.into())
        .await
        .expect("write corrupt active state");

    let error = store
        .active_service(&target_namespace_id, &target_service_id)
        .await
        .expect_err("wrong service payload is rejected");
    match error {
        CoreStateStoreError::CorruptActiveServiceState {
            key: actual_key,
            expected_service_id,
            actual_service_id,
        } => {
            assert_eq!(actual_key, key.as_str());
            assert_eq!(expected_service_id, target_service_id);
            assert_eq!(actual_service_id, other_service_id);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn active_machine_state_round_trips_through_kv_core() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let machine = active_machine_state("machine_7", "edge_7", "op_machine");

    store
        .replace_active_machine(&machine)
        .await
        .expect("active machine stores");

    assert_eq!(
        store
            .active_machine(&machine_id("machine_7"))
            .await
            .expect("active machine loads"),
        Some(machine)
    );
}

#[tokio::test]
async fn active_machines_list_sorted_by_machine_id() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let machine_8 = active_machine_state("machine_8", "edge_8", "op_8");
    let machine_7 = active_machine_state("machine_7", "edge_7", "op_7");

    store
        .replace_active_machine(&machine_8)
        .await
        .expect("machine 8 stores");
    store
        .replace_active_machine(&machine_7)
        .await
        .expect("machine 7 stores");

    assert_eq!(
        store.active_machines().await.expect("machines list"),
        vec![machine_7, machine_8]
    );
}

#[tokio::test]
async fn active_route_state_round_trips_through_kv_core() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let state = active_route_state(&route_target("API.example.com", 443), "svc_api", "rev_1");

    store
        .replace_active_route(&state)
        .await
        .expect("route state stores");

    assert_eq!(
        store
            .active_route(&state.target)
            .await
            .expect("active route loads"),
        Some(state)
    );
}

#[tokio::test]
async fn active_route_replace_overwrites_current_revision() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let target = route_target("api.example.com", 443);
    let first = active_route_state(&target, "svc_api", "rev_1");
    let second = active_route_state(&target, "svc_api", "rev_2");

    store
        .replace_active_route(&first)
        .await
        .expect("first route stores");
    store
        .replace_active_route(&second)
        .await
        .expect("second route stores");

    assert_eq!(
        store
            .active_route(&target)
            .await
            .expect("active route loads"),
        Some(second)
    );
}

#[tokio::test]
async fn active_routes_lists_only_route_state_sorted_by_target() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let api = route_target("api.example.com", 443);
    let www = route_target("www.example.com", 443);

    store
        .replace_active_service(&active_service_state("svc_api", "rev_1"))
        .await
        .expect("service state stores");
    store
        .replace_active_route(&active_route_state(&www, "svc_web", "rev_1"))
        .await
        .expect("www route stores");
    store
        .replace_active_route(&active_route_state(&api, "svc_api", "rev_2"))
        .await
        .expect("api route stores");

    assert_eq!(
        store.active_routes().await.expect("routes list"),
        vec![
            active_route_state(&api, "svc_api", "rev_2"),
            active_route_state(&www, "svc_web", "rev_1"),
        ]
    );
}

#[tokio::test]
async fn active_route_state_rejects_payload_for_wrong_route_key() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let target = route_target("api.example.com", 443);
    let other_target = route_target("www.example.com", 443);
    let key = ActiveRouteStateKey::from_target(&target);
    let bucket = nats
        .jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open test KV_CORE bucket");

    let wrong_payload = serde_json::to_vec(&ActiveRouteState {
        namespace_id: namespace_id("default"),
        target: other_target.clone(),
        endpoint_port: route_port(8080),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
    })
    .expect("encode wrong route state");
    bucket
        .put(key.as_str(), wrong_payload.into())
        .await
        .expect("write corrupt route state");

    let error = store
        .active_route(&target)
        .await
        .expect_err("wrong route payload is rejected");
    match error {
        ActiveRouteStoreError::CorruptActiveRouteState {
            key: actual_key,
            expected_target,
            actual_target,
        } => {
            assert_eq!(actual_key, key.as_str());
            assert_eq!(expected_target, target);
            assert_eq!(actual_target, other_target);
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn namespace_lock_acquire_is_busy_for_other_live_owner() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let namespace_id = namespace_id("production");

    assert_eq!(
        store
            .acquire_namespace_lock(&namespace_id, &operation_id("op_a"), 1_000)
            .await
            .expect("first lock acquire succeeds"),
        NamespaceLockAcquire::Acquired
    );
    assert_eq!(
        store
            .acquire_namespace_lock(&namespace_id, &operation_id("op_b"), 2_000)
            .await
            .expect("second lock acquire classifies busy"),
        NamespaceLockAcquire::Busy {
            owner: operation_id("op_a")
        }
    );
}

#[tokio::test]
async fn namespace_lock_same_owner_is_idempotent() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let namespace_id = namespace_id("production");
    let operation_id = operation_id("op_a");

    store
        .acquire_namespace_lock(&namespace_id, &operation_id, 1_000)
        .await
        .expect("first lock acquire succeeds");

    assert_eq!(
        store
            .acquire_namespace_lock(&namespace_id, &operation_id, 2_000)
            .await
            .expect("same owner reacquires"),
        NamespaceLockAcquire::Acquired
    );
}

#[tokio::test]
async fn namespace_lock_expired_owner_can_be_replaced() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let namespace_id = namespace_id("production");

    store
        .acquire_namespace_lock(&namespace_id, &operation_id("op_a"), 1_000)
        .await
        .expect("first lock acquire succeeds");

    assert_eq!(
        store
            .acquire_namespace_lock(&namespace_id, &operation_id("op_b"), 31_000)
            .await
            .expect("expired lock acquire succeeds"),
        NamespaceLockAcquire::Acquired
    );
}

#[tokio::test]
async fn namespace_lock_renew_and_release_are_owner_scoped() {
    let nats = test_nats().await;
    let store = core_state_store(&nats).await;
    let namespace_id = namespace_id("production");
    let owner = operation_id("op_a");

    store
        .acquire_namespace_lock(&namespace_id, &owner, 1_000)
        .await
        .expect("lock acquire succeeds");
    assert_eq!(
        store
            .renew_namespace_lock(&namespace_id, &owner, 2_000)
            .await
            .expect("owner renews"),
        NamespaceLockRenew::Renewed
    );
    assert_eq!(
        store
            .renew_namespace_lock(&namespace_id, &operation_id("op_b"), 3_000)
            .await
            .expect("non-owner renew returns lost"),
        NamespaceLockRenew::Lost
    );
    store
        .release_namespace_lock(&namespace_id, &operation_id("op_b"))
        .await
        .expect("non-owner release is harmless");
    assert_eq!(
        store
            .acquire_namespace_lock(&namespace_id, &operation_id("op_c"), 4_000)
            .await
            .expect("lock remains owned"),
        NamespaceLockAcquire::Busy {
            owner: owner.clone()
        }
    );
    store
        .release_namespace_lock(&namespace_id, &owner)
        .await
        .expect("owner release succeeds");
    assert_eq!(
        store
            .acquire_namespace_lock(&namespace_id, &operation_id("op_c"), 5_000)
            .await
            .expect("lock can be acquired after release"),
        NamespaceLockAcquire::Acquired
    );
}

#[test]
fn active_service_state_key_matches_kv_core_path() {
    assert_eq!(
        ActiveServiceStateKey::from_namespace_service(
            &namespace_id("default"),
            &service_id("svc_api")
        )
        .as_str(),
        "services.default.svc_api"
    );
}

#[test]
fn active_route_state_key_matches_kv_core_path() {
    assert_eq!(
        ActiveRouteStateKey::from_target(&route_target("API.example.com", 443)).as_str(),
        "routes.6170692e6578616d706c652e636f6d.443"
    );
}

#[test]
fn active_machine_state_key_matches_kv_core_path() {
    assert_eq!(
        ActiveMachineStateKey::from_machine_id(&machine_id("machine_7")).as_str(),
        "machines.machine_7"
    );
}

struct TestNats {
    _server: ployz_test_support::nats::TestNats,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let server = ployz_test_support::nats::TestNats::start().await;
    server.bootstrap_resources().await;
    let jetstream = server.jetstream.clone();

    TestNats {
        _server: server,
        jetstream,
    }
}

async fn core_state_store(nats: &TestNats) -> AsyncNatsCoreStateStore {
    AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store")
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

fn active_route_state(target: &RouteTarget, service: &str, revision: &str) -> ActiveRouteState {
    ActiveRouteState {
        namespace_id: namespace_id("default"),
        target: target.clone(),
        endpoint_port: route_port(8080),
        service_id: service_id(service),
        revision_id: revision_id(revision),
    }
}

fn active_service_state(service: &str, revision: &str) -> ActiveServiceState {
    ActiveServiceState {
        namespace_id: namespace_id("default"),
        service_id: service_id(service),
        active_revision: revision_id(revision),
    }
}

fn active_machine_state(machine: &str, name: &str, operation: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: machine_id(machine),
        name: machine_name(name),
        activated_by: operation_id(operation),
        substrate_versions: None,
    }
}
