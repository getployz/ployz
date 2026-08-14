use std::{
    collections::{BTreeSet, HashMap},
    net::IpAddr,
    str::FromStr,
};

use bollard::{
    body_full,
    models::{
        ContainerCreateBody, DeviceMapping, DeviceRequest, HealthConfig, HostConfig,
        HostConfigLogConfig, Mount, MountBindOptions, MountTmpfsOptions, MountType,
        MountVolumeOptions, MountVolumeOptionsDriverConfig, PortBinding, PortMap, ResourcesUlimits,
    },
    query_parameters::{RemoveContainerOptionsBuilder, UploadToContainerOptionsBuilder},
};
use ployz_core::{
    ContainerId, ContainerResources, HealthcheckSpec, HostBind, PortPublication,
    ResolvedServiceSpec, TransportProtocol, VolumeSource,
};

use crate::docker_image::prepare_image;

use super::{
    Error, LABEL_MANAGED, LABEL_SERVICE_ID, LABEL_SERVICE_NAME, LocalDocker, MachineSpecStore,
    display_name, some_map,
};

impl LocalDocker {
    pub async fn create_service_container(
        &self,
        specs: &MachineSpecStore,
        spec: ResolvedServiceSpec,
    ) -> Result<(ContainerId, String), Error> {
        let mounts = docker_mounts(&spec)?;
        // TODO(UT-127): non-local drivers intentionally receive no special handling.
        for mount in mounts
            .iter()
            .filter(|mount| mount.typ == Some(MountType::VOLUME))
        {
            let name = mount
                .source
                .as_deref()
                .ok_or_else(|| Error::InvalidMount("named Volume source is missing".into()))?;
            self.client.inspect_volume(name).await?;
            // TODO(UT-128): existence is the only mount-time check; do not recheck driver/options.
        }
        prepare_image(
            &self.client,
            &spec.container.image,
            spec.container.pull_policy,
        )
        .await?;
        let configs = docker_config_archive(&spec)?;
        let interface_addresses = if spec.ports.iter().any(|port| {
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
        let (exposed_ports, port_bindings) = docker_ports(&spec.ports, &interface_addresses)?;
        let labels = HashMap::from([
            (LABEL_MANAGED.to_owned(), String::new()),
            (LABEL_SERVICE_ID.to_owned(), spec.service_id.to_string()),
            (LABEL_SERVICE_NAME.to_owned(), spec.name.to_string()),
        ]);
        let config =
            ContainerCreateBody {
                image: Some(spec.container.image.clone()),
                cmd: (!spec.container.command.is_empty()).then(|| spec.container.command.clone()),
                entrypoint: (!spec.container.entrypoint.is_empty())
                    .then(|| spec.container.entrypoint.clone()),
                env: (!spec.container.environment.is_empty()).then(|| {
                    spec.container
                        .environment
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect()
                }),
                labels: Some(labels),
                exposed_ports,
                healthcheck: docker_healthcheck(spec.container.healthcheck.as_ref())?,
                user: spec.container.user.clone(),
                working_dir: spec
                    .container
                    .working_directory
                    .as_ref()
                    .map(ToString::to_string),
                tty: spec.container.tty.then_some(true),
                open_stdin: spec.container.open_stdin.then_some(true),
                host_config: Some(HostConfig {
                    mounts: Some(mounts),
                    network_mode: Some(crate::network::DOCKER_NETWORK_NAME.into()),
                    port_bindings,
                    log_config: spec.container.log_driver.as_ref().map(|driver| {
                        HostConfigLogConfig {
                            typ: Some(driver.name.clone()),
                            config: some_map(driver.options.clone()),
                        }
                    }),
                    init: spec.container.init,
                    cap_add: some_vec(spec.container.cap_add.clone()),
                    cap_drop: some_vec(spec.container.cap_drop.clone()),
                    pid_mode: spec.container.pid_mode.clone(),
                    privileged: spec.container.privileged.then_some(true),
                    sysctls: some_map(spec.container.sysctls.clone()),
                    ..docker_resources(&spec.container.resources)
                }),
                ..Default::default()
            };
        let created = self.client.create_container(None, config).await?;
        let created_id = created.id;
        let result = async {
            if let Some(configs) = configs {
                self.client
                    .upload_to_container(
                        &created_id,
                        Some(UploadToContainerOptionsBuilder::default().path("/").build()),
                        body_full(configs.into()),
                    )
                    .await?;
            }
            let container_id =
                ContainerId::parse(created_id.clone()).map_err(|source| Error::InvalidValue {
                    field: "container ID",
                    source,
                })?;
            let inspected = self.client.inspect_container(&created_id, None).await?;
            let display_name = display_name(inspected.name.as_deref());
            specs.put(&container_id, &spec).await?;
            Ok((container_id, display_name))
        }
        .await;
        if result.is_err() {
            let _ = self
                .client
                .remove_container(
                    &created_id,
                    Some(
                        RemoveContainerOptionsBuilder::default()
                            .force(true)
                            .v(true)
                            .build(),
                    ),
                )
                .await;
        }
        result
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
        let protocol = match transport_protocol {
            TransportProtocol::Tcp => "tcp",
            TransportProtocol::Udp => "udp",
        };
        let key = format!("{container_port}/{protocol}");
        exposed.insert(key.clone());
        let host_ips = match bind {
            HostBind::All => vec![None],
            HostBind::Address { address } => vec![Some(address.to_string())],
            HostBind::Prefix { prefix } => {
                let addresses = interface_addresses
                    .iter()
                    .filter(|address| prefix.contains(*address))
                    .map(ToString::to_string)
                    .map(Some)
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
            .get_or_insert_with(Vec::new)
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

pub(super) fn docker_healthcheck(
    healthcheck: Option<&HealthcheckSpec>,
) -> Result<Option<HealthConfig>, Error> {
    let Some(healthcheck) = healthcheck else {
        return Ok(None);
    };
    if healthcheck.disabled {
        return Ok(Some(HealthConfig {
            test: Some(vec!["NONE".into()]),
            ..Default::default()
        }));
    }
    Ok(Some(HealthConfig {
        test: Some(healthcheck.test.clone()),
        interval: duration_nanos(healthcheck.interval_millis, "healthcheck interval")?,
        timeout: duration_nanos(healthcheck.timeout_millis, "healthcheck timeout")?,
        start_period: duration_nanos(healthcheck.start_period_millis, "healthcheck start period")?,
        start_interval: duration_nanos(
            healthcheck.start_interval_millis,
            "healthcheck start interval",
        )?,
        retries: healthcheck.retries.map(i64::from),
    }))
}

fn duration_nanos(value: Option<u64>, field: &str) -> Result<Option<i64>, Error> {
    value
        .map(|millis| {
            millis
                .checked_mul(1_000_000)
                .and_then(|nanos| i64::try_from(nanos).ok())
                .ok_or_else(|| Error::InvalidContainerConfig(format!("{field} is too large")))
        })
        .transpose()
}

fn docker_config_archive(spec: &ResolvedServiceSpec) -> Result<Option<Vec<u8>>, Error> {
    if spec.container.config_mounts.is_empty() {
        return Ok(None);
    }
    let mut archive = tar::Builder::new(Vec::new());
    for mount in &spec.container.config_mounts {
        let config = spec
            .configs
            .iter()
            .find(|config| config.name == mount.config_name)
            .ok_or_else(|| {
                Error::InvalidMount(format!(
                    "config references undeclared config {:?}",
                    mount.config_name
                ))
            })?;
        let target = mount
            .target
            .as_ref()
            .map_or_else(|| format!("/{}", mount.config_name), ToString::to_string);
        let path = target
            .strip_prefix('/')
            .filter(|path| !path.is_empty())
            .ok_or_else(|| Error::InvalidMount(format!("invalid config target {target:?}")))?;
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(config.content.len() as u64);
        header.set_mode(mount.mode.unwrap_or(0o444));
        header.set_uid(mount.uid.unwrap_or(0));
        header.set_gid(mount.gid.unwrap_or(0));
        header.set_cksum();
        archive
            .append_data(&mut header, path, config.content.as_slice())
            .map_err(|error| Error::InvalidMount(format!("invalid config archive: {error}")))?;
    }
    archive
        .into_inner()
        .map(Some)
        .map_err(|error| Error::InvalidMount(format!("invalid config archive: {error}")))
}

pub(super) fn docker_resources(resources: &ContainerResources) -> HostConfig {
    HostConfig {
        nano_cpus: resources.cpu_nanos,
        memory: resources.memory_bytes,
        memory_reservation: resources.memory_reservation_bytes,
        shm_size: resources.shared_memory_bytes,
        devices: (!resources.devices.is_empty()).then(|| {
            resources
                .devices
                .iter()
                .map(|device| DeviceMapping {
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
                .map(|request| DeviceRequest {
                    driver: request.driver.clone(),
                    count: request.count,
                    device_ids: some_vec(request.device_ids.clone()),
                    capabilities: some_vec(request.capabilities.clone()),
                    options: some_map(request.options.clone()),
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
        ..Default::default()
    }
}

pub(super) fn docker_mounts(spec: &ResolvedServiceSpec) -> Result<Vec<Mount>, Error> {
    spec.mounts
        .iter()
        .map(|service_mount| {
            let volume = spec
                .volumes
                .iter()
                .find(|volume| volume.reference == service_mount.volume)
                .ok_or_else(|| Error::UnknownVolumeReference(service_mount.volume.clone()))?;
            let mut mount = Mount {
                target: Some(service_mount.target.to_string()),
                read_only: service_mount.read_only.then_some(true),
                ..Default::default()
            };
            match &volume.source {
                VolumeSource::Bind {
                    machine_path,
                    create_machine_path,
                    propagation,
                    recursive,
                } => {
                    mount.typ = Some(MountType::BIND);
                    mount.source = Some(machine_path.to_string());
                    mount.bind_options = Some(MountBindOptions {
                        propagation: propagation
                            .as_deref()
                            .map(bollard::models::MountBindOptionsPropagationEnum::from_str)
                            .transpose()
                            .map_err(Error::InvalidMount)?,
                        create_mountpoint: create_machine_path.then_some(true),
                        non_recursive: (recursive.as_deref() == Some("disabled")).then_some(true),
                        read_only_non_recursive: (recursive.as_deref() == Some("writable"))
                            .then_some(true),
                        read_only_force_recursive: (recursive.as_deref() == Some("readonly"))
                            .then_some(true),
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
                    mount.typ = Some(MountType::VOLUME);
                    mount.source = Some(name.to_string());
                    mount.volume_options = Some(MountVolumeOptions {
                        no_copy: no_copy.then_some(true),
                        labels: some_map(labels.clone()),
                        driver_config: driver.as_ref().map(|driver| {
                            MountVolumeOptionsDriverConfig {
                                name: Some(driver.name.clone()),
                                options: some_map(driver.options.clone()),
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
                    mount.typ = Some(MountType::TMPFS);
                    mount.tmpfs_options = Some(MountTmpfsOptions {
                        size_bytes: size_bytes
                            .map(i64::try_from)
                            .transpose()
                            .map_err(|_| Error::InvalidMount("tmpfs size exceeds i64".into()))?,
                        mode: mode.map(i64::from),
                        options: (!options.is_empty()).then(|| options.clone()),
                    });
                }
            }
            Ok(mount)
        })
        .collect()
}

fn some_vec<T>(values: Vec<T>) -> Option<Vec<T>> {
    (!values.is_empty()).then_some(values)
}
