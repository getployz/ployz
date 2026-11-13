//! Retrying an ended Deployment and rolling back to one: queueing again what it froze.

use super::*;

/// Queue Deployment `id` shipping exactly what `source` froze: its Saved revision,
/// targets, Namespace, registry credentials, upload and pinned commits, whatever
/// changed since. Any Deployment that ended can be redeployed, applied ones too.
/// One older than a Deployment that may have changed the Servers rolls that back,
/// once the caller confirmed exactly those. Its source's consent covered what the
/// Servers held then, and a later Deployment may have brought a Volume back, so a
/// rollback reviews its deletes against Applied State and the Servers now, and a
/// full one retires every Service added since, as a Deploy of its revision would.
pub(crate) fn retry(
    tx: &mut dyn Tx,
    who: &Actor,
    retry: &crate::Retry,
    trusted: &crate::Trusted,
) -> Result<(DeploymentSummary, Rollback), RpcError> {
    let (id, source) = (&retry.id, &retry.deployment);
    owned(tx, who, source)?;
    let stored = locked(tx, source)?;
    match stored.summary.status {
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => {}
        DeploymentStatus::WaitingForCi
        | DeploymentStatus::Queued
        | DeploymentStatus::Running
        | DeploymentStatus::Cancelling => {
            return Err(error::conflict(
                "This Deployment hasn't ended: cancel it or wait before redeploying it",
                json!({ "deployment": source }),
            ));
        }
        DeploymentStatus::Superseded => return Err(superseded(source)),
    }
    // Cancelled while waiting for CI: it never froze what it ships.
    if stored.summary.held {
        return Err(error::conflict(
            "This push was cancelled before its CI passed, so nothing was frozen to redeploy: push again",
            json!({ "deployment": source }),
        ));
    }
    if stored.summary.remove {
        if stored.summary.status == DeploymentStatus::Applied {
            return Err(error::conflict(
                "This removal applied: nothing is left to redeploy",
                json!({ "deployment": source }),
            ));
        }
        let environment = scope::load_by_id(tx, &stored.summary.environment_id)?;
        crate::removal::guard_removal(tx, &environment)?;
    } else {
        // Its revision asks a storage of each Volume it ships; one resized since may
        // no longer fit it. A Volume it never shipped it leaves as it is.
        let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
        let shipped = saved.volumes.iter().filter(|volume| {
            stored
                .nodes
                .iter()
                .any(|node| node.id() == volume.resource_id)
        });
        crate::volume::check_storage(
            tx,
            &stored.summary.environment_id,
            shipped,
            crate::volume::StorageRule::Replay,
        )?;
    }
    // What a rollback retires is frozen against Applied State now, which a newer
    // running Deployment may yet change. A queued one is superseded, as by a Deploy,
    // and one whose lease lapsed is unknown: its runner is gone.
    if let Some(newer) = tx
        .query(
            &format!(
                "SELECT MAX(number) FROM config_deployment WHERE environment_id = ?1 \
                 AND number > ?2 AND {} AND lease > ?3",
                CLAIMED.as_str()
            ),
            &[
                stored.summary.environment_id.as_str().into(),
                i64::try_from(stored.summary.numbered()?)
                    .map_err(|_| error::corrupt("Deployment number"))?
                    .into(),
                now().into(),
            ],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment number"))?
        .optional_int(0)?
    {
        return Err(error::conflict(
            format!("Wait for #{newer} to finish, then redeploy."),
            json!({ "deployment": source, "waiting_for": newer }),
        ));
    }
    let mut rollback = Rollback {
        number: stored.summary.numbered()?,
        rolls_back: undone(tx, &stored.summary)?,
        removes: Vec::new(),
        retires: Vec::new(),
        ships: None,
        replays_destruction: false,
        replays: BTreeSet::new(),
    };
    if let Some(confirmed) = &retry.rolls_back
        && *confirmed != rollback.rolls_back
    {
        return Err(error::confirmation_required(
            match numbered(&rollback.rolls_back) {
                Some(undoes) => format!(
                    "Redeploying #{} now undoes {undoes}: confirm again",
                    rollback.number
                ),
                None => format!(
                    "Redeploying #{} no longer rolls anything back: confirm again",
                    rollback.number
                ),
            },
            json!({
                "deployment": source,
                "number": rollback.number,
                "rolls_back": rollback.rolls_back,
            }),
        ));
    }
    let fresh = match rollback.rolls_back.is_empty() {
        true => None,
        false => Some(rollback_losses(tx, who, &stored, retry, trusted)?),
    };
    if let Some((_, losses)) = &fresh {
        rollback.removes.clone_from(losses);
    }
    let (services, nodes) = retried(tx, &stored, fresh)?;
    // A rollback retires each Service it ships that its revision doesn't hold. A
    // plain retry retires only what its source's review already covered.
    let revision = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    rollback.retires = nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Service { id, .. }
                if node.retires()
                    || revision
                        .services
                        .iter()
                        .all(|service| service.id != id.as_str()) =>
            {
                Some(id.clone())
            }
            TargetNode::Service { .. } | TargetNode::Volume { .. } | TargetNode::Config { .. } => {
                None
            }
        })
        .filter(|_| !rollback.rolls_back.is_empty())
        .collect();
    rollback.ships = services.is_empty().then(|| {
        nodes
            .iter()
            .filter_map(|node| match node {
                TargetNode::Service { id, .. } if !node.retires() => Some(id.clone()),
                TargetNode::Service { .. }
                | TargetNode::Volume { .. }
                | TargetNode::Config { .. } => None,
            })
            .collect()
    });
    let environment = scope::load_by_id(tx, &stored.summary.environment_id)?;
    let head = head(tx, &environment)?;
    let running = crate::review::approval::may_hold(&head);
    let dropped = crate::review::approval::effects_of(
        tx,
        (&stored.summary.environment_id, &head),
        (&running, &revision),
    )?;
    // A Service teardown it queues destroys nothing once an applied Deployment took
    // the Service off and Cloud saw every Server without it. A Volume delete always
    // counts: a failed Deployment may have left its data where Head can't see.
    let observed = trusted
        .running
        .as_ref()
        .filter(|running| running.failure.is_none() && running.unanswered.is_empty());
    let seen = |removals: &ployz_core::ServiceRemovals| {
        trusted.running.as_ref().is_some_and(|running| {
            running.services.iter().any(|service| {
                service.namespace == stored.namespace && removals.contains(&service.name)
            })
        })
    };
    let gone = |id: &ServiceLineageId, removals: &ployz_core::ServiceRemovals| {
        running
            .services
            .iter()
            .all(|service| service.id != id.as_str())
            && observed.is_some()
            && !seen(removals)
    };
    rollback.replays_destruction = !dropped.is_empty()
        || nodes.iter().any(|node| match node {
            TargetNode::Service { id, removals, .. } => !removals.is_empty() && !gone(id, removals),
            TargetNode::Volume { deletes, .. } => deletes.is_some(),
            TargetNode::Config { .. } => false,
        });
    // What its revision dropped, it destroys only on a Service or Volume it ships;
    // a Service it tears down that Cloud saw running, it removes.
    let stray = nodes.iter().filter_map(|node| match node {
        TargetNode::Service {
            id, name, removals, ..
        } if seen(removals) => Some(crate::review::DestructiveEffect {
            kind: crate::review::DestructiveKind::RemovesService,
            name: name.to_string(),
            node: id.to_string(),
            path: name.to_string(),
            lost_after: None,
        }),
        TargetNode::Service { .. } | TargetNode::Volume { .. } | TargetNode::Config { .. } => None,
    });
    rollback.replays = dropped
        .into_iter()
        .filter(|effect| {
            nodes.iter().any(|node| match node {
                TargetNode::Service { id, .. } => id.as_str() == effect.node,
                TargetNode::Volume { id, .. } => id.as_str() == effect.node,
                TargetNode::Config { .. } => false,
            })
        })
        .chain(stray)
        .collect();
    // A retry builds from what its source did, but sends nothing.
    let retried_uploads = stored.service_uploads.clone().map(|uploads| {
        json_text(&super::uploads::ServiceUploads {
            sent: false,
            provisional: false,
            ..uploads
        })
    });
    let admitted_at = now();
    queue(tx, &stored.summary.environment_id, false, |tx, number| {
        let listed = next_listed(tx, &stored.summary.environment_id)?;
        // Every other frozen column comes from the source, so a retry never re-reads
        // authored state.
        tx.execute(
            "INSERT INTO config_deployment \
             (id, organization_id, environment_id, number, status, saved_revision, services, \
              nodes, namespace, run, credentials, upload, cluster_domain, admitted, admitted_by, \
              message, listed, service_uploads, preview_review, awaits_approval) \
             SELECT ?1, organization_id, environment_id, ?2, 'queued', saved_revision, ?7, \
              ?8, namespace, ?3, credentials, upload, cluster_domain, ?5, ?6, message, ?9, \
              ?10, preview_review, ?11 \
             FROM config_deployment WHERE id = ?4",
            &[
                id.as_str().into(),
                i64::try_from(number)
                    .map_err(|_| error::corrupt("Deployment number"))?
                    .into(),
                json_text(&Run {
                    prune: stored.run.prune,
                    ..Run::default()
                })
                .as_str()
                .into(),
                source.as_str().into(),
                admitted_at.into(),
                who.principal.as_ref().map(Principal::as_str).into(),
                json_text(&services).as_str().into(),
                json_text(&nodes).as_str().into(),
                listed.into(),
                retried_uploads.as_deref().into(),
                i64::from(retry.awaits_approval).into(),
            ],
        )?;
        // A retry builds the commits its source pinned, and may reuse the image its
        // source built, with that image's token.
        tx.execute(
            "INSERT INTO config_build \
             (deployment_id, service, organization_id, commit_sha, status, message, log, \
              receipt, image_token) \
             SELECT ?1, service, organization_id, commit_sha, 'pending', NULL, '', receipt, \
              image_token \
             FROM config_build WHERE deployment_id = ?2",
            &[id.as_str().into(), source.as_str().into()],
        )?;
        for approval in &retry.approvals {
            build::approve_commit(tx, id, approval)?;
        }
        Ok(())
    })?;
    Ok((load(tx, id)?.summary, rollback))
}

