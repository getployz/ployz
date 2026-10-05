//! Freezing what a Deployment ships and lowering it to a Deploy Intent.

use super::*;

/// Freeze a Deployment of `saved`: its target nodes, checked to lower to a Deploy
/// Intent. `services` narrows it; none targets every Service and Volume, including
/// the removal of those Applied State holds and `saved` does not, which deletes the
/// Docker Volumes `losses` names. Generated domains expand under `cluster_domain`;
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

/// Lower Saved revision `saved` to the Deploy Intent of a Deployment of `services`
/// (none: every Service) into its Namespace, with generated domains under the
/// Cluster Domain. Variables resolve here, a Branch's Live Node references against
/// `branch.live`; secrets, and values that reference one, only with `unseal`, and
/// are left out without it.
pub(super) fn lower(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    services: &[ServiceName],
    (namespace, cluster_domain): (Namespace, Option<&Hostname>),
    branch: &crate::branch::Lowering,
    unseal: Option<&SealingKey>,
) -> Result<(Value, DeployIntent), RpcError> {
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
                    resolved_env: resolved.remove(&node.node_id).unwrap_or_default(),
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
    Ok((json_value(&input(with(false))), intent))
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
