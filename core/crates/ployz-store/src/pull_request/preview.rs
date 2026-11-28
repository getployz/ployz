//! Previews: a PR plan's preview rule, which a Device or a passkey signed, lets a member
//! deploy a pull request as a preview of the plan's start-from Environment, its Parent, in
//! a preview Namespace naming the one Server it runs on. Nothing opens until someone asks
//! ([`DeployPullRequestPreview`]). Any other pull request, from a fork or by someone
//! outside the Organization, deploys only with a person's [`CommitApproval`] of its head
//! for that preview's own Namespace, and each later commit waits for its own. The Store keeps the rule only for a Parent that can't be
//! production, refuses a deploy past the plan's cap, and opens a preview again on another
//! Server once its own leaves. A preview closes when its pull request closes or someone
//! closes it ([`ClosePreview`]), never for sitting idle. Every Server checks the rule and
//! the Namespace itself; nothing here admits a Container.

use std::num::NonZeroU8;

use ployz_core::authority::{AutoDeployRule, ChargedAuthority, CommitApproval, RuleId, Signed};
use ployz_core::{MachineId, Namespace, PreviewNamespace, RpcError};

use super::*;

/// The most previews a plan keeps open at once until someone sets another.
pub(crate) const DEFAULT_CAP: NonZeroU8 = NonZeroU8::new(5).expect("5 is not 0");

/// Why a deploy found no preview rule to open one under.
pub(crate) const OFF: &str = "Previews are off for this repository";

/// Deploy a pull request as a preview now, in each Project whose plan keeps a preview rule
/// for its repository. A member's, from the repository itself, deploys as it is; any
/// other only with `approval`, in the one Project whose rule it names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployPullRequestPreview {
    pub repository_id: RepositoryId,
    pub number: PullRequestNumber,
    /// The head the person saw: refused once the pull request moved on.
    pub head: CommitSha,
    /// A person's approval of `head` for the preview Namespace its row named. Kept with
    /// the preview's build, so its check-in relays it and every Server checks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub approval: Option<Signed<CommitApproval>>,
}

/// Close one live preview, freeing its place under the cap. Its pull request can deploy
/// again later, as a new preview with empty databases.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ClosePreview {
    /// The Project; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
    /// The preview's Environment.
    pub environment: EnvironmentName,
}

/// A plan's preview rule as stored, with its cap.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct StoredPreview {
    pub(super) rule: Signed<AutoDeployRule>,
    pub(super) cap: NonZeroU8,
}

/// A plan's preview rule as a reader sees it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewPlan {
    #[ts(as = "String")]
    pub rule: ployz_core::authority::RuleId,
    #[ts(type = "number")]
    pub generation: u64,
    #[ts(type = "number")]
    pub cap: NonZeroU8,
}

impl From<&StoredPreview> for Option<PreviewPlan> {
    fn from(stored: &StoredPreview) -> Self {
        let terms = stored.rule.open().ok()?;
        Some(PreviewPlan {
            rule: terms.rule,
            generation: terms.generation,
            cap: stored.cap,
        })
    }
}

/// The open previews among the Namespaces a Server holds, so Cloud closes the others there.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct OpenPreviewsQuery {
    pub namespaces: Vec<Namespace>,
}

/// Those of the asked Namespaces whose preview is open: anything else is closed or never
/// was one of the Organization's.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct OpenPreviewsView {
    pub open: Vec<Namespace>,
    /// Whether a PR plan keeps a preview rule or a preview is open. Without either, Cloud
    /// has no preview to place or close, so it needn't ask the Servers.
    pub active: bool,
}

pub(crate) fn open_previews(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &OpenPreviewsQuery,
) -> Result<OpenPreviewsView, RpcError> {
    let live = live(tx, who)?;
    let open = query
        .namespaces
        .iter()
        .filter(|namespace| {
            live.iter()
                .any(|open| open.preview.namespace() == *namespace)
        })
        .cloned()
        .collect();
    let active = !live.is_empty() || planned(tx, who)?;
    Ok(OpenPreviewsView { open, active })
}