/// What a redeploy rolls back: the Deployment it ships again, by number, the
/// newer ones it undoes, the Volumes it removes with the data each deletes, and
/// the Services it retires.
#[derive(Clone, Debug)]
pub(crate) struct Rollback {
    pub(crate) number: u64,
    pub(crate) rolls_back: Vec<u64>,
    pub(crate) removes: Vec<VolumeLoss>,
    pub(crate) retires: Vec<ServiceLineageId>,
    /// The Services it ships, when it ships every one: it then takes off any other
    /// Service that runs, or may. None for one of some Services, which leaves the
    /// rest as they are.
    pub(crate) ships: Option<Vec<ServiceLineageId>>,
    /// Whether it may destroy anything, as its source's review accepted: what it
    /// queues deletes a Docker Volume or takes a runtime Service off that the Servers
    /// may hold or Cloud saw running, or its revision
    /// drops a Service, Volume, mount or domain the Servers may hold.
    pub(crate) replays_destruction: bool,
    /// What of that it names: each Service it removes, Volume it deletes or
    /// detaches, and domain it drops, of what the Servers may hold now.
    pub(crate) replays: BTreeSet<crate::review::DestructiveEffect>,
}

/// The Deployments of `deployment`'s Environment after it that may have changed
/// the Servers, oldest first: every one that started. Redeploying it undoes them.
/// None for one still waiting for CI: unnumbered, nothing ran after it.
pub(crate) fn undone(
    tx: &mut dyn Tx,
    deployment: &DeploymentSummary,
) -> Result<Vec<u64>, RpcError> {
    let Some(number) = deployment.number else {
        return Ok(Vec::new());
    };
    let number = i64::try_from(number).map_err(|_| error::corrupt("Deployment number"))?;
    tx.query(
        "SELECT number FROM config_deployment \
         WHERE environment_id = ?1 AND number > ?2 AND started IS NOT NULL ORDER BY number",
        &[deployment.environment_id.as_str().into(), number.into()],
    )?
    .iter()
    .map(|row| u64::try_from(row.int(0)?).map_err(|_| error::corrupt("Deployment number")))
    .collect()
}

