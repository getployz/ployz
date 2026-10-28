//! Reading what ran: Head, Applied State, in-flight and past Deployments.

use super::*;

/// The rule every reading of "the Deployment history" shares: only numbered rows
/// count, in number order. A hold that never promoted (waiting, superseded or
/// cancelled while waiting) has no number and is not history. Postgres sorts
/// NULLs first under `DESC`, so the filter is never left to the sort order.
pub(crate) const NUMBERED: &str = "number IS NOT NULL";

/// `environment`'s latest `limit` Deployments that were admitted and weren't
/// superseded, newest first: none waiting for CI.
pub(crate) fn history(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    limit: i64,
) -> Result<Vec<DeploymentSummary>, RpcError> {
    let columns = COLUMNS.as_str();
    tx.query(
        &format!(
            "SELECT {columns} FROM config_deployment \
             WHERE environment_id = ?1 AND {NUMBERED} AND status NOT IN ('superseded') \
             ORDER BY number DESC LIMIT ?2"
        ),
        &[environment.as_str().into(), limit.into()],
    )?
    .iter()
    .map(|row| stored(row).map(|stored| stored.summary))
    .collect()
}

/// Applied State: each node as its latest confirmed Deployment applied it, as one
/// document shaped like `like`.
pub(crate) fn applied_state(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    like: &SavedEnvironmentIntent,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let rows = tx.query(
        "SELECT node, node_type FROM config_applied WHERE environment_id = ?1 ORDER BY node_id",
        &[environment.as_str().into()],
    )?;
    scope::nodes(&rows, like, "Applied State")
}

/// The resource ID of each Volume Applied State holds: a Deploy put its data on a Server.
pub(crate) fn deployed_volumes(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    like: &SavedEnvironmentIntent,
) -> Result<BTreeSet<String>, RpcError> {
    Ok(applied_state(tx, environment, like)?
        .volumes
        .into_iter()
        .map(|volume| volume.resource_id)
        .collect())
}

