pub(crate) mod fixtures;
mod preparation;
mod preparation_nats;
mod runtime_nats;

use crate::control::operations::deploy::{
    CertificateProvisioner, DeployCleanupResult, DeployExecutionError, DeployExecutionInput,
    DeployExecutionOutcome, DeployExecutionPorts, DeployHealthCheckError, DeployHealthChecker,
    DeployOperationRecorder, DeployTerminalEvent, MachineContainerRuntime,
    MachineContainerRuntimeError, MachineImageRemovalRuntime, NamespaceStateCommitter,
    execute_deploy_operation,
};
use crate::control::role_client::machine::{MachineClockTestimony, MachineVolumeEnsureError};
use crate::roles::machine::protocol::MachineContainerStopOutcome;
use fixtures::*;
use ployz_core::deploy::{ContainerCommand, ContainerRestartPolicy, ReplicaCount, ServiceMode};
use ployz_core::intent::{ServingTargetEntry, VolumePinState};
use ployz_core::machine::MachineUsabilityReason;
use ployz_core::machine::runtime::ManagedContainerKind;
use ployz_core::operation::{
    CertInterruptionStage, CertificateInterruptionNextAction, CertificateProvisionFailure,
    DeployCompletionOutcome, DeployEvidence, DeployOperationFailure, DeployPhaseOutcome,
    DeployRunningStage, DeployServiceResult, DeployTransition, DeployVolumeHandoffRestartFailure,
    DeployVolumeHandoffRollbackOutcome, FailureMessage, OperationInterruptionCause,
    PreStartHookFailure, RetainedArtifact, RouteHostname, RouteTarget, UnusableMachine,
};
use ployz_test_support::ids::{failure_message, namespace_id};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
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
    N: MachineContainerRuntime + MachineImageRemovalRuntime,
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
            RecordedOperation::ImageAvailabilityVerified,
            RecordedOperation::ImageAvailabilityVerified,
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
        runtime.image_ensures.len(),
        2,
        "one target ensure per service/machine"
    );
    assert!(runtime.image_ensures.iter().all(|(_, request)| matches!(
        request,
        ployz_core::image::ImageEnsureRequest::Start {
            source: ployz_core::image::ImageEnsureSource::Registry { .. },
            ..
        }
    )));
    assert!(!recorder.records.iter().any(|record| {
        record
            == &RecordedOperation::Transition(DeployTransition::Running {
                stage: DeployRunningStage::EnsuringVolumes,
            })
    }));
    assert!(runtime.requests.iter().all(|(_, request)| matches!(
        &request.image,
        reference if reference == &resolved_registry_image("registry.example/api:rev_2")
    )));
    assert_eq!(
        namespace_state.serving_requests,
        vec![ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: target_namespace_revision_entry_id(),
            image: resolved_registry_image("registry.example/api:rev_2"),
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(2).expect("valid replica count")
            },
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
async fn image_ensure_transport_retries_preserve_one_owner_and_then_run() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]).with_image_ensure_unavailable(2);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    execute_deploy(
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
    .expect("transient ImageEnsure transport recovers");
    assert_eq!(runtime.image_ensures.len(), 3);
    let owners = runtime
        .image_ensures
        .iter()
        .filter_map(|(_, request)| match request {
            ployz_core::image::ImageEnsureRequest::Start { owner, .. } => Some(owner),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(owners.len(), 3);
    assert!(owners.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(runtime.requests.len(), 1);
}

#[tokio::test]
async fn image_ensure_status_testimony_retries_same_owner_and_then_runs() {
    use crate::roles::machine::MachineRuntimeUnavailableReason;
    let completed = ployz_core::image::ImageEnsureStatus::Completed {
        reference: resolved_registry_image("registry.example/api:rev_2"),
    };
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]).with_image_ensure_script([
        Ok(ployz_core::image::ImageEnsureStatus::Accepted),
        Err(MachineRuntimeUnavailableReason::RequestTimedOut),
        Err(MachineRuntimeUnavailableReason::NoResponders),
        Ok(completed),
    ]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    execute_deploy(
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
    .expect("status testimony recovers");
    assert_eq!(runtime.image_ensures.len(), 4);
    let owners = runtime
        .image_ensures
        .iter()
        .map(|(_, request)| match request {
            ployz_core::image::ImageEnsureRequest::Start { owner, .. }
            | ployz_core::image::ImageEnsureRequest::Status { owner }
            | ployz_core::image::ImageEnsureRequest::Cancel { owner } => owner,
        })
        .collect::<Vec<_>>();
    assert!(owners.windows(2).all(|pair| pair[0] == pair[1]));
    assert!(matches!(
        runtime.image_ensures[0].1,
        ployz_core::image::ImageEnsureRequest::Start { .. }
    ));
    assert!(
        runtime.image_ensures[1..]
            .iter()
            .all(|(_, request)| matches!(
                request,
                ployz_core::image::ImageEnsureRequest::Status { .. }
            ))
    );
    assert_eq!(runtime.requests.len(), 1);
}

#[tokio::test]
async fn exhausted_status_testimony_cancels_same_owner_and_never_runs() {
    use crate::roles::machine::MachineRuntimeUnavailableReason;
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]).with_image_ensure_script([
        Ok(ployz_core::image::ImageEnsureStatus::Accepted),
        Err(MachineRuntimeUnavailableReason::RequestTimedOut),
        Err(MachineRuntimeUnavailableReason::NoResponders),
        Err(MachineRuntimeUnavailableReason::RequestTimedOut),
    ]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    execute_deploy(
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
    .expect_err("status testimony exhaustion fails deploy");
    assert!(runtime.requests.is_empty());
    assert_eq!(runtime.image_ensures.len(), 5);
    let start_owner = match &runtime.image_ensures[0].1 {
        ployz_core::image::ImageEnsureRequest::Start { owner, .. } => owner,
        _ => panic!("first request is Start"),
    };
    assert!(
        matches!(&runtime.image_ensures[4].1, ployz_core::image::ImageEnsureRequest::Cancel { owner } if owner == start_owner)
    );
}

#[tokio::test]
async fn stalled_image_ensure_is_typed_and_blocks_container_run() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]).with_image_ensure_status(
        ployz_core::image::ImageEnsureStatus::Failed {
            failure: ployz_core::image::ImageEnsureFailure::Stalled {
                timeout_millis: 12_345,
            },
        },
    );
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
    .expect_err("stalled ImageEnsure fails deploy");
    assert!(runtime.requests.is_empty());
    assert!(matches!(
        error,
        DeployExecutionError::Failed { failure, .. } if matches!(*failure, DeployOperationFailure::ArtifactUnavailable {
            reason: ployz_core::operation::ArtifactUnavailableReason::ImagePullStalled {
                timeout_millis: 12_345,
                ..
            },
            ..
        })
    ));
}

