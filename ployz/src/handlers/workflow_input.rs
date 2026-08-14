use std::{collections::BTreeMap, fs, num::NonZeroU32};

use clap::ArgMatches;
use ployz_core::{
    ContainerPath, ContainerResources, DockerVolumeName, MachineSelector, Placement,
    PortPublication, PullPolicy, RequestedServiceSpec, ResolvedServiceSpec, ServiceContainerSpec,
    ServiceId, ServiceMode, ServiceMount, ServiceName, ServiceVolume, ServiceVolumeReference,
    Ulimit, UpdateConfig, VolumeSource,
};

use crate::compose::{parse_bytes, parse_extension_port};

use super::{Error, string_values};

pub(super) fn requested_from_resolved(resolved: &ResolvedServiceSpec) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: resolved.name.clone(),
        mode: resolved.mode.clone(),
        container: resolved.container.clone(),
        placement: resolved.placement.clone(),
        ports: resolved.ports.clone(),
        volumes: resolved.volumes.clone(),
        mounts: resolved.mounts.clone(),
        configs: resolved.configs.clone(),
        pre_deploy: resolved.pre_deploy.clone(),
        caddy_config: resolved.caddy_config.clone(),
        update: UpdateConfig {
            order: Some(resolved.update.order),
            monitor_millis: resolved.update.monitor_millis,
        },
    }
}

