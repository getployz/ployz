//! Each target node's Node Outcome, projected from what a run recorded.

use super::*;

/// What a run recorded that settles its target nodes: the Deploy Preview it
/// prepared with the Services it confirmed while executing, then what executing
/// that preview did.
pub(super) enum Evidence<'run> {
    Planned {
        preview: &'run DeployPreview,
        /// Runtime Services whose every planned operation completed so far.
        confirmed: &'run [ServiceName],
    },
    Executed {
        preview: &'run DeployPreview,
        projection: &'run RuntimeOutcomeProjection,
        removed: &'run [VolumeRemoval],
    },
}

/// Each target node's Node Outcome from `evidence`. A Service the preview plans
/// nothing for is Unchanged; one it plans for is Pending until confirmed or executed, then all
/// completed, or switched with only its old copies' removals left, is Deployed
/// (Removed once it left Saved State), some ran is Failed and
/// none ran is Not attempted, as is one the planner blocked. A Service that left Saved State with nothing planned
/// is Removed, whether or not the snapshot saw every Machine: the Deploy removed
/// every copy it could reach, and a copy on a Machine it could not check is the
/// next full Deploy's prune. One with several runtime Services reads the worst of them. A kept Volume follows the
/// targeted Services mounting it: Deployed once one of them is, whatever the others do. It is Unchanged when Applied State already
/// holds it as saved. One no targeted Service mounts is Not attempted when new
/// (nothing creates its storage), and a held one is Deployed only by a Deploy that
/// succeeded. A removed
/// Volume is Removed once the Deploy succeeded and every Docker Volume it deletes is
/// gone; Failed when one wasn't deleted, and Not attempted when the Deploy failed first.
/// A removed Config is Removed once the Deploy succeeded and every targeted Service
/// that mounted it landed, else Not attempted.
pub(super) fn node_outcomes(
    nodes: &[TargetNode],
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    evidence: &Evidence<'_>,
) -> BTreeMap<String, NodeStatus> {
    // A Service confirmed or executed lands Deployed, or Removed once it left Saved State.
    let landed = |kept: bool| {
        if kept {
            NodeStatus::Deployed
        } else {
            NodeStatus::Removed
        }
    };
    let service = |name: &ServiceName, kept: bool| match evidence {
        Evidence::Planned { preview, .. } | Evidence::Executed { preview, .. }
            if preview.blocked.iter().any(|entry| entry.service == *name) =>
        {
            NodeStatus::NotAttempted
        }
        Evidence::Planned { confirmed, .. } if confirmed.contains(name) => landed(kept),
        Evidence::Planned { preview, .. } => {
            if preview
                .operations
                .iter()
                .any(|row| row.service_name() == Some(name))
            {
                NodeStatus::Pending
            } else {
                NodeStatus::Unchanged
            }
        }
        Evidence::Executed { projection, .. } => {
            if projection.confirmed_services.contains(name) {
                landed(kept)
            } else if projection.failed_services.contains(name) {
                NodeStatus::Failed
            } else if projection.unattempted_services.contains(name) {
                NodeStatus::NotAttempted
            } else {
                NodeStatus::Unchanged
            }
        }
    };
    let (Evidence::Planned { preview, .. } | Evidence::Executed { preview, .. }) = evidence;
    let removed_service = |runtimes: Vec<&ServiceName>| {
        let statuses: Vec<NodeStatus> = runtimes
            .into_iter()
            .map(|name| match service(name, false) {
                NodeStatus::Unchanged
                    if !preview
                        .operations
                        .iter()
                        .any(|row| row.service_name() == Some(name)) =>
                {
                    NodeStatus::Removed
                }
                NodeStatus::Unchanged => NodeStatus::NotAttempted,
                status @ (NodeStatus::Pending
                | NodeStatus::Deployed
                | NodeStatus::Removed
                | NodeStatus::Failed
                | NodeStatus::NotAttempted
                | NodeStatus::Unknown) => status,
            })
            .collect();
        [
            NodeStatus::Failed,
            NodeStatus::NotAttempted,
            NodeStatus::Pending,
        ]
        .into_iter()
        .find(|worst| statuses.contains(worst))
        .unwrap_or(NodeStatus::Removed)
    };
    let removed_volume = |deletes: &[DockerVolumeId]| match evidence {
        Evidence::Planned { .. } => NodeStatus::Pending,
        Evidence::Executed {
            projection,
            removed,
            ..
        } => {
            let gone = |id: &DockerVolumeId| {
                removed.iter().any(|removal| {
                    removal.id == *id && matches!(removal.outcome, VolumeRemovalOutcome::Removed)
                })
            };
            if !matches!(
                projection.summary,
                ployz_core::config::RuntimeOutcomeSummary::Success { .. }
            ) {
                NodeStatus::NotAttempted
            } else if deletes.iter().all(gone) {
                NodeStatus::Removed
            } else {
                NodeStatus::Failed
            }
        }
    };
    let kept_volume = |volume: &SavedVolumeIntent| {
        let mounting: Vec<NodeStatus> = saved
            .services
            .iter()
            .filter(|service| {
                nodes.iter().any(|node| node.id() == service.id)
                    && service
                        .volume_attachments
                        .iter()
                        .any(|mount| mount.volume_resource_id == volume.resource_id)
            })
            .map(|mounting| service(&mounting.config.private_dns, true))
            .collect();
        // Confirmed storage advances whatever its siblings do.
        if mounting.contains(&NodeStatus::Deployed) && !applied.volumes.contains(volume) {
            NodeStatus::Deployed
        } else if mounting.contains(&NodeStatus::Failed) {
            NodeStatus::Failed
        } else if mounting.contains(&NodeStatus::NotAttempted) {
            NodeStatus::NotAttempted
        } else if mounting.contains(&NodeStatus::Pending) {
            NodeStatus::Pending
        } else if applied.volumes.contains(volume) {
            NodeStatus::Unchanged
        } else if mounting.is_empty()
            && !applied
                .volumes
                .iter()
                .any(|held| held.resource_id == volume.resource_id)
        {
            // Storage work comes only with a mount: nothing a Deploy runs creates it.
            NodeStatus::NotAttempted
        } else if matches!(evidence, Evidence::Planned { .. })
            && !mounting.contains(&NodeStatus::Deployed)
        {
            NodeStatus::Pending
        } else if mounting.is_empty() && !succeeded(evidence) {
            // A held Volume no Service mounts changes only with a Deploy that fully ran.
            NodeStatus::NotAttempted
        } else {
            NodeStatus::Deployed
        }
    };
    let kept_config = |config: &SavedConfigIntent| {
        let mounting: Vec<NodeStatus> = saved
            .services
            .iter()
            .filter(|service| {
                nodes.iter().any(|node| node.id() == service.id)
                    && service
                        .config_attachments
                        .iter()
                        .any(|mount| mount.config_resource_id == config.resource_id)
            })
            .map(|mounting| service(&mounting.config.private_dns, true))
            .collect();
        let held = applied.configs.contains(config);
        let fresh = !applied
            .configs
            .iter()
            .any(|held| held.resource_id == config.resource_id);
        let deployed = mounting.contains(&NodeStatus::Deployed);
        if fresh && deployed {
            NodeStatus::Deployed
        } else if mounting.contains(&NodeStatus::Failed) {
            NodeStatus::Failed
        } else if mounting.contains(&NodeStatus::NotAttempted) {
            NodeStatus::NotAttempted
        } else if mounting.contains(&NodeStatus::Pending) {
            NodeStatus::Pending
        } else if held {
            NodeStatus::Unchanged
        } else if deployed {
            NodeStatus::Deployed
        } else if matches!(evidence, Evidence::Planned { .. }) {
            NodeStatus::Pending
        } else if succeeded(evidence) {
            NodeStatus::Deployed
        } else {
            NodeStatus::NotAttempted
        }
    };
    let service_node = |node: &TargetNode, id: &str, runtime: &ServiceName| {
        if saved.services.iter().any(|kept| kept.id == id) {
            service(runtime, true)
        } else {
            removed_service(node.runtimes())
        }
    };
    let removed_config = |config: &str| match evidence {
        Evidence::Planned { .. } => NodeStatus::Pending,
        Evidence::Executed { .. }
            if succeeded(evidence)
                && nodes.iter().all(|node| match node {
                    TargetNode::Service { id, runtime, .. } => {
                        !applied.services.iter().any(|held| {
                            held.id == id.as_str()
                                && held
                                    .config_attachments
                                    .iter()
                                    .any(|mount| mount.config_resource_id == config)
                        }) || matches!(
                            service_node(node, id.as_str(), runtime),
                            NodeStatus::Removed | NodeStatus::Deployed | NodeStatus::Unchanged
                        )
                    }
                    TargetNode::Volume { .. } | TargetNode::Config { .. } => true,
                }) =>
        {
            NodeStatus::Removed
        }
        Evidence::Executed { .. } => NodeStatus::NotAttempted,
    };
    nodes
        .iter()
        .map(|node| {
            let status = match node {
                TargetNode::Service { id, runtime, .. } => service_node(node, id.as_str(), runtime),
                TargetNode::Volume {
                    deletes: Some(deletes),
                    ..
                } => removed_volume(deletes),
                TargetNode::Volume {
                    id, deletes: None, ..
                } => saved
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == id.as_str())
                    .map_or(NodeStatus::Unchanged, kept_volume),
                TargetNode::Config { id, .. } => saved
                    .configs
                    .iter()
                    .find(|config| config.resource_id == id.as_str())
                    .map_or_else(|| removed_config(id.as_str()), kept_config),
            };
            (node.id().to_owned(), status)
        })
        .collect()
}

