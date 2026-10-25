//! Freezing what a Deployment ships and lowering it to a Deploy Intent.

use super::*;

/// Which nodes a Deployment ships.
pub(crate) enum Scope<'a> {
    /// Every Service, Volume and Config, including the removal of those Applied
    /// State holds and Saved does not, which deletes the Docker Volumes `losses`
    /// names. It tears down what `retire` names, from [`dropped`]: each Service
    /// Saved dropped, and beside each it ships, the runtime Services that one no
    /// longer runs as. With `prune` it tears down every other Service running in
    /// the Namespace too.
    All {
        losses: &'a [VolumeLoss],
        retire: &'a [Retire],
        prune: bool,
    },
    /// `services` and the Volumes and Configs they mount, and the teardown of the
    /// Services `retire` names. With `every_saved`, it ships every Saved change:
    /// each Service, Volume and Config its [`Landing`] doesn't hold settled, and the
    /// removal of each Config Saved drops. It removes a Volume of `losses` once it
    /// ships every Service that may run mounting it, at once when none does.
    Narrow {
        services: &'a [ServiceName],
        retire: &'a [Retire],
        losses: &'a [VolumeLoss],
        every_saved: bool,
    },
}

impl<'a> Scope<'a> {
    /// `services`, or every node with `losses` when it names none. For a plan: it
    /// tears down nothing beyond what Saved dropped.
    pub(crate) fn of(services: &'a [ServiceName], losses: &'a [VolumeLoss]) -> Self {
        if services.is_empty() {
            Self::All {
                losses,
                retire: &[],
                prune: false,
            }
        } else {
            Self::Narrow {
                services,
                retire: &[],
                losses: &[],
                every_saved: false,
            }
        }
    }
}

/// A Service whose runtime Services a Deployment tears down: one Saved no longer
/// holds, by every runtime it may still run as; or, on a full Deployment, one
/// Saved still holds, by those it ran as and no longer does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Retire {
    pub(crate) id: ServiceLineageId,
    pub(crate) name: ServiceName,
    pub(crate) runtimes: ServiceRemovals,
}

/// What tearing down the Services `removed` takes once Saved revision `saved`
/// dropped them: each by the runtime Services it may run as, by [`ran_as`]. One
/// still in `saved`, or that never ran nor will, has nothing to tear down, and a
/// runtime Service another kept Service runs is never one.
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
        let (name, runtimes) = ran_as(tx, environment, &applied, id)?;
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

/// The name of Service lineage `id` and every runtime Service it may run as: by
/// `applied`, and by each started or queued Deployment since the last one that
/// removed it, less each runtime an Applied Deployment whose snapshot was complete
/// since tore down beside it.
fn ran_as(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    applied: &SavedEnvironmentIntent,
    id: &ServiceLineageId,
) -> Result<(Option<ServiceName>, ServiceRemovals), RpcError> {
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
        "SELECT nodes, run, status FROM config_deployment \
         WHERE environment_id = ?1 AND number IS NOT NULL \
         AND (started IS NOT NULL OR status = 'queued') \
         AND nodes LIKE ?2 \
         ORDER BY number DESC",
        &[
            environment.as_str().into(),
            format!("%\"{id}\"%").as_str().into(),
        ],
    )?;
    let mut torn_down = BTreeSet::new();
    for row in &rows {
        let run: Run = row.json(1, "Deployment")?;
        // What ran it before a Deployment removed it is gone.
        if run.nodes.get(id.as_str()) == Some(&NodeStatus::Removed) {
            break;
        }
        let nodes: Vec<TargetNode> = row.json(0, "Deployment")?;
        // Applied means every operation the preview planned completed, so the
        // Deployment tore down what it shipped beside on every Machine it reached.
        // A copy on a Machine it could not check is the next full Deploy's prune,
        // not a removal to name again. What any other Deployment shipped or tried
        // to tear down may run still.
        let torn = row.text(2)? == "applied";
        for node in nodes.iter().filter(|node| node.id() == id.as_str()) {
            if let TargetNode::Service {
                name: then,
                removals,
                ..
            } = node
            {
                name.get_or_insert_with(|| then.clone());
                if torn {
                    torn_down.extend(removals.iter().cloned());
                }
                runtimes.extend(
                    node.runtimes()
                        .into_iter()
                        .filter(|runtime| !torn_down.contains(*runtime))
                        .cloned(),
                );
            }
        }
    }
    Ok((name, runtimes))
}

