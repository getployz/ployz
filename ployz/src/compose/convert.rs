use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    path::{Path, PathBuf},
};

use ployz_core::{
    ConfiguredHealthcheck, ContainerPath, ContainerResources, DeviceMapping, DeviceReservation,
    HEALTHCHECK_DISABLE_SENTINEL, HealthcheckCommand, HealthcheckSpec, LogDriver, MachinePath,
    MachineSelector, Placement, PortPublication, PullPolicy, RequestedServiceSpec, RestartPolicy,
    ServiceContainerSpec, ServiceMode, ServiceName, Ulimit, UpdateConfig, UpdateOrder,
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
    secrets::validate_secret,
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
    let mut warnings = Vec::new();
    for (service_name, service) in &raw.services {
        warnings.extend(classify(service_name, service));
        let service_dependencies = dependency_names(service);
        if let Some(missing) = service_dependencies
            .iter()
            .find(|dependency| !raw.services.contains_key(*dependency))
        {
            return Err(invalid(format!(
                "service '{service_name}' depends on undefined service '{missing}'"
            )));
        }
        if completed_dependency(service) {
            return Err(invalid(format!(
                "service '{service_name}': depends_on condition 'service_completed_successfully' is not supported; use x-pre_deploy"
            )));
        }
        dependencies.insert(service_name.clone(), service_dependencies);
        let (spec, build) =
            convert_service(&name, service_name, service, &raw, &working_dir, &images)?;
        if let Some(build) = build {
            builds.insert(service_name.clone(), build);
        }
        services.insert(service_name.clone(), spec);
    }
    Ok(ComposeProject {
        name,
        working_dir,
        context: raw.context,
        services,
        builds,
        dependencies,
        warnings,
        secrets: raw.secrets,
        environment,
        resolved_secrets: BTreeMap::new(),
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
                visit(dependency, project, visiting, visited, ordered)?;
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
    directory: &Path,
    images: &ImageState,
) -> Result<(RequestedServiceSpec, Option<BuildSpec>), ComposeError> {
    let build = raw.build.as_ref().map(build_spec);
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
    let caddy_config = caddy(raw.caddy.as_ref(), directory, name)?;
    if caddy_config
        .as_ref()
        .is_some_and(|config| !config.is_empty())
        && ports
            .iter()
            .any(|port| matches!(port, PortPublication::Ingress { .. }))
    {
        return Err(invalid(format!(
            "service '{name}': ingress ports and 'x-caddy' cannot be specified simultaneously"
        )));
    }
    let (volumes, mounts) = volumes(raw, root, project)?;
    let (configs, config_mounts) = configs(raw, root, directory)?;
    let placement = Placement {
        machines: raw
            .machines
            .as_ref()
            .map(string_list)
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|machine| MachineSelector::parse(machine).map_err(invalid))
            .collect::<Result<_, _>>()?,
    };
    // TODO(UT-076): preserve the baseline gap: service-level `tmpfs` is not reinterpreted as mounts.
    let container = ServiceContainerSpec {
        image,
        command: shell(&raw.command)?,
        entrypoint: shell(&raw.entrypoint)?,
        environment: environment(&raw.environment)?,
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
        config_mounts,
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
            volumes,
            mounts,
            configs,
            pre_deploy: pre_deploy(raw.pre_deploy.as_ref())?,
            caddy_config,
            update: update(raw.deploy.as_ref())?,
        },
        build,
    ))
}

fn build_spec(value: &Value) -> BuildSpec {
    match value {
        Value::String(context) => BuildSpec {
            context: Some(context.clone()),
            raw: value.clone(),
            ..Default::default()
        },
        Value::Mapping(map) => BuildSpec {
            context: mapping_string(map, "context"),
            dockerfile: mapping_string(map, "dockerfile"),
            additional_services: map
                .get(Value::String("additional_contexts".into()))
                .into_iter()
                .flat_map(additional_contexts)
                .collect(),
            raw: value.clone(),
        },
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Sequence(_) | Value::Tagged(_) => {
            BuildSpec {
                raw: value.clone(),
                ..Default::default()
            }
        }
    }
}

