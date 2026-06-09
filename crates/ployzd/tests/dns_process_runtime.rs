use async_nats::jetstream;
use ployz_core::ids::{NodeId, RevisionId, ServiceId};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{
    ActiveRouteCommitRequest, ExpectedActiveRoute, GatewayServingStatus, GatewayStatusObservation,
    NodePublicIpObservation,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployzd::dns_process_runtime::{DnsProcessAttempt, start_dns_process_runtime_with_client};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

#[tokio::test]
async fn dns_process_serves_udp_answers_from_nats_projection() {
    let nats = TestNats::start().await;
    let runtime = start_dns_process_runtime_with_client(
        nats.client.clone(),
        Duration::from_secs(60),
        socket_addr("127.0.0.1:0"),
    )
    .await
    .expect("DNS runtime starts");
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");

    routes
        .commit_active_route(&active_route_commit("api.example.com", 443, 8080))
        .await
        .expect("route stores");
    observations
        .replace_gateway_status(&gateway_status(
            "gateway_1",
            GatewayServingStatus::Current,
            1,
        ))
        .await
        .expect("gateway status stores");
    observations
        .replace_node_public_ip(&node_public_ip("gateway_1", [203, 0, 113, 10]))
        .await
        .expect("public ip stores");

    wait_until(Duration::from_secs(2), || {
        runtime
            .served_projection()
            .is_some_and(|projection| projection.records.len() == 1)
    })
    .await;
    assert_eq!(
        runtime.health().last_attempt,
        Some(DnsProcessAttempt::Current { record_count: 1 })
    );

    let response = dns_query(runtime.listen_addr(), "api.example.com", 1).await;
    assert_eq!(first_a_answer(&response), Some([203, 0, 113, 10]));

    observations
        .replace_node_public_ip(&node_public_ip("gateway_1", [203, 0, 113, 20]))
        .await
        .expect("updated public ip stores");
    wait_until(Duration::from_secs(2), || {
        runtime
            .served_projection()
            .is_some_and(|projection| projection.records[0].answers[0].render() == "203.0.113.20")
    })
    .await;

    let response = dns_query(runtime.listen_addr(), "api.example.com", 1).await;
    assert_eq!(first_a_answer(&response), Some([203, 0, 113, 20]));

    runtime.shutdown().await;
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
    jetstream: jetstream::Context,
}

impl TestNats {
    async fn start() -> Self {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let client = async_nats::connect(server.client_url())
            .await
            .expect("connect to test nats");
        let jetstream = jetstream::new(client.clone());
        for bucket in [KV_CORE_BUCKET, KV_OBS_BUCKET] {
            jetstream
                .create_key_value(jetstream::kv::Config {
                    bucket: bucket.to_owned(),
                    ..Default::default()
                })
                .await
                .expect("create key value bucket");
        }

        Self {
            _server: server,
            client,
            jetstream,
        }
    }
}

async fn dns_query(server: SocketAddr, hostname: &str, qtype: u16) -> Vec<u8> {
    let socket = UdpSocket::bind(socket_addr("127.0.0.1:0"))
        .await
        .expect("bind client socket");
    let packet = dns_query_packet(0x1234, hostname, qtype);
    socket
        .send_to(&packet, server)
        .await
        .expect("send DNS query");
    let mut response = [0_u8; 512];
    let (len, _) = socket
        .recv_from(&mut response)
        .await
        .expect("receive DNS response");
    response[..len].to_vec()
}

fn dns_query_packet(id: u16, hostname: &str, qtype: u16) -> Vec<u8> {
    let mut packet = Vec::new();
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&0x0100_u16.to_be_bytes());
    packet.extend_from_slice(&1_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    for label in hostname.split('.') {
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&qtype.to_be_bytes());
    packet.extend_from_slice(&1_u16.to_be_bytes());
    packet
}

fn first_a_answer(response: &[u8]) -> Option<[u8; 4]> {
    if response.len() < 12 || u16::from_be_bytes([response[0], response[1]]) != 0x1234 {
        return None;
    }
    let answers = u16::from_be_bytes([response[6], response[7]]);
    if answers == 0 {
        return None;
    }
    let mut offset = 12;
    while *response.get(offset)? != 0 {
        offset += 1 + *response.get(offset)? as usize;
    }
    offset += 5;
    if response.get(offset..offset + 2)? != [0xc0, 0x0c] {
        return None;
    }
    offset += 2;
    let answer_type = u16::from_be_bytes([response[offset], response[offset + 1]]);
    offset += 8;
    let data_len = u16::from_be_bytes([response[offset], response[offset + 1]]) as usize;
    offset += 2;
    if answer_type != 1 || data_len != 4 {
        return None;
    }
    Some([
        response[offset],
        response[offset + 1],
        response[offset + 2],
        response[offset + 3],
    ])
}

async fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition did not become true before timeout");
}

fn active_route_commit(
    hostname: &str,
    public_port: u16,
    endpoint_port: u16,
) -> ActiveRouteCommitRequest {
    ActiveRouteCommitRequest {
        target: route_target(hostname, public_port),
        endpoint_port: route_port(endpoint_port),
        expected_current: ExpectedActiveRoute::Absent,
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
    }
}

fn gateway_status(
    node_id_value: &str,
    serving: GatewayServingStatus,
    route_count: usize,
) -> GatewayStatusObservation {
    GatewayStatusObservation {
        node_id: node_id(node_id_value),
        listen_addr: socket_addr("127.0.0.1:8080"),
        serving,
        route_count,
    }
}

fn node_public_ip(node_id_value: &str, octets: [u8; 4]) -> NodePublicIpObservation {
    NodePublicIpObservation {
        node_id: node_id(node_id_value),
        public_ip: IpAddr::V4(Ipv4Addr::from(octets)),
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget {
        hostname: route_hostname(hostname),
        port: route_port(port),
    }
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

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn socket_addr(value: &str) -> SocketAddr {
    value.parse().expect("valid socket address")
}
