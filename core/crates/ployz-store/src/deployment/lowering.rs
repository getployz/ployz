//! Freezing what a Deployment ships and lowering it to a Deploy Intent.

use super::*;

/// Which nodes a Deployment ships.
pub(crate) enum Scope<'a> {
    /// Every Service, Volume and Config, including the removal of those Applied
    /// State holds and Saved does not, which deletes the Docker Volumes `losses` names.
    All { losses: &'a [VolumeLoss] },
    /// `services` and the Volumes and Configs they mount, and the teardown of the
    /// Services `retire` names. It can't remove a Volume.
    Narrow {
        services: &'a [ServiceName],
        retire: &'a [Retire],
    },
}

impl<'a> Scope<'a> {
    /// `services`, or every node with `losses` when it names none.
    pub(crate) fn of(services: &'a [ServiceName], losses: &'a [VolumeLoss]) -> Self {
        if services.is_empty() {
            Self::All { losses }
        } else {
            Self::Narrow {
                services,
                retire: &[],
            }
        }
    }
}

/// A Service a narrowed Deployment tears down: one Saved no longer holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Retire {
    pub(crate) id: ServiceLineageId,
    pub(crate) name: ServiceName,
    /// Every runtime Service it may still run as.
    pub(crate) runtimes: ServiceRemovals,
}

/// What tearing down the Services `removed` takes once Saved revision `saved`
/// dropped them: each by the runtime Services Applied State and every started or
/// queued Deployment since the last one that removed it runs it as. One still in
/// `saved`, or that never ran nor will, has nothing to tear down, and a runtime
/// Service another kept Service runs is never one.
pub(crate) fn retirements(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    removed: &BTreeSet<ServiceLineageId>,
) -> Result<Vec<Retire>, RpcError> {
    let applied = applied_state(tx, environment, saved)?;
    let gone = |id: &str| removed.iter().any(|removed| removed.as_str() == id);
    let kept: BTreeSet<&ServiceName> = saved
        .services
        .iter()
        .chain(applied.services.iter().filter(|service| !gone(&service.id)))
        .map(|service| &service.config.private_dns)
        .collect();
    let mut retire = Vec::new();
    for id in removed {
        if saved
            .services
            .iter()
            .any(|service| service.id == id.as_str())
        {
            continue;
        }
        let mut name = None;
        let mut runtimes = ServiceRemovals::default();
        if let Some(service) = applied
            .services
            .iter()
            .find(|service| service.id == id.as_str())
        {
            name = Some(
                ServiceName::parse(service.slug.as_str())
                    .map_err(|_| error::corrupt("Service name"))?,
            );
            runtimes.extend([service.config.private_dns.clone()]);
        }
        let rows = tx.query(
            "SELECT nodes, run FROM config_deployment \
             WHERE environment_id = ?1 AND (started IS NOT NULL OR status = 'queued') \
             AND nodes LIKE ?2 \
             ORDER BY number DESC",
            &[
                environment.as_str().into(),
                format!("%\"{id}\"%").as_str().into(),
            ],
        )?;
        for row in &rows {
            let run: Run = row.json(1, "Deployment")?;
            // What ran it before a Deployment removed it is gone.
            if run.nodes.get(id.as_str()) == Some(&NodeStatus::Removed) {
                break;
            }
            let nodes: Vec<TargetNode> = row.json(0, "Deployment")?;
            for node in nodes.iter().filter(|node| node.id() == id.as_str()) {
                if let TargetNode::Service { name: then, .. } = node {
                    name.get_or_insert_with(|| then.clone());
                    runtimes.extend(node.runtimes().into_iter().cloned());
                }
            }
        }
        let runtimes = ServiceRemovals::from_names(
            runtimes.iter().filter(|name| !kept.contains(name)).cloned(),
        );
        if let (Some(name), false) = (name, runtimes.is_empty()) {
            retire.push(Retire {
                id: id.clone(),
                name,
                runtimes,
            });
        }
    }
    Ok(retire)
}

