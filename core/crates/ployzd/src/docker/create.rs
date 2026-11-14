use ployz_core::labels::{self, Stamp};
use std::{
    collections::{BTreeSet, HashMap},
    net::IpAddr,
    path::Path,
};

use bollard::models::{
    ContainerCreateBody, DeviceMapping as DockerDeviceMapping, DeviceRequest, HostConfig,
    HostConfigLogConfig, Mount, MountBindOptions, MountBindOptionsPropagationEnum,
    MountTmpfsOptions, MountType, MountVolumeOptions, MountVolumeOptionsDriverConfig,
    NetworkingConfig, PortBinding, PortMap, ResourcesUlimits, RestartPolicy, RestartPolicyNameEnum,
};
use ployz_core::{
    BindPropagation, BindRecursive, ContainerId, ContainerKind, EnvValue, HostBind, MachineGateway,
    MachineId, Namespace, PortPublication, ResolvedServiceSpec, ResolvedServiceVolumeGraph,
    TransportProtocol,
};

use super::{Error, doors::Door};

mod healthcheck;
use crate::sealed_env;
pub(super) use healthcheck::{docker_healthcheck, effective_healthcheck};

pub(super) const TCP_MIGRATE_REQ: &str = "net.ipv4.tcp_migrate_req";

pub(crate) fn container_log_config() -> HostConfigLogConfig {
    HostConfigLogConfig {
        typ: Some("local".into()),
        config: Some(HashMap::from([
            ("max-size".into(), "10m".into()),
            ("max-file".into(), "3".into()),
            ("compress".into(), "false".into()),
        ])),
    }
}

/// Set at create, outside the spec, so changing these never replaces Caddy.
///
/// Caddy binds a fresh `SO_REUSEPORT` listener on every reload and closes the old one.
/// Without `tcp_migrate_req` the kernel resets connections still queued on the closing
/// listener.
///
/// A request written to a powered-off upstream waits for retransmits, which `dial_timeout`
/// doesn't cover and the default 15 take about 15 minutes. `tcp_retries2=5` gives up in
/// about 14s, and Caddy retries a GET on another upstream. It covers Caddy's whole netns, so a
/// public client whose link stalls with unacked data is reset then too, not after 15 minutes.
pub(super) fn ingress_sysctls(
    body: &mut ContainerCreateBody,
    door: Option<Door>,
    kernel_supports_migrate_req: bool,
) {
    // The holder owns the Ingress Proxy's netns, so the sysctls go on it. A Caddy that
    // joins it inherits them, and Docker refuses netns sysctls in `container:` mode.
    if door != Some(Door::Holder) {
        return;
    }
    let sysctls = body
        .host_config
        .get_or_insert_default()
        .sysctls
        .get_or_insert_default();
    sysctls.insert("net.ipv4.tcp_retries2".into(), "5".into());
    if kernel_supports_migrate_req {
        sysctls.insert(TCP_MIGRATE_REQ.into(), "1".into());
    }
}

pub(super) fn holds_sealed(kind: ContainerKind, spec: &ResolvedServiceSpec) -> bool {
    let hook = spec
        .pre_deploy
        .as_ref()
        .filter(|_| kind == ContainerKind::PreDeployHook);
    spec.container
        .environment
        .values()
        .chain(hook.into_iter().flat_map(|hook| hook.environment.values()))
        .any(EnvValue::is_sealed)
}

/// Which network a new Container is created in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum CreationNetwork {
    /// Its own endpoint on the ployz bridge.
    Bridge,
    /// The network namespace of this running Container, as Docker's `container:` mode.
    /// Docker refuses a hostname, DNS, hosts, ports and an endpoint in that mode, so the
    /// joined Container carries none and inherits the holder's.
    Holder(ContainerId),
}

