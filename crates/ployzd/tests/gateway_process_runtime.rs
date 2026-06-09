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
    GatewayHttpFailure, GatewayProcessAttempt, start_gateway_process_runtime_with_client,
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn gateway_process_starts_before_projection_sources_exist() {
    let nats = TestNats::start_without_buckets().await;
    let runtime = start_gateway_process_runtime_with_client(
        nats.client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
    )
    .await
    .expect("gateway runtime starts");
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
        runtime.served_projection().is_some()
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

#[tokio::test]
async fn gateway_process_serves_http_from_nats_projection() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_gateway_buckets().await;
    let upstream = TestUpstream::start().await;
    let runtime = start_gateway_process_runtime_with_client(
        nats.client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
    )
    .await
    .expect("gateway runtime starts");
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");

    routes
        .commit_active_route(&ActiveRouteCommitRequest {
            target: route_target("api.example.com", runtime.listen_addr().port()),
            endpoint_port: route_port(upstream.port()),
            expected_current: ExpectedActiveRoute::Absent,
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        })
        .await
        .expect("route stores");
    observations
        .replace_node_containers(&node_snapshot(
            "node_7",
            [managed_observation_with_endpoint(
                "node_7",
                "ctr_7",
                "127.0.0.1",
                upstream.port(),
            )],
        ))
        .await
        .expect("observation stores");

    wait_until(Duration::from_secs(2), || {
        gateway_serves_route(&runtime, "127.0.0.1", upstream.port())
    })
    .await;

    let mut client = TcpStream::connect(runtime.listen_addr())
        .await
        .expect("connect gateway");
    client
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n")
        .await
        .expect("write request");
    client.shutdown().await.expect("finish request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read response");

    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    assert_eq!(
        upstream.request().await,
        "GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n"
    );

    runtime.shutdown().await;
}

#[tokio::test]
async fn gateway_process_records_http_proxy_failures() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_gateway_buckets().await;
    let runtime = start_gateway_process_runtime_with_client(
        nats.client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
    )
    .await
    .expect("gateway runtime starts");
    wait_until(Duration::from_secs(2), || {
        runtime.served_projection().is_some()
    })
    .await;

    let mut client = TcpStream::connect(runtime.listen_addr())
        .await
        .expect("connect gateway");
    client
        .write_all(b"GET /missing HTTP/1.1\r\nHost: missing.example.com\r\n\r\n")
        .await
        .expect("write unroutable request");
    client.shutdown().await.expect("finish request");

    wait_until(Duration::from_secs(2), || {
        runtime.health().last_http_failure.is_some()
    })
    .await;
    assert!(matches!(
        runtime.health().last_http_failure,
        Some(GatewayHttpFailure::Proxy { .. })
    ));
    assert_eq!(runtime.health().consecutive_http_failures, 1);

    runtime.shutdown().await;
}

fn gateway_serves_route(
    runtime: &ployzd::gateway_process_runtime::RunningGatewayProcessRuntime,
    endpoint_ip: &str,
    endpoint_port: u16,
) -> bool {
    runtime.served_projection().is_some_and(|projection| {
        matches!(
            projection.routes.as_slice(),
            [route] if route.upstreams == vec![GatewayUpstream {
                node_id: node_id("node_7"),
                container_id: container_id("ctr_7"),
                endpoint: endpoint(endpoint_ip, endpoint_port),
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
    managed_observation_with_endpoint(node_id_value, container_id_value, "10.0.0.7", 8080)
}

fn managed_observation_with_endpoint(
    node_id_value: &str,
    container_id_value: &str,
    ip: &str,
    port: u16,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id(node_id_value),
        container_id: container_id(container_id_value),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::running_at(endpoint(ip, port)),
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

fn socket_addr(value: &str) -> std::net::SocketAddr {
    value.parse().expect("valid socket address")
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

struct TestUpstream {
    addr: std::net::SocketAddr,
    request: tokio::sync::oneshot::Receiver<String>,
}

impl TestUpstream {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream");
        let addr = listener.local_addr().expect("upstream local addr");
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept upstream");
            let mut request = Vec::new();
            read_until_http_head(&mut stream, &mut request).await;
            let _ = request_tx.send(String::from_utf8(request).expect("request is utf8"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke")
                .await
                .expect("write upstream response");
        });

        Self {
            addr,
            request: request_rx,
        }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    async fn request(self) -> String {
        self.request.await.expect("upstream receives request")
    }
}

async fn read_until_http_head(stream: &mut TcpStream, request: &mut Vec<u8>) {
    let mut chunk = [0; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .expect("read upstream request");
        assert!(read > 0, "client closed before complete HTTP head");
        request.extend_from_slice(
            chunk
                .get(..read)
                .expect("read byte count is within buffer length"),
        );
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return;
        }
    }
}
