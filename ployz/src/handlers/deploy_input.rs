use std::{collections::BTreeMap, fs, num::NonZeroU32};

use clap::ArgMatches;
use ployz_core::{
    ContainerPath, ContainerResources, DockerVolumeName, IngressProxyFragment, MachineTarget,
    Placement, PortPublication, PullPolicy, RequestedServiceSpec, RestartPolicy,
    ServiceContainerSpec, ServiceId, ServiceMode, ServiceMount, ServiceName, ServiceVolume,
    ServiceVolumeGraph, ServiceVolumeReference, Ulimit, UpdateConfig, VolumeSource,
};

use crate::{
    compose::{parse_bytes, parse_extension_port},
    image::with_default_tag,
};

use super::{Error, required, string_values};

pub(super) fn run_spec(matches: &ArgMatches) -> Result<RequestedServiceSpec, Error> {
    let image = with_default_tag(&required(matches, "image")?);
    let name = matches
        .get_one::<String>("name")
        .cloned()
        .unwrap_or_else(|| generated_name(&image));
    let name = ServiceName::parse(name)?;
    let mode = match required(matches, "mode")?.as_str() {
        "global" => {
            if matches.value_source("replicas") != Some(clap::parser::ValueSource::DefaultValue) {
                return Err(Error::usage(
                    "replicas can only be specified for replicated services",
                ));
            }
            ServiceMode::Global
        }
        "replicated" => ServiceMode::Replicated {
            replicas: NonZeroU32::new(parse_u32(matches, "replicas")?)
                .ok_or_else(|| Error::usage("replicas must be greater than zero"))?,
        },
        mode => return Err(Error::usage(format!("unsupported service mode '{mode}'"))),
    };
    let ports = string_values(matches, "publish")
        .into_iter()
        .map(|value| parse_extension_port(&value))
        .collect::<Result<Vec<_>, _>>()?;
    let caddy_config = matches
        .get_one::<String>("caddyfile")
        .map(fs::read_to_string)
        .transpose()
        .map_err(|error| Error::usage(format!("read caddyfile: {error}")))?;
    if caddy_config.is_some()
        && ports
            .iter()
            .any(|port| matches!(port, PortPublication::Ingress { .. }))
    {
        return Err(Error::usage(
            "ingress ports and --caddyfile cannot be specified together",
        ));
    }
    let (volumes, mounts) = parse_volumes(&string_values(matches, "volume"))?;
    let volume_graph = ServiceVolumeGraph::parse(volumes, mounts)
        .map_err(|error| Error::usage(error.to_string()))?;
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
                        shell_words::split(value)
                    }
                })
                .transpose()?
                .unwrap_or_default(),
            environment: parse_environment(&string_values(matches, "env"))?,
            labels: Default::default(),
            hostname: None,
            extra_hosts: Vec::new(),
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            healthcheck: None,
            pull_policy: match required(matches, "pull")?.as_str() {
                "always" => PullPolicy::Always,
                "missing" => PullPolicy::Missing,
                "never" => PullPolicy::Never,
                policy => return Err(Error::usage(format!("unsupported pull policy '{policy}'"))),
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
            stop_timeout_secs: None,
            sysctls: BTreeMap::new(),
            restart: RestartPolicy::No,
        },
        placement: Placement {
            machines: string_values(matches, "machine")
                .into_iter()
                .map(MachineTarget::parse)
                .collect::<Result<_, _>>()?,
        },
        ports,
        volume_graph,
        config_graph: Default::default(),
        pre_deploy: None,
        ingress_proxy_fragment: caddy_config
            .filter(|config| !config.trim().is_empty())
            .map(|config| IngressProxyFragment::parse(&config))
            .transpose()?,
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
                    std::env::var(value).map_err(|_| {
                        Error::usage(format!("environment variable '{value}' is not set"))
                    })?,
                ),
            };
            if key.is_empty() {
                return Err(Error::usage("environment variable name cannot be empty"));
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
            return Err(Error::usage(format!(
                "invalid volume '{value}'; expected SOURCE:TARGET[:ro]"
            )));
        }
        if option.is_some_and(|option| !matches!(option, "ro" | "volume-nocopy")) {
            return Err(Error::usage(format!(
                "unsupported volume option in '{value}'"
            )));
        }
        let reference = ServiceVolumeReference::parse(format!("mount-{index}"))
            .expect("generated Volume reference is valid");
        let source = if source.starts_with('/') {
            if option == Some("volume-nocopy") {
                return Err(Error::usage(format!(
                    "volume-nocopy requires a named volume in '{value}'"
                )));
            }
            VolumeSource::Bind {
                machine_path: ployz_core::MachinePath::parse(source)?,
                create_machine_path: true,
                propagation: None,
                recursive: None,
            }
        } else {
            VolumeSource::Ordinary {
                name: DockerVolumeName::parse(source)?,
                driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new())?,
                labels: BTreeMap::new(),
            }
        };
        volumes.push(ServiceVolume {
            reference: reference.clone(),
            source,
        });
        mounts.push(ServiceMount {
            volume: reference,
            target: ContainerPath::parse(target)?,
            read_only: option == Some("ro"),
            no_copy: option == Some("volume-nocopy"),
            subpath: None,
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
                .ok_or_else(|| Error::usage(format!("invalid ulimit '{value}'")))?;
            let (soft, hard) = limits
                .split_once(':')
                .map_or((limits, limits), |(soft, hard)| (soft, hard));
            let soft = soft
                .parse()
                .map_err(|_| Error::usage(format!("invalid ulimit '{value}'")))?;
            let hard = hard
                .parse()
                .map_err(|_| Error::usage(format!("invalid ulimit '{value}'")))?;
            Ok((name.to_owned(), Ulimit { soft, hard }))
        })
        .collect()
}

fn parse_cpu(value: &str) -> Result<i64, Error> {
    let cpu = value
        .parse::<f64>()
        .map_err(|_| Error::usage("cpu must be numeric"))?;
    if !cpu.is_finite() || cpu < 0.0 {
        return Err(Error::usage("cpu must be a non-negative finite number"));
    }
    Ok((cpu * 1e9) as i64)
}

fn optional_bytes(matches: &ArgMatches, name: &str) -> Result<Option<i64>, Error> {
    matches
        .get_one::<String>(name)
        .map(|value| {
            parse_bytes(value)
                .and_then(|value| i64::try_from(value).ok())
                .ok_or_else(|| Error::usage(format!("{name} must be a byte size")))
        })
        .transpose()
}

pub(super) fn parse_u32(matches: &ArgMatches, name: &str) -> Result<u32, Error> {
    required(matches, name)?
        .parse()
        .map_err(|_| Error::usage(format!("{name} must be a positive integer")))
}
