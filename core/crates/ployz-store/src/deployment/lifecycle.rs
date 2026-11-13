//! A Deployment's way into the queue and out of it without running: admitted, approved,
//! held for CI, superseded, forgotten and cancelled. Retrying is in `retry.rs`, claiming
//! in `claim.rs`, recording and ending in `record.rs`.

use super::*;
use ployz_core::OperationId;
use ployz_core::authority::{DeployApproval, Signed, TeardownApproval};

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
    /// A new one, numbered next, that ships only what already runs: it supersedes
    /// only the queued one, and what waits for CI still waits.
    Beside(&'a DeploymentId),
}

impl Admission<'_> {
    fn id(self) -> DeploymentId {
        match self {
            Self::New { id, .. } | Self::Promote(id) | Self::Beside(id) => id.clone(),
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
        Admission::Beside(_) => (false, None),
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
    let keeps_holds = held || matches!(admission, Admission::Beside(_));
    queue(tx, environment, keeps_holds, |tx, number| {
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
    Ok(load(tx, id)?.summary)
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

/// Keep the deploy approval a Device signed for Deployment `id`, for its runner to
/// hand each Server. One that names another operation is refused: every Server would.
pub(crate) fn approved_by_device(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    approval: &Signed<DeployApproval>,
) -> Result<(), RpcError> {
    let named = approval.open().ok().map(|body| body.operation);
    if named.is_none() || named != OperationId::from_uuid(id.as_str()).ok() {
        return Err(error::invalid(
            "The deploy approval names another Deployment: sign one for this Deployment's ID",
            json!({ "deployment": id }),
        ));
    }
    let approval =
        serde_json::to_string(approval).map_err(|_| error::corrupt("Deploy approval"))?;
    tx.execute(
        "UPDATE config_deployment SET deploy_approval = ?2 WHERE id = ?1",
        &[id.as_str().into(), approval.as_str().into()],
    )
    .map(drop)
}

/// Keep the teardown a person signed for what Deployment `id` takes off the Servers, for
/// its runner to hand each Server. Cloud keeps it unread; every Server checks it.
pub(crate) fn torn_down_under(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    teardown: &TeardownApproval,
) -> Result<(), RpcError> {
    let teardown = serde_json::to_string(teardown).map_err(|_| error::corrupt("Teardown"))?;
    tx.execute(
        "UPDATE config_deployment SET teardown = ?2 WHERE id = ?1",
        &[id.as_str().into(), teardown.as_str().into()],
    )
    .map(drop)
}

/// The teardown a person signed for Deployment `id`, if any.
pub(super) fn teardown(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<Option<TeardownApproval>, RpcError> {
    let rows = tx.query(
        "SELECT teardown FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first()
        .map(|row| row.optional_text(0))
        .transpose()?
        .flatten()
        .map(|text| serde_json::from_str(text).map_err(|_| error::corrupt("Teardown")))
        .transpose()
}

/// Keep a person's fresh approvals of the commits of queued Deployment
/// `approve.deployment`, which waited for them once its approved builds ended, and let it
/// run. They replace every approval it holds, one for each Service.
///
/// # Errors
/// `conflict` for a Deployment that doesn't wait for them, no longer queues, or whose
/// approved builds haven't all ended with an image, and `invalid_argument` for an
/// approval naming a Service or commit it doesn't build, or a set that leaves out or
/// repeats a Service it holds an approval for.
pub(crate) fn approve_commits(
    tx: &mut dyn Tx,
    who: &Actor,
    approve: &crate::ApproveDeployment,
) -> Result<DeploymentSummary, RpcError> {
    let id = &approve.deployment;
    owned(tx, who, id)?;
    let stored = locked(tx, id)?;
    let refused = |why: String| Err(error::conflict(why, json!({ "deployment": id })));
    let label = stored.summary.label();
    if !awaits(tx, id)? {
        return refused(format!("Deployment {label} doesn't wait for an approval"));
    }
    if stored.summary.status != DeploymentStatus::Queued {
        return refused(format!("Deployment {label} already started or ended"));
    }
    if !build::approved_built(tx, id)? {
        return refused(format!("Deployment {label} hasn't built every image yet"));
    }
    let held: BTreeSet<ServiceName> = build::commit_approvals(tx, id)?.into_keys().collect();
    let mut named = BTreeSet::new();
    for approval in &approve.approvals {
        let service = approval
            .open()
            .map_err(|_| error::invalid("A commit approval can't be read", json!({})))?
            .service;
        if !named.insert(service.clone()) {
            return Err(error::invalid(
                format!("The approvals name {service} twice"),
                json!({ "service": service }),
            ));
        }
    }
    if named != held {
        return Err(error::invalid(
            format!(
                "Approve every service Deployment {label} builds under your approval: {}",
                held.iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            json!({ "services": held }),
        ));
    }
    for approval in &approve.approvals {
        build::approve_commit(tx, id, approval)?;
    }
    tx.execute(
        "UPDATE config_deployment SET awaits_approval = 0 WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    Ok(stored.summary)
}

/// Whether queued Deployment `id` waits for a person to approve its commits again. One
/// cancelled or replaced meanwhile waits for nothing.
pub(crate) fn awaiting_approval(tx: &mut dyn Tx, id: &DeploymentId) -> Result<bool, RpcError> {
    Ok(load(tx, id)?.summary.status == DeploymentStatus::Queued && awaits(tx, id)?)
}

pub(super) fn awaits(tx: &mut dyn Tx, id: &DeploymentId) -> Result<bool, RpcError> {
    let rows = tx.query(
        "SELECT awaits_approval FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first()
        .ok_or_else(|| missing(id))?
        .int(0)
        .map(|awaits| awaits != 0)
}

/// The deploy approval a Device signed for Deployment `id`, if any.
pub(super) fn device_approval(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<Option<Signed<DeployApproval>>, RpcError> {
    let rows = tx.query(
        "SELECT deploy_approval FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    let Some(text) = rows
        .first()
        .map(|row| row.optional_text(0))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    serde_json::from_str(text)
        .map(Some)
        .map_err(|_| error::corrupt("Deploy approval"))
}

/// Keep `review`, a person's review of preview Deployment `id`'s commit, for its runner
/// to relay. Cloud checks only that it is readable; every Server checks it.
pub(crate) fn reviewed(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    review: &ployz_core::authority::Review,
) -> Result<(), RpcError> {
    let review = serde_json::to_string(review).map_err(|_| error::corrupt("Preview review"))?;
    tx.execute(
        "UPDATE config_deployment SET preview_review = ?2 WHERE id = ?1",
        &[id.as_str().into(), review.as_str().into()],
    )
    .map(drop)
}

/// Keep `review` on each of `environment`'s Deployments held for CI at `head`, so the
/// build CI releases carries it.
pub(crate) fn reviewed_held(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    head: &str,
    review: &ployz_core::authority::Review,
) -> Result<(), RpcError> {
    let review = serde_json::to_string(review).map_err(|_| error::corrupt("Preview review"))?;
    tx.execute(
        "UPDATE config_deployment SET preview_review = ?3 WHERE environment_id = ?1 \
         AND push_head = ?2 AND status = 'waiting_for_ci'",
        &[
            environment.as_str().into(),
            head.into(),
            review.as_str().into(),
        ],
    )
    .map(drop)
}

/// The review a person signed for preview Deployment `id`'s commit, if any.
pub(crate) fn preview_review(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<Option<ployz_core::authority::Review>, RpcError> {
    let rows = tx.query(
        "SELECT preview_review FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first()
        .map(|row| row.optional_text(0))
        .transpose()?
        .flatten()
        .map(|text| serde_json::from_str(text).map_err(|_| error::corrupt("Preview review")))
        .transpose()
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
    if ran(tx, environment)?.is_none() {
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
    applied::forget(tx, &stored.summary.environment_id)?;
    end(tx, stored, outcome, End::Applied)
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
pub(super) fn inherit(
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

/// Queue a Deployment of `environment`, written by `insert` with the next number.
/// It supersedes the queued one, if any, and every one waiting for CI unless
/// `keeps_holds`. Each superseded one ends now. The superseded queued one leaves
/// flight, so the sealed values only it kept in History go once the new one holds
/// its revision.
pub(super) fn queue(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    keeps_holds: bool,
    insert: impl FnOnce(&mut dyn Tx, u64) -> Result<(), RpcError>,
) -> Result<u64, RpcError> {
    let now = now();
    let superseded = tx.execute(
        "UPDATE config_deployment SET status = 'superseded', ended = ?2 \
         WHERE environment_id = ?1 AND status = 'queued'",
        &[environment.as_str().into(), now.into()],
    )?;
    if !keeps_holds {
        tx.execute(
            "UPDATE config_deployment SET status = 'superseded', ended = ?2 \
             WHERE environment_id = ?1 AND status = 'waiting_for_ci'",
            &[environment.as_str().into(), now.into()],
        )?;
    }
    let number = next_number(tx, environment)?;
    insert(tx, number)?;
    if superseded > 0 {
        crate::seal_scrub::history::scrub_sealed(tx, environment)?;
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
    stored.standing = stored.standing.cancel(now());
    stored.summary.outcome = Some(outcome.clone());
    stored.run.outcome = Some(outcome);
    save(tx, &mut stored)
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
    stored.standing = match stored.summary.status {
        DeploymentStatus::WaitingForCi | DeploymentStatus::Queued => stored.standing.cancel(now()),
        DeploymentStatus::Running => stored.standing.cancelling(),
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
        stored.standing = stored.standing.cancel(now());
        save(tx, &mut stored)?;
        cancelled.push(id);
    }
    Ok(cancelled)
}
