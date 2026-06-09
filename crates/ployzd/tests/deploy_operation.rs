#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use fixtures::*;
use ployz_core::deploy::DeployCleanupContainer;
use ployz_core::ops::{
    DeployOperationFailure, DeployRunningStage, DeployTransition, FailureMessage,
};
use ployz_core::state::{
    ActiveRouteCommitRequest, ActiveServiceCommitRequest, ExpectedActiveRoute,
    ExpectedActiveService,
};
use ployzd::deploy_worker::{
    ActiveRouteCommitter, ActiveServiceCommitter, DeployCleanupResult, DeployExecutionCommand,
    DeployExecutionError, DeployExecutionOutcome, DeployExecutionPorts, DeployExecutionStep,
    DeployHealthCheckError, DeployHealthChecker, DeployOperationRecorder, DeployTerminalEvent,
    NodeContainerRuntime, NodeContainerRuntimeError, WireGuardEbpfPreparer,
    execute_deploy_operation,
};
use ployzd::docker::labels::ManagedContainerIdentity;
use ployzd::node_agent::runtime::managed_container_labels;
use ployzd::operation_lease::{OperationLeasePolicy, with_advisory_operation_lease};
use std::time::Duration;

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("test failure message is non-empty")
}

fn assert_deploy_event_order(
    records: &[RecordedOperation],
    before: DeployRunningStage,
    after: DeployRunningStage,
) {
    let before_position = records
        .iter()
        .position(|record| {
            record == &RecordedOperation::Transition(DeployTransition::Running { stage: before })
        })
        .expect("before stage is recorded");
    let after_position = records
        .iter()
        .position(|record| {
            record == &RecordedOperation::Transition(DeployTransition::Running { stage: after })
        })
        .expect("after stage is recorded");

    assert!(
        before_position < after_position,
        "{before:?} should be recorded before {after:?}"
    );
}

fn cleanup_expected_identity(target: &DeployCleanupContainer) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        service_id: target.service_id.clone(),
        revision_id: target.revision_id.clone(),
        operation_id: target.operation_id.clone(),
        step_id: target.step_id.clone(),
        kind: target.kind,
    }
}

async fn execute_deploy<R, D, N, H, C, A>(
    command: DeployExecutionCommand,
    ports: DeployExecutionPorts<'_, R, D, N, H, C, A>,
) -> Result<DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
    D: WireGuardEbpfPreparer,
    N: NodeContainerRuntime,
    H: DeployHealthChecker,
    C: ActiveRouteCommitter,
    A: ActiveServiceCommitter,
{
    execute_deploy_operation(command, ports).await
}

#[tokio::test(start_paused = true)]
async fn advisory_lease_renews_without_controlling_work_result() {
    let lease_renewer = RecordingLeaseRenewer::lost();
    let policy = OperationLeasePolicy::try_new(
        ployz_core::ops::OperationLeaseDurationSeconds::try_new(60).expect("valid lease duration"),
        Duration::from_secs(5),
    )
    .expect("valid renewal interval");

    let outcome = with_advisory_operation_lease(
        operation_id("op_123"),
        policy,
        lease_renewer.clone(),
        async {
            tokio::time::sleep(Duration::from_secs(12)).await;
            "done"
        },
    )
    .await;

    assert_eq!(outcome, "done");
    assert_eq!(
        lease_renewer.renewals(),
        vec![operation_id("op_123"), operation_id("op_123")]
    );
}

#[tokio::test]
async fn deploy_worker_runs_containers_then_completes() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let outcome = execute_deploy(
        command.clone(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("deploy succeeds");

    assert_eq!(outcome.service_id, service_id("svc_api"));
    assert_eq!(outcome.target_revision, revision_id("rev_2"));
    assert_eq!(outcome.terminal_event, DeployTerminalEvent::Recorded);
    assert_eq!(
        outcome
            .containers
            .iter()
            .map(|container| container.container_id.clone())
            .collect::<Vec<_>>(),
        vec![container_id("ctr_1"), container_id("ctr_2")]
    );
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::WireGuardEbpfPrepared { node_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_b"),
                container_id: container_id("ctr_2"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Completed),
        ]
    );
    assert_eq!(runtime.requests.len(), 2);
    let [wireguard_ebpf_request] = wireguard_ebpf.requests.as_slice() else {
        panic!("expected exactly one wireguard/ebpf prepare request");
    };
    assert_eq!(
        wireguard_ebpf_request.nodes,
        vec![node_id("node_a"), node_id("node_b")]
    );
    assert_eq!(
        active_state.requests,
        vec![ActiveServiceCommitRequest {
            service_id: service_id("svc_api"),
            expected_current: ExpectedActiveService::Absent,
            target_revision: revision_id("rev_2"),
        }]
    );
    assert_eq!(
        health.checked,
        vec![vec![
            DeployContainerForAssert::new("node_a", "ctr_1"),
            DeployContainerForAssert::new("node_b", "ctr_2"),
        ]]
    );
    let [first_request, second_request] = runtime.requests.as_slice() else {
        panic!("expected exactly two runtime requests");
    };
    assert_eq!(first_request.node_id, node_id("node_a"));
    assert_eq!(first_request.container.operation_id, operation_id("op_123"));
    assert_eq!(first_request.container.step_id.as_str(), "run_1");
    assert_eq!(second_request.node_id, node_id("node_b"));
    assert_eq!(second_request.container.step_id.as_str(), "run_2");
}

