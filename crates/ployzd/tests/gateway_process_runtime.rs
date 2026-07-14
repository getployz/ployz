use futures_util::StreamExt;
use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    CertificateChallengeApplicationStatus, CertificateChallengeStatusRequest,
    CertificateChallengeStatusResponse,
};
use ployz_core::machine_rpc::MachineRpcResponse;
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, MachineFactsSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    ControlPlaneEpoch, GatewayServingStatus, IntentSnapshot, ManagedLeaseProjection,
    RouteBindingState,
};
use ployz_core::subjects::{
    INTENT_GET, MachineServiceEndpoint, gateway_status, machine_facts, machine_service,
};
use ployz_nats::service_runtime::{
    NatsJsonServiceRequestError, NatsServiceRequestFailure, NatsServiceResponse,
    RunningNatsService, request_json, start_nats_service,
};
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, route_hostname, route_port, service_id,
};
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::intent::service::{RunningIntentService, start_intent_service};
use ployzd::roles::gateway::process::{
    GatewayHttpFailure, GatewayProcessAttempt, GatewayProcessError,
    start_gateway_process_with_client,
};
use ployzd::roles::gateway::projection::GatewayUpstream;
use ployzd::service_catalog::{intent_get_endpoint_spec, intent_service};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn gateway_process_reports_http_bind_failure_before_returning() {
    let nats = TestNats::start().await;
    let occupied_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind occupied listener");
    let occupied_addr = occupied_listener
        .local_addr()
        .expect("read occupied listener address");

    let result = start_gateway_process_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        occupied_addr,
        machine_id("machine_7"),
        None,
    )
    .await;

    let Err(error) = result else {
        panic!("gateway runtime unexpectedly started on occupied address");
    };
    assert!(matches!(
        error,
        GatewayProcessError::BindHttp { addr, .. } if addr == occupied_addr
    ));
}

#[tokio::test]
async fn gateway_process_serves_http_from_nats_projection() {
    let nats = TestNats::start().await;
    let _intent = nats.start_intent().await;
    let upstream = TestUpstream::start().await;
    let runtime = start_gateway_process_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
        None,
    )
    .await
    .expect("gateway runtime starts");

    nats.namespace_intent
        .replace_serving_target_entry(serving_target_entry("svc_api", "entry_1"))
        .await
        .expect("serving target entry stores");
    nats.namespace_intent
        .replace_route_binding(RouteBindingState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", runtime.listen_addr().port()),
            endpoint_port: route_port(upstream.port()),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("route stores");
    nats.publish_machine_facts(machine_snapshot(
        "machine_7",
        [managed_observation_with_endpoint(
            "machine_7",
            "ctr_7",
            "127.0.0.1",
        )],
    ))
    .await;

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

    runtime.shutdown().await.expect("gateway shuts down");
}

#[tokio::test]
async fn gateway_process_serves_http01_challenge_before_route_attachment() {
    let nats = TestNats::start().await;
    let challenge = AcmeHttp01Challenge::try_new(
        route_hostname("app.example.com"),
        AcmeChallengeToken::try_new("challenge-token").expect("valid token"),
        AcmeChallengeValue::try_new("challenge-token.account-thumbprint")
            .expect("valid key authorization"),
        AcmeChallengeTtlSeconds::try_new(900).expect("valid ttl"),
    )
    .expect("valid challenge");
    let _intent = nats.start_static_intent(challenge.clone()).await;
    let runtime = start_gateway_process_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
        None,
    )
    .await
    .expect("gateway runtime starts");
    wait_until(Duration::from_secs(2), || {
        runtime
            .served_projection()
            .is_some_and(|projection| projection.challenges == vec![challenge.clone()])
    })
    .await;

    let mut client = TcpStream::connect(runtime.listen_addr())
        .await
        .expect("connect gateway");
    client
        .write_all(
            b"GET /.well-known/acme-challenge/challenge-token HTTP/1.1\r\nHost: app.example.com\r\nConnection: close\r\n\r\n",
        )
        .await
        .expect("write challenge request");
    let response = read_response_until(&mut client, b"challenge-token.account-thumbprint").await;

    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with("\r\n\r\nchallenge-token.account-thumbprint"));

    runtime.shutdown().await.expect("gateway shuts down");
}

