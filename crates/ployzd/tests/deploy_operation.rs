#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use fixtures::*;
use ployz_core::dataplane::DataplaneMember;
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationFailure, DeployRunningStage, DeployTransition,
    RouteHostname, RouteTarget,
};
use ployz_core::state::{RouteBindingState, ServingTargetEntry, VolumePinState};
use ployz_test_support::ids::{failure_message, namespace_id};
use ployzd::operations::deploy::{
    DataplanePreparer, DeployCleanupResult, DeployExecutionCommand, DeployExecutionError,
    DeployExecutionOutcome, DeployExecutionPorts, DeployExecutionStep, DeployHealthCheckError,
    DeployHealthChecker, DeployOperationRecorder, DeployTerminalEvent, MachineContainerRuntime,
    MachineContainerRuntimeError, NamespaceStateCommitter, execute_deploy_operation,
};
use ployzd::roles::machine::protocol::MachineEnsureEndpointNetworkRpcRequest;
use std::time::Duration;

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

async fn execute_deploy<R, D, N, H, S>(
    command: DeployExecutionCommand,
    ports: DeployExecutionPorts<'_, R, D, N, H, S>,
) -> Result<DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
    D: DataplanePreparer,
    N: MachineContainerRuntime,
    H: DeployHealthChecker,
    S: NamespaceStateCommitter,
{
    execute_deploy_operation(command, ports).await
}

#[tokio::test]
async fn deploy_worker_runs_containers_then_completes() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let outcome = execute_deploy(
        command.clone(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("deploy succeeds");
    assert_eq!(
        outcome.namespace_revision_id,
        target_namespace_revision_id(2)
    );
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
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::DataplanePrepared { machine_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_b"),
                container_id: container_id("ctr_2"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::completed()),
        ]
    );
    assert_eq!(runtime.requests.len(), 2);
    assert_eq!(
        runtime.endpoint_networks,
        vec![
            (
                machine_id("machine_a"),
                MachineEnsureEndpointNetworkRpcRequest {
                    operation_id: operation_id("op_123"),
                },
            ),
            (
                machine_id("machine_b"),
                MachineEnsureEndpointNetworkRpcRequest {
                    operation_id: operation_id("op_123"),
                },
            ),
        ]
    );
    let [dataplane_request] = wireguard_ebpf.requests.as_slice() else {
        panic!("expected exactly one dataplane prepare request");
    };
    assert_eq!(
        dataplane_request.membership,
        vec![
            DataplaneMember::default_for_machine(machine_id("machine_a")),
            DataplaneMember::default_for_machine(machine_id("machine_b")),
        ]
    );
    assert_eq!(
        namespace_state.serving_requests,
        vec![ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: target_namespace_revision_entry_id(),
        }]
    );
    assert_eq!(
        health.checked,
        vec![vec![
            DeployContainerForAssert::new("machine_a", "ctr_1"),
            DeployContainerForAssert::new("machine_b", "ctr_2"),
        ]]
    );
    let [
        (first_machine_id, first_request),
        (second_machine_id, second_request),
    ] = runtime.requests.as_slice()
    else {
        panic!("expected exactly two runtime requests");
    };
    assert_eq!(*first_machine_id, machine_id("machine_a"));
    assert_eq!(first_request.container.operation_id, operation_id("op_123"));
    assert_eq!(first_request.container.step_id.as_str(), "run_1");
    assert_eq!(*second_machine_id, machine_id("machine_b"));
    assert_eq!(second_request.container.step_id.as_str(), "run_2");
}

#[tokio::test]
async fn deploy_worker_commits_volume_pin_and_mounts_volume() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = volume_backed_deploy_command(1);

    execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("deploy succeeds");

    assert_eq!(
        namespace_state.volume_pin_requests,
        vec![VolumePinState {
            namespace_id: namespace_id("default"),
            volume_name: volume_name("postgres_data"),
            machine_id: machine_id("machine_a"),
        }]
    );
    let [(request_machine_id, request)] = runtime.requests.as_slice() else {
        panic!("expected one runtime request");
    };
    assert_eq!(*request_machine_id, machine_id("machine_a"));
    let [mount] = request.runtime.volume_mounts.as_slice() else {
        panic!("expected one runtime volume mount");
    };
    assert_eq!(mount.volume_name, volume_name("postgres_data"));
    assert_eq!(mount.target.as_str(), "/var/lib/postgresql/data");
}

