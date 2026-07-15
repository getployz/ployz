//! Machine lifecycle operations against real NATS stores: drain and resume
//! commit operator intent to the machine's roster row.

use crate::control::intent::machine_roster::MachineRosterStore;
use crate::control::operation_evidence::OperationRepository;
use crate::control::operations::machine_lifecycle::MachineLifecycleOperation;
use crate::control::sequencer::{
    MachineAddBootstrapConfig, MachineLifecycleSubmitCommand, OperationControllers,
};
use crate::tasks::TaskRegistry;
use ployz_core::install::{DEFAULT_MACHINE_BOOTSTRAP_URL, MachineBootstrapUrl};
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::active_machine_from_completed_add;
use ployz_core::operation::{
    MachineLifecycleFailure, MachineLifecycleOperationState, OperationEvent,
    OperationEventReplayRequest, OperationInterruptionCause, OperationStatus,
};
use std::time::Duration;

use ployz_test_support::ids::{event_replay_limit, event_sequence, machine_id, operation_id};

#[tokio::test]
async fn drain_records_lifecycle_evidence_and_resume_reverts() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let controllers = operation_controllers(nats.controller.clone()).await;
    let machine_roster = MachineRosterStore::new(
        crate::control::store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    seed_active_machine(&machine_roster, "machine_a").await;
    let tasks = TaskRegistry::default();

    let runtime = MachineLifecycleOperation::new(
        nats.controller.clone(),
        controllers.clone(),
        machine_roster.clone(),
        tasks.spawner(),
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
        crate::control::store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let tasks = TaskRegistry::default();

    let runtime = MachineLifecycleOperation::new(
        nats.controller.clone(),
        controllers.clone(),
        machine_roster.clone(),
        tasks.spawner(),
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

#[tokio::test]
async fn rejected_worker_admission_records_terminal_interruption_evidence() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let controllers = operation_controllers(nats.controller.clone()).await;
    let machine_roster = MachineRosterStore::new(
        crate::control::store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    seed_active_machine(&machine_roster, "machine_a").await;
    let tasks = TaskRegistry::default();
    let runtime = MachineLifecycleOperation::new(
        nats.controller.clone(),
        controllers.clone(),
        machine_roster,
        tasks.spawner(),
    );
    tasks
        .abort_and_join(Duration::from_secs(1))
        .await
        .expect("task supervisor closes");
    let accepted = controllers
        .submit_machine_lifecycle(MachineLifecycleSubmitCommand {
            operation_id: operation_id("op_rejected_worker"),
            machine_id: machine_id("machine_a"),
            target: MachineLifecycle::Draining,
        })
        .await
        .expect("operation accepts before worker admission");

    runtime.start(accepted).await;

    let status = controllers
        .repository()
        .get(&operation_id("op_rejected_worker"))
        .await
        .expect("status reads")
        .expect("status exists");
    let OperationStatus::MachineLifecycle {
        state: MachineLifecycleOperationState::Interrupted { evidence },
        ..
    } = status
    else {
        panic!("rejected worker admission must be terminal: {status:?}");
    };
    assert_eq!(
        evidence.cause(),
        OperationInterruptionCause::ControlShutdown
    );
    let events = controllers
        .repository()
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_rejected_worker"),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(10),
        })
        .await
        .expect("operation events replay");
    assert!(matches!(
        events.events.last().map(|event| &event.event),
        Some(OperationEvent::OperationInterrupted { evidence, .. })
            if evidence.cause() == OperationInterruptionCause::ControlShutdown
    ));
}

async fn seed_active_machine(machine_roster: &MachineRosterStore, machine: &str) {
    let active = active_machine_from_completed_add(
        ployz_core::machine::MachineName::try_new(machine).expect("valid machine name"),
        ployz_core::machine::roles::InstallRolePolicy::install_all(),
        ployz_core::intent::StagedMachineDataplaneState {
            operation_id: operation_id("op_add"),
            machine_id: machine_id(machine),
            endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.198.0.0/24")
                .expect("valid endpoint subnet"),
            mesh_endpoints: vec!["192.0.2.10:51820".parse().expect("mesh endpoint")],
            wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(format!(
                "public-{machine}"
            ))
            .expect("public key"),
        },
        ployz_core::operation::MachineAddOperationState::Completed,
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
    let core_store = crate::control::store::CoreStore::open(evidence_dir.join("ployz-core.db"))
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
