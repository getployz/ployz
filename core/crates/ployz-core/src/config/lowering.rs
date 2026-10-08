//! Lower resolved Cloud service settings to typed runtime deployment requests.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    ConfigError, ServiceConfig, ServiceEnvValue, ServiceHealthcheck, ServiceSource, ValuePart,
    ValuePartOwner, VolumeKind, parse_service_config,
};
use crate::{
    ByteQuantity, ConfiguredHealthcheck, ContainerResources, CpuNanos, DependencyCondition,
    DeployIntent, HealthcheckCommand, HealthcheckSpec, HttpHealthcheck, HttpProtocol, IngressHost,
    Namespace, PlanOptions, PortPublication, PreDeployCommand, PreDeployHook, PullPolicy,
    RawVolumeSource, RequestedServiceSpec, RestartPolicy, ServiceAttempt, ServiceContainerSpec,
    ServiceDependency, ServiceMode, ServiceMount, ServiceName, ServiceVolume, ServiceVolumeGraph,
    VolumeDriver,
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

/// Captured node settings plus adapter-resolved runtime inputs for one Namespace.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LowerDeploymentInput {
    /// The Namespace every lowered Service runs in.
    pub namespace: Namespace,
    /// Each Service's captured settings and resolved inputs.
    pub snapshots: Vec<LowerDeploymentSnapshot>,
    /// The Volumes the Namespace holds; mounts of any other are left out.
    #[serde(default)]
    pub volumes: Vec<LowerDeploymentVolume>,
    /// Service ID by lineage, from the attempt's frozen variable producers. References
    /// resolved through it order the deploy.
    #[serde(default)]
    pub lineages: BTreeMap<String, String>,
    /// Omitted preserves partial-deploy behavior; empty reconciles the complete target.
    pub selected: Option<Vec<ServiceAttempt>>,
}

/// One Service's captured settings and the runtime inputs resolved for it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LowerDeploymentSnapshot {
    /// The authored Service's ID, when it has one: references to it resolve by it.
    pub service_id: Option<String>,
    /// Its authored Service settings, as captured.
    pub config: Value,
    /// Replicas overriding the authored count, such as a PR Environment's one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u8>,
    /// Its variables with every reference resolved, by key.
    #[serde(default)]
    pub resolved_env: BTreeMap<String, String>,
    /// Run in order after the service's own pre-deploy command, in the same hook.
    #[serde(default)]
    pub setup_commands: Vec<String>,
}

/// One Volume the Namespace holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LowerDeploymentVolume {
    /// The authored Volume's ID, as mounts name it.
    pub volume_resource_id: String,
    /// Its fixed storage.
    pub storage: VolumeKind,
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
    let volume_sources: BTreeMap<_, _> = input
        .volumes
        .iter()
        .map(|v| (v.volume_resource_id.as_str(), v.storage))
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
            ServiceHealthcheck::None => Some(HealthcheckSpec::Disabled),
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
            ServiceHealthcheck::Command {
                command,
                timeout_seconds,
            } => Some(command_healthcheck(command, *timeout_seconds)),
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
        let mut volumes = Vec::new();
        let mut mounts = Vec::new();
        for mount in configured_mounts {
            let Some(storage) = volume_sources.get(mount.volume_resource_id.as_str()) else {
                continue;
            };
            let name = format!("vol-{}", mount.volume_resource_id);
            let reference: crate::ServiceVolumeReference =
                name.clone().try_into().map_err(lowering_error)?;
            let name = name.try_into().map_err(lowering_error)?;
            let source = match storage {
                VolumeKind::Docker {} => RawVolumeSource::Ordinary {
                    name,
                    driver: VolumeDriver::parse("local", BTreeMap::new())
                        .map_err(lowering_error)?,
                    labels: BTreeMap::new(),
                },
                VolumeKind::Provisioned { maximum_bytes } => RawVolumeSource::Provisioned {
                    name,
                    maximum_bytes: *maximum_bytes,
                    labels: BTreeMap::new(),
                },
            };
            volumes.push(ServiceVolume {
                reference: reference.clone(),
                source: source.try_into().map_err(lowering_error)?,
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
            .map_err(|error| match error {
                crate::ServiceSpecGraphError::RootMountTarget => {
                    ConfigError::at("mounts", "A volume cannot mount at the container root /")
                }
                crate::ServiceSpecGraphError::DuplicateMountTarget { .. } => ConfigError::at(
                    "mounts",
                    "Two volume mounts resolve to the same container path",
                ),
                error @ (crate::ServiceSpecGraphError::Volume(_)
                | crate::ServiceSpecGraphError::Config(_)) => lowering_error(error),
            })?;
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
        input.namespace,
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

fn command_healthcheck(command: &str, timeout_seconds: u16) -> HealthcheckSpec {
    let timeout = u64::from(timeout_seconds) * 1_000;
    HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(["CMD-SHELL", command])
            .expect("a CMD-SHELL test never begins with NONE"),
        interval_millis: Some(10_000),
        timeout_millis: Some(5_000),
        start_period_millis: Some(timeout),
        start_interval_millis: Some(1_000),
        retries: Some(3),
        deadline_millis: Some(timeout),
    })
}

/// A deployed Service waits for every deployed Service its variables reference, except that
/// edges inside a reference cycle are dropped. An HTTP or command healthcheck makes the wait for
/// health.
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
                    // Normal startup already monitors Docker health. An authored check also
                    // gates unchanged dependencies.
                    condition: match graph
                        .get(dependency)
                        .map(|(config, _)| &config.settings.healthcheck)
                    {
                        Some(
                            ServiceHealthcheck::Http { .. } | ServiceHealthcheck::Command { .. },
                        ) => DependencyCondition::ServiceHealthy,
                        Some(ServiceHealthcheck::None) | None => {
                            DependencyCondition::ServiceStarted
                        }
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
