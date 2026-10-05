//! User-facing failures retain useful identity and causes without Rust Debug syntax.
use ployz_core::{
    CodecError, ContainerAction, ContainerId, ContainerKind, ContainerRoleError,
    ContainerRuntimeObservation, ContainerSelector, ContainerSelectorError, DockerVolumeId,
    DockerVolumeName, ExecutionError, HealthFailure, HealthObservation, HookFailure,
    HttpCheckError, LastHealthCheck, MachineAction, MachineId, MachineSelectorError, MachineTarget,
    RpcError, RpcErrorCode,
    stream::{ExecRequestFrame, ExecResponseFrame},
};
use serde_json::json;

#[test]
fn selector_errors_list_plain_targets_and_ids() {
    let missing = MachineSelectorError::NotFound(
        ["east", "west"]
            .map(|name| MachineTarget::parse(name).unwrap())
            .to_vec(),
    )
    .to_string();
    assert!(missing.contains(r#""east", "west""#), "{missing}");
    let ids = [
        MachineId::parse("1".repeat(32)).unwrap(),
        MachineId::parse("2".repeat(32)).unwrap(),
    ];
    let ambiguous = MachineSelectorError::Ambiguous {
        selector: MachineTarget::parse("edge").unwrap(),
        matches: ids.to_vec(),
    }
    .to_string();
    assert!(
        ambiguous.starts_with("edge matches more than one Server"),
        "{ambiguous}"
    );
    assert!(
        ambiguous.contains(&format!("{}, {}", ids[0], ids[1])),
        "{ambiguous}"
    );
    let containers = [
        ContainerId::parse("a".repeat(64)).unwrap(),
        ContainerId::parse("b".repeat(64)).unwrap(),
    ];
    let ambiguous = ContainerSelectorError::Ambiguous {
        selector: ContainerSelector::parse("api").unwrap(),
        container_ids: containers.to_vec(),
    }
    .to_string();
    assert!(
        ambiguous.contains(&format!("{}, {}", containers[0], containers[1])),
        "{ambiguous}"
    );
}

#[test]
fn volume_identity_names_both_volume_and_machine() {
    let id = DockerVolumeId {
        machine_id: MachineId::parse("1".repeat(32)).unwrap(),
        name: DockerVolumeName::parse("app-data").unwrap(),
    };
    assert_eq!(
        id.to_string(),
        format!("app-data on Machine {}", id.machine_id)
    );
}

#[test]
fn deploy_failures_keep_runtime_exit_and_secondary_stop_causes() {
    let container_id = ContainerId::parse("a".repeat(64)).unwrap();
    let health = ExecutionError::Health {
        container_id,
        failure: HealthFailure::Runtime {
            observation: ContainerRuntimeObservation::Exited { code: 137 },
            last_check: None,
        },
    }
    .to_string();
    assert!(
        health.contains(container_id.as_str()) && health.contains("exited with code 137"),
        "{health}"
    );
    let stop_error = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "Machine unreachable".into(),
        details: json!(null),
    };
    for failure in [
        HookFailure::Cancelled {
            stop_error: Some(stop_error.clone()),
        },
        HookFailure::TimedOut {
            stop_error: Some(stop_error),
        },
    ] {
        let message = ExecutionError::Hook {
            container_id,
            failure,
        }
        .to_string();
        assert!(
            message.contains("stop also failed: Machine unreachable"),
            "{message}"
        );
        assert!(
            !message.contains("Some(") && !message.contains("RpcError"),
            "{message}"
        );
    }
    assert_eq!(
        HookFailure::TimedOut { stop_error: None }.to_string(),
        "timed out"
    );
    assert_eq!(
        HookFailure::Exit { code: 23 }.to_string(),
        "exited with code 23"
    );
    assert_eq!(HealthFailure::Cancelled.to_string(), "cancelled");
    assert_eq!(
        HealthFailure::TimedOut { last_check: None }.to_string(),
        "timed out"
    );
    assert_eq!(
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy
        }
        .to_string(),
        "running (health: unhealthy)"
    );
    assert_eq!(ContainerKind::PreDeployHook.to_string(), "pre-deploy hook");
    let role = ContainerRoleError {
        requested: ContainerKind::ServiceContainer,
        actual: ContainerKind::PreDeployHook,
    }
    .to_string();
    assert!(
        role.contains("pre-deploy hook") && role.contains("Service"),
        "{role}"
    );
    assert_eq!(MachineAction::PrepareVolumes.to_string(), "prepare Volumes");
    assert_eq!(ContainerAction::Stop.to_string(), "stop");
}

#[test]
fn wire_kind_errors_use_plain_names() {
    let frame = ExecResponseFrame::Stdout(b"hello".to_vec())
        .encode()
        .unwrap();
    let error = ExecRequestFrame::decode(&frame).unwrap_err().to_string();
    assert!(error.contains("received exec stdout"), "{error}");
    let error = CodecError::UnexpectedResponse {
        expected: "inspect",
        actual: "list".into(),
    }
    .to_string();
    assert!(
        error.contains("expected response kind inspect, received list"),
        "{error}"
    );
}

#[test]
fn unknown_runtime_failure_retains_observed_evidence() {
    let failure = HealthFailure::Runtime {
        observation: ContainerRuntimeObservation::Unknown {
            raw: json!({"state": "future-state\u{009b}[2J", "reason": "waiting\u{007f}"}),
        },
        last_check: None,
    }
    .to_string();
    assert!(
        failure.contains("future-state") && failure.contains("waiting"),
        "{failure}"
    );
    assert!(!failure.chars().any(char::is_control), "{failure}");
    assert!(
        !failure.contains("Unknown {") && !failure.contains("Object {"),
        "{failure}"
    );
}

