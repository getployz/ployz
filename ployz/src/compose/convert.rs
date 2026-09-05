use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use ployz_core::{
    ByteQuantity, ConfiguredHealthcheck, ContainerHostname, ContainerLabels, ContainerPath,
    ContainerResources, CpuNanos, DependencyCondition, DeviceMapping, DeviceReservation, ExtraHost,
    HEALTHCHECK_DISABLE_SENTINEL, HealthcheckCommand, HealthcheckSpec, LogDriver, MachinePath,
    MachineTarget, Placement, PortPublication, ProvisionedVolumeMaximumBytes, PullPolicy,
    RequestedServiceSpec, RestartPolicy, ServiceConfigGraph, ServiceContainerSpec,
    ServiceDependency, ServiceMode, ServiceName, ServiceVolumeGraph, ServiceVolumeReference,
    Ulimit, UpdateConfig, UpdateOrder,
};
use serde_norway::Value;

use super::{
    configs::configs,
    extensions::{caddy, pre_deploy},
    image::ImageState,
    model::{
        BuildSpec, ComposeError, ComposeProject, RawDeploy, RawDevice, RawDeviceRequest,
        RawHealthcheck, RawProject, RawService, RawStringList,
    },
    mounts::volumes,
    ports::ports,
    secrets::convert_secrets,
};

pub fn parse_normalized(
    yaml: &str,
    working_dir: impl Into<PathBuf>,
) -> Result<ComposeProject, ComposeError> {
    let raw: RawProject = serde_norway::from_str(yaml).map_err(invalid)?;
    convert_raw_project(
        raw,
        working_dir,
        std::env::vars().collect(),
        &BTreeSet::new(),
    )
}