#[tokio::test]
async fn gateway_process_owns_certificate_service_lifecycle() {
    let nats = TestNats::start().await;
    let runtime = start_gateway_process_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
        None,
    )
    .await
    .expect("gateway runtime starts");
    let subject = machine_service(
        &machine_id("machine_7"),
        MachineServiceEndpoint::CertificateChallengeStatus,
    );
    let request = CertificateChallengeStatusRequest {
        challenge: AcmeHttp01Challenge::try_new(
            route_hostname("app.example.com"),
            AcmeChallengeToken::try_new("token").expect("token"),
            AcmeChallengeValue::try_new("token.account-thumbprint").expect("value"),
            AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
        )
        .expect("challenge"),
    };
    let response: CertificateChallengeStatusResponse = request_json(
        &nats.client,
        subject.clone(),
        &request,
        Duration::from_millis(100),
    )
    .await
    .expect("certificate service responds");
    assert!(matches!(
        response,
        MachineRpcResponse::Ok(ok)
            if ok.machine_id == machine_id("machine_7")
                && ok.application == CertificateChallengeApplicationStatus::NotApplied
    ));

    runtime.shutdown().await.expect("gateway shuts down");

    assert!(matches!(
        request_json::<_, CertificateChallengeStatusResponse>(
            &nats.client,
            subject,
            &request,
            Duration::from_millis(100),
        )
        .await,
        Err(NatsJsonServiceRequestError::Request {
            failure: NatsServiceRequestFailure::NoResponders,
        })
    ));
}

#[tokio::test]
async fn gateway_shutdown_cancels_blocked_projection_refresh_and_releases_listener() {
    let nats = TestNats::start().await;
    let mut blocked_intent = nats
        .client
        .subscribe(INTENT_GET)
        .await
        .expect("subscribe without replying to intent requests");
    nats.client
        .flush()
        .await
        .expect("blocked responder is ready");
    let runtime = start_gateway_process_with_client(
        nats.machine_client.clone(),
        Duration::from_secs(60),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
        None,
    )
    .await
    .expect("gateway runtime starts");
    let listen_addr = runtime.listen_addr();
    tokio::time::timeout(Duration::from_secs(1), blocked_intent.next())
        .await
        .expect("projection refresh requests intent")
        .expect("blocked intent responder stays open");

    tokio::time::timeout(Duration::from_secs(3), runtime.shutdown())
        .await
        .expect("gateway shutdown cancels the blocked refresh")
        .expect("gateway shuts down");
    TcpListener::bind(listen_addr)
        .await
        .expect("gateway listener is released after shutdown");
}

#[tokio::test]
async fn gateway_process_applies_route_changes_on_next_poll() {
    let nats = TestNats::start().await;
    let _intent = nats.start_intent().await;
    let runtime = start_gateway_process_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
        None,
    )
    .await
    .expect("gateway runtime starts");
    let mut status_sub = nats
        .client
        .subscribe(gateway_status(&machine_id("machine_7")))
        .await
        .expect("subscribe gateway status");

    nats.namespace_intent
        .replace_serving_target_entry(serving_target_entry("svc_api", "entry_1"))
        .await
        .expect("serving target entry stores");
    nats.namespace_intent
        .replace_route_binding(RouteBindingState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("route stores");
    nats.publish_machine_facts(machine_snapshot(
        "machine_7",
        [managed_observation("machine_7", "ctr_7")],
    ))
    .await;

    wait_until(Duration::from_secs(2), || {
        gateway_serves_route(&runtime, "10.0.0.7", 8080)
    })
    .await;
    assert_eq!(
        runtime.health().last_attempt,
        Some(GatewayProcessAttempt::Current { route_count: 1 })
    );
    wait_until_gateway_status_current(&mut status_sub).await;

    runtime.shutdown().await.expect("gateway shuts down");
}

#[tokio::test]
async fn gateway_process_records_http_proxy_failures() {
    let nats = TestNats::start().await;
    let _intent = nats.start_intent().await;
    let runtime = start_gateway_process_with_client(
        nats.machine_client.clone(),
        Duration::from_millis(10),
        socket_addr("127.0.0.1:0"),
        machine_id("machine_7"),
        None,
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

    runtime.shutdown().await.expect("gateway shuts down");
}

async fn wait_until_gateway_status_current(status_sub: &mut async_nats::Subscriber) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(Some(message)) =
            tokio::time::timeout(Duration::from_millis(50), status_sub.next()).await
        {
            let status: ployz_core::state::GatewayStatusObservation =
                serde_json::from_slice(&message.payload).expect("gateway status decodes");
            if status.serving == GatewayServingStatus::Current && status.route_count == 1 {
                return;
            }
        }
    }
    panic!("gateway status did not become current before timeout");
}