#[test]
fn unknown_wire_kinds_escape_terminal_controls() {
    let response: ployz_core::RpcResponse = serde_json::from_value(json!({
        "protocol_major": ployz_core::PROTOCOL_MAJOR,
        "kind": "future\n\u{1b}[2J",
        "payload": null,
    }))
    .unwrap();
    let error = response
        .decode::<ployz_core::op::DescribeContract>()
        .unwrap_err()
        .to_string();
    assert!(error.contains(r"future\n\u{1b}[2J"), "{error}");
    assert!(!error.chars().any(char::is_control), "{error}");
}

#[test]
fn rpc_error_codes_escape_unknown_wire_values() {
    let code: RpcErrorCode = serde_json::from_value(json!("future\n\u{1b}[2J")).unwrap();
    assert_eq!(code.to_string(), r"future\n\u{1b}[2J");
    assert_eq!(RpcErrorCode::Unavailable.to_string(), "unavailable");
}

#[test]
fn unconstrained_names_and_health_failures_escape_controls() {
    let raw = "future\n\u{1b}[2J";
    let target = MachineTarget::parse(raw).unwrap();
    let machine_id = MachineId::parse("1".repeat(32)).unwrap();
    for message in [
        MachineSelectorError::NotFound(vec![target.clone()]).to_string(),
        MachineSelectorError::Ambiguous {
            selector: target,
            matches: vec![machine_id],
        }
        .to_string(),
        DockerVolumeId {
            machine_id,
            name: DockerVolumeName::parse(raw).unwrap(),
        }
        .to_string(),
        HealthFailure::Runtime {
            observation: ContainerRuntimeObservation::Running {
                health: HealthObservation::Unrecognized(raw.into()),
            },
            last_check: None,
        }
        .to_string(),
        HealthFailure::TimedOut {
            last_check: Some(LastHealthCheck::Exited {
                code: 1,
                output: raw.into(),
            }),
        }
        .to_string(),
        HookFailure::TimedOut {
            stop_error: Some(RpcError {
                code: RpcErrorCode::Unavailable,
                message: raw.into(),
                details: json!(null),
            }),
        }
        .to_string(),
    ] {
        assert!(message.contains(r"future\n\u{1b}[2J"), "{message}");
        assert!(!message.chars().any(char::is_control), "{message}");
    }
}

#[test]
fn missing_selectors_preserve_argument_boundaries() {
    let missing = MachineSelectorError::NotFound(
        ["east, west", "north"]
            .map(|name| MachineTarget::parse(name).unwrap())
            .to_vec(),
    )
    .to_string();
    assert!(missing.contains(r#""east, west", "north""#), "{missing}");
}

#[test]
fn health_failures_quote_the_last_check() {
    let unhealthy = ContainerRuntimeObservation::Running {
        health: HealthObservation::Unhealthy,
    };
    for (failure, expected) in [
        (
            HealthFailure::TimedOut {
                last_check: Some(LastHealthCheck::exited(127, "sh: pg_isready: not found\n")),
            },
            "timed out; last check exited 127: sh: pg_isready: not found",
        ),
        (
            HealthFailure::TimedOut {
                last_check: Some(LastHealthCheck::exited(1, "")),
            },
            "timed out; last check exited 1",
        ),
        (
            HealthFailure::Runtime {
                observation: unhealthy.clone(),
                last_check: Some(LastHealthCheck::HttpStatus { status: 503 }),
            },
            "running (health: unhealthy); last check: HTTP 503",
        ),
        (
            HealthFailure::Runtime {
                observation: unhealthy.clone(),
                last_check: Some(LastHealthCheck::HttpUnreachable {
                    error: HttpCheckError::ConnectionRefused,
                }),
            },
            "running (health: unhealthy); last check: connection refused",
        ),
        (
            HealthFailure::TimedOut {
                last_check: Some(LastHealthCheck::HttpUnreachable {
                    error: HttpCheckError::TimedOut,
                }),
            },
            "timed out; last check: timed out",
        ),
        (
            HealthFailure::TimedOut {
                last_check: Some(LastHealthCheck::HttpUnreachable {
                    error: HttpCheckError::Other,
                }),
            },
            "timed out; last check: request failed",
        ),
    ] {
        assert_eq!(failure.to_string(), expected);
    }
}

#[test]
fn a_last_check_keeps_the_first_line_of_output_cut_short() {
    assert_eq!(
        LastHealthCheck::exited(1, "\n  first line  \nPASSWORD=hunter2\n"),
        LastHealthCheck::Exited {
            code: 1,
            output: "first line".into(),
        }
    );
    let LastHealthCheck::Exited { output, .. } = LastHealthCheck::exited(1, &"é".repeat(500))
    else {
        unreachable!()
    };
    assert_eq!(output, format!("{}…", "é".repeat(120)));
    assert_eq!(
        LastHealthCheck::exited(1, &"é".repeat(120)),
        LastHealthCheck::Exited {
            code: 1,
            output: "é".repeat(120),
        }
    );
}

#[test]
fn health_failures_recorded_without_a_last_check_still_decode() {
    assert_eq!(
        serde_json::from_value::<HealthFailure>(json!({ "type": "timed_out" })).unwrap(),
        HealthFailure::TimedOut { last_check: None }
    );
    let runtime = json!({
        "type": "runtime",
        "observation": { "state": "running", "health": "unhealthy" },
    });
    let decoded: HealthFailure = serde_json::from_value(runtime.clone()).unwrap();
    assert_eq!(
        decoded,
        HealthFailure::Runtime {
            observation: ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            },
            last_check: None,
        }
    );
    assert_eq!(serde_json::to_value(&decoded).unwrap(), runtime);
}
