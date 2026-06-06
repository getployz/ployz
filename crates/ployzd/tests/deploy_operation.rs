#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use fixtures::*;
use ployz_core::ops::{
    DeployOperationFailure, DeployRunningStage, DeployTransition, FailureMessage,
};
use ployz_core::state::{ActiveServiceCommitRequest, ExpectedActiveService};
use ployzd::deploy_worker::{
    ActiveServiceCommitter, DeployCompletionRecord, DeployCompletionRecordFailure,
    DeployExecutionCommand, DeployExecutionError, DeployExecutionOutcome, DeployExecutionPorts,
    DeployExecutionStep, DeployHealthCheckError, DeployHealthChecker, DeployOperationRecorder,
    NodeContainerRuntime, NodeContainerRuntimeError, execute_deploy_operation,
};
use ployzd::operation_lease::{OperationLeasePolicy, with_advisory_operation_lease};
use std::time::Duration;

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("test failure message is non-empty")
}

async fn execute_deploy<R, N, H, A>(
    command: DeployExecutionCommand,
    ports: DeployExecutionPorts<'_, R, N, H, A>,
) -> Result<DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: NodeContainerRuntime,
    H: DeployHealthChecker,
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let outcome = execute_deploy(
        command.clone(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("deploy succeeds");

    assert_eq!(outcome.service_id, service_id("svc_api"));
    assert_eq!(outcome.target_revision, revision_id("rev_2"));
    assert_eq!(outcome.completion_record, DeployCompletionRecord::Recorded);
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
                stage: route_cutover_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Completed),
        ]
    );
    assert_eq!(runtime.requests.len(), 2);
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
    assert_eq!(first_request.labels.operation_id, operation_id("op_123"));
    assert_eq!(first_request.labels.step_id.as_str(), "run_1");
    assert_eq!(second_request.node_id, node_id("node_b"));
    assert_eq!(second_request.labels.step_id.as_str(), "run_2");
}

#[tokio::test]
async fn deploy_worker_reuses_running_target_containers_from_observed_reality() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_with_existing_container(2, "node_b", "ctr_existing");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
                stage: route_cutover_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Completed),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_does_not_claim_existing_container_as_retained_artifact() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::unhealthy("node_b", "ctr_existing");
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_with_existing_container(1, "node_b", "ctr_existing");

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
    let mut runtime = RecordingRuntime::reusing_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
    let mut runtime = RecordingRuntime::failing_after_first_container();
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
async fn deploy_worker_records_planning_before_plan_failure() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command_without_eligible_nodes(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
async fn deploy_worker_waits_for_health_before_completing() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::unhealthy("node_b", "ctr_2");
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
async fn deploy_worker_times_out_hanging_steps() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = HangingHealth;
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
async fn deploy_worker_does_not_repair_missing_completion_after_active_commit() {
    let mut recorder = RecordingOperations::fail_completed_transition_times(1);
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let _outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
            active_state: &mut active_state,
        },
    )
    .await
    .expect("deploy succeeds after active state commit");

    assert_eq!(
        _outcome.completion_record,
        DeployCompletionRecord::Missing {
            reason: DeployCompletionRecordFailure::RecordRejected,
        }
    );

    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 1 },
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
                stage: route_cutover_running(),
            }),
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = HangingActiveState;
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
                stage: route_cutover_running(),
            }),
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stale_mismatch();
    let command = deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            node_runtime: &mut runtime,
            health_checker: &mut health,
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
                stage: route_cutover_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::ActiveServiceCommitRejected {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                    reason: ployz_core::ops::ActiveServiceCommitFailure::RevisionMismatch {
                        expected_revision: revision_id("rev_old"),
                        current_revision: revision_id("rev_other"),
                    },
                    retained_artifacts: vec![retained_container("node_a", "ctr_1")],
                }
            }),
        ]
    );
    assert_eq!(active_state.requests.len(), 1);
}
