//! Recording what a runner did with a Deployment it claimed, and ending it: each confirmed
//! Node Outcome advances Applied State.

use super::*;

/// Record `runner`'s evidence. Only the runner that claimed the Deployment may, and
/// recording the same evidence again changes nothing.
pub(crate) fn record(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
    evidence: RunEvidence,
) -> Result<DeploymentSummary, RpcError> {
    // What it records may follow into its Branches.
    let environment = super::load(tx, id)?.summary.environment_id;
    scope::lock_project(tx, &environment)?;
    let mut stored = locked(tx, id)?;
    if stored.summary.runner.as_ref() != Some(runner) {
        return Err(owned_elsewhere(id));
    }
    // Any record while it runs shows the runner is still there.
    if matches!(
        stored.summary.status,
        DeploymentStatus::Running | DeploymentStatus::Cancelling
    ) {
        stored.standing.renew(now());
    }
    match evidence {
        RunEvidence::Alive => {
            if stored.summary.status == DeploymentStatus::Unknown {
                // Its runner lease lapsed, so its runner is lost; one whose row
                // already says Unknown keeps its end.
                stored.standing = stored.standing.lose_runner(now());
            } else {
                running(&stored)?;
            }
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Prepared(preview) => {
            // Redacted before anything keeps it.
            let preview = serde_json::to_value(preview)
                .ok()
                .and_then(|preview| parse_runtime_preview(preview).ok())
                .ok_or_else(|| invalid_evidence("Deploy Preview"))?;
            match &stored.run.preview {
                Some(recorded) if redacted_runtime_preview(recorded.clone()) == preview => {
                    return Ok(stored.summary);
                }
                Some(_) => {
                    return Err(error::conflict(
                        "This Deployment already recorded a different Deploy Preview",
                        json!({ "deployment": id }),
                    ));
                }
                None => {}
            }
            running(&stored)?;
            let saved = held_revision(tx, &stored)?;
            let applied = applied_state(tx, &stored.summary.environment_id, &saved)?;
            // What the preview already settles shows while it runs; the rest reads
            // as Pending until what executing it did replaces these.
            stored.run.nodes = settled(&stored.nodes, &saved, &applied, &preview, &[]);
            let outcomes = node_outcomes(
                &stored.nodes,
                &saved,
                &applied,
                &Evidence::Planned {
                    preview: &preview,
                    confirmed: &[],
                },
            );
            // A shared Volume reads Not attempted when one Service mounting it is
            // blocked, yet the run still prepares it for any other that is planned.
            let pending = |id: &str| outcomes.get(id) == Some(&NodeStatus::Pending);
            crate::volume::fix_prepared(tx, &stored.summary.environment_id, &saved, |id| {
                pending(id)
                    || saved.services.iter().any(|service| {
                        pending(&service.id)
                            && service
                                .volume_attachments
                                .iter()
                                .any(|mount| mount.volume_resource_id == id)
                    })
            })?;
            stored.run.preview = Some(preview);
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Executed {
            outcome,
            removed,
            progress,
        } => {
            // A replay must carry the same evidence: Applied State moved since, so
            // recomputing its Node Outcomes would not tell.
            let executed = crate::digest::short_digest(
                &serde_json::to_string(&(&outcome, &removed)).expect("evidence is JSON"),
            );
            if stored.run.outcome.is_some() {
                if stored.run.executed.as_ref() == Some(&executed) {
                    return Ok(stored.summary);
                }
                return Err(error::conflict(
                    "This Deployment already recorded a different outcome",
                    json!({ "deployment": id }),
                ));
            }
            let Some(preview) = &stored.run.preview else {
                return Err(error::conflict(
                    "Record the Deploy Preview before what executing it did",
                    json!({ "deployment": id }),
                ));
            };
            let taken_over = stored.summary.status == DeploymentStatus::Unknown;
            if taken_over && !matches!(*outcome, DeployOutcome::Interrupted { .. }) {
                running(&stored)?;
            }
            let success = matches!(*outcome, DeployOutcome::Success { .. });
            let mut projection =
                project_runtime_outcome(preview, json!({ "version": 1, "outcome": outcome }))
                    .map_err(|_| invalid_evidence("Deploy Outcome"))?;
            super::rows::record(tx, &stored, &progress)?;
            let (failed, stopped) = super::rows::failed_or_cancelled(tx, &stored)?;
            let failed_on_its_own =
                |service: &ServiceName| failed.iter().any(|(s, _)| s == service);
            // The runner names only the first failure it saw. A Service that failed
            // on its own beside it reaches the outcome as never attempted, and a
            // cancel named before it would hide why the run failed. One that switched
            // reaches it as deployed, though its old copy's removal failed.
            for (service, _) in &failed {
                if projection.unattempted_services.contains(service)
                    || projection.confirmed_services.contains(service)
                {
                    projection.unattempted_services.retain(|s| s != service);
                    projection.confirmed_services.retain(|s| s != service);
                    projection.failed_services.push(service.clone());
                }
            }
            projection.failed_services.sort();
            let named_cancel = matches!(
                &*outcome,
                DeployOutcome::Failed {
                    failed: FailedOperation::Operation { error, .. }
                        | FailedOperation::Replacement { error, .. },
                    ..
                } if error.cancelled()
            );
            let real = failed
                .first()
                .filter(|_| named_cancel)
                .map(|(service, failure)| Failure {
                    reason: format!(
                        "{}: {}",
                        current_name(&stored.nodes, service),
                        failure.reason
                    )
                    .chars()
                    .take(500)
                    .collect(),
                    cause: failure.cause.clone(),
                    same_again: false,
                });
            let (reason, cause) = real
                .or_else(|| failure(&outcome, &stored.nodes, &preview.operations))
                .or_else(|| blocked(&stored.nodes, &preview.blocked))
                .or_else(|| {
                    success
                        .then(|| undeleted(&stored.nodes, &removed, &preview.operations))
                        .flatten()
                })
                .map_or((None, Vec::new()), |failure| {
                    (Some(failure.reason), failure.cause)
                });
            let cancelling = stored.summary.status == DeploymentStatus::Cancelling;
            // The Deployment's cancel stopped the operation the run failed on.
            let stopped_by_cancel = cancelling && named_cancel;
            // Each Service it left unfinished that the cancel stopped: every one when
            // it stopped the run, else those whose rows it stopped, and never one with
            // a row that failed on its own.
            let cancelled = if cancelling {
                projection
                    .failed_services
                    .iter()
                    .filter(|service| {
                        !failed_on_its_own(service)
                            && (stopped_by_cancel || stopped.contains(*service))
                    })
                    .cloned()
                    .collect()
            } else {
                std::collections::BTreeSet::new()
            };
            let saved = held_revision(tx, &stored)?;
            let applied = applied_state(tx, &stored.summary.environment_id, &saved)?;
            let outcomes = |projection| {
                node_outcomes(
                    &stored.nodes,
                    &saved,
                    &applied,
                    &Evidence::Executed {
                        preview,
                        projection,
                        removed: &removed,
                    },
                )
            };
            let nodes = keep_advanced(&stored.run.nodes, outcomes(&projection));
            // A node the cancel alone failed no longer fails once the Services it
            // stopped count as never attempted.
            let mut uncancelled = projection.clone();
            uncancelled
                .failed_services
                .retain(|service| !cancelled.contains(service));
            uncancelled.unattempted_services.extend(cancelled);
            let uncancelled = outcomes(&uncancelled);
            let cancelled = nodes
                .iter()
                .filter(|(id, status)| {
                    **status == NodeStatus::Failed
                        && uncancelled.get(*id) != Some(&NodeStatus::Failed)
                })
                .map(|(id, _)| id.clone())
                .collect();
            // A Deploy that left a Volume's data behind, or may have left a Service
            // it retires or removes running, didn't finish: a retry removes it.
            let unfinished = |(id, status): (&String, &NodeStatus)| {
                *status == NodeStatus::Failed
                    || (*status == NodeStatus::NotAttempted
                        && stored.nodes.iter().any(|node| {
                            node.id() == id
                                && (node.retires()
                                    || matches!(node, TargetNode::Service { .. })
                                        && !saved.services.iter().any(|kept| kept.id == *id))
                        }))
            };
            // A blocked Service was never attempted, whatever the rest did (bet 8).
            let status = if success && preview.blocked.is_empty() && !nodes.iter().any(unfinished) {
                End::Applied
            } else {
                End::Failed
            };
            let outcome = Outcome::Executed {
                summary: serde_json::to_value(projection.summary).expect("a summary is JSON"),
                reason,
                cause,
                services: projection.services.clone(),
                compensation: match &*outcome {
                    DeployOutcome::Failed {
                        failed: FailedOperation::Replacement { compensation, .. },
                        ..
                    } => compensation.lines().into_iter().map(String::from).collect(),
                    DeployOutcome::Failed { .. }
                    | DeployOutcome::Success { .. }
                    | DeployOutcome::Interrupted { .. } => Vec::new(),
                },
                cancelled: stopped_by_cancel && failed.is_empty(),
            };
            super::rows::settle(tx, &stored)?;
            stored.run.nodes = nodes;
            stored.run.cancelled = cancelled;
            stored.run.executed = Some(executed);
            if taken_over {
                // Another runner owns the Environment now: the run stays Unknown and
                // moves no Applied State. It ended when its runner says it stopped.
                let now = now();
                stored.standing = stored.standing.lose_runner(now);
                stored.standing.restamp(now);
                stored.summary.outcome = Some(outcome.clone());
                stored.run.outcome = Some(outcome);
                save(tx, &mut stored)?;
                return Ok(stored.summary);
            }
            finish(tx, stored, outcome, status)
        }
        RunEvidence::Confirmed(services) => {
            running(&stored)?;
            let (Some(preview), None) = (&stored.run.preview, &stored.run.outcome) else {
                return Err(error::conflict(
                    "Confirm Services between the Deploy Preview and the outcome",
                    json!({ "deployment": id }),
                ));
            };
            let mut confirmed = services;
            for node in &stored.nodes {
                if stored
                    .run
                    .nodes
                    .get(node.id())
                    .is_some_and(|status| status.advances())
                {
                    for runtime in node.runtimes() {
                        if !confirmed.contains(runtime) {
                            confirmed.push(runtime.clone());
                        }
                    }
                }
            }
            let saved = held_revision(tx, &stored)?;
            let applied = applied_state(tx, &stored.summary.environment_id, &saved)?;
            stored.run.nodes = keep_advanced(
                &stored.run.nodes,
                settled(&stored.nodes, &saved, &applied, preview, &confirmed),
            );
            // The seal's walk below waits for the end, so it runs once (#3780).
            advance(tx, &stored, false, false)?;
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Progress(rows) => {
            running(&stored)?;
            super::rows::record(tx, &stored, &rows)?;
            Ok(stored.summary)
        }
        RunEvidence::Built(receipts) => {
            running(&stored)?;
            for (service, receipt) in &receipts {
                save_receipt(tx, &stored.summary.environment_id, service, receipt)?;
            }
            Ok(stored.summary)
        }
        RunEvidence::Build(report) => {
            running(&stored)?;
            build::record(tx, &stored, &report)?;
            Ok(stored.summary)
        }
        RunEvidence::NotExecuted(Failure {
            reason,
            cause,
            same_again,
        }) => {
            let reason = reason.chars().take(500).collect();
            finish(
                tx,
                stored,
                Outcome::NotExecuted {
                    reason,
                    cause,
                    needs_upload: Vec::new(),
                    same_again,
                },
                End::Failed,
            )
        }
        RunEvidence::Ingress(ingress) => {
            running(&stored)?;
            stored.run.ingress = ingress;
            stored.summary.warnings = stored.run.summary_warnings();
            stored.summary.ingress.clone_from(&stored.run.ingress);
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
        RunEvidence::IngressUnchecked(cause) => {
            warn(tx, stored, DeploymentWarning::IngressUnchecked { cause })
        }
        RunEvidence::Warned(warning) => warn(tx, stored, DeploymentWarning::Deploy { warning }),
        RunEvidence::UploadNeeded(services) => {
            let names = services
                .iter()
                .map(|service| current_name(&stored.nodes, service))
                .collect::<Vec<_>>()
                .join(", ");
            finish(
                tx,
                stored,
                Outcome::NotExecuted {
                    reason: format!(
                        "{names} has no source to build: its last upload is gone and no \
                         image is left to reuse. Upload it again or add an image."
                    ),
                    cause: Vec::new(),
                    needs_upload: services,
                    same_again: false,
                },
                End::Failed,
            )
        }
        RunEvidence::Abandoned => {
            if stored.run.outcome.is_some() || stored.summary.status == DeploymentStatus::Unknown {
                return Ok(stored.summary);
            }
            running(&stored)?;
            if stored.run.preview.is_none() {
                let reason = "Its runner stopped before executing anything".to_owned();
                return finish(
                    tx,
                    stored,
                    Outcome::NotExecuted {
                        reason,
                        cause: Vec::new(),
                        needs_upload: Vec::new(),
                        same_again: false,
                    },
                    End::Failed,
                );
            }
            stored.standing = stored.standing.lose_runner(now());
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
    }
}

/// Add `warning` to a running Deployment once.
fn warn(
    tx: &mut dyn Tx,
    mut stored: Stored,
    warning: DeploymentWarning,
) -> Result<DeploymentSummary, RpcError> {
    running(&stored)?;
    if !stored.run.warnings.contains(&warning) {
        stored.run.warnings.push(warning);
    }
    stored.summary.warnings = stored.run.summary_warnings();
    save(tx, &mut stored)?;
    Ok(stored.summary)
}

/// Why a Deploy failed, for users: the failed operation's Service, by its current
/// name, and its error. None when it succeeded.
fn failure(
    outcome: &DeployOutcome<ExecutionError>,
    nodes: &[TargetNode],
    operations: &[ployz_core::OperationRow],
) -> Option<Failure> {
    let DeployOutcome::Failed { failed, .. } = outcome else {
        return None;
    };
    let (service, machine, error) = match failed {
        FailedOperation::Operation { operation, error } => {
            (operation.service_name(), operation.machine_id(), error)
        }
        FailedOperation::Replacement {
            operation, error, ..
        } => (Some(&operation.spec.name), operation.machine_id, error),
    };
    let Failure { reason, cause, .. } = Failure::on(error, operations, Some(machine));
    let reason = match service {
        Some(service) => format!("{}: {reason}", current_name(nodes, service)),
        None => reason,
    };
    Some(Failure {
        reason: reason.chars().take(500).collect(),
        cause,
        same_again: false,
    })
}

/// Why the first Service the planner blocked was never attempted.
fn blocked(nodes: &[TargetNode], blocked: &[ployz_core::BlockedService]) -> Option<Failure> {
    blocked.first().map(|entry| Failure {
        reason: format!(
            "{}: {}",
            current_name(nodes, &entry.service),
            entry.error.message
        )
        .chars()
        .take(500)
        .collect(),
        cause: entry.error.cause.clone(),
        same_again: false,
    })
}

/// Why a Deploy that succeeded left a removed Volume's data behind, in fixed words:
/// a provider's error can quote its stderr, so only the operator log carries it.
fn undeleted(
    nodes: &[TargetNode],
    removed: &[VolumeRemoval],
    operations: &[ployz_core::OperationRow],
) -> Option<Failure> {
    nodes.iter().find_map(|node| {
        let TargetNode::Volume {
            name,
            deletes: Some(deletes),
            ..
        } = node
        else {
            return None;
        };
        deletes.iter().find_map(|id| {
            let server = operations
                .iter()
                .find(|row| row.machine_id == id.machine_id)
                .and_then(|row| row.machine_name.as_ref())
                .map_or_else(|| "a Server".to_owned(), |name| format!("Server {name}"));
            let outcome = removed
                .iter()
                .find(|removal| removal.id == *id)
                .map(|removal| &removal.outcome);
            let why = match outcome {
                Some(VolumeRemovalOutcome::Removed) => return None,
                Some(VolumeRemovalOutcome::Failed { error }) => {
                    tracing::warn!(
                        volume = name.to_string().as_str(),
                        docker_volume = id.to_string().as_str(),
                        code = error.code.as_str(),
                        error = error.message.as_str(),
                        "couldn't delete a removed Volume's data"
                    );
                    match error.code {
                        ployz_core::RpcErrorCode::Unavailable => format!("{server} did not answer"),
                        ployz_core::RpcErrorCode::Conflict => {
                            format!("it is still in use on {server}")
                        }
                        ployz_core::RpcErrorCode::InvalidArgument
                        | ployz_core::RpcErrorCode::NotFound
                        | ployz_core::RpcErrorCode::Ambiguous
                        | ployz_core::RpcErrorCode::Unsupported
                        | ployz_core::RpcErrorCode::Internal
                        | ployz_core::RpcErrorCode::Unauthenticated
                        | ployz_core::RpcErrorCode::ConfirmationRequired
                        | ployz_core::RpcErrorCode::Unknown(_) => format!("{server} refused"),
                    }
                }
                Some(VolumeRemovalOutcome::Omitted) => format!("{server} did not answer"),
                None => "the Cluster could not be reached".to_owned(),
            };
            Some(Failure::from(format!(
                "{name}: deleting its data failed: {why}"
            )))
        })
    })
}

/// The current name of the target Service that lowers to runtime Service `runtime`.
fn current_name<'nodes>(nodes: &'nodes [TargetNode], runtime: &'nodes ServiceName) -> &'nodes str {
    nodes
        .iter()
        .find_map(|node| match node {
            TargetNode::Service { name, .. } if node.runtimes().contains(&runtime) => {
                Some(name.as_str())
            }
            TargetNode::Service { .. } | TargetNode::Volume { .. } | TargetNode::Config { .. } => {
                None
            }
        })
        .unwrap_or(runtime.as_str())
}

pub(super) fn finish(
    tx: &mut dyn Tx,
    stored: Stored,
    outcome: Outcome,
    status: End,
) -> Result<DeploymentSummary, RpcError> {
    match &stored.run.outcome {
        Some(recorded) if *recorded == outcome => return Ok(stored.summary),
        Some(_) => {
            return Err(error::conflict(
                "This Deployment already recorded a different outcome",
                json!({ "deployment": stored.summary.id }),
            ));
        }
        None => running(&stored)?,
    }
    end(tx, stored, outcome, status)
}

/// End `stored` with `outcome`, now: what it confirmed enters Applied State.
pub(super) fn end(
    tx: &mut dyn Tx,
    mut stored: Stored,
    outcome: Outcome,
    status: End,
) -> Result<DeploymentSummary, RpcError> {
    advance(tx, &stored, status == End::Applied, true)?;
    let status = match (stored.summary.status, status) {
        (DeploymentStatus::Cancelling, End::Failed) => End::Cancelled,
        _ => status,
    };
    stored.standing = stored.standing.end(status, now());
    stored.summary.outcome = Some(outcome.clone());
    stored.run.outcome = Some(outcome);
    save(tx, &mut stored)?;
    Ok(stored.summary)
}

/// Put each node `stored` confirmed into Applied State as its Saved revision has
/// it, by [`NodeStatus::applied`], a Volume at the size its Servers hold. Applying
/// one again changes nothing. The Environment's Branches then follow it.
/// With `seal_below`, the Follow also seals what Branches below inherited: only as the
/// Deployment ends, so once per Deployment (#3780).
fn advance(
    tx: &mut dyn Tx,
    stored: &Stored,
    succeeded: bool,
    seal_below: bool,
) -> Result<(), RpcError> {
    let advanced: Vec<&TargetNode> = stored
        .nodes
        .iter()
        .filter(|node| {
            stored
                .run
                .nodes
                .get(node.id())
                .is_some_and(|status| status.applied(succeeded))
        })
        .collect();
    if !advanced.is_empty() {
        let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
        let mut volumes = saved.volumes.clone();
        crate::volume::size::held(tx, &stored.summary.environment_id, &mut volumes)?;
        // What a removed node was, so its removal follows too (#3705).
        let before = applied_state(tx, &stored.summary.environment_id, &saved)?;
        let mut deployed = std::collections::BTreeSet::new();
        for node in advanced {
            let applied = node_in((&saved, &volumes), node);
            if let Some(lineage) = applied
                .or_else(|| node_in((&before, &before.volumes), node))
                .map(scope::Node::lineage)
            {
                deployed.insert(lineage.to_owned());
            }
            match applied {
                Some(applied) => applied::confirm(
                    tx,
                    stored.summary.id.as_str(),
                    node.id(),
                    applied.document().as_str(),
                    name_of(applied.node_type()).as_str(),
                )?,
                None => applied::remove_node(tx, &stored.summary.environment_id, node.id())?,
            }
        }
        crate::branch::deployed(tx, &stored.summary.environment_id, &saved, &deployed)?;
        crate::branch::follow(
            tx,
            &stored.summary.environment_id,
            &saved,
            &deployed,
            seal_below,
        )?;
    }
    Ok(())
}

/// `node` as `intent` holds it, its Volumes as `volumes` has them.
fn node_in<'intent>(
    (intent, volumes): (
        &'intent SavedEnvironmentIntent,
        &'intent [SavedVolumeIntent],
    ),
    node: &TargetNode,
) -> Option<scope::Node<'intent>> {
    match node {
        TargetNode::Service { .. } => intent
            .services
            .iter()
            .find(|service| service.id == node.id())
            .map(scope::Node::Service),
        TargetNode::Volume { .. } => volumes
            .iter()
            .find(|volume| volume.resource_id == node.id())
            .map(scope::Node::Volume),
        TargetNode::Config { .. } => intent
            .configs
            .iter()
            .find(|config| config.resource_id == node.id())
            .map(scope::Node::Config),
    }
}
