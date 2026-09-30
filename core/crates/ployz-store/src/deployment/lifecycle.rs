//! A Deployment's life: admitted, claimed, recorded, retried, started, cancelled.

use super::*;

/// Admit a frozen Deployment of Saved revision `saved`. A Deployment still queued is
/// superseded: the newest admission replaces the pending one.
pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    (id, services, upload, message): (
        &DeploymentId,
        &[ServiceName],
        Option<UploadedSource>,
        Option<String>,
    ),
    environment: &EnvironmentId,
    saved: Revision,
    frozen: &Frozen,
) -> Result<DeploymentSummary, RpcError> {
    let intent = saved_at(tx, environment, saved)?;
    crate::volume::fix_storage(tx, environment, &intent, &frozen.nodes)?;
    let environment_id = environment.as_str();
    // Without a new upload, Services without a source keep building from the latest one.
    let upload = match upload {
        Some(upload) => {
            upload.check()?;
            Some(upload)
        }
        None => match tx
            .query(
                "SELECT upload FROM config_deployment \
                 WHERE environment_id = ?1 AND upload <> 'null' ORDER BY number DESC LIMIT 1",
                &[environment_id.into()],
            )?
            .first()
        {
            Some(row) => row.json(0, "Deployment upload")?,
            None => None,
        },
    };
    // A Service with nothing to run would fail the whole Deploy on the Servers.
    if let (None, Some(name)) = (&upload, frozen.sourceless.first()) {
        return Err(error::invalid(
            format!("{name} has nothing to run yet: add an image or connect a repository"),
            json!({ "service": name }),
        ));
    }
    let message = message
        .map(|message| message.trim().to_owned())
        .filter(|message| !message.is_empty());
    if message
        .as_ref()
        .is_some_and(|message| message.chars().count() > MESSAGE_MAX)
    {
        return Err(error::invalid(
            format!("A Deployment message is at most {MESSAGE_MAX} characters"),
            json!({}),
        ));
    }
    let number = queue(tx, environment)?;
    let summary = DeploymentSummary {
        id: id.clone(),
        environment_id: environment.clone(),
        number,
        status: DeploymentStatus::Queued,
        saved,
        services: services.to_vec(),
        runner: None,
        upload,
        remove: saved == NOTHING,
        admitted_by: who.principal.clone(),
        admitted_at: now(),
        started_at: None,
        ended_at: None,
        message,
        in_flight: true,
    };
    tx.execute(
        "INSERT INTO config_deployment \
         (id, organization_id, environment_id, number, status, saved_revision, services, nodes, \
          namespace, run, credentials, upload, cluster_domain, admitted, admitted_by, message) \
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            environment_id.into(),
            i64::try_from(number)
                .map_err(|_| error::corrupt("Deployment number"))?
                .into(),
            revision_param(saved)?.into(),
            json_text(&summary.services).as_str().into(),
            json_text(&frozen.nodes).as_str().into(),
            frozen.namespace.as_str().into(),
            json_text(&Run::default()).as_str().into(),
            json_text(&frozen.credentials).as_str().into(),
            json_text(&summary.upload).as_str().into(),
            frozen.cluster_domain.as_ref().map(Hostname::as_str).into(),
            summary.admitted_at.into(),
            who.principal.as_ref().map(Principal::as_str).into(),
            summary.message.as_deref().into(),
        ],
    )?;
    Ok(summary)
}

/// How a removal of `environment` applies without a runner, if it can: nothing of
/// it ever ran on a Server, or Cloud counted no Server left to run it. Zero
/// enrolled Servers isn't runtime absence: that removal completes only in
/// configuration, and says so.
pub(crate) fn forgettable(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    trusted: &crate::Trusted,
) -> Result<Option<Outcome>, RpcError> {
    if crate::teardown::ran(tx, environment)?.is_none() {
        return Ok(Some(Outcome::NeverRan));
    }
    Ok(trusted.no_servers().then_some(Outcome::Forgotten))
}