#[tokio::test]
async fn cancelled_image_ensure_is_typed_and_blocks_container_run() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"])
        .with_image_ensure_status(ployz_core::image::ImageEnsureStatus::Cancelled);
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
    .expect_err("cancelled ImageEnsure fails deploy");
    assert!(runtime.requests.is_empty());
    assert!(matches!(
        error,
        DeployExecutionError::Failed { failure, .. } if matches!(*failure, DeployOperationFailure::ArtifactUnavailable {
            reason: ployz_core::operation::ArtifactUnavailableReason::ImagePullCancelled { .. },
            ..
        })
    ));
}

#[tokio::test]
async fn failed_image_ensure_preserves_message_and_blocks_container_run() {
    let mut recorder = RecordingOperations::default();
    let message = ployz_core::operation::FailureMessage::try_new("registry denied manifest")
        .expect("message");
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]).with_image_ensure_status(
        ployz_core::image::ImageEnsureStatus::Failed {
            failure: ployz_core::image::ImageEnsureFailure::PullFailed {
                message: message.clone(),
            },
        },
    );
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
    .expect_err("failed ImageEnsure fails deploy");
    assert!(runtime.requests.is_empty());
    assert!(
        matches!(error, DeployExecutionError::Failed { failure, .. } if matches!(&*failure, DeployOperationFailure::ArtifactUnavailable { reason: ployz_core::operation::ArtifactUnavailableReason::ImagePullFailed { message: actual, .. }, .. } if actual == &message))
    );
}

#[tokio::test]
async fn global_operation_records_full_placement_and_completes_with_deferral_warning() {
    let unavailable = UnusableMachine {
        machine_id: machine_id("machine_silent"),
        reason: MachineUsabilityReason::FactsUnavailable,
    };
    let draining = UnusableMachine {
        machine_id: machine_id("machine_draining"),
        reason: MachineUsabilityReason::Draining,
    };
    let command = global_deploy_command(
        vec![machine_id("machine_a")],
        vec![unavailable.clone(), draining],
        Vec::new(),
        Vec::new(),
    );
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_global"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

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
    .expect("global deploy completes");

    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::GlobalPlacement {
            candidates,
            selected,
            deferred,
            draining,
        } if candidates == &vec![
            machine_id("machine_a"),
            machine_id("machine_draining"),
            machine_id("machine_silent"),
        ] && selected == &vec![machine_id("machine_a")]
            && deferred == &vec![unavailable.clone()]
            && draining == &vec![machine_id("machine_draining")]
    )));
    assert!(matches!(
        recorder.records.last(),
        Some(RecordedOperation::Transition(DeployTransition::Completed {
            outcome: DeployCompletionOutcome::CompletedWithWarnings,
        }))
    ));
    assert_eq!(
        outcome.completion_outcome,
        DeployCompletionOutcome::CompletedWithWarnings
    );
}

#[tokio::test]
async fn replicated_to_global_reuse_reports_completed_service_work() {
    let mut promoted = ployz_test_support::fixtures::serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    promoted.mode = ServiceMode::Replicated {
        replicas: ReplicaCount::try_new(1).expect("replicas"),
    };
    let observation = ployz_core::machine::runtime::MachineContainerObservationSnapshot::try_new(
        machine_id("machine_a"),
        [observed_service_container_with_entry(
            "machine_a",
            "ctr_existing",
            target_namespace_revision_entry_id(),
        )],
    )
    .expect("valid observation");
    let command = global_deploy_command(
        vec![machine_id("machine_a")],
        Vec::new(),
        vec![observation],
        vec![promoted],
    );
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

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
    .expect("mode switch reuses the selected container");

    assert!(runtime.requests.is_empty());
    assert!(
        runtime.image_ensures.is_empty(),
        "existing-only work skips ImageEnsure"
    );
    assert!(recorder.phase_records.iter().any(|evidence| matches!(
        evidence,
        DeployEvidence::PhaseFinished {
            outcome: DeployPhaseOutcome::Promoted,
            services,
            ..
        } if matches!(services.as_slice(), [DeployServiceResult::Completed { .. }])
    )));
}

