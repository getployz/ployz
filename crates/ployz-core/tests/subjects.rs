use ployz_core::ids::{CertId, NodeId, OperationId, SubjectTokenError};
use ployz_core::ops::DeployRunningStage;
use ployz_core::subjects::{
    NodeObservationEvent, NodeServiceEndpoint, cert_renewal_job, cert_renewal_schedule,
    node_observation, node_service, op_cert_challenge_published, op_cert_completed, op_cert_failed,
    op_cert_submitted, op_cert_validation_started, op_deploy_completed,
    op_deploy_container_started, op_deploy_health_check_started, op_deploy_plan_created,
    op_deploy_planning_started, op_deploy_running, op_deploy_submitted,
    op_deploy_wireguard_ebpf_prepared, op_machine_add_completed, op_machine_add_failed,
    op_machine_add_joined, op_machine_add_submitted, op_watch,
};

#[test]
fn operation_subjects_use_validated_operation_ids() {
    let op_id = OperationId::try_new("op_123").expect("valid operation id");

    assert_eq!(op_watch(&op_id), "plz.v1.op.op_123.>");
    assert_eq!(
        op_deploy_submitted(&op_id),
        "plz.v1.op.op_123.deploy.submitted"
    );
    assert_eq!(
        op_deploy_planning_started(&op_id),
        "plz.v1.op.op_123.deploy.planning.started"
    );
    assert_eq!(
        op_deploy_plan_created(&op_id),
        "plz.v1.op.op_123.deploy.plan.created"
    );
    assert_eq!(
        op_deploy_running(&op_id, DeployRunningStage::ActiveServiceCommit),
        "plz.v1.op.op_123.deploy.running.active_service_commit"
    );
    assert_eq!(
        op_deploy_wireguard_ebpf_prepared(&op_id),
        "plz.v1.op.op_123.deploy.wireguard_ebpf.prepared"
    );
    assert_eq!(
        op_deploy_container_started(&op_id, &node_id("node_7"), &container_id("ctr_1")),
        "plz.v1.op.op_123.deploy.container.started.node_7.ctr_1"
    );
    assert_eq!(
        op_deploy_health_check_started(&op_id),
        "plz.v1.op.op_123.deploy.health_check.started"
    );
    assert_eq!(
        op_deploy_completed(&op_id),
        "plz.v1.op.op_123.deploy.completed"
    );
}

#[test]
fn machine_add_operation_subjects_use_validated_operation_ids() {
    let op_id = OperationId::try_new("op_machine").expect("valid operation id");

    assert_eq!(
        op_machine_add_submitted(&op_id),
        "plz.v1.op.op_machine.machine.add.submitted"
    );
    assert_eq!(
        op_machine_add_joined(&op_id),
        "plz.v1.op.op_machine.machine.add.joined"
    );
    assert_eq!(
        op_machine_add_completed(&op_id),
        "plz.v1.op.op_machine.machine.add.completed"
    );
    assert_eq!(
        op_machine_add_failed(&op_id),
        "plz.v1.op.op_machine.machine.add.failed"
    );
}

#[test]
fn node_subjects_use_known_endpoint_and_event_tokens() {
    let node_id = NodeId::try_new("node_7").expect("valid node id");

    assert_eq!(
        node_service(&node_id, NodeServiceEndpoint::ContainerRun),
        "plz.v1.svc.node.node_7.container.run"
    );
    assert_eq!(
        node_service(&node_id, NodeServiceEndpoint::ContainerStop),
        "plz.v1.svc.node.node_7.container.stop"
    );
    assert_eq!(
        node_service(&node_id, NodeServiceEndpoint::ContainerRemove),
        "plz.v1.svc.node.node_7.container.remove"
    );
    assert_eq!(
        node_service(&node_id, NodeServiceEndpoint::WireGuardEbpfPrepare),
        "plz.v1.svc.node.node_7.wireguard_ebpf.prepare"
    );
    assert_eq!(
        node_observation(&node_id, NodeObservationEvent::ContainerRunning),
        "plz.v1.obs.node.node_7.container.running"
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
fn cert_operation_subjects_use_validated_operation_ids() {
    let op_id = OperationId::try_new("op_cert").expect("valid operation id");

    assert_eq!(
        op_cert_submitted(&op_id),
        "plz.v1.op.op_cert.cert.submitted"
    );
    assert_eq!(
        op_cert_challenge_published(&op_id),
        "plz.v1.op.op_cert.cert.challenge.published"
    );
    assert_eq!(
        op_cert_validation_started(&op_id),
        "plz.v1.op.op_cert.cert.validation.started"
    );
    assert_eq!(
        op_cert_completed(&op_id),
        "plz.v1.op.op_cert.cert.completed"
    );
    assert_eq!(op_cert_failed(&op_id), "plz.v1.op.op_cert.cert.failed");
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

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ployz_core::ids::ContainerId {
    ployz_core::ids::ContainerId::try_new(value).expect("valid container id")
}