/// What a full Deployment of `saved` tears down beside each Service it ships: the
/// runtime Services the Service ran as, by [`ran_as`], and no Service of `saved`
/// runs as now.
fn renamed(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
) -> Result<Vec<Retire>, RpcError> {
    let applied = applied_state(tx, environment, saved)?;
    let runs: BTreeSet<&ServiceName> = saved
        .services
        .iter()
        .map(|service| &service.config.private_dns)
        .collect();
    let mut retire = Vec::new();
    for service in &saved.services {
        let id = parse_stored(&service.id)?;
        let (_, runtimes) = ran_as(tx, environment, &applied, &id)?;
        let runtimes = ServiceRemovals::from_names(
            runtimes.iter().filter(|name| !runs.contains(name)).cloned(),
        );
        if !runtimes.is_empty() {
            retire.push(Retire {
                id,
                name: ServiceName::parse(service.slug.as_str())
                    .map_err(|_| error::corrupt("Service name"))?,
                runtimes,
            });
        }
    }
    Ok(retire)
}

/// What a full Deployment of `saved` tears down: each Service `landing` may run
/// and `saved` dropped, and each a Deployment that started and ended short of
/// Applied put in place and `saved` lacks, by [`retirements`]; and beside each
/// Service it ships, the runtime Services that one no longer runs as, by
/// [`renamed`].
pub(crate) fn dropped(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    landing: &Landing<'_>,
) -> Result<Vec<Retire>, RpcError> {
    let mut removed = landing
        .dropped(saved)
        .services
        .iter()
        .map(|service| parse_stored(&service.id))
        .collect::<Result<BTreeSet<ServiceLineageId>, _>>()?;
    let in_flight = in_flight_sql();
    let rows = tx.query(
        &format!(
            "SELECT nodes FROM config_deployment \
             WHERE environment_id = ?1 AND {NUMBERED} AND started IS NOT NULL \
             AND status != 'applied' AND NOT {in_flight}"
        ),
        &[environment.as_str().into()],
    )?;
    for row in &rows {
        let nodes: Vec<TargetNode> = row.json(0, "Deployment")?;
        for node in nodes {
            if let TargetNode::Service { id, .. } = node
                && !saved
                    .services
                    .iter()
                    .any(|service| service.id == id.as_str())
            {
                removed.insert(id);
            }
        }
    }
    let mut retire = retirements(tx, environment, saved, &removed)?;
    retire.extend(renamed(tx, environment, saved)?);
    Ok(retire)
}

/// What a Deployment lowers: the Saved Services it ships and the runtime
/// Services it tears down.
pub(crate) enum Ships {
    /// Every Saved Service, tearing down `removals`; with `prune`, every other
    /// Service running in the Namespace too.
    All {
        removals: ServiceRemovals,
        prune: bool,
    },
    /// `services`, tearing down `removals`.
    Named {
        services: Vec<ServiceName>,
        removals: ServiceRemovals,
    },
}