pub(super) fn convert_raw_project(
    mut raw: RawProject,
    working_dir: impl Into<PathBuf>,
    environment: BTreeMap<String, String>,
    recovered_secrets: &BTreeSet<String>,
) -> Result<ComposeProject, ComposeError> {
    for name in recovered_secrets {
        if let Some(secret) = raw.secrets.get_mut(name) {
            secret.external = Value::Null;
        }
    }
    validate_definitions(&raw)?;
    let provisioned_volume_bounds = raw
        .provisioned_volumes
        .iter()
        .map(|(name, raw)| {
            let size =
                crate::volume::ProvisionedVolumeSize::parse(&raw.size().to_ascii_lowercase())
                    .map_err(|error| invalid(format!("x-volumes.{name}: {error}")))?;
            Ok((
                ServiceVolumeReference::parse(name.clone()).map_err(invalid)?,
                ProvisionedVolumeMaximumBytes::new(size.bytes()),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, ComposeError>>()?;
    let secrets = convert_secrets(std::mem::take(&mut raw.secrets))?;
    let working_dir = working_dir.into();
    let name = raw.name.clone().unwrap_or_else(|| {
        working_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_owned()
    });
    let images = ImageState::inspect(&working_dir)?;
    let mut services = BTreeMap::new();
    let mut builds = BTreeMap::new();
    let mut dependencies = BTreeMap::new();
    let mut service_profiles = BTreeMap::new();
    let mut warnings = Vec::new();
    for (service_name, service) in &raw.services {
        warnings.extend(classify(service_name, service)?);
        let service_dependencies = service_dependencies(service_name, service)?;
        if let Some(missing) = service_dependencies
            .iter()
            .find(|dependency| !raw.services.contains_key(dependency.service.as_str()))
        {
            return Err(invalid(format!(
                "service '{service_name}' depends on undefined service '{}'",
                missing.service
            )));
        }
        dependencies.insert(service_name.clone(), service_dependencies);
        if !service.profiles.is_empty() {
            service_profiles.insert(service_name.clone(), service.profiles.clone());
        }
        let (spec, build) = convert_service(
            &name,
            service_name,
            service,
            &raw,
            &provisioned_volume_bounds,
            &working_dir,
            &images,
        )?;
        if let Some(build) = build {
            builds.insert(service_name.clone(), build);
        }
        services.insert(service_name.clone(), spec);
    }
    for (dependent, service_dependencies) in &dependencies {
        for dependency in service_dependencies {
            if dependency.condition == DependencyCondition::ServiceHealthy
                && !matches!(
                    services
                        .get(dependency.service.as_str())
                        .and_then(|spec| spec.container.healthcheck.as_ref()),
                    Some(HealthcheckSpec::Configured(_))
                )
            {
                return Err(invalid(format!(
                    "service '{dependent}': depends_on service '{}' uses condition 'service_healthy', but that service has no configured healthcheck",
                    dependency.service
                )));
            }
        }
    }
    Ok(ComposeProject {
        name,
        working_dir,
        context: raw.context,
        services,
        builds,
        dependencies,
        warnings,
        service_profiles,
        volumes: raw.volumes,
        secrets,
        environment,
    })
}

impl ComposeProject {
    pub fn dependency_order(&self) -> Result<Vec<&RequestedServiceSpec>, ComposeError> {
        fn visit<'a>(
            name: &'a str,
            project: &'a ComposeProject,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
            ordered: &mut Vec<&'a RequestedServiceSpec>,
        ) -> Result<(), ComposeError> {
            if visited.contains(name) {
                return Ok(());
            }
            if !visiting.insert(name) {
                return Err(invalid(format!("dependency cycle at service '{name}'")));
            }
            for dependency in project.dependencies.get(name).into_iter().flatten() {
                visit(
                    dependency.service.as_str(),
                    project,
                    visiting,
                    visited,
                    ordered,
                )?;
            }
            visiting.remove(name);
            visited.insert(name);
            ordered.push(
                project
                    .services
                    .get(name)
                    .ok_or_else(|| invalid(format!("undefined service '{name}'")))?,
            );
            Ok(())
        }

        let mut ordered = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for name in self.services.keys() {
            visit(name, self, &mut visiting, &mut visited, &mut ordered)?;
        }
        Ok(ordered)
    }
}

fn convert_service(
    project: &str,
    name: &str,
    raw: &RawService,
    root: &RawProject,
    provisioned_volume_bounds: &BTreeMap<ServiceVolumeReference, ProvisionedVolumeMaximumBytes>,
    directory: &Path,
    images: &ImageState,
) -> Result<(RequestedServiceSpec, Option<BuildSpec>), ComposeError> {
    let build = raw
        .build
        .as_ref()
        .map(|value| BuildSpec { raw: value.clone() });
    let image = images.image(project, name, raw.image.as_deref(), build.is_some())?;
    let mode = match raw
        .deploy
        .as_ref()
        .and_then(|deploy| deploy.mode.as_deref())
    {
        Some("global") => ServiceMode::Global,
        None | Some("") | Some("replicated") => ServiceMode::Replicated {
            replicas: NonZeroU32::new(
                raw.deploy
                    .as_ref()
                    .and_then(|deploy| deploy.replicas)
                    .or(raw.scale)
                    .unwrap_or(1),
            )
            .ok_or_else(|| invalid("replicas must be greater than zero"))?,
        },
        Some(mode) => return Err(invalid(format!("unsupported deploy mode: '{mode}'"))),
    };
    let ports = ports(name, raw)?;
    let ingress_proxy_fragment = caddy(raw.caddy.as_ref(), directory, name)?;
    if ingress_proxy_fragment.is_some()
        && ports
            .iter()
            .any(|port| matches!(port, PortPublication::Ingress { .. }))
    {
        return Err(invalid(format!(
            "service '{name}': ingress ports and 'x-caddy' cannot be specified simultaneously"
        )));
    }
    let (volumes, mounts) = volumes(raw, root, provisioned_volume_bounds)?;
    let volume_graph = ServiceVolumeGraph::parse(volumes, mounts).map_err(invalid)?;
    let (configs, config_mounts) = configs(raw, root, directory)?;
    let config_graph = ServiceConfigGraph::parse(configs, config_mounts).map_err(invalid)?;
    let placement = Placement {
        machines: raw
            .machines
            .as_ref()
            .map(string_list)
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|machine| MachineTarget::parse(machine).map_err(invalid))
            .collect::<Result<_, _>>()?,
    };
    // TODO: service-level `tmpfs` is not reinterpreted as mounts.
    let container = ServiceContainerSpec {
        image,
        command: shell(&raw.command)?,
        entrypoint: shell(&raw.entrypoint)?,
        environment: environment(&raw.environment)?,
        labels: labels(name, &raw.labels)?,
        hostname: raw
            .hostname
            .as_ref()
            .map(ContainerHostname::parse)
            .transpose()
            .map_err(|error| invalid(format!("service '{name}': {error}")))?,
        extra_hosts: extra_hosts(&raw.extra_hosts)?,
        cap_add: raw.cap_add.clone(),
        cap_drop: raw.cap_drop.clone(),
        healthcheck: raw.healthcheck.as_ref().map(healthcheck).transpose()?,
        pull_policy: match raw.pull_policy.as_deref() {
            Some("always") => PullPolicy::Always,
            Some("never") => PullPolicy::Never,
            None | Some("") | Some("missing") | Some("if_not_present") => PullPolicy::Missing,
            Some(policy) => {
                return Err(invalid(format!("unsupported pull policy: '{policy}'")));
            }
        },
        init: raw.init,
        user: raw.user.clone(),
        working_directory: raw
            .working_dir
            .as_ref()
            .map(|path| ContainerPath::parse(path).map_err(invalid))
            .transpose()?,
        tty: raw.tty,
        open_stdin: raw.stdin_open,
        privileged: raw.privileged,
        pid_mode: raw
            .pid
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(invalid)?,
        log_driver: Some(
            raw.logging
                .as_ref()
                .and_then(|logging| {
                    logging.driver.as_ref().map(|driver| LogDriver {
                        name: driver.clone(),
                        options: logging.options.clone(),
                    })
                })
                .unwrap_or(LogDriver {
                    name: "local".into(),
                    options: BTreeMap::new(),
                }),
        ),
        resources: resources(raw)?,
        stop_timeout_secs: duration_millis(raw.stop_grace_period.as_deref())?
            .map(|millis| (millis / 1_000) as i64),
        sysctls: raw.sysctls.clone(),
        restart: raw
            .restart
            .as_deref()
            .map(RestartPolicy::parse)
            .transpose()
            .map_err(invalid)?
            .unwrap_or_default(),
    };
    Ok((
        RequestedServiceSpec {
            name: ServiceName::parse(name).map_err(invalid)?,
            mode,
            container,
            placement,
            ports,
            mount_graph: ployz_core::ServiceMountGraph::new(volume_graph, config_graph)
                .map_err(invalid)?,
            pre_deploy: pre_deploy(raw.pre_deploy.as_ref())?,
            ingress_proxy_fragment,
            update: update(raw.deploy.as_ref())?,
        },
        build,
    ))
}

fn resources(raw: &RawService) -> Result<ContainerResources, ComposeError> {
    let mut resources = ContainerResources {
        cpu_nanos: raw
            .cpus
            .as_ref()
            .map(|value| number(value).ok_or_else(|| invalid("cpus must be numeric")))
            .transpose()?
            .map(CpuNanos::from_cpus)
            .transpose()
            .map_err(invalid)?,
        memory_bytes: optional_bytes(raw.mem_limit.as_ref(), "mem_limit")?,
        memory_reservation_bytes: optional_bytes(raw.mem_reservation.as_ref(), "mem_reservation")?,
        shared_memory_bytes: optional_bytes(raw.shm_size.as_ref(), "shm_size")?,
        ..Default::default()
    };
    for device in &raw.devices {
        let (source, target, permissions) = match device {
            RawDevice::Short(value) => {
                let parts = value.split(':').collect::<Vec<_>>();
                let source = parts.first().copied().unwrap_or_default();
                (
                    source.to_owned(),
                    parts.get(1).copied().unwrap_or(source).to_owned(),
                    parts.get(2).copied().unwrap_or("rwm").to_owned(),
                )
            }
            RawDevice::Long {
                source,
                target,
                permissions,
            } => (
                source.clone(),
                target.clone(),
                permissions.clone().unwrap_or_else(|| "rwm".into()),
            ),
        };
        if source == target && is_cdi_name(&source) {
            if let Some(request) = resources
                .device_reservations
                .iter_mut()
                .find(|request| request.driver.as_deref() == Some("cdi"))
            {
                request.device_ids.push(source);
            } else {
                resources.device_reservations.push(DeviceReservation {
                    driver: Some("cdi".into()),
                    device_ids: vec![source],
                    capabilities: Vec::new(),
                    count: None,
                    options: BTreeMap::new(),
                });
            }
        } else {
            resources.devices.push(DeviceMapping {
                machine_path: MachinePath::parse(source).map_err(invalid)?,
                container_path: ContainerPath::parse(target).map_err(invalid)?,
                cgroup_permissions: permissions,
            });
        }
    }
    for request in &raw.gpus {
        resources
            .device_reservations
            .push(device_request(request, true)?);
    }
    for (name, value) in &raw.ulimits {
        let (soft, hard) = match value {
            Value::Mapping(map) => (
                mapping_i64(map, "soft")
                    .ok_or_else(|| invalid(format!("ulimit '{name}'.soft must be an integer")))?,
                mapping_i64(map, "hard")
                    .ok_or_else(|| invalid(format!("ulimit '{name}'.hard must be an integer")))?,
            ),
            value @ (Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::String(_)
            | Value::Sequence(_)
            | Value::Tagged(_)) => {
                let value = integer(value)
                    .ok_or_else(|| invalid(format!("ulimit '{name}' must be an integer")))?;
                (value, value)
            }
        };
        resources
            .ulimits
            .insert(name.clone(), Ulimit { soft, hard });
    }
    if let Some(deploy) = raw
        .deploy
        .as_ref()
        .and_then(|deploy| deploy.resources.as_ref())
    {
        if let Some(limits) = &deploy.limits {
            if let Some(cpus) = &limits.cpus {
                resources.cpu_nanos = Some(
                    CpuNanos::from_cpus(
                        number(cpus)
                            .ok_or_else(|| invalid("deploy.limits.cpus must be numeric"))?,
                    )
                    .map_err(invalid)?,
                );
            }
            if let Some(memory) = &limits.memory {
                resources.memory_bytes = Some(
                    bytes(memory)
                        .ok_or_else(|| invalid("deploy.limits.memory must be a byte size"))?,
                );
            }
        }
        if let Some(reservations) = &deploy.reservations {
            if let Some(memory) = &reservations.memory {
                resources.memory_reservation_bytes =
                    Some(bytes(memory).ok_or_else(|| {
                        invalid("deploy.reservations.memory must be a byte size")
                    })?);
            }
            for request in &reservations.devices {
                resources
                    .device_reservations
                    .push(device_request(request, false)?);
            }
        }
    }
    Ok(resources)
}

fn device_request(
    request: &RawDeviceRequest,
    gpu: bool,
) -> Result<DeviceReservation, ComposeError> {
    let mut capabilities = request.capabilities.clone();
    if gpu && !capabilities.iter().any(|capability| capability == "gpu") {
        capabilities.push("gpu".into());
    }
    Ok(DeviceReservation {
        driver: request.driver.clone(),
        count: request
            .count
            .as_ref()
            .map(|count| integer(count).ok_or_else(|| invalid("device count must be an integer")))
            .transpose()?,
        device_ids: request.device_ids.clone(),
        capabilities: (!capabilities.is_empty())
            .then_some(capabilities)
            .into_iter()
            .collect(),
        options: request.options.clone(),
    })
}

fn healthcheck(raw: &RawHealthcheck) -> Result<HealthcheckSpec, ComposeError> {
    if raw.disable {
        return Ok(HealthcheckSpec::Disabled);
    }
    let test = match &raw.test {
        Value::Null => Vec::new(),
        Value::String(value) => vec![value.clone()],
        Value::Sequence(values) => values
            .iter()
            .map(|value| {
                scalar(value).ok_or_else(|| invalid("healthcheck test values must be scalar"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Value::Bool(_) | Value::Number(_) | Value::Mapping(_) | Value::Tagged(_) => {
            return Err(invalid("healthcheck test must be a string or list"));
        }
    };
    if test
        .first()
        .is_some_and(|command| command == HEALTHCHECK_DISABLE_SENTINEL)
    {
        return Ok(HealthcheckSpec::Disabled);
    }
    Ok(HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(test).map_err(invalid)?,
        interval_millis: duration_millis(raw.interval.as_deref())?,
        timeout_millis: duration_millis(raw.timeout.as_deref())?,
        start_period_millis: duration_millis(raw.start_period.as_deref())?,
        start_interval_millis: duration_millis(raw.start_interval.as_deref())?,
        retries: raw.retries,
    }))
}

fn update(deploy: Option<&RawDeploy>) -> Result<UpdateConfig, ComposeError> {
    let Some(update) = deploy.and_then(|deploy| deploy.update_config.as_ref()) else {
        return Ok(UpdateConfig::default());
    };
    Ok(UpdateConfig {
        order: match update.order.as_deref() {
            None | Some("") => None,
            Some("start-first") => Some(UpdateOrder::StartFirst),
            Some("stop-first") => Some(UpdateOrder::StopFirst),
            Some(order) => {
                return Err(invalid(format!(
                    "unsupported deploy.update_config.order: '{order}'"
                )));
            }
        },
        monitor_millis: duration_millis(update.monitor.as_deref())?,
    })
}

fn classify(name: &str, service: &RawService) -> Result<Vec<String>, ComposeError> {
    const FEATURES: &[&str] = &[
        "dns",
        "dns_opt",
        "dns_search",
        "group_add",
        "ipc",
        "links",
        "network_mode",
        "oom_kill_disable",
        "pids_limit",
        "runtime",
        "storage_opt",
        "tmpfs",
        "userns_mode",
        "uts",
        "volumes_from",
    ];
    if service.other.contains_key("x-volumes") {
        return Err(invalid(format!(
            "service '{name}': x-volumes is only supported at Compose top level"
        )));
    }
    if service.other.get("read_only") == Some(&Value::Bool(true)) {
        return Err(invalid(unsupported_feature(name, "read_only")));
    }
    if present(&service.other, "security_opt") {
        return Err(invalid(unsupported_feature(name, "security_opt")));
    }
    if service
        .deploy
        .as_ref()
        .is_some_and(|deploy| present(&deploy.other, "placement"))
    {
        return Err(invalid(format!(
            "{}; use x-machines",
            unsupported_feature(name, "deploy.placement")
        )));
    }
    let mut warnings = FEATURES
        .iter()
        .filter(|feature| present(&service.other, feature))
        .map(|feature| unsupported_feature(name, feature))
        .collect::<Vec<_>>();
    for (typo, correction) in [("x-port", "x-ports"), ("x-machine", "x-machines")] {
        if service.other.contains_key(typo) {
            warnings.push(format!(
                "{}; use {correction}",
                unsupported_feature(name, typo)
            ));
        }
    }
    for feature in ["mem_swappiness", "memswap_limit"] {
        if service
            .other
            .get(feature)
            .and_then(integer)
            .is_some_and(|value| value > 0)
        {
            warnings.push(unsupported_feature(name, feature));
        }
    }
    if service.other.get("networks").is_some_and(custom_networks) {
        warnings.push(unsupported_feature(name, "networks"));
    }
    // TODO: secret file mounts remain unsupported; plaintext env references are separate.
    if !service.secrets.is_empty() {
        warnings.push(unsupported_feature(name, "secrets"));
    }
    if let Some(deploy) = &service.deploy {
        warnings.extend(
            deploy
                .other
                .iter()
                .filter(|(feature, value)| !feature.starts_with("x-") && !value.is_null())
                .map(|(feature, _)| unsupported_feature(name, &format!("deploy.{feature}"))),
        );
    }
    Ok(warnings)
}

fn present(map: &BTreeMap<String, Value>, key: &str) -> bool {
    map.get(key).is_some_and(|value| !value.is_null())
}

fn unsupported_feature(name: &str, feature: &str) -> String {
    format!("service '{name}': unsupported feature '{feature}'")
}

fn validate_definitions(project: &RawProject) -> Result<(), ComposeError> {
    if let Some(name) = project
        .volumes
        .keys()
        .find(|name| project.provisioned_volumes.contains_key(name.as_str()))
    {
        return Err(invalid(format!(
            "volume '{name}' is declared in both volumes and x-volumes"
        )));
    }
    for (name, config) in &project.configs {
        // TODO: external config objects remain unsupported.
        if is_external(&config.external) {
            return Err(invalid(format!(
                "external configs are not supported: {name}"
            )));
        }
        // TODO: config labels and environment sources remain ignored.
        let _ = (&config.labels, &config.environment);
    }
    Ok(())
}

pub(super) fn environment(value: &Value) -> Result<BTreeMap<String, String>, ComposeError> {
    match value {
        Value::Null => Ok(BTreeMap::new()),
        Value::Mapping(map) => Ok(map
            .iter()
            .filter_map(|(key, value)| Some((key.as_str()?.to_owned(), scalar(value)?)))
            .collect()),
        Value::Sequence(items) => Ok(items
            .iter()
            .filter_map(Value::as_str)
            .map(|value| {
                value.split_once('=').map_or_else(
                    || (value.to_owned(), String::new()),
                    |(key, value)| (key.to_owned(), value.to_owned()),
                )
            })
            .collect()),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            Err(invalid("environment must be a map or list"))
        }
    }
}

fn labels(name: &str, value: &Value) -> Result<ContainerLabels, ComposeError> {
    let labels = match value {
        Value::Null => BTreeMap::new(),
        Value::Mapping(map) => map
            .iter()
            .map(|(key, value)| {
                let key = key
                    .as_str()
                    .ok_or_else(|| invalid("label keys must be strings"))?
                    .to_owned();
                let value = match value {
                    Value::Null => String::new(),
                    Value::Bool(value) => value.to_string(),
                    Value::Number(value) => value.to_string(),
                    Value::String(value) => value.clone(),
                    Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => {
                        return Err(invalid("label values must be scalar"));
                    }
                };
                Ok((key, value))
            })
            .collect::<Result<_, ComposeError>>()?,
        Value::Sequence(items) => items
            .iter()
            .map(|item| {
                let item = item
                    .as_str()
                    .ok_or_else(|| invalid("labels must be a map or list of strings"))?;
                Ok(item.split_once('=').map_or_else(
                    || (item.to_owned(), String::new()),
                    |(key, value)| (key.to_owned(), value.to_owned()),
                ))
            })
            .collect::<Result<_, ComposeError>>()?,
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            return Err(invalid("labels must be a map or list of strings"));
        }
    };
    ContainerLabels::parse(labels).map_err(|error| invalid(format!("service '{name}': {error}")))
}

fn extra_hosts(value: &Value) -> Result<Vec<ExtraHost>, ComposeError> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::Sequence(items) => items
            .iter()
            .map(|item| {
                let item = item
                    .as_str()
                    .ok_or_else(|| invalid("extra_hosts must be a map or list of strings"))?;
                let (host, address) = item
                    .split_once('=')
                    .or_else(|| item.split_once(':'))
                    .ok_or_else(|| invalid(format!("invalid extra_hosts entry '{item}'")))?;
                ExtraHost::from_parts(host, address)
                    .map_err(|_| invalid(format!("invalid extra_hosts entry '{item}'")))
            })
            .collect(),
        Value::Mapping(map) => {
            let mut entries = Vec::new();
            for (host, addresses) in map {
                let host = host
                    .as_str()
                    .ok_or_else(|| invalid("extra_hosts keys must be strings"))?;
                let addresses = match addresses {
                    Value::Sequence(addresses) => addresses.iter().collect(),
                    address @ (Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::String(_)
                    | Value::Mapping(_)
                    | Value::Tagged(_)) => vec![address],
                };
                for address in addresses {
                    let address = scalar(address)
                        .ok_or_else(|| invalid("extra_hosts values must be scalar"))?;
                    entries.push(ExtraHost::from_parts(host, &address).map_err(|_| {
                        invalid(format!("invalid extra_hosts entry '{host}:{address}'"))
                    })?);
                }
            }
            Ok(entries)
        }
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            Err(invalid("extra_hosts must be a map or list of strings"))
        }
    }
}