fn gateway_serves_route(
    runtime: &ployzd::roles::gateway::process::RunningGatewayProcess,
    endpoint_ip: &str,
    endpoint_port: u16,
) -> bool {
    runtime.served_projection().is_some_and(|projection| {
        matches!(
            projection.routes.as_slice(),
            [route] if route.upstreams == vec![GatewayUpstream {
                machine_id: machine_id("machine_7"),
                container_id: container_id("ctr_7"),
                address: std::net::SocketAddr::new(
                    endpoint_ip.parse().expect("valid endpoint ip"),
                    endpoint_port,
                ),
            }]
        )
    })
}

struct TestNats {
    _connected: ployz_test_support::nats::TestNats,
    /// Controller principal: route-state writes and intent service.
    client: async_nats::Client,
    /// Machine principal: the gateway process side (gateway runs as the
    /// machine's Machine user in v1) and fact writes.
    machine_client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
}

impl TestNats {
    async fn start() -> Self {
        let connected =
            ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_7")])
                .await;
        let client = connected.controller.clone();
        let machine_client = connected.machine_client(&machine_id("machine_7")).await;
        let namespace_intent = NamespaceIntentStore::new(
            ployzd::core_store::CoreStore::open_in_memory()
                .await
                .expect("open core store"),
        );

        Self {
            _connected: connected,
            client,
            machine_client,
            namespace_intent,
        }
    }

    async fn start_intent(&self) -> RunningIntentService {
        start_intent_service(
            self.client.clone(),
            machine_id("machine_a"),
            self.namespace_intent.clone(),
            ployzd::core_store::CoreStore::open_in_memory()
                .await
                .expect("core store opens"),
            Duration::from_millis(10),
        )
        .await
        .expect("intent runtime starts")
    }

    async fn start_static_intent(&self, challenge: AcmeHttp01Challenge) -> RunningNatsService {
        let mut service = start_nats_service(self.client.clone(), &intent_service())
            .await
            .expect("intent service starts");
        service
            .bind_endpoint(&intent_get_endpoint_spec(), move |_request| {
                let snapshot = IntentSnapshot {
                    epoch: ControlPlaneEpoch::initial(),
                    core_machine_id: machine_id("machine_a"),
                    active_machines: Vec::new(),
                    dataplane_projection: ployz_core::dataplane::DataplaneProjection::try_new(
                        Vec::new(),
                        None,
                    )
                    .expect("empty projection"),
                    route_bindings: Vec::new(),
                    serving_target_entries: Vec::new(),
                    volume_pins: Vec::new(),
                    nats_authorizations: Vec::new(),
                    public_url: ployz_core::state::IntentPublicUrl::Auto(Box::new(
                        ManagedLeaseProjection::Unacquired,
                    )),
                    custom_certificates: Vec::new(),
                    acme_http01_challenges: vec![challenge.clone()],
                };
                async move { NatsServiceResponse::json_ok(&snapshot) }
            })
            .await
            .expect("intent endpoint binds");
        self.client
            .flush()
            .await
            .expect("intent service subscription is ready");
        service
    }

    async fn publish_machine_facts(&self, snapshot: MachineContainerObservationSnapshot) {
        let facts = MachineFactsSnapshot::try_new(
            snapshot.machine_id().clone(),
            snapshot,
            None,
            test_disk_space(),
            ployz_core::image::OciPlatform::current(),
            1,
        )
        .expect("machine facts are valid");
        let payload = serde_json::to_vec(&facts).expect("machine facts encode");
        self.machine_client
            .publish(machine_facts(facts.machine_id()), payload.into())
            .await
            .expect("publish machine facts");
        self.machine_client
            .flush()
            .await
            .expect("flush machine facts");
    }
}

fn test_disk_space() -> ployz_core::machine_runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
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
    managed_observation_with_endpoint(machine_id_value, container_id_value, "10.0.0.7")
}

fn managed_observation_with_endpoint(
    machine_id_value: &str,
    container_id_value: &str,
    ip: &str,
) -> ManagedContainerObservation {
    containers::observation(machine_id_value, container_id_value)
        .with(
            containers::identity("svc_api")
                .entry("entry_1")
                .operation("op_123")
                .step("step_1"),
        )
        .running_at(ip.parse().expect("valid endpoint ip"))
        .build()
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

fn socket_addr(value: &str) -> std::net::SocketAddr {
    value.parse().expect("valid socket address")
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