/// Whether one of `who`'s PR plans keeps a preview rule.
fn planned(tx: &mut dyn Tx, who: &Actor) -> Result<bool, RpcError> {
    let rows = tx.query(
        "SELECT p.plan FROM config_pr_plan p \
         JOIN config_project j ON j.id = p.project_id WHERE j.organization_id = ?1",
        &[who.organization.as_str().into()],
    )?;
    for row in rows {
        let plan: Stored = row.json(0, "PR plan")?;
        if plan.preview.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Refuse `rule` unless a Server would open it, by the check a Server runs on a deploy
/// under it, with the keys Cloud read off a Server and the Cluster identity Cloud
/// recorded: every Server would refuse each preview it opened.
pub(super) fn signed(rule: &Signed<AutoDeployRule>, trusted: &Trusted) -> Result<(), RpcError> {
    let (Some(signers), Some(cluster)) = (&trusted.signers, &trusted.sealing) else {
        return Err(error::unavailable(
            "Ployz Cloud couldn't read which keys your Servers trust: try again",
        ));
    };
    let keys = signers.keys();
    let opened = ployz_core::authority::version::Budget::default()
        .charge(rule, &keys)
        .and_then(|charged| {
            ployz_core::authority::rule::verify_signature(charged, Some(cluster.public_key))
        });
    if opened.is_err() {
        return Err(error::invalid(
            "Your Servers wouldn't accept this preview rule's signature: sign it again",
            json!({}),
        ));
    }
    Ok(())
}

/// The terms of `rule` when it fits `plan` for `repository_id`: a preview rule of that
/// repository whose Parent is the plan's start-from, in its Namespace, and that Parent can't
/// be production. Servers can't tell production, so the Store refuses here what could be:
/// a Project without a Protected Environment, which hasn't said which one production is,
/// and a Parent that is Protected or a Branch.
pub(super) fn fit(
    tx: &mut dyn Tx,
    who: &Actor,
    (project, plan, repository_id): (&crate::id::ProjectId, &Stored, RepositoryId),
    rule: &Signed<AutoDeployRule>,
) -> Result<AutoDeployRule, RpcError> {
    let refuse = |message: &str| Err(error::invalid(message, json!({})));
    let Ok(terms) = rule.open() else {
        return refuse("The preview rule isn't one: sign it again");
    };
    let Some(preview) = &terms.preview else {
        return refuse("This rule deploys one Environment, not previews");
    };
    if terms
        .source
        .github()
        .is_none_or(|github| github.repository_id != repository_id.get())
    {
        return refuse("The preview rule is for another repository");
    }
    let Some(parent) = plan.start_from.as_ref() else {
        return refuse("Pick the Environment previews start from first");
    };
    if parent.as_str() != preview.parent.to_string() {
        return refuse(
            "The preview rule's Parent isn't the Environment previews start from: sign it again",
        );
    }
    let protects = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 AND protected <> 0",
        &[project.as_str().into()],
    )?;
    if protects.is_empty() {
        return refuse(
            "Protect production first: previews need a Project that has a Protected Environment",
        );
    }
    if scope::protected(tx, parent)? {
        return refuse("Previews never start from a Protected Environment");
    }
    if crate::branch::rows::parent_of(tx, parent.as_str())?.is_some() {
        return refuse("Previews start from an Environment that isn't a Branch");
    }
    let summary = scope::load_by_id(tx, parent)?.summary;
    if terms.namespace != deployment::namespace(tx, who, &summary)? {
        return refuse("The preview rule names another Namespace than its Parent's: sign it again");
    }
    Ok(terms)
}

/// The preview `event` opens under the rule `terms`: `asked`, the one a person approved,
/// while `trusted` says its Server can hold one and no Environment ever took it, else a
/// new one on the Server with the fewest of the `live` previews among those. Refused
/// while it merges into the plan's Parent or no Server can hold it.
fn slot(
    tx: &mut dyn Tx,
    (project, plan, terms): (&crate::id::ProjectId, &Stored, &AutoDeployRule),
    event: &PullRequest,
    (live, trusted): (&[Live], &Trusted),
    asked: Option<&Namespace>,
) -> Result<PreviewNamespace, RpcError> {
    let destinations = destinations_of(tx, project, event.repository_id, &event.target_branch)?;
    if plan
        .start_from
        .as_ref()
        .is_some_and(|parent| destinations.contains(parent))
    {
        return Err(error::conflict(
            "This pull request merges into the Environment previews start from, so it gets no preview",
            json!({}),
        ));
    }
    let Some(homes) = trusted.preview_homes.as_deref() else {
        return Err(error::conflict(
            "Ployz Cloud couldn't tell which Servers can hold a preview",
            json!({}),
        ));
    };
    if let Some(asked) = asked {
        let fresh = tx
            .query(
                "SELECT environment_id FROM config_namespace WHERE namespace = ?1",
                &[asked.as_str().into()],
            )?
            .is_empty();
        return PreviewNamespace::of(asked)
            .filter(|preview| fresh && homes.iter().any(|home| preview.homed_on(home)))
            .ok_or_else(stale_approval);
    }
    fresh(&terms.rule, homes, live).ok_or_else(|| {
        error::conflict(
            "No Server can hold a preview: upgrade one, or let one take Services",
            json!({}),
        )
    })
}

/// A new preview of `rule` on whichever of `homes` holds the fewest of the `live`
/// previews: where a preview opening now goes, and the Namespace a person's approval of a
/// commit names before it does. None while no Server can hold one.
pub(super) fn fresh(rule: &RuleId, homes: &[MachineId], live: &[Live]) -> Option<PreviewNamespace> {
    let home = homes.iter().min_by_key(|home| {
        let held = live
            .iter()
            .filter(|open| open.preview.homed_on(home))
            .count();
        (held, *home)
    })?;
    let nonce = uuid::Uuid::new_v4().into_bytes();
    let nonce = [nonce[0], nonce[1], nonce[2], nonce[3]];
    Some(PreviewNamespace::mint(rule, home, nonce))
}

/// Refuse `approval` of `head` unless a Server would take it for a build under the rule
/// `terms`, by the check a Server runs, with the keys Cloud read off a Server and the
/// Cluster identity Cloud recorded. Returns the preview Namespace it approves.
fn approved(
    terms: &AutoDeployRule,
    approval: &Signed<CommitApproval>,
    head: &CommitSha,
    trusted: &Trusted,
) -> Result<Namespace, RpcError> {
    let (Some(signers), Some(cluster)) = (&trusted.signers, &trusted.sealing) else {
        return Err(error::unavailable(
            "Ployz Cloud couldn't read which keys your Servers trust: try again",
        ));
    };
    let keys = signers.keys();
    let commit = ployz_core::authority::CommitSha::parse(head.as_str())
        .map_err(|_| error::corrupt("pull request head"))?;
    let checked = ployz_core::authority::version::Budget::default()
        .charge(approval, &keys)
        .and_then(|charged| {
            terms.verify_authority(
                ChargedAuthority::Approved(charged),
                cluster.public_key,
                commit,
                None,
            )
        });
    match (checked, approval.open()) {
        (Ok(()), Ok(approved)) => Ok(approved.namespace),
        _ => Err(stale_approval()),
    }
}

/// Why an approval that no longer fits, or never did, deploys nothing.
fn stale_approval() -> RpcError {
    error::conflict(
        "This approval is out of date. Reload and deploy again.",
        json!({}),
    )
}

/// Fix `preview`'s Namespace as PR Environment `environment`'s, before anything deploys it.
pub(super) fn reserve(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    preview: &PreviewNamespace,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_namespace (organization_id, namespace, environment_id) \
         VALUES (?1, ?2, ?3)",
        &[
            who.organization.as_str().into(),
            preview.namespace().as_str().into(),
            environment.as_str().into(),
        ],
    )?;
    Ok(())
}

