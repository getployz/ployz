use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{
    GatewayServingStatus, GatewayStatusObservation, MachinePublicIpObservation, RouteBindingState,
};
use ployz_test_support::ids::{machine_id, namespace_id, route_hostname, route_port, service_id};
use ployzd::roles::dns::projection::{
    DnsAnswer, DnsProjectionUpdate, DnsRecordSet, DnsProjector, DnsServingState, project_dns,
};
use ployzd::roles::dns::source::load_dns_projection_update_from_nats;
use ployzd::intent::service::{NatsIntentReader, RunningIntentService, start_intent_service};
use ployzd::intent::machine_roster::MachineRosterStore;
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::fact_cache::FactCache;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

#[tokio::test]
async fn dns_source_loads_active_route_hostnames_and_serving_gateway_public_ips_from_nats() {
    let nats = test_nats().await;

    nats.namespace_intent
        .replace_route_binding(active_route_state("api.example.com", 443, 8080))
        .await
        .expect("api route stores");
    nats.namespace_intent
        .replace_route_binding(active_route_state("www.example.com", 443, 8080))
        .await
        .expect("www route stores");
    nats.facts.record_gateway_status(gateway_status(
        "gateway_1",
        GatewayServingStatus::Current,
        2,
    ));
    nats.facts.record_gateway_status(gateway_status(
        "gateway_2",
        GatewayServingStatus::LastKnownGood,
        1,
    ));
    nats.facts.record_gateway_status(gateway_status(
        "gateway_3",
        GatewayServingStatus::Current,
        0,
    ));
    nats.facts
        .record_machine_facts(machine_facts("gateway_1", Some([203, 0, 113, 10])));
    nats.facts
        .record_machine_facts(machine_facts("gateway_2", Some([203, 0, 113, 11])));
    nats.facts
        .record_machine_facts(machine_facts("gateway_3", Some([203, 0, 113, 12])));
    nats.facts
        .record_machine_facts(machine_facts("edge_4", Some([203, 0, 113, 13])));

    let update = load_dns_projection_update_from_nats(&nats.intent_reader, &nats.facts).await;
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
async fn dns_runtime_applies_nats_dns_changes_without_control_runtime() {
    let nats = test_nats().await;
    let mut runtime = DnsProjector::new();
    let hostname = route_hostname("api.example.com");

    nats.namespace_intent
        .replace_route_binding(active_route_state("api.example.com", 443, 8080))
        .await
        .expect("route stores");
    nats.facts.record_gateway_status(gateway_status(
        "gateway_1",
        GatewayServingStatus::Current,
        1,
    ));
    nats.facts
        .record_machine_facts(machine_facts("gateway_1", Some([203, 0, 113, 10])));

    let first_tick = runtime.apply_source_update(
        load_dns_projection_update_from_nats(&nats.intent_reader, &nats.facts).await,
    );
    assert_eq!(
        first_tick.serving,
        DnsServingState::Current { record_count: 1 }
    );
    assert_eq!(
        runtime.answers().answers_for(&hostname),
        &[DnsAnswer::try_new("203.0.113.10").expect("valid answer")]
    );

    nats.facts
        .record_machine_facts(machine_facts("gateway_1", Some([203, 0, 113, 20])));
    let second_tick = runtime.apply_source_update(
        load_dns_projection_update_from_nats(&nats.intent_reader, &nats.facts).await,
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
    _nats: ployz_test_support::nats::TestNats,
    intent_reader: NatsIntentReader,
    facts: FactCache,
    _intent: RunningIntentService,
    _intent_dir: tempfile::TempDir,
    namespace_intent: NamespaceIntentStore,
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
    let machine_client = nats.machine_client(&dns_machine).await;
    let lifecycle_dir = tempfile::tempdir().expect("lifecycle dir");
    let namespace_intent = NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let intent = start_intent_service(
        nats.controller.clone(),
        MachineRosterStore::new(
            ployzd::core_store::CoreStore::open_in_memory()
                .await
                .expect("open core store"),
        ),
        namespace_intent.clone(),
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    TestNats {
        _nats: nats,
        intent_reader: NatsIntentReader::new(machine_client)
            .with_request_timeout(Duration::from_secs(1)),
        facts: FactCache::default(),
        _intent: intent,
        _intent_dir: lifecycle_dir,
        namespace_intent,
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

fn machine_facts(machine_id_value: &str, public_ip: Option<[u8; 4]>) -> MachineFactsSnapshot {
    let machine_id = machine_id(machine_id_value);
    MachineFactsSnapshot::try_new(
        machine_id.clone(),
        MachineContainerObservationSnapshot::try_new(machine_id, Vec::new())
            .expect("empty container facts are valid"),
        public_ip.map(|address| machine_public_ip(machine_id_value, address)),
        1,
    )
    .expect("machine facts are valid")
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
