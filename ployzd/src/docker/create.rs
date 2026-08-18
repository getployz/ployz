use std::{
    collections::{BTreeSet, HashMap},
    net::IpAddr,
};

use bollard::models::{
    ContainerCreateBody, DeviceMapping as DockerDeviceMapping, DeviceRequest, HealthConfig,
    HostConfig, HostConfigLogConfig, Mount, MountBindOptions, MountBindOptionsPropagationEnum,
    MountTmpfsOptions, MountType, MountVolumeOptions, MountVolumeOptionsDriverConfig, PortBinding,
    PortMap, ResourcesUlimits, RestartPolicy, RestartPolicyNameEnum,
};
use ployz_core::{
    BindPropagation, BindRecursive, ContainerKind, HEALTHCHECK_DISABLE_SENTINEL, HealthcheckSpec,
    HostBind, MachineGateway, MachineId, PortPublication, ProjectName, ResolvedServiceSpec,
    ServiceVolumeGraph, TransportProtocol, VolumeSource,
};

use super::{
    Error, LABEL_HOOK, LABEL_HOOK_PRE_DEPLOY, LABEL_MANAGED, LABEL_PROJECT_NAME, LABEL_SERVICE_ID,
    LABEL_SERVICE_NAME,
};

pub(super) fn container_create_body(
    machine_id: &MachineId,
    gateway: MachineGateway,
    kind: ContainerKind,
    project_name: &ProjectName,
    spec: &ResolvedServiceSpec,
) -> Result<ContainerCreateBody, Error> {
    let hook = match kind {
        ContainerKind::ServiceContainer => None,
        ContainerKind::PreDeployHook => Some(
            spec.pre_deploy
                .as_ref()
                .ok_or(Error::MissingPreDeployHook)?,
        ),
    };
    let container = &spec.container;
    let mut environment = container.environment.clone();
    if let Some(hook) = hook {
        environment.extend(hook.environment.clone());
        environment.insert("PLOYZ_HOOK_PRE_DEPLOY".into(), "true".into());
    }
    environment.insert("PLOYZ_MACHINE_ID".into(), machine_id.to_string());
    let mut labels = HashMap::from([
        (LABEL_MANAGED.into(), String::new()),
        (LABEL_PROJECT_NAME.into(), project_name.to_string()),
        (LABEL_SERVICE_ID.into(), spec.service_id.to_string()),
        (LABEL_SERVICE_NAME.into(), spec.name.to_string()),
    ]);
    if hook.is_some() {
        labels.insert(LABEL_HOOK.into(), LABEL_HOOK_PRE_DEPLOY.into());
    }
    let mounts = docker_mounts(&spec.volume_graph)?;
    let interface_addresses = if hook.is_none()
        && spec.ports.iter().any(|port| {
            matches!(
                port,
                PortPublication::Host {
                    bind: HostBind::Prefix { .. },
                    ..
                }
            )
        }) {
        crate::network::interface_addresses()?
    } else {
        Vec::new()
    };
    let (exposed_ports, port_bindings) = if hook.is_none() {
        docker_ports(&spec.ports, &interface_addresses)?
    } else {
        (None, None)
    };
    let resources = &container.resources;
    let host_config = HostConfig {
        dns: Some(vec![gateway.0.to_string()]),
        dns_search: Some(vec!["internal".into()]),
        dns_options: Some(vec!["ndots:1".into()]),
        init: container.init,
        network_mode: Some(crate::network::DOCKER_NETWORK_NAME.into()),
        log_config: container
            .log_driver
            .as_ref()
            .map(|driver| HostConfigLogConfig {
                typ: Some(driver.name.clone()),
                config: Some(driver.options.clone().into_iter().collect()),
            }),
        port_bindings,
        restart_policy: Some(docker_restart(if hook.is_some() {
            ployz_core::RestartPolicy::No
        } else {
            spec.container.restart
        })),
        mounts: (!mounts.is_empty()).then_some(mounts),
        cap_add: (!container.cap_add.is_empty()).then(|| container.cap_add.clone()),
        cap_drop: (!container.cap_drop.is_empty()).then(|| container.cap_drop.clone()),
        pid_mode: container.pid_mode.as_ref().map(ToString::to_string),
        privileged: Some(
            hook.and_then(|hook| hook.privileged)
                .unwrap_or(container.privileged),
        ),
        sysctls: (!container.sysctls.is_empty())
            .then(|| container.sysctls.clone().into_iter().collect()),
        ..docker_resources(resources)
    };
    Ok(ContainerCreateBody {
        user: hook
            .and_then(|hook| hook.user.clone())
            .or_else(|| container.user.clone()),
        tty: Some(container.tty),
        open_stdin: Some(container.open_stdin),
        env: Some(
            environment
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        ),
        cmd: Some(hook.map_or_else(|| container.command.clone(), |hook| hook.command.clone())),
        healthcheck: if hook.is_some() {
            Some(HealthConfig {
                test: Some(vec![HEALTHCHECK_DISABLE_SENTINEL.into()]),
                ..Default::default()
            })
        } else {
            container
                .healthcheck
                .as_ref()
                .map(docker_healthcheck)
                .transpose()?
        },
        image: Some(container.image.clone()),
        working_dir: container
            .working_directory
            .as_ref()
            .map(ToString::to_string),
        entrypoint: (!container.entrypoint.is_empty()).then(|| container.entrypoint.clone()),
        exposed_ports,
        labels: Some(labels),
        stop_timeout: container.stop_timeout_secs,
        host_config: Some(host_config),
        ..Default::default()
    })
}