/// The PR plans of `repository`, or every one of `who`'s, locked in key order. Every
/// write that changes a plan's previews or the branches they track takes these and
/// reads nothing it decides by until it holds them: news of a pull request, an
/// approval, the sweep with the removals it settles, a change to a plan ([`lock_plan`])
/// and removing a Project ([`lock_project_plans`]). A plan's start-from can change, so
/// it is no lock; the plan's own row is.
///
/// Writers lock in one order: pull request rows, then Projects, then plans, then
/// Environments, each in key order. Deleting a Project deletes its plans, so it counts
/// as taking them: a writer that may delete one holds its Project and its plans first.
/// [`crate::storage::ranked`] checks the order in every debug and test transaction.
pub(super) struct Plans {
    repository: Option<RepositoryId>,
    keys: BTreeSet<(crate::id::ProjectId, RepositoryId)>,
}

pub(super) fn lock_plans(
    tx: &mut dyn Tx,
    who: &Actor,
    repository: Option<RepositoryId>,
) -> Result<Plans, RpcError> {
    let mut keys = BTreeSet::new();
    for (project, repository_id, _) in plans(tx, who, repository)? {
        keys.insert((project, repository_id));
    }
    for (project, repository_id) in &keys {
        lock_plan(tx, project, *repository_id)?;
    }
    Ok(Plans { repository, keys })
}

