//! Freezing what a Deployment ships and lowering it to a Deploy Intent.

use super::*;

/// Freeze a Deployment of `saved`: its target nodes, checked to lower to a Deploy
/// Intent. `services` narrows it to those Services and the Volumes and Configs they
/// mount; none targets every Service, Volume and Config, including the removal of
/// those Applied State holds and `saved` does not, which deletes the Docker Volumes
/// `losses` names. Generated domains expand under `cluster_domain`;
/// a plan, which has none, checks the rest.
pub(crate) fn freeze(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    services: &[ServiceName],
    namespace: Namespace,
    cluster_domain: Option<&Hostname>,
    losses: &[VolumeLoss],
) -> Result<Frozen, RpcError> {
    let mut nodes: Vec<TargetNode> = if services.is_empty() {
        saved
            .services
            .iter()
            .chain(
                applied
                    .services
                    .iter()
                    .filter(|old| !saved.services.iter().any(|new| new.id == old.id)),
            )
            .map(TargetNode::service)
            .collect::<Result<_, _>>()?
    } else {
        services
            .iter()
            .map(|name| {
                saved
                    .services
                    .iter()
                    .find(|service| service.slug == name.as_str())
                    .map(TargetNode::service)
                    .transpose()?
                    .ok_or_else(|| {
                        error::not_found(
                            format!("No Service named {name} to deploy"),
                            json!({ "services": saved.services.iter().map(|service| &service.slug).collect::<Vec<_>>() }),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    // A full Deploy applies every Volume; a narrowed one those its Services mount.
    let mounted = |volume: &SavedVolumeIntent| {
        saved.services.iter().any(|service| {
            nodes.iter().any(|node| node.id() == service.id)
                && service
                    .volume_attachments
                    .iter()
                    .any(|mount| mount.volume_resource_id == volume.resource_id)
        })
    };
    let kept: Vec<TargetNode> = saved
        .volumes
        .iter()
        .filter(|volume| services.is_empty() || mounted(volume))
        .map(|volume| TargetNode::volume(volume, None))
        .collect::<Result<_, _>>()?;
    nodes.extend(kept);
    // A narrowed Deploy applies a Config its Services mount once no untargeted Service
    // would keep running an older one. A full Deploy also removes those only Applied holds.
    let applies = |config: &SavedConfigIntent| {
        let targeted: Vec<bool> = saved
            .services
            .iter()
            .filter(|service| {
                service
                    .config_attachments
                    .iter()
                    .any(|mount| mount.config_resource_id == config.resource_id)
            })
            .map(|service| nodes.iter().any(|node| node.id() == service.id))
            .collect();
        let fresh = !applied
            .configs
            .iter()
            .any(|old| old.resource_id == config.resource_id);
        targeted.contains(&true) && (fresh || !targeted.contains(&false))
    };
    let configs: Vec<TargetNode> = saved
        .configs
        .iter()
        .filter(|config| services.is_empty() || applies(config))
        .chain(applied.configs.iter().filter(|old| {
            services.is_empty()
                && !saved
                    .configs
                    .iter()
                    .any(|new| new.resource_id == old.resource_id)
        }))
        .map(TargetNode::config)
        .collect::<Result<_, _>>()?;
    nodes.extend(configs);
    if services.is_empty() {
        for loss in losses {
            let volume = applied
                .volumes
                .iter()
                .find(|volume| volume.resource_id == loss.volume.id.as_str())
                .ok_or_else(|| error::corrupt("Applied State"))?;
            nodes.push(TargetNode::volume(volume, Some(loss.deletes.clone()))?);
        }
    }
    // Live values and Setup Commands resolve at claim; checking without them is the same.
    lower(
        environment,
        saved,
        services,
        (namespace.clone(), cluster_domain),
        &crate::branch::Lowering::default(),
        None,
    )?;
    let sourceless = saved
        .services
        .iter()
        .filter(|service| {
            matches!(service.config.source, ServiceSource::Empty { .. })
                && nodes.iter().any(|node| node.id() == service.id)
        })
        .map(|service| service.slug.clone())
        .collect();
    Ok(Frozen {
        nodes,
        namespace,
        credentials: BTreeMap::new(),
        cluster_domain: cluster_domain.cloned(),
        sourceless,
    })
}

/// A Deployment lowered at claim.
pub(super) struct Lowered {
    /// The lowering input the runner lowers again once it built.
    pub(super) input: Value,
    pub(super) intent: DeployIntent,
    /// Config file references whose resolved value may break the file. Only
    /// lowering with the sealing key sees every value, so only it warns.
    pub(super) warnings: Vec<DeploymentWarning>,
}

/// Lower Saved revision `saved` to the Deploy Intent of a Deployment of `services`
/// (none: every Service) into its Namespace, with generated domains under the
/// Cluster Domain. Variables and Config files resolve here, a Branch's Live Node
/// references against `branch.live`; secrets, and values that reference one, only
/// with `unseal`. Without it such a variable is left out and such a file is empty.
pub(super) fn lower(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    services: &[ServiceName],
    (namespace, cluster_domain): (Namespace, Option<&Hostname>),
    branch: &crate::branch::Lowering,
    unseal: Option<&SealingKey>,
) -> Result<Lowered, RpcError> {
    let mut compiled = compile_environment_intent(
        environment.as_str(),
        crate::domain::expand(saved, cluster_domain),
    );
    // Live values order nothing: what provides them runs elsewhere.
    let lineages: BTreeMap<String, String> = compiled
        .variable_producers
        .iter()
        .map(|producer| (producer.owner_lineage_id.clone(), producer.owner_id.clone()))
        .collect();
    compiled
        .variable_producers
        .extend(branch.live.iter().cloned());
    let mut resolved = variables::resolve(&compiled, unseal)?;
    let targeted: Vec<&str> = saved
        .services
        .iter()
        .filter(|service| services.iter().any(|name| name.as_str() == service.slug))
        .map(|service| service.id.as_str())
        .collect();
    let snapshots: Vec<(ServiceConfig, LowerDeploymentSnapshot)> = compiled
        .node_snapshots
        .into_iter()
        .filter_map(|node| match node.snapshot.0 {
            // A targeted Deployment builds only the Git Services it targets.
            CompiledNodeConfig::Service(config)
                if !services.is_empty()
                    && matches!(config.settings.source, ServiceSource::Git { .. })
                    && !targeted.contains(&node.node_id.as_str()) =>
            {
                None
            }
            CompiledNodeConfig::Service(config) => Some((
                *config,
                LowerDeploymentSnapshot {
                    resolved_env: resolved.env.remove(&node.node_id).unwrap_or_default(),
                    setup_commands: branch.setup.get(&node.node_id).cloned().unwrap_or_default(),
                    service_id: Some(node.node_id),
                    config: Value::Null,
                    replicas: None,
                },
            )),
            CompiledNodeConfig::Volume(_) | CompiledNodeConfig::Config(_) => None,
        })
        .collect();
    // Empty reconciles the whole Namespace; names deploy only those Services.
    let selected = saved
        .services
        .iter()
        .filter(|service| services.iter().any(|name| name.as_str() == service.slug))
        .map(|service| ServiceAttempt {
            name: service.config.private_dns.clone(),
        })
        .collect::<Vec<_>>();
    let slugs: BTreeMap<&str, &str> = saved
        .services
        .iter()
        .map(|service| (service.lineage_id.as_str(), service.slug.as_str()))
        .collect();
    let mut warnings = Vec::new();
    let mut configs = Vec::new();
    // Only Configs a lowered Service mounts ship, and only those it deploys warn.
    for config in &saved.configs {
        let mounting: Vec<&str> = saved
            .services
            .iter()
            .filter(|service| {
                service
                    .config_attachments
                    .iter()
                    .any(|mount| mount.config_resource_id == config.resource_id)
            })
            .map(|service| service.id.as_str())
            .collect();
        if !snapshots.iter().any(|(_, snapshot)| {
            snapshot
                .service_id
                .as_deref()
                .is_some_and(|id| mounting.contains(&id))
        }) {
            continue;
        }
        let warns = unseal.is_some()
            && mounting
                .iter()
                .any(|id| services.is_empty() || targeted.contains(id));
        let mut files = resolved
            .configs
            .remove(&config.resource_id)
            .unwrap_or_default();
        let mut lowered = BTreeMap::new();
        for (name, file) in &config.files {
            let resolved = files.remove(name).ok_or_else(|| error::corrupt("Config"))?;
            if warns {
                warnings.extend(resolved.fragile.iter().map(|reference| DeploymentWarning {
                    config: config.name.clone(),
                    file: name.clone(),
                    variable: match &reference.owner {
                        ValuePartOwner::Service { lineage_id } => {
                            match slugs.get(lineage_id.as_str()) {
                                Some(slug) => format!("{slug}.{}", reference.key),
                                None => reference.key.clone(),
                            }
                        }
                        ValuePartOwner::Self_ => reference.key.clone(),
                    },
                }));
            }
            lowered.insert(
                name.clone(),
                LowerDeploymentConfigFile {
                    content: resolved.content,
                    mode: file.mode,
                    uid: file.uid,
                    gid: file.gid,
                },
            );
        }
        configs.push(LowerDeploymentConfig {
            config_resource_id: config.resource_id.clone(),
            name: config.name.clone(),
            references: config
                .files
                .values()
                .flat_map(|file| file.referenced_lineages())
                .map(str::to_owned)
                .collect(),
            files: lowered,
        });
    }
    let input = |snapshots: Vec<LowerDeploymentSnapshot>| LowerDeploymentInput {
        namespace: namespace.clone(),
        snapshots,
        volumes: saved
            .volumes
            .iter()
            .map(|volume| LowerDeploymentVolume {
                volume_resource_id: volume.resource_id.clone(),
                storage: volume.storage,
            })
            .collect(),
        lineages: lineages.clone(),
        selected: Some(selected.clone()),
        configs: configs.clone(),
    };
    let with = |built_later: bool| {
        snapshots
            .iter()
            .map(|(config, snapshot)| LowerDeploymentSnapshot {
                config: json_value(&if built_later {
                    built(config)
                } else {
                    config.clone()
                }),
                ..snapshot.clone()
            })
            .collect()
    };
    let intent = lower_deployment(input(with(true))).map_err(|error| {
        error::invalid(
            format!("This Environment can't deploy: {}", error.message),
            json!({ "path": error.path }),
        )
    })?;
    Ok(Lowered {
        input: json_value(&input(with(false))),
        intent,
        warnings,
    })
}

/// `config` with a Git source replaced by the image its build will produce, as the
/// SDK's preparation does once it built it.
pub(super) fn built(config: &ServiceConfig) -> ServiceConfig {
    let mut config = config.clone();
    if matches!(config.settings.source, ServiceSource::Git { .. }) {
        config.settings.source = ServiceSource::Image {
            version: 1,
            image: format!("ployz-build/{}:pending", config.settings.private_dns),
            credentials: ServiceImageCredentials::None,
        };
    }
    config
}

pub(super) fn json_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("lowering input is JSON")
}
