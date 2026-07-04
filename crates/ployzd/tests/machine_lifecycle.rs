//! Machine lifecycle operations against real NATS stores: drain and resume
//! commit operator intent to the KV machine record with on-disk evidence,
//! and the evidence file is adopted back into KV on control start.

use async_nats::jetstream;
use ployz_core::install::{DEFAULT_MACHINE_BOOTSTRAP_URL, MachineBootstrapUrl};
use ployz_core::machine::active_machine_from_completed_add;
use ployz_core::ops::{MachineLifecycleOperationState, OperationStatus};
use ployz_core::state::MachineLifecycle;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::operations::{AsyncNatsOperationEventLog, AsyncNatsOperationStatusStore};
use ployzd::controllers::{
    MachineAddBootstrapConfig, MachineLifecycleSubmitCommand, OperationControllers,
};
use ployzd::machine_lifecycle_runtime::{
    MachineLifecycleOperationRuntime, adopt_machine_lifecycles_from_file,
};
use ployzd::tasks::TaskRegistry;

use ployz_test_support::ids::{machine_id, operation_id};

#[path = "support/mod.rs"]
mod support;

#[tokio::test]
async fn drain_commits_lifecycle_with_evidence_and_resume_reverts() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    nats.bootstrap_resources().await;
    let jetstream = nats.jetstream.clone();
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let controllers = operation_controllers(&jetstream).await;
    let work_dir = tempfile::tempdir().expect("evidence dir");
    let evidence_file = work_dir.path().join("machine-lifecycles.json");
    seed_active_machine(&core_state, "machine_a").await;

    let runtime = MachineLifecycleOperationRuntime::new(
        controllers.clone(),
        core_state.clone(),
        evidence_file.clone(),
        TaskRegistry::default(),
    );

    let accepted = controllers
        .submit_machine_lifecycle(MachineLifecycleSubmitCommand {
            operation_id: operation_id("op_drain_1"),
            machine_id: machine_id("machine_a"),
            target: MachineLifecycle::Draining,
        })
        .await
        .expect("drain accepted");
    runtime.clone().run(accepted).await;

    let drained = core_state
        .active_machine(&machine_id("machine_a"))
        .await
        .expect("machine reads")
        .expect("machine exists");
    assert_eq!(drained.lifecycle, MachineLifecycle::Draining);
    let evidence = std::fs::read_to_string(&evidence_file).expect("evidence file written");
    assert!(
        evidence.contains("machine_a"),
        "evidence records the drained machine: {evidence}"
    );
    assert_terminal_completed(&controllers, "op_drain_1").await;

    let accepted = controllers
        .submit_machine_lifecycle(MachineLifecycleSubmitCommand {
            operation_id: operation_id("op_resume_1"),
            machine_id: machine_id("machine_a"),
            target: MachineLifecycle::Active,
        })
        .await
        .expect("resume accepted");
    runtime.clone().run(accepted).await;

    let resumed = core_state
        .active_machine(&machine_id("machine_a"))
        .await
        .expect("machine reads")
        .expect("machine exists");
    assert_eq!(resumed.lifecycle, MachineLifecycle::Active);
    let evidence = std::fs::read_to_string(&evidence_file).expect("evidence file exists");
    assert!(
        !evidence.contains("machine_a"),
        "resume clears the drained record: {evidence}"
    );
    assert_terminal_completed(&controllers, "op_resume_1").await;
}

#[tokio::test]
async fn lifecycle_evidence_is_adopted_into_kv_on_start() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    nats.bootstrap_resources().await;
    let jetstream = nats.jetstream.clone();
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let work_dir = tempfile::tempdir().expect("evidence dir");
    let evidence_file = work_dir.path().join("machine-lifecycles.json");
    std::fs::write(&evidence_file, r#"{"draining":["machine_a"]}"#).expect("seed evidence file");
    seed_active_machine(&core_state, "machine_a").await;
    seed_active_machine(&core_state, "machine_b").await;

    adopt_machine_lifecycles_from_file(&evidence_file, &core_state)
        .await
        .expect("adoption succeeds");

    let drained = core_state
        .active_machine(&machine_id("machine_a"))
        .await
        .expect("machine reads")
        .expect("machine exists");
    assert_eq!(drained.lifecycle, MachineLifecycle::Draining);
    let untouched = core_state
        .active_machine(&machine_id("machine_b"))
        .await
        .expect("machine reads")
        .expect("machine exists");
    assert_eq!(untouched.lifecycle, MachineLifecycle::Active);
}

async fn seed_active_machine(core_state: &AsyncNatsCoreStateStore, machine: &str) {
    let active = active_machine_from_completed_add(
        operation_id("op_add"),
        machine_id(machine),
        ployz_core::machine::MachineName::try_new(machine).expect("valid machine name"),
        ployz_core::machine::MachineAddOperationState::Completed,
    )
    .expect("completed add activates");
    core_state
        .replace_active_machine(&active)
        .await
        .expect("machine record writes");
}

async fn assert_terminal_completed(controllers: &OperationControllers, operation: &str) {
    let status = controllers
        .repository()
        .records()
        .get(&operation_id(operation))
        .await
        .expect("status reads")
        .expect("status exists");
    let OperationStatus::MachineLifecycle { state, .. } = status else {
        panic!("expected machine lifecycle status, got {status:?}");
    };
    assert_eq!(state, MachineLifecycleOperationState::Completed);
}

async fn operation_controllers(jetstream: &jetstream::Context) -> OperationControllers {
    OperationControllers::new(
        AsyncNatsOperationEventLog::new(jetstream.clone()),
        AsyncNatsOperationStatusStore::from_jetstream(jetstream)
            .await
            .expect("open operation status store"),
        AsyncNatsCoreStateStore::from_jetstream(jetstream)
            .await
            .expect("open core state store"),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    )
}