#[tokio::test]
async fn deploy_worker_reuses_running_target_containers_from_observed_reality() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_with_existing_container(2, "node_b", "ctr_existing");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("deploy succeeds with an existing target container");

    assert_eq!(
        outcome
            .containers
            .iter()
            .map(|container| container.container_id.clone())
            .collect::<Vec<_>>(),
        vec![container_id("ctr_existing"), container_id("ctr_new")]
    );
    assert_eq!(runtime.requests.len(), 1);
    let [request] = runtime.requests.as_slice() else {
        panic!("expected one runtime request");
    };
    assert_eq!(request.node_id, node_id("node_a"));
    assert_eq!(
        health.checked,
        vec![vec![
            DeployContainerForAssert::new("node_b", "ctr_existing"),
            DeployContainerForAssert::new("node_a", "ctr_new"),
        ]]
    );
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::WireGuardEbpfPrepared { node_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_new"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Completed),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_removes_superseded_containers_after_active_commit() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_replacing_old_container(1, "node_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("deploy succeeds and cleans up old containers");

    assert_eq!(
        runtime.removals,
        vec![ployzd::deploy_worker::NodeRemoveContainerRequest {
            node_id: node_id("node_b"),
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: cleanup_expected_identity(&cleanup_container(
                "node_b", "ctr_old", "rev_old",
            )),
        }]
    );
    let cleanup_target = cleanup_container("node_b", "ctr_old", "rev_old");
    assert_eq!(
        outcome.cleanup,
        vec![DeployCleanupResult::Removed(cleanup_target.clone())]
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::ActiveServiceCommit,
        DeployRunningStage::RemovingSupersededContainers,
    );
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Completed))
    );
    assert!(
        recorder
            .records
            .contains(&RecordedOperation::CleanupFinished {
                removed: vec![cleanup_target],
                failed: Vec::new(),
            })
    );
}

#[tokio::test]
async fn deploy_worker_reports_cleanup_failure_without_failing_successful_deploy() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]).with_remove_failure();
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_replacing_old_container(1, "node_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("deploy succeeds even when old-container cleanup fails");

    assert_eq!(active_state.requests.len(), 1);
    let cleanup_target = cleanup_container("node_b", "ctr_old", "rev_old");
    assert_eq!(
        outcome.cleanup,
        vec![DeployCleanupResult::Failed {
            target: cleanup_target.clone(),
            message: failure_message("container remove failed: busy"),
        }]
    );
    assert!(
        recorder
            .records
            .contains(&RecordedOperation::CleanupFinished {
                removed: Vec::new(),
                failed: vec![ployz_core::ops::DeployCleanupFailure {
                    target: cleanup_target,
                    message: failure_message("container remove failed: busy"),
                }],
            })
    );
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Completed))
    );
}

#[tokio::test]
async fn deploy_worker_does_not_claim_existing_container_as_retained_artifact() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::unhealthy("node_b", "ctr_existing");
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_with_existing_container(1, "node_b", "ctr_existing");

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("existing unhealthy target container fails deploy");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            failure: DeployOperationFailure::HealthCheckFailed {
                health_check:
                    ployz_core::ops::HealthCheckFailure::ProbeFailed {
                        node_id: failed_node_id,
                        container_id: failed_container_id,
                        ..
                    },
                retained_artifacts,
            },
            ..
        } if failed_node_id == node_id("node_b")
            && failed_container_id == container_id("ctr_existing")
            && retained_artifacts.is_empty()
    ));
    assert!(runtime.requests.is_empty());
    assert!(active_state.requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_treats_reused_operation_step_container_as_progress() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::reusing_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("reused operation-step container is idempotent progress");

    assert_eq!(
        outcome
            .containers
            .iter()
            .map(|container| container.container_id.clone())
            .collect::<Vec<_>>(),
        vec![container_id("ctr_1")]
    );
    assert!(
        recorder
            .records
            .contains(&RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            })
    );
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Completed))
    );
    assert_eq!(runtime.requests.len(), 1);
}

