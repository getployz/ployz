//! What Caddy dials from a kept route (#3682), and the route it leaves unchanged.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, CopyRotation, DeploymentLogId, HealthObservation, HttpProtocol,
    IngressHost, Machine, MachineId, MachineName, Namespace, PortPublication, ProxyRoutes,
    ResolvedServiceSpec, ServiceContainer, WireGuardPublicKey, proxy_routes, service_containers,
};
use serde_json::json;

use crate::ingress::{next_pass, route_acks};

fn machine(id: char) -> MachineId {
    MachineId::parse(id.to_string().repeat(32)).unwrap()
}

fn local() -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: machine('a'),
        name: MachineName::parse("node-a").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
}

fn copy(suffix: u8, machine: MachineId, rotation: CopyRotation) -> ContainerObservation {
    let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": "a".repeat(32),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "example.test/image", "pull_policy": "missing" },
        "ports": [PortPublication::Ingress {
            hostname: IngressHost::parse("api.example").unwrap(),
            load_balancer_port: 80.try_into().unwrap(),
            container_port: 80.try_into().unwrap(),
            http_protocol: HttpProtocol::Http,
        }],
    }))
    .unwrap();
    ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        rotation: Some(rotation),
        r#gen: None,
        container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
        display_name: format!("api-{suffix}"),
        created_at_unix_nanos: 0,
        machine_id: machine,
        namespace: Namespace::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: healthy(),
        effective_healthcheck: None,
        resolved_spec,
        address: Some(ContainerAddress([10, 210, suffix, 2].into())),
        labels: BTreeMap::new(),
        stopped: false,
        run: None,
    })
    .unwrap()
}

fn healthy() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    }
}

/// How a copy reads once a stop has withdrawn it, before the stop itself.
fn withdrawn() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Stopping,
    }
}

fn exited() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Exited { code: 137 }
}

fn observed(
    mut copy: ContainerObservation,
    runtime: ContainerRuntimeObservation,
) -> ContainerObservation {
    copy.try_update(|parts| parts.runtime = runtime).unwrap();
    copy
}

/// One proxy pass carrying `routed`, as `watch_caddy` runs it.
struct Pass {
    routes: ProxyRoutes,
    dialed: Option<BTreeSet<u8>>,
}

fn pass(
    copies: impl IntoIterator<Item = ContainerObservation>,
    down: &[MachineId],
    routed: &ProxyRoutes,
) -> Pass {
    let containers: Vec<ServiceContainer> = service_containers(copies);
    let (routes, projection) = next_pass(
        &local(),
        &containers,
        &BTreeMap::new(),
        |machine| down.contains(machine),
        routed,
    );
    let dialed = projection
        .sites
        .iter()
        .find(|site| site.hostname.as_str() == "api.example")
        .and_then(|site| site.proxied(HttpProtocol::Http))
        .map(|endpoints| {
            endpoints
                .iter()
                .map(|endpoint| endpoint.address.0.octets()[2])
                .collect()
        });
    Pass { routes, dialed }
}

fn routed_ids(routes: &ProxyRoutes) -> BTreeSet<u8> {
    routes
        .values()
        .flatten()
        .map(|serving| serving.address().0.octets()[2])
        .collect()
}

fn first_routed() -> ProxyRoutes {
    pass(
        [copy(1, machine('a'), CopyRotation::In)],
        &[],
        &ProxyRoutes::new(),
    )
    .routes
}

#[test]
fn a_volume_moves_sole_copy_leaves_caddy_once_withdrawn_but_stays_routed_and_acked() {
    let routed = first_routed();

    let mut carried = routed;
    for runtime in [withdrawn(), exited()] {
        let next = pass(
            [observed(
                copy(1, machine('a'), CopyRotation::In),
                runtime.clone(),
            )],
            &[],
            &carried,
        );

        assert_eq!(next.dialed, Some(BTreeSet::new()), "{runtime}");
        assert_eq!(routed_ids(&next.routes), BTreeSet::from([1]), "{runtime}");
        carried = next.routes;
    }

    let moved = pass(
        [
            observed(copy(1, machine('a'), CopyRotation::In), exited()),
            copy(2, machine('b'), CopyRotation::In),
        ],
        &[],
        &carried,
    );
    assert_eq!(moved.dialed, Some(BTreeSet::from([2])));
    assert_eq!(routed_ids(&moved.routes), BTreeSet::from([2]));
}

