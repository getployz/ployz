use futures_util::StreamExt;
use ployz_core::state::{ActiveMachineState, IntentSnapshot, MachineLifecycle};
use ployz_core::subjects::INTENT_CHANGED;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_test_support::ids::{machine_id, operation_id};
use ployzd::intent::{NatsIntentReader, start_intent_runtime};
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
            substrate_versions: None,
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
    let _runtime =
        start_intent_runtime(nats.controller.clone(), core_state, Duration::from_secs(30))
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
