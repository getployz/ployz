#[path = "deploy_operation/fixtures.rs"]
mod fixtures;

use fixtures::*;
use ployz_core::deploy::{ContainerCommand, ContainerRestartPolicy, ReplicaCount};
use ployz_core::intent::{ServingTargetEntry, VolumePinState};
use ployz_core::machine::runtime::ManagedContainerKind;
use ployz_core::operation::{
    CertificateProvisionFailure, DeployCompletionOutcome, DeployEvidence, DeployOperationFailure,
    DeployPhaseOutcome, DeployRunningStage, DeployServiceResult, DeployTransition, FailureMessage,
    PreStartHookFailure, RouteHostname, RouteTarget,
};
use ployz_test_support::ids::{failure_message, namespace_id};
use ployzd::operations::deploy::{
    CertificateProvisioner, DeployCleanupResult, DeployExecutionError, DeployExecutionInput,
    DeployExecutionOutcome, DeployExecutionPorts, DeployExecutionStep, DeployHealthCheckError,
    DeployHealthChecker, DeployOperationRecorder, DeployTerminalEvent, MachineContainerRuntime,
    MachineContainerRuntimeError, NamespaceStateCommitter, execute_deploy_operation,
};
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

async fn execute_deploy<R, N, H, C, S>(
    command: DeployExecutionInput,
    ports: DeployExecutionPorts<'_, R, N, H, C, S>,
) -> Result<DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
    N: MachineContainerRuntime,
    H: DeployHealthChecker,
    C: CertificateProvisioner,
    S: NamespaceStateCommitter,
{
    execute_deploy_operation(command, ports).await
}

#[tokio::test]
async fn deploy_worker_runs_containers_then_completes() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let outcome = execute_deploy(
        command.clone(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    assert!(runtime.requests.iter().all(|(_, request)| matches!(
        &request.pull,
        ployzd::roles::machine::protocol::MachineImagePull::Registry {
            reference,
            credential: None,
        } if reference == &resolved_registry_image("registry.example/api:rev_2")
    )));
    assert_eq!(
        namespace_state.serving_requests,
        vec![ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: target_namespace_revision_entry_id(),
            image: resolved_registry_image("registry.example/api:rev_2"),
            desired_replicas: ReplicaCount::try_new(2).expect("valid replica count"),
            volume_names: Vec::new(),
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
async fn deploy_promotes_each_dependency_phase_before_starting_the_next() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_database", "ctr_web"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        phased_deploy_command(&["svc_database", "svc_web"]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("phased deploy succeeds");

    assert_eq!(namespace_state.phase_requests.len(), 2);
    assert!(matches!(
        recorder.phase_records.as_slice(),
        [
            DeployEvidence::PhaseStarted {
                phase: first_started,
                ..
            },
            DeployEvidence::PhaseFinished {
                phase: first_finished,
                outcome: DeployPhaseOutcome::Promoted,
                ..
            },
            DeployEvidence::PhaseStarted {
                phase: second_started,
                ..
            },
            DeployEvidence::PhaseFinished {
                phase: second_finished,
                outcome: DeployPhaseOutcome::Promoted,
                ..
            }
        ] if *first_started == phase_number(1)
            && *first_finished == phase_number(1)
            && *second_started == phase_number(2)
            && *second_finished == phase_number(2)
    ));
}

#[tokio::test]
async fn reused_promoted_dependency_is_unchanged_and_not_regated() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_web"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        phased_deploy_with_reused_dependency(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("reused dependency deploy succeeds");

    assert_eq!(runtime.requests.len(), 1);
    assert_eq!(health.checked.len(), 1);
    let Some(dependency_result) = recorder.phase_records.get(1) else {
        panic!("dependency phase result is recorded");
    };
    assert!(matches!(
        dependency_result,
        DeployEvidence::PhaseFinished {
            services,
            ..
        } if services == &vec![DeployServiceResult::Unchanged {
            service_id: service_id("svc_database")
        }]
    ));
}

#[tokio::test]
async fn healthy_dependency_without_healthcheck_fails_before_runtime_mutation() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        invalid_healthy_dependency_command(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("invalid healthy dependency is rejected");

    assert!(runtime.requests.is_empty());
    assert!(runtime.stops.is_empty());
    assert!(namespace_state.phase_requests.is_empty());
}

#[tokio::test]
async fn same_phase_failure_cleans_successes_and_promotes_nothing() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::failing_after_first_container();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        same_phase_deploy_command(&["svc_a", "svc_b"]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("second service fails");

    assert!(namespace_state.phase_requests.is_empty());
    assert_eq!(runtime.stops.len(), 1);
    assert_eq!(runtime.removals.len(), 1);
    assert!(matches!(
        recorder.phase_records.last(),
        Some(DeployEvidence::PhaseFinished {
            outcome: DeployPhaseOutcome::Failed,
            services,
            ..
        }) if matches!(services.as_slice(), [
            DeployServiceResult::Completed { .. },
            DeployServiceResult::Failed { .. }
        ])
    ));
}

#[tokio::test]
async fn committed_phase_is_not_cleaned_up_when_phase_evidence_write_fails() {
    let mut recorder = RecordingOperations::fail_phase_finished_evidence_times(1);
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    let error = execute_deploy(
        deploy_command(1),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("phase evidence failure is visible");

    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            source,
            ..
        } if matches!(*source, DeployExecutionError::RecordEvidence(_))
    ));

    assert_eq!(namespace_state.phase_requests.len(), 1);
    assert!(runtime.stops.is_empty());
    assert!(runtime.removals.is_empty());
    assert!(matches!(
        recorder.records.last(),
        Some(RecordedOperation::Transition(DeployTransition::Completed {
            outcome: DeployCompletionOutcome::PartiallyCompleted
        }))
    ));
    assert!(matches!(
        recorder.phase_records.as_slice(),
        [DeployEvidence::PhaseStarted { .. }]
    ));
}

