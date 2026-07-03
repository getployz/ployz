use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanStep, DeployRequest, DeployServicePlan,
    DeployServiceSpec, ImageReference, ReplicaCount, ReplicaSlot,
};
use ployz_core::ids::OperationId;
use ployz_core::machine::JoinTokenRedeemedAt;
use ployz_core::machine_runtime::ManagedContainerKind;
use ployz_core::ops::{DeployEvidence, DeployRunningStage, DeployTransition, OperationEvent};
use ployz_nats::operations::{OperationEventAppend, operation_status_key};
use ployz_nats::streams::MessageId;
use ployz_test_support::ids::{
    cert_id, container_id, machine_id, namespace_id, namespace_revision_entry_id,
    namespace_revision_id, operation_id, service_id, step_id,
};

#[test]
fn operation_status_key_uses_token_safe_operation_id() {
    let operation_id = OperationId::try_new("op_123").expect("valid operation id");

    assert_eq!(operation_status_key(&operation_id), "ops.op_123");
}

#[test]
fn operation_event_append_carries_nats_message_id() {
    let operation_id = OperationId::try_new("op_123").expect("valid operation id");
    let append = OperationEventAppend::from_event(
        MessageId::new("deploy.submit.idem_1"),
        OperationEvent::DeploySubmitted {
            operation_id,
            target: deploy_target("svc_api"),
        },
    );

    assert_eq!(append.subject(), "plz.v1.op.op_123.deploy.submitted");
    assert_eq!(append.message_id().as_str(), "deploy.submit.idem_1");
}

#[test]
fn deploy_transition_append_uses_stable_small_message_id() {
    let append = OperationEventAppend::deploy_transition(
        &operation_id("op_123"),
        &DeployTransition::Running {
            stage: active_service_running(),
        },
    );

    assert_eq!(
        append.subject(),
        "plz.v1.op.op_123.deploy.running.serving_target_commit"
    );
    assert_eq!(
        append.message_id().as_str(),
        "deploy.event.op_123.running.serving_target_commit"
    );
}

#[test]
fn deploy_container_started_append_uses_stable_message_id() {
    let append = OperationEventAppend::deploy_container_started(
        &operation_id("op_123"),
        &machine_id("machine_a"),
        &container_id("ctr_1"),
    );

    assert_eq!(
        append.subject(),
        "plz.v1.op.op_123.deploy.container.started.machine_a.ctr_1"
    );
    assert_eq!(
        append.message_id().as_str(),
        "deploy.container.started.op_123.machine_a.ctr_1"
    );
    assert_eq!(
        append.payload(),
        &OperationEvent::DeployContainerStarted {
            operation_id: operation_id("op_123"),
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_1"),
        }
    );
}

#[test]
fn deploy_health_check_started_append_uses_stable_message_id() {
    let append = OperationEventAppend::deploy_health_check_started(&operation_id("op_123"));

    assert_eq!(
        append.subject(),
        "plz.v1.op.op_123.deploy.health_check.started"
    );
    assert_eq!(
        append.message_id().as_str(),
        "deploy.health_check.started.op_123"
    );
    assert_eq!(
        append.payload(),
        &OperationEvent::DeployHealthCheckStarted {
            operation_id: operation_id("op_123"),
        }
    );
}

#[test]
fn deploy_cleanup_finished_append_uses_stable_message_id() {
    let removed = vec![cleanup_container("machine_b", "ctr_old")];
    let append = OperationEventAppend::deploy_evidence(
        &operation_id("op_123"),
        &DeployEvidence::CleanupFinished {
            removed: removed.clone(),
            failed: Vec::new(),
        },
    );

    assert_eq!(append.subject(), "plz.v1.op.op_123.deploy.cleanup.finished");
    assert_eq!(
        append.message_id().as_str(),
        "deploy.cleanup.finished.op_123"
    );
    assert_eq!(
        append.payload(),
        &OperationEvent::DeployCleanupFinished {
            operation_id: operation_id("op_123"),
            removed,
            failed: Vec::new(),
        }
    );
}

#[test]
fn deploy_plan_created_append_uses_stable_message_id() {
    let plan = deploy_plan();
    let append = OperationEventAppend::deploy_plan_created(&operation_id("op_123"), &plan);

    assert_eq!(append.subject(), "plz.v1.op.op_123.deploy.plan.created");
    assert_eq!(append.message_id().as_str(), "deploy.plan.created.op_123");
    assert_eq!(
        append.payload(),
        &OperationEvent::DeployPlanCreated {
            operation_id: operation_id("op_123"),
            plan,
        }
    );
}

#[test]
fn cert_submitted_append_uses_stable_message_id() {
    let append = OperationEventAppend::cert_submitted(operation_id("op_cert"), cert_id("cert_api"));

    assert_eq!(append.subject(), "plz.v1.op.op_cert.cert.submitted");
    assert_eq!(append.message_id().as_str(), "operation.submit.op_cert");
    assert_eq!(
        append.payload(),
        &OperationEvent::CertRenewalSubmitted {
            operation_id: operation_id("op_cert"),
            cert_id: cert_id("cert_api"),
        }
    );
}

#[test]
fn machine_add_joined_append_uses_stable_message_id() {
    let append = OperationEventAppend::machine_add_joined(
        &operation_id("op_machine"),
        &machine_id("machine_2"),
        joined_at(50),
    );

    assert_eq!(append.subject(), "plz.v1.op.op_machine.machine.add.joined");
    assert_eq!(
        append.message_id().as_str(),
        "machine.add.joined.op_machine"
    );
    assert_eq!(
        append.payload(),
        &OperationEvent::MachineAddJoined {
            operation_id: operation_id("op_machine"),
            machine_id: machine_id("machine_2"),
            joined_at: joined_at(50),
        }
    );
}

fn joined_at(value: u64) -> JoinTokenRedeemedAt {
    JoinTokenRedeemedAt::try_new(value).expect("valid join time")
}

fn cleanup_container(machine: &str, container: &str) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        service_id: service_id("svc_api"),
        namespace_revision_entry_id: namespace_revision_entry_id("rev_old"),
        operation_id: operation_id("op_existing"),
        step_id: step_id(container),
        kind: ManagedContainerKind::Service,
    }
}

fn deploy_plan() -> DeployPlan {
    DeployPlan {
        namespace_id: namespace_id("default"),
        namespace_revision_id: ployz_core::ids::NamespaceRevisionId::try_new("rev_2").expect("valid revision id"),
        services: vec![DeployServicePlan {
            service_id: service_id("svc_api"),
            steps: vec![DeployPlanStep::RunContainer {
                machine_id: machine_id("machine_a"),
                slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
            }],
        }],
        cleanup_containers: Vec::new(),
    }
}

fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ServingTargetCommit
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id("rev_2"),
        services: vec![DeployServiceSpec {
            service_id: self::service_id(service_id),
            image: image("ghcr.io/acme/api:rev-2"),
            replicas: replicas(1),
            routes: Vec::new(),
        }],
    }
}
