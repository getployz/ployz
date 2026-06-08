use async_nats::jetstream;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerEndpoint, ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{ActiveRouteCommitRequest, ExpectedActiveRoute};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployzd::gateway::GatewayUpstream;
use ployzd::gateway_process_runtime::{
    GatewayProcessAttempt, start_gateway_process_runtime_with_client,
};
use std::time::{Duration, Instant};

#[tokio::test]
async fn gateway_process_starts_before_projection_sources_exist() {
    let nats = TestNats::start_without_buckets().await;
    let runtime =
        start_gateway_process_runtime_with_client(nats.client.clone(), Duration::from_millis(10));
    wait_until(Duration::from_secs(1), || {
        runtime.health().last_attempt.is_some()
    })
    .await;

    assert!(matches!(
        runtime.health().last_attempt,
        Some(GatewayProcessAttempt::Failed { .. })
    ));

    nats.create_gateway_buckets().await;
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");
    routes
        .commit_active_route(&ActiveRouteCommitRequest {
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            expected_current: ExpectedActiveRoute::Absent,
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        })
        .await
        .expect("route stores");
    observations
        .replace_node_containers(&node_snapshot(
            "node_7",
            [managed_observation("node_7", "ctr_7")],
        ))
        .await
        .expect("observation stores");

    wait_until(Duration::from_secs(2), || {
        gateway_serves_smoke_route(&runtime)
    })
    .await;

    let projection = runtime
        .served_projection()
        .expect("gateway serves projection");
    let [route] = projection.routes.as_slice() else {
        panic!("expected one gateway route, got {:?}", projection.routes);
    };
    assert_eq!(
        route.upstreams,
        vec![GatewayUpstream {
            node_id: node_id("node_7"),
            container_id: container_id("ctr_7"),
            endpoint: endpoint("10.0.0.7", 8080),
        }]
    );
    assert_eq!(
        runtime.health().last_attempt,
        Some(GatewayProcessAttempt::Current { route_count: 1 })
    );

    runtime.shutdown().await;
}

fn gateway_serves_smoke_route(
    runtime: &ployzd::gateway_process_runtime::RunningGatewayProcessRuntime,
) -> bool {
    runtime.served_projection().is_some_and(|projection| {
        matches!(
            projection.routes.as_slice(),
            [route] if route.upstreams == vec![GatewayUpstream {
                node_id: node_id("node_7"),
                container_id: container_id("ctr_7"),
                endpoint: endpoint("10.0.0.7", 8080),
            }]
        )
    })
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
}

impl TestNats {
    async fn start_without_buckets() -> Self {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let client = async_nats::connect(server.client_url())
            .await
            .expect("connect to test nats");

        Self {
            _server: server,
            client,
        }
    }

    async fn create_gateway_buckets(&self) {
        let jetstream = jetstream::new(self.client.clone());
        for bucket in [KV_CORE_BUCKET, KV_OBS_BUCKET] {
            jetstream
                .create_key_value(jetstream::kv::Config {
                    bucket: bucket.to_owned(),
                    ..Default::default()
                })
                .await
                .expect("create key value bucket");
        }
    }
}

async fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(predicate(), "condition did not become true before timeout");
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
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id(node_id_value),
        container_id: container_id(container_id_value),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
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
