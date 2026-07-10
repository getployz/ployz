use ployz::commands::network::NetworkStatusOutput;
use ployz::commands::ops::{OpsWatchOutput, StatusOutput, WatchOutput};
use ployz::commands::{PloyzctlCommand, parse_command};
use ployz_core::dataplane::{
    EbpfForwardingReady, MachineEndpointSubnet, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
};
use ployz_core::machine::MachineName;
use ployz_core::state::{ActiveMachineState, MachineLifecycle};
use ployz_sdk_types::{
    NetworkDataplaneTestimony, NetworkInternalDnsTestimony, NetworkStatusMachine,
};
use ployz_test_support::ids::{event_sequence, machine_id, operation_id};

#[test]
fn cli_parses_network_status_probe() {
    let command = parse_command(["network", "status", "--probe"].map(str::to_owned))
        .expect("network status parses");
    let PloyzctlCommand::NetworkStatus(command) = command else {
        panic!("expected network status command");
    };

    assert_eq!(
        command.into_request(),
        ployz_sdk_types::NetworkStatusRequest { probe: true }
    );
}

#[test]
fn cli_parses_network_resolve() {
    let command = parse_command(["network", "resolve", "web.team-a"].map(str::to_owned))
        .expect("network resolve parses");
    let PloyzctlCommand::NetworkResolve(command) = command else {
        panic!("expected network resolve command");
    };

    assert_eq!(command.into_request().name, "web.team-a");
}

#[test]
fn cli_parses_detached_network_repair() {
    let command = parse_command(
        ["network", "repair", "--machine", "machine_a", "--detach"].map(str::to_owned),
    )
    .expect("network repair parses");
    let PloyzctlCommand::NetworkRepair(command) = command else {
        panic!("expected network repair command");
    };

    assert!(command.detach);
    let request = command.into_request();
    assert_eq!(request.machine_id, Some(machine_id("machine_a")));
    assert!(
        request
            .operation_id
            .as_str()
            .starts_with("op_network_repair_cluster_")
    );
}

#[test]
fn network_status_keeps_no_answer_machine_row() {
    let output = NetworkStatusOutput {
        machines: vec![NetworkStatusMachine {
            active: ActiveMachineState {
                machine_id: machine_id("machine_a"),
                name: MachineName::try_new("edge-a").expect("valid machine name"),
                activated_by: operation_id("op_machine_a"),
                lifecycle: MachineLifecycle::Active,
                control_endpoints: vec!["203.0.113.10".parse().expect("valid IP")],
                mesh_endpoints: vec!["203.0.113.10:51820".parse().expect("valid socket address")],
                endpoint_subnet: MachineEndpointSubnet::try_new("10.198.1.0/24")
                    .expect("valid endpoint subnet"),
            },
            dataplane: NetworkDataplaneTestimony::NoAnswer,
            internal_dns: NetworkInternalDnsTestimony::NoAnswer,
        }],
    }
    .render();

    assert_eq!(
        output,
        "machine machine_a edge-a endpoint-subnet 10.198.1.0/24\n  dataplane no answer\n  internal-dns no answer\n"
    );
}

#[test]
fn network_repair_status_renders_typed_failure() {
    let output = StatusOutput::new(
        ployz_sdk_types::OperationStatusSnapshot {
            status: ployz_sdk_types::OperationStatus::NetworkRepair {
                id: operation_id("op_network_repair"),
                target_machine_id: None,
                state: ployz_sdk_types::NetworkRepairOperationState::Failed {
                    failure: ployz_sdk_types::NetworkRepairFailure::NoActiveMachines,
                },
                last_event_sequence: event_sequence(3),
            },
        },
        Vec::new(),
    )
    .render();

    assert!(output.contains("failure no-active-machines\n"));
}

#[test]
fn targeted_network_repair_status_renders_machine_subject() {
    let output = StatusOutput::new(
        ployz_sdk_types::OperationStatusSnapshot {
            status: ployz_sdk_types::OperationStatus::NetworkRepair {
                id: operation_id("op_network_repair"),
                target_machine_id: Some(machine_id("machine_a")),
                state: ployz_sdk_types::NetworkRepairOperationState::Running {
                    stage: ployz_sdk_types::NetworkRepairRunningStage::PreparingDataplane,
                },
                last_event_sequence: event_sequence(2),
            },
        },
        Vec::new(),
    )
    .render();

    assert!(output.contains("\nmachine machine_a\n"));
}

#[test]
fn network_repair_watch_renders_dataplane_failure_evidence() {
    let output = WatchOutput {
        events: vec![ployz_sdk_types::ReplayedOperationEvent {
            sequence: event_sequence(3),
            event: ployz_sdk_types::OperationEvent::NetworkRepairFailed {
                operation_id: operation_id("op_network_repair"),
                failure: ployz_sdk_types::NetworkRepairFailure::DataplaneConvergenceFailed {
                    machine_id: machine_id("machine_a"),
                    component: ployz_sdk_types::PloyzNativeMeshComponent::WireGuard,
                    message: ployz_sdk_types::FailureMessage::try_new("prepare rejected")
                        .expect("valid failure message"),
                },
            },
        }],
        output: OpsWatchOutput::Text,
    }
    .render();

    assert_eq!(
        output,
        "3 network.repair.failed dataplane-convergence-failed machine=machine_a component=wireguard message=prepare rejected\n"
    );
}

#[test]
fn network_repair_watch_renders_dataplane_prepared_evidence() {
    let report = PloyzNativeMeshPrepareReport::from_machines([PloyzNativeMeshMachineReady {
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
    .expect("valid dataplane report");
    let output = WatchOutput {
        events: vec![ployz_sdk_types::ReplayedOperationEvent {
            sequence: event_sequence(3),
            event: ployz_sdk_types::OperationEvent::NetworkRepairDataplanePrepared {
                operation_id: operation_id("op_network_repair"),
                report,
            },
        }],
        output: OpsWatchOutput::Text,
    }
    .render();

    assert_eq!(output, "3 network.repair.dataplane_prepared\n");
}