#[tokio::test]
async fn global_draining_cleanup_reports_completed_service_work() {
    let mut promoted = ployz_test_support::fixtures::serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    promoted.mode = ServiceMode::Global;
    let selected = ployz_core::machine::runtime::MachineContainerObservationSnapshot::try_new(
        machine_id("machine_a"),
        [observed_service_container_with_entry(
            "machine_a",
            "ctr_existing",
            target_namespace_revision_entry_id(),
        )],
    )
    .expect("valid selected observation");
    let draining = ployz_core::machine::runtime::MachineContainerObservationSnapshot::try_new(
        machine_id("machine_draining"),
        [observed_service_container(
            "machine_draining",
            "ctr_draining",
            "entry_old",
        )],
    )
    .expect("valid draining observation");
    let command = global_deploy_command(
        vec![machine_id("machine_a")],
        vec![UnusableMachine {
            machine_id: machine_id("machine_draining"),
            reason: MachineUsabilityReason::Draining,
        }],
        vec![selected, draining],
        vec![promoted],
    );
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers([]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

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
    .expect("draining cleanup completes");

    assert!(runtime.requests.is_empty());
    assert_eq!(runtime.removals.len(), 1);
    assert!(recorder.phase_records.iter().any(|evidence| matches!(
        evidence,
        DeployEvidence::PhaseFinished {
            outcome: DeployPhaseOutcome::Promoted,
            services,
            ..
        } if matches!(services.as_slice(), [DeployServiceResult::Completed { .. }])
    )));
}

#[tokio::test]
async fn deferred_global_candidate_does_not_authorize_cleanup() {
    let mut promoted = ployz_test_support::fixtures::serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    promoted.mode = ServiceMode::Global;
    let observation = ployz_core::machine::runtime::MachineContainerObservationSnapshot::try_new(
        machine_id("machine_silent"),
        [observed_service_container(
            "machine_silent",
            "ctr_old",
            "entry_old",
        )],
    )
    .expect("valid observation");
    let command = global_deploy_command(
        vec![machine_id("machine_a")],
        vec![UnusableMachine {
            machine_id: machine_id("machine_silent"),
            reason: MachineUsabilityReason::FactsUnavailable,
        }],
        vec![observation],
        vec![promoted],
    );
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_global"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

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
    .expect("deferred global deploy completes with warning");

    assert_eq!(runtime.requests.len(), 1);
    assert!(runtime.removals.is_empty());
    assert!(recorder.records.iter().all(|record| !matches!(
        record,
        RecordedOperation::CleanupFinished { removed, .. } if !removed.is_empty()
    )));
}

#[tokio::test]
async fn global_selected_slot_failure_then_distinct_resubmission_converges() {
    let command = global_deploy_command(
        vec![machine_id("machine_a"), machine_id("machine_b")],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::failing_start_after_first(["ctr_a", "ctr_b"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

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
    .expect_err("global slot failure is terminal");

    assert!(namespace_state.phase_requests.is_empty());
    assert_eq!(runtime.stops.len(), 1);
    let [(_, removed)] = runtime.removals.as_slice() else {
        panic!("successful unpromoted slot is removed")
    };
    assert_eq!(removed.container_id, container_id("ctr_a"));
    assert!(matches!(
        error,
        DeployExecutionError::Failed {
            failure,
            ..
        } if matches!(failure.as_ref(),
            DeployOperationFailure::ContainerStartFailed { retained_artifacts, .. }
                if retained_artifacts == &vec![retained_created_container("machine_b", "ctr_b")]
        )
    ));

    let [
        (_, _successful_request),
        (failed_machine_id, failed_request),
    ] = runtime.requests.as_slice()
    else {
        panic!("two selected slots attempted")
    };
    let retained_observation = ployz_core::machine::runtime::ManagedContainerObservation {
        machine_id: failed_machine_id.clone(),
        container_id: container_id("ctr_b"),
        identity: failed_request.container.clone(),
        state: ployz_core::machine::runtime::ContainerRuntimeState::Exited,
        health_status: None,
        resolved_image_identity: None,
        created_at_unix_seconds: None,
        named_volume_names: Default::default(),
    };
    let snapshot = ployz_core::machine::runtime::MachineContainerObservationSnapshot::try_new(
        failed_machine_id.clone(),
        [retained_observation],
    )
    .expect("fresh retained observation");
    let retry_command = global_deploy_command(
        vec![machine_id("machine_a"), machine_id("machine_b")],
        Vec::new(),
        vec![snapshot],
        Vec::new(),
    )
    .with_operation_id(operation_id("op_retry"));
    let mut retry_recorder = RecordingOperations::for_operation(operation_id("op_retry"));
    let mut retry_runtime = RecordingRuntime::with_containers(["ctr_retry_a", "ctr_retry_b"]);
    let mut retry_health = RecordingHealth::healthy();
    let mut retry_namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        retry_command,
        DeployExecutionPorts {
            recorder: &mut retry_recorder,
            machine_runtime: &mut retry_runtime,
            health_checker: &mut retry_health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut retry_namespace_state,
        },
    )
    .await
    .expect("ordinary resubmission converges");

    assert!(matches!(
        retry_runtime.requests.as_slice(),
        [(first_machine, first), (second_machine, second)]
            if first_machine == &machine_id("machine_a")
                && second_machine == &machine_id("machine_b")
                && first.container.operation_id == operation_id("op_retry")
                && second.container.operation_id == operation_id("op_retry")
    ));
    assert_eq!(retry_namespace_state.phase_requests.len(), 1);
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
async fn routed_later_dependency_phase_records_its_own_cutover() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_database", "ctr_web"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        phased_deploy_with_routed_later_phase(),
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

    let running_stages = recorder.records.iter().filter_map(|record| {
        let RecordedOperation::Transition(DeployTransition::Running { stage }) = record else {
            return None;
        };
        Some(*stage)
    });
    assert_eq!(
        running_stages
            .filter(|stage| *stage == DeployRunningStage::RouteCutover)
            .count(),
        1
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::ServingTargetCommit,
        DeployRunningStage::RouteCutover,
    );
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
async fn mixed_platform_pushed_deploy_selects_each_platform_image_and_keeps_one_service_identity() {
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

    let [amd64_request, arm64_request] = runtime.requests.as_slice() else {
        panic!("mixed-platform fixture must run two containers");
    };
    assert_eq!(
        runtime.image_ensures.len(),
        4,
        "each pushed target validates its seed then ensures the target"
    );
    let amd64 = platform("amd64");
    let arm64 = platform("arm64");
    assert!(
        matches!(&runtime.image_ensures[0], (machine, ployz_core::image::ImageEnsureRequest::Start { source: ployz_core::image::ImageEnsureSource::LocalSeed { platform, .. }, .. }) if machine == &machine_id("machine_seed") && platform == &amd64)
    );
    assert!(
        matches!(&runtime.image_ensures[1], (machine, ployz_core::image::ImageEnsureRequest::Start { source: ployz_core::image::ImageEnsureSource::LocalSeed { platform, .. }, .. }) if machine == &machine_id("machine_arm_seed") && platform == &arm64)
    );
    assert!(
        matches!(&runtime.image_ensures[2], (machine, ployz_core::image::ImageEnsureRequest::Start { source: ployz_core::image::ImageEnsureSource::MeshSeed { platform, .. }, .. }) if machine == &machine_id("machine_a") && platform == &amd64)
    );
    assert!(
        matches!(&runtime.image_ensures[3], (machine, ployz_core::image::ImageEnsureRequest::Start { source: ployz_core::image::ImageEnsureSource::MeshSeed { platform, .. }, .. }) if machine == &machine_id("machine_b") && platform == &arm64)
    );
    assert_eq!(amd64_request.0, machine_id("machine_a"));
    assert!(matches!(
        &amd64_request.1.image,
        reference if reference.as_str().starts_with("10.198.99.254:5000/") && reference.as_str().ends_with(&format!("@sha256:{}", "a".repeat(64)))
    ));
    assert_eq!(arm64_request.0, machine_id("machine_b"));
    assert!(matches!(
        &arm64_request.1.image,
        reference if reference.as_str().starts_with("10.198.98.254:5000/") && reference.as_str().ends_with(&format!("@sha256:{}", "d".repeat(64)))
    ));
    assert_eq!(
        amd64_request.1.container.namespace_revision_entry_id,
        arm64_request.1.container.namespace_revision_entry_id
    );
    assert_eq!(
        recorder
            .records
            .iter()
            .filter(|record| **record == RecordedOperation::ImageAvailabilityVerified)
            .count(),
        2
    );
}

#[tokio::test]
async fn pushed_receipt_places_only_on_machines_with_a_covered_platform() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        amd64_pushed_deploy_command([
            (machine_id("machine_a"), platform("amd64")),
            (machine_id("machine_b"), platform("arm64")),
        ]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("amd64 receipt deploys to the compatible machine");

    assert_eq!(
        runtime
            .requests
            .iter()
            .map(|(machine_id, _)| machine_id.clone())
            .collect::<Vec<_>>(),
        [machine_id("machine_a")]
    );
}

#[tokio::test]
async fn pushed_receipt_without_a_compatible_machine_fails_before_effects() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["unused"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let required = platform("amd64");
    let reported = platform("arm64");

    let error = execute_deploy(
        amd64_pushed_deploy_command([(machine_id("machine_b"), reported.clone())]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("receipt without a compatible target fails deploy");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("expected recorded deploy failure");
    };
    assert_eq!(
        *failure,
        DeployOperationFailure::NoUsableMachines {
            reasons: vec![ployz_core::operation::UnusableMachine {
                machine_id: machine_id("machine_b"),
                reason: ployz_core::machine::MachineUsabilityReason::PlatformMismatch {
                    supported: ployz_core::build::BuildPlatforms::try_new([required])
                        .expect("one supported platform"),
                    reported,
                },
            }],
        }
    );
    assert!(runtime.requests.is_empty());
    assert!(runtime.image_ensures.is_empty());
    assert!(runtime.volume_ensures.is_empty());
    assert!(
        !recorder
            .records
            .iter()
            .any(|record| matches!(record, RecordedOperation::PlanCreated { .. }))
    );
}

#[tokio::test]
async fn seed_clock_ahead_of_control_fails_before_image_ensure_rpc() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = route_less_pushed_deploy_command(1).with_seed_clock_testimony(
        machine_id("machine_seed"),
        MachineClockTestimony {
            control_request_started_at_unix_ms: 1_000_000,
            machine_observed_at_unix_ms: 1_300_001,
        },
    );

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
    .expect_err("future seed clock rejects pushed-image deploy");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("expected recorded deploy failure");
    };
    assert!(matches!(
        *failure,
        DeployOperationFailure::SeedUnavailable { message, .. }
            if message.as_str() == "image seed clock is more than 300 seconds ahead of Control"
    ));
    assert!(runtime.image_ensures.is_empty());
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
    let pin_committed = Arc::new(AtomicBool::new(false));
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"])
        .requiring_pin_commit(Arc::clone(&pin_committed));
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::signaling_pin_commit(pin_committed);
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
        vec![VolumePinState::plain(
            namespace_id("default"),
            volume_name("postgres_data"),
            machine_id("machine_a"),
        )]
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
    assert_eq!(
        runtime.volume_ensures,
        vec![(
            machine_id("machine_a"),
            VolumePinState::plain(
                namespace_id("default"),
                volume_name("postgres_data"),
                machine_id("machine_a"),
            ),
        )]
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::EnsuringVolumes,
        DeployRunningStage::StartingContainers,
    );
    assert_eq!(
        recorder
            .records
            .iter()
            .filter(|record| {
                record
                    == &&RecordedOperation::Transition(DeployTransition::Running {
                        stage: DeployRunningStage::EnsuringVolumes,
                    })
            })
            .count(),
        1
    );
}

#[tokio::test]
async fn deploy_volume_ensure_failure_retains_pin_and_retry_reissues_effect_before_containers() {
    let (first_command, volume) = provisioned_volume_backed_deploy_command(false);
    let dataset = match volume.kind() {
        ployz_core::intent::VolumeKind::Provisioned { dataset, .. } => dataset.clone(),
        ployz_core::intent::VolumeKind::Plain => panic!("fixture is provisioned"),
    };
    let failure = ployz_core::machine::VolumeEnsureFailure::Dataset {
        dataset,
        failure: ployz_core::storage::StorageEffectFailure::Dataset {
            message: "synthetic dataset create failure".to_owned(),
        },
    };
    let mut first_recorder = RecordingOperations::default();
    let mut first_runtime = RecordingRuntime::with_containers(["unused"])
        .with_volume_ensure_failure(MachineVolumeEnsureError::Domain {
            machine_id: machine_id("machine_a"),
            volume_name: volume_name("postgres_data"),
            failure: failure.clone(),
        });
    let mut first_namespace_state = RecordingNamespaceState::stored();
    let error = execute_deploy(
        first_command,
        DeployExecutionPorts {
            recorder: &mut first_recorder,
            machine_runtime: &mut first_runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut first_namespace_state,
        },
    )
    .await
    .expect_err("volume ensure failure fails deploy");
    let DeployExecutionError::Failed {
        failure: operation_failure,
        ..
    } = error
    else {
        panic!("expected recorded deploy failure");
    };
    assert_eq!(
        *operation_failure,
        DeployOperationFailure::VolumeEnsureFailed {
            machine_id: machine_id("machine_a"),
            volume_name: volume_name("postgres_data"),
            failure,
        }
    );
    assert_eq!(
        first_namespace_state.volume_pin_requests,
        vec![volume.clone()]
    );
    assert_eq!(
        first_runtime.volume_ensures,
        vec![(machine_id("machine_a"), volume.clone())]
    );
    assert!(first_runtime.requests.is_empty());

    let mut retry_recorder = RecordingOperations::default();
    let mut retry_runtime = RecordingRuntime::with_containers(["ctr_retry"]);
    let mut retry_namespace_state = RecordingNamespaceState::stored();
    let (retry_command, retry_pin) = provisioned_volume_backed_deploy_command(true);
    assert_eq!(retry_pin, volume);
    execute_deploy(
        retry_command,
        DeployExecutionPorts {
            recorder: &mut retry_recorder,
            machine_runtime: &mut retry_runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut retry_namespace_state,
        },
    )
    .await
    .expect("retry converges from committed pin and missing machine effect");
    assert!(retry_namespace_state.volume_pin_requests.is_empty());
    assert_eq!(
        retry_runtime.volume_ensures,
        vec![(machine_id("machine_a"), volume)]
    );
    assert_eq!(retry_runtime.requests.len(), 1);
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
            RecordedOperation::ImageAvailabilityVerified,
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
            crate::roles::machine::protocol::MachineContainerRemoveRpcRequest {
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
                images: Vec::new(),
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
                images: Vec::new(),
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
async fn deploy_worker_reclaims_only_the_image_of_successfully_removed_superseded_container() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();
    let command = deploy_command_replacing_old_container_with_keep("machine_b", "ctr_old");

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
    .expect("deploy succeeds");

    assert_eq!(
        outcome.completion_outcome(),
        DeployCompletionOutcome::Completed
    );
    let expected = ployz_core::operation::DeployImageCleanup::RetainedInUse {
        machine_id: machine_id("machine_b"),
        service_id: service_id("svc_api"),
        image_identity: ployz_core::image::OciDigest::sha256(b"old image"),
    };
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::CleanupFinished { images, .. }
            if images.as_slice() == std::slice::from_ref(&expected)
    )));
}

#[tokio::test]
async fn cleanup_dedupes_shared_image_removal_and_preserves_service_and_identity_evidence() {
    let image_identity = ployz_core::image::OciDigest::sha256(b"shared image");
    let api = cleanup_container("machine_b", "ctr_api", "entry_old");
    let mut worker = cleanup_container("machine_b", "ctr_worker", "entry_old");
    worker.identity.service_id = service_id("svc_worker");
    let invalid = cleanup_container("machine_b", "ctr_invalid", "entry_old");
    let actions = vec![
        ployz_core::deploy::DeployCleanupAction::RemoveContainerAndReclaimImage {
            target: api.clone(),
            image_identity: image_identity.clone(),
        },
        ployz_core::deploy::DeployCleanupAction::RemoveContainerAndReclaimImage {
            target: worker.clone(),
            image_identity: image_identity.clone(),
        },
        ployz_core::deploy::DeployCleanupAction::RemoveContainerWithInvalidImageIdentity {
            target: invalid.clone(),
            observed_identity: Some("not-a-digest".to_owned()),
        },
    ];
    let mut runtime = RecordingRuntime::with_containers([]);

    let (cleanup, evidence) = crate::control::operations::deploy::execute_cleanup_actions(
        &operation_id("op_123"),
        std::time::Duration::from_secs(5),
        &mut runtime,
        &actions,
    )
    .await;

    assert_eq!(cleanup.len(), 3);
    let [removal] = runtime.image_removals.as_slice() else {
        panic!("one shared image removal must be executed");
    };
    assert_eq!(removal.0, machine_id("machine_b"));
    assert_eq!(removal.1.image_identity, image_identity);
    assert!(
        evidence.contains(&ployz_core::operation::DeployImageCleanup::RetainedInUse {
            machine_id: machine_id("machine_b"),
            service_id: service_id("svc_api"),
            image_identity: image_identity.clone(),
        })
    );
    assert!(
        evidence.contains(&ployz_core::operation::DeployImageCleanup::RetainedInUse {
            machine_id: machine_id("machine_b"),
            service_id: service_id("svc_worker"),
            image_identity,
        })
    );
    assert!(evidence.contains(
        &ployz_core::operation::DeployImageCleanup::MissingIdentity {
            machine_id: machine_id("machine_b"),
            service_id: service_id("svc_api"),
            container_id: container_id("ctr_invalid"),
            observed_identity: Some("not-a-digest".to_owned()),
        }
    ));
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
            crate::roles::machine::protocol::MachineContainerRemoveRpcRequest {
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
                images: Vec::new(),
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
        RecordedOperation::CleanupFinished { removed, failed, .. }
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
            vec![crate::certificate::GatewayCertificateTarget {
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
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::RouteCutover,
        DeployRunningStage::ServingTargetCommit,
    );
}

#[tokio::test]
async fn removal_only_phase_records_route_cutover() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        unrouted_deploy_removing_route_command(1),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("route removal succeeds");

    let [(_, phase_route_removals, _)] = namespace_state.phase_requests.as_slice() else {
        panic!("expected one phase commit");
    };
    assert_eq!(
        phase_route_removals,
        &vec![route_target("old.example.com", 443)]
    );
    assert_deploy_event_order(
        &recorder.records,
        DeployRunningStage::RouteCutover,
        DeployRunningStage::ServingTargetCommit,
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
        CertificateProvisionFailure::CoreInterrupted {
            cause: OperationInterruptionCause::PriorCoreProcessLoss,
            last_durable_stage: CertInterruptionStage::Accepted,
            next_action: CertificateInterruptionNextAction::RetryFromCurrentIntent,
        },
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

    let command = ployz_automatic_deploy_command();
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
    .expect("managed HTTPS and plain HTTP routes deploy");

    assert_eq!(certificates.ployz_wildcard_requests, 1);
    assert_eq!(certificates.ployz_operation_ids, [operation_id("op_123")]);
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
            RecordedOperation::ImageAvailabilityVerified,
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
async fn deploy_worker_awaits_slow_active_commit_beyond_the_machine_step_budget() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_1"]);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state =
        RecordingNamespaceState::slow_serving_commits(Duration::from_millis(10));
    let command = deploy_command(1).with_step_timeout(Duration::from_millis(1));

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
    .expect("the definitive namespace transaction outlives machine step budgets");

    assert_eq!(
        outcome.completion_outcome,
        DeployCompletionOutcome::Completed
    );
    assert_eq!(namespace_state.phase_requests.len(), 1);
    assert!(runtime.stops.is_empty());
    assert!(runtime.removals.is_empty());
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
        DeployExecutionError::CommitNamespaceState(error)
            if matches!(
                *error,
                crate::control::operations::deploy::NamespaceCommitError::ServingTargetLockLost { .. }
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

#[tokio::test]
async fn volume_handoff_checks_every_planned_owner_before_start_and_removes_after_promotion() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"])
        .with_stop_outcome(
            "ctr_old_stopped",
            MachineContainerStopOutcome::AlreadyStopped,
        )
        .with_stop_outcome("ctr_old_missing", MachineContainerStopOutcome::Missing);
    let mut health = RecordingHealth::healthy();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        volume_backed_replacement_command(&[
            ("ctr_old_running", true),
            ("ctr_old_stopped", false),
            ("ctr_old_missing", false),
        ]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect("volume replacement succeeds");

    assert_eq!(
        runtime.actions.get(..4),
        Some(
            &[
                RuntimeAction::Stop(container_id("ctr_old_missing")),
                RuntimeAction::Stop(container_id("ctr_old_running")),
                RuntimeAction::Stop(container_id("ctr_old_stopped")),
                RuntimeAction::Run(container_id("ctr_new")),
            ][..]
        )
    );
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffApplied { handoff }
            if handoff.superseded.as_slice().iter().any(|participant|
                participant.target.container_id == container_id("ctr_old_running")
                    && participant.stop_outcome == ployz_core::deploy::DeployVolumeHandoffStopOutcome::StoppedRunning)
                && handoff.superseded.as_slice().iter().any(|participant|
                    participant.target.container_id == container_id("ctr_old_stopped")
                        && participant.stop_outcome == ployz_core::deploy::DeployVolumeHandoffStopOutcome::AlreadyStopped)
                && handoff.superseded.as_slice().iter().any(|participant|
                    participant.target.container_id == container_id("ctr_old_missing")
                        && participant.stop_outcome == ployz_core::deploy::DeployVolumeHandoffStopOutcome::Missing)
    )));
    assert!(runtime.restarts.is_empty());
    let promoted = recorder.phase_records.iter().any(|evidence| {
        matches!(
            evidence,
            DeployEvidence::PhaseFinished {
                outcome: DeployPhaseOutcome::Promoted,
                ..
            }
        )
    });
    assert!(promoted);
    assert!(
        runtime
            .removals
            .iter()
            .any(|(_, request)| { request.container_id == container_id("ctr_old_running") })
    );
}

#[tokio::test]
async fn volume_handoff_health_failure_quiesces_new_consumer_then_restarts_old_owner() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::unhealthy("machine_a", "ctr_new");
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("unhealthy replacement fails");

    assert_eq!(
        runtime.actions.get(..4),
        Some(
            &[
                RuntimeAction::Stop(container_id("ctr_old")),
                RuntimeAction::Run(container_id("ctr_new")),
                RuntimeAction::Stop(container_id("ctr_new")),
                RuntimeAction::Restart(container_id("ctr_old")),
            ][..]
        )
    );
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome == DeployVolumeHandoffRollbackOutcome::Restarted)
    )));
}