#[tokio::test]
async fn later_phase_failure_records_partial_outcome_and_skips_remaining_services() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::failing_after_first_container();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        phased_deploy_command(&["svc_database", "svc_web", "svc_worker"]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("second phase fails");

    assert_eq!(namespace_state.phase_requests.len(), 1);
    assert!(runtime.stops.is_empty());
    assert!(runtime.removals.is_empty());
    assert!(matches!(
        recorder.records.last(),
        Some(RecordedOperation::Transition(DeployTransition::Completed {
            outcome: DeployCompletionOutcome::PartiallyCompleted
        }))
    ));
    assert!(matches!(
        recorder.phase_records.last(),
        Some(DeployEvidence::PhaseFinished {
            phase,
            outcome: DeployPhaseOutcome::Failed,
            services,
        }) if *phase == phase_number(2) && matches!(services.as_slice(), [
            DeployServiceResult::Failed { .. },
            DeployServiceResult::Skipped { .. }
        ])
    ));
}

#[tokio::test]
async fn digest_pinned_registry_image_skips_resolution() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        pinned_deploy_command(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("pinned deploy succeeds");

    assert!(runtime.resolutions.is_empty());
}

#[tokio::test]
async fn route_less_pushed_deploy_uses_one_membership_for_prepare_and_seed_pull() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        route_less_pushed_deploy_command(2),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("route-less pushed deploy succeeds");

    assert!(runtime.requests.iter().all(|(_, request)| matches!(
        &request.pull,
        ployzd::roles::machine::protocol::MachineImagePull::MeshSeed { seed_host, .. }
            if *seed_host
                == "10.198.99.254".parse::<std::net::Ipv4Addr>().expect("valid seed host")
    )));
}