/// Apply removal `id` without a runner, with `outcome` from [`forgettable`]. It
/// never started. Applied State lets go of every node, which Unknown reads.
pub(crate) fn forget(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    outcome: Outcome,
) -> Result<DeploymentSummary, RpcError> {
    let mut stored = locked(tx, id)?;
    stored.run.nodes = stored
        .nodes
        .iter()
        .map(|node| (node.id().to_owned(), NodeStatus::Unknown))
        .collect();
    tx.execute(
        "DELETE FROM config_applied WHERE environment_id = ?1",
        &[stored.summary.environment_id.as_str().into()],
    )?;
    end(tx, stored, outcome, DeploymentStatus::Applied)
}

/// Supersede `environment`'s queued Deployment, if any, and number the next one.
pub(super) fn queue(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<u64, RpcError> {
    tx.execute(
        "UPDATE config_deployment SET status = 'superseded' \
         WHERE environment_id = ?1 AND status = 'queued'",
        &[environment.as_str().into()],
    )?;
    let number = tx
        .query(
            "SELECT COALESCE(MAX(number), 0) FROM config_deployment WHERE environment_id = ?1",
            &[environment.as_str().into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment number"))?
        .int(0)?;
    u64::try_from(number + 1).map_err(|_| error::corrupt("Deployment number"))
}

/// Queue Deployment `id` shipping exactly what `source` froze: its Saved revision,
/// targets, Namespace, registry credentials and upload, whatever changed since. Only a Deployment that ended
/// without applying can be retried: failed, unknown or cancelled.
pub(crate) fn retry(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
    source: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, source)?;
    let stored = locked(tx, source)?;
    match stored.summary.status {
        DeploymentStatus::Failed | DeploymentStatus::Unknown | DeploymentStatus::Cancelled => {}
        DeploymentStatus::Applied => {
            return Err(error::conflict(
                "This Deployment applied, so there is nothing to retry",
                json!({ "deployment": source }),
            ));
        }
        DeploymentStatus::Queued | DeploymentStatus::Running | DeploymentStatus::Cancelling => {
            return Err(error::conflict(
                "This Deployment hasn't ended: cancel it or wait before retrying it",
                json!({ "deployment": source }),
            ));
        }
        DeploymentStatus::Superseded => return Err(superseded(source)),
    }
    if stored.summary.remove {
        let environment = scope::load_by_id(tx, &stored.summary.environment_id)?;
        crate::teardown::guard_removal(tx, &environment)?;
    }
    let number = queue(tx, &stored.summary.environment_id)?;
    let admitted_at = now();
    // Every frozen column comes from the source, so a retry never re-reads authored state.
    tx.execute(
        "INSERT INTO config_deployment \
         (id, organization_id, environment_id, number, status, saved_revision, services, nodes, \
          namespace, run, credentials, upload, cluster_domain, admitted, admitted_by, message) \
         SELECT ?1, organization_id, environment_id, ?2, 'queued', saved_revision, services, \
          nodes, namespace, ?3, credentials, upload, cluster_domain, ?5, ?6, message \
         FROM config_deployment WHERE id = ?4",
        &[
            id.as_str().into(),
            i64::try_from(number)
                .map_err(|_| error::corrupt("Deployment number"))?
                .into(),
            json_text(&Run::default()).as_str().into(),
            source.as_str().into(),
            admitted_at.into(),
            who.principal.as_ref().map(Principal::as_str).into(),
        ],
    )?;
    // A retry builds the commits its source pinned.
    tx.execute(
        "INSERT INTO config_build \
         (deployment_id, service, organization_id, commit_sha, status, message, log) \
         SELECT ?1, service, organization_id, commit_sha, 'pending', NULL, '' \
         FROM config_build WHERE deployment_id = ?2",
        &[id.as_str().into(), source.as_str().into()],
    )?;
    Ok(DeploymentSummary {
        id: id.clone(),
        number,
        status: DeploymentStatus::Queued,
        runner: None,
        admitted_by: who.principal.clone(),
        admitted_at,
        started_at: None,
        ended_at: None,
        in_flight: true,
        ..stored.summary
    })
}

/// A queued Deployment of `who`'s Organization, checked that a runner may still
/// claim it. Nothing changes: the caller hands it to a runner.
pub(crate) fn start(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, id)?;
    let stored = locked(tx, id)?;
    match stored.summary.status {
        DeploymentStatus::Queued => Ok(stored.summary),
        DeploymentStatus::Running | DeploymentStatus::Cancelling => Err(error::conflict(
            "This Deployment is already running",
            json!({ "deployment": id }),
        )),
        DeploymentStatus::Superseded => Err(superseded(id)),
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => Err(ended(id)),
    }
}

/// Bind a queued Deployment to `runner` and hand over its frozen Deploy Intent.
/// Claiming again as the same runner returns the same until it records a Deploy
/// Preview; after that it may have executed, so a second claim leaves the Deployment
/// `unknown` and refuses: nothing replays silently. A Deployment another runner still
/// holds without an outcome reads `unknown` from here on.
///
/// The outer error rolls back; the inner one is a refusal that keeps what it recorded.
pub(crate) fn claim(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
    sealing: &SealingKey,
) -> Result<Result<Claimed, RpcError>, RpcError> {
    let mut stored = locked(tx, id)?;
    match stored.summary.status {
        DeploymentStatus::Running | DeploymentStatus::Cancelling
            if stored.summary.runner.as_ref() == Some(runner) =>
        {
            if stored.run.preview.is_some() {
                stored.summary.status = DeploymentStatus::Unknown;
                save(tx, &mut stored)?;
                return Ok(Err(error::conflict(
                    "This Deployment's runner lost track of it after preparing it, so what \
                     ran is unknown. Start a new Deployment",
                    json!({ "deployment": id }),
                )));
            }
            stored.lease = now() + LEASE;
            save(tx, &mut stored)?;
        }
        DeploymentStatus::Running | DeploymentStatus::Cancelling => {
            return Ok(Err(owned_elsewhere(id)));
        }
        DeploymentStatus::Superseded => return Ok(Err(superseded(id))),
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => return Ok(Err(ended(id))),
        DeploymentStatus::Queued => {
            tx.execute(
                "UPDATE config_deployment SET status = 'unknown' \
                 WHERE environment_id = ?1 AND status IN ('running', 'cancelling')",
                &[stored.summary.environment_id.as_str().into()],
            )?;
            let started = now();
            stored.summary.status = DeploymentStatus::Running;
            stored.summary.runner = Some(runner.clone());
            stored.summary.started_at = Some(started);
            stored.lease = started + LEASE;
            save(tx, &mut stored)?;
        }
    }
    let sources = build::sources_of(tx, &stored)?;
    let uploads = build::uploads_of(tx, &stored)?;
    let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    let branch = crate::branch::lowering(tx, &stored.summary.environment_id, &saved)?;
    let (input, mut intent) = lower(
        &stored.summary.environment_id,
        &saved,
        &stored.summary.services,
        (stored.namespace, stored.cluster_domain.as_ref()),
        &branch,
        Some(sealing),
    )?;
    let credentials = tx.query(
        "SELECT credentials FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    let credentials = credentials
        .first()
        .ok_or_else(|| missing(id))?
        .text(0)
        .and_then(|text| {
            serde_json::from_str(text).map_err(|_| error::corrupt("Deployment credentials"))
        })?;
    intent.registry_auth = registry::unseal(credentials, sealing)?;
    let receipts = receipts(tx, &stored.summary.environment_id)?;
    let deletes = stored
        .nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Volume { deletes, .. } => deletes.clone(),
            TargetNode::Service { .. } => None,
        })
        .flatten()
        .collect();
    let organization = build::organization(tx, id)?;
    let build_order = crate::builders::order(tx, organization.as_str())?;
    Ok(Ok(Claimed {
        deployment: stored.summary,
        intent,
        input,
        receipts,
        sources,
        deletes,
        uploads,
        build_order,
    }))
}