/// Lock every plan of Project `project`, in key order, before deleting it: see [`Plans`].
pub(crate) fn lock_project_plans(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT repository_id FROM config_pr_plan WHERE project_id = ?1 ORDER BY repository_id",
        &[project.as_str().into()],
    )?;
    for row in rows {
        lock_plan(tx, project, row.number(0, "repository")?)?;
    }
    Ok(())
}

/// Lock Project `project`'s plan for `repository_id`, if it has one: see [`Plans`].
pub(super) fn lock_plan(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
    repository_id: RepositoryId,
) -> Result<(), RpcError> {
    crate::storage::ranked::take(crate::storage::ranked::Lock::plan(project, repository_id));
    tx.execute(
        "UPDATE config_pr_plan SET plan = plan WHERE project_id = ?1 AND repository_id = ?2",
        &[project.as_str().into(), repository_id.into()],
    )?;
    Ok(())
}

/// What a reconcile under the locked `plans` holds the lock of: the start-from of each
/// that keeps a preview rule, under which its previews are counted and opened, and each
/// live preview of their repositories, which it may close. Read only once the plans are
/// locked; a caller locks these with everything else it touches, in one
/// [`scope::lock_all`], before it reconciles.
pub(super) struct Locked {
    plans: Plans,
    start_froms: BTreeSet<EnvironmentId>,
    previews: BTreeSet<EnvironmentId>,
}

impl Locked {
    pub(super) fn of(tx: &mut dyn Tx, who: &Actor, plans: Plans) -> Result<Self, RpcError> {
        let repository = plans.repository;
        let start_froms = self::plans(tx, who, repository)?
            .into_iter()
            .filter(|(project, repository_id, _)| {
                plans.keys.contains(&(project.clone(), *repository_id))
            })
            .filter(|(_, _, plan)| plan.preview.is_some())
            .filter_map(|(_, _, plan)| plan.start_from)
            .collect();
        let previews = live(tx, who)?
            .into_iter()
            .filter(|open| repository.is_none_or(|repository| repository == open.repository_id))
            .map(|open| open.environment)
            .collect();
        Ok(Self {
            plans,
            start_froms,
            previews,
        })
    }

    pub(super) fn ids(&self) -> impl Iterator<Item = EnvironmentId> + '_ {
        self.start_froms.iter().chain(&self.previews).cloned()
    }
}

/// Bring the previews `locked` holds to what the recorded facts say. Each one that no
/// longer fits closes once: its pull request closed with nothing a merge holds open, it
/// now merges into the Parent its Branch was made from, or `trusted` no longer lists its
/// home. One that closed for its home opens again on another Server, as it would on its
/// member's deploy; nothing else opens here, and a freed place stays free until someone
/// deploys. Every change to a pull request's facts or to the Servers ends here. A
/// preview or plan that appeared after `locked` was read waits for the next one.
pub(super) fn reconcile(
    tx: &mut dyn Tx,
    who: &Actor,
    locked: &Locked,
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let mut moved = Vec::new();
    for open in live(tx, who)? {
        if !locked.previews.contains(&open.environment) {
            continue;
        }
        let Some(because) = unfit(tx, who, &open, trusted)? else {
            continue;
        };
        // Read first: a close of one that never ran deletes it at once.
        let pr = crate::pr_environment::of(tx, &open.environment)?;
        crate::removal::close(tx, who, &open.environment, because, automated)?;
        if let (crate::removal::mark::Closed::Moved, Some(pr)) = (because, pr) {
            moved.push((open.project, pr));
        }
    }
    if moved.is_empty() {
        return Ok(());
    }
    let mut live = live(tx, who)?;
    for (project, repository_id, plan) in held(tx, who, locked)? {
        for (_, pr) in moved
            .iter()
            .filter(|(has, pr)| *has == project && pr.repository_id == repository_id)
        {
            let Some(event) = record(tx, who, repository_id, pr.number)?.map(|r| r.facts()) else {
                continue;
            };
            // One a person approved commit by commit needs a new approval for its new home.
            let opened = match trusts(&event, trusted.members.as_deref()) {
                true => {
                    let at = (&project, repository_id, &plan);
                    open_one(tx, who, at, (&mut live, &event, None), trusted, automated)
                }
                false => Err(needs_approval(&event.author)),
            };
            let Err(refused) = opened else {
                continue;
            };
            let Some(start) = &plan.start_from else {
                return Err(refused);
            };
            automation::record(start, Err(refused), automated)?;
        }
    }
    Ok(())
}