#[tokio::test]
async fn pre_start_hook_runs_before_service_with_derived_runtime() {
    let mut recorder = RecordingOperations::default();
    let mut runtime =
        RecordingRuntime::with_containers(["ctr_service"]).with_hook_outcome("ctr_hook", 0);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_with_pre_start();

    execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("deploy succeeds");

    let [(hook_machine, hook_request)] = runtime.hook_requests.as_slice() else {
        panic!("expected one pre-start hook request");
    };
    assert_eq!(hook_machine, &machine_id("machine_a"));
    assert_eq!(hook_request.container.kind, ManagedContainerKind::Predeploy);
    assert_eq!(hook_request.container.step_id.as_str(), "pre_start");
    assert_eq!(
        hook_request.runtime.command,
        Some(
            ContainerCommand::try_new(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "echo ready".to_owned(),
            ])
            .expect("valid hook command")
        )
    );
    assert_eq!(hook_request.runtime.healthcheck, None);
    assert_eq!(
        hook_request.runtime.restart_policy,
        ContainerRestartPolicy::No
    );
    assert_eq!(runtime.requests.len(), 1);
}

#[tokio::test]
async fn nonzero_pre_start_hook_fails_before_service_start_and_retains_hook() {
    let mut recorder = RecordingOperations::default();
    let mut runtime =
        RecordingRuntime::with_containers(["ctr_service"]).with_hook_outcome("ctr_hook", 7);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    let error = execute_deploy(
        deploy_command_with_pre_start(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("hook failure rejects deploy");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("expected recorded deploy failure");
    };
    let DeployOperationFailure::PreStartHookFailed {
        machine_id: failed_machine,
        failure:
            PreStartHookFailure::Exited {
                container_id: failed_container,
                exit_code,
                ..
            },
        retained_artifacts,
        ..
    } = *failure
    else {
        panic!("expected pre-start hook failure");
    };
    assert_eq!(failed_machine, machine_id("machine_a"));
    assert_eq!(failed_container, container_id("ctr_hook"));
    assert_eq!(exit_code, 7);
    assert_eq!(retained_artifacts.len(), 1);
    assert!(runtime.requests.is_empty());
}

#[tokio::test]
async fn pre_start_hook_cleanup_failure_is_typed_and_blocks_service_start() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_service"])
        .with_hook_outcome("ctr_hook", 0)
        .with_remove_failure();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    let error = execute_deploy(
        deploy_command_with_pre_start(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("hook cleanup failure rejects deploy");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("expected recorded deploy failure");
    };
    let DeployOperationFailure::PreStartHookFailed {
        failure:
            PreStartHookFailure::CleanupFailed {
                container_id: failed_container,
                ..
            },
        ..
    } = *failure
    else {
        panic!("expected typed hook cleanup failure");
    };
    assert_eq!(failed_container, container_id("ctr_hook"));
    assert!(runtime.requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_commits_volume_pin_and_mounts_volume() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = volume_backed_deploy_command(1);

    execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_with_existing_container(2, "machine_b", "ctr_existing");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
        vec![vec![DeployContainerForAssert::new("machine_a", "ctr_new")]]
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]).with_remove_failure();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
                failed: vec![ployz_core::operation::DeployCleanupFailure {
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
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = empty_deploy_command_with_running_container("machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]).with_remove_failure();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]).with_remove_failure();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container(1, "machine_b", "ctr_old");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
async fn deploy_worker_does_not_health_check_existing_container() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::unhealthy("machine_b", "ctr_existing");
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_with_existing_container(1, "machine_b", "ctr_existing");

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("existing target container is not health-gated");

    assert_eq!(
        outcome
            .containers
            .iter()
            .map(|container| container.container_id.clone())
            .collect::<Vec<_>>(),
        vec![container_id("ctr_existing")]
    );
    assert!(health.checked.is_empty());
    assert!(runtime.requests.is_empty());
    assert!(runtime.stops.is_empty());
    assert_eq!(namespace_state.serving_requests.len(), 1);
}

#[tokio::test]
async fn deploy_worker_treats_reused_operation_step_container_as_progress() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::reusing_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
async fn deploy_worker_does_not_health_check_started_existing_container() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::starting_existing_containers(["ctr_1"]);
    let mut health = RecordingHealth::unhealthy("machine_a", "ctr_1");
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_with_healthcheck(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("started existing target container is not health-gated");

    assert_eq!(
        outcome
            .containers
            .iter()
            .map(|container| container.container_id.clone())
            .collect::<Vec<_>>(),
        vec![container_id("ctr_1")]
    );
    assert!(health.checked.is_empty());
}

#[tokio::test]
async fn deploy_worker_records_failure_when_container_run_fails() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::failing_after_first_container();
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    assert_eq!(runtime.stops.len(), 1);
    assert_eq!(runtime.removals.len(), 1);
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::CleanupFinished { removed, failed }
            if removed.len() == 1 && failed.is_empty()
    )));
    assert!(matches!(
        recorder.records.last(),
        Some(RecordedOperation::Transition(DeployTransition::Failed {
            failure: DeployOperationFailure::RuntimeUnavailable {
                retained_artifacts,
                ..
            }
        })) if retained_artifacts.is_empty()
    ));
}

