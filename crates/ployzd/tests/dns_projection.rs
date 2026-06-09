use async_nats::jetstream;
use ployz_core::ops::RouteHostname;
use ployz_core::state::{
    ActiveRouteCommitRequest, ExpectedActiveRoute, GatewayServingStatus, GatewayStatusObservation,
    NodePublicIpObservation,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployzd::dns::{
    DnsAnswer, DnsProjection, DnsProjectionError, DnsProjectionInput, DnsProjectionState,
    DnsProjectionUpdate, DnsRecordSet, DnsRuntime, DnsServingState, apply_dns_update, project_dns,
};
use ployzd::dns_source::{
    load_dns_projection_input_from_nats, load_dns_projection_update_from_nats,
};

#[test]
fn dns_projection_sorts_records_and_deduplicates_answers() {
    let projection = project_dns(DnsProjectionInput {
        records: vec![
            record("www.example.com", ["203.0.113.20"]),
            record(
                "api.example.com",
                ["203.0.113.10", "203.0.113.10", "203.0.113.11"],
            ),
            record("API.example.com", ["2001:db8::10", "203.0.113.11"]),
        ],
    });

    assert_eq!(
        projection,
        DnsProjection {
            records: vec![
                record(
                    "api.example.com",
                    ["203.0.113.10", "203.0.113.11", "2001:db8::10"]
                ),
                record("www.example.com", ["203.0.113.20"]),
            ],
        }
    );
}

#[test]
fn dns_keeps_last_good_projection_when_source_is_unavailable() {
    let last_good = DnsProjection {
        records: vec![record("api.example.com", ["203.0.113.10"])],
    };

    assert_eq!(
        apply_dns_update(
            DnsProjectionState::Current(last_good),
            DnsProjectionUpdate::SourceUnavailable,
        ),
        DnsProjectionState::LastKnownGood(DnsProjection {
            records: vec![record("api.example.com", ["203.0.113.10"])],
        })
    );
    assert_eq!(
        apply_dns_update(
            DnsProjectionState::Unavailable,
            DnsProjectionUpdate::SourceUnavailable,
        ),
        DnsProjectionState::Unavailable
    );
}

#[test]
fn dns_retains_last_good_projection_when_source_is_invalid() {
    let last_good = DnsProjection {
        records: vec![record("api.example.com", ["203.0.113.10"])],
    };
    let error = DnsProjectionError::InvalidSource {
        message: "decode failed".to_owned(),
    };

    assert_eq!(
        apply_dns_update(
            DnsProjectionState::Current(last_good),
            DnsProjectionUpdate::SourceInvalid(error.clone()),
        ),
        DnsProjectionState::ProjectionFailedRetained {
            retained: DnsProjection {
                records: vec![record("api.example.com", ["203.0.113.10"])],
            },
            error,
        }
    );
}

#[test]
fn dns_keeps_failure_evidence_when_invalid_source_then_disappears() {
    let last_good = DnsProjection {
        records: vec![record("api.example.com", ["203.0.113.10"])],
    };
    let error = DnsProjectionError::InvalidSource {
        message: "decode failed".to_owned(),
    };

    let failed = apply_dns_update(
        DnsProjectionState::Current(last_good),
        DnsProjectionUpdate::SourceInvalid(error.clone()),
    );

    assert_eq!(
        apply_dns_update(failed, DnsProjectionUpdate::SourceUnavailable),
        DnsProjectionState::ProjectionFailedRetained {
            retained: DnsProjection {
                records: vec![record("api.example.com", ["203.0.113.10"])],
            },
            error,
        }
    );
}

#[test]
fn dns_runtime_keeps_serving_last_good_answers_after_source_disappears() {
    let mut runtime = DnsRuntime::new();
    let hostname = route_hostname("api.example.com");
    let first_tick =
        runtime.apply_source_update(DnsProjectionUpdate::SourceAvailable(DnsProjectionInput {
            records: vec![record("api.example.com", ["203.0.113.10"])],
        }));

    assert_eq!(
        first_tick.serving,
        DnsServingState::Current { record_count: 1 }
    );
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid answer")]
    );

    let second_tick = runtime.apply_source_update(DnsProjectionUpdate::SourceUnavailable);

    assert!(matches!(
        second_tick.serving,
        DnsServingState::LastKnownGood {
            record_count: 1,
            ..
        }
    ));
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid answer")]
    );
}

#[test]
fn dns_answers_reject_empty_and_whitespace_values() {
    assert_eq!(
        DnsAnswer::try_new(""),
        Err(ployzd::dns::DnsAnswerError::Empty)
    );
    assert!(DnsAnswer::try_new("not-an-address").is_err());
    assert!(DnsAnswer::try_new("203.0.113.10 203.0.113.11").is_err());
    assert_eq!(
        DnsAnswer::try_new("2001:db8::10")
            .expect("valid ipv6 answer")
            .render(),
        "2001:db8::10"
    );
}