/// Every build receipt of each Service of `environment`'s Project, by runtime name:
/// the Environment's own first, then other Environments' (a Branch copy, a saved
/// change). Preparation reuses the first whose fingerprint matches.
pub(crate) fn receipts(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeMap<ServiceName, Vec<Value>>, RpcError> {
    let mut receipts = BTreeMap::<_, Vec<_>>::new();
    for row in tx.query(
        "SELECT r.service, r.receipt FROM config_build_receipt r \
         JOIN config_environment e ON e.id = r.environment_id \
         WHERE e.project_id = (SELECT project_id FROM config_environment WHERE id = ?1) \
         ORDER BY CASE WHEN r.environment_id = ?1 THEN 0 ELSE 1 END, r.environment_id",
        &[environment.as_str().into()],
    )? {
        let service = row.parse::<ServiceName>(0, "receipt")?;
        receipts
            .entry(service)
            .or_default()
            .push(row.json(1, "receipt")?);
    }
    Ok(receipts)
}

/// Replace `service`'s latest build receipt in `environment`.
pub(crate) fn save_receipt(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service: &ServiceName,
    receipt: &Value,
) -> Result<(), RpcError> {
    if !receipt.is_object() {
        return Err(invalid_evidence("build receipt"));
    }
    tx.execute(
        "INSERT INTO config_build_receipt (environment_id, service, organization_id, receipt) \
         SELECT id, ?2, organization_id, ?3 FROM config_environment WHERE id = ?1 \
         ON CONFLICT (environment_id, service) DO UPDATE SET receipt = excluded.receipt",
        &[
            environment.as_str().into(),
            service.as_str().into(),
            json_text(receipt).as_str().into(),
        ],
    )?;
    Ok(())
}

/// The lowering input of `stored`, secrets unsealed: what a build of it takes.
/// In-process only.
pub(crate) fn input(
    tx: &mut dyn Tx,
    stored: &Stored,
    sealing: &SealingKey,
) -> Result<Value, RpcError> {
    let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    let branch = crate::branch::lowering(tx, &stored.summary.environment_id, &saved)?;
    let (input, _) = lower(
        &stored.summary.environment_id,
        &saved,
        &stored.summary.services,
        (stored.namespace.clone(), stored.cluster_domain.as_ref()),
        &branch,
        Some(sealing),
    )?;
    Ok(input)
}

/// Cancel a Deployment of `who`'s Organization. A queued one never runs; a running
/// one reads `cancelling` until its runner stops it and records what ran. Cancelling
/// again changes nothing.
pub(crate) fn cancel(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, id)?;
    let mut stored = locked(tx, id)?;
    stored.summary.status = match stored.summary.status {
        DeploymentStatus::Queued => {
            stored.summary.ended_at = Some(now());
            DeploymentStatus::Cancelled
        }
        DeploymentStatus::Running => DeploymentStatus::Cancelling,
        DeploymentStatus::Cancelling | DeploymentStatus::Cancelled => return Ok(stored.summary),
        DeploymentStatus::Superseded
        | DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown => return Err(ended(id)),
    };
    save(tx, &mut stored)?;
    Ok(stored.summary)
}