/// Freeze a Deployment of `saved` over `scope`, diffed against `landing`: its
/// target nodes, checked to lower to a Deploy Intent. Generated domains expand
/// under `cluster_domain`; a plan, which has none, checks the rest.
pub(crate) fn freeze(
    environment: &EnvironmentId,
    organization: &OrganizationId,
    saved: &SavedEnvironmentIntent,
    landing: &Landing<'_>,
    scope: Scope<'_>,
    namespace: Namespace,
    cluster_domain: Option<&Hostname>,
) -> Result<Frozen, RpcError> {
    let (full, prune, services, retire, losses, every_saved): (
        bool,
        bool,
        &[ServiceName],
        &[Retire],
        &[VolumeLoss],
        bool,
    ) = match scope {
        Scope::All {
            losses,
            retire,
            prune,
        } => (true, prune, &[], retire, losses, false),
        Scope::Narrow {
            services,
            retire,
            losses,
            every_saved,
        } => (false, false, services, retire, losses, every_saved),
    };
    let dropped = landing.dropped(saved);
    if !full && services.is_empty() && retire.is_empty() {
        return Err(error::invalid(
            "A targeted Deployment names no Service",
            json!({}),
        ));
    }
    // A changed Config or Volume ships with the Service that mounts it, and is
    // recorded only once that Service is a node.
    let changed = |service: &SavedServiceIntent| {
        !landing.settled_service(service)
            || service.config_attachments.iter().any(|mount| {
                let id = &mount.config_resource_id;
                match saved
                    .configs
                    .iter()
                    .find(|config| config.resource_id == *id)
                {
                    Some(config) => !landing.settled_config(config),
                    None => landing.configs().any(|config| config.resource_id == *id),
                }
            })
            || service.volume_attachments.iter().any(|mount| {
                let id = &mount.volume_resource_id;
                match saved
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == *id)
                {
                    Some(volume) => !landing.settled_volume(volume),
                    None => landing.volumes().any(|volume| volume.resource_id == *id),
                }
            })
    };
    let mut named = services.to_vec();
    if every_saved {
        for service in saved.services.iter().filter(|service| changed(service)) {
            let name = ServiceName::parse(service.slug.as_str())
                .map_err(|_| error::corrupt("Service name"))?;
            if !named.contains(&name) {
                named.push(name);
            }
        }
    }
    let services = named.as_slice();
    let mut removals = ServiceRemovals::default();
    for (at, gone) in retire.iter().enumerate() {
        let running = |name: &ServiceName| {
            saved
                .services
                .iter()
                .any(|service| service.config.private_dns == *name)
        };
        let held = saved
            .services
            .iter()
            .any(|service| service.id == gone.id.as_str());
        // Only a full Deployment tears down, beside a Service it ships, what that
        // one no longer runs as.
        if held && !full
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
    let holds =
        |service: &SavedServiceIntent| retire.iter().find(|gone| gone.id.as_str() == service.id);
    let mut nodes: Vec<TargetNode> = if full {
        let shipping = |service: &SavedServiceIntent| {
            let mut node = TargetNode::service(service)?;
            if let (Some(gone), TargetNode::Service { removals, .. }) = (holds(service), &mut node)
            {
                removals.extend(gone.runtimes.iter().cloned());
            }
            Ok(node)
        };
        // A dropped Service that ran is its teardown; one that never ran is just gone.
        let tearing_down = |service: &SavedServiceIntent| {
            holds(service).map_or_else(|| TargetNode::service(service), TargetNode::retire)
        };
        // A teardown of a Service nothing may run is one a failed attempt left behind.
        let left_behind = retire.iter().filter(|gone| {
            !dropped
                .services
                .iter()
                .chain(&saved.services)
                .any(|service| service.id == gone.id.as_str())
        });
        saved
            .services
            .iter()
            .map(shipping)
            .chain(dropped.services.iter().map(tearing_down))
            .chain(left_behind.map(TargetNode::retire))
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
                        let staged = dropped
                            .services
                            .iter()
                            .any(|service| service.slug == name.as_str());
                        error::not_found(
                            if staged {
                                format!("Service {name} is staged for removal; a full Deploy removes it")
                            } else {
                                format!("No Service named {name} to deploy")
                            },
                            json!({ "services": saved.services.iter().map(|service| &service.slug).collect::<Vec<_>>() }),
                        )
                    })
            })
            .chain(retire.iter().map(TargetNode::retire))
            .collect::<Result<Vec<_>, _>>()?
    };
    let ships = if full {
        Ships::All { removals, prune }
    } else {
        Ships::Named {
            services: services.to_vec(),
            removals,
        }
    };
    // Live values and Setup Commands resolve at claim; checking without them is the same.
    let lowered = lower(
        environment,
        saved,
        &BTreeSet::new(),
        &ships,
        (namespace.clone(), cluster_domain),
        &crate::branch::Lowering::default(),
        organization,
    )?;
    if !full {
        // The planner also ships, at its Saved snapshot, each Service the named ones reference.
        let shipped = &lowered.shipped;
        let referenced: Vec<TargetNode> = saved
            .services
            .iter()
            .filter(|service| {
                shipped.contains(&service.config.private_dns)
                    && !nodes.iter().any(|node| node.id() == service.id)
                    && changed(service)
            })
            .map(TargetNode::service)
            .collect::<Result<_, _>>()?;
        nodes.extend(referenced);
    }
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
    // Shipping every Saved change, it also ships a changed one nothing mounts.
    let kept: Vec<TargetNode> = saved
        .volumes
        .iter()
        .filter(|volume| full || mounted(volume) || every_saved && !landing.settled_volume(volume))
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
        let fresh = !landing
            .configs()
            .any(|old| old.resource_id == config.resource_id);
        targeted.contains(&true) && (fresh || !targeted.contains(&false))
    };
    let configs: Vec<TargetNode> = saved
        .configs
        .iter()
        .filter(|config| {
            full || applies(config)
                || every_saved
                    && !saved.services.iter().any(|service| {
                        service
                            .config_attachments
                            .iter()
                            .any(|mount| mount.config_resource_id == config.resource_id)
                    })
                    && !landing.settled_config(config)
        })
        .chain(dropped.configs.iter().filter(|_| full || every_saved))
        .map(TargetNode::config)
        .collect::<Result<_, _>>()?;
    nodes.extend(configs);
    if !full {
        let unmounted = unmounted(saved, landing, &nodes)?;
        nodes.extend(unmounted);
    }
    let ships = |id: &str| nodes.iter().any(|node| node.id() == id);
    let unmounts = |loss: &&VolumeLoss| {
        full || landing
            .services()
            .filter(|service| {
                service
                    .volume_attachments
                    .iter()
                    .any(|mount| mount.volume_resource_id == loss.volume.id.as_str())
            })
            .all(|service| ships(&service.id))
    };
    let removed: Vec<TargetNode> = losses
        .iter()
        .filter(unmounts)
        .map(|loss| {
            let volume = landing
                .volumes()
                .find(|volume| volume.resource_id == loss.volume.id.as_str())
                .ok_or_else(|| error::corrupt("Applied State"))?;
            TargetNode::volume(volume, Some(loss.deletes.clone()))
        })
        .collect::<Result<_, _>>()?;
    nodes.extend(removed);
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
        full,
        prune,
        namespace,
        credentials: BTreeMap::new(),
        cluster_domain: cluster_domain.cloned(),
        sourceless,
    })
}