#[tokio::test]
async fn dns_source_builds_records_from_active_routes_and_serving_gateways() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_dns_buckets().await;
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");

    routes
        .commit_active_route(&active_route("api.example.com", "svc_api", "rev_1"))
        .await
        .expect("api route stores");
    routes
        .commit_active_route(&active_route("www.example.com", "svc_web", "rev_9"))
        .await
        .expect("web route stores");
    observations
        .replace_gateway_status(&gateway_status(
            "node_1",
            "127.0.0.1:8080",
            GatewayServingStatus::Current,
            2,
        ))
        .await
        .expect("current gateway stores");
    observations
        .replace_gateway_status(&gateway_status(
            "node_2",
            "127.0.0.1:8081",
            GatewayServingStatus::LastKnownGood,
            1,
        ))
        .await
        .expect("last-good gateway stores");
    observations
        .replace_gateway_status(&gateway_status(
            "node_3",
            "127.0.0.1:8082",
            GatewayServingStatus::Current,
            0,
        ))
        .await
        .expect("empty gateway stores");
    observations
        .replace_gateway_status(&gateway_status(
            "node_4",
            "127.0.0.1:8083",
            GatewayServingStatus::Unavailable,
            2,
        ))
        .await
        .expect("unavailable gateway stores");
    for (node, ip) in [
        ("node_1", "203.0.113.10"),
        ("node_2", "203.0.113.11"),
        ("node_3", "203.0.113.12"),
        ("node_4", "203.0.113.13"),
    ] {
        observations
            .replace_node_public_ip(&node_public_ip(node, ip))
            .await
            .expect("public ip stores");
    }

    let input = load_dns_projection_input_from_nats(&routes, &observations)
        .await
        .expect("dns input loads");

    assert_eq!(
        project_dns(input),
        DnsProjection {
            records: vec![
                record("api.example.com", ["203.0.113.10", "203.0.113.11"]),
                record("www.example.com", ["203.0.113.10", "203.0.113.11"]),
            ],
        }
    );
}

#[tokio::test]
async fn dns_source_reports_unavailable_when_nats_buckets_are_missing() {
    let nats = TestNats::start_without_buckets().await;
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream).await;
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream).await;

    assert!(routes.is_err());
    assert!(observations.is_err());
}

#[tokio::test]
async fn dns_update_keeps_last_good_answers_when_source_disappears() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_dns_buckets().await;
    let jetstream = jetstream::new(nats.client.clone());
    let routes = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&jetstream)
        .await
        .expect("open observation store");
    routes
        .commit_active_route(&active_route("api.example.com", "svc_api", "rev_1"))
        .await
        .expect("route stores");
    observations
        .replace_gateway_status(&gateway_status(
            "node_1",
            "127.0.0.1:8080",
            GatewayServingStatus::Current,
            1,
        ))
        .await
        .expect("gateway stores");
    observations
        .replace_node_public_ip(&node_public_ip("node_1", "203.0.113.10"))
        .await
        .expect("public ip stores");
    let mut runtime = DnsRuntime::new();

    runtime.apply_source_update(load_dns_projection_update_from_nats(&routes, &observations).await);
    runtime.apply_source_update(DnsProjectionUpdate::SourceUnavailable);

    assert_eq!(
        runtime
            .answers()
            .answers_for(&route_hostname("api.example.com")),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid DNS answer")]
    );
}

fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn record<const N: usize>(hostname: &str, answers: [&str; N]) -> DnsRecordSet {
    DnsRecordSet {
        hostname: route_hostname(hostname),
        answers: answers
            .into_iter()
            .map(|answer| DnsAnswer::try_new(answer).expect("valid DNS answer"))
            .collect(),
    }
}

fn route_port(value: u16) -> ployz_core::ops::RoutePort {
    ployz_core::ops::RoutePort::try_new(value).expect("valid route port")
}

fn route_target(hostname: &str, port: u16) -> ployz_core::ops::RouteTarget {
    ployz_core::ops::RouteTarget::try_new(route_hostname(hostname), route_port(port))
}

fn service_id(value: &str) -> ployz_core::ids::ServiceId {
    ployz_core::ids::ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> ployz_core::ids::RevisionId {
    ployz_core::ids::RevisionId::try_new(value).expect("valid revision id")
}

fn node_id(value: &str) -> ployz_core::ids::NodeId {
    ployz_core::ids::NodeId::try_new(value).expect("valid node id")
}

fn active_route(
    hostname: &str,
    service_id_value: &str,
    revision_id_value: &str,
) -> ActiveRouteCommitRequest {
    ActiveRouteCommitRequest {
        target: route_target(hostname, 443),
        endpoint_port: route_port(8080),
        expected_current: ExpectedActiveRoute::Absent,
        service_id: service_id(service_id_value),
        revision_id: revision_id(revision_id_value),
    }
}

fn gateway_status(
    node: &str,
    listen_addr: &str,
    serving: GatewayServingStatus,
    route_count: usize,
) -> GatewayStatusObservation {
    GatewayStatusObservation {
        node_id: node_id(node),
        listen_addr: listen_addr.parse().expect("valid gateway listen addr"),
        serving,
        route_count,
    }
}

fn node_public_ip(node: &str, ip: &str) -> NodePublicIpObservation {
    NodePublicIpObservation {
        node_id: node_id(node),
        public_ip: ip.parse().expect("valid public ip"),
    }
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

    async fn create_dns_buckets(&self) {
        let jetstream = jetstream::new(self.client.clone());
        for bucket in [
            ployz_nats::kv::KV_CORE_BUCKET,
            ployz_nats::observations::KV_OBS_BUCKET,
        ] {
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