#[tokio::test]
async fn deploy_worker_reuses_running_target_containers_from_observed_reality() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_with_existing_container(2, "machine_b", "ctr_existing");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
    let [(request_machine_id, _)] = runtime.requests.as_slice() else {
        panic!("expected one runtime request");
    };
    assert_eq!(*request_machine_id, machine_id("machine_a"));
    assert_eq!(
        health.checked,
        vec![vec![
            DeployContainerForAssert::new("machine_b", "ctr_existing"),
            DeployContainerForAssert::new("machine_a", "ctr_new"),
        ]]
    );
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::DataplanePrepared { machine_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_new"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Running {
                stage: active_service_running(),
            }),
            RecordedOperation::Transition(DeployTransition::completed()),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_removes_superseded_containers_after_active_commit() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("deploy succeeds and cleans up old containers");

    assert_eq!(
        runtime.removals,
        vec![(
            machine_id("machine_b"),
            ployzd::roles::machine::protocol::MachineContainerRemoveRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id("ctr_old"),
                expected_identity: cleanup_container("machine_b", "ctr_old", "entry_old").identity,
            },
        )]
    );
    let cleanup_target = cleanup_container("machine_b", "ctr_old", "entry_old");
    assert_eq!(
        outcome.cleanup,
        vec![DeployCleanupResult::Removed(cleanup_target.clone())]
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::ServingTargetCommit,
        DeployRunningStage::RemovingSupersededContainers,
    );
    assert!(namespace_state.serving_removals.is_empty());
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::completed()))
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
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("deploy succeeds even when old-container cleanup fails");

    assert_eq!(namespace_state.serving_requests.len(), 1);
    let cleanup_target = cleanup_container("machine_b", "ctr_old", "entry_old");
    assert_eq!(
        outcome.cleanup,
        vec![DeployCleanupResult::Failed {
            target: cleanup_target.clone(),
            message: failure_message("container remove failed: busy"),
        }]
    );
    assert_eq!(
        outcome.completion_outcome(),
        DeployCompletionOutcome::CompletedWithWarnings
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
        Some(&RecordedOperation::Transition(
            DeployTransition::Completed {
                outcome: DeployCompletionOutcome::CompletedWithWarnings,
            }
        ))
    );
}

#[tokio::test]
async fn empty_deploy_removes_running_namespace_containers() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = empty_deploy_command_with_running_container("machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("empty deploy succeeds");

    let cleanup_target = cleanup_container("machine_b", "ctr_old", "entry_old");
    assert_eq!(
        runtime.removals,
        vec![(
            machine_id("machine_b"),
            ployzd::roles::machine::protocol::MachineContainerRemoveRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id("ctr_old"),
                expected_identity: cleanup_target.identity.clone(),
            },
        )]
    );
    // Manifest omission unpublishes the service and detaches its routes:
    // an empty deploy must not leave the old service serveable in KV.
    assert_eq!(
        namespace_state.serving_removals,
        vec![service_id("svc_api")]
    );
    assert_eq!(
        namespace_state.route_removals,
        vec![RouteTarget::new(
            RouteHostname::try_new("api.example.com").expect("valid route hostname"),
            route_port(443),
        )]
    );
    assert!(runtime.requests.is_empty());
    assert_eq!(health.checked, Vec::<Vec<DeployContainerForAssert>>::new());
    assert_eq!(
        outcome.cleanup,
        vec![DeployCleanupResult::Removed(cleanup_target.clone())]
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
async fn deploy_worker_does_not_record_warning_completion_without_cleanup_failure_evidence() {
    let mut recorder = RecordingOperations::fail_cleanup_evidence_times(1);
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]).with_remove_failure();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("deploy succeeds even when cleanup evidence cannot be recorded");

    assert_eq!(namespace_state.serving_requests.len(), 1);
    assert_eq!(
        outcome.completion_outcome(),
        DeployCompletionOutcome::CompletedWithWarnings
    );
    assert_eq!(outcome.terminal_event, DeployTerminalEvent::Missing);
    assert_eq!(recorder.completed_transition_attempts, 0);
    assert!(!recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::Transition(DeployTransition::Completed {
            outcome: DeployCompletionOutcome::CompletedWithWarnings,
        })
    )));
}

#[tokio::test]
async fn deploy_worker_counts_warning_completion_write_failure() {
    let mut recorder = RecordingOperations::fail_completed_transition_times(1);
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]).with_remove_failure();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("deploy succeeds even when warning completion cannot be recorded");

    assert_eq!(
        outcome.completion_outcome(),
        DeployCompletionOutcome::CompletedWithWarnings
    );
    assert_eq!(outcome.terminal_event, DeployTerminalEvent::Missing);
    assert_eq!(recorder.completed_transition_attempts, 1);
}