/// Each Config `saved` drops of `landing` that `nodes` don't ship yet but leave
/// with: a removed Config goes with the Services that may run mounting it, once
/// `nodes` ship every one.
pub(super) fn unmounted(
    saved: &SavedEnvironmentIntent,
    landing: &Landing<'_>,
    nodes: &[TargetNode],
) -> Result<Vec<TargetNode>, RpcError> {
    let ships = |id: &str| nodes.iter().any(|node| node.id() == id);
    landing
        .dropped(saved)
        .configs
        .iter()
        .filter(|old| {
            let targeted: Vec<bool> = landing
                .services()
                .filter(|service| {
                    service
                        .config_attachments
                        .iter()
                        .any(|mount| mount.config_resource_id == old.resource_id)
                })
                .map(|service| ships(&service.id))
                .collect();
            !ships(&old.resource_id) && targeted.contains(&true) && !targeted.contains(&false)
        })
        .map(TargetNode::config)
        .collect()
}

/// A Deployment lowered at claim.
pub(crate) struct Lowered {
    /// The lowering input the runner lowers again once it built.
    pub(crate) input: LowerDeploymentInput,
    pub(crate) intent: DeployIntent,
    /// The Services a narrowed Deploy ships once its builds and uploads land: the
    /// named ones and what they reference. Empty for a full Deploy.
    pub(crate) shipped: BTreeSet<ServiceName>,
    /// Service variables a secret without a value or a broken reference left out,
    /// then Config file references whose resolved value may break the file or is
    /// left empty.
    pub(crate) warnings: Vec<DeploymentWarning>,
}