#[tokio::test]
async fn volume_handoff_cleans_ordinary_siblings_before_restarting_the_old_owner() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new_api", "ctr_new_worker"]);

    execute_deploy(
        volume_and_ordinary_replacement_command(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::unhealthy("machine_a", "ctr_new_api"),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("failed phase rolls back the volume handoff");

    let ordinary_remove = runtime
        .actions
        .iter()
        .position(|action| action == &RuntimeAction::Remove(container_id("ctr_new_worker")))
        .expect("ordinary sibling is removed");
    let old_restart = runtime
        .actions
        .iter()
        .position(|action| action == &RuntimeAction::Restart(container_id("ctr_old_api")))
        .expect("old owner is restarted");
    assert!(ordinary_remove < old_restart);
    assert!(
        !runtime
            .removals
            .iter()
            .any(|(_, request)| { request.container_id == container_id("ctr_new_api") })
    );
}

#[tokio::test]
async fn volume_handoff_commit_failure_rolls_back_but_post_commit_evidence_failure_does_not() {
    let command = volume_backed_replacement_command(&[("ctr_old", true)]);
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    let mut health = RecordingHealth::healthy();
    execute_deploy(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut health,
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::lost_lock_serving_commits(),
        },
    )
    .await
    .expect_err("failed promotion rolls back");
    assert_eq!(runtime.restarts.len(), 1);

    let mut recorder = RecordingOperations::fail_phase_finished_evidence_times(1);
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("post-commit evidence failure remains a deploy failure");
    assert!(runtime.restarts.is_empty());
}

