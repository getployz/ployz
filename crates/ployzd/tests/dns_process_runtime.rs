use async_nats::jetstream;
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    ActiveRouteCommitRequest, ExpectedActiveRoute, GatewayServingStatus, GatewayStatusObservation,
    MachinePublicIpObservation,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::ids::{
    machine_id, namespace_id, revision_id, route_hostname, route_port, service_id,
};
use ployzd::dns::DnsAnswer;
use ployzd::dns_process_runtime::{
    DnsProcessAttempt, RunningDnsProcessRuntime, start_dns_process_runtime_with_client,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

#[tokio::test]
async fn dns_process_starts_before_projection_sources_exist() {
    let nats = TestNats::start_without_buckets().await;
    let runtime =
        start_dns_process_runtime_with_client(nats.dns_client.clone(), Duration::from_millis(10))
            .await
            .expect("dns runtime starts");
    wait_until(Duration::from_secs(1), || {
        runtime.health().last_attempt.is_some()
    })
    .await;

    assert!(matches!(
        runtime.health().last_attempt,
        Some(DnsProcessAttempt::Failed { .. })
    ));

    nats.create_buckets().await;
    nats.commit_route("api.example.com").await;
    nats.publish_serving_gateway("gateway_1", [203, 0, 113, 10])
        .await;

    wait_until(Duration::from_secs(2), || {
        dns_serves_answer(&runtime, "api.example.com", "203.0.113.10")
    })
    .await;

    assert_eq!(
        runtime.health().last_attempt,
        Some(DnsProcessAttempt::Current { record_count: 1 })
    );

    runtime.shutdown().await;
}

#[tokio::test]
async fn dns_process_applies_route_changes_from_nats_watch_before_next_poll() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_buckets().await;
    // A 60s refresh interval means observed changes must arrive through the
    // NATS watchers, not through polling.
    let runtime =
        start_dns_process_runtime_with_client(nats.dns_client.clone(), Duration::from_secs(60))
            .await
            .expect("dns runtime starts");
    wait_until(Duration::from_secs(2), || {
        runtime.health().last_attempt.is_some()
    })
    .await;

    nats.commit_route("api.example.com").await;
    nats.publish_serving_gateway("gateway_1", [203, 0, 113, 10])
        .await;

    wait_until(Duration::from_secs(2), || {
        dns_serves_answer(&runtime, "api.example.com", "203.0.113.10")
    })
    .await;
    assert_eq!(
        runtime.health().last_attempt,
        Some(DnsProcessAttempt::Current { record_count: 1 })
    );

    runtime.shutdown().await;
}

fn dns_serves_answer(runtime: &RunningDnsProcessRuntime, hostname: &str, answer: &str) -> bool {
    runtime.served_projection().is_some_and(|projection| {
        matches!(
            projection.records.as_slice(),
            [record] if record.hostname == route_hostname(hostname)
                && record.answers
                    == vec![DnsAnswer::try_new(answer).expect("valid answer")]
        )
    })
}

struct TestNats {
    connected: ployz_test_support::nats::TestNats,
    /// The DNS machine's Machine principal: the runtime under test.
    dns_client: async_nats::Client,
}

impl TestNats {
    async fn start_without_buckets() -> Self {
        let connected = ployz_test_support::nats::TestNats::start_with_machines(&[
            machine_id("dns_machine"),
            machine_id("gateway_1"),
        ])
        .await;
        let dns_client = connected.machine_client(&machine_id("dns_machine")).await;

        Self {
            connected,
            dns_client,
        }
    }

    async fn create_buckets(&self) {
        self.connected.bootstrap_resources().await;
    }

    /// Commits an active route as the controller principal.
    async fn commit_route(&self, hostname: &str) {
        let routes = AsyncNatsCoreStateStore::from_jetstream(&self.connected.jetstream)
            .await
            .expect("open core state store");
        routes
            .commit_active_route(&ActiveRouteCommitRequest {
                namespace_id: namespace_id("default"),
                target: RouteTarget::new(route_hostname(hostname), route_port(443)),
                endpoint_port: route_port(8080),
                expected_current: ExpectedActiveRoute::Absent,
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_1"),
            })
            .await
            .expect("route stores");
    }

    /// Publishes a serving gateway status plus its public IP as that
    /// gateway machine's Machine principal.
    async fn publish_serving_gateway(&self, machine_id_value: &str, address: [u8; 4]) {
        let gateway_client = self
            .connected
            .machine_client(&machine_id(machine_id_value))
            .await;
        let observations =
            AsyncNatsObservationStore::from_jetstream(&jetstream::new(gateway_client))
                .await
                .expect("open gateway observation store");
        observations
            .replace_gateway_status(&GatewayStatusObservation {
                machine_id: machine_id(machine_id_value),
                listen_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
                serving: GatewayServingStatus::Current,
                route_count: 1,
            })
            .await
            .expect("gateway status stores");
        observations
            .replace_machine_public_ip(&MachinePublicIpObservation {
                machine_id: machine_id(machine_id_value),
                public_ip: IpAddr::V4(Ipv4Addr::from(address)),
            })
            .await
            .expect("public ip stores");
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