/// Freeze a Deployment of `saved` over `scope`: its target nodes, checked to lower
/// to a Deploy Intent. Generated domains expand under `cluster_domain`;
/// a plan, which has none, checks the rest.
pub(crate) fn freeze(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    scope: Scope<'_>,
    namespace: Namespace,
    cluster_domain: Option<&Hostname>,
) -> Result<Frozen, RpcError> {
    let (full, services, retire, losses): (bool, &[ServiceName], &[Retire], &[VolumeLoss]) =
        match scope {
            Scope::All { losses } => (true, &[], &[], losses),
            Scope::Narrow { services, retire } => (false, services, retire, &[]),
        };
    if !full && services.is_empty() && retire.is_empty() {
        return Err(error::invalid(
            "A targeted Deployment names no Service",
            json!({}),
        ));
    }
    let mut removals = ServiceRemovals::default();
    for (at, gone) in retire.iter().enumerate() {
        let running = |name: &ServiceName| {
            saved
                .services
                .iter()
                .any(|service| service.config.private_dns == *name)
        };
        if saved
            .services
            .iter()
            .any(|service| service.id == gone.id.as_str())
            || retire.iter().take(at).any(|earlier| earlier.id == gone.id)
            || gone.runtimes.iter().any(running)
        {
            return Err(error::invalid(
                format!("Service {} can't be removed by this Deployment", gone.name),
                json!({ "service": gone.id }),
            ));
        }
        removals.extend(gone.runtimes.iter().cloned());
    }
    let mut nodes: Vec<TargetNode> = if full {
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
            .chain(retire.iter().map(TargetNode::retire))
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
        .filter(|volume| full || mounted(volume))
        .map(|volume| TargetNode::volume(volume, None))
        .collect::<Result<_, _>>()?;
    nodes.extend(kept);
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
        .filter(|config| full || applies(config))
        .chain(applied.configs.iter().filter(|old| {
            full && !saved
                .configs
                .iter()
                .any(|new| new.resource_id == old.resource_id)
        }))
        .map(TargetNode::config)
        .collect::<Result<_, _>>()?;
    nodes.extend(configs);
    if !full {
        let unmounted = unmounted(saved, applied, &nodes)?;
        nodes.extend(unmounted);
    }
    for loss in losses {
        let volume = applied
            .volumes
            .iter()
            .find(|volume| volume.resource_id == loss.volume.id.as_str())
            .ok_or_else(|| error::corrupt("Applied State"))?;
        nodes.push(TargetNode::volume(volume, Some(loss.deletes.clone()))?);
    }
    // Live values and Setup Commands resolve at claim; checking without them is the same.
    lower(
        environment,
        saved,
        (services, &removals),
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

/// Each Config Applied State `applied` holds and `saved` lacks that `nodes` don't
/// ship yet but leave with: a removed Config goes with the Services that mounted
/// it, once `nodes` ship every one.
pub(super) fn unmounted(
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    nodes: &[TargetNode],
) -> Result<Vec<TargetNode>, RpcError> {
    let ships = |id: &str| nodes.iter().any(|node| node.id() == id);
    applied
        .configs
        .iter()
        .filter(|old| {
            let targeted: Vec<bool> = applied
                .services
                .iter()
                .filter(|service| {
                    service
                        .config_attachments
                        .iter()
                        .any(|mount| mount.config_resource_id == old.resource_id)
                })
                .map(|service| ships(&service.id))
                .collect();
            !saved
                .configs
                .iter()
                .any(|new| new.resource_id == old.resource_id)
                && !ships(&old.resource_id)
                && targeted.contains(&true)
                && !targeted.contains(&false)
        })
        .map(TargetNode::config)
        .collect()
}

/// A Deployment lowered at claim.
pub(super) struct Lowered {
    /// The lowering input the runner lowers again once it built.
    pub(super) input: Value,
    pub(super) intent: DeployIntent,
    /// The environment keys of each lowered Service that hold a secret, by runtime name.
    pub(super) secret_env: BTreeMap<ServiceName, BTreeSet<String>>,
    /// Service variables a secret without a value left out, then Config file
    /// references whose resolved value may break the file. Only lowering with the
    /// sealing key sees every value, so only it warns of those.
    pub(super) warnings: Vec<DeploymentWarning>,
}

/// Lower Saved revision `saved` to the Deploy Intent of a Deployment of `services`
/// that tears down the runtime Services `removals` names (neither: every Service)
/// into its Namespace, with generated domains under the
/// Cluster Domain. Variables and Config files resolve here, a Branch's Live Node
/// references against `branch.live`; secrets, and values that reference one, only
/// with `unseal`. Without it such a variable is left out and such a file is empty.
pub(super) fn lower(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    (services, removals): (&[ServiceName], &ServiceRemovals),
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
    let narrowed = !services.is_empty() || !removals.is_empty();
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
                if narrowed
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
                    built_with: None,
                },
            )),
            CompiledNodeConfig::Volume(_) | CompiledNodeConfig::Config(_) => None,
        })
        .collect();
    let secret_env = snapshots
        .iter()
        .filter_map(|(config, snapshot)| {
            let keys = resolved.secret.remove(snapshot.service_id.as_deref()?)?;
            Some((config.settings.private_dns.clone(), keys))
        })
        .collect();
    // Empty reconciles the whole Namespace; names deploy only those Services.
    let selected = saved
        .services
        .iter()
        .filter(|service| services.iter().any(|name| name.as_str() == service.slug))
        .map(|service| service.config.private_dns.clone())
        .chain(removals.iter().cloned())
        .map(|name| ServiceAttempt { name })
        .collect::<Vec<_>>();
    let slugs: BTreeMap<&str, &str> = saved
        .services
        .iter()
        .map(|service| (service.lineage_id.as_str(), service.slug.as_str()))
        .collect();
    // Only the Services this Deploy ships; the rest ride along unchanged.
    let mut warnings: Vec<DeploymentWarning> = snapshots
        .iter()
        .filter_map(|(config, snapshot)| {
            let id = snapshot.service_id.as_deref()?;
            if narrowed && !targeted.contains(&id) {
                return None;
            }
            let keys = resolved.not_set.remove(id)?;
            Some(keys.into_iter().map(|variable| DeploymentWarning::NotSet {
                service: config.settings.private_dns.clone(),
                variable,
            }))
        })
        .flatten()
        .collect();
    let mut configs = Vec::new();
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
        let warns =
            unseal.is_some() && mounting.iter().any(|id| !narrowed || targeted.contains(id));
        let mut files = resolved
            .configs
            .remove(&config.resource_id)
            .unwrap_or_default();
        let mut lowered = BTreeMap::new();
        for (name, file) in &config.files {
            let resolved = files.remove(name).ok_or_else(|| error::corrupt("Config"))?;
            if warns {
                let shown =
                    |reference: &ployz_core::config::FragileReference| match &reference.owner {
                        ValuePartOwner::Service { lineage_id } => {
                            match slugs.get(lineage_id.as_str()) {
                                Some(slug) => format!("{slug}.{}", reference.key),
                                None => reference.key.clone(),
                            }
                        }
                        ValuePartOwner::Self_ => reference.key.clone(),
                    };
                warnings.extend(resolved.fragile.iter().map(|reference| {
                    DeploymentWarning::Fragile {
                        config: config.name.clone(),
                        file: name.clone(),
                        variable: shown(reference),
                    }
                }));
                warnings.extend(resolved.not_set.iter().map(|reference| {
                    DeploymentWarning::NotSetInFile {
                        config: config.name.clone(),
                        file: name.clone(),
                        secret: shown(reference),
                    }
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
        removals: removals.clone(),
    };
    let with = |built_later: bool| {
        snapshots
            .iter()
            .map(|(config, snapshot)| {
                let build =
                    built_later && matches!(config.settings.source, ServiceSource::Git { .. });
                LowerDeploymentSnapshot {
                    config: json_value(&if build { built(config) } else { config.clone() }),
                    built_with: build.then_some(config.settings.build.build_method),
                    ..snapshot.clone()
                }
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
        secret_env,
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