/// Record `runner`'s evidence. Only the runner that claimed the Deployment may, and
/// recording the same evidence again changes nothing.
pub(crate) fn record(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
    evidence: RunEvidence,
) -> Result<DeploymentSummary, RpcError> {
    let mut stored = locked(tx, id)?;
    if stored.summary.runner.as_ref() != Some(runner) {
        return Err(owned_elsewhere(id));
    }
    // Any record while it runs shows the runner is still there.
    if matches!(
        stored.summary.status,
        DeploymentStatus::Running | DeploymentStatus::Cancelling
    ) {
        stored.lease = now() + LEASE;
    }
    match evidence {
        RunEvidence::Alive => {
            running(&stored)?;
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Prepared(preview) => {
            let parsed = serde_json::to_value(preview)
                .ok()
                .and_then(|preview| parse_runtime_preview(preview).ok())
                .ok_or_else(|| invalid_evidence("Deploy Preview"))?;
            let preview = serde_json::to_value(&parsed).expect("a Deploy Preview is JSON");
            match &stored.run.preview {
                Some(recorded) if *recorded == preview => return Ok(stored.summary),
                Some(_) => {
                    return Err(error::conflict(
                        "This Deployment already recorded a different Deploy Preview",
                        json!({ "deployment": id }),
                    ));
                }
                None => {}
            }
            running(&stored)?;
            let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
            let applied = applied_state(tx, &stored.summary.environment_id, &saved)?;
            // What the preview already settles shows while it runs; the rest reads
            // as Pending until what executing it did replaces these.
            stored.run.nodes = settled(&stored.nodes, &saved, &applied, &parsed, &[]);
            stored.run.preview = Some(preview);
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Executed { outcome, removed } => {
            // A replay must carry the same evidence: Applied State moved since, so
            // recomputing its Node Outcomes would not tell.
            let executed = crate::removal::short_digest(
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
            let Some(preview) = stored.run.preview.clone() else {
                return Err(error::conflict(
                    "Record the Deploy Preview before what executing it did",
                    json!({ "deployment": id }),
                ));
            };
            let success = matches!(*outcome, DeployOutcome::Success { .. });
            let reason = failure(&outcome, &stored.nodes);
            let projection =
                project_runtime_outcome(preview, json!({ "version": 1, "outcome": outcome }))
                    .map_err(|_| invalid_evidence("Deploy Outcome"))?;
            let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
            let applied = applied_state(tx, &stored.summary.environment_id, &saved)?;
            let nodes = node_outcomes(
                &stored.nodes,
                &saved,
                &applied,
                &Evidence::Executed {
                    projection: &projection,
                    removed: &removed,
                },
            );
            // A Deploy that left a Volume's data behind didn't finish: a retry removes it.
            let status = if success && nodes.values().all(|status| *status != NodeStatus::Failed) {
                DeploymentStatus::Applied
            } else {
                DeploymentStatus::Failed
            };
            let outcome = Outcome::Executed {
                summary: serde_json::to_value(projection.summary).expect("a summary is JSON"),
                reason,
            };
            stored.run.nodes = nodes;
            stored.run.executed = Some(executed);
            finish(tx, stored, outcome, status)
        }
        RunEvidence::Confirmed(services) => {
            running(&stored)?;
            if stored.run.preview.is_none() || stored.run.outcome.is_some() {
                return Err(error::conflict(
                    "Confirm Services between the Deploy Preview and the outcome",
                    json!({ "deployment": id }),
                ));
            }
            let preview: DeployPreview = stored
                .run
                .preview
                .clone()
                .and_then(|preview| serde_json::from_value(preview).ok())
                .ok_or_else(|| error::corrupt("Deploy Preview"))?;
            // Everything confirmed so far, and these now.
            let mut confirmed = services;
            for node in &stored.nodes {
                if let TargetNode::Service { runtime, .. } = node
                    && stored
                        .run
                        .nodes
                        .get(node.id())
                        .is_some_and(|status| status.advances())
                    && !confirmed.contains(runtime)
                {
                    confirmed.push(runtime.clone());
                }
            }
            let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
            let applied = applied_state(tx, &stored.summary.environment_id, &saved)?;
            stored.run.nodes = settled(&stored.nodes, &saved, &applied, &preview, &confirmed);
            advance(tx, &stored)?;
            save(tx, &mut stored)?;
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
        RunEvidence::NotExecuted(reason) => {
            let reason = reason.chars().take(500).collect();
            finish(
                tx,
                stored,
                Outcome::NotExecuted {
                    reason,
                    needs_upload: Vec::new(),
                },
                DeploymentStatus::Failed,
            )
        }
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
                    needs_upload: services,
                },
                DeploymentStatus::Failed,
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
                        needs_upload: Vec::new(),
                    },
                    DeploymentStatus::Failed,
                );
            }
            stored.summary.status = DeploymentStatus::Unknown;
            save(tx, &mut stored)?;
            Ok(stored.summary)
        }
    }
}