#[tokio::test]
async fn deploy_worker_records_failure_when_container_run_fails() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::failing_after_first_container();
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("deploy fails");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::RunContainer(NodeContainerRuntimeError::Unavailable { .. }))
    ));
    assert_eq!(runtime.requests.len(), 2);
    assert!(active_state.requests.is_empty());
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::WireGuardEbpfPrepared { node_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::RuntimeUnavailable {
                    node_id: node_id("node_b"),
                    message: ployz_core::ops::FailureMessage::try_new(
                        "node runtime request failed: synthetic runtime failure",
                    )
                    .expect("valid failure message"),
                    retained_artifacts: vec![retained_container("node_a", "ctr_1")],
                }
            }),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_retains_created_container_when_start_fails() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::failing_start("ctr_created");
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("deploy fails");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            failure:
                DeployOperationFailure::RuntimeUnavailable {
                    node_id: failure_node_id,
                    message,
                    retained_artifacts,
                },
            ..
        } if matches!(*source, DeployExecutionError::RunContainer(NodeContainerRuntimeError::CreatedContainerStartFailed { .. }))
            && failure_node_id == node_id("node_a")
            && message == failure_message("container start failed: exec format error")
            && retained_artifacts == vec![retained_created_container("node_a", "ctr_created")]
    ));
    assert!(active_state.requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_records_planning_before_plan_failure() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_without_eligible_nodes(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("deploy fails while planning");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::Plan(_))
    ));
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::PlanningFailed {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                    message: ployz_core::ops::FailureMessage::try_new("deploy planning failed")
                        .expect("valid failure message"),
                }
            }),
        ]
    );
    assert!(runtime.requests.is_empty());
    assert!(health.checked.is_empty());
    assert!(active_state.requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_fails_before_container_run_when_wireguard_ebpf_is_unavailable() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::wireguard_failed("node_b");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("deploy fails before container mutation");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::PrepareWireGuardEbpf(_))
    ));
    assert!(runtime.requests.is_empty());
    assert!(health.checked.is_empty());
    assert!(active_state.requests.is_empty());
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::WireGuardEbpfUnavailable {
                    node_id: node_id("node_b"),
                    component: ployz_core::dataplane::WireGuardEbpfComponent::WireGuard,
                    message: failure_message("wireguard interface failed"),
                    retained_artifacts: Vec::new(),
                }
            }),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_waits_for_health_before_completing() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::unhealthy("node_b", "ctr_2");
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("deploy fails");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::WaitHealthy(DeployHealthCheckError::Unhealthy { .. }))
    ));
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::WireGuardEbpfPrepared { node_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_b"),
                container_id: container_id("ctr_2"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::HealthCheckFailed {
                    health_check: ployz_core::ops::HealthCheckFailure::ProbeFailed {
                        node_id: node_id("node_b"),
                        container_id: container_id("ctr_2"),
                        message: ployz_core::ops::FailureMessage::try_new("probe failed")
                            .expect("valid failure message"),
                        log_hint: ployz_core::ops::OperatorHint::try_new("ployz logs ctr_2")
                            .expect("valid log hint"),
                    },
                    retained_artifacts: vec![
                        retained_container("node_a", "ctr_1"),
                        retained_container("node_b", "ctr_2"),
                    ],
                }
            }),
        ]
    );
    assert!(active_state.requests.is_empty());
}

#[tokio::test]
async fn routed_deploy_commits_route_before_completion() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = routed_deploy_command(1);

    execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("routed deploy succeeds");

    assert_eq!(
        route_state.requests,
        vec![ActiveRouteCommitRequest {
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            expected_current: ExpectedActiveRoute::Absent,
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_2"),
        }]
    );
    assert_eq!(active_state.requests.len(), 1);
    let [runtime_request] = runtime.requests.as_slice() else {
        panic!("expected one runtime request");
    };
    assert_eq!(
        runtime_request
            .endpoint
            .as_ref()
            .map(|endpoint| endpoint.port),
        Some(route_port(8080))
    );
    assert_eq!(
        managed_container_labels(
            &runtime_request.container,
            runtime_request.endpoint.as_ref()
        )
        .endpoint_port,
        Some(route_port(8080))
    );
    assert_eq!(
        health.checked,
        vec![vec![DeployContainerForAssert::routed("node_a", "ctr_1")]]
    );
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Completed))
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::RouteCutover,
        DeployRunningStage::ActiveServiceCommit,
    );
}

