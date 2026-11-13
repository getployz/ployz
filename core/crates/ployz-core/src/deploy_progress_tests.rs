use crate::{
    ContainerId, ContainerRuntimeObservation, ExecutionError, HealthFailure, HealthObservation,
    HookFailure, LastHealthCheck, LastRun, MachineAction, OperationPhase, OperationRow,
    OperationStatus, RpcError, RpcErrorCode,
};

use super::{Failure, RowPhase, RowState};

fn server(statuses: &[OperationStatus]) -> RowState {
    let statuses: Vec<_> = statuses.iter().map(|status| (status, true)).collect();
    super::state(&[], any_machine(), &statuses, None, true, false)
}

#[test]
fn a_server_waits_for_the_switch_only_once_each_new_container_is_ready() {
    let switch = OperationStatus::Running {
        phase: OperationPhase::WaitingForSwitch {
            container_id: ContainerId::parse("a".repeat(64)).unwrap(),
        },
    };
    let creating = OperationStatus::Running {
        phase: OperationPhase::CreatingContainer,
    };
    let running = |phase| RowState::Running {
        phase,
        serving: false,
    };
    assert_eq!(
        server(&[switch.clone(), creating]),
        running(RowPhase::CreatingContainer)
    );
    assert_eq!(
        server(&[switch.clone(), OperationStatus::Pending]),
        running(RowPhase::Starting)
    );
    assert_eq!(
        server(&[switch.clone(), switch.clone()]),
        running(RowPhase::WaitingForSwitch)
    );
    assert_eq!(
        super::state(
            &[],
            any_machine(),
            &[(&switch, true), (&OperationStatus::Pending, false)],
            None,
            true,
            false
        ),
        running(RowPhase::WaitingForSwitch),
        "the old copy's removal waits for the switch, not the Server"
    );
}

/// A later failure that aborts the run leaves the old copy's removal unexecuted
/// after the new copy switched, which still reads as switched (#3684).
#[test]
fn a_server_whose_new_copy_switched_reads_completed_when_only_its_removal_did_not_run() {
    let read = |start: &OperationStatus| {
        super::state(
            &[],
            any_machine(),
            &[(start, true), (&OperationStatus::Unexecuted, false)],
            None,
            true,
            false,
        )
    };
    assert_eq!(read(&OperationStatus::Completed), RowState::Completed);
    assert_eq!(
        read(&OperationStatus::Unexecuted),
        RowState::NotAttempted,
        "a copy whose switch never landed did not start"
    );
}

#[test]
fn only_cancelled_work_is_stopped_and_a_real_failure_names_the_row() {
    let container_id = ContainerId::parse("a".repeat(64)).unwrap();
    let failed = |error| OperationStatus::Failed { error };
    let create = || {
        failed(ExecutionError::Machine {
            action: MachineAction::CreateContainer,
            error: RpcError::internal("no space left on device"),
        })
    };
    let hook = |stop_error| {
        failed(ExecutionError::Hook {
            container_id,
            failure: HookFailure::Cancelled { stop_error },
        })
    };
    let tracked = |statuses: &[OperationStatus]| {
        let machine_id = "a".repeat(32);
        let rows: Vec<OperationRow> = serde_json::from_value(serde_json::Value::Array(
            statuses
                .iter()
                .enumerate()
                .map(|(index, status)| serde_json::json!({
                    "index": index, "machine_id": machine_id, "service_name": "web",
                    "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
                    "status": status
                }))
                .collect(),
        ))
        .unwrap();
        let row = super::RowTracker::default().changes(&rows).remove(0);
        let RowState::Failed { reason, .. } = row.state else {
            panic!("expected a failed row, got {:?}", row.state);
        };
        (row.stopped, reason)
    };
    let cancelled = || failed(ExecutionError::Cancelled);
    assert_eq!(tracked(&[cancelled()]), (true, "Cancelled".to_owned()));
    assert!(tracked(&[hook(None)]).0);
    assert!(tracked(&[cancelled(), OperationStatus::Completed]).0);
    assert!(!tracked(&[create()]).0);
    let (stopped, reason) = tracked(&[cancelled(), create()]);
    assert!(!stopped, "a real failure beside a cancel's is not stopped");
    assert_ne!(reason, "Cancelled", "the row names its real failure");
    assert!(
        !tracked(&[hook(Some(RpcError::unavailable("stop timed out")))]).0,
        "a hook the cancel could not stop may still run"
    );
}

