//! Direct progress aggregation, retained diagnostics, and recovery commands.
use super::*;
use ployz_core::{
    DeployPreview, MachineAction, MachineName, RequestedServiceSpec, ResolvedServiceSpec,
    ResolvedUpdateConfig, UpdateOrder,
};

fn spec() -> ResolvedServiceSpec {
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({"name":"web", "mode":{"mode":"replicated","replicas":1}, "container":{"image":"nginx", "pull_policy":"missing"}})).unwrap();
    requested
        .to_resolved(
            ployz_core::ServiceId::random(),
            ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: None,
            },
        )
        .unwrap()
}
fn operation(index: u32, machine: MachineId, status: OperationStatus) -> OperationRow {
    OperationRow {
        index,
        machine_id: machine,
        machine_name: Some(MachineName::parse("edge").unwrap()),
        service_name: Some(ServiceName::parse("web").unwrap()),
        display_name: None,
        operation: DeployOperation::RunContainer {
            machine_id: machine,
            spec: spec(),
            skip_health_monitor: false,
        },
        status,
    }
}
fn denied() -> ExecutionError {
    ExecutionError::Machine {
        action: MachineAction::CreateContainer,
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Image pull failed".into(),
            cause: vec![
                "registry request failed".into(),
                "denied by registry".into(),
            ],
            details: serde_json::Value::Null,
        },
    }
}

#[test]
fn operations_group_by_service_and_machine_without_merging_duplicate_names() {
    let one = MachineId::random();
    let two = MachineId::random();
    let operations = vec![
        operation(0, one, OperationStatus::Completed),
        operation(
            1,
            one,
            OperationStatus::Running {
                phase: OperationPhase::StartingContainer,
            },
        ),
        operation(2, two, OperationStatus::Completed),
    ];
    let preview = DeployPreview::new(
        operations.clone(),
        Vec::new(),
        Namespace::parse("shop").unwrap(),
    );
    let mut direct = Direct::new(&preview, "Deploying shop".into());
    let frame = direct.frame();
    assert_eq!(frame.rows.len(), 2);
    assert_eq!(
        frame
            .rows
            .iter()
            .filter(|row| row.state == State::Completed)
            .count(),
        1
    );
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.state == State::Running(ployz_store::RowPhase::StartingContainer))
    );
    let completed: Vec<_> = operations
        .into_iter()
        .map(|row| OperationRow {
            status: OperationStatus::Completed,
            ..row
        })
        .collect();
    direct.observe(&completed);
    assert!(
        direct
            .frame()
            .rows
            .iter()
            .all(|row| row.state == State::Completed)
    );
}

#[test]
fn contextual_failure_keeps_one_deepest_cause_tail_and_exact_inspect_command() {
    let machine = MachineId::random();
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation: operation(0, machine, OperationStatus::Pending).operation,
            error: denied(),
        },
        unexecuted: Vec::new(),
    };
    let tail = LogTail {
        service: QualifiedService::parse("ployz-system/ingress").unwrap(),
        machine,
        server: "edge".into(),
        lines: vec!["could not load configuration".into()],
    };
    let failure = failure(&outcome, vec![tail], "production")
        .context("Server initialized; ingress deployment incomplete.")
        .hint(Hint::Retry(
            "ployz server set edge --accepts-ingress=true".into(),
        ));
    let text = crate::ui::plain(&failure);
    assert!(text.contains("error: Server initialized; ingress deployment incomplete."));
    assert_eq!(text.matches("cause:").count(), 1, "{text}");
    assert!(text.contains("cause: denied by registry"));
    assert!(text.contains("could not load configuration"));
    assert!(text.contains(&format!(
        "inspect: ployz logs ployz-system/ingress --machine {machine} --context production"
    )));
    assert!(text.contains("retry: ployz server set edge --accepts-ingress=true"));
    let json = failure.json();
    assert!(
        json.get("cause")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "registry request failed")
    );
    assert_eq!(
        json.pointer("/details/progress/logs/0/lines/0").unwrap(),
        "could not load configuration"
    );
}

#[test]
fn missing_tail_retains_inspect_and_compensation_is_only_actual_evidence() {
    let machine = MachineId::random();
    let replacement = ployz_core::ReplacementOperation {
        machine_id: machine,
        old_container_id: ContainerId::parse("a".repeat(64)).unwrap(),
        spec: spec(),
        skip_health_monitor: false,
    };
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Replacement {
            operation: replacement,
            error: denied(),
            compensation: ReplacementCompensation::OldStopped {
                stop_new_container: Some(StopAttempt::Stopped),
                restart_old_container: RestartAttempt::Failed { error: denied() },
            },
        },
        unexecuted: Vec::new(),
    };
    let tail = LogTail {
        service: QualifiedService::parse("shop/web").unwrap(),
        machine,
        server: "edge".into(),
        lines: Vec::new(),
    };
    let text = crate::ui::plain(&failure(&outcome, vec![tail], "production"));
    assert!(text.contains("Stopped the new Container."));
    assert!(text.contains("Could not restart the old Container."));
    assert_eq!(
        text.matches("cause: denied by registry").count(),
        2,
        "{text}"
    );
    assert!(text.contains("inspect: ployz logs shop/web"));
    assert!(!text.contains("Last 0"));
    assert!(!text.contains("Restarted the old"));
}