#[tokio::test]
async fn deploy_worker_retains_created_container_when_start_fails() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::failing_start("ctr_created");
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("deploy fails");

    let DeployExecutionError::Failed {
        source, failure, ..
    } = error
    else {
        panic!("deploy must record a terminal failure");
    };
    let DeployOperationFailure::ContainerStartFailed {
        machine_id: failure_machine_id,
        container_id: failure_container_id,
        message,
        retained_artifacts,
    } = *failure
    else {
        panic!("deploy must record container start failure");
    };
    assert!(matches!(
        *source,
        DeployExecutionError::RunContainer(
            MachineContainerRuntimeError::CreatedContainerStartFailed { .. }
        )
    ));
    assert_eq!(failure_machine_id, machine_id("machine_a"));
    assert_eq!(failure_container_id, container_id("ctr_created"));
    assert_eq!(
        message,
        failure_message("container start failed: exec format error")
    );
    assert_eq!(
        retained_artifacts,
        vec![retained_created_container("machine_a", "ctr_created")]
    );
    assert!(namespace_state.serving_requests.is_empty());
    assert!(runtime.stops.is_empty());
    assert!(runtime.removals.is_empty());
}

#[tokio::test]
async fn deploy_worker_does_not_stop_the_actual_failed_container() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]).with_stop_failure();
    let mut health = RecordingHealth::unhealthy("machine_a", "ctr_1");
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("deploy fails");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy must record a terminal failure");
    };
    let DeployOperationFailure::HealthCheckFailed {
        retained_artifacts, ..
    } = *failure
    else {
        panic!("deploy must record health check failure");
    };
    assert_eq!(
        retained_artifacts,
        vec![retained_container("machine_a", "ctr_1")]
    );
    assert!(runtime.stops.is_empty());
}

#[tokio::test]
async fn deploy_worker_records_planning_before_plan_failure() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_without_eligible_machines(1);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
async fn deploy_worker_waits_for_health_before_completing() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1", "ctr_2"]);
    let mut health = RecordingHealth::unhealthy("machine_b", "ctr_2");
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(2);

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    assert_eq!(runtime.stops.len(), 1);
    assert_eq!(runtime.removals.len(), 1);
    assert!(matches!(
        recorder.records.last(),
        Some(RecordedOperation::Transition(DeployTransition::Failed {
            failure: DeployOperationFailure::HealthCheckFailed {
                retained_artifacts,
                ..
            }
        })) if retained_artifacts == &vec![retained_container("machine_b", "ctr_2")]
    ));
    assert!(namespace_state.serving_requests.is_empty());
}

