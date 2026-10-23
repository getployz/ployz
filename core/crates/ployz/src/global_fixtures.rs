//! Services and their Containers, as catch-up, Drain retirement and Deploy summary tests
//! observe them.

use ployz_core::{
    ContainerId, ContainerKind, ContainerObservation, ContainerResources,
    ContainerRuntimeObservation, HealthObservation, Machine, MachineId, MachineName, Namespace,
    Placement, PullPolicy, QualifiedService, RequestedServiceSpec, ResolvedServiceSpec,
    ResolvedUpdateConfig, RestartPolicy, ServiceContainerSpec, ServiceId, ServiceMode, ServiceName,
    ServiceObservation, UpdateConfig, WireGuardPublicKey, service_containers,
};

pub(crate) fn machine(hex: char, name: &str) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
}

pub(crate) fn qualified(namespace: &str, name: &str) -> QualifiedService {
    QualifiedService::new(
        Namespace::parse(namespace).unwrap(),
        ServiceName::parse(name).unwrap(),
    )
}

pub(crate) fn service_id(hex: char) -> ServiceId {
    ServiceId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(crate) fn container_id(hex: char) -> ContainerId {
    ContainerId::parse(hex.to_string().repeat(64)).unwrap()
}

pub(crate) fn requested(mode: ServiceMode) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: ServiceName::parse("api").unwrap(),
        mode,
        container: ServiceContainerSpec {
            image: "ghcr.io/getployz/api:1".into(),
            command: Vec::new(),
            entrypoint: Vec::new(),
            environment: Default::default(),
            sealed_identity: None,
            labels: Default::default(),
            hostname: None,
            extra_hosts: Vec::new(),
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            healthcheck: None,
            pull_policy: PullPolicy::Missing,
            init: None,
            user: None,
            working_directory: None,
            tty: false,
            open_stdin: false,
            privileged: false,
            pid_mode: None,
            resources: ContainerResources::default(),
            stop_timeout_secs: None,
            sysctls: Default::default(),
            restart: RestartPolicy::default(),
            ipv6: false,
        },
        placement: Placement::default(),
        ports: Vec::new(),
        mount_graph: Default::default(),
        pre_deploy: None,
        update: UpdateConfig::default(),
    }
}

pub(crate) fn global_service(
    identity: QualifiedService,
    id: char,
    placement: Placement,
    container: ContainerObservation,
) -> ServiceObservation {
    global_service_with_image(identity, id, placement, container, "ghcr.io/getployz/api:1")
}

pub(crate) fn global_service_with_image(
    identity: QualifiedService,
    id: char,
    placement: Placement,
    container: ContainerObservation,
    image: &str,
) -> ServiceObservation {
    let mut spec = requested(ServiceMode::Global);
    spec.name = identity.name.clone();
    spec.placement = placement;
    spec.container.image = image.into();
    grouped(
        identity,
        spec.to_resolved(service_id(id), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        container,
    )
}

pub(crate) fn grouped(
    identity: QualifiedService,
    spec: ResolvedServiceSpec,
    mut container: ContainerObservation,
) -> ServiceObservation {
    container
        .try_update(|parts| {
            parts.namespace = identity.namespace.clone();
            parts.resolved_spec = spec.clone();
        })
        .unwrap();
    ServiceObservation {
        identity,
        service_id: spec.service_id,
        containers: service_containers([container]),
        hook_containers: Vec::new(),
    }
}

pub(crate) fn running_on(machine: &Machine, hex: char) -> ContainerObservation {
    container_on(
        machine,
        hex,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
    )
}

pub(crate) fn created_on(machine: &Machine, hex: char) -> ContainerObservation {
    container_on(machine, hex, ContainerRuntimeObservation::Created)
}

fn container_on(
    machine: &Machine,
    hex: char,
    runtime: ContainerRuntimeObservation,
) -> ContainerObservation {
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        rotation: None,
        r#gen: None,
        container_id: container_id(hex),
        display_name: format!("slot-{hex}"),
        created_at_unix_nanos: 0,
        machine_id: machine.id,
        namespace: Namespace::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime,
        effective_healthcheck: None,
        resolved_spec: requested(ServiceMode::Global)
            .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        address: None,
        labels: Default::default(),
        stopped: false,
        run: None,
    })
    .unwrap()
}

/// `api` in `app` asked for `replicas` copies across Servers `1` to `servers`, where each
/// Server in `running` already runs one up-to-date copy.
pub(crate) fn replicated_plan(
    servers: u32,
    running: &[char],
    replicas: u32,
) -> crate::deploy::DeployPlan {
    let spec = requested(ServiceMode::Replicated {
        replicas: std::num::NonZeroU32::new(replicas).unwrap(),
    });
    let machines: Vec<_> = (1..=servers)
        .map(|n| {
            let hex = char::from_digit(n, 16).unwrap();
            machine(hex, &format!("server-{hex}"))
        })
        .collect();
    let resolved = spec
        .to_resolved(service_id('a'), ResolvedUpdateConfig::default())
        .expect("volume graph is scoped");
    let containers = running
        .iter()
        .map(|hex| {
            let on = machines
                .iter()
                .find(|machine| machine.id == MachineId::parse(hex.to_string().repeat(32)).unwrap())
                .expect("a running copy sits on a listed Server");
            let mut copy = running_on(on, *hex);
            copy.try_update(|parts| parts.resolved_spec = resolved.clone())
                .unwrap();
            copy
        })
        .collect();
    let snapshot = crate::deploy::DeploySnapshot {
        machines: machines
            .into_iter()
            .map(|machine| {
                ployz_core::MachineObservation::new(machine, ployz_core::MembershipObservation::Up)
            })
            .collect(),
        containers,
        ..Default::default()
    };
    let intent = ployz_core::DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&spec],
        Default::default(),
    );
    crate::deploy::plan_deploy(&intent, &snapshot).unwrap()
}
