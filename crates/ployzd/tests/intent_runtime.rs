use futures_util::StreamExt;
use ployz_core::state::{
    ActiveMachineState, IntentSnapshot, MachineLifecycle, RouteBindingState, ServingTargetEntry,
};
use ployz_core::subjects::INTENT_CHANGED;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_test_support::ids::{machine_id, namespace_revision_entry_id, operation_id, service_id};
use ployzd::intent::{NatsIntentReader, start_intent_runtime};
use ployzd::namespace_intent::NamespaceIntentStore;
use std::time::Duration;

#[tokio::test]
async fn intent_runtime_rebroadcasts_full_intent_on_the_drumbeat() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    nats.bootstrap_resources().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&ActiveMachineState {
            machine_id: machine_id("machine_a"),
            name: ployz_core::machine::MachineName::try_new("machine_a")
                .expect("valid machine name"),
            activated_by: operation_id("op_machine_add"),
            lifecycle: MachineLifecycle::Active,
        })
        .await
        .expect("active machine stores");
    let mut changed = nats
        .controller
        .subscribe(INTENT_CHANGED)
        .await
        .expect("subscribe intent changes");
    let _runtime = start_intent_runtime(
        nats.controller.clone(),
        core_state,
        temp_namespace_intent(),
        tempfile::tempdir()
            .expect("lifecycle dir")
            .path()
            .join("machine-lifecycles.json"),
        Duration::from_millis(10),
    )
    .await
    .expect("intent runtime starts");

    let message = tokio::time::timeout(Duration::from_secs(1), changed.next())
        .await
        .expect("intent rebroadcast arrives")
        .expect("intent message exists");
    let intent: IntentSnapshot =
        serde_json::from_slice(&message.payload).expect("intent snapshot decodes");

    assert_eq!(intent.active_machines.len(), 1);
    assert_eq!(
        intent.active_machines[0].machine_id,
        machine_id("machine_a")
    );
}

#[tokio::test]
async fn intent_reader_gets_current_intent() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    nats.bootstrap_resources().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    let _runtime = start_intent_runtime(
        nats.controller.clone(),
        core_state,
        temp_namespace_intent(),
        tempfile::tempdir()
            .expect("lifecycle dir")
            .path()
            .join("machine-lifecycles.json"),
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    let intent = NatsIntentReader::new(nats.controller)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent reads");

    assert!(intent.active_machines.is_empty());
    assert!(intent.route_bindings.is_empty());
    assert!(intent.serving_target_entries.is_empty());
}

#[tokio::test]
async fn intent_reader_overlays_machine_lifecycle_evidence() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    nats.bootstrap_resources().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    core_state
        .replace_active_machine(&ActiveMachineState {
            machine_id: machine_id("machine_a"),
            name: ployz_core::machine::MachineName::try_new("machine_a")
                .expect("valid machine name"),
            activated_by: operation_id("op_machine_add"),
            lifecycle: MachineLifecycle::Active,
        })
        .await
        .expect("active machine stores");
    let lifecycle_dir = tempfile::tempdir().expect("lifecycle dir");
    let lifecycle_file = lifecycle_dir.path().join("machine-lifecycles.json");
    std::fs::write(&lifecycle_file, r#"{"draining":["machine_a"]}"#)
        .expect("write lifecycle evidence");
    let _runtime = start_intent_runtime(
        nats.controller.clone(),
        core_state,
        temp_namespace_intent(),
        lifecycle_file,
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    let intent = NatsIntentReader::new(nats.controller)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent reads");

    assert_eq!(
        intent.active_machines[0].lifecycle,
        MachineLifecycle::Draining
    );
}

#[tokio::test]
async fn intent_reader_gets_namespace_intent_from_file() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    nats.bootstrap_resources().await;
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    let namespace_intent = temp_namespace_intent();
    namespace_intent
        .replace_serving_target_entry(ServingTargetEntry {
            namespace_id: ployz_test_support::ids::namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_api"),
        })
        .await
        .expect("serving target stores");
    namespace_intent
        .replace_route_binding(RouteBindingState {
            namespace_id: ployz_test_support::ids::namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: ployz_core::ops::RoutePort::try_new(8080).expect("valid route port"),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("route binding stores");
    let _runtime = start_intent_runtime(
        nats.controller.clone(),
        core_state,
        namespace_intent,
        tempfile::tempdir()
            .expect("lifecycle dir")
            .path()
            .join("machine-lifecycles.json"),
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    let intent = NatsIntentReader::new(nats.controller)
        .with_request_timeout(Duration::from_secs(1))
        .intent()
        .await
        .expect("intent reads");

    assert_eq!(intent.route_bindings.len(), 1);
    assert_eq!(intent.serving_target_entries.len(), 1);
}

fn temp_namespace_intent() -> NamespaceIntentStore {
    NamespaceIntentStore::new(
        tempfile::tempdir()
            .expect("namespace intent dir")
            .path()
            .join("namespace-intent.json"),
    )
}

fn route_target(hostname: &str, port: u16) -> ployz_core::ops::RouteTarget {
    ployz_core::ops::RouteTarget::new(
        ployz_core::ops::RouteHostname::try_new(hostname).expect("valid route hostname"),
        ployz_core::ops::RoutePort::try_new(port).expect("valid route port"),
    )
}