pub(super) fn shell(value: &Option<Value>) -> Result<Vec<String>, ComposeError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(command)) => shell_words::split(command).map_err(invalid),
        Some(Value::Sequence(values)) => values
            .iter()
            .map(|value| scalar(value).ok_or_else(|| invalid("command values must be scalar")))
            .collect(),
        _ => Err(invalid("command must be a string or list")),
    }
}

pub(super) fn string_list(value: &RawStringList) -> Result<Vec<String>, ComposeError> {
    let values = match value {
        RawStringList::String(value) => value.split(',').map(str::to_owned).collect::<Vec<_>>(),
        RawStringList::List(values) => values.clone(),
    };
    values
        .into_iter()
        .map(|value| {
            let value = value.trim().to_owned();
            if value.is_empty() {
                Err(invalid("value cannot be empty"))
            } else {
                Ok(value)
            }
        })
        .collect()
}

fn service_dependencies(
    service_name: &str,
    service: &RawService,
) -> Result<Vec<ServiceDependency>, ComposeError> {
    match &service.depends_on {
        Value::Mapping(map) => map
            .iter()
            .map(|(name, value)| {
                let name = name
                    .as_str()
                    .ok_or_else(|| invalid("depends_on service names must be strings"))?;
                let condition = match value {
                    Value::Mapping(options) => {
                        if options.contains_key(Value::String("restart".into())) {
                            return Err(invalid(format!(
                                "service '{service_name}': depends_on service '{name}' uses unsupported 'restart'"
                            )));
                        }
                        match options.get(Value::String("required".into())) {
                            None | Some(Value::Bool(true)) => {}
                            Some(Value::Bool(false)) => {
                                return Err(invalid(format!(
                                    "service '{service_name}': depends_on service '{name}' uses unsupported 'required: false'"
                                )));
                            }
                            Some(_) => {
                                return Err(invalid(format!(
                                    "service '{service_name}': depends_on service '{name}' field 'required' must be true when present"
                                )));
                            }
                        }
                        match mapping_string(options, "condition").as_deref() {
                            None | Some("service_started") => DependencyCondition::ServiceStarted,
                            Some("service_healthy") => DependencyCondition::ServiceHealthy,
                            Some("service_completed_successfully") => {
                                return Err(invalid(format!(
                                    "service '{service_name}': depends_on condition 'service_completed_successfully' is not supported; use x-pre_deploy"
                                )));
                            }
                            Some(condition) => {
                                return Err(invalid(format!(
                                    "service '{service_name}': depends_on condition '{condition}' is not supported"
                                )));
                            }
                        }
                    }
                    Value::Null => DependencyCondition::ServiceStarted,
                    Value::Bool(_)
                    | Value::Number(_)
                    | Value::String(_)
                    | Value::Sequence(_)
                    | Value::Tagged(_) => {
                        return Err(invalid(format!(
                            "service '{service_name}': depends_on service '{name}' must be a mapping"
                        )));
                    }
                };
                Ok(ServiceDependency {
                    service: ServiceName::parse(name).map_err(invalid)?,
                    condition,
                })
            })
            .collect(),
        Value::Sequence(values) => Ok(values
            .iter()
            .filter_map(Value::as_str)
            .map(|name| ServiceName::parse(name).map(|service| ServiceDependency {
                service,
                condition: DependencyCondition::ServiceStarted,
            }).map_err(invalid))
            .collect::<Result<_, _>>()?),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            Ok(Vec::new())
        }
    }
}