#[tokio::test]
async fn volume_handoff_restart_failure_is_typed_and_pre_stopped_owner_is_never_restarted() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"])
        .with_stop_outcome(
            "ctr_pre_stopped",
            MachineContainerStopOutcome::AlreadyStopped,
        )
        .with_restart_failure();
    let error = execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true), ("ctr_pre_stopped", false)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::unhealthy("machine_a", "ctr_new"),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("health failure exposes rollback result");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy failure")
    };

    assert_eq!(runtime.restarts.len(), 1);
    assert_eq!(
        runtime
            .restarts
            .first()
            .map(|(_, request)| &request.container_id),
        Some(&container_id("ctr_old"))
    );
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if matches!(outcome.outcome,
                    DeployVolumeHandoffRollbackOutcome::RestartFailed {
                        failure: DeployVolumeHandoffRestartFailure::StartFailed { .. }
                    }))
    )));
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        RetainedArtifact::VolumeOwnerRestorationUnconfirmed {
            target,
            reason: ployz_core::operation::DeployVolumeHandoffRestorationUnconfirmed::RestartFailed {
                failure: DeployVolumeHandoffRestartFailure::StartFailed { .. },
            },
        } if target.container_id == container_id("ctr_old")
    )));
}

#[tokio::test]
async fn volume_handoff_restarts_a_planning_stopped_owner_found_running_at_point_of_use() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);

    execute_deploy(
        volume_backed_replacement_command(&[("ctr_raced_running", false)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::unhealthy("machine_a", "ctr_new"),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("replacement failure restores point-of-use running owner");

    let [(_, restarted)] = runtime.restarts.as_slice() else {
        panic!("expected exactly one restart: {:?}", runtime.restarts);
    };
    assert_eq!(restarted.container_id, container_id("ctr_raced_running"));
}

#[tokio::test]
async fn volume_handoff_stop_timeout_never_restarts_even_when_delivery_completes_late() {
    let mut recorder = RecordingOperations::default();
    let late_completion = Arc::new(tokio::sync::Notify::new());
    let mut runtime = RecordingRuntime::with_containers(["unused"])
        .with_hanging_stop_completing_late("ctr_old", Arc::clone(&late_completion));

    let error = execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)])
            .with_step_timeout(Duration::from_millis(1)),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("indeterminate owner stop fails before a consumer starts");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy failure")
    };
    assert!(runtime.requests.is_empty());
    late_completion.notified().await;
    assert!(runtime.restarts.is_empty());
    assert!(!recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if outcomes.iter().any(|outcome|
                outcome.outcome == DeployVolumeHandoffRollbackOutcome::Restarted)
    )));
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        ployz_core::operation::RetainedArtifact::VolumeOwnerStopUncertain {
            prior_state: ployz_core::deploy::DeployVolumeHandoffPriorState::Running,
            uncertainty: ployz_core::operation::DeployVolumeHandoffStopUncertain::TimedOut { .. },
            ..
        }
    )));
    assert!(
        !failure.retained_artifacts().iter().any(|artifact| matches!(
            artifact,
            RetainedArtifact::VolumeOwnerRestorationUnconfirmed { .. }
        ))
    );
}