/// What reviews compare Working State against: Applied State, overlaid with the
/// target nodes of the newest Deployment in flight, if any. Its token changes
/// whenever a Deployment is admitted or ends.
pub(crate) fn head(tx: &mut dyn Tx, environment: &Environment) -> Result<Head, RpcError> {
    let id = &environment.summary.id;
    let applied = applied_state(tx, id, &environment.working)?;
    let in_flight = in_flight_sql();
    let ended = tx
        .query(
            &format!(
                "SELECT COUNT(*) FROM config_deployment \
                 WHERE environment_id = ?1 AND {NUMBERED} AND status NOT IN ('superseded') \
                 AND NOT {in_flight}"
            ),
            &[id.as_str().into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment count"))?
        .int(0)?;
    let rows = tx.query(
        &format!(
            "SELECT number, saved_revision, nodes, cluster_domain FROM config_deployment \
             WHERE environment_id = ?1 AND {NUMBERED} AND {in_flight} ORDER BY number DESC"
        ),
        &[id.as_str().into()],
    )?;
    let mut head = Head {
        token: format!("0.{ended}"),
        intent: applied.clone(),
        applied,
        in_flight: Vec::new(),
        may_run: may_run(tx, id)?,
    };
    for row in &rows {
        let saved = saved_at(tx, id, row.number(1, "revision")?)?;
        let nodes: Vec<TargetNode> = row.json(2, "Deployment")?;
        let mut target = InFlight {
            cluster_domain: row.parse_optional(3, "Cluster Domain")?,
            services: nodes
                .iter()
                .filter_map(|node| match node {
                    TargetNode::Service { .. } => saved
                        .services
                        .iter()
                        .find(|service| service.id == node.id()),
                    TargetNode::Volume { .. } | TargetNode::Config { .. } => None,
                })
                .cloned()
                .collect(),
            // A Volume the Deployment removes isn't in Head.
            volumes: nodes
                .iter()
                .filter_map(|node| match node {
                    TargetNode::Volume { deletes: None, .. } => saved
                        .volumes
                        .iter()
                        .find(|volume| volume.resource_id == node.id()),
                    TargetNode::Service { .. }
                    | TargetNode::Config { .. }
                    | TargetNode::Volume {
                        deletes: Some(_), ..
                    } => None,
                })
                .cloned()
                .collect(),
            configs: nodes
                .iter()
                .filter_map(|node| match node {
                    TargetNode::Config { .. } => saved
                        .configs
                        .iter()
                        .find(|config| config.resource_id == node.id()),
                    TargetNode::Service { .. } | TargetNode::Volume { .. } => None,
                })
                .cloned()
                .collect(),
            removes: BTreeSet::new(),
        };
        crate::volume::size::held(tx, id, &mut target.volumes)?;
        target.removes = nodes
            .iter()
            .map(TargetNode::id)
            .filter(|id| {
                !target.services.iter().any(|service| service.id == *id)
                    && !target
                        .volumes
                        .iter()
                        .any(|volume| volume.resource_id == *id)
                    && !target
                        .configs
                        .iter()
                        .any(|config| config.resource_id == *id)
            })
            .map(str::to_owned)
            .collect();
        if head.in_flight.is_empty() {
            head.token = format!("{}.{ended}", row.int(0)?);
            let intent = &mut head.intent;
            intent
                .services
                .retain(|service| nodes.iter().all(|node| node.id() != service.id));
            intent
                .volumes
                .retain(|volume| nodes.iter().all(|node| node.id() != volume.resource_id));
            intent
                .configs
                .retain(|config| nodes.iter().all(|node| node.id() != config.resource_id));
            intent.configs.extend(target.configs.iter().cloned());
            intent.services.extend(target.services.iter().cloned());
            intent.volumes.extend(target.volumes.iter().cloned());
        }
        head.in_flight.push(target);
    }
    Ok(head)
}

/// The Services `environment`'s Deployments that started may have left running
/// without Applied State holding them: every Service of one of unknown outcome,
/// whose runner lost track of it, and each Service of one that failed or was
/// cancelled whose work started and didn't finish, or whose outcome its runner was
/// lost before recording. The runner names only the first failure it saw, so a
/// Service it reports as never attempted may still have a row that started: a
/// Container it created beside the one it names. Only an applied Deployment of
/// every Service settles them; one of some Services leaves the rest in doubt. A
/// Service comes once for each such Deployment, as each may have put
/// its own routes and mounts in place.
fn may_run(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<SavedServiceIntent>, RpcError> {
    let rows = tx.query(
        &format!(
            "SELECT saved_revision, nodes, status, run, id FROM config_deployment \
             WHERE environment_id = ?1 AND {NUMBERED} AND started IS NOT NULL \
             AND (status IN ('unknown', 'failed', 'cancelled') \
              OR (status IN ('running', 'cancelling') AND lease <= {})) \
             AND number > COALESCE((SELECT MAX(number) FROM config_deployment \
              WHERE environment_id = ?1 AND status = 'applied' AND services = '[]'), 0)",
            now()
        ),
        &[environment.as_str().into()],
    )?;
    let mut services: Vec<SavedServiceIntent> = Vec::new();
    for row in &rows {
        let saved = saved_at(tx, environment, row.number(0, "revision")?)?;
        let nodes: Vec<TargetNode> = row.json(1, "Deployment")?;
        let ended = matches!(
            row.variant::<DeploymentStatus>(2, "Deployment status")?,
            DeploymentStatus::Failed | DeploymentStatus::Cancelled
        );
        let run: Run = row.json(3, "Deployment run")?;
        let started = if ended {
            started_runtimes(tx, row.text(4)?)?
        } else {
            BTreeSet::new()
        };
        for node in &nodes {
            let TargetNode::Service { .. } = node else {
                continue;
            };
            let doubtful = match run.nodes.get(node.id()) {
                Some(NodeStatus::Failed | NodeStatus::Unknown) => true,
                Some(NodeStatus::Deployed | NodeStatus::Removed | NodeStatus::Unchanged) => false,
                Some(NodeStatus::Pending | NodeStatus::NotAttempted) | None => node
                    .runtimes()
                    .into_iter()
                    .any(|runtime| started.contains(runtime)),
            };
            if ended && !doubtful {
                continue;
            }
            if let Some(service) = saved
                .services
                .iter()
                .find(|service| service.id == node.id())
            {
                services.push(service.clone());
            }
        }
    }
    Ok(services)
}

/// The runtime Services Deployment `id`'s runner began work on, on any Server. A
/// row keeps when it started however its state is rewritten later, including by
/// the settling of unfinished rows as never attempted when the Deployment ends.
fn started_runtimes(tx: &mut dyn Tx, id: &str) -> Result<BTreeSet<ServiceName>, RpcError> {
    tx.query(
        "SELECT DISTINCT service FROM config_deployment_row \
         WHERE deployment_id = ?1 AND started IS NOT NULL",
        &[id.into()],
    )?
    .iter()
    .map(|row| row.parse(0, "Deployment row"))
    .collect()
}

/// The Cluster Domain each generated domain runs under, by node ID and prefix: that
/// of the Deployment that applied it, else of the newest Deployment in flight that
/// puts it in place.
pub(crate) fn cluster_domains(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    head: &Head,
) -> Result<BTreeMap<(String, String), Hostname>, RpcError> {
    let mut applied = BTreeMap::new();
    for row in &tx.query(
        "SELECT a.node_id, d.cluster_domain FROM config_applied a \
         JOIN config_deployment d ON d.id = a.deployment_id \
         WHERE a.environment_id = ?1 AND d.cluster_domain IS NOT NULL",
        &[environment.as_str().into()],
    )? {
        applied.insert(row.text(0)?.to_owned(), row.parse(1, "Cluster Domain")?);
    }
    let mut domains = BTreeMap::new();
    let running = head
        .applied
        .services
        .iter()
        .filter_map(|service| Some((service, applied.get(&service.id)?)));
    let in_flight = head.in_flight.iter().flat_map(|target| {
        let cluster = target.cluster_domain.as_ref();
        target
            .services
            .iter()
            .filter_map(move |service| Some((service, cluster?)))
    });
    for (service, cluster) in running.chain(in_flight) {
        for managed in &service.config.managed_hostnames {
            domains
                .entry((service.id.clone(), managed.prefix.clone()))
                .or_insert_with(|| cluster.clone());
        }
    }
    Ok(domains)
}

/// `environment`'s Deployment that may still run, if any: queued, or claimed by a
/// runner whose lease holds.
pub(crate) fn in_flight(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    latest_where(tx, environment, &in_flight_sql())
}

/// `environment`'s latest Deployment queued or claimed, even one whose runner's lease
/// lapsed: what it sent the Machines may still land.
pub(crate) fn claimed_or_queued(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    latest_where(
        tx,
        environment,
        "status IN ('queued', 'running', 'cancelling')",
    )
}

/// How long after a Deployment that ran ends its Servers may still be doing what it
/// sent: a Machine finishes admitted work its caller stopped waiting for, and a
/// runner waits two minutes on each call.
pub(crate) const SETTLING: i64 = 10 * 60;

/// `environment`'s latest Deployment that ran and ended within [`SETTLING`], whatever
/// its outcome: what it sent its Servers may still land. One whose runner was lost
/// records no end, so its lease, which every record renews, stands in for it.
pub(crate) fn settling(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    let since = super::now() - SETTLING;
    latest_where(
        tx,
        environment,
        &format!(
            "started IS NOT NULL AND (ended >= {since} OR (ended IS NULL AND lease >= {since}))"
        ),
    )
}

/// `environment`'s latest Deployment that started (claimed by a runner), or a
/// removal that applied without one. What may run on the Servers.
pub(crate) fn last_ran(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    latest_where(
        tx,
        environment,
        "(started IS NOT NULL OR (status = 'applied' AND saved_revision = 0))",
    )
}

/// The Saved revision `environment`'s latest applied Deployment shipped: what its
/// Servers run. None before one applied, or once a removal applied.
pub(crate) fn live(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Revision>, RpcError> {
    Ok(latest_where(tx, environment, "status = 'applied'")?
        .map(|deployment| deployment.saved)
        .filter(|saved| *saved != NOTHING))
}

/// Whether the latest Deployment of Saved `revision` failed. Any later attempt
/// at it clears it, even one superseded before it ran.
pub(crate) fn failed(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
) -> Result<bool, RpcError> {
    let condition = format!("saved_revision = {}", revision_param(revision)?);
    Ok(latest_where(tx, environment, &condition)?
        .is_some_and(|deployment| deployment.status == DeploymentStatus::Failed))
}

/// Whether Saved `revision` was only ever shipped by Deployments that ended having
/// run nothing, and at least one shipped it: the save is all that is left of them. A
/// run that settled any node into Applied State before it stopped ran something. A
/// Superseded one was never claimed. It only labels a review, so an unreadable run
/// reads as "not known to have run nothing" rather than failing the review.
pub(crate) fn only_ran_nothing(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
) -> Result<bool, RpcError> {
    let rows = tx.query(
        &format!(
            "SELECT status, run FROM config_deployment \
             WHERE environment_id = ?1 AND {NUMBERED} AND saved_revision = ?2 \
             AND status NOT IN ('superseded')"
        ),
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
        ],
    )?;
    if rows.is_empty() {
        return Ok(false);
    }
    for row in &rows {
        if row.variant::<DeploymentStatus>(0, "Deployment status")? != DeploymentStatus::Failed {
            return Ok(false);
        }
        let Ok(run) = row.json::<Run>(1, "Deployment run") else {
            return Ok(false);
        };
        if !matches!(run.outcome, Some(Outcome::NotExecuted { .. }))
            || run.nodes.values().any(|status| status.applied(false))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `environment`'s latest numbered Deployment matching `condition`.
pub(super) fn latest_where(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    condition: &str,
) -> Result<Option<DeploymentSummary>, RpcError> {
    let columns = COLUMNS.as_str();
    tx.query(
        &format!(
            "SELECT {columns} FROM config_deployment \
             WHERE environment_id = ?1 AND {NUMBERED} AND {condition} \
             ORDER BY number DESC LIMIT 1"
        ),
        &[environment.as_str().into()],
    )?
    .first()
    .map(|row| stored(row).map(|stored| stored.summary))
    .transpose()
}

/// Every Deployment of `who`'s Organization waiting for CI of commit `head` of
/// `repository_id`, with its Environment and push, by Environment.
pub(crate) fn held_for_head(
    tx: &mut dyn Tx,
    who: &Actor,
    repository_id: RepositoryId,
    head: &CommitSha,
) -> Result<Vec<(DeploymentId, EnvironmentId, GitPush)>, RpcError> {
    tx.query(
        "SELECT id, environment_id, push_branch FROM config_deployment \
         WHERE organization_id = ?1 AND push_repository_id = ?2 AND push_head = ?3 \
         AND status = 'waiting_for_ci' ORDER BY environment_id, admitted, id",
        &[
            who.organization.as_str().into(),
            repository_id.into(),
            head.as_str().into(),
        ],
    )?
    .iter()
    .map(|row| {
        Ok((
            row.parse(0, "identity")?,
            row.parse(1, "identity")?,
            GitPush {
                repository_id,
                branch: row.parse(2, "Deployment push")?,
                head: head.clone(),
            },
        ))
    })
    .collect()
}

/// `environment`'s Deployment waiting for CI of `push`, if any.
pub(crate) fn held_by_push(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    push: &GitPush,
) -> Result<Option<DeploymentId>, RpcError> {
    tx.query(
        "SELECT id FROM config_deployment WHERE environment_id = ?1 AND push_repository_id = ?2 \
         AND push_branch = ?3 AND push_head = ?4 AND status = 'waiting_for_ci'",
        &[
            environment.as_str().into(),
            push.repository_id.into(),
            push.branch.as_str().into(),
            push.head.as_str().into(),
        ],
    )?
    .first()
    .map(|row| row.parse(0, "identity"))
    .transpose()
}

/// The Services a Deployment waiting for CI selected, by ID; none selects every one.
pub(crate) fn held_services(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<String>, RpcError> {
    tx.query(
        "SELECT push_services FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?
    .first()
    .map(|row| match row.optional_text(0)? {
        Some(_) => row.json(0, "Deployment"),
        None => Ok(Vec::new()),
    })
    .transpose()
    .map(Option::unwrap_or_default)
}

/// Every queued Deployment no runner claimed that was admitted before `before`
/// (Unix seconds), oldest first: those whose dispatch may have been lost.
pub(crate) fn unclaimed(tx: &mut dyn Tx, before: i64) -> Result<Vec<Unclaimed>, RpcError> {
    tx.query(
        "SELECT organization_id, environment_id, id, admitted, number FROM config_deployment \
         WHERE status = 'queued' AND runner IS NULL AND admitted < ?1 ORDER BY admitted, id",
        &[before.into()],
    )?
    .iter()
    .map(|row| {
        Ok(Unclaimed {
            organization: row.parse(0, "identity")?,
            environment: row.parse(1, "identity")?,
            deployment: row.parse(2, "identity")?,
            admitted_at: row.int(3)?,
            number: row.number(4, "Deployment number")?,
        })
    })
    .collect()
}

/// One Deployment in `who`'s Organization, with its Node Outcomes.
pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentView, RpcError> {
    let stored = owned(tx, who, id)?;
    let environment = scope::load_by_id(tx, &stored.summary.environment_id)?;
    let mut saved = None;
    let by_service = super::rows::by_service(tx, &stored)?;
    let mut rows = super::rows::of_nodes(tx, &stored, &by_service, &mut saved)?;
    let mut targets: Vec<&TargetNode> = stored.nodes.iter().collect();
    targets.sort_by(|a, b| a.listed().cmp(&b.listed()));
    // Only nodes sharing a kind and name need Saved State to order, which is rare.
    if targets
        .windows(2)
        .any(|pair| matches!(pair, [a, b] if a.listed() == b.listed()))
    {
        let saved = match saved.take() {
            Some(saved) => saved,
            None => saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?,
        };
        targets.sort_by(|a, b| a.order(&saved).cmp(&b.order(&saved)));
    }
    let nodes: Vec<NodeOutcome> = targets
        .into_iter()
        .map(|node| NodeOutcome {
            node: node.shown(),
            rows: rows.remove(node.id()).unwrap_or_default(),
            cancelled: stored.run.nodes.get(node.id()) == Some(&NodeStatus::Failed)
                && stored.run.cancelled.contains(node.id()),
            outcome: match (stored.run.nodes.get(node.id()), stored.summary.status) {
                (Some(status), _) => *status,
                (None, DeploymentStatus::Unknown) => NodeStatus::Unknown,
                (
                    None,
                    DeploymentStatus::Queued
                    | DeploymentStatus::Running
                    | DeploymentStatus::Cancelling,
                ) => NodeStatus::Pending,
                (
                    None,
                    DeploymentStatus::WaitingForCi
                    | DeploymentStatus::Superseded
                    | DeploymentStatus::Applied
                    | DeploymentStatus::Failed
                    | DeploymentStatus::Cancelled,
                ) => NodeStatus::NotAttempted,
            },
        })
        .collect();
    let runtime_names = stored
        .nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Service { name, runtime, .. } => Some((name.clone(), runtime.clone())),
            TargetNode::Volume { .. } | TargetNode::Config { .. } => None,
        })
        .collect();
    let builds = build::views(tx, &stored)?;
    let rolls_back = undone(tx, &stored.summary)?;
    let untargeted = super::rows::untargeted(&stored, &by_service);
    Ok(DeploymentView {
        builds,
        runtime_names,
        rolls_back,
        untargeted,
        deployment: stored.summary,
        environment: environment.summary,
        namespace: stored.namespace,
        nodes,
        preview: stored.run.preview,
    })
}

/// One Git build of a Deployment in `who`'s Organization, with its log.
pub(crate) fn build_log(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &build::BuildLogQuery,
) -> Result<build::BuildLogView, RpcError> {
    let stored = owned(tx, who, &query.deployment)?;
    build::log(tx, &stored, &query.service)
}

/// The rows a list of an Environment's Deployments shows: every numbered one, a
/// hold still waiting for CI, and a hold cancelled while waiting. A hold a newer
/// push replaced before CI answered is left out: it never got a number, and a
/// burst of them would push real Deployments off the first page.
const LISTED: &str = "(number IS NOT NULL OR status <> 'superseded')";

/// Where a page ended: the list position of its last row. A position is taken once,
/// at insert, and never moves, so the next page starts below it whatever became of
/// the row since: superseded, it leaves the list; promoted, it keeps its place, and
/// the page that showed it as a hold stays the only one that shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cursor(i64);

impl Cursor {
    /// An opaque word for the API: the position, base64 without padding.
    pub(crate) fn encode(&self) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0.to_string())
    }

    /// The position an [`encode`](Self::encode)d word carries; none for any other text.
    pub(crate) fn decode(word: &str) -> Option<Self> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(word)
            .ok()?;
        let text = String::from_utf8(bytes).ok()?;
        let listed: i64 = text.parse().ok()?;
        (listed > 0 && listed.to_string() == text).then_some(Self(listed))
    }
}

/// One page of `environment`'s [`LISTED`] Deployments, newest position first, after
/// the position `after` carries: where the previous page ended. The second element
/// says where this page ended when another follows.
pub(crate) fn page(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    limit: usize,
    after: Option<&Cursor>,
) -> Result<(Vec<DeploymentSummary>, Option<Cursor>), RpcError> {
    let columns = COLUMNS.as_str();
    let mut params: Vec<crate::storage::Param<'_>> = vec![
        environment.as_str().into(),
        i64::try_from(limit + 1).unwrap_or(i64::MAX).into(),
    ];
    let mut after_sql = "";
    if let Some(after) = after {
        params.push(after.0.into());
        after_sql = " AND listed < ?3";
    }
    let rows = tx.query(
        &format!(
            "SELECT {columns} FROM config_deployment \
             WHERE environment_id = ?1 AND {LISTED}{after_sql} \
             ORDER BY listed DESC LIMIT ?2"
        ),
        &params,
    )?;
    let mut page = rows
        .iter()
        .map(|row| stored(row).map(|stored| (stored.summary, Cursor(stored.listed))))
        .collect::<Result<Vec<_>, _>>()?;
    let next = (page.len() > limit).then(|| {
        page.truncate(limit);
        page.last().map(|(_, cursor)| cursor.clone())
    });
    let page = page.into_iter().map(|(summary, _)| summary).collect();
    Ok((page, next.flatten()))
}

/// `environment`'s newest numbered Deployment, whatever page of its list it falls
/// on: holds take no number, so a burst of them never hides it.
pub(crate) fn latest_numbered(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    let columns = COLUMNS.as_str();
    let rows = tx.query(
        &format!(
            "SELECT {columns} FROM config_deployment \
             WHERE environment_id = ?1 AND number IS NOT NULL ORDER BY number DESC LIMIT 1"
        ),
        &[environment.as_str().into()],
    )?;
    rows.first()
        .map(|row| stored(row).map(|stored| stored.summary))
        .transpose()
}

/// Every Deployment of `environment` in flight, newest number first: queued, or
/// claimed by a runner whose lease holds. A bounded set, however long the list.
pub(crate) fn all_in_flight(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<DeploymentSummary>, RpcError> {
    let columns = COLUMNS.as_str();
    let in_flight = in_flight_sql();
    let rows = tx.query(
        &format!(
            "SELECT {columns} FROM config_deployment \
             WHERE environment_id = ?1 AND {NUMBERED} AND {in_flight} ORDER BY number DESC"
        ),
        &[environment.as_str().into()],
    )?;
    rows.iter()
        .map(|row| stored(row).map(|stored| stored.summary))
        .collect()
}

/// The Servers Deployment `id` failed on, by name, sorted.
pub(crate) fn failed_on(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<String>, RpcError> {
    let stored = load(tx, id)?;
    let by_service = super::rows::by_service(tx, &stored)?;
    let mut servers: Vec<String> = super::rows::of_nodes(tx, &stored, &by_service, &mut None)?
        .into_values()
        .flatten()
        .filter(|row| {
            matches!(
                row.state,
                super::RowState::Failed {
                    stopped_by: crate::StoppedBy::NOTHING,
                    ..
                }
            )
        })
        .map(|row| row.server)
        .collect();
    servers.sort();
    servers.dedup();
    Ok(servers)
}

/// The name each of `machines` last ran a Deployment row under in `who`'s
/// Organization. One no row named is left out.
pub(crate) fn server_names(
    tx: &mut dyn Tx,
    who: &Actor,
    machines: impl IntoIterator<Item = ployz_core::MachineId>,
) -> Result<BTreeMap<ployz_core::MachineId, String>, RpcError> {
    let mut names = BTreeMap::new();
    for machine in machines.into_iter().collect::<BTreeSet<_>>() {
        let id = machine.to_string();
        let named = tx.query(
            "SELECT server FROM config_deployment_row \
             WHERE organization_id = ?1 AND machine = ?2 AND server <> machine \
             ORDER BY COALESCE(finished, started, 0) DESC LIMIT 1",
            &[who.organization.as_str().into(), id.as_str().into()],
        )?;
        if let Some(row) = named.first() {
            names.insert(machine, row.text(0)?.to_owned());
        }
    }
    Ok(names)
}

/// The Machines Deployment `id` failed on because they didn't answer.
pub(crate) fn unanswered_on(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<BTreeSet<ployz_core::MachineId>, RpcError> {
    let stored = load(tx, id)?;
    let by_service = super::rows::by_service(tx, &stored)?;
    Ok(super::rows::of_nodes(tx, &stored, &by_service, &mut None)?
        .into_values()
        .flatten()
        .filter(|row| {
            matches!(
                row.state,
                super::RowState::Failed {
                    unanswered: true,
                    ..
                }
            )
        })
        .map(|row| row.machine_id)
        .collect())
}
