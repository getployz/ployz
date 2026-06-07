use async_nats::jetstream;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
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
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let target = route_target("api.example.com", 443);

    routes
        .commit_active_route(&ActiveRouteCommitRequest {
            target: target.clone(),
            expected_current: ExpectedActiveRoute::Absent,
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        })
        .await
        .expect("route stores");
    observations
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
            }],
        }]
    );
}

#[tokio::test]
async fn gateway_source_marks_old_observations_stale_before_projection() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let target = route_target("api.example.com", 443);

    routes
        .commit_active_route(&ActiveRouteCommitRequest {
            target: target.clone(),
            expected_current: ExpectedActiveRoute::Absent,
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        })
        .await
        .expect("route stores");
    observations
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
        }]
    );
}

#[tokio::test]
async fn gateway_source_reports_invalid_route_state_as_invalid_source() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observation store");
    let core_bucket = nats
        .jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open raw core bucket");
    let payload = serde_json::to_vec(&ActiveRouteState {
        target: route_target("api.example.com", 443),
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
    _server: nats_server::Server,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let config = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ployz-nats/tests/configs/jetstream.conf"
    );
    let server = nats_server::run_server(config);
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let jetstream = jetstream::new(client);
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
        _server: server,
        jetstream,
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
        state: ContainerRuntimeState::Running,
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