#[tokio::test]
async fn custom_https_deploy_ensures_certificate_before_route_commit() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();
    let mut namespace_state =
        RecordingNamespaceState::requiring_certificate_ready(certificates.readiness());
    let command = routed_deploy_command(1);

    execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("routed deploy succeeds");

    let [route] = namespace_state.route_requests.as_slice() else {
        panic!("one route is committed");
    };
    assert_eq!(route.namespace_id, namespace_id("default"));
    assert_eq!(route.target, route_target("api.example.com", 443));
    assert_eq!(route.endpoint_port, route_port(8080));
    assert_eq!(route.service_id, service_id("svc_api"));
    assert_eq!(
        route.origin,
        ployz_core::ingress::RouteBindingOrigin::Declared
    );
    assert_eq!(namespace_state.serving_requests.len(), 1);
    assert_eq!(
        certificates.requests,
        vec![(
            operation_id("op_123"),
            RouteHostname::try_new("api.example.com").expect("valid route hostname"),
            vec![ployzd::certificate::GatewayCertificateTarget {
                machine_id: machine_id("gateway_a"),
                public_ips: vec!["203.0.113.10".parse().expect("valid gateway IP")],
            }],
        )]
    );
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
        DeployRunningStage::EnsuringCertificates,
        DeployRunningStage::RouteCutover,
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::RouteCutover,
        DeployRunningStage::ServingTargetCommit,
    );
}

#[tokio::test]
async fn replaced_service_routes_are_removed_inside_the_phase_commit() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        routed_deploy_replacing_route_command(1),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("routed deploy succeeds");

    let [(_, phase_route_removals, _)] = namespace_state.phase_requests.as_slice() else {
        panic!("expected one phase commit");
    };
    assert_eq!(
        phase_route_removals,
        &vec![route_target("old.example.com", 443)]
    );
    assert_eq!(
        namespace_state.route_removals,
        vec![route_target("old.example.com", 443)]
    );
}

#[tokio::test]
async fn custom_https_certificate_failure_leaves_route_uncommitted() {
    let hostname = RouteHostname::try_new("api.example.com").expect("valid route hostname");
    let message = || FailureMessage::try_new("certificate failed").expect("valid failure message");
    let failures = vec![
        CertificateProvisionFailure::OperationEvidenceWrite { message: message() },
        CertificateProvisionFailure::DnsPreflight { message: message() },
        CertificateProvisionFailure::ChallengePublish { message: message() },
        CertificateProvisionFailure::ChallengeReadiness {
            missing_machine_ids: vec![machine_id("gateway_a")],
        },
        CertificateProvisionFailure::AcmeValidation { message: message() },
        CertificateProvisionFailure::GatewayArtifactPush {
            machine_id: machine_id("gateway_a"),
            message: message(),
        },
        CertificateProvisionFailure::ActiveCertCommit {
            attempted_active_cert: active_certificate(hostname.clone()),
            message: message(),
        },
    ];

    for failure in failures {
        let mut recorder = RecordingOperations::default();
        let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
        let mut health = RecordingHealth::healthy();
        let mut certificates = RecordingCertificates::failing(failure.clone());
        let mut namespace_state = RecordingNamespaceState::stored();

        execute_deploy(
            routed_deploy_command(1),
            DeployExecutionPorts {
                recorder: &mut recorder,
                machine_runtime: &mut runtime,
                health_checker: &mut health,
                certificate_provisioner: &mut certificates,
                namespace_state: &mut namespace_state,
            },
        )
        .await
        .expect_err("certificate failure fails the deploy");

        assert_eq!(
            recorder.records.last(),
            Some(&RecordedOperation::Transition(DeployTransition::Failed {
                failure: DeployOperationFailure::CertificateProvisionFailed {
                    hostname: hostname.clone(),
                    namespace_revision_id: routed_namespace_revision_id(),
                    failure,
                    retained_artifacts: Vec::new(),
                },
            }))
        );
        assert!(namespace_state.route_requests.is_empty());
        assert!(namespace_state.serving_requests.is_empty());
    }
}

