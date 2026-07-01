use async_nats::jetstream;
use ployz_core::machine_runtime::{
    ContainerEndpoint, ContainerRuntimeState, MachineContainerObservationSnapshot,
    ManagedContainerKind, ManagedContainerObservation,
};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{ActiveRouteCommitRequest, ExpectedActiveRoute, GatewayServingStatus};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::ids::{
    container_id, machine_id, operation_id, revision_id, route_hostname, route_port, service_id,
    step_id,
};
use ployzd::gateway::GatewayUpstream;
use ployzd::gateway_process_runtime::{
    GatewayHttpFailure, GatewayProcessAttempt, GatewayStatusPublishFailure,
    start_gateway_process_runtime_with_client,
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn gateway_process_starts_before_projection_sources_exist() {
    let nats = TestNats::start_without_buckets().await;
    let runtime = start_gateway_process_runtime_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
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
    assert!(matches!(
        runtime.health().last_status_publish_failure,
        Some(GatewayStatusPublishFailure::Write { .. })
    ));

    nats.create_gateway_buckets().await;
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream())
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
        .replace_machine_containers(&machine_snapshot(
            "machine_7",
            [managed_observation("machine_7", "ctr_7")],
        ))
        .await
        .expect("observation stores");

    wait_until(Duration::from_secs(2), || {
        gateway_serves_route(&runtime, "10.0.0.7", 8080)
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
            machine_id: machine_id("machine_7"),
            container_id: container_id("ctr_7"),
            endpoint: endpoint("10.0.0.7", 8080),
        }]
    );
    assert_eq!(
        runtime.health().last_attempt,
        Some(GatewayProcessAttempt::Current { route_count: 1 })
    );
    assert_eq!(runtime.health().last_status_publish_failure, None);

    runtime.shutdown().await;
}

#[tokio::test]
async fn gateway_process_serves_http_from_nats_projection() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_gateway_buckets().await;
    let upstream = TestUpstream::start().await;
    let runtime = start_gateway_process_runtime_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
    )
    .await
    .expect("gateway runtime starts");
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream())
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
        .replace_machine_containers(&machine_snapshot(
            "machine_7",
            [managed_observation_with_endpoint(
                "machine_7",
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
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com\r\nConnection: close\r\n\r\n")
        .await
        .expect("write request");
    let response = read_response_until(&mut client, b"smoke").await;

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("\r\n\r\nsmoke"));
    let upstream_request = upstream.request().await;
    assert!(upstream_request.starts_with("GET /smoke HTTP/1.1\r\n"));
    assert!(upstream_request.contains("\r\nHost: api.example.com\r\n"));
    assert!(upstream_request.contains("\r\nConnection: close\r\n"));
    drop(client);

    runtime.shutdown().await;
}

#[tokio::test]
async fn gateway_process_applies_route_changes_from_nats_watch_before_next_poll() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_gateway_buckets().await;
    let runtime = start_gateway_process_runtime_with_client(
        nats.machine_client.clone(),
        Duration::from_secs(60),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
    )
    .await
    .expect("gateway runtime starts");
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream())
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
        .replace_machine_containers(&machine_snapshot(
            "machine_7",
            [managed_observation("machine_7", "ctr_7")],
        ))
        .await
        .expect("observation stores");

    wait_until(Duration::from_secs(2), || {
        gateway_serves_route(&runtime, "10.0.0.7", 8080)
    })
    .await;
    assert_eq!(
        runtime.health().last_attempt,
        Some(GatewayProcessAttempt::Current { route_count: 1 })
    );
    wait_until_gateway_status_current(&observations, "machine_7").await;

    runtime.shutdown().await;
}

#[tokio::test]
async fn gateway_process_records_http_proxy_failures() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_gateway_buckets().await;
    let runtime = start_gateway_process_runtime_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
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
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read gateway error response");

    wait_until(Duration::from_secs(2), || {
        runtime.health().last_http_failure.is_some()
    })
    .await;
    assert!(matches!(
        runtime.health().last_http_failure,
        Some(GatewayHttpFailure::Proxy { .. })
    ));
    assert_eq!(runtime.health().consecutive_http_failures, 1);
    assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));

    runtime.shutdown().await;
}

async fn wait_until_gateway_status_current(
    observations: &AsyncNatsObservationStore,
    machine_id_value: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let status = observations
            .gateway_status(&machine_id(machine_id_value))
            .await
            .expect("gateway status loads");
        if status.is_some_and(|status| {
            status.serving == GatewayServingStatus::Current && status.route_count == 1
        }) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("gateway status did not become current before timeout");
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
                machine_id: machine_id("machine_7"),
                container_id: container_id("ctr_7"),
                endpoint: endpoint(endpoint_ip, endpoint_port),
            }]
        )
    })
}

struct TestNats {
    connected: ployz_test_support::nats::TestNats,
    /// Controller principal: bucket administration and route-state writes.
    client: async_nats::Client,
    /// Machine principal: the gateway process side (gateway runs as the
    /// machine's Machine user in v1) and observation writes.
    machine_client: async_nats::Client,
}

impl TestNats {
    async fn start_without_buckets() -> Self {
        let connected =
            ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_7")])
                .await;
        let client = connected.controller.clone();
        let machine_client = connected.machine_client(&machine_id("machine_7")).await;

        Self {
            connected,
            client,
            machine_client,
        }
    }

    fn machine_jetstream(&self) -> jetstream::Context {
        jetstream::new(self.machine_client.clone())
    }

    async fn create_gateway_buckets(&self) {
        self.connected.bootstrap_resources().await;
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

fn machine_snapshot(
    machine_id_value: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(machine_id_value), containers)
        .expect("matching machine snapshot")
}

fn managed_observation(
    machine_id_value: &str,
    container_id_value: &str,
) -> ManagedContainerObservation {
    managed_observation_with_endpoint(machine_id_value, container_id_value, "10.0.0.7", 8080)
}

fn managed_observation_with_endpoint(
    machine_id_value: &str,
    container_id_value: &str,
    ip: &str,
    port: u16,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: machine_id(machine_id_value),
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
    RouteTarget::new(route_hostname(hostname), route_port(port))
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
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept upstream");
                let mut request = Vec::new();
                if !read_until_http_head(&mut stream, &mut request).await {
                    continue;
                }
                let _ = request_tx.send(String::from_utf8(request).expect("request is utf8"));
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke")
                    .await
                    .expect("write upstream response");
                return;
            }
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

async fn read_until_http_head(stream: &mut TcpStream, request: &mut Vec<u8>) -> bool {
    let mut chunk = [0; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .expect("read upstream request");
        if read == 0 {
            return false;
        }
        request.extend_from_slice(
            chunk
                .get(..read)
                .expect("read byte count is within buffer length"),
        );
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return true;
        }
    }
}

async fn read_response_until(stream: &mut TcpStream, expected: &[u8]) -> String {
    let mut response = Vec::new();
    let read = async {
        let mut chunk = [0; 1024];
        loop {
            let count = stream.read(&mut chunk).await.expect("read response");
            assert!(count > 0, "gateway closed before expected response");
            response.extend_from_slice(
                chunk
                    .get(..count)
                    .expect("read byte count is within buffer length"),
            );
            if response
                .windows(expected.len())
                .any(|window| window == expected)
            {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(2), read)
        .await
        .expect("gateway response arrives");

    String::from_utf8(response).expect("response is utf8")
}