/// `saved` compiled, and its variables and Config files resolved as [`lower`]
/// resolves them. The compiled producers are `saved`'s own: live values order
/// nothing, since what provides them runs elsewhere.
fn resolve(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    cluster_domain: Option<&Hostname>,
    branch: &crate::branch::Lowering,
) -> Result<
    (
        ployz_core::config::CompiledEnvironmentIntent,
        variables::Resolved,
    ),
    RpcError,
> {
    let mut compiled = compile_environment_intent(
        environment.as_str(),
        crate::domain::expand(saved, cluster_domain),
    );
    let own = compiled.variable_producers.len();
    compiled
        .variable_producers
        .extend(branch.live.iter().cloned());
    let resolved = variables::resolve(&compiled, environment.as_str())?;
    compiled.variable_producers.truncate(own);
    Ok((compiled, resolved))
}

/// Lower Saved revision `saved` to the Deploy Intent of a Deployment that
/// `ships` into its Namespace, with generated domains under the
/// Cluster Domain. Variables and Config files resolve here, a Branch's Live Node
/// references against `branch.live`. A value sealed in the browser for `environment`
/// reaches its Service unopened, with the identity the Machine opens it under from
/// `organization`. `deployed` names the Volumes Applied State holds.
pub(super) fn lower(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    deployed: &BTreeSet<String>,
    ships: &Ships,
    (namespace, cluster_domain): (Namespace, Option<&Hostname>),
    branch: &crate::branch::Lowering,
    organization: &OrganizationId,
) -> Result<Lowered, RpcError> {
    let (compiled, mut resolved) = resolve(environment, saved, cluster_domain, branch)?;
    let (narrowed, services, removals, prune): (bool, &[ServiceName], _, _) = match ships {
        Ships::All { removals, prune } => (
            false,
            &[],
            removals,
            if *prune {
                PruneScope::Namespace
            } else {
                PruneScope::Removals
            },
        ),
        Ships::Named { services, removals } => {
            (true, services.as_slice(), removals, PruneScope::Namespace)
        }
    };
    let lineages: BTreeMap<String, String> = compiled
        .variable_producers
        .iter()
        .map(|producer| (producer.owner_lineage_id.clone(), producer.owner_id.clone()))
        .collect();
    let targeted: Vec<&str> = saved
        .services
        .iter()
        .filter(|service| services.iter().any(|name| name.as_str() == service.slug))
        .map(|service| service.id.as_str())
        .collect();
    // Only the Services this Deployment starts must fit a Container's environment.
    for service in &saved.services {
        if (!narrowed || targeted.contains(&service.id.as_str()))
            && let Some(env) = resolved.env.get(&service.id)
        {
            variables::refuse_too_big(service.config.private_dns.as_str(), env)?;
        }
    }
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
    // Empty reconciles the whole Namespace; names deploy only those Services.
    let selected = if narrowed {
        saved
            .services
            .iter()
            .filter(|service| services.iter().any(|name| name.as_str() == service.slug))
            .map(|service| service.config.private_dns.clone())
            .chain(removals.iter().cloned())
            .map(|name| ServiceAttempt { name })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let slugs: BTreeMap<&str, &str> = saved
        .services
        .iter()
        .map(|service| (service.lineage_id.as_str(), service.slug.as_str()))
        .collect();
    // Each warning with the Services whose Deploy it concerns.
    let mut warnings: Vec<(Vec<&ServiceName>, DeploymentWarning)> = snapshots
        .iter()
        .filter_map(|(config, snapshot)| {
            let id = snapshot.service_id.as_deref()?;
            let service = saved
                .services
                .iter()
                .find(|service| service.id == id)
                .and_then(|service| ServiceName::parse(service.slug.as_str()).ok())
                .unwrap_or_else(|| config.settings.private_dns.clone());
            let not_set = resolved.not_set.remove(id).unwrap_or_default();
            let broken = resolved.broken.remove(id).unwrap_or_default();
            let left_out = not_set
                .into_iter()
                .map(|variable| (variable, false))
                .chain(broken.into_iter().map(|variable| (variable, true)))
                .map(|(variable, broken)| {
                    (
                        vec![&config.settings.private_dns],
                        DeploymentWarning::NotSet {
                            service: service.clone(),
                            variable,
                            broken,
                        },
                    )
                })
                .collect::<Vec<_>>();
            Some(left_out)
        })
        .flatten()
        .collect();
    let mut configs = Vec::new();
    for config in &saved.configs {
        let mounting: Vec<&SavedServiceIntent> = saved
            .services
            .iter()
            .filter(|service| {
                service
                    .config_attachments
                    .iter()
                    .any(|mount| mount.config_resource_id == config.resource_id)
            })
            .collect();
        if !snapshots.iter().any(|(_, snapshot)| {
            snapshot
                .service_id
                .as_deref()
                .is_some_and(|id| mounting.iter().any(|service| service.id == id))
        }) {
            continue;
        }
        let mounts: Vec<&ServiceName> = mounting
            .iter()
            .map(|service| &service.config.private_dns)
            .collect();
        let mut files = resolved
            .configs
            .remove(&config.resource_id)
            .unwrap_or_default();
        let mut lowered = BTreeMap::new();
        for (name, file) in &config.files {
            let resolved = files.remove(name).ok_or_else(|| error::corrupt("Config"))?;
            let shown = |reference: &ployz_core::config::FragileReference| match &reference.owner {
                ValuePartOwner::Service { lineage_id } => match slugs.get(lineage_id.as_str()) {
                    Some(slug) => format!("{slug}.{}", reference.key),
                    None => reference.key.clone(),
                },
                ValuePartOwner::Self_ => reference.key.clone(),
            };
            warnings.extend(resolved.fragile.iter().map(|reference| {
                (
                    mounts.clone(),
                    DeploymentWarning::Fragile {
                        config: config.name.clone(),
                        file: name.clone(),
                        variable: shown(reference),
                    },
                )
            }));
            let not_set = resolved.not_set.iter().map(|reference| (reference, false));
            let broken = resolved.broken.iter().map(|reference| (reference, true));
            warnings.extend(not_set.chain(broken).map(|(reference, broken)| {
                (
                    mounts.clone(),
                    DeploymentWarning::NotSetInFile {
                        config: config.name.clone(),
                        file: name.clone(),
                        secret: shown(reference),
                        broken,
                    },
                )
            }));
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
    for service in &saved.services {
        if narrowed && !targeted.contains(&service.id.as_str()) {
            continue;
        }
        let mounted: Vec<&LowerDeploymentConfig> = configs
            .iter()
            .filter(|config| {
                service
                    .config_attachments
                    .iter()
                    .any(|mount| mount.config_resource_id == config.config_resource_id)
            })
            .collect();
        let bytes = mounted
            .iter()
            .flat_map(|config| config.files.values())
            .map(|file| file.content.len())
            .sum();
        let names: Vec<&ConfigName> = mounted.iter().map(|config| &config.name).collect();
        let name =
            ServiceName::parse(service.slug.as_str()).map_err(|_| error::corrupt("Service"))?;
        if let Some(error) = crate::config_item::mounts_too_much(&name, &names, bytes) {
            return Err(error);
        }
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
                deployed: deployed.contains(&volume.resource_id),
            })
            .collect(),
        lineages: lineages.clone(),
        selected: Some(selected.clone()),
        configs: configs.clone(),
        removals: removals.clone(),
        prune,
        cluster_domain: cluster_domain
            .and_then(|cluster| ployz_core::IngressHost::parse(cluster.as_str()).ok()),
        sealed_for: uuid::Uuid::parse_str(environment.as_str())
            .ok()
            .map(|environment| SealedFor {
                organization: organization.as_str().to_owned(),
                environment,
            }),
    };
    // `uploaded` also gives each named source-less Service the image its upload
    // builds, as the SDK's preparation does.
    let with = |built_later: bool, uploaded: bool| {
        snapshots
            .iter()
            .map(|(config, snapshot)| {
                let build = built_later
                    && match config.settings.source {
                        ServiceSource::Git { .. } => true,
                        ServiceSource::Empty { .. } => {
                            uploaded
                                && snapshot
                                    .service_id
                                    .as_deref()
                                    .is_some_and(|id| targeted.contains(&id))
                        }
                        ServiceSource::Image { .. } => false,
                    };
                LowerDeploymentSnapshot {
                    config: json_value(&if build { built(config) } else { config.clone() }),
                    built_with: build.then_some(config.settings.build.build_method),
                    ..snapshot.clone()
                }
            })
            .collect()
    };
    let checked = |snapshots| {
        lower_deployment(input(snapshots)).map_err(|error| {
            error::invalid(
                format!("This Environment can't deploy: {}", error.message),
                json!({ "path": error.path }),
            )
        })
    };
    let intent = checked(with(true, false))?;
    // Only the Services this Deploy ships, the named ones and what they reference;
    // the rest ride along unchanged.
    let shipped: BTreeSet<ServiceName> = if narrowed {
        checked(with(true, true))?
            .applied_names()
            .into_iter()
            .cloned()
            .collect()
    } else {
        BTreeSet::new()
    };
    // A Legacy secret of a Service this Deploy ships waits for the reseal: leaving
    // it out would drop a value the Service holds. One a Branch reads live from its
    // parent has no Service here, and is Not set in the Branch even once resealed.
    let waiting = resolved.legacy.iter().find_map(|(owner, key)| {
        let service = saved.services.iter().find(|service| service.id == *owner)?;
        (!narrowed || shipped.contains(&service.config.private_dns)).then_some((service, key))
    });
    if let Some((service, key)) = waiting {
        return Err(awaiting_reseal(&service.slug, key));
    }
    let warnings = warnings
        .into_iter()
        .filter(|(services, _)| !narrowed || services.iter().any(|name| shipped.contains(*name)))
        .map(|(_, warning)| warning)
        .collect();
    Ok(Lowered {
        input: input(with(false, false)),
        shipped,
        intent,
        warnings,
    })
}

/// A Legacy secret, which no Machine opens. `invalid_argument`, so a claim fails the
/// Deployment rather than leave it queued.
fn awaiting_reseal(service: &str, key: &str) -> RpcError {
    let variable = format!("{service}.{key}");
    error::invalid(
        format!(
            "{variable} is waiting for Ployz Cloud to reseal it for the Cluster, and this Deploy would leave it out. \
             Cloud reseals such values when its worker starts. Deploy again in a minute, or set {variable} again"
        ),
        json!({ "variable": variable, "reseal": "pending" }),
    )
}

/// `config` with a Git or empty source replaced by the image its build or upload
/// will produce, as the SDK's preparation does once it built it.
pub(super) fn built(config: &ServiceConfig) -> ServiceConfig {
    let mut config = config.clone();
    if matches!(
        config.settings.source,
        ServiceSource::Git { .. } | ServiceSource::Empty { .. }
    ) {
        config.settings.source = ServiceSource::Image {
            version: 1,
            image: format!("ployz-build/{}:pending", config.settings.private_dns),
            credentials: ServiceImageCredentials::None,
        };
    }
    config
}

pub(crate) fn json_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("lowering input is JSON")
}
