//! A Deployment's life: admitted, claimed, recorded, retried, started, cancelled.

use super::*;

/// Which Deployment an admission queues.
#[derive(Clone, Copy)]
pub(crate) enum Admission<'a> {
    /// A new one, `id`, numbered next, admitted by `push` if a push did. It
    /// supersedes the queued one and every one waiting for CI.
    New {
        id: &'a DeploymentId,
        push: Option<&'a GitPush>,
    },
    /// This one, waiting for CI, in place: same push, numbered next as it queues.
    /// It supersedes only the queued one.
    Promote(&'a DeploymentId),
}

impl Admission<'_> {
    fn id(self) -> DeploymentId {
        match self {
            Self::New { id, .. } | Self::Promote(id) => id.clone(),
        }
    }
}

/// Admit a frozen Deployment of Saved revision `saved`. A Deployment still queued is
/// superseded: the newest admission replaces the pending one and takes over the
/// Services it would have torn down.
pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    (admission, services, upload, message): (
        Admission<'_>,
        &[ServiceName],
        Option<UploadedSource>,
        Option<String>,
    ),
    environment: &EnvironmentId,
    saved: Revision,
    frozen: &Frozen,
) -> Result<DeploymentSummary, RpcError> {
    let id = &admission.id();
    let (held, push) = match admission {
        Admission::New { push, .. } => (false, push.cloned()),
        Admission::Promote(id) => {
            let hold = load(tx, id)?.summary;
            if hold.status != DeploymentStatus::WaitingForCi {
                return Err(error::conflict(
                    "This Deployment no longer waits for CI",
                    json!({ "deployment": id }),
                ));
            }
            (true, hold.push)
        }
    };
    let intent = saved_at(tx, environment, saved)?;
    crate::volume::check_admitted(tx, environment, &intent)?;
    let environment_id = environment.as_str();
    // Without a new upload, each target Service without a source builds from the upload
    // it last built from.
    let (upload, service_uploads) =
        super::uploads::admitted(tx, environment, &intent, frozen, upload)?;
    // A Service with nothing to run would fail the whole Deploy on the Servers.
    if let (None, Some(name)) = (&upload, frozen.sourceless.first()) {
        return Err(error::invalid(
            format!(
                "{name} has nothing to run yet: add an image, connect a repository, or run ployz up in its directory"
            ),
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
    let full = frozen.full;
    let mut nodes = frozen.nodes.clone();
    inherit(tx, environment, (&intent, &intent), &mut nodes)?;
    let mut targets = services.to_vec();
    for node in nodes.iter().filter(|node| !full && node.retires()) {
        if let TargetNode::Service { name, .. } = node
            && !targets.contains(name)
        {
            targets.push(name.clone());
        }
    }
    let admitted_at = now();
    let run = Run {
        prune: frozen.prune,
        ..Run::default()
    };
    let (targets_text, nodes_text, run_text, credentials_text) = (
        json_text(&targets),
        json_text(&nodes),
        json_text(&run),
        json_text(&frozen.credentials),
    );
    let upload_text = upload.as_ref().map(json_text);
    let service_uploads_text = service_uploads.as_ref().map(json_text);
    let number = queue(tx, environment, held, |tx, number| {
        let frozen_columns: [crate::storage::Param<'_>; 12] = [
            revision_param(saved)?.into(),
            targets_text.as_str().into(),
            nodes_text.as_str().into(),
            frozen.namespace.as_str().into(),
            run_text.as_str().into(),
            credentials_text.as_str().into(),
            upload_text.as_deref().into(),
            frozen.cluster_domain.as_ref().map(Hostname::as_str).into(),
            admitted_at.into(),
            who.principal.as_ref().map(Principal::as_str).into(),
            message.as_deref().into(),
            service_uploads_text.as_deref().into(),
        ];
        let [
            saved,
            targets,
            nodes,
            namespace,
            run,
            credentials,
            upload,
            cluster,
            at,
            by,
            message,
            service_uploads,
        ] = frozen_columns;
        let number = i64::try_from(number).map_err(|_| error::corrupt("Deployment number"))?;
        // Number and list position are both one past the last, taken at the same
        // moment under the lock: among numbered Deployments the list runs in number
        // order. A promoted hold moves to the top of the list for it.
        let listed = next_listed(tx, environment)?;
        if held {
            return tx
                .execute(
                    "UPDATE config_deployment SET status = 'queued', number = ?13, \
                     listed = ?14, \
                     saved_revision = ?2, services = ?3, nodes = ?4, namespace = ?5, run = ?6, \
                     credentials = ?7, upload = ?8, cluster_domain = ?9, admitted = ?10, \
                     admitted_by = ?11, message = ?12, service_uploads = ?15 \
                     WHERE id = ?1 AND status = 'waiting_for_ci'",
                    &[
                        id.as_str().into(),
                        saved,
                        targets,
                        nodes,
                        namespace,
                        run,
                        credentials,
                        upload,
                        cluster,
                        at,
                        by,
                        message,
                        number.into(),
                        listed.into(),
                        service_uploads,
                    ],
                )
                .map(|_| ());
        }
        tx.execute(
            "INSERT INTO config_deployment \
             (id, organization_id, environment_id, number, status, saved_revision, services, \
              nodes, namespace, run, credentials, upload, cluster_domain, admitted, admitted_by, \
              message, push_repository_id, push_branch, push_head, listed, service_uploads) \
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
              ?16, ?17, ?18, ?19, ?20)",
            &[
                id.as_str().into(),
                who.organization.as_str().into(),
                environment_id.into(),
                number.into(),
                saved,
                targets,
                nodes,
                namespace,
                run,
                credentials,
                upload,
                cluster,
                at,
                by,
                message,
                push.as_ref()
                    .and_then(|push| i64::try_from(push.repository_id.get()).ok())
                    .into(),
                push.as_ref().map(|push| push.branch.as_str()).into(),
                push.as_ref().map(|push| push.head.as_str()).into(),
                listed.into(),
                service_uploads,
            ],
        )
        .map(|_| ())
    })?;
    let summary = DeploymentSummary {
        id: id.clone(),
        environment_id: environment.clone(),
        number: Some(number),
        status: DeploymentStatus::Queued,
        saved,
        services: targets,
        ships: shipped(&nodes),
        runner: None,
        upload,
        chooses_upload_at_start: super::chooses_upload_at_start(
            DeploymentStatus::Queued,
            service_uploads.as_ref(),
        ),
        preparing: false,
        remove: saved == NOTHING,
        prune: frozen.prune,
        admitted_by: who.principal.clone(),
        admitted_at,
        started_at: None,
        ended_at: None,
        message,
        push,
        ci: None,
        held: false,
        in_flight: DeploymentStatus::Queued.in_flight(),
        outcome: None,
        warnings: Vec::new(),
        ingress: Vec::new(),
    };
    Ok(summary)
}

/// Record that the user Cloud authenticated admitted Deployment `id`, by the
/// immutable id that makes it their own when they cancel it ([`admitter`]).
pub(crate) fn admitted_by_user(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    trusted: &crate::Trusted,
) -> Result<(), RpcError> {
    let Some(user) = &trusted.user else {
        return Ok(());
    };
    tx.execute(
        "UPDATE config_deployment SET admitted_by_user = ?2 WHERE id = ?1",
        &[id.as_str().into(), user.as_str().into()],
    )
    .map(drop)
}

/// Who admitted Deployment `id`, by the immutable id Cloud authenticated: none for
/// Cloud's own automation and for one an older Cloud admitted.
pub(crate) fn admitter(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Option<String>, RpcError> {
    let rows = tx.query(
        "SELECT admitted_by_user FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(row.optional_text(0)?.map(str::to_owned))
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
    if crate::removal::ran(tx, environment)?.is_none() {
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

/// Add to `nodes` the teardown of each Service `environment`'s queued Deployment
/// retires and Saved revision `saved` still lacks, but none of the runtime Services
/// `saved` runs: superseding it must not leave them running. Nor one the revision
/// `shipped` holds or runs: a retry of that revision runs it again. A node of that
/// Service becomes its teardown, so the next Deployment to supersede this one takes
/// it over.
/// Each Service it adds takes along the removed Configs that leave with it:
/// [`unmounted`] from `shipped`, the revision these nodes ship, over what may run
/// once the Deployments in flight end.
fn inherit(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    (saved, shipped): (&SavedEnvironmentIntent, &SavedEnvironmentIntent),
    nodes: &mut Vec<TargetNode>,
) -> Result<(), RpcError> {
    let given = nodes.len();
    let holds = |id: &str| {
        [saved, shipped]
            .iter()
            .flat_map(|intent| &intent.services)
            .any(|service| service.id == id)
    };
    let runs = |runtime: &ServiceName| {
        [saved, shipped]
            .iter()
            .flat_map(|intent| &intent.services)
            .any(|service| service.config.private_dns == *runtime)
    };
    let rows = tx.query(
        "SELECT nodes FROM config_deployment WHERE environment_id = ?1 AND status = 'queued'",
        &[environment.as_str().into()],
    )?;
    for row in &rows {
        let queued: Vec<TargetNode> = row.json(0, "Deployment")?;
        for node in queued {
            let TargetNode::Service {
                id, name, removals, ..
            } = node
            else {
                continue;
            };
            if holds(id.as_str()) {
                continue;
            }
            let runtimes = ServiceRemovals::from_names(
                removals.iter().filter(|runtime| !runs(runtime)).cloned(),
            );
            if runtimes.is_empty() {
                continue;
            }
            let retire = TargetNode::retire(&Retire { id, name, runtimes })?;
            match nodes.iter_mut().find(|ours| ours.id() == retire.id()) {
                Some(TargetNode::Service { removals: ours, .. }) if !ours.is_empty() => {
                    ours.extend(retire.runtimes().into_iter().cloned());
                }
                Some(ours @ TargetNode::Service { .. }) => *ours = retire,
                Some(_) => {}
                None => nodes.push(retire),
            }
        }
    }
    if nodes.len() > given {
        let whole = crate::scope::load_by_id(tx, environment)?;
        let head = head(tx, &whole)?;
        let unmounted = unmounted(shipped, &Landing::of(&head), nodes)?;
        nodes.extend(unmounted);
    }
    Ok(())
}

/// Queue a Deployment of `environment`, written by `insert` with the next number,
/// superseding the queued one, if any. `held` queues the Deployment waiting for CI in
/// place; otherwise a new one queues, superseding every one waiting for CI too. The
/// superseded one leaves flight, so the sealed values only it kept in History go once
/// the new one holds its revision.
pub(super) fn queue(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    held: bool,
    insert: impl FnOnce(&mut dyn Tx, u64) -> Result<(), RpcError>,
) -> Result<u64, RpcError> {
    let superseded = tx.execute(
        "UPDATE config_deployment SET status = 'superseded' \
         WHERE environment_id = ?1 AND status = 'queued'",
        &[environment.as_str().into()],
    )?;
    if !held {
        tx.execute(
            "UPDATE config_deployment SET status = 'superseded', ended = ?2 \
             WHERE environment_id = ?1 AND status = 'waiting_for_ci'",
            &[environment.as_str().into(), now().into()],
        )?;
    }
    let number = next_number(tx, environment)?;
    insert(tx, number)?;
    if superseded > 0 {
        crate::review::scrub_sealed(tx, environment)?;
    }
    Ok(number)
}

/// The list position the next Deployment of `environment` takes: one past the
/// last. Read under the Environment's lock, as every insert of one is, so two never
/// take the same. A hold takes one at insert and another as it promotes, so a
/// position only ever moves up: a page cursor past it can skip a hold promoted
/// since (it heads the list on the next read) but never reads a row twice.
pub(super) fn next_listed(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<i64, RpcError> {
    tx.query(
        "SELECT COALESCE(MAX(listed), 0) + 1 FROM config_deployment WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?
    .first()
    .ok_or_else(|| error::corrupt("Deployment list position"))?
    .int(0)
}

fn next_number(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<u64, RpcError> {
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

/// Hold `push` of `environment` until its commit's CI passes: a Deployment waiting
/// for CI of `services` (none: every one) at Saved revision `saved`, unnumbered until
/// it queues. `ids` are the same Services by ID, which promotion selects by: a name may change
/// while it waits. It freezes, builds and supersedes nothing; passing CI admits it in
/// place. A hold of the same head is kept as is, one of an older head of the branch
/// is superseded. The caller holds the Environment's lock.
pub(crate) fn hold(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentSummary,
    push: &GitPush,
    services: &[ServiceName],
    ids: &[String],
    saved: Revision,
) -> Result<(), RpcError> {
    let id = &environment.id;
    let key: [crate::storage::Param<'_>; 3] = [
        id.as_str().into(),
        push.repository_id.into(),
        push.branch.as_str().into(),
    ];
    let held = tx.query(
        "SELECT push_head FROM config_deployment WHERE environment_id = ?1 \
         AND push_repository_id = ?2 AND push_branch = ?3 AND status = 'waiting_for_ci'",
        &key,
    )?;
    if held
        .first()
        .map(|row| row.text(0))
        .transpose()?
        .is_some_and(|head| head == push.head.as_str())
    {
        return Ok(());
    }
    let [environment_param, repository_param, branch_param] = key;
    tx.execute(
        "UPDATE config_deployment SET status = 'superseded', ended = ?4 WHERE environment_id = ?1 \
         AND push_repository_id = ?2 AND push_branch = ?3 AND status = 'waiting_for_ci'",
        &[
            environment_param,
            repository_param,
            branch_param,
            now().into(),
        ],
    )?;
    let namespace = namespace(tx, who, environment, false)?;
    let deployment = DeploymentId::parse(uuid::Uuid::new_v4().to_string())?;
    let listed = next_listed(tx, id)?;
    tx.execute(
        "INSERT INTO config_deployment \
         (id, organization_id, environment_id, status, saved_revision, services, nodes, \
          namespace, run, credentials, admitted, push_repository_id, push_branch, push_head, \
          push_services, listed) \
         VALUES (?1, ?2, ?3, 'waiting_for_ci', ?4, ?5, '[]', ?6, ?7, '{}', ?8, ?9, ?10, ?11, \
          ?12, ?13)",
        &[
            deployment.as_str().into(),
            who.organization.as_str().into(),
            id.as_str().into(),
            revision_param(saved)?.into(),
            json_text(&services).as_str().into(),
            namespace.as_str().into(),
            json_text(&Run::default()).as_str().into(),
            now().into(),
            push.repository_id.into(),
            push.branch.as_str().into(),
            push.head.as_str().into(),
            json_text(&ids).as_str().into(),
            listed.into(),
        ],
    )?;
    Ok(())
}

/// Supersede every Deployment of `who`'s Organization waiting for CI of a push to
/// `branch` of `repository_id` other than `head`: a newer head of the branch never
/// lets an older one ship.
pub(crate) fn supersede_held(
    tx: &mut dyn Tx,
    who: &Actor,
    (repository_id, branch): (RepositoryId, &BranchName),
    head: Option<&CommitSha>,
) -> Result<(), RpcError> {
    let head = head.map(CommitSha::as_str).unwrap_or_default();
    let params: [crate::storage::Param<'_>; 4] = [
        who.organization.as_str().into(),
        repository_id.into(),
        branch.as_str().into(),
        head.into(),
    ];
    let condition = "organization_id = ?1 AND push_repository_id = ?2 AND push_branch = ?3 \
                     AND push_head <> ?4 AND status = 'waiting_for_ci'";
    let environments = tx
        .query(
            &format!("SELECT environment_id FROM config_deployment WHERE {condition}"),
            &params,
        )?
        .iter()
        .map(|row| row.parse::<EnvironmentId>(0, "Environment ID"))
        .collect::<Result<Vec<_>, _>>()?;
    if environments.is_empty() {
        return Ok(());
    }
    scope::lock_all(tx, environments)?;
    let [organization, repository, branch, head] = params;
    tx.execute(
        &format!(
            "UPDATE config_deployment SET status = 'superseded', ended = ?5 WHERE {condition}"
        ),
        &[organization, repository, branch, head, now().into()],
    )?;
    Ok(())
}

/// Cancel every Deployment of `environment` waiting for CI: it is going away. The
/// caller holds its lock.
pub(crate) fn cancel_held_in(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_deployment SET status = 'cancelled', ended = ?2 \
         WHERE environment_id = ?1 AND status = 'waiting_for_ci'",
        &[environment.as_str().into(), now().into()],
    )?;
    Ok(())
}

/// Cancel Deployment `id` waiting for CI without running it: its CI passed but it
/// can't deploy, for `reason`, which users read. One no longer waiting is left be.
pub(crate) fn cancel_held(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    reason: String,
) -> Result<(), RpcError> {
    let mut stored = locked(tx, id)?;
    if stored.summary.status != DeploymentStatus::WaitingForCi {
        return Ok(());
    }
    let outcome = Outcome::NotExecuted {
        reason,
        cause: Vec::new(),
        needs_upload: Vec::new(),
        same_again: false,
    };
    stored.summary.status = DeploymentStatus::Cancelled;
    stored.summary.ended_at = Some(now());
    stored.summary.outcome = Some(outcome.clone());
    stored.run.outcome = Some(outcome);
    save(tx, &mut stored)
}

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
            "SELECT MAX(number) FROM config_deployment WHERE environment_id = ?1 \
             AND number > ?2 AND status IN ('running', 'cancelling') AND lease > ?3",
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
    let cluster_domains = match crate::domain::has_generated(&running) {
        true => cluster_domains(tx, &stored.summary.environment_id, &head)?,
        false => BTreeMap::new(),
    };
    let dropped =
        crate::review::approval::destructive_effects(&running, &revision, &cluster_domains);
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
    let number = queue(tx, &stored.summary.environment_id, false, |tx, number| {
        let listed = next_listed(tx, &stored.summary.environment_id)?;
        // Every other frozen column comes from the source, so a retry never re-reads
        // authored state.
        tx.execute(
            "INSERT INTO config_deployment \
             (id, organization_id, environment_id, number, status, saved_revision, services, \
              nodes, namespace, run, credentials, upload, cluster_domain, admitted, admitted_by, \
              message, listed, service_uploads) \
             SELECT ?1, organization_id, environment_id, ?2, 'queued', saved_revision, ?7, \
              ?8, namespace, ?3, credentials, upload, cluster_domain, ?5, ?6, message, ?9, \
              ?10 \
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
            ],
        )?;
        // A retry builds the commits its source pinned.
        tx.execute(
            "INSERT INTO config_build \
             (deployment_id, service, organization_id, commit_sha, status, message, log) \
             SELECT ?1, service, organization_id, commit_sha, 'pending', NULL, '' \
             FROM config_build WHERE deployment_id = ?2",
            &[id.as_str().into(), source.as_str().into()],
        )
        .map(|_| ())
    })?;
    let summary = DeploymentSummary {
        id: id.clone(),
        number: Some(number),
        status: DeploymentStatus::Queued,
        services,
        ships: shipped(&nodes),
        runner: None,
        admitted_by: who.principal.clone(),
        admitted_at,
        started_at: None,
        ended_at: None,
        in_flight: DeploymentStatus::Queued.in_flight(),
        outcome: None,
        warnings: Vec::new(),
        ingress: Vec::new(),
        push: None,
        ci: None,
        held: false,
        chooses_upload_at_start: false,
        preparing: false,
        ..stored.summary
    };
    Ok((summary, rollback))
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
) -> Result<Vec<crate::removal::RemovedVolume>, RpcError> {
    let source = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    let mut removed = crate::removal::removed(applied, &source, &stored.namespace)?;
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
            removed.push(crate::removal::RemovedVolume {
                docker_volume: crate::removal::docker_volume(&stored.namespace, id.as_str())?,
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

/// A queued Deployment of `who`'s Organization, checked that a runner may still
/// claim it and a Server could run it. Nothing changes: the caller hands it to a runner.
pub(crate) fn start(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
    trusted: &crate::Trusted,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, id)?;
    let stored = locked(tx, id)?;
    match stored.summary.status {
        DeploymentStatus::Queued => {
            trusted.runnable()?;
            Ok(stored.summary)
        }
        DeploymentStatus::Running | DeploymentStatus::Cancelling => Err(error::conflict(
            "This Deployment is already running",
            json!({ "deployment": id }),
        )),
        DeploymentStatus::WaitingForCi => Err(waiting(id)),
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
    let starting = match stored.summary.status {
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
            false
        }
        DeploymentStatus::Running | DeploymentStatus::Cancelling => {
            return Ok(Err(owned_elsewhere(id)));
        }
        DeploymentStatus::WaitingForCi => return Ok(Err(waiting(id))),
        DeploymentStatus::Superseded => return Ok(Err(superseded(id))),
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => return Ok(Err(ended(id))),
        DeploymentStatus::Queued => {
            let started = now();
            stored.summary.status = DeploymentStatus::Running;
            stored.summary.runner = Some(runner.clone());
            stored.summary.started_at = Some(started);
            stored.lease = started + LEASE;
            true
        }
    };
    let saved = held_revision(tx, &stored)?;
    // An earlier run may have fixed a Volume's storage since this one was admitted.
    if let Err(refused) = crate::volume::check_admitted(tx, &stored.summary.environment_id, &saved)
    {
        if refused.code != ployz_core::RpcErrorCode::Conflict {
            return Err(refused);
        }
        finish(
            tx,
            stored,
            Outcome::NotExecuted {
                reason: refused.message.clone(),
                cause: Vec::new(),
                needs_upload: Vec::new(),
                same_again: false,
            },
            DeploymentStatus::Failed,
        )?;
        return Ok(Err(refused));
    }
    let Lowered {
        input,
        mut intent,
        warnings,
        ..
    } = match lowered(tx, &stored, &saved) {
        Ok(lowered) => lowered,
        // Every claim lowers it the same way: it fails rather than waits queued,
        // and takes nothing over. A new Deploy may still get past it, since a Branch
        // reference resolves against what its parent has applied since.
        Err(refused) if refused.code == ployz_core::RpcErrorCode::InvalidArgument => {
            finish(
                tx,
                stored,
                Outcome::NotExecuted {
                    reason: refused.message.clone(),
                    cause: Vec::new(),
                    needs_upload: Vec::new(),
                    same_again: false,
                },
                DeploymentStatus::Failed,
            )?;
            return Ok(Err(error::conflict(refused.message, refused.details)));
        }
        Err(error) => return Err(error),
    };
    let sources = build::routed(tx, &stored, &saved)?;
    if starting {
        let taken_over = tx.execute(
            "UPDATE config_deployment SET status = 'unknown' \
             WHERE environment_id = ?1 AND status IN ('running', 'cancelling')",
            &[stored.summary.environment_id.as_str().into()],
        )?;
        super::uploads::claimed(tx, &mut stored)?;
        save(tx, &mut stored)?;
        if taken_over > 0 {
            crate::review::scrub_sealed(tx, &stored.summary.environment_id)?;
        }
    }
    let uploads = build::uploads_of(tx, &stored)?;
    if stored.run.warnings != warnings {
        stored.run.warnings = warnings;
        stored.summary.warnings = stored.run.summary_warnings();
        save(tx, &mut stored)?;
    }
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
            TargetNode::Service { .. } | TargetNode::Config { .. } => None,
        })
        .flatten()
        .collect();
    let organization = build::organization(tx, id)?;
    let build_order = crate::builders::order(tx, organization.as_str())?;
    let sealed_names = crate::review::sealed::names(tx, stored.summary.environment_id.as_str())?;
    Ok(Ok(Claimed {
        deployment: stored.summary,
        intent,
        sealed_names,
        input: json_value(&input),
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

/// The Saved revision `stored` ships, each Volume at the size its Servers hold: a
/// Deploy neither grows nor shrinks one, so a revision saved before a resize asks
/// the size it holds now.
pub(crate) fn held_revision(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let mut saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    crate::volume::size::held(tx, &stored.summary.environment_id, &mut saved.volumes)?;
    Ok(saved)
}

/// `stored` lowered from `saved`, its held revision: what a build of it takes.
pub(crate) fn lowered(
    tx: &mut dyn Tx,
    stored: &Stored,
    saved: &SavedEnvironmentIntent,
) -> Result<Lowered, RpcError> {
    let branch = crate::branch::lowering(tx, &stored.summary.environment_id, saved)?;
    let ships = stored.lowering(saved);
    let deployed = deployed_volumes(tx, &stored.summary.environment_id, saved)?;
    let organization = crate::build::organization(tx, &stored.summary.id)?;
    lower(
        &stored.summary.environment_id,
        saved,
        &deployed,
        &ships,
        (stored.namespace.clone(), stored.cluster_domain.as_ref()),
        &branch,
        &organization,
    )
}

/// Cancel a Deployment of `who`'s Organization. A queued one, or one waiting for CI,
/// never runs; a running one reads `cancelling` until its runner stops it and records
/// what ran. Cancelling again changes nothing.
pub(crate) fn cancel(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, id)?;
    let mut stored = locked(tx, id)?;
    stored.summary.status = match stored.summary.status {
        DeploymentStatus::WaitingForCi | DeploymentStatus::Queued => {
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

/// Cancel every Deployment of `who`'s Organization that may still run, claimed or
/// not or waiting for CI, without waiting on a runner: its Servers are gone, so none
/// will record what ran. The caller holds each Environment's lock. Returns them,
/// oldest first.
pub(crate) fn cancel_in_flight(
    tx: &mut dyn Tx,
    who: &Actor,
) -> Result<Vec<DeploymentId>, RpcError> {
    let rows = tx.query(
        &format!(
            "SELECT id FROM config_deployment WHERE organization_id = ?1 \
             AND ({} OR status = 'waiting_for_ci') ORDER BY admitted, id",
            in_flight_sql()
        ),
        &[who.organization.as_str().into()],
    )?;
    let mut cancelled = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.parse::<DeploymentId>(0, "Deployment")?;
        let mut stored = load(tx, &id)?;
        stored.summary.status = DeploymentStatus::Cancelled;
        stored.summary.ended_at = Some(now());
        save(tx, &mut stored)?;
        cancelled.push(id);
    }
    Ok(cancelled)
}

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
        stored.lease = now() + LEASE;
    }
    match evidence {
        RunEvidence::Alive => {
            if stored.summary.status != DeploymentStatus::Unknown {
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
                DeploymentStatus::Applied
            } else {
                DeploymentStatus::Failed
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
                // moves no Applied State.
                stored.summary.ended_at = Some(now());
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
            advance(tx, &stored, false)?;
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
                DeploymentStatus::Failed,
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
                        cause: Vec::new(),
                        needs_upload: Vec::new(),
                        same_again: false,
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
                    tracing::warn!(volume = %name, docker_volume = %id, %error, "couldn't delete a removed Volume's data");
                    match error.code {
                        ployz_core::RpcErrorCode::Unavailable => format!("{server} did not answer"),
                        ployz_core::RpcErrorCode::Conflict => format!("it is still in use on {server}"),
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
            Some(Failure::from(format!("{name}: deleting its data failed: {why}")))
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
    advance(tx, &stored, status == DeploymentStatus::Applied)?;
    stored.summary.status = match (stored.summary.status, status) {
        (DeploymentStatus::Cancelling, DeploymentStatus::Failed) => DeploymentStatus::Cancelled,
        _ => status,
    };
    stored.summary.ended_at = Some(now());
    stored.summary.outcome = Some(outcome.clone());
    stored.run.outcome = Some(outcome);
    save(tx, &mut stored)?;
    Ok(stored.summary)
}

/// Put each node `stored` confirmed into Applied State as its Saved revision has
/// it, by [`NodeStatus::applied`], a Volume at the size its Servers hold. Applying
/// one again changes nothing. The Environment's Branches then follow it.
fn advance(tx: &mut dyn Tx, stored: &Stored, succeeded: bool) -> Result<(), RpcError> {
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
        let mut deployed = std::collections::BTreeSet::new();
        for node in advanced {
            let applied = match node {
                TargetNode::Service { .. } => saved
                    .services
                    .iter()
                    .find(|service| service.id == node.id())
                    .map(scope::Node::Service),
                TargetNode::Volume { .. } => volumes
                    .iter()
                    .find(|volume| volume.resource_id == node.id())
                    .map(scope::Node::Volume),
                TargetNode::Config { .. } => saved
                    .configs
                    .iter()
                    .find(|config| config.resource_id == node.id())
                    .map(scope::Node::Config),
            };
            if let Some(applied) = applied {
                deployed.insert(applied.lineage().to_owned());
            }
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
        crate::branch::deployed(tx, &stored.summary.environment_id, &saved, &deployed)?;
        crate::branch::follow(tx, &stored.summary.environment_id, &deployed)?;
    }
    Ok(())
}

/// `next`, with every node `prior` already advanced kept as it was: Applied State
/// holds it now, so recomputing would read it Unchanged.
fn keep_advanced(
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