fn docker_restart(policy: ployz_core::RestartPolicy) -> RestartPolicy {
    let (name, maximum_retry_count) = match policy {
        ployz_core::RestartPolicy::No => (RestartPolicyNameEnum::NO, None),
        ployz_core::RestartPolicy::Always => (RestartPolicyNameEnum::ALWAYS, None),
        ployz_core::RestartPolicy::UnlessStopped => (RestartPolicyNameEnum::UNLESS_STOPPED, None),
        ployz_core::RestartPolicy::OnFailure {
            maximum_retry_count,
        } => (RestartPolicyNameEnum::ON_FAILURE, maximum_retry_count),
    };
    RestartPolicy {
        name: Some(name),
        maximum_retry_count,
    }
}

pub(super) fn docker_healthcheck(spec: &HealthcheckSpec) -> Result<HealthConfig, Error> {
    match spec {
        HealthcheckSpec::Disabled => Ok(HealthConfig {
            test: Some(vec![HEALTHCHECK_DISABLE_SENTINEL.into()]),
            ..Default::default()
        }),
        HealthcheckSpec::Configured(configured) => Ok(HealthConfig {
            test: Some(configured.test.as_slice().to_vec()),
            interval: millis_to_nanos(configured.interval_millis)?,
            timeout: millis_to_nanos(configured.timeout_millis)?,
            retries: configured.retries.map(i64::from),
            start_period: millis_to_nanos(configured.start_period_millis)?,
            start_interval: millis_to_nanos(configured.start_interval_millis)?,
        }),
    }
}

fn millis_to_nanos(value: Option<u64>) -> Result<Option<i64>, Error> {
    value
        .map(|value| {
            value
                .checked_mul(1_000_000)
                .and_then(|value| i64::try_from(value).ok())
                .ok_or(Error::DurationOverflow)
        })
        .transpose()
}

pub(super) fn docker_ports(
    ports: &[PortPublication],
    interface_addresses: &[IpAddr],
) -> Result<(Option<Vec<String>>, Option<PortMap>), Error> {
    let mut exposed = BTreeSet::new();
    let mut bindings = PortMap::new();
    for port in ports {
        let PortPublication::Host {
            bind,
            published_port,
            container_port,
            transport_protocol,
        } = port
        else {
            continue;
        };
        let key = format!(
            "{container_port}/{}",
            match transport_protocol {
                TransportProtocol::Tcp => "tcp",
                TransportProtocol::Udp => "udp",
            }
        );
        exposed.insert(key.clone());
        let host_ips = match bind {
            HostBind::All => vec![None],
            HostBind::Address { address } => vec![Some(address.to_string())],
            HostBind::Prefix { prefix } => {
                let addresses = interface_addresses
                    .iter()
                    .filter(|address| prefix.contains(*address))
                    .map(|address| Some(address.to_string()))
                    .collect::<Vec<_>>();
                if addresses.is_empty() {
                    return Err(Error::InvalidContainerConfig(format!(
                        "no host addresses are contained in prefix {prefix:?}"
                    )));
                }
                addresses
            }
        };
        bindings
            .entry(key)
            .or_default()
            .get_or_insert_default()
            .extend(host_ips.into_iter().map(|host_ip| PortBinding {
                host_ip,
                host_port: Some(published_port.to_string()),
            }));
    }
    Ok((
        (!exposed.is_empty()).then(|| exposed.into_iter().collect()),
        (!bindings.is_empty()).then_some(bindings),
    ))
}

