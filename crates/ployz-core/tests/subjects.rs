use ployz_core::ids::{CertId, MachineId, OperationId, SubjectTokenError};
use ployz_core::machine::MachineAddFailure;
use ployz_core::ops::{
    CancellationReason, DeployCompletionOutcome, DeployRunningStage, MachineSubstrateVersions,
    OperationEvent,
};
use ployz_core::state::MachineLifecycle;
use ployz_core::subjects::{
    MachineServiceEndpoint, OperationProgressScope, machine_container_facts, machine_facts,
    machine_service, operation_progress_watch,
};
use ployz_test_support::ids::{container_id, machine_id, namespace_id, operation_id};

/// Operation progress subject literals are externally observed contracts.
#[test]
fn operation_event_subjects_are_pinned() {
    let op_id = operation_id("op_123");
    let deploy_scope = OperationProgressScope::Namespace {
        namespace_id: namespace_id("default"),
    };

    assert_eq!(
        operation_progress_watch(&deploy_scope, &op_id),
        "plz.v1.progress.namespace.default.operation.op_123.>"
    );
    assert_eq!(
        planning_started(&op_id).subject(&deploy_scope),
        "plz.v1.progress.namespace.default.operation.op_123.deploy.planning.started"
    );
    assert_eq!(
        deploy_running(&op_id, DeployRunningStage::ServingTargetCommit).subject(&deploy_scope),
        "plz.v1.progress.namespace.default.operation.op_123.deploy.running.serving_target_commit"
    );
    assert_eq!(
        container_started(&op_id).subject(&deploy_scope),
        "plz.v1.progress.namespace.default.operation.op_123.deploy.container.started.machine_7.ctr_1"
    );
    assert_eq!(
        health_check_started(&op_id).subject(&deploy_scope),
        "plz.v1.progress.namespace.default.operation.op_123.deploy.health_check.started"
    );
    assert_eq!(
        deploy_completed(&op_id).subject(&deploy_scope),
        "plz.v1.progress.namespace.default.operation.op_123.deploy.completed"
    );
    assert_eq!(
        cancelled(&op_id).subject(&deploy_scope),
        "plz.v1.progress.namespace.default.operation.op_123.cancelled"
    );

    let op_id = operation_id("op_machine");
    let machine_scope = OperationProgressScope::Machine {
        machine_id: machine_id("machine_7"),
    };
    assert_eq!(
        machine_add_joined(&op_id).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.add.joined"
    );
    assert_eq!(
        machine_add_completed(&op_id).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.add.completed"
    );
    assert_eq!(
        machine_add_failed(&op_id).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.add.failed"
    );
    assert_eq!(
        machine_update_running(&op_id).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.update.running"
    );
    assert_eq!(
        machine_update_completed(&op_id).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.update.completed"
    );

    assert_eq!(
        lifecycle_submitted(&op_id, MachineLifecycle::Draining).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.lifecycle.drain.submitted"
    );
    assert_eq!(
        lifecycle_submitted(&op_id, MachineLifecycle::Active).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.lifecycle.resume.submitted"
    );
    assert_eq!(
        lifecycle_completed(&op_id).subject(&machine_scope),
        "plz.v1.progress.machine.machine_7.operation.op_machine.machine.lifecycle.completed"
    );

    let op_id = operation_id("op_cert");
    let cluster_scope = OperationProgressScope::Cluster;
    assert_eq!(
        cert_submitted(&op_id).subject(&cluster_scope),
        "plz.v1.progress.cluster.operation.op_cert.cert.submitted"
    );
    assert_eq!(
        cert_validation_started(&op_id).subject(&cluster_scope),
        "plz.v1.progress.cluster.operation.op_cert.cert.validation.started"
    );
}

#[test]
fn machine_subjects_use_known_endpoint_and_event_tokens() {
    let machine_id = MachineId::try_new("machine_7").expect("valid machine id");

    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerRun),
        "plz.v1.rpc.machine.command.machine_7.container.run"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerRunHook),
        "plz.v1.rpc.machine.command.machine_7.container.run_hook"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::FactsGet),
        "plz.v1.rpc.machine.query.machine_7.facts.get"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerStop),
        "plz.v1.rpc.machine.command.machine_7.container.stop"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerRemove),
        "plz.v1.rpc.machine.command.machine_7.container.remove"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::VolumeRemove),
        "plz.v1.rpc.machine.command.machine_7.volume.remove"
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::DataplanePrepare),
        "plz.v1.rpc.machine.command.machine_7.dataplane.prepare"
    );
    assert_eq!(
        machine_facts(&machine_id),
        "plz.v1.testimony.machine.machine_7.snapshot"
    );
    assert_eq!(
        machine_container_facts(&machine_id),
        "plz.v1.testimony.machine.machine_7.containers"
    );
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
            host_runner: None,
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