/// Why a Deploy failed, for users: the failed operation's Service, by its current
/// name, and its error. None when it succeeded.
fn failure(outcome: &DeployOutcome<ExecutionError>, nodes: &[TargetNode]) -> Option<String> {
    let DeployOutcome::Failed { failed, .. } = outcome else {
        return None;
    };
    let (service, error) = match failed {
        FailedOperation::Operation { operation, error } => (operation.service_name(), error),
        FailedOperation::Replacement {
            operation, error, ..
        } => (Some(&operation.spec.name), error),
    };
    let reason = match service {
        Some(service) => format!("{}: {error}", current_name(nodes, service)),
        None => error.to_string(),
    };
    Some(reason.chars().take(500).collect())
}

/// The current name of the target Service that lowers to runtime Service `runtime`.
fn current_name<'nodes>(nodes: &'nodes [TargetNode], runtime: &'nodes ServiceName) -> &'nodes str {
    nodes
        .iter()
        .find_map(|node| match node {
            TargetNode::Service {
                name, runtime: of, ..
            } if of == runtime => Some(name.as_str()),
            TargetNode::Service { .. } | TargetNode::Volume { .. } => None,
        })
        .unwrap_or(runtime.as_str())
}

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
        projection: &'run RuntimeOutcomeProjection,
        removed: &'run [VolumeRemoval],
    },
}