pub(super) fn docker_resources(resources: &ployz_core::ContainerResources) -> HostConfig {
    HostConfig {
        memory: resources.memory_bytes,
        memory_reservation: resources.memory_reservation_bytes,
        nano_cpus: resources.cpu_nanos,
        devices: (!resources.devices.is_empty()).then(|| {
            resources
                .devices
                .iter()
                .map(|device| DockerDeviceMapping {
                    path_on_host: Some(device.machine_path.to_string()),
                    path_in_container: Some(device.container_path.to_string()),
                    cgroup_permissions: Some(device.cgroup_permissions.clone()),
                })
                .collect()
        }),
        device_requests: (!resources.device_reservations.is_empty()).then(|| {
            resources
                .device_reservations
                .iter()
                .map(|reservation| DeviceRequest {
                    driver: reservation.driver.clone(),
                    count: reservation.count,
                    device_ids: Some(reservation.device_ids.clone()),
                    capabilities: Some(reservation.capabilities.clone()),
                    options: Some(reservation.options.clone().into_iter().collect()),
                })
                .collect()
        }),
        ulimits: (!resources.ulimits.is_empty()).then(|| {
            resources
                .ulimits
                .iter()
                .map(|(name, limit)| ResourcesUlimits {
                    name: Some(name.clone()),
                    soft: Some(limit.soft),
                    hard: Some(limit.hard),
                })
                .collect()
        }),
        shm_size: resources.shared_memory_bytes,
        ..Default::default()
    }
}

pub(super) fn docker_mounts(graph: &ServiceVolumeGraph) -> Result<Vec<Mount>, Error> {
    graph
        .mounts()
        .iter()
        .map(|mount| {
            let volume = graph.volume_for(mount);
            let mut translated = Mount {
                target: Some(mount.target.to_string()),
                read_only: Some(mount.read_only),
                ..Default::default()
            };
            match &volume.source {
                VolumeSource::Bind {
                    machine_path,
                    create_machine_path,
                    propagation,
                    recursive,
                } => {
                    translated.typ = Some(MountType::BIND);
                    translated.source = Some(machine_path.to_string());
                    let (non_recursive, read_only_non_recursive, read_only_force_recursive) =
                        match recursive {
                            Some(BindRecursive::Disabled) => (Some(true), None, None),
                            Some(BindRecursive::Writable) => (None, Some(true), None),
                            Some(BindRecursive::Readonly) => (None, None, Some(true)),
                            None => (None, None, None),
                        };
                    translated.bind_options = Some(MountBindOptions {
                        propagation: propagation.map(|propagation| match propagation {
                            BindPropagation::Private => MountBindOptionsPropagationEnum::PRIVATE,
                            BindPropagation::Rprivate => MountBindOptionsPropagationEnum::RPRIVATE,
                            BindPropagation::Shared => MountBindOptionsPropagationEnum::SHARED,
                            BindPropagation::Rshared => MountBindOptionsPropagationEnum::RSHARED,
                            BindPropagation::Slave => MountBindOptionsPropagationEnum::SLAVE,
                            BindPropagation::Rslave => MountBindOptionsPropagationEnum::RSLAVE,
                        }),
                        create_mountpoint: create_machine_path.then_some(true),
                        non_recursive,
                        read_only_non_recursive,
                        read_only_force_recursive,
                    });
                }
                VolumeSource::Named {
                    name,
                    driver,
                    labels,
                    no_copy,
                    subpath,
                    ..
                } => {
                    translated.typ = Some(MountType::VOLUME);
                    translated.source = Some(name.to_string());
                    translated.volume_options = Some(MountVolumeOptions {
                        no_copy: Some(*no_copy),
                        labels: Some(labels.clone().into_iter().collect()),
                        driver_config: driver.as_ref().map(|driver| {
                            MountVolumeOptionsDriverConfig {
                                name: Some(driver.name.clone()),
                                options: Some(driver.options.clone().into_iter().collect()),
                            }
                        }),
                        subpath: subpath.clone(),
                    });
                }
                VolumeSource::Tmpfs {
                    size_bytes,
                    mode,
                    options,
                } => {
                    translated.typ = Some(MountType::TMPFS);
                    translated.tmpfs_options = Some(MountTmpfsOptions {
                        size_bytes: size_bytes
                            .map(i64::try_from)
                            .transpose()
                            .map_err(|_| Error::SizeOverflow)?,
                        mode: mode.map(i64::from),
                        options: Some(options.clone()),
                    });
                }
            }
            Ok(translated)
        })
        .collect()
}