#[tokio::test]
async fn volume_handoff_stop_unavailable_after_delivery_never_restarts_after_late_completion() {
    let mut recorder = RecordingOperations::default();
    let late_completion = Arc::new(tokio::sync::Notify::new());
    let mut runtime = RecordingRuntime::with_containers(["unused"])
        .with_stop_unavailable_after_delivery("ctr_old", Arc::clone(&late_completion));

    let error = execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("lost stop response fails before a consumer starts");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy failure")
    };
    late_completion.notified().await;
    assert!(runtime.requests.is_empty());
    assert!(runtime.restarts.is_empty());
    assert!(!recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if outcomes.iter().any(|outcome|
                outcome.outcome == DeployVolumeHandoffRollbackOutcome::Restarted)
    )));
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        RetainedArtifact::VolumeOwnerStopUncertain {
            prior_state: ployz_core::deploy::DeployVolumeHandoffPriorState::Running,
            uncertainty:
                ployz_core::operation::DeployVolumeHandoffStopUncertain::RuntimeUnavailable { .. },
            ..
        }
    )));
    assert!(
        !failure.retained_artifacts().iter().any(|artifact| matches!(
            artifact,
            RetainedArtifact::VolumeOwnerRestorationUnconfirmed { .. }
        ))
    );
}

