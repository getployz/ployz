use async_nats::jetstream;
use ployz_core::ids::{NodeId, RevisionId, ServiceId};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{
    ActiveRouteCommitRequest, ActiveRouteState, ExpectedActiveRoute, GatewayServingStatus,
    GatewayStatusObservation, NodePublicIpObservation,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployzd::dns::{
    DnsAnswer, DnsProjectionError, DnsProjectionUpdate, DnsRecordSet, DnsRuntime, DnsServingState,
    project_dns,
};
use ployzd::dns_source::load_dns_projection_update_from_nats;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[tokio::test]
async fn dns_source_loads_active_route_hostnames_and_serving_gateway_public_ips_from_nats() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.node_jetstream)
        .await
        .expect("open observation store");

    routes
        .commit_active_route(&active_route_commit("api.example.com", 443, 8080))
        .await
        .expect("api route stores");
    routes
        .commit_active_route(&active_route_commit("www.example.com", 443, 8080))
        .await
        .expect("www route stores");
    let gateway_1 = nats.node_observations("gateway_1").await;
    let gateway_2 = nats.node_observations("gateway_2").await;
    let gateway_3 = nats.node_observations("gateway_3").await;
    gateway_1
        .replace_gateway_status(&gateway_status(
            "gateway_1",
            GatewayServingStatus::Current,
            2,
        ))
        .await
        .expect("current gateway status stores");
    gateway_2
        .replace_gateway_status(&gateway_status(
            "gateway_2",
            GatewayServingStatus::LastKnownGood,
            1,
        ))
        .await
        .expect("last-good gateway status stores");
    gateway_3
        .replace_gateway_status(&gateway_status(
            "gateway_3",
            GatewayServingStatus::Current,
            0,
        ))
        .await
        .expect("empty gateway status stores");
    gateway_1
        .replace_node_public_ip(&node_public_ip("gateway_1", [203, 0, 113, 10]))
        .await
        .expect("gateway one public ip stores");
    gateway_2
        .replace_node_public_ip(&node_public_ip("gateway_2", [203, 0, 113, 11]))
        .await
        .expect("gateway two public ip stores");
    gateway_3
        .replace_node_public_ip(&node_public_ip("gateway_3", [203, 0, 113, 12]))
        .await
        .expect("empty gateway public ip stores");
    nats.node_observations("edge_4")
        .await
        .replace_node_public_ip(&node_public_ip("edge_4", [203, 0, 113, 13]))
        .await
        .expect("non-gateway public ip stores");

    let update = load_dns_projection_update_from_nats(&routes, &observations).await;
    let DnsProjectionUpdate::SourceAvailable(input) = update else {
        panic!("DNS source should be available, got {update:?}");
    };
    let projection = project_dns(input);

    assert_eq!(
        projection.records,
        vec![
            dns_record(
                "api.example.com",
                [
                    DnsAnswer::try_new("203.0.113.10").expect("valid answer"),
                    DnsAnswer::try_new("203.0.113.11").expect("valid answer")
                ]
            ),
            dns_record(
                "www.example.com",
                [
                    DnsAnswer::try_new("203.0.113.10").expect("valid answer"),
                    DnsAnswer::try_new("203.0.113.11").expect("valid answer")
                ]
            ),
        ]
    );
}

#[tokio::test]
async fn dns_source_reports_invalid_route_state_as_invalid_source() {
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

    let update = load_dns_projection_update_from_nats(&routes, &observations).await;

    let DnsProjectionUpdate::SourceInvalid(DnsProjectionError::InvalidSource { message }) = update
    else {
        panic!("DNS source should be invalid, got {update:?}");
    };
    assert!(message.contains("active route state key"));
}

#[tokio::test]
async fn dns_runtime_applies_nats_dns_changes_without_control_runtime() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.node_jetstream)
        .await
        .expect("open observation store");
    let mut runtime = DnsRuntime::new();
    let hostname = route_hostname("api.example.com");

    routes
        .commit_active_route(&active_route_commit("api.example.com", 443, 8080))
        .await
        .expect("route stores");
    let gateway_1 = nats.node_observations("gateway_1").await;
    gateway_1
        .replace_gateway_status(&gateway_status(
            "gateway_1",
            GatewayServingStatus::Current,
            1,
        ))
        .await
        .expect("gateway status stores");
    gateway_1
        .replace_node_public_ip(&node_public_ip("gateway_1", [203, 0, 113, 10]))
        .await
        .expect("public ip stores");

    let first_tick = runtime
        .apply_source_update(load_dns_projection_update_from_nats(&routes, &observations).await);
    assert_eq!(
        first_tick.serving,
        DnsServingState::Current { record_count: 1 }
    );
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid answer")]
    );

    gateway_1
        .replace_node_public_ip(&node_public_ip("gateway_1", [203, 0, 113, 20]))
        .await
        .expect("updated public ip stores");
    let second_tick = runtime
        .apply_source_update(load_dns_projection_update_from_nats(&routes, &observations).await);

    assert_eq!(
        second_tick.serving,
        DnsServingState::Current { record_count: 1 }
    );
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.20").expect("valid answer")]
    );
}

struct TestNats {
    nats: ployz_test_support::nats::SecuredTestNats,
    /// Controller principal: route-state writes and bucket administration.
    jetstream: jetstream::Context,
    /// The DNS machine's Node principal: the read side (DNS runs as the
    /// machine's Node user in v1).
    node_jetstream: jetstream::Context,
}

impl TestNats {
    /// The observation store connected as the given node — each node may
    /// only write its own observation keys.
    async fn node_observations(&self, node_id_value: &str) -> AsyncNatsObservationStore {
        let node_client = ployz_nats::connect::connect_authenticated(
            &self
                .nats
                .node_config(&node_id(node_id_value))
                .expect("fixture minted the node credentials"),
            TEST_NATS_CONNECT_TIMEOUT,
        )
        .await
        .expect("observing node connects");
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(node_client))
            .await
            .expect("open node observation store")
    }
}

const TEST_NATS_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn test_nats() -> TestNats {
    let dns_node = node_id("dns_node");
    let nats = ployz_test_support::nats::SecuredTestNats::start_with_nodes(&[
        dns_node.clone(),
        node_id("gateway_1"),
        node_id("gateway_2"),
        node_id("gateway_3"),
        node_id("edge_4"),
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
            .node_config(&dns_node)
            .expect("fixture minted dns_node credentials"),
        TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("dns node connects");
    let jetstream = jetstream::new(client);
    let node_jetstream = jetstream::new(node_client);
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
        nats,
        jetstream,
        node_jetstream,
    }
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
        listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
        serving,
        route_count,
    }
}

fn node_public_ip(node_id_value: &str, address: [u8; 4]) -> NodePublicIpObservation {
    NodePublicIpObservation {
        node_id: node_id(node_id_value),
        public_ip: IpAddr::V4(Ipv4Addr::from(address)),
    }
}

fn dns_record<const N: usize>(hostname: &str, answers: [DnsAnswer; N]) -> DnsRecordSet {
    DnsRecordSet {
        hostname: route_hostname(hostname),
        answers: answers.into_iter().collect(),
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
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