/// Each target node's Node Outcome from `evidence`. A Service the preview plans
/// nothing for is Unchanged; one it plans for is Pending until confirmed or executed, then all
/// completed is Deployed (Removed once it left Saved State), some ran is Failed and
/// none ran is Not attempted. A kept Volume follows the targeted Services mounting
/// it: Deployed once one of them is. It is Unchanged when Applied State already
/// holds it as saved. One no targeted Service mounts is Not attempted when new
/// (nothing creates its storage), and a held one is Deployed only by a Deploy that
/// succeeded. A removed
/// Volume is Removed once the Deploy succeeded and every Docker Volume it deletes is
/// gone; Failed when one wasn't deleted, and Not attempted when the Deploy failed first.
/// While it runs, a Deployment stores only what is settled; its Pending nodes matter
/// only to the Volumes they mount.
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
    let removed_volume = |deletes: &[DockerVolumeId]| match evidence {
        Evidence::Planned { .. } => NodeStatus::Pending,
        Evidence::Executed {
            projection,
            removed,
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
        if mounting.contains(&NodeStatus::Failed) {
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
    nodes
        .iter()
        .map(|node| {
            let status = match node {
                TargetNode::Service { id, runtime, .. } => service(
                    runtime,
                    saved.services.iter().any(|kept| kept.id == id.as_str()),
                ),
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
            };
            (node.id().to_owned(), status)
        })
        .collect()
}

pub(super) fn finish(
    tx: &mut dyn Tx,
    stored: Stored,
    outcome: Outcome,
    status: DeploymentStatus,
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

/// End `stored` with `outcome`: what it confirmed enters Applied State.
fn end(
    tx: &mut dyn Tx,
    mut stored: Stored,
    outcome: Outcome,
    status: DeploymentStatus,
) -> Result<DeploymentSummary, RpcError> {
    advance(tx, &stored)?;
    // A cancelled Deployment that stopped short reads cancelled, not failed.
    stored.summary.status = match (stored.summary.status, status) {
        (DeploymentStatus::Cancelling, DeploymentStatus::Failed) => DeploymentStatus::Cancelled,
        _ => status,
    };
    stored.summary.ended_at = Some(now());
    stored.run.outcome = Some(outcome);
    save(tx, &mut stored)?;
    Ok(stored.summary)
}

/// Put each node `stored` confirmed (Deployed or Removed) into Applied State as
/// its Saved revision has it. Applying one again changes nothing.
fn advance(tx: &mut dyn Tx, stored: &Stored) -> Result<(), RpcError> {
    let advanced: Vec<&TargetNode> = stored
        .nodes
        .iter()
        .filter(|node| {
            stored
                .run
                .nodes
                .get(node.id())
                .is_some_and(|status| status.advances())
        })
        .collect();
    if !advanced.is_empty() {
        let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
        for node in advanced {
            let applied = match node {
                TargetNode::Service { .. } => saved
                    .services
                    .iter()
                    .find(|service| service.id == node.id())
                    .map(scope::Node::Service),
                TargetNode::Volume { .. } => saved
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == node.id())
                    .map(scope::Node::Volume),
            };
            match applied {
                Some(applied) => tx.execute(
                    "INSERT INTO config_applied \
                     (environment_id, node_id, organization_id, deployment_id, node, node_type) \
                     SELECT environment_id, ?2, organization_id, id, ?3, ?4 \
                     FROM config_deployment WHERE id = ?1 \
                     ON CONFLICT (environment_id, node_id) \
                     DO UPDATE SET deployment_id = excluded.deployment_id, node = excluded.node",
                    &[
                        stored.summary.id.as_str().into(),
                        node.id().into(),
                        applied.document().as_str().into(),
                        name_of(applied.node_type()).as_str().into(),
                    ],
                )?,
                None => tx.execute(
                    "DELETE FROM config_applied WHERE environment_id = ?1 AND node_id = ?2",
                    &[
                        stored.summary.environment_id.as_str().into(),
                        node.id().into(),
                    ],
                )?,
            };
        }
    }
    Ok(())
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
fn settled(
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