/// Each plan of `locked` it holds the lock of, with its start-from.
fn held(
    tx: &mut dyn Tx,
    who: &Actor,
    locked: &Locked,
) -> Result<Vec<(crate::id::ProjectId, RepositoryId, Stored)>, RpcError> {
    Ok(plans(tx, who, locked.plans.repository)?
        .into_iter()
        .filter(|(project, repository_id, plan)| {
            locked
                .plans
                .keys
                .contains(&(project.clone(), *repository_id))
                && plan
                    .start_from
                    .as_ref()
                    .is_some_and(|start| locked.start_froms.contains(start))
        })
        .collect())
}

/// Open `event`'s preview now in each Project whose plan `locked` holds keeps a preview
/// rule, as a member asked. A pull request that isn't a member's own from the repository
/// opens only with `approval`, in the one Project whose rule it names, and an open
/// preview of it deploys the approved head again. Refused, opening none, while previews
/// are off, it needs an approval it lacks, or a plan's cap is full.
pub(super) fn deploy(
    tx: &mut dyn Tx,
    who: &Actor,
    locked: &Locked,
    (event, approval): (&PullRequest, Option<&Signed<CommitApproval>>),
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let mut planned: Vec<_> = held(tx, who, locked)?
        .into_iter()
        .filter(|(_, _, plan)| plan.preview.is_some())
        .collect();
    if planned.is_empty() {
        return Err(error::conflict(OFF, json!({})));
    }
    let approval = match trusts(event, trusted.members.as_deref()) {
        true => None,
        false => Some(approval.ok_or_else(|| needs_approval(&event.author))?),
    };
    if let Some(approval) = approval {
        let named = approval.open().map_err(|_| stale_approval())?.rule;
        planned.retain(|(_, _, plan)| {
            (plan.preview.as_ref())
                .and_then(|preview| preview.rule.open().ok())
                .is_some_and(|terms| terms.rule == named)
        });
        if planned.is_empty() {
            return Err(stale_approval());
        }
    }
    let mut live = live(tx, who)?;
    for (project, repository_id, plan) in &planned {
        let at = (project, *repository_id, plan);
        open_one(
            tx,
            who,
            at,
            (&mut live, event, approval),
            trusted,
            automated,
        )?;
    }
    Ok(())
}

/// Why a pull request by `author` that isn't a member's own from the repository doesn't
/// deploy: it needs a person's approval of its commit.
fn needs_approval(author: &str) -> RpcError {
    error::conflict(
        format!("Not deployed: approve {author}'s commit with your passkey"),
        json!({ "author": author }),
    )
}

/// Open `event`'s preview under `plan` and admit its first Deployment, unless one is open
/// there. One whose last deploy failed with nothing live closes, and a new one opens in
/// its place. With `approval`, it opens in the Namespace the approval names, and an open
/// one, which must be that one, deploys the approved head unless it already does.
/// Refused while previews are off, the cap is full counting the `live` previews that hold
/// a place, or no Server can hold it.
fn open_one(
    tx: &mut dyn Tx,
    who: &Actor,
    (project, repository_id, plan): (&crate::id::ProjectId, RepositoryId, &Stored),
    (live, event, approval): (
        &mut Vec<Live>,
        &PullRequest,
        Option<&Signed<CommitApproval>>,
    ),
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let off = || error::conflict(OFF, json!({}));
    let Some(preview) = plan.preview.as_ref() else {
        return Err(off());
    };
    let terms = fit(tx, who, (project, plan, repository_id), &preview.rule)?;
    if terms.ended {
        return Err(off());
    }
    let asked = approval
        .map(|approval| approved(&terms, approval, &event.head, trusted))
        .transpose()?;
    let Some(start) = plan
        .start_from
        .as_ref()
        .map(|id| start_from(tx, id))
        .transpose()?
        .flatten()
    else {
        return Err(off());
    };
    let mut holding = 0;
    for open in live
        .iter()
        .filter(|open| open.project == *project && open.repository_id == repository_id)
    {
        let shown = shown(tx, &open.environment)?;
        let theirs = crate::pr_environment::of(tx, &open.environment)?
            .is_some_and(|pr| pr.number == event.number);
        match (theirs, shown) {
            (true, Shown::Failed(_)) => {
                let because = crate::removal::mark::Closed::Person;
                crate::removal::close(tx, who, &open.environment, because, automated)?;
            }
            (true, Shown::Deploying(..) | Shown::Live) => {
                let (Some(approval), Some(asked)) = (approval, &asked) else {
                    return Ok(());
                };
                if open.preview.namespace() != asked {
                    return Err(stale_approval());
                }
                if deploys(tx, &open.environment, &event.head)? {
                    return Ok(());
                }
                trusted.runnable()?;
                let environment = open.environment.clone();
                let deployed = crate::storage::attempt(tx, |tx| {
                    redeploy(tx, who, &environment, (event, approval), trusted)
                })?;
                automation::record(&environment, Ok(Some(deployed)), automated)?;
                return Ok(());
            }
            (false, Shown::Failed(_)) => {}
            (false, Shown::Deploying(..) | Shown::Live) => holding += 1,
        }
    }
    let limit = usize::from(preview.cap.get());
    if holding >= limit {
        return Err(error::conflict(
            format!("Not deployed: {holding} of {limit} previews are live"),
            json!({ "live": holding, "limit": limit }),
        ));
    }
    trusted.runnable()?;
    let deployed = crate::storage::attempt(tx, |tx| {
        let namespace = slot(
            tx,
            (project, plan, &terms),
            event,
            (live, trusted),
            asked.as_ref(),
        )?;
        create(
            tx,
            who,
            &start,
            plan,
            (event, trusted, Some(&namespace)),
            approval,
        )
    })?;
    *live = self::live(tx, who)?;
    automation::record(&start.summary.id, Ok(deployed), automated)?;
    Ok(())
}

