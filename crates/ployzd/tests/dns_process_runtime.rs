use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    GatewayServingStatus, GatewayStatusObservation, MachineEndpointObservation, RouteBindingState,
};
use ployz_core::subjects::{INTENT_CHANGED, gateway_status, machine_facts};
use ployz_test_support::ids::{machine_id, namespace_id, route_hostname, route_port, service_id};
use ployzd::intent::machine_roster::MachineRosterStore;
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::intent::service::{RunningIntentService, start_intent_service};
use ployzd::roles::dns::process::{
    DnsIntentWatchHealth, DnsProcessAttempt, RunningDnsProcess, start_dns_process_with_client,
};
use ployzd::roles::dns::projection::DnsAnswer;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

#[tokio::test]
async fn dns_process_fails_fast_before_projection_sources_exist() {
    let nats = TestNats::start().await;
    let runtime = start_dns_process_with_client(
        nats.dns_client.clone(),
        machine_id("dns_machine"),
        Duration::from_millis(10),
        None,
        None,
    )
    .await
    .expect("dns runtime starts before sources exist");

    wait_until(Duration::from_secs(2), || {
        matches!(
            runtime.health().last_attempt,
            Some(DnsProcessAttempt::Failed { .. })
        )
    })
    .await;

    runtime.shutdown().await;
}

#[tokio::test]
async fn dns_process_applies_route_changes_on_next_poll() {
    let nats = TestNats::start().await;
    let _intent = nats.start_intent().await;
    let runtime = start_dns_process_with_client(
        nats.dns_client.clone(),
        machine_id("dns_machine"),
        Duration::from_millis(10),
        None,
        None,
    )
    .await
    .expect("dns runtime starts");
    wait_until(Duration::from_secs(2), || {
        runtime.health().last_attempt.is_some()
            && runtime.health().intent_watch == DnsIntentWatchHealth::Watching
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

#[tokio::test]
async fn intent_invalidation_wakes_dns_refresh_before_poll_interval() {
    let nats = TestNats::start().await;
    let _intent = nats
        .start_intent_with_interval(Duration::from_secs(60))
        .await;
    let runtime = start_dns_process_with_client(
        nats.dns_client.clone(),
        machine_id("dns_machine"),
        Duration::from_secs(60),
        None,
        None,
    )
    .await
    .expect("dns runtime starts");
    wait_until(Duration::from_secs(2), || {
        runtime.health().last_attempt.is_some()
    })
    .await;

    nats.commit_route("api.example.com").await;
    nats.publish_serving_gateway("gateway_1", [203, 0, 113, 10])
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    nats.connected
        .controller
        .publish(INTENT_CHANGED, Vec::new().into())
        .await
        .expect("intent invalidation publishes");
    nats.connected
        .controller
        .flush()
        .await
        .expect("intent invalidation flushes");

    wait_until(Duration::from_secs(2), || {
        dns_serves_answer(&runtime, "api.example.com", "203.0.113.10")
    })
    .await;

    runtime.shutdown().await;
}

fn dns_serves_answer(runtime: &RunningDnsProcess, hostname: &str, answer: &str) -> bool {
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
    namespace_intent: NamespaceIntentStore,
}

impl TestNats {
    async fn start() -> Self {
        let connected = ployz_test_support::nats::TestNats::start_with_machines(&[
            machine_id("dns_machine"),
            machine_id("gateway_1"),
        ])
        .await;
        let dns_client = connected.machine_client(&machine_id("dns_machine")).await;
        let namespace_intent = NamespaceIntentStore::new(
            ployzd::core_store::CoreStore::open_in_memory()
                .await
                .expect("open core store"),
        );

        Self {
            connected,
            dns_client,
            namespace_intent,
        }
    }

    async fn start_intent(&self) -> RunningIntentService {
        self.start_intent_with_interval(Duration::from_millis(10))
            .await
    }

    async fn start_intent_with_interval(&self, publish_interval: Duration) -> RunningIntentService {
        start_intent_service(
            self.connected.controller.clone(),
            machine_id("machine_a"),
            MachineRosterStore::new(
                ployzd::core_store::CoreStore::open_in_memory()
                    .await
                    .expect("open core store"),
            ),
            self.namespace_intent.clone(),
            ployzd::core_store::CoreStore::open_in_memory()
                .await
                .expect("core store opens"),
            publish_interval,
        )
        .await
        .expect("intent runtime starts")
    }

    /// Commits an route binding as the controller principal.
    async fn commit_route(&self, hostname: &str) {
        self.namespace_intent
            .replace_route_binding(RouteBindingState {
                namespace_id: namespace_id("default"),
                target: RouteTarget::new(route_hostname(hostname), route_port(443)),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
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
        let status = GatewayStatusObservation {
            machine_id: machine_id(machine_id_value),
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            serving: GatewayServingStatus::Current,
            route_count: 1,
        };
        let facts = gateway_machine_facts(machine_id_value, address);
        gateway_client
            .publish(
                gateway_status(&status.machine_id),
                serde_json::to_vec(&status)
                    .expect("gateway status encodes")
                    .into(),
            )
            .await
            .expect("gateway status publishes");
        gateway_client
            .publish(
                machine_facts(facts.machine_id()),
                serde_json::to_vec(&facts)
                    .expect("machine facts encode")
                    .into(),
            )
            .await
            .expect("machine facts publish");
        gateway_client.flush().await.expect("flush gateway facts");
    }
}

fn gateway_machine_facts(machine_id_value: &str, address: [u8; 4]) -> MachineFactsSnapshot {
    let machine_id = machine_id(machine_id_value);
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id.clone(), Vec::new())
            .expect("empty container facts are valid"),
        Some(MachineEndpointObservation {
            machine_id,
            control_endpoints: vec![IpAddr::V4(Ipv4Addr::from(address))],
            mesh_endpoints: Vec::new(),
        }),
        test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts are valid")
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