fn additional_contexts(value: &Value) -> Vec<String> {
    let values = match value {
        Value::Mapping(map) => map.values().filter_map(Value::as_str).collect(),
        Value::Sequence(values) => values
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|value| value.split_once('=').map(|(_, context)| context))
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            Vec::new()
        }
    };
    values
        .into_iter()
        .filter_map(|context| context.strip_prefix("service:"))
        .map(str::to_owned)
        .collect()
}

fn resources(raw: &RawService) -> Result<ContainerResources, ComposeError> {
    let mut resources = ContainerResources {
        cpu_nanos: raw
            .cpus
            .as_ref()
            .map(|value| number(value).ok_or_else(|| invalid("cpus must be numeric")))
            .transpose()?
            .map(|cpus| (cpus * 1e9) as i64),
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
                    (number(cpus).ok_or_else(|| invalid("deploy.limits.cpus must be numeric"))?
                        * 1e9) as i64,
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

fn classify(name: &str, service: &RawService) -> Vec<String> {
    const FEATURES: &[&str] = &[
        "dns",
        "dns_search",
        "labels",
        "links",
        "security_opt",
        "storage_opt",
    ];
    // TODO(UT-077): unsupported-field detection intentionally remains incomplete.
    let mut warnings = FEATURES
        .iter()
        .filter(|feature| {
            service
                .other
                .get(**feature)
                .is_some_and(|value| !value.is_null())
        })
        .map(|feature| format!("service '{name}': unsupported feature '{feature}'"))
        .collect::<Vec<_>>();
    for feature in ["mem_swappiness", "memswap_limit"] {
        if service
            .other
            .get(feature)
            .and_then(integer)
            .is_some_and(|value| value > 0)
        {
            warnings.push(format!("service '{name}': unsupported feature '{feature}'"));
        }
    }
    if service.other.get("networks").is_some_and(custom_networks) {
        warnings.push(format!("service '{name}': unsupported feature 'networks'"));
    }
    // TODO(UT-047): secret file mounts remain unsupported; plaintext env references are separate.
    if !service.secrets.is_empty() {
        warnings.push(format!("service '{name}': unsupported feature 'secrets'"));
    }
    warnings
}

fn validate_definitions(project: &RawProject) -> Result<(), ComposeError> {
    for (name, config) in &project.configs {
        // TODO(UT-002): external config objects remain unsupported.
        if is_external(&config.external) {
            return Err(invalid(format!(
                "external configs are not supported: {name}"
            )));
        }
        // TODO(UT-003, UT-004): config labels and environment sources remain ignored.
        let _ = (&config.labels, &config.environment);
    }
    for (name, secret) in &project.secrets {
        validate_secret(name, secret)?;
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

fn dependency_names(service: &RawService) -> Vec<String> {
    match &service.depends_on {
        Value::Mapping(map) => map
            .keys()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Value::Sequence(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {
            Vec::new()
        }
    }
}

fn completed_dependency(service: &RawService) -> bool {
    match &service.depends_on {
        Value::Mapping(map) => map.values().any(|value| {
            matches!(value, Value::Mapping(dependency) if mapping_string(dependency, "condition").as_deref() == Some("service_completed_successfully"))
        }),
        Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Sequence(_)
        | Value::Tagged(_) => false,
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

fn bytes(value: &Value) -> Option<i64> {
    bytes_u64(value).and_then(|value| i64::try_from(value).ok())
}

fn optional_bytes(value: Option<&Value>, field: &str) -> Result<Option<i64>, ComposeError> {
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

pub(super) fn duration_millis(value: Option<&str>) -> Result<Option<u64>, ComposeError> {
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