/// Whether preview `environment`'s latest Deployment deploys `head` and hasn't failed or
/// been cancelled.
fn deploys(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    head: &CommitSha,
) -> Result<bool, RpcError> {
    Ok(deployment::history(tx, environment, 1)?
        .first()
        .is_some_and(|latest| {
            latest.push.as_ref().is_some_and(|push| push.head == *head)
                && !matches!(
                    latest.status,
                    deployment::DeploymentStatus::Failed | deployment::DeploymentStatus::Cancelled
                )
        }))
}

/// What a preview shows: what its Deployments came to.
pub(super) enum Shown {
    /// Its latest Deployment hasn't ended: it, with the head it deploys when a push
    /// admitted it.
    Deploying(crate::DeploymentId, Option<CommitSha>),
    /// One of its latest Deployments applied, and the latest ended.
    Live,
    /// None of its latest Deployments applied, and the latest ended: why.
    Failed(String),
}

/// What preview `environment` shows. Only a deploying or live one holds a place under the
/// cap: one whose deploy failed runs nothing.
pub(super) fn shown(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Shown, RpcError> {
    let latest = deployment::history(tx, environment, 20)?;
    let applied = latest
        .iter()
        .any(|deployment| deployment.status == deployment::DeploymentStatus::Applied);
    let Some(latest) = latest.into_iter().next() else {
        // Its first Deployment may still wait for its commit's CI.
        let rows = tx.query(
            &format!(
                "SELECT id, push_head FROM config_deployment \
                 WHERE environment_id = ?1 AND status = '{}' ORDER BY admitted DESC LIMIT 1",
                deployment::DeploymentStatus::WaitingForCi.as_str()
            ),
            &[environment.as_str().into()],
        )?;
        return Ok(match rows.first() {
            Some(row) => Shown::Deploying(
                row.parse(0, "Deployment ID")?,
                row.optional_text(1)?.map(CommitSha::parse).transpose()?,
            ),
            None => Shown::Failed(DIDNT_FINISH.to_owned()),
        });
    };
    if !latest.status.ended() {
        return Ok(Shown::Deploying(
            latest.id,
            latest.push.map(|push| push.head),
        ));
    }
    if applied {
        return Ok(Shown::Live);
    }
    Ok(Shown::Failed(match latest.outcome {
        Some(
            deployment::Outcome::Executed {
                reason: Some(reason),
                ..
            }
            | deployment::Outcome::NotExecuted { reason, .. },
        ) => reason,
        _ => DIDNT_FINISH.to_owned(),
    }))
}

/// Why a preview whose deploy ended without a reason runs nothing.
const DIDNT_FINISH: &str = "The deploy didn't finish";

/// Close preview `asked` as the person asked: it comes off its Server, and its pull
/// request can deploy again. Locks its pull request's row, then the repository's plans,
/// then the preview, as a deploy of it does.
pub(crate) fn close(
    tx: &mut dyn Tx,
    who: &Actor,
    asked: &ClosePreview,
) -> Result<Automated, RpcError> {
    let at = EnvironmentRef {
        project: asked.project.clone(),
        environment: Some(asked.environment.clone()),
    };
    let id = scope::environment(tx, who, &at)?.summary.id;
    let not_one = || {
        error::invalid(
            format!("{} isn't a preview", asked.environment),
            json!({ "environment": asked.environment }),
        )
    };
    if of(tx, &id)?.is_none() {
        return Err(not_one());
    }
    let Some(pr) = crate::pr_environment::of(tx, &id)? else {
        return Err(not_one());
    };
    lock(tx, who, pr.repository_id, pr.number)?;
    let plans = lock_plans(tx, who, Some(pr.repository_id))?;
    let locked = Locked::of(tx, who, plans)?;
    scope::lock_all(tx, locked.ids().chain([id.clone()]))?;
    let mut automated = Automated::default();
    if live(tx, who)?.iter().any(|open| open.environment == id) {
        let because = crate::removal::mark::Closed::Person;
        crate::removal::close(tx, who, &id, because, &mut automated)?;
    }
    automated.checks.push(pr);
    Ok(automated)
}

/// `who`'s PR plans, of `repository` alone when it names one.
fn plans(
    tx: &mut dyn Tx,
    who: &Actor,
    repository: Option<RepositoryId>,
) -> Result<Vec<(crate::id::ProjectId, RepositoryId, Stored)>, RpcError> {
    let rows = tx.query(
        "SELECT project_id, repository_id, plan FROM config_pr_plan \
         WHERE organization_id = ?1 ORDER BY project_id, repository_id",
        &[who.organization.as_str().into()],
    )?;
    let mut plans = Vec::new();
    for row in rows {
        let repository_id: RepositoryId = row.number(1, "repository")?;
        if repository.is_none_or(|repository| repository == repository_id) {
            let project = row.parse::<crate::id::ProjectId>(0, "Project ID")?;
            plans.push((project, repository_id, row.json(2, "PR plan")?));
        }
    }
    Ok(plans)
}

/// Why `open` no longer fits, if it doesn't.
fn unfit(
    tx: &mut dyn Tx,
    who: &Actor,
    open: &Live,
    trusted: &Trusted,
) -> Result<Option<crate::removal::mark::Closed>, RpcError> {
    use crate::removal::mark::Closed;
    let Some(pr) = crate::pr_environment::of(tx, &open.environment)? else {
        return Ok(None);
    };
    let Some(record) = record(tx, who, pr.repository_id, pr.number)? else {
        return Ok(None);
    };
    if !matches!(record.phase, Phase::Open { .. }) {
        let held = on_merge::holds_open(tx, who, &open.environment)?;
        return Ok((!held).then_some(Closed::PullRequest));
    }
    let parent = crate::branch::rows::parent_of(tx, open.environment.as_str())?;
    let target = &record.facts.target_branch;
    let destinations = destinations_of(tx, &open.project, open.repository_id, target)?;
    if parent.is_some_and(|parent| destinations.iter().any(|into| into.as_str() == parent)) {
        return Ok(Some(Closed::PullRequest));
    }
    // A preview moves only when a new one can open: that admission reads the members.
    if let (Some(homes), Some(_)) = (trusted.preview_homes.as_deref(), &trusted.members)
        && !homes.iter().any(|home| open.preview.homed_on(home))
    {
        return Ok(Some(Closed::Moved));
    }
    Ok(None)
}

/// `environment`'s preview, when it opened as one: what it closes as, whatever its plan
/// says now.
pub(crate) fn of(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<PreviewNamespace>, RpcError> {
    let rows = tx.query(
        "SELECT namespace FROM config_namespace WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(PreviewNamespace::of(
        &row.parse::<Namespace>(0, "Namespace")?,
    ))
}

/// The rule preview `environment` deploys under: its plan's preview rule, whichever
/// Parent and generation it names now. None for any other Environment. Every Server checks
/// the rule against the preview's Namespace and Parent itself.
pub(crate) fn rule(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Signed<AutoDeployRule>>, RpcError> {
    if of(tx, environment)?.is_none() {
        return Ok(None);
    }
    let rows = tx.query(
        "SELECT e.project_id, p.repository_id FROM config_pr_environment p \
         JOIN config_environment e ON e.id = p.environment_id WHERE p.environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let project: crate::id::ProjectId = row.parse(0, "Project ID")?;
    let plan = load(tx, &project, row.number(1, "repository")?)?;
    Ok(plan
        .and_then(|plan| plan.preview)
        .map(|preview| preview.rule))
}

/// Refuse a Deployment of preview `environment` whose specs `target`, lowered in its
/// `namespace` from `saved`, its Parent's copy, its preview rule doesn't cover: every
/// Server would refuse it. The view says so first ([`not_covered`]).
pub(crate) fn cover(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    (namespace, target): (&Namespace, &[ployz_core::RequestedServiceSpec]),
) -> Result<(), RpcError> {
    let Some(rule) = rule(tx, environment)?.and_then(|rule| rule.open().ok()) else {
        return Ok(());
    };
    match crate::auto_rule::uncovered(&rule, namespace, saved, target) {
        Some(service) => Err(uncovered(&service)),
        None => Ok(()),
    }
}

/// The Service of `plan`'s start-from whose settings its preview rule doesn't cover, as a
/// preview opening now would deploy them; None while it covers them all, or previews are off.
pub(super) fn not_covered(
    tx: &mut dyn Tx,
    who: &Actor,
    plan: &Stored,
) -> Result<Option<ServiceName>, RpcError> {
    let Some(rule) = (plan.preview.as_ref()).and_then(|preview| preview.rule.open().ok()) else {
        return Ok(None);
    };
    let Some(parent) = &plan.start_from else {
        return Ok(None);
    };
    let Some(saved) = crate::review::latest_saved(tx, parent)? else {
        return Ok(None);
    };
    let Some(home) = rule.machines.first() else {
        return Ok(None);
    };
    let sample = PreviewNamespace::mint(&rule.rule, home, [0; 4]);
    let target = match deployment::query::preview_target(tx, who, (parent, &saved.intent), &sample)
    {
        Ok(target) => target,
        Err(error) if error.code == ployz_core::RpcErrorCode::InvalidArgument => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    Ok(crate::auto_rule::uncovered(
        &rule,
        sample.namespace(),
        &saved.intent,
        &target,
    ))
}

/// Why a preview of settings its rule doesn't cover isn't deployed.
fn uncovered(service: &ServiceName) -> RpcError {
    error::conflict(
        format!("Not deployed: changes {service}'s settings, which the policy doesn't cover"),
        json!({ "service": service }),
    )
}

/// Whether `environment` is a preview its own removal closes. A Close of it may have
/// reached its Server, which then never opens that Namespace again, so nothing makes it
/// live again.
pub(crate) fn closing(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<bool, RpcError> {
    let target = crate::removal::Target::Environment(environment.clone());
    let removing = crate::removal::mark::of(tx, &target)?
        .is_some_and(|mark| mark.state == crate::removal::mark::State::Removing);
    Ok(removing && of(tx, environment)?.is_some())
}

/// One open preview: a PR Environment in a preview Namespace, not being closed.
pub(super) struct Live {
    pub(super) environment: EnvironmentId,
    pub(super) project: crate::id::ProjectId,
    pub(super) repository_id: RepositoryId,
    pub(super) preview: PreviewNamespace,
}

/// Every open preview of `who`'s Organization.
pub(super) fn live(tx: &mut dyn Tx, who: &Actor) -> Result<Vec<Live>, RpcError> {
    let rows = tx.query(
        &format!(
            "SELECT p.environment_id, e.project_id, p.repository_id, n.namespace \
             FROM config_pr_environment p \
             JOIN config_environment e ON e.id = p.environment_id \
             JOIN config_environment_branch b ON b.environment_id = p.environment_id \
             JOIN config_namespace n ON n.environment_id = p.environment_id \
             WHERE p.organization_id = ?1 AND {} \
             ORDER BY p.environment_id",
            mark::unmarked("b.environment_id")
        ),
        &[who.organization.as_str().into()],
    )?;
    let mut live = Vec::new();
    for row in rows {
        let namespace = row.parse::<Namespace>(3, "Namespace")?;
        let Some(preview) = PreviewNamespace::of(&namespace) else {
            continue;
        };
        live.push(Live {
            environment: row.parse(0, "Environment ID")?,
            project: row.parse(1, "Project ID")?,
            repository_id: row.number(2, "repository")?,
            preview,
        });
    }
    Ok(live)
}
