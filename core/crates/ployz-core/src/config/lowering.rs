//! Lower resolved Cloud service settings to typed runtime deployment requests.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use super::{ConfigError, ServiceHealthcheck, ServiceSource, parse_service_config};
use crate::{
    ByteQuantity, ContainerResources, CpuNanos, DeployIntent, HealthcheckSpec, HttpHealthcheck,
    HttpProtocol, IngressHostname, PlanOptions, PortPublication, PreDeployCommand, PreDeployHook,
    ProjectName, PullPolicy, RawVolumeSource, RequestedServiceSpec, RestartPolicy, ServiceAttempt,
    ServiceContainerSpec, ServiceMode, ServiceMount, ServiceVolume, ServiceVolumeGraph,
    VolumeDriver,
};

/// Captured node settings plus adapter-resolved runtime inputs for one Project.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LowerDeploymentInput {
    project_name: ProjectName,
    snapshots: Vec<LowerDeploymentSnapshot>,
    #[serde(default)]
    volumes: Vec<LowerDeploymentVolume>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LowerDeploymentSnapshot {
    config: Value,
    replicas: Option<u8>,
    #[serde(default)]
    resolved_env: BTreeMap<String, String>,
    healthcheck_port: Option<u16>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LowerDeploymentVolume {
    volume_resource_id: String,
}

/// Lower captured authored settings and adapter-supplied image/environment inputs.
/// No source lookup, build, provider execution, or Cluster observation occurs here.
///
/// # Errors
/// Returns ConfigError when a source lacks a pullable image, a setting is unsupported by the runtime,
/// or resolved ports, limits, mounts, or commands cannot form a valid runtime request.
pub fn lower_deployment(input: LowerDeploymentInput) -> Result<DeployIntent, ConfigError> {
    let volume_ids: BTreeSet<_> = input
        .volumes
        .iter()
        .map(|v| v.volume_resource_id.as_str())
        .collect();
    let mut target: Vec<RequestedServiceSpec> = Vec::new();
    for snapshot in input.snapshots {
        let super::ServiceConfig {
            settings: config,
            mounts: configured_mounts,
            ..
        } = parse_service_config(snapshot.config)?;
        let image = match &config.source {
            ServiceSource::Empty { .. } => continue,
            ServiceSource::Git { .. } => {
                return Err(ConfigError::at(
                    "source",
                    "Git service is missing a pullable image",
                ));
            }
            ServiceSource::Image { image, .. } => image,
        };
        if config.cron.is_some() {
            return Err(ConfigError::at(
                "cron",
                "Cron scheduling is not supported by the runtime",
            ));
        }
        let healthcheck = match &config.healthcheck {
            ServiceHealthcheck::None => None,
            ServiceHealthcheck::Http {
                path,
                timeout_seconds,
            } => {
                let port = snapshot
                    .healthcheck_port
                    .or_else(|| {
                        snapshot
                            .resolved_env
                            .get("PORT")
                            .and_then(|v| v.parse::<u16>().ok())
                    })
                    .filter(|p| *p != 0)
                    .ok_or_else(|| {
                        ConfigError::at(
                            "healthcheck",
                            "HTTP healthcheck requires a valid PORT variable",
                        )
                    })?;
                Some(HealthcheckSpec::Http(HttpHealthcheck {
                    path: path.clone(),
                    port: port.try_into().map_err(lowering_error)?,
                    timeout_seconds: *timeout_seconds,
                }))
            }
        };
        let replicas = snapshot.replicas.unwrap_or(config.replicas);
        if replicas == 0 || replicas > 50 {
            return Err(ConfigError::at(
                "replicas",
                "Runtime deployment requires 1–50 replicas",
            ));
        }
        let cpu_nanos = config
            .cpu_limit
            .map(CpuNanos::from_cpus)
            .transpose()
            .map_err(|_| ConfigError::at("cpuLimit", "Invalid CPU limit"))?;
        if cpu_nanos.is_some_and(|n| n.get() == 0) {
            return Err(ConfigError::at(
                "cpuLimit",
                "CPU limit is below one nanocpu",
            ));
        }
        let memory_bytes = config.mem_limit.map(|gb| (gb * 1_000_000_000.0) as i64);
        if memory_bytes == Some(0) {
            return Err(ConfigError::at(
                "memLimit",
                "Memory limit is below one byte",
            ));
        }
        let restart = match config.restart_policy.0 {
            RestartPolicy::OnFailure { .. } => RestartPolicy::OnFailure {
                maximum_retry_count: Some(i64::from(config.max_retries)),
            },
            policy @ (RestartPolicy::No | RestartPolicy::Always | RestartPolicy::UnlessStopped) => {
                policy
            }
        };
        let mounted: Vec<_> = configured_mounts
            .iter()
            .filter(|m| volume_ids.contains(m.volume_resource_id.as_str()))
            .collect();
        let mut volumes = Vec::new();
        let mut mounts = Vec::new();
        for mount in mounted {
            let name = format!("vol-{}", mount.volume_resource_id);
            let reference: crate::ServiceVolumeReference =
                name.clone().try_into().map_err(lowering_error)?;
            volumes.push(ServiceVolume {
                reference: reference.clone(),
                source: RawVolumeSource::Ordinary {
                    name: name.try_into().map_err(lowering_error)?,
                    driver: VolumeDriver::parse("local", BTreeMap::new())
                        .map_err(lowering_error)?,
                    labels: BTreeMap::new(),
                }
                .try_into()
                .map_err(lowering_error)?,
            });
            mounts.push(ServiceMount {
                volume: reference,
                target: mount
                    .mount_path
                    .clone()
                    .try_into()
                    .map_err(lowering_error)?,
                read_only: false,
                no_copy: false,
                subpath: None,
            });
        }
        let mut ports = Vec::new();
        for route in &config.routes {
            ports.push(PortPublication::Ingress {
                hostname: IngressHostname::explicit(route.hostname.clone())
                    .map_err(lowering_error)?,
                load_balancer_port: std::num::NonZeroU16::new(443).expect("HTTPS port is nonzero"),
                container_port: route.target_port.try_into().map_err(lowering_error)?,
                http_protocol: HttpProtocol::Https,
            });
        }
        if let Some(hostname) = config.managed_hostname {
            let port = hostname
                .target_port
                .or_else(|| {
                    snapshot
                        .resolved_env
                        .get("PORT")
                        .and_then(|v| v.parse::<u16>().ok())
                })
                .filter(|p| *p != 0)
                .ok_or_else(|| {
                    ConfigError::at(
                        "managedHostname",
                        "Managed domain requires a valid target port or PORT variable",
                    )
                })?;
            ports.push(PortPublication::Ingress {
                hostname: IngressHostname::ClusterDomain {
                    label: Some(hostname.prefix.try_into().map_err(lowering_error)?),
                },
                load_balancer_port: std::num::NonZeroU16::new(443).expect("HTTPS port is nonzero"),
                container_port: port.try_into().map_err(lowering_error)?,
                http_protocol: HttpProtocol::Https,
            });
        }
        let command = |command: &str| vec!["/bin/sh".into(), "-c".into(), command.into()];
        let pre_deploy = config
            .pre_deploy_command
            .as_deref()
            .map(|value| {
                Ok::<_, ConfigError>(PreDeployHook {
                    command: PreDeployCommand::parse(command(value)).map_err(lowering_error)?,
                    environment: BTreeMap::new(),
                    privileged: None,
                    timeout_millis: None,
                    user: None,
                })
            })
            .transpose()?;
        let mut spec = RequestedServiceSpec {
            name: config.private_dns,
            mode: ServiceMode::Replicated {
                replicas: u32::from(replicas).try_into().map_err(lowering_error)?,
            },
            container: ServiceContainerSpec {
                image: image.clone(),
                command: config
                    .start_command
                    .as_deref()
                    .map(command)
                    .unwrap_or_default(),
                environment: snapshot.resolved_env,
                pull_policy: PullPolicy::Missing,
                restart,
                healthcheck,
                resources: ContainerResources {
                    cpu_nanos,
                    memory_bytes: memory_bytes
                        .map(ByteQuantity::try_from)
                        .transpose()
                        .map_err(lowering_error)?,
                    ..Default::default()
                },
                entrypoint: Vec::new(),
                labels: Default::default(),
                hostname: None,
                extra_hosts: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                init: None,
                user: None,
                working_directory: None,
                tty: false,
                open_stdin: false,
                privileged: false,
                pid_mode: None,
                log_driver: None,
                stop_timeout_secs: None,
                sysctls: BTreeMap::new(),
            },
            placement: Default::default(),
            ports,
            mount_graph: Default::default(),
            pre_deploy,
            ingress_proxy_fragment: None,
            update: Default::default(),
        };
        spec.set_volume_graph(ServiceVolumeGraph::parse(volumes, mounts).map_err(lowering_error)?)
            .map_err(lowering_error)?;
        target.push(spec);
    }
    let selected = target
        .iter()
        .map(|spec| ServiceAttempt {
            name: spec.name.clone(),
        })
        .collect();
    Ok(DeployIntent::new(
        input.project_name,
        target,
        PlanOptions {
            force_recreate: false,
            skip_health_monitor: false,
            placement_seed: 0,
            selected,
        },
    ))
}

fn lowering_error(_: impl std::fmt::Display) -> ConfigError {
    ConfigError::at(
        "service",
        "Authored service could not be lowered to a runtime specification",
    )
}
