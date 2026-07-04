use ployz_core::ids::{CertId, MachineId, OperationId, SubjectTokenError};
use ployz_core::machine::MachineAddFailure;
use ployz_core::ops::{
    CancellationReason, DeployCompletionOutcome, DeployRunningStage, MachineSubstrateVersions,
    OperationEvent,
};
use ployz_core::state::MachineLifecycle;
use ployz_core::subjects::{
    MachineObservationEvent, MachineServiceEndpoint, cert_renewal_job, cert_renewal_schedule,
    machine_observation, machine_service, op_watch,
};
use ployz_test_support::ids::{container_id, machine_id, operation_id};

/// Subjects and message ids are persisted stream contracts. Every literal in
/// this file pins a rendering that must never change for an existing event
/// variant.
#[test]
fn operation_event_subjects_are_pinned() {
    let op_id = operation_id("op_123");

    assert_eq!(op_watch(&op_id), "plz.v1.op.op_123.>");
    assert_eq!(
        planning_started(&op_id).subject(),
        "plz.v1.op.op_123.deploy.planning.started"
    );
    assert_eq!(
        deploy_running(&op_id, DeployRunningStage::ServingTargetCommit).subject(),
        "plz.v1.op.op_123.deploy.running.serving_target_commit"
    );
    assert_eq!(
        container_started(&op_id).subject(),
        "plz.v1.op.op_123.deploy.container.started.machine_7.ctr_1"
    );
    assert_eq!(
        health_check_started(&op_id).subject(),
        "plz.v1.op.op_123.deploy.health_check.started"
    );
    assert_eq!(
        deploy_completed(&op_id).subject(),
        "plz.v1.op.op_123.deploy.completed"
    );
    assert_eq!(cancelled(&op_id).subject(), "plz.v1.op.op_123.cancelled");

    let op_id = operation_id("op_machine");
    assert_eq!(
        machine_add_joined(&op_id).subject(),
        "plz.v1.op.op_machine.machine.add.joined"
    );
    assert_eq!(
        machine_add_completed(&op_id).subject(),
        "plz.v1.op.op_machine.machine.add.completed"
    );
    assert_eq!(
        machine_add_failed(&op_id).subject(),
        "plz.v1.op.op_machine.machine.add.failed"
    );
    assert_eq!(
        machine_update_running(&op_id).subject(),
        "plz.v1.op.op_machine.machine.update.running"
    );
    assert_eq!(
        machine_update_completed(&op_id).subject(),
        "plz.v1.op.op_machine.machine.update.completed"
    );

    assert_eq!(
        lifecycle_submitted(&op_id, MachineLifecycle::Draining).subject(),
        "plz.v1.op.op_machine.machine.lifecycle.drain.submitted"
    );
    assert_eq!(
        lifecycle_submitted(&op_id, MachineLifecycle::Active).subject(),
        "plz.v1.op.op_machine.machine.lifecycle.resume.submitted"
    );
    assert_eq!(
        lifecycle_completed(&op_id).subject(),
        "plz.v1.op.op_machine.machine.lifecycle.completed"
    );

    let op_id = operation_id("op_cert");
    assert_eq!(
        cert_submitted(&op_id).subject(),
        "plz.v1.op.op_cert.cert.submitted"
    );
    assert_eq!(
        cert_validation_started(&op_id).subject(),
        "plz.v1.op.op_cert.cert.validation.started"
    );
}

#[test]
fn operation_event_message_ids_are_pinned() {
    let op_id = operation_id("op_123");

    assert_eq!(
        cert_submitted(&op_id).message_id(),
        "operation.submit.op_123"
    );
    assert_eq!(
        planning_started(&op_id).message_id(),
        "deploy.event.op_123.planning.started"
    );
    assert_eq!(
        deploy_running(&op_id, DeployRunningStage::StartingContainers).message_id(),
        "deploy.event.op_123.running.starting_containers"
    );
    assert_eq!(
        container_started(&op_id).message_id(),
        "deploy.container.started.op_123.machine_7.ctr_1"
    );
    assert_eq!(
        health_check_started(&op_id).message_id(),
        "deploy.health_check.started.op_123"
    );
    assert_eq!(
        machine_add_joined(&op_id).message_id(),
        "machine.add.joined.op_123"
    );
    assert_eq!(
        machine_update_running(&op_id).message_id(),
        "machine.update.running.op_123"
    );
    assert_eq!(
        cert_validation_started(&op_id).message_id(),
        "cert.validation.started.op_123"
    );
}

