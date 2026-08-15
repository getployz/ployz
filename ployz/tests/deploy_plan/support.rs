pub(super) use std::{
    net::Ipv6Addr,
    num::{NonZeroU16, NonZeroU32},
};

pub(super) use ployz::deploy::{
    DeployOperation, DeployPlan, DeploySnapshot, FailedOperation, PlanError, PlanOptions,
    ReplacementCompensation, ReplacementOperation, RestartAttempt, compare_specs, plan_deploy,
};
pub(super) use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation, ContainerPath,
    ContainerResources, ContainerRuntimeObservation, DeviceMapping, DeviceReservation,
    DockerVolumeId, DockerVolumeName, HealthObservation, HostBind, LogDriver, Machine, MachineId,
    MachineName, MachineObservation, MachinePath, MachineSelector, MachineSubnet,
    ManagementAddress, MembershipObservation, PidMode, Placement, PortPublication, PreDeployHook,
    PullPolicy, RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig, RestartPolicy,
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
            config_mounts: Vec::new(),
            restart: RestartPolicy::default(),
        },
        placement: Placement::default(),
        ports: Vec::new(),
        volumes: Vec::new(),
        mounts: Vec::new(),
        configs: Vec::new(),
        pre_deploy: None,
        caddy_config: None,
        update: UpdateConfig::default(),
    }
}

pub(super) fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation {
        machine: Machine {
            id: machine_id(hex),
            name: MachineName::parse(name).unwrap(),
            subnet: MachineSubnet(
                format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                    .parse()
                    .unwrap(),
            ),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        membership: MembershipObservation::Up,
        selected_endpoint: None,
    }
}

pub(super) fn machine_id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(super) fn service_id(hex: char) -> ServiceId {
    ServiceId::parse(hex.to_string().repeat(32)).unwrap()
}

pub(super) fn container_id(hex: char) -> ContainerId {
    ContainerId::parse(hex.to_string().repeat(64)).unwrap()
}

pub(super) fn add_named_volume(requested: &mut RequestedServiceSpec, name: &str) {
    let reference = ServiceVolumeReference::parse(name).unwrap();
    requested.volumes.push(ServiceVolume {
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
    requested.mounts.push(ServiceMount {
        volume: reference,
        target: ContainerPath::parse(format!("/{name}")).unwrap(),
        read_only: false,
    });
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
        service_id: *service_id,
        service_name: requested.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: ResolvedServiceSpec {
            service_id: *service_id,
            name: requested.name.clone(),
            mode: requested.mode.clone(),
            container: requested.container.clone(),
            placement: requested.placement.clone(),
            ports: requested.ports.clone(),
            volumes: requested.volumes.clone(),
            mounts: requested.mounts.clone(),
            configs: requested.configs.clone(),
            pre_deploy: requested.pre_deploy.clone(),
            caddy_config: requested.caddy_config.clone(),
            update: ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: requested.update.monitor_millis,
            },
        },
        address: None,
        labels: Default::default(),
    }
}