fn any_machine() -> crate::MachineId {
    crate::MachineId::parse("1".repeat(32)).unwrap()
}

/// A failure marked with the Leave that overlapped it names the removed Server
/// and nothing of what the copy did, whether the Server answered an error or an
/// unhealthy copy (#2924).
#[test]
fn a_failure_a_leave_overlapped_names_the_removed_server() {
    let server = crate::MachineName::parse("web-2").unwrap();
    let failed = Failure::of(
        &ExecutionError::Machine {
            action: MachineAction::CreateContainer,
            error: RpcError::internal("detail temporarily unavailable").overlapping_leave(&server),
        },
        &[],
    );
    assert_eq!(
        failed.reason,
        "Server web-2 was removed while the deploy ran"
    );
    assert!(failed.cause.is_empty(), "{failed:?}");

    let exited = Failure::of(
        &ExecutionError::Health {
            container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            failure: HealthFailure::Runtime {
                observation: ContainerRuntimeObservation::Exited { code: 0 },
                last_check: None,
            },
            out_of_memory: None,
            last_run: None,
            departing: Some(crate::Departure { server }),
        },
        &[],
    );
    assert_eq!(
        exited.reason,
        "Server web-2 was removed while the deploy ran"
    );
    assert!(exited.cause.is_empty(), "{exited:?}");
}

fn reason(observation: ContainerRuntimeObservation) -> String {
    failure(observation).reason
}

fn failure(observation: ContainerRuntimeObservation) -> Failure {
    Failure::of(
        &ExecutionError::Health {
            container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            failure: HealthFailure::Runtime {
                observation,
                last_check: None,
            },
            out_of_memory: None,
            last_run: None,
            departing: None,
        },
        &[],
    )
}

#[test]
fn a_server_without_a_capability_is_named_and_told_to_upgrade() {
    let machine_id = "a".repeat(32);
    let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
        "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
        "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
        "status": {"type": "pending"}
    }]))
    .unwrap();
    let error = ExecutionError::Machine {
        action: MachineAction::PullImage,
        error: RpcError::unsupported("unimplemented")
            .with_details(
                serde_json::json!({"capability": "ployz.serving.routes.v1", "target": machine_id}),
            )
            .with_causes(vec!["unimplemented".into()]),
    };
    let named = Failure::of(&error, &operations);
    assert_eq!(
        named.reason,
        "Server prod-1 needs a newer Ployz. It cannot serve ployz.serving.routes.v1."
    );
    assert!(named.cause.is_empty());
    assert_eq!(
        Failure::of(&error, &[]).reason,
        format!(
            "Server {machine_id} needs a newer Ployz. It cannot serve ployz.serving.routes.v1."
        )
    );
}

/// A Server removed while the deploy ran is gone from the entry Machine's routing
/// by its turn, which refuses with the ID it looked up. The row the call went to
/// names the Server (#3151).
#[test]
fn a_server_removed_while_the_deploy_ran_is_named_from_its_row() {
    let machine_id = "a".repeat(32);
    let machine = crate::MachineId::parse(&machine_id).unwrap();
    let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
        "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
        "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
        "status": {"type": "pending"}
    }]))
    .unwrap();
    let not_found = |details| ExecutionError::Machine {
        action: MachineAction::CreateContainer,
        error: RpcError::not_found(format!("No Server named {machine_id:?}")).with_details(details),
    };
    let removed = not_found(serde_json::json!({
        "reason": "no_such_server",
        "targets": [machine_id],
    }));
    let named = Failure::on(&removed, &operations, Some(machine));
    assert_eq!(
        named.reason,
        "Server prod-1 was removed while the deploy ran"
    );
    assert!(named.cause.is_empty());
    assert!(!named.same_again);
    // The row a reader tracks names it too, as Cloud's per-Server row shows it.
    let failed: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
        "index": 0, "machine_id": machine_id, "machine_name": "prod-1", "service_name": "web",
        "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
        "status": OperationStatus::Failed { error: removed.clone() },
    }]))
    .unwrap();
    let tracked = super::RowTracker::default().changes(&failed);
    let Some(RowState::Failed { reason, .. }) = tracked.first().map(|row| &row.state) else {
        panic!("expected a failed row, got {tracked:?}");
    };
    assert_eq!(reason, "Server prod-1 was removed while the deploy ran");
    assert_eq!(
        Failure::on(&removed, &[], Some(machine)).reason,
        format!("Server {machine_id} was removed while the deploy ran")
    );
    // A reader that doesn't know which Machine the call went to keeps the answer.
    assert_eq!(
        Failure::of(&removed, &operations).reason,
        "create Container failed"
    );
    // A daemon from before the tag, or a Server's own not-found, reads as before.
    let untagged = not_found(serde_json::json!({}));
    let kept = Failure::on(&untagged, &operations, Some(machine));
    assert_eq!(kept.reason, "create Container failed");
    assert_eq!(kept.cause, [format!("No Server named {machine_id:?}")]);
}

