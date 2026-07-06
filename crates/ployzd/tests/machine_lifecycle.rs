//! Machine lifecycle operations against real NATS stores: drain and resume
//! commit operator intent to the machine's roster row.

use ployz_core::install::{DEFAULT_MACHINE_BOOTSTRAP_URL, MachineBootstrapUrl};
use ployz_core::machine::active_machine_from_completed_add;
use ployz_core::ops::{MachineLifecycleFailure, MachineLifecycleOperationState, OperationStatus};
use ployz_core::state::MachineLifecycle;
use ployzd::controllers::{
    MachineAddBootstrapConfig, MachineLifecycleSubmitCommand, OperationControllers,
};
use ployzd::operations::machine_lifecycle::MachineLifecycleOperationRuntime;
use ployzd::machine_roster::MachineRosterStore;
use ployzd::operations::log::OperationRepository;
use ployzd::tasks::TaskRegistry;

use ployz_test_support::ids::{machine_id, operation_id};

#[path = "support/mod.rs"]
mod support;

#[tokio::test]
async fn drain_records_lifecycle_evidence_and_resume_reverts() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let controllers = operation_controllers(nats.controller.clone()).await;
    let machine_roster = MachineRosterStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    seed_active_machine(&machine_roster, "machine_a").await;

    let runtime = MachineLifecycleOperationRuntime::new(
        nats.controller.clone(),
        controllers.clone(),
        machine_roster.clone(),
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

    assert_eq!(
        drained_lifecycle(&machine_roster, "machine_a").await,
        MachineLifecycle::Draining
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

    assert_eq!(
        drained_lifecycle(&machine_roster, "machine_a").await,
        MachineLifecycle::Active
    );
    assert_terminal_completed(&controllers, "op_resume_1").await;
}

async fn drained_lifecycle(machine_roster: &MachineRosterStore, machine: &str) -> MachineLifecycle {
    machine_roster
        .active_machine(&machine_id(machine))
        .await
        .expect("machine reads")
        .expect("machine exists")
        .lifecycle
}

#[tokio::test]
async fn drain_of_unknown_machine_fails_without_writing_evidence() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let controllers = operation_controllers(nats.controller.clone()).await;
    let machine_roster = MachineRosterStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );

    let runtime = MachineLifecycleOperationRuntime::new(
        nats.controller.clone(),
        controllers.clone(),
        machine_roster.clone(),
        TaskRegistry::default(),
    );

    let accepted = controllers
        .submit_machine_lifecycle(MachineLifecycleSubmitCommand {
            operation_id: operation_id("op_drain_ghost"),
            machine_id: machine_id("machine_ghost"),
            target: MachineLifecycle::Draining,
        })
        .await
        .expect("drain accepted");
    runtime.clone().run(accepted).await;

    assert!(
        machine_roster
            .active_machine(&machine_id("machine_ghost"))
            .await
            .expect("machine reads")
            .is_none(),
        "a failed drain must not create a machine record"
    );
    let status = controllers
        .repository()
        .get(&operation_id("op_drain_ghost"))
        .await
        .expect("status reads")
        .expect("status exists");
    let OperationStatus::MachineLifecycle { state, .. } = status else {
        panic!("expected machine lifecycle status, got {status:?}");
    };
    assert!(
        matches!(
            state,
            MachineLifecycleOperationState::Failed {
                failure: MachineLifecycleFailure::NoSuchMachine { .. }
            }
        ),
        "expected NoSuchMachine failure, got {state:?}"
    );
}

async fn seed_active_machine(machine_roster: &MachineRosterStore, machine: &str) {
    let active = active_machine_from_completed_add(
        operation_id("op_add"),
        machine_id(machine),
        ployz_core::machine::MachineName::try_new(machine).expect("valid machine name"),
        ployz_core::ops::MachineAddOperationState::Completed,
    )
    .expect("completed add activates");
    machine_roster
        .replace_active_machine(&active)
        .await
        .expect("machine record writes");
}

async fn assert_terminal_completed(controllers: &OperationControllers, operation: &str) {
    let status = controllers
        .repository()
        .get(&operation_id(operation))
        .await
        .expect("status reads")
        .expect("status exists");
    let OperationStatus::MachineLifecycle { state, .. } = status else {
        panic!("expected machine lifecycle status, got {status:?}");
    };
    assert_eq!(state, MachineLifecycleOperationState::Completed);
}

async fn operation_controllers(client: async_nats::Client) -> OperationControllers {
    let evidence_dir = tempfile::tempdir()
        .expect("operation evidence temp dir")
        .keep();
    let core_store = ployzd::core_store::CoreStore::open(evidence_dir.join("ployz-core.db"))
        .await
        .expect("open core store");
    OperationControllers::new(
        OperationRepository::open(core_store, client),
        MachineAddBootstrapConfig::new(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .expect("default bootstrap URL is valid"),
        ),
    )
}