pub(super) fn container_create_body(
    machine_id: &MachineId,
    gateway: MachineGateway,
    kind: ContainerKind,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    network: &CreationNetwork,
    sealing_key_dir: Option<&Path>,
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
    environment.insert("PLOYZ_MACHINE_ID".into(), machine_id.to_string().into());
    let opened = sealed_env::open(
        environment,
        container.sealed_identity.as_ref(),
        sealing_key_dir,
    )?;
    let mut labels: HashMap<_, _> = container
        .labels
        .as_map()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let publishes_host_port = spec
        .ports
        .iter()
        .any(|port| matches!(port, PortPublication::Host { .. }));
    Stamp {
        namespace,
        service_id: &spec.service_id,
        service_name: &spec.name,
        kind,
        bridge_ipv6: !publishes_host_port,
    }
    .write(&mut labels);
    if let Some(record) = &opened.record {
        labels.insert(
            labels::SEALED_ENV.into(),
            serde_json::to_string(record).expect("a sealed record serializes"),
        );
    }
    let mounts = docker_mounts(spec.volume_graph())?;
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
    let on_bridge = matches!(network, CreationNetwork::Bridge);
    let resources = &container.resources;
    let host_config = HostConfig {
        dns: on_bridge.then(|| vec![gateway.0.to_string()]),
        dns_search: on_bridge
            .then(|| vec![ployz_core::domain::private_dns::search_domain(namespace)]),
        dns_options: on_bridge.then(|| vec!["ndots:1".into()]),
        init: container.init,
        network_mode: Some(match network {
            CreationNetwork::Bridge => crate::network::DOCKER_NETWORK_NAME.into(),
            CreationNetwork::Holder(holder) => format!("container:{holder}"),
        }),
        log_config: Some(container_log_config()),
        port_bindings: port_bindings.filter(|_| on_bridge),
        restart_policy: Some(docker_restart(if hook.is_some() {
            ployz_core::RestartPolicy::No
        } else {
            spec.container.restart
        })),
        mounts: (!mounts.is_empty()).then_some(mounts),
        cap_add: (!container.cap_add.is_empty()).then(|| container.cap_add.clone()),
        cap_drop: (!container.cap_drop.is_empty()).then(|| container.cap_drop.clone()),
        pid_mode: container.pid_mode.as_ref().map(ToString::to_string),
        extra_hosts: (on_bridge && !container.extra_hosts.is_empty()).then(|| {
            container
                .extra_hosts
                .iter()
                .map(ToString::to_string)
                .collect()
        }),
        privileged: Some(
            hook.and_then(|hook| hook.privileged)
                .unwrap_or(container.privileged),
        ),
        sysctls: (!container.sysctls.is_empty()).then(|| {
            container
                .sysctls
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }),
        ..docker_resources(resources)
    };
    Ok(ContainerCreateBody {
        user: hook
            .and_then(|hook| hook.user.clone())
            .or_else(|| container.user.clone()),
        tty: Some(container.tty),
        open_stdin: Some(container.open_stdin),
        env: Some(
            opened
                .values
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        ),
        cmd: Some(hook.map_or_else(
            || container.command.clone(),
            |hook| hook.command.as_slice().to_vec(),
        )),
        healthcheck: if hook.is_some() {
            Some(healthcheck::disabled())
        } else {
            container
                .healthcheck
                .as_ref()
                .map(docker_healthcheck)
                .transpose()?
        },
        image: Some(container.image.clone()),
        hostname: container
            .hostname
            .as_ref()
            .filter(|_| on_bridge)
            .map(ToString::to_string),
        working_dir: container
            .working_directory
            .as_ref()
            .map(ToString::to_string),
        // A hook runs only its own command: an empty ENTRYPOINT clears the image's,
        // and the Service's entrypoint is its start command, which the hook must not run.
        entrypoint: match hook {
            Some(_) => Some(Vec::new()),
            None => (!container.entrypoint.is_empty()).then(|| container.entrypoint.clone()),
        },
        exposed_ports: exposed_ports.filter(|_| on_bridge),
        networking_config: super::bridge_endpoint(super::takes_bridge_ipv6(&labels))
            .filter(|_| on_bridge)
            .map(|endpoint| NetworkingConfig {
                endpoints_config: Some(HashMap::from([(
                    crate::network::DOCKER_NETWORK_NAME.to_owned(),
                    endpoint,
                )])),
            }),
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
                        "no host addresses are contained in prefix {prefix}"
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
        memory: resources.memory_bytes.map(Into::into),
        memory_reservation: resources.memory_reservation_bytes.map(Into::into),
        nano_cpus: resources.cpu_nanos.map(Into::into),
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
                    options: Some(
                        reservation
                            .options
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect(),
                    ),
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
        shm_size: resources.shared_memory_bytes.map(Into::into),
        ..Default::default()
    }
}

pub(super) fn docker_mounts(graph: &ResolvedServiceVolumeGraph) -> Result<Vec<Mount>, Error> {
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
            match volume.source.kind() {
                ployz_core::RawVolumeSource::Bind {
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
                ployz_core::RawVolumeSource::External { name } => {
                    translated.typ = Some(MountType::VOLUME);
                    translated.source = Some(name.to_string());
                    translated.volume_options = Some(MountVolumeOptions {
                        no_copy: Some(mount.no_copy),
                        labels: None,
                        driver_config: None,
                        subpath: mount.subpath.clone(),
                    });
                }
                ployz_core::RawVolumeSource::Ordinary { .. }
                | ployz_core::RawVolumeSource::Provisioned { .. } => {
                    let request = volume
                        .source
                        .to_create_volume_request()
                        .expect("managed Docker Volumes have creation requests");
                    translated.typ = Some(MountType::VOLUME);
                    translated.source = Some(request.name.to_string());
                    translated.volume_options = Some(MountVolumeOptions {
                        no_copy: Some(mount.no_copy),
                        labels: Some(request.labels.into_iter().collect()),
                        driver_config: Some(MountVolumeOptionsDriverConfig {
                            name: Some(request.driver),
                            options: Some(request.options.into_iter().collect()),
                        }),
                        subpath: mount.subpath.clone(),
                    });
                }
                ployz_core::RawVolumeSource::Tmpfs {
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::container_log_config;

    #[test]
    fn container_log_config_is_local_uncompressed_rotation() {
        let log_config = container_log_config();

        assert_eq!(log_config.typ.as_deref(), Some("local"));
        assert_eq!(
            log_config.config,
            Some(HashMap::from([
                ("max-size".to_string(), "10m".to_string()),
                ("max-file".to_string(), "3".to_string()),
                ("compress".to_string(), "false".to_string()),
            ]))
        );
    }
}
