use async_nats::jetstream;
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    GatewayServingStatus, GatewayStatusObservation, MachinePublicIpObservation, RouteBindingState,
};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::ids::{machine_id, namespace_id, route_hostname, route_port, service_id};
use ployzd::dns::DnsAnswer;
use ployzd::dns_process_runtime::{
    DnsProcessAttempt, DnsProcessRuntimeError, RunningDnsProcessRuntime,
    start_dns_process_runtime_with_client,
};
use ployzd::intent::{RunningIntentRuntime, start_intent_runtime};
use ployzd::machine_roster::MachineRosterStore;
use ployzd::namespace_intent::NamespaceIntentStore;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

#[tokio::test]
async fn dns_process_fails_fast_before_projection_sources_exist() {
    let nats = TestNats::start_without_buckets().await;
    let result =
        start_dns_process_runtime_with_client(nats.dns_client.clone(), Duration::from_millis(10))
            .await;
    let Err(error) = result else {
        panic!("dns runtime should fail before buckets exist");
    };

    assert!(matches!(error, DnsProcessRuntimeError::OpenObservations(_)));
}

#[tokio::test]
async fn dns_process_applies_route_changes_on_next_poll() {
    let nats = TestNats::start_without_buckets().await;
    nats.create_buckets().await;
    let _intent = nats.start_intent().await;
    let runtime =
        start_dns_process_runtime_with_client(nats.dns_client.clone(), Duration::from_millis(10))
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
    intent_dir: tempfile::TempDir,
    namespace_intent: NamespaceIntentStore,
}

impl TestNats {
    async fn start_without_buckets() -> Self {
        let connected = ployz_test_support::nats::TestNats::start_with_machines(&[
            machine_id("dns_machine"),
            machine_id("gateway_1"),
        ])
        .await;
        let dns_client = connected.machine_client(&machine_id("dns_machine")).await;
        let intent_dir = tempfile::tempdir().expect("intent dir");
        let namespace_intent =
            NamespaceIntentStore::new(intent_dir.path().join("namespace-intent.json"));

        Self {
            connected,
            dns_client,
            intent_dir,
            namespace_intent,
        }
    }

    async fn create_buckets(&self) {
        self.connected.bootstrap_resources().await;
    }

    async fn start_intent(&self) -> RunningIntentRuntime {
        start_intent_runtime(
            self.connected.controller.clone(),
            MachineRosterStore::new(self.intent_dir.path().join("machine-roster.json")),
            self.namespace_intent.clone(),
            self.intent_dir.path().join("machine-lifecycles.json"),
            Duration::from_millis(10),
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
