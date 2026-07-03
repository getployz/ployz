use async_nats::jetstream;
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::RouteTarget;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id,
    route_hostname, route_port, service_id,
};
use ployzd::gateway::{
    GatewayProjectedRoute, GatewayProjectionError, GatewayProjectionUpdate, GatewayUpstream,
    project_gateway,
};
use ployzd::gateway_source::{
    load_gateway_projection_update_from_nats,
    load_gateway_projection_update_from_nats_with_stale_after,
};
use std::time::Duration;

#[tokio::test]
async fn gateway_source_loads_routes_and_current_observations_from_nats() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");
    let target = route_target("api.example.com", 443);

    routes
        .replace_serving_target_entry(&ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
        })
        .await
        .expect("serving target entry stores");
    routes
        .replace_route_binding(&RouteBindingState {
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

    let update = load_gateway_projection_update_from_nats(&routes, &observations).await;
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
async fn gateway_source_marks_old_observations_stale_before_projection() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");
    let target = route_target("api.example.com", 443);

    routes
        .replace_serving_target_entry(&ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
        })
        .await
        .expect("serving target entry stores");
    routes
        .replace_route_binding(&RouteBindingState {
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

    let update = load_gateway_projection_update_from_nats_with_stale_after(
        &routes,
        &observations,
        Duration::ZERO,
    )
    .await;
    let GatewayProjectionUpdate::SourceAvailable(input) = update else {
        panic!("gateway source should be available, got {update:?}");
    };
    let projection = project_gateway(input).expect("gateway projection succeeds");

    assert_eq!(
        projection.routes,
        vec![GatewayProjectedRoute {
            target,
            upstreams: vec![],
            unroutable_containers: vec![],
        }]
    );
}

#[tokio::test]
async fn gateway_source_reports_invalid_route_state_as_invalid_source() {
    let nats = test_nats().await;
    let routes = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.machine_jetstream)
        .await
        .expect("open observation store");
    let core_bucket = nats
        .jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open raw core bucket");
    let payload = serde_json::to_vec(&RouteBindingState {
        namespace_id: namespace_id("default"),
        target: route_target("api.example.com", 443),
        endpoint_port: route_port(8080),
        service_id: service_id("svc_api"),
    })
    .expect("route state encodes");
    core_bucket
        .put("routes.deadbeef.443", payload.into())
        .await
        .expect("store corrupt route key");

    let update = load_gateway_projection_update_from_nats(&routes, &observations).await;

    let GatewayProjectionUpdate::SourceInvalid(GatewayProjectionError::InvalidSource { message }) =
        update
    else {
        panic!("gateway source should be invalid, got {update:?}");
    };
    assert!(message.contains("route binding state key"));
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: route-state writes and bucket administration.
    jetstream: jetstream::Context,
    /// The gateway machine's Machine principal: the read side (the gateway
    /// runs as the machine's Machine user in v1).
    machine_jetstream: jetstream::Context,
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
    let machine_jetstream = jetstream::new(nats.machine_client(&gateway_machine).await);
    let machine_7_jetstream = jetstream::new(nats.machine_client(&machine_id("machine_7")).await);
    let jetstream = nats.jetstream.clone();

    TestNats {
        _nats: nats,
        jetstream,
        machine_jetstream,
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
        .service(service_id_value)
        .entry(namespace_revision_entry_id_value)
        .operation("op_123")
        .step("step_1")
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
