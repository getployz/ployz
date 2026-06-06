use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::deploy::{
    DeployPlan, DeployPlanStep, DeployRequest, ImageReference, ReplicaCount, ReplicaSlot,
};
use ployz_core::ids::{OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::ops::{
    CancellationReason, DeployOperationFailure, DeployRunningStage, EventSequence, FailureMessage,
    OperationEventReplayLimit, OperationIdempotencyKey, OperationLeaseExpiresAt,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, KV_OPS_BUCKET, OperationLeaseClaim, PLZ_OPS_STREAM,
};

pub(super) struct TestNats {
    _server: nats_server::Server,
    pub(super) jetstream: jetstream::Context,
}

pub(super) async fn test_nats() -> TestNats {
    let server = nats_server::run_server("tests/configs/jetstream.conf");
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let jetstream = jetstream::new(client);
    bootstrap_operation_resources(&jetstream).await;

    TestNats {
        _server: server,
        jetstream,
    }
}

pub(super) async fn bootstrap_operation_resources(jetstream: &jetstream::Context) {
    jetstream
        .create_stream(jetstream::stream::Config {
            name: PLZ_OPS_STREAM.to_owned(),
            subjects: vec!["plz.v1.op.>".to_owned()],
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create PLZ_OPS stream");
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_OPS_BUCKET.to_owned(),
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create KV_OPS bucket");
}

pub(super) async fn operation_repository(
    jetstream: &jetstream::Context,
) -> AsyncNatsOperationRepository {
    AsyncNatsOperationRepository::new(
        AsyncNatsOperationEventLog::new(jetstream.clone()),
        AsyncNatsOperationStatusStore::from_jetstream(jetstream)
            .await
            .expect("open operation status store"),
    )
}

pub(super) fn deploy_submission(
    operation_id: &str,
    idempotency_key: &str,
    service_id: &str,
) -> DeployOperationSubmission {
    DeployOperationSubmission {
        operation_id: self::operation_id(operation_id),
        target: deploy_target(service_id),
        idempotency_key: self::idempotency_key(idempotency_key),
    }
}

pub(super) fn lease_claim(owner_id: &str, now: u64, expires_at: u64) -> OperationLeaseClaim {
    OperationLeaseClaim::try_new(
        OperationOwnerId::try_new(owner_id).expect("valid operation owner id"),
        lease_time(now),
        lease_time(expires_at),
    )
    .expect("valid lease claim")
}

pub(super) fn default_lease_claim() -> OperationLeaseClaim {
    lease_claim("control_a", 100, 160)
}

pub(super) fn deploy_plan() -> DeployPlan {
    deploy_plan_on("node_a")
}

pub(super) fn deploy_plan_on(node: &str) -> DeployPlan {
    DeployPlan {
        service_id: service_id("svc_api"),
        target_revision: ployz_core::ids::RevisionId::try_new("rev_2").expect("valid revision id"),
        steps: vec![DeployPlanStep::RunContainer {
            node_id: node_id(node),
            slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
        }],
    }
}

pub(super) fn planning_failure(message: &str) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        message: failure_message(message),
    }
}

pub(super) fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

pub(super) fn owner_id(value: &str) -> OperationOwnerId {
    OperationOwnerId::try_new(value).expect("valid operation owner id")
}

pub(super) fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

pub(super) fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

pub(super) fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: self::service_id(service_id),
        target_revision: revision_id("rev_2"),
        image: image("ghcr.io/acme/api:rev-2"),
        replicas: replicas(1),
    }
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

pub(super) fn node_id(value: &str) -> ployz_core::ids::NodeId {
    ployz_core::ids::NodeId::try_new(value).expect("valid node id")
}

pub(super) fn container_id(value: &str) -> ployz_core::ids::ContainerId {
    ployz_core::ids::ContainerId::try_new(value).expect("valid container id")
}

pub(super) fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

pub(super) fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

pub(super) fn lease_time(value: u64) -> OperationLeaseExpiresAt {
    OperationLeaseExpiresAt::try_new(value).expect("valid lease time")
}

pub(super) fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

pub(super) fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

pub(super) fn cancellation_reason(value: &str) -> CancellationReason {
    CancellationReason::try_new(value).expect("valid cancellation reason")
}

pub(super) fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ActiveServiceCommit
}