#[tokio::test]
async fn deploy_worker_does_not_claim_existing_container_as_retained_artifact() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::unhealthy("machine_b", "ctr_existing");
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_with_existing_container(1, "machine_b", "ctr_existing");

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
                        machine_id: failed_machine_id,
                        container_id: failed_container_id,
                        ..
                    },
                retained_artifacts,
            },
            ..
        } if failed_machine_id == machine_id("machine_b")
            && failed_container_id == container_id("ctr_existing")
            && retained_artifacts.is_empty()
    ));
    assert!(runtime.requests.is_empty());
    assert!(runtime.stops.is_empty());
    assert!(namespace_state.serving_requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_treats_reused_operation_step_container_as_progress() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::reusing_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            })
    );
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::completed()))
    );
    assert_eq!(runtime.requests.len(), 1);
}

#[tokio::test]
async fn deploy_worker_records_failure_when_container_run_fails() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::failing_after_first_container();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("deploy fails");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::RunContainer(MachineContainerRuntimeError::Unavailable { .. }))
    ));
    assert_eq!(runtime.requests.len(), 2);
    assert!(namespace_state.serving_requests.is_empty());
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::DataplanePrepared { machine_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::RuntimeUnavailable {
                    machine_id: machine_id("machine_b"),
                    message: ployz_core::ops::FailureMessage::try_new(
                        "machine runtime request failed: synthetic runtime failure",
                    )
                    .expect("valid failure message"),
                    retained_artifacts: vec![retained_container("machine_a", "ctr_1")],
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
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
                    machine_id: failure_machine_id,
                    message,
                    retained_artifacts,
                },
            ..
        } if matches!(*source, DeployExecutionError::RunContainer(MachineContainerRuntimeError::CreatedContainerStartFailed { .. }))
            && failure_machine_id == machine_id("machine_a")
            && message == failure_message("container start failed: exec format error")
            && retained_artifacts == vec![retained_created_container("machine_a", "ctr_created")]
    ));
    assert!(namespace_state.serving_requests.is_empty());
    assert_eq!(
        runtime.stops,
        runtime
            .requests
            .iter()
            .zip([container_id("ctr_created")])
            .map(|((request_machine_id, request), container_id)| {
                (
                    request_machine_id.clone(),
                    ployzd::roles::machine::protocol::MachineContainerStopRpcRequest {
                        operation_id: operation_id("op_123"),
                        container_id,
                        expected_identity: request.container.clone(),
                    },
                )
            })
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn deploy_worker_reports_retained_stop_failure_in_terminal_failure() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]).with_stop_failure();
    let mut health = RecordingHealth::unhealthy("machine_a", "ctr_1");
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("deploy fails");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            failure:
                DeployOperationFailure::HealthCheckFailed {
                    retained_artifacts,
                    ..
                },
            ..
        } if retained_artifacts == vec![
            retained_container("machine_a", "ctr_1"),
            retained_stop_failed_container("machine_a", "ctr_1"),
        ]
    ));
    assert_eq!(runtime.stops.len(), 1);
}

#[tokio::test]
async fn deploy_worker_records_planning_before_plan_failure() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_without_eligible_machines(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
                failure: DeployOperationFailure::NoUsableMachines {
                    reasons: Vec::new(),
                }
            }),
        ]
    );
    assert!(runtime.requests.is_empty());
    assert!(health.checked.is_empty());
    assert!(namespace_state.serving_requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_fails_before_wireguard_ebpf_when_endpoint_network_is_unavailable() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime =
        RecordingRuntime::with_containers(["ctr_1", "ctr_2"]).with_endpoint_network_failure();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("deploy fails before wireguard/eBPF preparation");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::RunContainer(_))
    ));
    assert_eq!(runtime.endpoint_networks.len(), 1);
    assert!(wireguard_ebpf.requests.is_empty());
    assert!(runtime.requests.is_empty());
    assert!(health.checked.is_empty());
    assert!(namespace_state.serving_requests.is_empty());
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::RuntimeUnavailable {
                    machine_id: machine_id("machine_a"),
                    message: failure_message(
                        "machine runtime request failed: synthetic endpoint network failure"
                    ),
                    retained_artifacts: Vec::new(),
                }
            }),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_fails_before_container_run_when_wireguard_ebpf_is_unavailable() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::wireguard_failed("machine_b");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("deploy fails before container mutation");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::PrepareDataplane(_))
    ));
    assert!(runtime.requests.is_empty());
    assert!(health.checked.is_empty());
    assert!(namespace_state.serving_requests.is_empty());
    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::DataplaneUnavailable {
                    machine_id: machine_id("machine_b"),
                    provider_failure:
                        ployz_core::dataplane::DataplaneProviderFailure::PloyzNativeMesh {
                            component: ployz_core::dataplane::PloyzNativeMeshComponent::WireGuard,
                        },
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
    let mut health = RecordingHealth::unhealthy("machine_b", "ctr_2");
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::DataplanePrepared { machine_count: 2 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_b"),
                container_id: container_id("ctr_2"),
            },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            RecordedOperation::HealthCheckStarted,
            RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::HealthCheckFailed {
                    health_check: ployz_core::ops::HealthCheckFailure::ProbeFailed {
                        machine_id: machine_id("machine_b"),
                        container_id: container_id("ctr_2"),
                        message: ployz_core::ops::FailureMessage::try_new("probe failed")
                            .expect("valid failure message"),
                        log_hint: ployz_core::ops::OperatorHint::try_new("ployz logs ctr_2")
                            .expect("valid log hint"),
                    },
                    retained_artifacts: vec![
                        retained_container("machine_a", "ctr_1"),
                        retained_container("machine_b", "ctr_2"),
                    ],
                }
            }),
        ]
    );
    assert!(namespace_state.serving_requests.is_empty());
}