/// A Server that left a call unanswered is named from its row, not by the id the
/// client's deadline put in the message (#3277).
#[test]
fn a_server_that_did_not_answer_is_named_from_its_row() {
    let machine_id = "a".repeat(32);
    let machine = crate::MachineId::parse(&machine_id).unwrap();
    let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
        "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
        "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
        "status": {"type": "pending"}
    }]))
    .unwrap();
    let unanswered = |said: String| ExecutionError::Machine {
        action: MachineAction::CreateContainer,
        error: RpcError::unanswered_target(RpcErrorCode::Unavailable, said),
    };
    let by_id = unanswered(format!(
        "Machine {machine_id} did not answer: target Machine RPC timed out"
    ));
    let named = Failure::on(&by_id, &operations, Some(machine));
    assert_eq!(
        named.reason,
        "Server prod-1 did not answer during create Container"
    );
    assert_eq!(named.cause, ["target Machine RPC timed out"]);
    // The deploy names a Machine it already knows silent by name.
    let by_name = unanswered("Machine prod-1 did not answer: deadline has elapsed".into());
    let named = Failure::on(&by_name, &operations, Some(machine));
    assert_eq!(
        named.reason,
        "Server prod-1 did not answer during create Container"
    );
    assert_eq!(named.cause, ["deadline has elapsed"]);
    // Another Machine's silence, met listing across the Cluster, is not this row's.
    let other = unanswered("Machine prod-2 did not answer: deadline has elapsed".into());
    let kept = Failure::on(&other, &operations, Some(machine));
    assert_eq!(kept.reason, "create Container failed");
    assert_eq!(
        kept.cause,
        ["Machine prod-2 did not answer: deadline has elapsed"]
    );
    let mut tagged = RpcError::unanswered_target(
        RpcErrorCode::Unavailable,
        format!("Machine {machine_id} did not answer: deadline has elapsed"),
    );
    if let Some(details) = tagged.details.as_object_mut() {
        details.insert("machine_id".into(), serde_json::json!("b".repeat(32)));
    }
    let elsewhere = ExecutionError::Machine {
        action: MachineAction::CreateContainer,
        error: tagged,
    };
    assert_eq!(
        Failure::on(&elsewhere, &operations, Some(machine)).reason,
        "create Container failed"
    );
    // A limits update keeps saying its live limits are unknown.
    let limits = ExecutionError::Machine {
        action: MachineAction::UpdateContainerLimits,
        error: RpcError::unanswered_target(
            RpcErrorCode::Unavailable,
            format!("Machine {machine_id} did not answer: deadline has elapsed"),
        ),
    };
    assert!(
        Failure::on(&limits, &operations, Some(machine))
            .reason
            .ends_with("could not confirm the new limits")
    );
    // A reader that doesn't know which Machine the call went to keeps the client's words.
    let kept = Failure::of(&by_id, &operations);
    assert_eq!(kept.reason, "create Container failed");
    assert_eq!(
        kept.cause,
        [format!(
            "Machine {machine_id} did not answer: target Machine RPC timed out"
        )]
    );
}

#[test]
fn a_container_updated_in_place_serves_throughout() {
    let machine_id = "a".repeat(32);
    let tracked = |status: serde_json::Value| {
        let rows: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
            "index": 0, "machine_id": machine_id, "service_name": "web",
            "operation": {
                "type": "update_container", "machine_id": machine_id, "container_id": "c".repeat(64),
                "spec": {
                    "service_id": "b".repeat(32), "name": "web",
                    "mode": {"mode": "replicated", "replicas": 1},
                    "container": {"image": "web:1", "pull_policy": "missing", "resources": {"memory_bytes": 64}}
                }
            },
            "status": status
        }]))
        .unwrap();
        super::RowTracker::default().changes(&rows).remove(0).state
    };
    assert_eq!(
        tracked(serde_json::json!({"type": "running", "phase": {"type": "updating_container"}})),
        RowState::Running {
            phase: RowPhase::UpdatingContainer,
            serving: true,
        }
    );
    assert_eq!(
        tracked(serde_json::json!({"type": "completed"})),
        RowState::Completed
    );
}