/// `next`, with every node `prior` already advanced kept as it was: Applied State
/// holds it now, so recomputing would read it Unchanged.
pub(super) fn keep_advanced(
    prior: &BTreeMap<String, NodeStatus>,
    mut next: BTreeMap<String, NodeStatus>,
) -> BTreeMap<String, NodeStatus> {
    next.extend(
        prior
            .iter()
            .filter(|(_, status)| status.advances())
            .map(|(id, status)| (id.clone(), *status)),
    );
    next
}

/// Whether `evidence` is a Deploy that executed every operation.
fn succeeded(evidence: &Evidence<'_>) -> bool {
    matches!(
        evidence,
        Evidence::Executed { projection, .. }
            if matches!(projection.summary, ployz_core::config::RuntimeOutcomeSummary::Success { .. })
    )
}

/// The Node Outcomes a running Deployment already settles from its preview and the
/// Services it confirmed: the ones no longer Pending.
pub(super) fn settled(
    nodes: &[TargetNode],
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    preview: &DeployPreview,
    confirmed: &[ServiceName],
) -> BTreeMap<String, NodeStatus> {
    node_outcomes(
        nodes,
        saved,
        applied,
        &Evidence::Planned { preview, confirmed },
    )
    .into_iter()
    .filter(|(_, status)| *status != NodeStatus::Pending)
    .collect()
}