/// `numbers` as people read them: "#5", "#5 and #6", "#5, #6 and #7"; none for none.
pub(crate) fn numbered(numbers: &[u64]) -> Option<String> {
    let (last, rest) = numbers.split_last()?;
    Some(match rest {
        [] => format!("#{last}"),
        _ => format!(
            "{} and #{last}",
            rest.iter()
                .map(|number| format!("#{number}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

/// Applied State and what rolling back to `stored` deletes, reviewed afresh
/// against the Servers now with `retry`'s acceptance, never its source's consent.
fn rollback_losses(
    tx: &mut dyn Tx,
    who: &Actor,
    stored: &Stored,
    retry: &crate::Retry,
    trusted: &crate::Trusted,
) -> Result<(SavedEnvironmentIntent, Vec<VolumeLoss>), RpcError> {
    let environment = &stored.summary.environment_id;
    scope::lock_all(tx, [environment.clone()])?;
    let loaded = scope::load_by_id(tx, environment)?;
    if !stored.summary.remove {
        crate::removal::marked(tx, &loaded)?;
    }
    let review = crate::review::review(tx, &loaded)?;
    crate::review::check(&review, retry.version.as_deref())?;
    let removed = rollback_removed(tx, stored, &review.head.applied)?;
    let losses = crate::removal::review(
        tx,
        who,
        environment,
        (&review.view.version, retry.version.as_deref()),
        removed,
        trusted.volumes.as_ref(),
        (&retry.accept_volume_loss, &[]),
    )?;
    Ok((review.head.applied, losses))
}

/// The Volumes redeploying `stored` removes: those its Saved revision drops, or
/// only those a targeted one removed itself, among the Volumes Applied State
/// `applied` holds and those any newer Deployment that started targeted. One that
/// failed or lost track may have created a Volume Applied State doesn't list.
pub(crate) fn rollback_removed(
    tx: &mut dyn Tx,
    stored: &Stored,
    applied: &SavedEnvironmentIntent,
) -> Result<Vec<loss::RemovedVolume>, RpcError> {
    let source = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    let mut removed = loss::removed(applied, &source, &stored.namespace)?;
    let number = i64::try_from(stored.summary.numbered()?)
        .map_err(|_| error::corrupt("Deployment number"))?;
    for row in &tx.query(
        "SELECT nodes FROM config_deployment \
         WHERE environment_id = ?1 AND number > ?2 AND started IS NOT NULL",
        &[stored.summary.environment_id.as_str().into(), number.into()],
    )? {
        let nodes: Vec<TargetNode> = row.json(0, "Deployment")?;
        for node in nodes {
            let TargetNode::Volume { id, name, .. } = node else {
                continue;
            };
            if source
                .volumes
                .iter()
                .any(|kept| kept.resource_id == id.as_str())
                || removed.iter().any(|volume| volume.id == id)
            {
                continue;
            }
            removed.push(loss::RemovedVolume {
                docker_volume: loss::docker_volume(&stored.namespace, id.as_str())?,
                id,
                name,
            });
        }
    }
    if !stored.summary.services.is_empty() {
        removed.retain(|volume| {
            stored.nodes.iter().any(|node| {
                matches!(
                    node,
                    TargetNode::Volume {
                        deletes: Some(_),
                        ..
                    }
                ) && node.id() == volume.id.as_str()
            })
        });
    }
    Ok(removed)
}

/// The targets and nodes a retry of `stored` ships: its own, but a targeted one
/// tears down only what the latest Saved revision still lacks, as after an Undo.
/// Either takes over the teardown of the queued Deployment it supersedes. A retry whose
/// Saved revision still holds a Service that teardown retires is refused: it would
/// run that Service again, or tear it down while deploying it, so the teardown stays.
/// A rollback passes `fresh`: Applied State now and the losses reviewed against it.
fn retried(
    tx: &mut dyn Tx,
    stored: &Stored,
    fresh: Option<(SavedEnvironmentIntent, Vec<VolumeLoss>)>,
) -> Result<(Vec<ServiceName>, Vec<TargetNode>), RpcError> {
    let environment = &stored.summary.environment_id;
    let organization = crate::build::organization(tx, &stored.summary.id)?;
    let latest = crate::review::latest_saved(tx, environment)?.map(|latest| latest.intent);
    let source = saved_at(tx, environment, stored.summary.saved)?;
    let holds = |intent: &SavedEnvironmentIntent, id: &str| {
        intent.services.iter().any(|service| service.id == id)
    };
    // A full Deployment tears down what Saved dropped too; superseding it runs that
    // Service again from the retried revision, as it always has.
    for row in &tx.query(
        "SELECT nodes FROM config_deployment \
         WHERE environment_id = ?1 AND status = 'queued' AND services != '[]'",
        &[environment.as_str().into()],
    )? {
        let queued: Vec<TargetNode> = row.json(0, "Deployment")?;
        if queued.iter().filter(|node| node.retires()).any(|node| {
            latest
                .as_ref()
                .is_some_and(|latest| !holds(latest, node.id()))
                && holds(&source, node.id())
        }) {
            return Err(error::conflict(
                "A teardown is queued: deploy instead",
                json!({ "deployment": stored.summary.id }),
            ));
        }
    }
    // A rollback deletes the losses reviewed afresh. One whose Volume Applied State
    // doesn't list, which a newer Deployment may have created, deletes what was found.
    let (fresh, unlisted) = match fresh {
        Some((applied, losses)) => {
            let (listed, unlisted): (Vec<VolumeLoss>, Vec<VolumeLoss>) =
                losses.into_iter().partition(|loss| {
                    applied
                        .volumes
                        .iter()
                        .any(|volume| volume.resource_id == loss.volume.id.as_str())
                });
            (Some((applied, listed)), unlisted)
        }
        None => (None, Vec::new()),
    };
    let unlisted = unlisted
        .into_iter()
        .filter(|loss| !loss.deletes.is_empty())
        .map(|loss| TargetNode::Volume {
            id: loss.volume.id,
            name: loss.volume.name,
            deletes: Some(loss.deletes),
        });
    if stored.summary.services.is_empty() {
        // A full rollback freezes its revision over Applied State now: it removes
        // what newer Deployments added and deletes only the losses reviewed afresh.
        let mut nodes = match &fresh {
            Some((applied, losses)) => {
                let landing = Landing::applied(applied);
                let retire = dropped(tx, environment, &source, &landing)?;
                freeze(
                    environment,
                    &organization,
                    &source,
                    &landing,
                    Scope::All {
                        losses,
                        retire: &retire,
                        prune: stored.summary.prune,
                    },
                    stored.namespace.clone(),
                    stored.cluster_domain.as_ref(),
                )?
                .nodes
            }
            None => stored.nodes.clone(),
        };
        nodes.extend(unlisted);
        if let Some(latest) = &latest {
            inherit(tx, environment, (latest, &source), &mut nodes)?;
        }
        return Ok((Vec::new(), nodes));
    }
    let latest = latest.ok_or_else(|| error::corrupt("Saved State"))?;
    let mut nodes = Vec::new();
    for node in &stored.nodes {
        match node {
            TargetNode::Service {
                id, name, removals, ..
            } if node.retires() => {
                if latest
                    .services
                    .iter()
                    .any(|service| service.id == id.as_str())
                {
                    continue;
                }
                let runtimes = ServiceRemovals::from_names(
                    removals
                        .iter()
                        .filter(|runtime| {
                            !latest
                                .services
                                .iter()
                                .any(|service| service.config.private_dns == **runtime)
                        })
                        .cloned(),
                );
                if !runtimes.is_empty() {
                    nodes.push(TargetNode::retire(&Retire {
                        id: id.clone(),
                        name: name.clone(),
                        runtimes,
                    })?);
                }
            }
            // A rollback deletes only the losses reviewed afresh, added below.
            TargetNode::Volume {
                deletes: Some(_), ..
            } if fresh.is_some() => {}
            TargetNode::Service { .. } | TargetNode::Volume { .. } | TargetNode::Config { .. } => {
                nodes.push(node.clone());
            }
        }
    }
    if let Some((applied, losses)) = &fresh {
        for loss in losses {
            let volume = applied
                .volumes
                .iter()
                .find(|volume| volume.resource_id == loss.volume.id.as_str())
                .ok_or_else(|| error::corrupt("Applied State"))?;
            nodes.push(TargetNode::volume(volume, Some(loss.deletes.clone()))?);
        }
    }
    nodes.extend(unlisted);
    inherit(tx, environment, (&latest, &source), &mut nodes)?;
    let named = |target: &ServiceName| {
        nodes
            .iter()
            .any(|node| matches!(node, TargetNode::Service { name, .. } if name == target))
    };
    let mut services: Vec<ServiceName> = stored
        .summary
        .services
        .iter()
        .filter(|target| named(target))
        .cloned()
        .collect();
    for node in nodes.iter().filter(|node| node.retires()) {
        if let TargetNode::Service { name, .. } = node
            && !services.contains(name)
        {
            services.push(name.clone());
        }
    }
    if services.is_empty() {
        return Err(error::conflict(
            "Every Service this Deployment removes is saved again: deploy instead",
            json!({ "deployment": stored.summary.id }),
        ));
    }
    Ok((services, nodes))
}
