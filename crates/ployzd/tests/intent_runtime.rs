use futures_util::StreamExt;
use ployz_core::state::{
    ActiveMachineState, IntentSnapshot, MachineLifecycle, RouteBindingState, ServingTargetEntry,
};
use ployz_core::subjects::INTENT_CHANGED;
use ployz_test_support::ids::{machine_id, namespace_revision_entry_id, operation_id, service_id};
use ployzd::intent::machine_roster::MachineRosterStore;
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::intent::service::{NatsIntentReader, start_intent_service};
use std::time::Duration;

#[tokio::test]
async fn intent_runtime_rebroadcasts_full_intent_on_the_drumbeat() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let machine_roster = temp_machine_roster().await;
    machine_roster
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
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine_roster,
        temp_namespace_intent().await,
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
        intent
            .active_machines
            .first()
            .expect("one active machine")
            .machine_id,
        machine_id("machine_a")
    );
}

#[tokio::test]
async fn intent_reader_gets_current_intent() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let _runtime = start_intent_service(
        nats.controller.clone(),
        temp_machine_roster().await,
        temp_namespace_intent().await,
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
    let machine_roster = temp_machine_roster().await;
    machine_roster
        .replace_active_machine(&ActiveMachineState {
            machine_id: machine_id("machine_a"),
            name: ployz_core::machine::MachineName::try_new("machine_a")
                .expect("valid machine name"),
            activated_by: operation_id("op_machine_add"),
            lifecycle: MachineLifecycle::Draining,
        })
        .await
        .expect("active machine stores");
    let _runtime = start_intent_service(
        nats.controller.clone(),
        machine_roster,
        temp_namespace_intent().await,
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
        intent
            .active_machines
            .first()
            .expect("one active machine")
            .lifecycle,
        MachineLifecycle::Draining
    );
}

#[tokio::test]
async fn intent_reader_gets_namespace_intent_from_file() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let namespace_intent = temp_namespace_intent().await;
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
    let _runtime = start_intent_service(
        nats.controller.clone(),
        temp_machine_roster().await,
        namespace_intent,
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

async fn temp_namespace_intent() -> NamespaceIntentStore {
    NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    )
}

async fn temp_machine_roster() -> MachineRosterStore {
    MachineRosterStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    )
}

fn route_target(hostname: &str, port: u16) -> ployz_core::ops::RouteTarget {
    ployz_core::ops::RouteTarget::new(
        ployz_core::ops::RouteHostname::try_new(hostname).expect("valid route hostname"),
        ployz_core::ops::RoutePort::try_new(port).expect("valid route port"),
    )
}
