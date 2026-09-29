//! Lower resolved Cloud service settings to typed runtime deployment requests.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;

use super::{
    ConfigError, ServiceConfig, ServiceEnvValue, ServiceHealthcheck, ServiceSource, ValuePart,
    ValuePartOwner, parse_service_config,
};
use crate::{
    ByteQuantity, ContainerResources, CpuNanos, DependencyCondition, DeployIntent, HealthcheckSpec,
    HttpHealthcheck, HttpProtocol, IngressHost, PlanOptions, PortPublication, PreDeployCommand,
    PreDeployHook, ProjectName, PullPolicy, RawVolumeSource, RequestedServiceSpec, RestartPolicy,
    ServiceAttempt, ServiceContainerSpec, ServiceDependency, ServiceMode, ServiceMount,
    ServiceName, ServiceVolume, ServiceVolumeGraph, VolumeDriver,
};

/// Injected into a Cloud-authored service only when it has no authored PORT.
pub const DEFAULT_SERVICE_PORT: u16 = 8080;

fn target_port(
    explicit: Option<u16>,
    environment: &BTreeMap<String, String>,
    path: &str,
) -> Result<std::num::NonZeroU16, ConfigError> {
    explicit
        .or_else(|| environment.get("PORT").and_then(|value| value.parse().ok()))
        .and_then(std::num::NonZeroU16::new)
        .ok_or_else(|| {
            ConfigError::at(path, "Expected a target port or PORT variable from 1–65535")
        })
}