#[test]
fn a_released_withdrawal_puts_the_copy_back_in_caddy() {
    let withdrawn = pass(
        [observed(
            copy(1, machine('a'), CopyRotation::In),
            withdrawn(),
        )],
        &[],
        &first_routed(),
    );
    assert_eq!(withdrawn.dialed, Some(BTreeSet::new()));

    let released = pass(
        [copy(1, machine('a'), CopyRotation::In)],
        &[],
        &withdrawn.routes,
    );

    assert_eq!(released.dialed, Some(BTreeSet::from([1])));
}

#[test]
fn a_kept_copy_that_runs_again_is_back_in_caddy() {
    let stopped = pass(
        [
            observed(copy(1, machine('a'), CopyRotation::Out), exited()),
            observed(copy(2, machine('b'), CopyRotation::In), exited()),
        ],
        &[],
        &first_routed(),
    );
    assert_eq!(stopped.dialed, Some(BTreeSet::new()));

    let running = pass(
        [
            copy(1, machine('a'), CopyRotation::Out),
            observed(copy(2, machine('b'), CopyRotation::In), exited()),
        ],
        &[],
        &stopped.routes,
    );

    assert_eq!(running.dialed, Some(BTreeSet::from([1])));
}

#[test]
fn a_running_out_copy_stays_in_caddy_while_its_in_sibling_exits() {
    let next = pass(
        [
            copy(1, machine('a'), CopyRotation::Out),
            observed(copy(2, machine('b'), CopyRotation::In), exited()),
        ],
        &[],
        &first_routed(),
    );

    assert_eq!(next.dialed, Some(BTreeSet::from([1])));
}

#[test]
fn a_stopped_copy_on_a_down_machine_stays_in_caddy() {
    let next = pass(
        [observed(copy(1, machine('a'), CopyRotation::In), exited())],
        &[machine('a')],
        &first_routed(),
    );

    assert_eq!(next.dialed, Some(BTreeSet::from([1])));
}

#[test]
fn a_routed_copy_out_of_view_stays_in_caddy() {
    let next = pass(
        [copy(2, machine('b'), CopyRotation::Pending)],
        &[],
        &first_routed(),
    );

    assert_eq!(next.dialed, Some(BTreeSet::from([1])));
}

/// The route and its acks are exactly what [`proxy_routes`] carries, unfiltered,
/// for every rotation and runtime of two copies across three passes.
#[test]
fn caddys_filter_never_changes_the_carried_route_or_its_acks() {
    let rotations = [
        CopyRotation::In,
        CopyRotation::Out,
        CopyRotation::Pending,
        CopyRotation::Held(DeploymentLogId::parse("run").unwrap()),
    ];
    let runtimes = [
        healthy(),
        withdrawn(),
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        },
        ContainerRuntimeObservation::Created,
        ContainerRuntimeObservation::Restarting,
        ContainerRuntimeObservation::Paused,
        exited(),
        ContainerRuntimeObservation::Dead,
    ];
    let states: Vec<(CopyRotation, ContainerRuntimeObservation)> = rotations
        .iter()
        .flat_map(|rotation| {
            runtimes
                .iter()
                .map(move |runtime| (rotation.clone(), runtime.clone()))
        })
        .collect();
    let copies = |first: &(CopyRotation, ContainerRuntimeObservation),
                  second: &(CopyRotation, ContainerRuntimeObservation)| {
        [
            observed(copy(1, machine('a'), first.0.clone()), first.1.clone()),
            observed(copy(2, machine('b'), second.0.clone()), second.1.clone()),
        ]
    };
    let unfiltered =
        |copies: [ContainerObservation; 2], down: &[MachineId], routed: &ProxyRoutes| {
            let containers = service_containers(copies);
            proxy_routes(&containers, |machine| down.contains(machine), routed)
        };
    let start = pass(
        [
            copy(1, machine('a'), CopyRotation::In),
            copy(2, machine('b'), CopyRotation::In),
        ],
        &[],
        &ProxyRoutes::new(),
    )
    .routes;
    for down in [&[][..], &[machine('b')][..]] {
        for first in &states {
            for second in &states {
                let mut filtered = start.clone();
                let mut plain = start.clone();
                for state in [copies(first, second), copies(second, first)] {
                    filtered = pass(state.clone(), down, &filtered).routes;
                    plain = unfiltered(state, down, &plain);
                    assert_eq!(filtered, plain, "{first:?} {second:?} down {down:?}");
                    assert_eq!(route_acks(&filtered), route_acks(&plain));
                }
            }
        }
    }
}