fn custom_networks(value: &Value) -> bool {
    match value {
        Value::Mapping(map) => map.len() != 1 || !map.contains_key(Value::String("default".into())),
        Value::Sequence(values) => {
            values.len() != 1 || values.first().and_then(Value::as_str) != Some("default")
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            true
        }
    }
}

pub(super) fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::Null | Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => None,
    }
}

fn number(value: &Value) -> Option<f64> {
    scalar(value)?.parse().ok()
}

pub(super) fn integer(value: &Value) -> Option<i64> {
    scalar(value)?.parse().ok()
}

fn bytes(value: &Value) -> Option<ByteQuantity> {
    bytes_u64(value)
        .and_then(|value| i64::try_from(value).ok())
        .and_then(|value| ByteQuantity::try_from(value).ok())
}

fn optional_bytes(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<ByteQuantity>, ComposeError> {
    value
        .map(|value| bytes(value).ok_or_else(|| invalid(format!("{field} must be a byte size"))))
        .transpose()
}

pub(super) fn bytes_u64(value: &Value) -> Option<u64> {
    let value = scalar(value)?;
    if let Ok(value) = value.parse() {
        return Some(value);
    }
    let split = value.find(|character: char| !character.is_ascii_digit() && character != '.')?;
    let amount = value[..split].parse::<f64>().ok()?;
    let unit = value[split..].to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "b" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => return None,
    };
    Some((amount * f64::from(multiplier)) as u64)
}