/// Captured node settings plus adapter-resolved runtime inputs for one Project.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LowerDeploymentInput {
    project_name: ProjectName,
    snapshots: Vec<LowerDeploymentSnapshot>,
    #[serde(default)]
    volumes: Vec<LowerDeploymentVolume>,
    /// Service ID by lineage, from the attempt's frozen variable producers. References
    /// resolved through it order the deploy.
    #[serde(default)]
    lineages: BTreeMap<String, String>,
    /// Omitted preserves partial-deploy behavior; empty reconciles the complete target.
    selected: Option<Vec<ServiceAttempt>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LowerDeploymentSnapshot {
    service_id: Option<String>,
    config: Value,
    replicas: Option<u8>,
    #[serde(default)]
    resolved_env: BTreeMap<String, String>,
    /// Run in order after the service's own pre-deploy command, in the same hook.
    #[serde(default)]
    setup_commands: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LowerDeploymentVolume {
    volume_resource_id: String,
}

/// Lower captured authored settings and adapter-supplied image/environment inputs.
/// No source lookup, build, provider execution, or Cluster observation occurs here.
/// Missing PORT defaults to 8080; authored values take precedence, including invalid ones.
/// Domains without an explicit target and HTTP healthchecks use that same container PORT.
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
    let snapshots = input
        .snapshots
        .into_iter()
        .map(|mut snapshot| Ok((parse_service_config(snapshot.config.take())?, snapshot)))
        .collect::<Result<Vec<_>, ConfigError>>()?;
    let dependencies = deployment_dependencies(&snapshots, &input.lineages);
    let mut target: Vec<RequestedServiceSpec> = Vec::new();
    for (parsed, snapshot) in snapshots {
        let ServiceConfig {
            settings: config,
            mounts: configured_mounts,
            ..
        } = parsed;
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
        let mut environment = snapshot.resolved_env;
        environment
            .entry("PORT".into())
            .or_insert_with(|| DEFAULT_SERVICE_PORT.to_string());
        let healthcheck = match &config.healthcheck {
            ServiceHealthcheck::None => None,
            ServiceHealthcheck::Http {
                path,
                timeout_seconds,
            } => {
                let port = target_port(None, &environment, "healthcheck")?;
                Some(HealthcheckSpec::Http(HttpHealthcheck {
                    path: path.clone(),
                    port,
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
                hostname: IngressHost::parse(route.hostname.clone()).map_err(lowering_error)?,
                load_balancer_port: std::num::NonZeroU16::new(443).expect("HTTPS port is nonzero"),
                container_port: target_port(route.target_port, &environment, "routes")?,
                http_protocol: HttpProtocol::Https,
            });
        }
        // Managed hostnames are Cloud's authored shorthand; Cloud expands them into
        // explicit routes before lowering because only it knows the Cluster Domain.
        if !config.managed_hostnames.is_empty() {
            return Err(ConfigError::at(
                "managedHostnames",
                "Expand managed hostnames into explicit routes before deploying",
            ));
        }
        let command = |command: &str| vec!["/bin/sh".into(), "-c".into(), command.into()];
        let mut hook_commands: Vec<String> = config.pre_deploy_command.into_iter().collect();
        for mut setup in snapshot.setup_commands {
            super::validation::trimmed(&mut setup, "setupCommands", 2000)?;
            hook_commands.push(setup);
        }
        // Each command is its own positional argument run by its own shell, so a
        // trailing comment or quote in one can never swallow or change the next.
        let hook_command = match hook_commands.as_slice() {
            [] => None,
            [only] => Some(command(only)),
            _ => Some(
                [
                    "/bin/sh",
                    "-c",
                    r#"for c do /bin/sh -c "$c" || exit; done"#,
                    "sh",
                ]
                .into_iter()
                .map(String::from)
                .chain(hook_commands)
                .collect(),
            ),
        };
        let pre_deploy = hook_command
            .map(|value| {
                Ok::<_, ConfigError>(PreDeployHook {
                    command: PreDeployCommand::parse(value).map_err(lowering_error)?,
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
                environment,
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
                labels: crate::ContainerLabels::parse(
                    snapshot
                        .service_id
                        .map(|id| ("cloud.ployz.service.id".into(), id))
                        .into_iter()
                        .collect(),
                )
                .map_err(lowering_error)?,
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
            update: Default::default(),
        };
        spec.set_volume_graph(ServiceVolumeGraph::parse(volumes, mounts).map_err(lowering_error)?)
            .map_err(lowering_error)?;
        target.push(spec);
    }
    let selected = input.selected.unwrap_or_else(|| {
        target
            .iter()
            .map(|spec| ServiceAttempt {
                name: spec.name.clone(),
            })
            .collect()
    });
    Ok(DeployIntent::new(
        input.project_name,
        target,
        PlanOptions {
            force_recreate: false,
            skip_health_monitor: false,
            placement_seed: 0,
            selected,
        },
    )
    .with_dependencies(dependencies))
}

/// A deployed Service waits for every deployed Service its variables reference, except that
/// edges inside a reference cycle are dropped. An HTTP healthcheck makes the wait for health.
fn deployment_dependencies(
    snapshots: &[(ServiceConfig, LowerDeploymentSnapshot)],
    lineages: &BTreeMap<String, String>,
) -> BTreeMap<ServiceName, Vec<ServiceDependency>> {
    let deployed = || {
        snapshots
            .iter()
            .filter(|(config, _)| !matches!(config.settings.source, ServiceSource::Empty { .. }))
    };
    let by_id: BTreeMap<&str, &ServiceConfig> = deployed()
        .filter_map(|(config, snapshot)| Some((snapshot.service_id.as_deref()?, config)))
        .collect();
    let mut graph: BTreeMap<&ServiceName, (&ServiceConfig, BTreeSet<&ServiceName>)> =
        BTreeMap::new();
    for (config, _) in deployed() {
        let name = &config.settings.private_dns;
        let references = config
            .env
            .values()
            .filter_map(|value| match value {
                ServiceEnvValue::Literal { parts, .. } => parts.as_ref(),
                ServiceEnvValue::Secret { .. } => None,
            })
            .flatten()
            .filter_map(|part| match part {
                ValuePart::Ref {
                    owner: ValuePartOwner::Service { lineage_id },
                    ..
                } => by_id.get(lineages.get(lineage_id)?.as_str()),
                ValuePart::Ref {
                    owner: ValuePartOwner::Self_,
                    ..
                }
                | ValuePart::Text { .. } => None,
            })
            .map(|dependency| &dependency.settings.private_dns)
            .filter(|dependency| *dependency != name)
            .collect();
        graph.insert(name, (config, references));
    }
    // ponytail: per-edge reachability keeps this small; use SCCs if large environments make it costly.
    let reaches = |from: &ServiceName, target: &ServiceName| {
        let mut pending = vec![from];
        let mut visited = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if name == target {
                return true;
            }
            if visited.insert(name) {
                pending.extend(graph.get(name).into_iter().flat_map(|(_, next)| next));
            }
        }
        false
    };
    graph
        .iter()
        .filter_map(|(name, (_, references))| {
            let edges: Vec<_> = references
                .iter()
                .filter(|dependency| !reaches(dependency, name))
                .map(|dependency| ServiceDependency {
                    service: (*dependency).clone(),
                    // Normal startup already monitors Docker health. An explicit HTTP check
                    // also gates unchanged dependencies.
                    condition: match graph
                        .get(dependency)
                        .map(|(config, _)| &config.settings.healthcheck)
                    {
                        Some(ServiceHealthcheck::Http { .. }) => {
                            DependencyCondition::ServiceHealthy
                        }
                        _ => DependencyCondition::ServiceStarted,
                    },
                })
                .collect();
            (!edges.is_empty()).then(|| ((*name).clone(), edges))
        })
        .collect()
}

fn lowering_error(_: impl std::fmt::Display) -> ConfigError {
    ConfigError::at(
        "service",
        "Authored service could not be lowered to a runtime specification",
    )
}
