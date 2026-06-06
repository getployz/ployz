use ployz_core::deploy::{
    DeployPlan, DeployPlanStep, DeployRequest, ImageReference, ReplicaCount, ReplicaSlot,
};
use ployz_core::ids::{OperationId, RevisionId, ServiceId};
use ployz_core::ops::{DeployRunningStage, DeployTransition, EventSequence, OperationEvent};
use ployz_nats::operations::{OperationEventAppend, operation_status_key};
use ployz_nats::streams::{MessageId, OperationEventStream};

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
        "plz.v1.op.op_123.deploy.running.active_service_commit"
    );
    assert_eq!(
        append.message_id().as_str(),
        "deploy.event.op_123.running.active_service_commit"
    );
}

#[test]
fn deploy_container_started_append_uses_stable_message_id() {
    let append = OperationEventAppend::deploy_container_started(
        &operation_id("op_123"),
        &node_id("node_a"),
        &container_id("ctr_1"),
    );

    assert_eq!(
        append.subject(),
        "plz.v1.op.op_123.deploy.container.started.node_a.ctr_1"
    );
    assert_eq!(
        append.message_id().as_str(),
        "deploy.container.started.op_123.node_a.ctr_1"
    );
    assert_eq!(
        append.payload(),
        &OperationEvent::DeployContainerStarted {
            operation_id: operation_id("op_123"),
            node_id: node_id("node_a"),
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
fn operation_stream_replays_matching_operation_events_from_start_sequence() {
    let mut stream = OperationEventStream::default();
    stream.append(
        "plz.v1.op.op_123.deploy.submitted",
        MessageId::new("op_123.submitted"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            target: deploy_target("svc_api"),
        },
    );
    stream.append(
        "plz.v1.op.op_456.deploy.submitted",
        MessageId::new("op_456.submitted"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_456"),
            target: deploy_target("svc_worker"),
        },
    );

    let replay = stream.replay("plz.v1.op.op_123.", event_sequence(1));
    let [event] = replay.as_slice() else {
        panic!("expected one replayed event, got {replay:?}");
    };

    assert_eq!(event.sequence, event_sequence(1));
    assert_eq!(
        event.payload,
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            target: deploy_target("svc_api"),
        }
    );
}

#[test]
fn operation_stream_deduplicates_by_message_id() {
    let mut stream = OperationEventStream::default();
    let first = stream.append(
        "plz.v1.op.op_123.deploy.submitted",
        MessageId::new("deploy.submit.idem_1"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            target: deploy_target("svc_api"),
        },
    );
    let duplicate = stream.append(
        "plz.v1.op.op_456.deploy.submitted",
        MessageId::new("deploy.submit.idem_1"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_456"),
            target: deploy_target("svc_worker"),
        },
    );

    assert_eq!(duplicate.sequence(), first.sequence());
    assert_eq!(stream.messages().len(), 1);
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn node_id(value: &str) -> ployz_core::ids::NodeId {
    ployz_core::ids::NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ployz_core::ids::ContainerId {
    ployz_core::ids::ContainerId::try_new(value).expect("valid container id")
}

fn deploy_plan() -> DeployPlan {
    DeployPlan {
        service_id: service_id("svc_api"),
        target_revision: ployz_core::ids::RevisionId::try_new("rev_2").expect("valid revision id"),
        steps: vec![DeployPlanStep::RunContainer {
            node_id: node_id("node_a"),
            slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
        }],
    }
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ActiveServiceCommit
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: self::service_id(service_id),
        target_revision: revision_id("rev_2"),
        image: image("ghcr.io/acme/api:rev-2"),
        replicas: replicas(1),
    }
}