#[test]
fn a_failed_limits_update_names_the_server_and_keeps_dockers_words() {
    let machine_id = "a".repeat(32);
    let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
        "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
        "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
        "status": {"type": "pending"}
    }]))
    .unwrap();
    let docker = "Docker responded with status code 400: Conflicting options: Nano CPUs and CPU Period cannot both be set";
    let error = ExecutionError::Machine {
        action: MachineAction::UpdateContainerLimits,
        error: RpcError::internal("Docker operation failed")
            .with_details(serde_json::json!({"machine_id": machine_id}))
            .with_causes(vec![docker.into()]),
    };
    let named = Failure::of(&error, &operations);
    assert_eq!(
        named.reason,
        "Server prod-1 could not confirm the new limits"
    );
    assert_eq!(named.cause, ["Docker operation failed", docker]);
    assert_eq!(
        Failure::of(&error, &[]).reason,
        format!("Server {machine_id} could not confirm the new limits")
    );
}

#[test]
fn a_step_whose_answer_was_lost_says_so_not_that_the_step_failed() {
    let machine_id = "a".repeat(32);
    let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
        "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
        "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
        "status": {"type": "pending"}
    }]))
    .unwrap();
    let lost = |action| ExecutionError::Machine {
        action,
        error: RpcError::unavailable("connection lost")
            .reply_lost_from(&machine_id.parse().unwrap()),
    };
    let pull = Failure::of(&lost(MachineAction::PullImage), &operations);
    assert_eq!(
        pull.reason,
        "The connection to Server prod-1 dropped during pull image, so it may have happened anyway"
    );
    assert_eq!(pull.cause, ["connection lost"]);
    let create = lost(MachineAction::CreateContainer);
    assert_eq!(
        Failure::of(&create, &[]).reason,
        format!(
            "The connection to Server {machine_id} dropped during create Container, so it may have happened anyway"
        )
    );
    let failed = OperationStatus::Failed { error: create };
    assert!(
        matches!(
            server(&[failed]),
            RowState::Failed {
                unanswered: true,
                ..
            }
        ),
        "a lost answer may still have done the work"
    );
}

#[test]
fn a_refused_limits_update_says_which_limits_hold() {
    let machine_id = "a".repeat(32);
    let refused = |reason: &str, message: &str| ExecutionError::Machine {
        action: MachineAction::UpdateContainerLimits,
        error: RpcError::conflict(String::from(message))
            .with_details(serde_json::json!({"machine_id": machine_id, "reason": reason})),
    };
    let message = "container c cannot take these limits in place: the Container runs with a higher memory limit";
    let before_any_change = Failure::of(&refused("would_lower_memory", message), &[]);
    assert_eq!(
        before_any_change.reason,
        format!(
            "Server {machine_id} did not apply the new limits. The Container keeps its previous limits"
        )
    );
    assert_eq!(before_any_change.cause, [message]);
    assert_eq!(
        Failure::of(&refused("not_running", "stopped"), &[]).reason,
        format!(
            "Server {machine_id} found the Container not running, so the new limits do not apply"
        )
    );
    assert_eq!(
        Failure::of(&refused("some_future_reason", "later"), &[]).reason,
        format!("Server {machine_id} could not confirm the new limits")
    );
}

#[test]
fn a_failed_hook_names_both_commands_it_can_run() {
    let failed = Failure::of(
        &ExecutionError::Hook {
            container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            failure: HookFailure::Exit { code: 3 },
        },
        &[],
    );
    assert_eq!(failed.reason, "The setup or pre-deploy command failed");
    assert_eq!(failed.cause, ["exited with code 3"]);
}

#[test]
fn a_container_that_stops_running_is_not_called_unhealthy() {
    assert_eq!(
        reason(ContainerRuntimeObservation::Restarting),
        "The Container is restarting"
    );
    assert_eq!(
        reason(ContainerRuntimeObservation::Exited { code: 1 }),
        "The Container exited with code 1"
    );
    assert_eq!(
        reason(ContainerRuntimeObservation::Dead),
        "The Container stopped"
    );
}

fn checked(observation: ContainerRuntimeObservation, last_line: &str) -> Failure {
    Failure::of(
        &ExecutionError::Health {
            container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            failure: HealthFailure::Runtime {
                observation,
                last_check: Some(LastHealthCheck::exited(127, "sh: pg_isready: not found")),
            },
            out_of_memory: None,
            last_run: Some(LastRun {
                exit_code: None,
                last_line: Some(last_line.to_owned()),
            }),
            departing: None,
        },
        &[],
    )
}

