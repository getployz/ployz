#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use fixtures::*;
use ployz_core::ops::{
    DeployOperationFailure, DeployRunningStage, DeployTransition, FailureMessage,
};
use ployz_core::state::{ActiveServiceCommitRequest, ExpectedActiveService};
use ployzd::deploy_worker::{
    CompletionRecordAttemptError, DeployExecutionError, DeployExecutionPorts, DeployExecutionStep,
    DeployHealthCheckError, DeployWorker, NodeContainerRuntimeError,
};
use std::time::Duration;

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("test failure message is non-empty")
}

#[tokio::test]
async fn deploy_worker_runs_containers_then_completes() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(2);

    let outcome = DeployWorker
        .execute(
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
            StartedDeployContainerForAssert::new("node_a", "ctr_1"),
            StartedDeployContainerForAssert::new("node_b", "ctr_2"),
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
async fn deploy_worker_treats_reused_operation_step_container_as_progress() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::reusing_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let outcome = DeployWorker
        .execute(
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

    let error = DeployWorker
        .execute(
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
                        "node runtime unavailable while starting container",
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
    let mut command = deploy_command(1);
    command.eligible_nodes.clear();

    let error = DeployWorker
        .execute(
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

    let error = DeployWorker
        .execute(
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
                        message: ployz_core::ops::FailureMessage::try_new("probe failed")
                            .expect("valid failure message"),
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
    let mut command = deploy_command(1);
    command.step_timeout = Duration::from_millis(1);

    let error = DeployWorker
        .execute(
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
async fn deploy_worker_retries_completed_record_after_active_commit() {
    let mut recorder = RecordingOperations::fail_completed_transition_times(2);
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let _outcome = DeployWorker
        .execute(
            command,
            DeployExecutionPorts {
                recorder: &mut recorder,
                node_runtime: &mut runtime,
                health_checker: &mut health,
                active_state: &mut active_state,
            },
        )
        .await
        .expect("completion status is recorded");

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
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Completed),
        ]
    );
    assert_eq!(active_state.requests.len(), 1);
    assert_eq!(recorder.completed_transition_attempts, 2);
}

#[tokio::test]
async fn deploy_worker_records_terminal_failure_when_completion_cannot_be_recorded() {
    let mut recorder = RecordingOperations::fail_completed_transition_times(3);
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = RecordingActiveState::stored();
    let command = deploy_command(1);

    let error = DeployWorker
        .execute(
            command,
            DeployExecutionPorts {
                recorder: &mut recorder,
                node_runtime: &mut runtime,
                health_checker: &mut health,
                active_state: &mut active_state,
            },
        )
        .await
        .expect_err("completion record exhaustion marks the operation failed");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            failure: DeployOperationFailure::CompletionRecordFailedAfterActiveCommit { .. },
            source,
            ..
        } if matches!(
            *source,
            DeployExecutionError::CompletionRecordPending {
                attempts: 3,
                last_error: CompletionRecordAttemptError::Record(_),
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
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::CompletionRecordFailedAfterActiveCommit {
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                    message: failure_message("completion record could not be recorded"),
                    retained_artifacts: vec![retained_container("node_a", "ctr_1")],
                }
            }),
        ]
    );
    assert_eq!(active_state.requests.len(), 1);
    assert_eq!(recorder.completed_transition_attempts, 3);
}

#[tokio::test]
async fn deploy_worker_marks_failed_when_active_commit_times_out() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut active_state = HangingActiveState;
    let mut command = deploy_command(1);
    command.step_timeout = Duration::from_millis(1);

    let error = DeployWorker
        .execute(
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

    let error = DeployWorker
        .execute(
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