#[tokio::test]
async fn ployz_automatic_route_synchronizes_wildcard_before_commit() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::successful();
    let mut namespace_state =
        RecordingNamespaceState::requiring_certificate_ready(certificates.ployz_readiness());

    execute_deploy(
        ployz_automatic_deploy_command(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("managed HTTPS and plain HTTP routes deploy");

    assert_eq!(certificates.ployz_wildcard_requests, 1);
    let [target_request] = certificates.ployz_target_requests.as_slice() else {
        panic!("expected one Ployz target request");
    };
    assert_eq!(target_request.len(), 1);
    assert_eq!(namespace_state.route_requests.len(), 1);
}

#[tokio::test]
async fn ployz_wildcard_sync_failure_leaves_automatic_binding_unattached() {
    let failure = CertificateProvisionFailure::GatewayArtifactPush {
        machine_id: machine_id("gateway_a"),
        message: FailureMessage::try_new("wildcard sync failed").expect("failure message"),
    };
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut certificates = RecordingCertificates::failing(failure);
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        ployz_automatic_deploy_command(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut certificates,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("wildcard synchronization failure fails deploy");

    assert_eq!(certificates.ployz_wildcard_requests, 1);
    assert!(namespace_state.route_requests.is_empty());
    assert!(namespace_state.serving_requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_times_out_hanging_steps() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = HangingHealth;
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
                health_check: ployz_core::operation::HealthCheckFailure::TimedOut {
                    timeout_seconds: 1
                },
                retained_artifacts: Vec::new(),
            }
        }))
    );
    assert!(namespace_state.serving_requests.is_empty());
}

#[tokio::test]
async fn deploy_worker_keeps_success_when_completed_event_fails_after_active_commit() {
    let mut recorder = RecordingOperations::fail_completed_transition_times(1);
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command(1);

    let outcome = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
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
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::hanging_serving_commits();
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

    let error = execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("active commit timeout fails the operation");

    let DeployExecutionError::Failed {
        source, failure, ..
    } = error
    else {
        panic!("deploy must record a terminal failure");
    };
    assert!(matches!(
        *failure,
        DeployOperationFailure::ControlPlaneCommitFailed { .. }
    ));
    assert!(matches!(
        *source,
        DeployExecutionError::StepTimedOut {
            step: DeployExecutionStep::CommitServingTarget { .. },
            ..
        }
    ));
    assert_eq!(runtime.stops.len(), 1);
    assert_eq!(runtime.removals.len(), 1);
    assert!(matches!(
        recorder.records.last(),
        Some(RecordedOperation::Transition(DeployTransition::Failed {
            failure: DeployOperationFailure::ControlPlaneCommitFailed {
                retained_artifacts,
                ..
            }
        })) if retained_artifacts.is_empty()
    ));
}

#[tokio::test]
async fn deploy_worker_records_retained_artifacts_when_namespace_lock_is_lost_before_commit() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::lost_lock_serving_commits();

    let error = execute_deploy(
        deploy_command(1),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("lost namespace lock fails the operation through the worker path");

    let DeployExecutionError::Failed {
        source, failure, ..
    } = error
    else {
        panic!("deploy must record a terminal failure");
    };
    assert!(matches!(
        *failure,
        DeployOperationFailure::ControlPlaneCommitFailed { .. }
    ));
    assert!(matches!(
        *source,
        DeployExecutionError::CommitNamespaceState(
            ployzd::operations::deploy::NamespaceCommitError::ServingTargetLockLost { .. }
        )
    ));
    assert_eq!(
        recorder.records.last(),
        Some(&RecordedOperation::Transition(DeployTransition::Failed {
            failure: DeployOperationFailure::ControlPlaneCommitFailed {
                scope: ployz_core::operation::ControlPlaneCommitScope::DeployPhase {
                    namespace_revision_id: target_namespace_revision_id(1),
                    phase: phase_number(1),
                },
                message: failure_message("namespace lock was lost before serving target commit"),
                retained_artifacts: Vec::new(),
            }
        }))
    );
}