#[test]
fn a_running_container_s_failed_check_is_the_deepest_cause() {
    // What the app printed while Ployz stopped it is not why its check failed (#2841).
    let failed = checked(
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        },
        "[notice] 1#1: signal 29 (SIGIO) received",
    );
    assert_eq!(failed.reason, "The Container failed its health check");
    assert_eq!(
        failed.cause,
        ["running (health: unhealthy); last check exited 127: sh: pg_isready: not found"]
    );
}

#[test]
fn a_container_that_stopped_ends_with_its_last_line() {
    let failed = checked(
        ContainerRuntimeObservation::Restarting,
        "panic: no DATABASE_URL",
    );
    assert_eq!(
        failed.cause.last().map(String::as_str),
        Some("panic: no DATABASE_URL")
    );
}

#[test]
fn a_failure_row_from_before_same_again_reads_back_as_one_a_retry_may_get_past() {
    let old: Failure =
        serde_json::from_str(r#"{"reason":"capacity unknown","cause":["fsn-1"]}"#).unwrap();
    assert!(!old.same_again);
    assert_eq!(
        serde_json::to_value(&old).unwrap(),
        serde_json::json!({"reason": "capacity unknown", "cause": ["fsn-1"]})
    );
    let refused = Failure {
        same_again: true,
        ..Failure::from("hostname taken".to_owned())
    };
    let json = serde_json::to_value(&refused).unwrap();
    assert_eq!(json.get("same_again"), Some(&serde_json::Value::Bool(true)));
    assert!(serde_json::from_value::<Failure>(json).unwrap().same_again);
}

#[test]
fn a_cause_that_repeats_the_reason_is_left_out() {
    assert!(
        failure(ContainerRuntimeObservation::Restarting)
            .cause
            .is_empty()
    );
    assert!(
        failure(ContainerRuntimeObservation::Exited { code: 1 })
            .cause
            .is_empty()
    );
}

fn pull_failed(cause: &str) -> Failure {
    let error = RpcError::internal("api: pull image failed.")
        .with_causes(vec![cause.to_owned()])
        .pulling("ghcr.io/example/private:1", crate::PullPolicy::Always);
    Failure::of(
        &ExecutionError::Machine {
            action: MachineAction::PullImage,
            error,
        },
        &[],
    )
}

#[test]
fn a_registry_that_denies_the_pull_names_the_image_as_missing_or_private() {
    for cause in [
        "Docker responded with status code 500: error from registry: denied; denied",
        "Docker responded with status code 500: Head \"https://registry.example.com/v2/app/manifests/1\": unauthorized: authentication required",
    ] {
        let failure = pull_failed(cause);
        assert_eq!(
            failure.reason,
            "Image ghcr.io/example/private:1 not found, or its registry needs credentials",
            "{cause}"
        );
        assert_eq!(failure.cause, [cause]);
    }
}

#[test]
fn a_registry_host_that_does_not_resolve_is_named() {
    let cause = "Docker responded with status code 500: Get \"https://registry.example.invalid/v2/\": dial tcp: lookup registry.example.invalid on 127.0.0.53:53: no such host";
    let failure = pull_failed(cause);
    assert_eq!(
        failure.reason,
        "Host registry.example.invalid does not resolve, so the image could not be pulled. Check the image's registry, or this Server's DNS"
    );
    assert_eq!(failure.cause, [cause]);
}

#[test]
fn a_lost_reply_that_names_an_unresolved_host_still_says_it_may_have_happened() {
    let error = RpcError::unavailable("transport error")
        .with_details(serde_json::json!({"reply_lost": true}))
        .with_causes(vec![
            "dial tcp: lookup peer.example.invalid on 127.0.0.53:53: no such host".into(),
        ]);
    let failure = Failure::of(
        &ExecutionError::Machine {
            action: MachineAction::PullImage,
            error,
        },
        &[],
    );
    assert!(
        failure.reason.starts_with("The connection to"),
        "{}",
        failure.reason
    );
}

#[test]
fn a_local_permission_error_is_not_a_registry_refusal() {
    let cause =
        "Docker responded with status code 500: open /var/lib/docker/tmp/x: permission denied";
    assert_eq!(pull_failed(cause).reason, "pull image failed");
}
