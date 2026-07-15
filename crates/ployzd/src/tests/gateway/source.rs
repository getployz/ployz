use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::{
    NatsIntentReader, RunningIntentService, start_intent_service,
};
use crate::role_testimony::RoleTestimonyCache;
use crate::roles::gateway::projection::{
    GatewayProjectedRoute, GatewayProjectionUpdate, GatewayUpstream, project_gateway,
};
use crate::roles::gateway::source::{
    GatewayCertificateStore, load_gateway_projection_update_from_nats,
};
use ployz_core::ids::RouteBindingId;
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::intent::RouteBindingState;
use ployz_core::machine::runtime::{
    MachineContainerObservationSnapshot, MachineFactsSnapshot, ManagedContainerObservation,
};
use ployz_core::operation::RouteTarget;
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, route_hostname, route_port, service_id,
};
use std::time::Duration;

#[tokio::test]
async fn gateway_source_loads_routes_and_current_observations_from_nats() {
    let nats = test_nats().await;
    let target = route_target("api.example.com", 443);

    nats.namespace_intent
        .replace_serving_target_entry(serving_target_entry("svc_api", "entry_1"))
        .await
        .expect("serving target entry stores");
    nats.namespace_intent
        .replace_route_binding(RouteBindingState {
            id: route_binding_id(),
            namespace_id: namespace_id("default"),
            target: target.clone(),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            origin: RouteBindingOrigin::Declared,
        })
        .await
        .expect("route stores");
    nats.facts
        .record_machine_facts(machine_facts(machine_snapshot(
            "machine_7",
            [managed_observation(
                "machine_7",
                "ctr_7",
                "svc_api",
                "entry_1",
            )],
        )));

    let certificate_dir = tempfile::tempdir().expect("gateway certificate directory");
    let update = load_gateway_projection_update_from_nats(
        &nats.intent_reader,
        &nats.facts,
        &GatewayCertificateStore::new(certificate_dir.path().to_path_buf()),
    )
    .await;
    let GatewayProjectionUpdate::Available(input) = update else {
        panic!("gateway source should be available, got {update:?}");
    };
    let projection = project_gateway(*input).expect("gateway projection succeeds");

    assert_eq!(
        projection.routes,
        vec![GatewayProjectedRoute {
            id: route_binding_id(),
            origin: RouteBindingOrigin::Declared,
            target,
            upstreams: vec![GatewayUpstream {
                machine_id: machine_id("machine_7"),
                container_id: container_id("ctr_7"),
                address: socket_addr("10.0.0.7", 8080),
            }],
            unroutable_containers: vec![],
        }]
    );
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    intent_reader: NatsIntentReader,
    facts: RoleTestimonyCache,
    _intent: RunningIntentService,
    _intent_dir: tempfile::TempDir,
    namespace_intent: NamespaceIntentStore,
}

async fn test_nats() -> TestNats {
    let gateway_machine = machine_id("gateway_machine");
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        gateway_machine.clone(),
        machine_id("machine_7"),
    ])
    .await;
    let machine_client = nats.machine_client(&gateway_machine).await;
    let lifecycle_dir = tempfile::tempdir().expect("lifecycle dir");
    let namespace_intent = NamespaceIntentStore::new(
        crate::control::store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let intent_core_store = crate::control::store::CoreStore::open_in_memory()
        .await
        .expect("core store opens");
    crate::tests::support::intent::initialize_disabled_ingress(&intent_core_store).await;
    let intent = start_intent_service(
        nats.controller.clone(),
        machine_id("machine_a"),
        namespace_intent.clone(),
        intent_core_store,
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    TestNats {
        _nats: nats,
        intent_reader: NatsIntentReader::new(machine_client)
            .with_request_timeout(Duration::from_secs(1)),
        facts: RoleTestimonyCache::default(),
        _intent: intent,
        _intent_dir: lifecycle_dir,
        namespace_intent,
    }
}

fn machine_snapshot(
    machine_id_value: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(machine_id_value), containers)
        .expect("matching machine snapshot")
}

fn machine_facts(snapshot: MachineContainerObservationSnapshot) -> MachineFactsSnapshot {
    MachineFactsSnapshot::try_new(
        snapshot.machine_id().clone(),
        snapshot,
        None,
        test_disk_space(),
        ployz_core::image::OciPlatform::current(),
        1,
    )
    .expect("machine facts are valid")
}

fn test_disk_space() -> ployz_core::machine::runtime::MachineDiskSpace {
    ployz_test_support::fixtures::test_disk_space()
}

fn managed_observation(
    machine_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    namespace_revision_entry_id_value: &str,
) -> ManagedContainerObservation {
    containers::observation(machine_id_value, container_id_value)
        .with(
            containers::identity(service_id_value)
                .entry(namespace_revision_entry_id_value)
                .operation("op_123")
                .step("step_1"),
        )
        .running_at(endpoint_ip("10.0.0.7"))
        .build()
}

fn route_target(hostname: &str, _port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname))
}

fn route_binding_id() -> RouteBindingId {
    RouteBindingId::try_new("route_api").expect("valid route binding id")
}

fn endpoint_ip(ip: &str) -> std::net::IpAddr {
    ip.parse().expect("valid endpoint ip")
}

fn socket_addr(ip: &str, port: u16) -> std::net::SocketAddr {
    std::net::SocketAddr::new(endpoint_ip(ip), port)
}
