use async_nats::jetstream;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerEndpoint, ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{ActiveRouteCommitRequest, ActiveRouteState, ExpectedActiveRoute};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployzd::gateway::{
    GatewayProjectedRoute, GatewayProjectionError, GatewayProjectionUpdate, GatewayUpstream,
    project_gateway,
};
use ployzd::gateway_source::{
    load_gateway_projection_update_from_nats,
    load_gateway_projection_update_from_nats_with_stale_after,
};
use std::time::Duration;

#[tokio::test]
async fn gateway_source_loads_routes_and_current_observations_from_nats() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.node_jetstream)
        .await
        .expect("open observation store");
    let target = route_target("api.example.com", 443);

    routes
        .commit_active_route(&ActiveRouteCommitRequest {
            target: target.clone(),
            endpoint_port: route_port(8080),
            expected_current: ExpectedActiveRoute::Absent,
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        })
        .await
        .expect("route stores");
    AsyncNatsObservationStore::from_jetstream(&nats.node_7_jetstream)
        .await
        .expect("open node_7 observation store")
        .replace_node_containers(&node_snapshot(
            "node_7",
            [managed_observation("node_7", "ctr_7", "svc_api", "rev_1")],
        ))
        .await
        .expect("node snapshot stores");

    let update = load_gateway_projection_update_from_nats(&routes, &observations).await;
    let GatewayProjectionUpdate::SourceAvailable(input) = update else {
        panic!("gateway source should be available, got {update:?}");
    };
    let projection = project_gateway(input).expect("gateway projection succeeds");

    assert_eq!(
        projection.routes,
        vec![GatewayProjectedRoute {
            target,
            upstreams: vec![GatewayUpstream {
                node_id: node_id("node_7"),
                container_id: container_id("ctr_7"),
                endpoint: endpoint("10.0.0.7", 8080),
            }],
            unroutable_containers: vec![],
        }]
    );
}

#[tokio::test]
async fn gateway_source_marks_old_observations_stale_before_projection() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.node_jetstream)
        .await
        .expect("open observation store");
    let target = route_target("api.example.com", 443);

    routes
        .commit_active_route(&ActiveRouteCommitRequest {
            target: target.clone(),
            endpoint_port: route_port(8080),
            expected_current: ExpectedActiveRoute::Absent,
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        })
        .await
        .expect("route stores");
    AsyncNatsObservationStore::from_jetstream(&nats.node_7_jetstream)
        .await
        .expect("open node_7 observation store")
        .replace_node_containers(&node_snapshot(
            "node_7",
            [managed_observation("node_7", "ctr_7", "svc_api", "rev_1")],
        ))
        .await
        .expect("node snapshot stores");

    let update = load_gateway_projection_update_from_nats_with_stale_after(
        &routes,
        &observations,
        Duration::ZERO,
    )
    .await;
    let GatewayProjectionUpdate::SourceAvailable(input) = update else {
        panic!("gateway source should be available, got {update:?}");
    };
    let projection = project_gateway(input).expect("gateway projection succeeds");

    assert_eq!(
        projection.routes,
        vec![GatewayProjectedRoute {
            target,
            upstreams: vec![],
            unroutable_containers: vec![],
        }]
    );
}

#[tokio::test]
async fn gateway_source_reports_invalid_route_state_as_invalid_source() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.node_jetstream)
        .await
        .expect("open observation store");
    let core_bucket = nats
        .jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open raw core bucket");
    let payload = serde_json::to_vec(&ActiveRouteState {
        target: route_target("api.example.com", 443),
        endpoint_port: route_port(8080),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
    })
    .expect("route state encodes");
    core_bucket
        .put("routes.deadbeef.443", payload.into())
        .await
        .expect("store corrupt route key");

    let update = load_gateway_projection_update_from_nats(&routes, &observations).await;

    let GatewayProjectionUpdate::SourceInvalid(GatewayProjectionError::InvalidSource { message }) =
        update
    else {
        panic!("gateway source should be invalid, got {update:?}");
    };
    assert!(message.contains("active route state key"));
}

struct TestNats {
    _nats: ployz_test_support::nats::SecuredTestNats,
    /// Controller principal: route-state writes and bucket administration.
    jetstream: jetstream::Context,
    /// The gateway machine's Node principal: the read side (the gateway
    /// runs as the machine's Node user in v1).
    node_jetstream: jetstream::Context,
    /// `node_7`'s Node principal: each node may only write its own
    /// observation keys, so the workload node seeds its own snapshot.
    node_7_jetstream: jetstream::Context,
}

const TEST_NATS_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn test_nats() -> TestNats {
    let gateway_node = node_id("gateway_node");
    let nats = ployz_test_support::nats::SecuredTestNats::start_with_nodes(&[
        gateway_node.clone(),
        node_id("node_7"),
    ])
    .await
    .expect("secured test nats starts");
    let client = ployz_nats::connect::connect_authenticated(
        &nats.controller_config(),
        TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("controller connects");
    let node_client = ployz_nats::connect::connect_authenticated(
        &nats
            .node_config(&gateway_node)
            .expect("fixture minted gateway_node credentials"),
        TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("gateway node connects");
    let node_7_client = ployz_nats::connect::connect_authenticated(
        &nats
            .node_config(&node_id("node_7"))
            .expect("fixture minted node_7 credentials"),
        TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("node_7 connects");
    let jetstream = jetstream::new(client);
    let node_jetstream = jetstream::new(node_client);
    let node_7_jetstream = jetstream::new(node_7_client);
    for bucket in [KV_CORE_BUCKET, KV_OBS_BUCKET] {
        jetstream
            .create_key_value(jetstream::kv::Config {
                bucket: bucket.to_owned(),
                ..Default::default()
            })
            .await
            .expect("create key value bucket");
    }

    TestNats {
        _nats: nats,
        jetstream,
        node_jetstream,
        node_7_jetstream,
    }
}

fn node_snapshot(
    node_id_value: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> NodeContainerObservationSnapshot {
    NodeContainerObservationSnapshot::try_new(node_id(node_id_value), containers)
        .expect("matching node snapshot")
}

fn managed_observation(
    node_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    revision_id_value: &str,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id(node_id_value),
        container_id: container_id(container_id_value),
        service_id: service_id(service_id_value),
        revision_id: revision_id(revision_id_value),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::running_at(endpoint("10.0.0.7", 8080)),
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::try_new(route_hostname(hostname), route_port(port))
}

fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}

fn endpoint(ip: &str, port: u16) -> ContainerEndpoint {
    ContainerEndpoint {
        ip: ip.parse().expect("valid endpoint ip"),
        port: route_port(port),
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
