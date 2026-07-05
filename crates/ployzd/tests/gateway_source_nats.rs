use async_nats::jetstream;
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, route_hostname,
    route_port, service_id,
};
use ployzd::gateway::{
    GatewayProjectedRoute, GatewayProjectionUpdate, GatewayUpstream, project_gateway,
};
use ployzd::gateway_source::load_gateway_projection_update_from_nats;
use ployzd::intent::{NatsIntentReader, RunningIntentRuntime, start_intent_runtime};
use ployzd::machine_roster::MachineRosterStore;
use ployzd::namespace_intent::NamespaceIntentStore;
use std::path::PathBuf;
use std::time::Duration;

#[tokio::test]
async fn gateway_source_loads_routes_and_current_observations_from_nats() {
    let nats = test_nats().await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");
    let target = route_target("api.example.com", 443);

    nats.namespace_intent
        .replace_serving_target_entry(ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
        })
        .await
        .expect("serving target entry stores");
    nats.namespace_intent
        .replace_route_binding(RouteBindingState {
            namespace_id: namespace_id("default"),
            target: target.clone(),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
        })
        .await
        .expect("route stores");
    AsyncNatsObservationStore::from_jetstream(&nats.machine_7_jetstream)
        .await
        .expect("open machine_7 observation store")
        .replace_machine_containers(&machine_snapshot(
            "machine_7",
            [managed_observation(
                "machine_7",
                "ctr_7",
                "svc_api",
                "entry_1",
            )],
        ))
        .await
        .expect("machine snapshot stores");

    let update = load_gateway_projection_update_from_nats(&nats.intent_reader, &observations).await;
    let GatewayProjectionUpdate::SourceAvailable(input) = update else {
        panic!("gateway source should be available, got {update:?}");
    };
    let projection = project_gateway(input).expect("gateway projection succeeds");

    assert_eq!(
        projection.routes,
        vec![GatewayProjectedRoute {
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

#[tokio::test]
async fn gateway_source_reports_unreadable_intent_as_unavailable() {
    let nats = test_nats().await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");
    std::fs::write(&nats.namespace_intent_file, b"{").expect("corrupt namespace intent file");

    let update = load_gateway_projection_update_from_nats(&nats.intent_reader, &observations).await;

    assert!(matches!(
        update,
        GatewayProjectionUpdate::SourceUnavailable(_)
    ));
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// The gateway machine's Machine principal: the read side (the gateway
    /// runs as the machine's Machine user in v1).
    machine_jetstream: jetstream::Context,
    intent_reader: NatsIntentReader,
    _intent: RunningIntentRuntime,
    _intent_dir: tempfile::TempDir,
    namespace_intent: NamespaceIntentStore,
    namespace_intent_file: PathBuf,
    /// `machine_7`'s Machine principal: each machine may only write its own
    /// observation keys, so the workload machine seeds its own snapshot.
    machine_7_jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let gateway_machine = machine_id("gateway_machine");
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        gateway_machine.clone(),
        machine_id("machine_7"),
    ])
    .await;
    nats.bootstrap_resources().await;
    let machine_client = nats.machine_client(&gateway_machine).await;
    let machine_jetstream = jetstream::new(machine_client.clone());
    let machine_7_jetstream = jetstream::new(nats.machine_client(&machine_id("machine_7")).await);
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
        _nats: nats,
        machine_jetstream,
        intent_reader: NatsIntentReader::new(machine_client)
            .with_request_timeout(Duration::from_secs(1)),
        _intent: intent,
        _intent_dir: lifecycle_dir,
        namespace_intent,
        namespace_intent_file,
        machine_7_jetstream,
    }
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

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

fn endpoint_ip(ip: &str) -> std::net::IpAddr {
    ip.parse().expect("valid endpoint ip")
}

fn socket_addr(ip: &str, port: u16) -> std::net::SocketAddr {
    std::net::SocketAddr::new(endpoint_ip(ip), port)
}