#[tokio::test]
async fn volume_handoff_stop_timeout_never_restarts_a_planning_stopped_owner() {
    let mut recorder = RecordingOperations::default();
    let mut runtime =
        RecordingRuntime::with_containers(["unused"]).with_hanging_stop_for("ctr_old");

    let error = execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", false)])
            .with_step_timeout(Duration::from_millis(1)),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("indeterminate planning-stopped owner remains retained");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy failure")
    };
    assert!(runtime.restarts.is_empty());
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        ployz_core::operation::RetainedArtifact::VolumeOwnerStopUncertain {
            prior_state: ployz_core::deploy::DeployVolumeHandoffPriorState::Stopped,
            uncertainty: ployz_core::operation::DeployVolumeHandoffStopUncertain::TimedOut { .. },
            ..
        }
    )));
}

#[tokio::test]
async fn volume_handoff_partial_owner_stop_failure_restarts_only_confirmed_stopped_owner() {
    let mut recorder = RecordingOperations::default();
    let mut runtime =
        RecordingRuntime::with_containers(["ctr_new"]).with_stop_failure_for("ctr_old_2");
    let error = execute_deploy(
        volume_backed_replacement_command(&[("ctr_old_1", true), ("ctr_old_2", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("second owner stop fails");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy failure")
    };

    assert!(runtime.requests.is_empty());
    assert_eq!(
        runtime
            .restarts
            .iter()
            .map(|(_, request)| request.container_id.clone())
            .collect::<Vec<_>>(),
        [container_id("ctr_old_1")]
    );
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.target.container_id == container_id("ctr_old_1")
                    && outcome.outcome == DeployVolumeHandoffRollbackOutcome::Restarted)
    )));
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        RetainedArtifact::VolumeOwnerStopUncertain {
            target,
            uncertainty: ployz_core::operation::DeployVolumeHandoffStopUncertain::StopFailed { .. },
            ..
        } if target.container_id == container_id("ctr_old_2")
    )));
}

#[tokio::test]
async fn volume_handoff_quiescence_failure_never_restarts_an_old_owner() {
    let mut recorder = RecordingOperations::default();
    let mut runtime =
        RecordingRuntime::with_containers(["ctr_new"]).with_stop_failure_for("ctr_new");
    let mut namespace_state = RecordingNamespaceState::stored();
    let error = execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::unhealthy("machine_a", "ctr_new"),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("new consumer cannot be quiesced");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy failure")
    };
    assert!(runtime.restarts.is_empty());
    assert!(namespace_state.phase_requests.is_empty());
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome
                    == DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed)
    )));
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        ployz_core::operation::RetainedArtifact::VolumeConsumerQuiescenceUncertain {
            target,
            ..
        } if target.container_id == container_id("ctr_new")
    )));
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        RetainedArtifact::VolumeOwnerRestorationUnconfirmed {
            target,
            reason: ployz_core::operation::DeployVolumeHandoffRestorationUnconfirmed::NewConsumerQuiescenceUnconfirmed,
        } if target.container_id == container_id("ctr_old")
    )));
}

