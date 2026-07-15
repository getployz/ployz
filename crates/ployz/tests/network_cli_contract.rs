use std::net::Ipv4Addr;

use ployz::commands::{PloyzctlCommand, parse_command};
use ployz::network::command::{NetworkResolveOutput, NetworkStatusOutput};
use ployz::operation::command::{OpsWatchOutput, StatusOutput, WatchOutput};
use ployz_core::ids::{NamespaceId, ServiceId};
use ployz_core::intent::ActiveMachineState;
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::MachineName;
use ployz_core::network::internal_dns::InternalServiceName;
use ployz_core::network::{DataplaneProjection, MachineEndpointSubnet};
use ployz_core::operation::FailureMessage;
use ployz_core::roles::InstallRolePolicy;

use ployz_sdk_types::{
    NetworkDataplaneTestimony, NetworkInternalDnsTestimony, NetworkResolveMachineTestimony,
    NetworkResolveResult, NetworkStatusMachine,
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
        ployz_sdk_types::NetworkStatusRequest::First {
            mode: ployz_sdk_types::NetworkStatusMode::ProbePathMtu,
        }
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
            .starts_with("op_network_repair_machine_a_")
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
                roles: InstallRolePolicy::install_all(),
                lifecycle: MachineLifecycle::Active,
                control_endpoints: vec!["203.0.113.10".parse().expect("valid IP")],
                mesh_endpoints: vec!["203.0.113.10:51820".parse().expect("valid socket address")],
                endpoint_subnet: MachineEndpointSubnet::try_new("10.198.1.0/24")
                    .expect("valid endpoint subnet"),
                wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(
                    "public-machine-a",
                )
                .expect("public key"),
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
fn network_status_renders_dataplane_failures() {
    let failures = [
        (
            NetworkDataplaneTestimony::ReadFailed {
                message: FailureMessage::try_new("wg read failed").expect("failure message"),
            },
            "dataplane read failed: wg read failed",
        ),
        (
            NetworkDataplaneTestimony::WrongResponder {
                actual_machine_id: machine_id("machine_b"),
            },
            "dataplane wrong responder: machine_b",
        ),
        (NetworkDataplaneTestimony::TimedOut, "dataplane timed out"),
        (
            NetworkDataplaneTestimony::RequestFailed {
                message: "request failed".to_owned(),
            },
            "dataplane request failed: request failed",
        ),
        (
            NetworkDataplaneTestimony::ProtocolFailed {
                message: "bad headers".to_owned(),
            },
            "dataplane protocol failed: bad headers",
        ),
        (
            NetworkDataplaneTestimony::DecodeFailed {
                message: "bad json".to_owned(),
            },
            "dataplane decode failed: bad json",
        ),
    ];

    for (dataplane, expected) in failures {
        let output = NetworkStatusOutput {
            machines: vec![status_machine(dataplane)],
        }
        .render();
        assert!(output.contains(expected));
    }
}

#[test]
fn network_status_renders_internal_dns_failures() {
    let failures = [
        (
            NetworkInternalDnsTestimony::WrongResponder {
                actual_machine_id: machine_id("machine_b"),
            },
            "internal-dns wrong responder: machine_b",
        ),
        (
            NetworkInternalDnsTestimony::TimedOut,
            "internal-dns timed out",
        ),
        (
            NetworkInternalDnsTestimony::RequestFailed {
                message: "request failed".to_owned(),
            },
            "internal-dns request failed: request failed",
        ),
        (
            NetworkInternalDnsTestimony::ProtocolFailed {
                message: "bad headers".to_owned(),
            },
            "internal-dns protocol failed: bad headers",
        ),
        (
            NetworkInternalDnsTestimony::DecodeFailed {
                message: "bad json".to_owned(),
            },
            "internal-dns decode failed: bad json",
        ),
    ];

    for (internal_dns, expected) in failures {
        let mut machine = status_machine(NetworkDataplaneTestimony::NoAnswer);
        machine.internal_dns = internal_dns;
        let output = NetworkStatusOutput {
            machines: vec![machine],
        }
        .render();
        assert!(output.contains(expected));
    }
}

#[test]
fn network_resolve_marks_consistent_and_divergent_answer_sets() {
    let name = InternalServiceName::try_from_ids(
        &ServiceId::try_new("web").expect("service id"),
        &NamespaceId::try_new("team-a").expect("namespace id"),
    )
    .expect("internal service name");
    let testimony = |machine: &str, last_octet| NetworkResolveMachineTestimony::Answered {
        machine_id: machine_id(machine),
        addresses: vec![Ipv4Addr::new(10, 42, 1, last_octet)],
    };
    let consistent = NetworkResolveOutput::new(NetworkResolveResult {
        name: name.clone(),
        machines: vec![testimony("machine_a", 8), testimony("machine_b", 8)],
    })
    .render();
    let divergent = NetworkResolveOutput::new(NetworkResolveResult {
        name,
        machines: vec![testimony("machine_a", 8), testimony("machine_b", 9)],
    })
    .render();

    assert!(consistent.starts_with("answer-sets consistent\n"));
    assert!(divergent.starts_with("answer-sets divergent\n"));
}

#[test]
fn network_resolve_renders_typed_failures() {
    let name = InternalServiceName::try_from_ids(
        &ServiceId::try_new("web").expect("service id"),
        &NamespaceId::try_new("team-a").expect("namespace id"),
    )
    .expect("internal service name");
    let output = NetworkResolveOutput::new(NetworkResolveResult {
        name,
        machines: vec![
            NetworkResolveMachineTestimony::WrongResponder {
                machine_id: machine_id("machine_a"),
                actual_machine_id: machine_id("machine_b"),
            },
            NetworkResolveMachineTestimony::TimedOut {
                machine_id: machine_id("machine_b"),
            },
            NetworkResolveMachineTestimony::RequestFailed {
                machine_id: machine_id("machine_c"),
                message: "resolver unavailable".to_owned(),
            },
            NetworkResolveMachineTestimony::ProtocolFailed {
                machine_id: machine_id("machine_d"),
                message: "wrong echo".to_owned(),
            },
            NetworkResolveMachineTestimony::DecodeFailed {
                machine_id: machine_id("machine_e"),
                message: "bad json".to_owned(),
            },
        ],
    })
    .render();

    for expected in [
        "machine machine_a web.team-a.internal wrong responder: machine_b",
        "machine machine_b web.team-a.internal timed out",
        "machine machine_c web.team-a.internal request failed: resolver unavailable",
        "machine machine_d web.team-a.internal protocol failed: wrong echo",
        "machine machine_e web.team-a.internal decode failed: bad json",
    ] {
        assert!(output.contains(expected));
    }
    assert!(output.starts_with("answer-sets unconfirmed answered=0/5\n"));
}

#[test]
fn network_resolve_marks_partial_testimony_unconfirmed() {
    let name = InternalServiceName::try_from_ids(
        &ServiceId::try_new("web").expect("service id"),
        &NamespaceId::try_new("team-a").expect("namespace id"),
    )
    .expect("internal service name");
    let output = NetworkResolveOutput::new(NetworkResolveResult {
        name,
        machines: vec![
            NetworkResolveMachineTestimony::Answered {
                machine_id: machine_id("machine_a"),
                addresses: vec![Ipv4Addr::new(10, 42, 1, 8)],
            },
            NetworkResolveMachineTestimony::NoAnswer {
                machine_id: machine_id("machine_b"),
            },
        ],
    })
    .render();

    assert!(output.starts_with("answer-sets unconfirmed answered=1/2\n"));
}

fn status_machine(dataplane: NetworkDataplaneTestimony) -> NetworkStatusMachine {
    NetworkStatusMachine {
        active: ActiveMachineState {
            machine_id: machine_id("machine_a"),
            name: MachineName::try_new("edge-a").expect("valid machine name"),
            activated_by: operation_id("op_machine_a"),
            roles: InstallRolePolicy::install_all(),
            lifecycle: MachineLifecycle::Active,
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.198.1.0/24")
                .expect("valid endpoint subnet"),
            wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(
                "public-machine-a",
            )
            .expect("public key"),
        },
        dataplane,
        internal_dns: NetworkInternalDnsTestimony::NoAnswer,
    }
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
fn network_repair_status_renders_missing_projection_member() {
    let revision = ployz_sdk_types::DataplaneProjection::try_new(Vec::new(), None)
        .expect("empty projection")
        .declared_revision()
        .clone();
    let output = StatusOutput::new(
        ployz_sdk_types::OperationStatusSnapshot {
            status: ployz_sdk_types::OperationStatus::NetworkRepair {
                id: operation_id("op_network_repair"),
                target_machine_id: Some(machine_id("machine_a")),
                state: ployz_sdk_types::NetworkRepairOperationState::Failed {
                    failure: ployz_sdk_types::NetworkRepairFailure::ProjectionMemberMissing {
                        machine_id: machine_id("machine_a"),
                        revision: revision.clone(),
                    },
                },
                last_event_sequence: event_sequence(3),
            },
        },
        Vec::new(),
    )
    .render();

    assert!(output.contains(&format!(
        "failure projection-member-missing machine=machine_a revision={}\n",
        revision.as_str()
    )));
}

#[test]
fn targeted_network_repair_status_renders_machine_subject() {
    let output = StatusOutput::new(
        ployz_sdk_types::OperationStatusSnapshot {
            status: ployz_sdk_types::OperationStatus::NetworkRepair {
                id: operation_id("op_network_repair"),
                target_machine_id: Some(machine_id("machine_a")),
                state: ployz_sdk_types::NetworkRepairOperationState::Running {
                    stage: ployz_sdk_types::NetworkRepairRunningStage::AwaitingDataplane,
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
                failure: ployz_sdk_types::NetworkRepairFailure::DataplaneUnavailable {
                    machine_id: machine_id("machine_a"),
                    reason: ployz_sdk_types::DataplaneProjectionAdmissionFailure::NoAnswer {
                        message: ployz_sdk_types::FailureMessage::try_new("prepare rejected")
                            .expect("valid failure message"),
                    },
                },
            },
        }],
        output: OpsWatchOutput::Text,
    }
    .render();

    assert_eq!(
        output,
        "3 network.repair.failed dataplane-unavailable machine=machine_a reason=no answer: prepare rejected\n"
    );
}

#[test]
fn network_repair_watch_renders_dataplane_converged_evidence() {
    let revision = DataplaneProjection::try_new(Vec::new(), None)
        .expect("empty projection")
        .declared_revision()
        .clone();
    let output = WatchOutput {
        events: vec![ployz_sdk_types::ReplayedOperationEvent {
            sequence: event_sequence(3),
            event: ployz_sdk_types::OperationEvent::NetworkRepairDataplaneConverged {
                operation_id: operation_id("op_network_repair"),
                revision,
                machine_ids: vec![machine_id("machine_a")],
            },
        }],
        output: OpsWatchOutput::Text,
    }
    .render();

    assert_eq!(output, "3 network.repair.dataplane_converged\n");
}
