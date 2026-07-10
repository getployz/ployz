use ployz_core::dataplane::{
    EbpfForwardingReady, PloyzNativeMeshMachineReady, PloyzNativeMeshPrepareReport,
    PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
};
use ployz_core::ops::{
    NetworkRepairOperationState, NetworkRepairRunningStage, NetworkRepairTransition,
    OperationEvent, OperationProjection, OperationStatus, ProjectionOperationState,
    StatusProjectionError, project_network_repair_transition, project_operation_event,
};
use ployz_test_support::ids::machine_id;
use ployz_test_support::ids::{event_sequence, operation_id};

fn dataplane_report() -> PloyzNativeMeshPrepareReport {
    PloyzNativeMeshPrepareReport::from_machines([PloyzNativeMeshMachineReady {
        machine_id: machine_id("machine_a"),
        ready: PloyzNativeMeshReady {
            wireguard: WireGuardReady {
                public_key: WireGuardPublicKey::try_new("public-key-a")
                    .expect("valid wireguard public key"),
                evidence: Vec::new(),
            },
            ebpf_forwarding: EbpfForwardingReady {
                evidence: Vec::new(),
            },
        },
    }])
    .expect("valid dataplane report")
}

#[test]
fn network_repair_projects_running_then_completed() {
    let accepted = OperationStatus::network_repair_accepted(
        operation_id("op_network_repair"),
        event_sequence(1),
    );
    let running = project_network_repair_transition(
        &accepted,
        NetworkRepairTransition::Running {
            stage: NetworkRepairRunningStage::PreparingDataplane,
        },
        event_sequence(2),
    )
    .expect("accepted repair starts running");
    let OperationProjection::StatusChanged { status: running } = running else {
        panic!("running transition changes status");
    };

    assert_eq!(
        project_network_repair_transition(
            &running,
            NetworkRepairTransition::Completed,
            event_sequence(3),
        )
        .expect("running repair completes"),
        OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::NetworkRepair {
                id: operation_id("op_network_repair"),
                state: NetworkRepairOperationState::Completed,
                last_event_sequence: event_sequence(3),
            }),
        }
    );
}

#[test]
fn network_repair_dataplane_prepared_evidence_advances_projection_cursor() {
    let running = OperationStatus::NetworkRepair {
        id: operation_id("op_network_repair"),
        state: NetworkRepairOperationState::Running {
            stage: NetworkRepairRunningStage::PreparingDataplane,
        },
        last_event_sequence: event_sequence(2),
    };

    assert_eq!(
        project_operation_event(
            &running,
            OperationEvent::NetworkRepairDataplanePrepared {
                operation_id: operation_id("op_network_repair"),
                report: dataplane_report(),
            },
            event_sequence(3),
        )
        .expect("dataplane evidence projects"),
        OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::NetworkRepair {
                id: operation_id("op_network_repair"),
                state: NetworkRepairOperationState::Running {
                    stage: NetworkRepairRunningStage::PreparingDataplane,
                },
                last_event_sequence: event_sequence(3),
            }),
        }
    );
}

#[test]
fn network_repair_dataplane_prepared_evidence_has_stable_singleton_subject() {
    let event = OperationEvent::NetworkRepairDataplanePrepared {
        operation_id: operation_id("op_network_repair"),
        report: dataplane_report(),
    };

    assert_eq!(
        (event.subject_suffix(), event.singleton_subject()),
        (
            "network.repair.dataplane.prepared".to_owned(),
            Some("network.repair.dataplane.prepared"),
        )
    );
}

#[test]
fn network_repair_rejects_completed_before_running() {
    let accepted = OperationStatus::network_repair_accepted(
        operation_id("op_network_repair"),
        event_sequence(1),
    );

    assert_eq!(
        project_network_repair_transition(
            &accepted,
            NetworkRepairTransition::Completed,
            event_sequence(2),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_network_repair"),
            current: Box::new(ProjectionOperationState::NetworkRepair(
                NetworkRepairOperationState::Accepted,
            )),
            attempted: Box::new(ProjectionOperationState::NetworkRepair(
                NetworkRepairOperationState::Completed,
            )),
        })
    );
}

#[test]
fn network_repair_rejects_failed_before_running() {
    let accepted = OperationStatus::network_repair_accepted(
        operation_id("op_network_repair"),
        event_sequence(1),
    );
    let failure = ployz_core::ops::NetworkRepairFailure::NoActiveMachines;

    assert_eq!(
        project_network_repair_transition(
            &accepted,
            NetworkRepairTransition::Failed {
                failure: failure.clone(),
            },
            event_sequence(2),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_network_repair"),
            current: Box::new(ProjectionOperationState::NetworkRepair(
                NetworkRepairOperationState::Accepted,
            )),
            attempted: Box::new(ProjectionOperationState::NetworkRepair(
                NetworkRepairOperationState::Failed { failure },
            )),
        })
    );
}

#[test]
fn network_repair_terminal_state_is_final() {
    let completed = OperationStatus::NetworkRepair {
        id: operation_id("op_network_repair"),
        state: NetworkRepairOperationState::Completed,
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_network_repair_transition(
            &completed,
            NetworkRepairTransition::Running {
                stage: NetworkRepairRunningStage::PreparingDataplane,
            },
            event_sequence(4),
        ),
        Err(StatusProjectionError::TerminalState {
            operation_id: operation_id("op_network_repair"),
            current: Box::new(ProjectionOperationState::NetworkRepair(
                NetworkRepairOperationState::Completed,
            )),
            attempted: Box::new(ProjectionOperationState::NetworkRepair(
                NetworkRepairOperationState::Running {
                    stage: NetworkRepairRunningStage::PreparingDataplane,
                },
            )),
        })
    );
}
