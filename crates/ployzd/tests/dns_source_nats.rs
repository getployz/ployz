use async_nats::jetstream;
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    GatewayServingStatus, GatewayStatusObservation, MachinePublicIpObservation, RouteBindingState,
};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::ids::{machine_id, namespace_id, route_hostname, route_port, service_id};
use ployzd::dns::{
    DnsAnswer, DnsProjectionUpdate, DnsRecordSet, DnsRuntime, DnsServingState, project_dns,
};
use ployzd::dns_source::load_dns_projection_update_from_nats;
use ployzd::intent::{NatsIntentReader, RunningIntentRuntime, start_intent_runtime};
use ployzd::machine_roster::MachineRosterStore;
use ployzd::namespace_intent::NamespaceIntentStore;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

#[tokio::test]
async fn dns_source_loads_active_route_hostnames_and_serving_gateway_public_ips_from_nats() {
    let nats = test_nats().await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");

    nats.namespace_intent
        .replace_route_binding(active_route_state("api.example.com", 443, 8080))
        .await
        .expect("api route stores");
    nats.namespace_intent
        .replace_route_binding(active_route_state("www.example.com", 443, 8080))
        .await
        .expect("www route stores");
    let gateway_1 = nats.machine_observations("gateway_1").await;
    let gateway_2 = nats.machine_observations("gateway_2").await;
    let gateway_3 = nats.machine_observations("gateway_3").await;
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
        .replace_machine_public_ip(&machine_public_ip("gateway_1", [203, 0, 113, 10]))
        .await
        .expect("gateway one public ip stores");
    gateway_2
        .replace_machine_public_ip(&machine_public_ip("gateway_2", [203, 0, 113, 11]))
        .await
        .expect("gateway two public ip stores");
    gateway_3
        .replace_machine_public_ip(&machine_public_ip("gateway_3", [203, 0, 113, 12]))
        .await
        .expect("empty gateway public ip stores");
    nats.machine_observations("edge_4")
        .await
        .replace_machine_public_ip(&machine_public_ip("edge_4", [203, 0, 113, 13]))
        .await
        .expect("non-gateway public ip stores");

    let update = load_dns_projection_update_from_nats(&nats.intent_reader, &observations).await;
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
async fn dns_source_reports_unreadable_intent_as_unavailable() {
    let nats = test_nats().await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");
    std::fs::write(&nats.namespace_intent_file, b"{").expect("corrupt namespace intent file");

    let update = load_dns_projection_update_from_nats(&nats.intent_reader, &observations).await;

    assert_eq!(update, DnsProjectionUpdate::SourceUnavailable);
}

#[tokio::test]
async fn dns_runtime_applies_nats_dns_changes_without_control_runtime() {
    let nats = test_nats().await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");
    let mut runtime = DnsRuntime::new();
    let hostname = route_hostname("api.example.com");

    nats.namespace_intent
        .replace_route_binding(active_route_state("api.example.com", 443, 8080))
        .await
        .expect("route stores");
    let gateway_1 = nats.machine_observations("gateway_1").await;
    gateway_1
        .replace_gateway_status(&gateway_status(
            "gateway_1",
            GatewayServingStatus::Current,
            1,
        ))
        .await
        .expect("gateway status stores");
    gateway_1
        .replace_machine_public_ip(&machine_public_ip("gateway_1", [203, 0, 113, 10]))
        .await
        .expect("public ip stores");

    let first_tick = runtime.apply_source_update(
        load_dns_projection_update_from_nats(&nats.intent_reader, &observations).await,
    );
    assert_eq!(
        first_tick.serving,
        DnsServingState::Current { record_count: 1 }
    );
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid answer")]
    );

    gateway_1
        .replace_machine_public_ip(&machine_public_ip("gateway_1", [203, 0, 113, 20]))
        .await
        .expect("updated public ip stores");
    let second_tick = runtime.apply_source_update(
        load_dns_projection_update_from_nats(&nats.intent_reader, &observations).await,
    );

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
    nats: ployz_test_support::nats::TestNats,
    /// The DNS machine's Machine principal: the read side (DNS runs as the
    /// machine's Machine user in v1).
    machine_jetstream: jetstream::Context,
    intent_reader: NatsIntentReader,
    _intent: RunningIntentRuntime,
    _intent_dir: tempfile::TempDir,
    namespace_intent: NamespaceIntentStore,
    namespace_intent_file: PathBuf,
}

impl TestNats {
    /// The observation store connected as the given machine — each machine may
    /// only write its own observation keys.
    async fn machine_observations(&self, machine_id_value: &str) -> AsyncNatsObservationStore {
        let machine_client = self
            .nats
            .machine_client(&machine_id(machine_id_value))
            .await;
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(machine_client))
            .await
            .expect("open machine observation store")
    }
}

async fn test_nats() -> TestNats {
    let dns_machine = machine_id("dns_machine");
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        dns_machine.clone(),
        machine_id("gateway_1"),
        machine_id("gateway_2"),
        machine_id("gateway_3"),
        machine_id("edge_4"),
    ])
    .await;
    nats.bootstrap_resources().await;
    let machine_client = nats.machine_client(&dns_machine).await;
    let machine_jetstream = jetstream::new(machine_client.clone());
    let lifecycle_dir = tempfile::tempdir().expect("lifecycle dir");
    let namespace_intent_file = lifecycle_dir.path().join("namespace-intent.json");
    let namespace_intent = NamespaceIntentStore::new(namespace_intent_file.clone());
    let intent = start_intent_runtime(
        nats.controller.clone(),
        MachineRosterStore::new(lifecycle_dir.path().join("machine-roster.json")),
        namespace_intent.clone(),
        lifecycle_dir.path().join("machine-lifecycles.json"),
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    TestNats {
        nats,
        machine_jetstream,
        intent_reader: NatsIntentReader::new(machine_client)
            .with_request_timeout(Duration::from_secs(1)),
        _intent: intent,
        _intent_dir: lifecycle_dir,
        namespace_intent,
        namespace_intent_file,
    }
}

fn active_route_state(hostname: &str, public_port: u16, endpoint_port: u16) -> RouteBindingState {
    RouteBindingState {
        namespace_id: namespace_id("default"),
        target: route_target(hostname, public_port),
        endpoint_port: route_port(endpoint_port),
        service_id: service_id("svc_api"),
    }
}

fn gateway_status(
    machine_id_value: &str,
    serving: GatewayServingStatus,
    route_count: usize,
) -> GatewayStatusObservation {
    GatewayStatusObservation {
        machine_id: machine_id(machine_id_value),
        listen_addr: SocketAddr::from(([127, 0, 0, 1], 8080)),
        serving,
        route_count,
    }
}

fn machine_public_ip(machine_id_value: &str, address: [u8; 4]) -> MachinePublicIpObservation {
    MachinePublicIpObservation {
        machine_id: machine_id(machine_id_value),
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