#[tokio::test]
async fn volume_handoff_quiescence_uncertainty_blocks_only_its_service_restart() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new_api", "ctr_new_worker"])
        .with_stop_failure_for("ctr_new_api");

    execute_deploy(
        two_service_volume_backed_replacement_command(),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::unhealthy("machine_a", "ctr_new_api"),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("one service consumer cannot be quiesced");

    let [(_, restarted)] = runtime.restarts.as_slice() else {
        panic!("expected exactly one restart: {:?}", runtime.restarts);
    };
    assert_eq!(restarted.container_id, container_id("ctr_old_worker"));
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if outcomes.iter().any(|outcome|
                outcome.target.container_id == container_id("ctr_old_api")
                    && outcome.outcome
                        == DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed)
    )));
}

#[tokio::test]
async fn volume_handoff_ambiguity_quiesces_every_known_consumer_before_restarting_owner() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["unused"])
        .with_run_ambiguity(["ctr_new_1", "ctr_new_2"]);
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("ambiguous replacement run fails");

    assert_eq!(
        runtime.actions,
        vec![
            RuntimeAction::Stop(container_id("ctr_old")),
            RuntimeAction::Stop(container_id("ctr_new_1")),
            RuntimeAction::Stop(container_id("ctr_new_2")),
            RuntimeAction::Restart(container_id("ctr_old")),
        ]
    );
    assert!(namespace_state.phase_requests.is_empty());
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome == DeployVolumeHandoffRollbackOutcome::Restarted)
    )));
}

#[tokio::test]
async fn volume_handoff_empty_ambiguity_does_not_restart_without_quiescence_evidence() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["unused"]).with_run_ambiguity([]);
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("empty ambiguity leaves replacement state unknown");

    assert_eq!(
        runtime.actions,
        vec![RuntimeAction::Stop(container_id("ctr_old"))]
    );
    assert!(runtime.restarts.is_empty());
    assert!(namespace_state.phase_requests.is_empty());
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome
                    == DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed)
    )));
}

#[tokio::test]
async fn volume_handoff_runtime_unavailable_does_not_restart_without_quiescence_evidence() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["unused"]).with_run_unavailable();
    let mut namespace_state = RecordingNamespaceState::stored();

    execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("unavailable runtime leaves replacement state unknown");

    assert_eq!(
        runtime.actions,
        vec![RuntimeAction::Stop(container_id("ctr_old"))]
    );
    assert!(runtime.restarts.is_empty());
    assert!(namespace_state.phase_requests.is_empty());
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome
                    == DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed)
    )));
}

#[tokio::test]
async fn volume_handoff_run_timeout_does_not_restart_without_quiescence_evidence() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["unused"]).with_hanging_run();
    let mut namespace_state = RecordingNamespaceState::stored();

    let error = execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)])
            .with_step_timeout(Duration::from_millis(1)),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("replacement run timeout leaves replacement state unknown");

    let DeployExecutionError::Failed { failure, .. } = error else {
        panic!("deploy failure")
    };
    assert_eq!(
        runtime.actions,
        vec![RuntimeAction::Stop(container_id("ctr_old"))]
    );
    assert!(runtime.restarts.is_empty());
    assert!(namespace_state.phase_requests.is_empty());
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome
                    == DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed)
    )));
    assert!(failure.retained_artifacts().iter().any(|artifact| matches!(
        artifact,
        ployz_core::operation::RetainedArtifact::VolumeConsumerStartUncertain {
            expected_identity,
            ..
        } if expected_identity.service_id == service_id("svc_api")
    )));
}

#[tokio::test]
async fn volume_handoff_start_failure_restarts_old_owner_without_quiescing_unstarted_container() {
    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::failing_start("ctr_new");
    let mut namespace_state = RecordingNamespaceState::stored();
    execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .expect_err("replacement start fails");

    assert_eq!(
        runtime.actions,
        vec![
            RuntimeAction::Stop(container_id("ctr_old")),
            RuntimeAction::Run(container_id("ctr_new")),
            RuntimeAction::Restart(container_id("ctr_old")),
        ]
    );
    assert!(namespace_state.phase_requests.is_empty());
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome == DeployVolumeHandoffRollbackOutcome::Restarted)
    )));
}

#[tokio::test]
async fn volume_handoff_hook_exit_restarts_old_owner_but_ambiguous_hook_failure_does_not() {
    let mut recorder = RecordingOperations::default();
    let mut runtime =
        RecordingRuntime::with_containers(["ctr_new"]).with_hook_outcome("ctr_hook", 1);
    execute_deploy(
        volume_backed_replacement_command_with_hook(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("failed hook exits before starting the service");
    assert_eq!(runtime.restarts.len(), 1);
    assert!(runtime.requests.is_empty());

    let mut recorder = RecordingOperations::default();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    execute_deploy(
        volume_backed_replacement_command_with_hook(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("missing hook response leaves consumer state ambiguous");
    assert!(runtime.restarts.is_empty());
    assert!(recorder.records.iter().any(|record| matches!(
        record,
        RecordedOperation::VolumeHandoffRollbackFinished { outcomes }
            if matches!(outcomes.as_slice(), [outcome]
                if outcome.outcome
                    == DeployVolumeHandoffRollbackOutcome::NotRestartedNewConsumerQuiescenceUnconfirmed)
    )));
}

#[tokio::test]
async fn volume_handoff_applied_evidence_failure_rolls_back_before_any_new_consumer_runs() {
    let mut recorder = RecordingOperations::fail_handoff_applied_evidence_once();
    let mut runtime = RecordingRuntime::with_containers(["ctr_new"]);
    execute_deploy(
        volume_backed_replacement_command(&[("ctr_old", true)]),
        DeployExecutionPorts {
            recorder: &mut recorder,
            machine_runtime: &mut runtime,
            health_checker: &mut RecordingHealth::healthy(),
            certificate_provisioner: &mut RecordingCertificates::successful(),
            namespace_state: &mut RecordingNamespaceState::stored(),
        },
    )
    .await
    .expect_err("handoff evidence is required before starting the replacement");

    assert_eq!(
        runtime.actions,
        vec![
            RuntimeAction::Stop(container_id("ctr_old")),
            RuntimeAction::Restart(container_id("ctr_old")),
        ]
    );
    assert!(runtime.requests.is_empty());
}