#[tokio::test]
async fn routed_deploy_fails_before_completion_when_route_is_stale() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stale_mismatch();
    let mut active_state = RecordingActiveState::stored();
    let command = routed_deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("stale route cutover fails deploy");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            failure:
                DeployOperationFailure::RouteCutoverFailed {
                    route,
                    reason: ployz_core::ops::RouteCutoverFailureReason::RouteRejected { .. },
                    retained_artifacts,
                },
            ..
        } if matches!(*source, DeployExecutionError::ActiveRouteCommitRejected { .. })
            && route == route_target("api.example.com", 443)
            && retained_artifacts == vec![retained_container("node_a", "ctr_1")]
    ));
    assert_eq!(route_state.requests.len(), 1);
    assert!(active_state.requests.is_empty());
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Failed {
            failure: DeployOperationFailure::RouteCutoverFailed {
                route: route_target("api.example.com", 443),
                reason: ployz_core::ops::RouteCutoverFailureReason::RouteRejected {
                    message: failure_message("route changed before cutover"),
                },
                retained_artifacts: vec![retained_container("node_a", "ctr_1")],
            }
        }))
    );
    assert!(
        !recorder
            .records
            .contains(&RecordedOperation::Transition(DeployTransition::Completed))
    );
}

#[tokio::test]
async fn deploy_worker_times_out_hanging_steps() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = HangingHealth;
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("health wait times out");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::StepTimedOut { .. })
    ));
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Failed {
            failure: DeployOperationFailure::HealthCheckFailed {
                health_check: ployz_core::ops::HealthCheckFailure::TimedOut { timeout_seconds: 1 },
                retained_artifacts: vec![retained_container("node_a", "ctr_1")],
            }
        }))
    );
    assert!(active_state.requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_keeps_success_when_completed_event_fails_after_active_commit() {
    let mut recorder = RecordingOperations::fail_completed_transition_times(1);
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("active commit succeeds even when the completed event is rejected");

    assert_eq!(outcome.service_id, service_id("svc_api"));
    assert_eq!(outcome.target_revision, revision_id("rev_2"));
    assert_eq!(outcome.terminal_event, DeployTerminalEvent::Missing);

    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::WireGuardEbpfPrepared { node_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
        ]
    );
    assert_eq!(active_state.requests.len(), 1);
    assert_eq!(recorder.completed_transition_attempts, 1);
}

#[tokio::test]
async fn deploy_worker_marks_failed_when_active_commit_times_out() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = HangingActiveState;
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("active commit timeout fails the operation");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            failure: DeployOperationFailure::ControlPlaneCommitFailed { .. },
            ..
        } if matches!(
            *source,
            DeployExecutionError::StepTimedOut {
                step: DeployExecutionStep::CommitActiveService,
                ..
            }
        )
    ));
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::WireGuardEbpfPrepared { node_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::ControlPlaneCommitFailed {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                    message: failure_message("active service commit timed out after 1ms"),
                    retained_artifacts: vec![retained_container("node_a", "ctr_1")],
                }
            }),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_marks_failed_when_active_commit_is_stale() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut route_state = RecordingRouteState::stored();
    let mut active_state = RecordingActiveState::stale_mismatch();
    let command = deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            wireguard_ebpf: &mut wireguard_ebpf,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            route_state: &mut route_state,
            active_state: &mut active_state,
        },
    )
    .await
    .expect_err("stale active commit fails");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::ActiveServiceCommitRejected { .. })
    ));
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            }),
            RecordedOperation::WireGuardEbpfPrepared { node_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::ActiveServiceCommitRejected {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                    reason: ployz_core::ops::ActiveServiceCommitFailure::ActiveServiceChanged {
                        expected_current: ployz_core::state::ExpectedActiveService::Revision(
                            revision_id("rev_old"),
                        ),
                        current_revision: Some(revision_id("rev_other")),
                        attempted_revision: revision_id("rev_2"),
                    },
                    retained_artifacts: vec![retained_container("node_a", "ctr_1")],
                }
            }),
        ]
    );
    assert_eq!(active_state.requests.len(), 1);
}