/// Every terminal event of one operation kind shares one message id, so
/// JetStream dedup enforces "terminal states are final" at the stream:
/// a second terminal write for the same operation is dropped.
#[test]
fn terminal_events_share_one_message_id_per_operation_kind() {
    let op_id = operation_id("op_123");

    assert_eq!(
        deploy_completed(&op_id).message_id(),
        "deploy.terminal.op_123"
    );
    assert_eq!(
        cancelled(&op_id).message_id(),
        deploy_completed(&op_id).message_id()
    );
    assert_eq!(
        machine_add_completed(&op_id).message_id(),
        "machine.add.terminal.op_123"
    );
    assert_eq!(
        machine_add_failed(&op_id).message_id(),
        machine_add_completed(&op_id).message_id()
    );
    assert_eq!(
        machine_update_completed(&op_id).message_id(),
        "machine.update.terminal.op_123"
    );
    assert_eq!(
        cancelled_with_kind(&op_id, ployz_core::ops::OperationKind::MachineUpdate).message_id(),
        machine_update_completed(&op_id).message_id()
    );
    assert_eq!(
        lifecycle_completed(&op_id).message_id(),
        "machine.lifecycle.terminal.op_123"
    );
    assert_eq!(
        cancelled_with_kind(&op_id, ployz_core::ops::OperationKind::MachineLifecycle).message_id(),
        lifecycle_completed(&op_id).message_id()
    );
    assert_eq!(
        lifecycle_submitted(&op_id, MachineLifecycle::Draining).message_id(),
        "operation.submit.op_123"
    );
}

#[test]
fn machine_subjects_use_known_endpoint_and_event_tokens() {
    let machine_id = MachineId::try_new("machine_7").expect("valid machine id");

    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerRun),
        "plz.v1.svc.machine.machine_7.container.run"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerStop),
        "plz.v1.svc.machine.machine_7.container.stop"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerRemove),
        "plz.v1.svc.machine.machine_7.container.remove"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::DataplanePrepare),
        "plz.v1.svc.machine.machine_7.dataplane.prepare"
    );
    assert_eq!(
        machine_observation(&machine_id, MachineObservationEvent::ContainerRunning),
        "plz.v1.obs.machine.machine_7.container.running"
    );
}

#[test]
fn cert_schedule_subjects_target_the_cert_job_subject() {
    let cert_id = CertId::try_new("cert_api").expect("valid cert id");

    assert_eq!(
        cert_renewal_schedule(&cert_id),
        "plz.v1.sched.cert.renew.cert_api"
    );
    assert_eq!(cert_renewal_job(&cert_id), "plz.v1.job.cert.renew.cert_api");
}

#[test]
fn ids_reject_wildcard_subject_tokens() {
    assert_eq!(
        OperationId::try_new("op.>"),
        Err(SubjectTokenError::InvalidCharacter {
            value: "op.>".to_owned()
        })
    );
}

#[test]
fn ids_use_positive_ascii_token_grammar() {
    assert_eq!(
        OperationId::try_new("op\u{7}123"),
        Err(SubjectTokenError::InvalidCharacter {
            value: "op\u{7}123".to_owned()
        })
    );
    assert_eq!(
        OperationId::try_new("op/123"),
        Err(SubjectTokenError::InvalidCharacter {
            value: "op/123".to_owned()
        })
    );
}

fn planning_started(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::DeployPlanningStarted {
        operation_id: operation_id.clone(),
    }
}

fn deploy_running(operation_id: &OperationId, stage: DeployRunningStage) -> OperationEvent {
    OperationEvent::DeployRunning {
        operation_id: operation_id.clone(),
        stage,
    }
}

fn container_started(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::DeployContainerStarted {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
        container_id: container_id("ctr_1"),
    }
}

fn health_check_started(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::DeployHealthCheckStarted {
        operation_id: operation_id.clone(),
    }
}

fn deploy_completed(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::DeployCompleted {
        operation_id: operation_id.clone(),
        outcome: DeployCompletionOutcome::Completed,
    }
}

fn cancelled(operation_id: &OperationId) -> OperationEvent {
    cancelled_with_kind(operation_id, ployz_core::ops::OperationKind::Deploy)
}

fn cancelled_with_kind(
    operation_id: &OperationId,
    kind: ployz_core::ops::OperationKind,
) -> OperationEvent {
    OperationEvent::Cancelled {
        operation_id: operation_id.clone(),
        kind,
        reason: CancellationReason::try_new("operator cancelled").expect("valid reason"),
    }
}

fn machine_add_joined(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::MachineAddJoined {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
        joined_at: ployz_core::machine::JoinTokenRedeemedAt::try_new(1_700_000_000)
            .expect("valid redeemed-at"),
    }
}

fn machine_add_completed(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::MachineAddCompleted {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
    }
}

fn machine_add_failed(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::MachineAddFailed {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
        failure: MachineAddFailure::InvalidJoinToken,
    }
}

fn machine_update_running(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::MachineUpdateRunning {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
    }
}

fn machine_update_completed(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::MachineUpdateCompleted {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
        reported: MachineSubstrateVersions {
            ployzd: None,
            keeper: None,
        },
    }
}

fn lifecycle_submitted(operation_id: &OperationId, target: MachineLifecycle) -> OperationEvent {
    OperationEvent::MachineLifecycleSubmitted {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
        target,
    }
}

fn lifecycle_completed(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::MachineLifecycleCompleted {
        operation_id: operation_id.clone(),
        machine_id: machine_id("machine_7"),
    }
}

fn cert_submitted(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::CertRenewalSubmitted {
        operation_id: operation_id.clone(),
        cert_id: CertId::try_new("cert_api").expect("valid cert id"),
    }
}

fn cert_validation_started(operation_id: &OperationId) -> OperationEvent {
    OperationEvent::CertValidationStarted {
        operation_id: operation_id.clone(),
        cert_id: CertId::try_new("cert_api").expect("valid cert id"),
    }
}