pub(super) fn file_mode(value: &Value) -> Option<u32> {
    let value = scalar(value)?;
    u32::from_str_radix(value.trim_start_matches("0o"), 8)
        .ok()
        .or_else(|| value.parse().ok())
}

pub(crate) fn duration_millis(value: Option<&str>) -> Result<Option<u64>, ComposeError> {
    let Some(mut remaining) = value else {
        return Ok(None);
    };
    let mut total = 0.0;
    while !remaining.is_empty() {
        let split = remaining
            .find(|character: char| !character.is_ascii_digit() && character != '.')
            .ok_or_else(|| invalid(format!("invalid duration '{remaining}'")))?;
        let amount = remaining[..split]
            .parse::<f64>()
            .map_err(|_| invalid(format!("invalid duration '{remaining}'")))?;
        remaining = &remaining[split..];
        let unit_len = remaining
            .find(|character: char| character.is_ascii_digit() || character == '.')
            .unwrap_or(remaining.len());
        let unit = &remaining[..unit_len];
        total += amount
            * match unit {
                "ns" => 0.000_001,
                "us" | "µs" => 0.001,
                "ms" => 1.0,
                "s" => 1_000.0,
                "m" => 60_000.0,
                "h" => 3_600_000.0,
                _ => return Err(invalid(format!("invalid duration unit '{unit}'"))),
            };
        remaining = &remaining[unit_len..];
    }
    Ok(Some(total as u64))
}

pub(super) fn mapping_string(map: &serde_norway::Mapping, key: &str) -> Option<String> {
    map.get(Value::String(key.into())).and_then(scalar)
}

fn mapping_i64(map: &serde_norway::Mapping, key: &str) -> Option<i64> {
    map.get(Value::String(key.into())).and_then(integer)
}

pub(super) fn is_external(value: &Value) -> bool {
    matches!(value, Value::Bool(true)) || matches!(value, Value::Mapping(_))
}

fn is_cdi_name(value: &str) -> bool {
    value
        .split_once('=')
        .is_some_and(|(class, name)| class.contains('/') && !name.is_empty())
}

pub(super) fn invalid(error: impl std::fmt::Display) -> ComposeError {
    ComposeError::Invalid(error.to_string())
}