pub(super) fn run_spec(matches: &ArgMatches) -> Result<RequestedServiceSpec, Error> {
    let image = required(matches, "image")?;
    let name = matches
        .get_one::<String>("name")
        .cloned()
        .unwrap_or_else(|| generated_name(&image));
    let name = ServiceName::parse(name).map_err(|error| error.to_string())?;
    let mode = match required(matches, "mode")?.as_str() {
        "global" => ServiceMode::Global,
        "replicated" => ServiceMode::Replicated {
            replicas: NonZeroU32::new(parse_u32(matches, "replicas")?)
                .ok_or_else(|| Error::from("replicas must be greater than zero"))?,
        },
        mode => return Err(format!("unsupported service mode '{mode}'").into()),
    };
    let ports = string_values(matches, "publish")
        .into_iter()
        .map(|value| parse_extension_port(&value).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    // TODO(UT-044, UT-096): run preserves the baseline's lack of L4 ingress support.
    if ports
        .iter()
        .any(|port| matches!(port, PortPublication::IngressTransport { .. }))
    {
        return Err("run supports TCP/UDP publications only in @host mode".into());
    }
    let caddy_config = matches
        .get_one::<String>("caddyfile")
        .map(fs::read_to_string)
        .transpose()
        .map_err(|error| format!("read caddyfile: {error}"))?;
    if caddy_config.is_some()
        && ports
            .iter()
            .any(|port| matches!(port, PortPublication::Ingress { .. }))
    {
        return Err("ingress ports and --caddyfile cannot be specified together".into());
    }
    let (volumes, mounts) = parse_volumes(&string_values(matches, "volume"))?;
    Ok(RequestedServiceSpec {
        name,
        mode,
        container: ServiceContainerSpec {
            image,
            command: string_values(matches, "command"),
            entrypoint: matches
                .get_one::<String>("entrypoint")
                .map(|value| {
                    if value.is_empty() {
                        Ok(vec![String::new()])
                    } else {
                        shell_words::split(value).map_err(|error| error.to_string())
                    }
                })
                .transpose()?
                .unwrap_or_default(),
            environment: parse_environment(&string_values(matches, "env"))?,
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            healthcheck: None,
            pull_policy: match required(matches, "pull")?.as_str() {
                "always" => PullPolicy::Always,
                "missing" => PullPolicy::Missing,
                "never" => PullPolicy::Never,
                policy => return Err(format!("unsupported pull policy '{policy}'").into()),
            },
            init: None,
            user: matches.get_one::<String>("user").cloned(),
            working_directory: None,
            tty: false,
            open_stdin: false,
            privileged: matches.get_flag("privileged"),
            pid_mode: None,
            log_driver: None,
            resources: ContainerResources {
                cpu_nanos: matches
                    .get_one::<String>("cpu")
                    .map(|value| parse_cpu(value))
                    .transpose()?,
                memory_bytes: optional_bytes(matches, "memory")?,
                shared_memory_bytes: optional_bytes(matches, "shm-size")?,
                ulimits: parse_ulimits(&string_values(matches, "ulimit"))?,
                ..Default::default()
            },
            stop_grace_period_millis: None,
            sysctls: BTreeMap::new(),
            config_mounts: Vec::new(),
        },
        placement: Placement {
            machines: string_values(matches, "machine")
                .into_iter()
                .map(|value| MachineSelector::parse(value).map_err(|error| error.to_string()))
                .collect::<Result<_, _>>()?,
        },
        ports,
        volumes,
        mounts,
        configs: Vec::new(),
        pre_deploy: None,
        caddy_config,
        update: UpdateConfig::default(),
    })
}

fn generated_name(image: &str) -> String {
    let base = image
        .split('@')
        .next()
        .unwrap_or(image)
        .rsplit('/')
        .next()
        .unwrap_or(image)
        .split(':')
        .next()
        .unwrap_or("service");
    let mut base = base
        .chars()
        .map(|character| {
            let character = character.to_ascii_lowercase();
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    base = base.trim_matches('-').chars().take(54).collect();
    if base.is_empty() {
        base = "service".into();
    }
    format!("{base}-{}", &ServiceId::random().as_str()[..8])
}

fn parse_environment(values: &[String]) -> Result<BTreeMap<String, String>, Error> {
    values
        .iter()
        .map(|value| {
            let (key, value) = match value.split_once('=') {
                Some(pair) => (pair.0, pair.1.to_owned()),
                None => (
                    value.as_str(),
                    std::env::var(value)
                        .map_err(|_| format!("environment variable '{value}' is not set"))?,
                ),
            };
            if key.is_empty() {
                return Err(Error::from("environment variable name cannot be empty"));
            }
            Ok((key.to_owned(), value))
        })
        .collect()
}

fn parse_volumes(values: &[String]) -> Result<(Vec<ServiceVolume>, Vec<ServiceMount>), Error> {
    let mut volumes = Vec::new();
    let mut mounts = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let mut parts = value.split(':');
        let source = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or_default();
        let option = parts.next();
        if source.is_empty() || target.is_empty() || parts.next().is_some() {
            return Err(format!("invalid volume '{value}'; expected SOURCE:TARGET[:ro]").into());
        }
        if option.is_some_and(|option| option != "ro") {
            return Err(format!("unsupported volume option in '{value}'").into());
        }
        let reference = ServiceVolumeReference::parse(format!("mount-{index}"))
            .expect("generated Volume reference is valid");
        let source = if source.starts_with('/') {
            VolumeSource::Bind {
                machine_path: ployz_core::MachinePath::parse(source)
                    .map_err(|error| error.to_string())?,
                create_machine_path: true,
                propagation: None,
                recursive: None,
            }
        } else {
            VolumeSource::Named {
                name: DockerVolumeName::parse(source).map_err(|error| error.to_string())?,
                external: false,
                driver: None,
                labels: BTreeMap::new(),
                no_copy: false,
                subpath: None,
            }
        };
        volumes.push(ServiceVolume {
            reference: reference.clone(),
            source,
        });
        mounts.push(ServiceMount {
            volume: reference,
            target: ContainerPath::parse(target).map_err(|error| error.to_string())?,
            read_only: option == Some("ro"),
        });
    }
    Ok((volumes, mounts))
}

fn parse_ulimits(values: &[String]) -> Result<BTreeMap<String, Ulimit>, Error> {
    values
        .iter()
        .map(|value| {
            let (name, limits) = value
                .split_once('=')
                .ok_or_else(|| Error::from(format!("invalid ulimit '{value}'")))?;
            let (soft, hard) = limits
                .split_once(':')
                .map_or((limits, limits), |(soft, hard)| (soft, hard));
            let soft = soft
                .parse()
                .map_err(|_| format!("invalid ulimit '{value}'"))?;
            let hard = hard
                .parse()
                .map_err(|_| format!("invalid ulimit '{value}'"))?;
            Ok((name.to_owned(), Ulimit { soft, hard }))
        })
        .collect()
}

fn parse_cpu(value: &str) -> Result<i64, Error> {
    let cpu = value
        .parse::<f64>()
        .map_err(|_| Error::from("cpu must be numeric"))?;
    if !cpu.is_finite() || cpu < 0.0 {
        return Err("cpu must be a non-negative finite number".into());
    }
    Ok((cpu * 1e9) as i64)
}

fn optional_bytes(matches: &ArgMatches, name: &str) -> Result<Option<i64>, Error> {
    matches
        .get_one::<String>(name)
        .map(|value| {
            parse_bytes(value)
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| Error::from(format!("{name} must be a byte size")))
        })
        .transpose()
}

fn parse_u32(matches: &ArgMatches, name: &str) -> Result<u32, Error> {
    required(matches, name)?
        .parse()
        .map_err(|_| format!("{name} must be a positive integer").into())
}

pub(super) fn required(matches: &ArgMatches, name: &str) -> Result<String, Error> {
    matches
        .get_one::<String>(name)
        .cloned()
        .ok_or_else(|| format!("{name} is required").into())
}