#[tokio::test]
async fn routed_deploy_commits_route_before_completion() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = routed_deploy_command(1);

    execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("routed deploy succeeds");

    assert_eq!(
        namespace_state.route_requests,
        vec![RouteBindingState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
        }]
    );
    assert_eq!(namespace_state.serving_requests.len(), 1);
    let [(_, runtime_request)] = runtime.requests.as_slice() else {
        panic!("expected one runtime request");
    };
    assert_eq!(
        runtime_request.container.namespace_revision_entry_id,
        target_namespace_revision_entry_id()
    );
    assert_eq!(
        health.checked,
        vec![vec![DeployContainerForAssert::routed("machine_a", "ctr_1")]]
    );
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::completed()))
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::RouteCutover,
        DeployRunningStage::ServingTargetCommit,
    );
}

#[tokio::test]
async fn deploy_worker_times_out_hanging_steps() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = HangingHealth;
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
                retained_artifacts: vec![retained_container("machine_a", "ctr_1")],
            }
        }))
    );
    assert!(namespace_state.serving_requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_keeps_success_when_completed_event_fails_after_active_commit() {
    let mut recorder = RecordingOperations::fail_completed_transition_times(1);
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("active commit succeeds even when the completed event is rejected");
    assert_eq!(
        outcome.namespace_revision_id,
        target_namespace_revision_id(1)
    );
    assert_eq!(outcome.terminal_event, DeployTerminalEvent::Missing);

    assert_eq!(
        recorder.records,
        vec![
            RecordedOperation::Transition(DeployTransition::Planning),
            RecordedOperation::PlanCreated { replica_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::DataplanePrepared { machine_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_a"),
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
    assert_eq!(namespace_state.serving_requests.len(), 1);
    assert_eq!(recorder.completed_transition_attempts, 1);
}

#[tokio::test]
async fn deploy_worker_marks_failed_when_active_commit_times_out() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::hanging_serving_commits();
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
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
                step: DeployExecutionStep::CommitServingTarget,
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
                stage: DeployRunningStage::PreparingDataplane,
            }),
            RecordedOperation::DataplanePrepared { machine_count: 1 },
            RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            RecordedOperation::ContainerStarted {
                machine_id: machine_id("machine_a"),
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
                    scope: ployz_core::ops::ControlPlaneCommitScope::ServiceEntry {
                        service_id: service_id("svc_api"),
                        namespace_revision_entry_id: target_namespace_revision_entry_id(),
                    },
                    message: failure_message("serving target commit timed out after 1ms"),
                    retained_artifacts: vec![retained_container("machine_a", "ctr_1")],
                }
            }),
        ]
    );
}

#[tokio::test]
async fn deploy_worker_records_retained_artifacts_when_namespace_lock_is_lost_before_commit() {
    let mut recorder = RecordingOperations::default();
    let mut wireguard_ebpf = RecordingWireGuardEbpf::ready();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::lost_lock_serving_commits();

    let error = execute_deploy(
        deploy_command(1),
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane: &mut wireguard_ebpf,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("lost namespace lock fails the operation through the worker path");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            failure: DeployOperationFailure::ControlPlaneCommitFailed { .. },
            ..
        } if matches!(
            *source,
            DeployExecutionError::CommitNamespaceState(
                ployzd::operations::deploy::NamespaceCommitError::ServingTargetLockLost { .. }
            )
        )
    ));
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Failed {
            failure: DeployOperationFailure::ControlPlaneCommitFailed {
                scope: ployz_core::ops::ControlPlaneCommitScope::ServiceEntry {
                    service_id: service_id("svc_api"),
                    namespace_revision_entry_id: target_namespace_revision_entry_id(),
                },
                message: failure_message("namespace lock was lost before serving target commit"),
                retained_artifacts: vec![retained_container("machine_a", "ctr_1")],
            }
        }))
    );
}
