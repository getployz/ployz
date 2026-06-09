use async_nats::jetstream;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot, NodeContainerObservationSnapshotError,
};
use ployz_core::state::{GatewayServingStatus, GatewayStatusObservation, NodePublicIpObservation};
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[tokio::test]
async fn container_observation_store_writes_latest_container_to_kv_obs() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let running = managed_observation("ctr_123", ContainerRuntimeState::running_unroutable());
    let exited = managed_observation("ctr_123", ContainerRuntimeState::Exited);

    store
        .replace_node_containers(&node_snapshot([running]))
        .await
        .expect("first snapshot stores");
    store
        .replace_node_containers(&node_snapshot([exited.clone()]))
        .await
        .expect("second snapshot stores");

    assert_eq!(
        store
            .container(&node_id("node_7"), &container_id("ctr_123"))
            .await
            .expect("container observation loads"),
        Some(exited)
    );
}

#[tokio::test]
async fn container_observation_snapshot_removes_stale_node_containers() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let stale = managed_observation("ctr_123", ContainerRuntimeState::running_unroutable());
    let retained = managed_observation("ctr_456", ContainerRuntimeState::running_unroutable());

    store
        .replace_node_containers(&node_snapshot([stale, retained.clone()]))
        .await
        .expect("initial node snapshot stores");
    store
        .replace_node_containers(&node_snapshot([retained.clone()]))
        .await
        .expect("node snapshot stores");

    assert_eq!(
        store
            .container(&node_id("node_7"), &container_id("ctr_123"))
            .await
            .expect("stale lookup succeeds"),
        None
    );
    assert_eq!(
        store
            .container(&node_id("node_7"), &container_id("ctr_456"))
            .await
            .expect("retained lookup succeeds"),
        Some(retained)
    );
}

#[tokio::test]
async fn node_observation_snapshots_list_sorted_current_snapshots() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let node_7 = node_snapshot_for(
        "node_7",
        [managed_observation_for(
            "node_7",
            "ctr_7",
            ContainerRuntimeState::running_unroutable(),
        )],
    );
    let node_8 = node_snapshot_for(
        "node_8",
        [managed_observation_for(
            "node_8",
            "ctr_8_old",
            ContainerRuntimeState::Exited,
        )],
    );
    let node_8_current = node_snapshot_for(
        "node_8",
        [managed_observation_for(
            "node_8",
            "ctr_8",
            ContainerRuntimeState::running_unroutable(),
        )],
    );

    store
        .replace_node_containers(&node_8)
        .await
        .expect("old node 8 snapshot stores");
    store
        .replace_node_containers(&node_7)
        .await
        .expect("node 7 snapshot stores");
    store
        .replace_node_containers(&node_8_current)
        .await
        .expect("node 8 snapshot replaces old");

    assert_eq!(
        store.node_snapshots().await.expect("snapshots list"),
        vec![node_7, node_8_current]
    );
}

#[tokio::test]
async fn node_public_ip_observations_are_latest_per_node() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let old = node_public_ip("node_7", [203, 0, 113, 7]);
    let current = node_public_ip("node_7", [203, 0, 113, 8]);
    let other = node_public_ip("node_8", [203, 0, 113, 9]);

    store
        .replace_node_public_ip(&old)
        .await
        .expect("old ip stores");
    store
        .replace_node_public_ip(&other)
        .await
        .expect("other ip stores");
    store
        .replace_node_public_ip(&current)
        .await
        .expect("current ip stores");

    assert_eq!(
        store
            .node_public_ip(&node_id("node_7"))
            .await
            .expect("public ip loads"),
        Some(current.clone())
    );
    assert_eq!(
        store.node_public_ips().await.expect("public ips list"),
        vec![current, other]
    );
}

#[tokio::test]
async fn node_public_ip_observation_clear_removes_stale_value() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let node_id = node_id("node_7");
    let stale = node_public_ip("node_7", [203, 0, 113, 7]);

    store
        .replace_node_public_ip(&stale)
        .await
        .expect("public ip stores");
    store
        .clear_node_public_ip(&node_id)
        .await
        .expect("public ip clears");

    assert_eq!(
        store
            .node_public_ip(&node_id)
            .await
            .expect("public ip lookup succeeds"),
        None
    );
    assert_eq!(
        store.node_public_ips().await.expect("public ips list"),
        Vec::<NodePublicIpObservation>::new()
    );
}

#[tokio::test]
async fn gateway_status_observations_are_latest_per_node() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let old = gateway_status("node_7", GatewayServingStatus::Unavailable, 0);
    let current = gateway_status("node_7", GatewayServingStatus::Current, 2);
    let other = gateway_status("node_8", GatewayServingStatus::LastKnownGood, 1);

    store
        .replace_gateway_status(&old)
        .await
        .expect("old gateway status stores");
    store
        .replace_gateway_status(&other)
        .await
        .expect("other gateway status stores");
    store
        .replace_gateway_status(&current)
        .await
        .expect("current gateway status stores");

    assert_eq!(
        store
            .gateway_status(&node_id("node_7"))
            .await
            .expect("gateway status loads"),
        Some(current.clone())
    );
    assert_eq!(
        store
            .gateway_statuses()
            .await
            .expect("gateway statuses list"),
        vec![current, other]
    );
}

#[tokio::test]
async fn container_observation_snapshot_rejects_wrong_node_before_store_write() {
    let mut wrong_node =
        managed_observation("ctr_456", ContainerRuntimeState::running_unroutable());
    wrong_node.node_id = node_id("node_8");

    assert_eq!(
        NodeContainerObservationSnapshot::try_new(node_id("node_7"), [wrong_node]),
        Err(NodeContainerObservationSnapshotError::NodeMismatch {
            expected: node_id("node_7"),
            actual: node_id("node_8"),
            container_id: container_id("ctr_456")
        })
    );
}

#[tokio::test]
async fn missing_container_observation_returns_none() {
    let nats = test_nats().await;
    let store = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");

    assert_eq!(
        store
            .container(&node_id("node_7"), &container_id("ctr_missing"))
            .await
            .expect("missing container lookup succeeds"),
        None
    );
}

struct TestNats {
    _server: nats_server::Server,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let server = nats_server::run_server("tests/configs/jetstream.conf");
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let jetstream = jetstream::new(client);
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_OBS_BUCKET.to_owned(),
            ..Default::default()
        })
        .await
        .expect("create KV_OBS bucket");

    TestNats {
        _server: server,
        jetstream,
    }
}

fn managed_observation(
    container_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    managed_observation_for("node_7", container_id_value, state)
}

fn managed_observation_for(
    node_id_value: &str,
    container_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id(node_id_value),
        container_id: container_id(container_id_value),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
        state,
    }
}

fn node_snapshot(
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> NodeContainerObservationSnapshot {
    node_snapshot_for("node_7", containers)
}

fn node_snapshot_for(
    node_id_value: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> NodeContainerObservationSnapshot {
    NodeContainerObservationSnapshot::try_new(node_id(node_id_value), containers)
        .expect("matching node snapshot")
}

fn node_public_ip(node_id_value: &str, octets: [u8; 4]) -> NodePublicIpObservation {
    NodePublicIpObservation {
        node_id: node_id(node_id_value),
        public_ip: IpAddr::V4(Ipv4Addr::from(octets)),
    }
}

fn gateway_status(
    node_id_value: &str,
    serving: GatewayServingStatus,
    route_count: usize,
) -> GatewayStatusObservation {
    GatewayStatusObservation {
        node_id: node_id(node_id_value),
        listen_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, 8080)),
        serving,
        route_count,
    }
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}
