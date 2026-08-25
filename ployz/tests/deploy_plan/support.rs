pub(super) use std::{
    collections::BTreeMap,
    net::Ipv6Addr,
    num::{NonZeroU16, NonZeroU32},
};

pub(super) use ployz::deploy::{
    DeployIntent, DeployOperation, DeployPreview, DeploySnapshot, EliminatingConstraint,
    EliminatingConstraints, IngressContext, PlanError, PlanOptions, ReplacementOperation,
    ServiceAttempt, VolumeSnapshot, compare_specs, preview_deploy,
};

pub(super) fn plan_deploy<'a>(
    requested: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    snapshot: &DeploySnapshot,
    options: PlanOptions,
) -> Result<DeployPreview, PlanError> {
    preview_deploy(
        &DeployIntent::apply_all(ProjectName::parse("app").unwrap(), requested, options),
        snapshot,
        IngressContext::default(),
    )
}

pub(super) fn operations(preview: &DeployPreview) -> Vec<DeployOperation> {
    preview
        .operations
        .iter()
        .map(|row| row.operation.clone())
        .collect()
}

pub(super) fn assert_no_eligible(
    result: Result<DeployPreview, PlanError>,
    expected: &[EliminatingConstraint],
    display_needles: &[&str],
) {
    let error = result.expect_err("plan should find no eligible Machine");
    let display = error.to_string();
    assert_eq!(
        error,
        PlanError::NoEligibleMachines {
            constraints: EliminatingConstraints::new(expected.to_vec()),
        }
    );
    assert!(
        display.starts_with("no machines available that satisfy all constraints: "),
        "{display}"
    );
    for needle in display_needles {
        assert!(
            display.contains(needle),
            "expected {needle:?} in {display:?}"
        );
    }
}

pub(super) use ployz_core::{
    AdvertisedEndpoint, BridgeEndpointCapacity, ContainerId, ContainerKind, ContainerObservation,
    ContainerPath, ContainerResources, ContainerRuntimeObservation, DeviceMapping,
    DeviceReservation, DockerVolume, DockerVolumeId, DockerVolumeName,
    DockerVolumeStorageObservation, HealthObservation, HostBind, LogDriver, MANAGED_LABEL, Machine,
    MachineId, MachineName, MachineObservation, MachinePath, MachineTarget, ManagementAddress,
    MembershipObservation, PROJECT_NAME_LABEL, PidMode, Placement, PortPublication, PreDeployHook,
    ProjectName, PullPolicy, RequestedServiceSpec, ResolvedUpdateConfig, RestartPolicy,
    ServiceContainerSpec, ServiceId, ServiceMode, ServiceMount, ServiceName, ServiceVolume,
    ServiceVolumeReference, SpecChange, TransportProtocol, Ulimit, UpdateConfig, UpdateOrder,
    VolumeSource, WireGuardPublicKey,
};
pub(super) fn requested(mode: ServiceMode) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: ServiceName::parse("api").unwrap(),
        mode,
        container: ServiceContainerSpec {
            image: "ghcr.io/getployz/api:1".into(),
            command: Vec::new(),
            entrypoint: Vec::new(),
            environment: Default::default(),
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
            log_driver: None,
            resources: ContainerResources::default(),
            stop_timeout_secs: None,
            sysctls: Default::default(),
            restart: RestartPolicy::default(),
        },
        placement: Placement::default(),
        ports: Vec::new(),
        volume_graph: Default::default(),
        config_graph: Default::default(),
        pre_deploy: None,
        ingress_proxy_fragment: None,
        update: UpdateConfig::default(),
    }
}

pub(super) fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation::new(
        Machine {
            id: machine_id(hex),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        MembershipObservation::Up,
    )
}

pub(super) fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(super) fn capacity(
    entries: impl IntoIterator<Item = (char, u64)>,
) -> Option<BTreeMap<MachineId, BridgeEndpointCapacity>> {
    Some(
        entries
            .into_iter()
            .map(|(machine, free)| (machine_id(machine), BridgeEndpointCapacity::new(free, 0)))
            .collect(),
    )
}

pub(super) fn service_id(hex: char) -> ServiceId {
    ServiceId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(super) fn container_id(hex: char) -> ContainerId {
    ContainerId::parse(hex.to_string().repeat(64)).unwrap()
}

pub(super) fn add_named_volume(requested: &mut RequestedServiceSpec, name: &str) {
    let reference = ServiceVolumeReference::parse(name).unwrap();
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mut mounts = requested.volume_graph.mounts().to_vec();
    volumes.push(ServiceVolume {
        reference: reference.clone(),
        source: VolumeSource::Named {
            name: DockerVolumeName::parse(name).unwrap(),
            external: false,
            driver: None,
            labels: Default::default(),
            no_copy: false,
            subpath: None,
        },
    });
    mounts.push(ServiceMount {
        volume: reference,
        target: ContainerPath::parse(format!("/{name}")).unwrap(),
        read_only: false,
    });
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
}

pub(super) fn app_volume(logical: &str) -> DockerVolumeName {
    ProjectName::parse("app")
        .unwrap()
        .volume_name(&DockerVolumeName::parse(logical).unwrap())
}

pub(super) fn observed_volume(machine_id: MachineId, logical: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id,
            name: app_volume(logical),
        },
        options: Default::default(),
        labels: Default::default(),
        storage: DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}

pub(super) fn owned_volume(machine_id: MachineId, logical: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id,
            name: app_volume(logical),
        },
        options: Default::default(),
        labels: BTreeMap::from([
            (MANAGED_LABEL.to_owned(), String::new()),
            (PROJECT_NAME_LABEL.to_owned(), "app".to_owned()),
        ]),
        storage: DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    }
}

pub(super) fn scoped_spec(spec: &RequestedServiceSpec) -> RequestedServiceSpec {
    let project = ProjectName::parse("app").unwrap();
    let mut spec = spec.clone();
    let mut volumes = spec.volume_graph.volumes().to_vec();
    let mounts = spec.volume_graph.mounts().to_vec();
    for volume in &mut volumes {
        volume.source.scope_to_project(&project);
    }
    spec.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
    spec
}

pub(super) fn host_port(port: u16) -> PortPublication {
    PortPublication::Host {
        bind: HostBind::All,
        published_port: NonZeroU16::new(port).unwrap(),
        container_port: NonZeroU16::new(port).unwrap(),
        transport_protocol: TransportProtocol::Tcp,
    }
}

pub(super) fn container(
    hex: char,
    machine_hex: char,
    requested: &RequestedServiceSpec,
    service_id: &ServiceId,
) -> ContainerObservation {
    ContainerObservation {
        container_id: container_id(hex),
        display_name: format!("{}-{hex}", requested.name),
        created_at_unix_nanos: 0,
        machine_id: machine_id(machine_hex),
        project_name: ProjectName::parse("app").unwrap(),
        service_id: *service_id,
        service_name: requested.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: scoped_spec(requested).to_resolved(
            *service_id,
            ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: requested.update.monitor_millis,
            },
        ),
        address: None,
        labels: Default::default(),
    }
}
