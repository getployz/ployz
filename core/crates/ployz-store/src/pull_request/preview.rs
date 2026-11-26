//! Previews: a PR plan's preview rule, which a Device or a passkey signed, gives each pull
//! request a preview of the plan's start-from Environment, its Parent, in a preview
//! Namespace naming the one Server it runs on. The Store keeps the rule only for a Parent
//! that can't be production, opens at most the plan's cap of previews and holds the rest
//! until one closes, and opens a preview again on another Server once its own leaves.
//! Every Server checks the rule and the Namespace itself; nothing here admits a Container.

use std::num::NonZeroU8;

use ployz_core::authority::{AutoDeployRule, Signed};
use ployz_core::{Namespace, PreviewNamespace, RpcError};

use super::*;

/// The most previews a plan keeps open at once until someone sets another.
pub(crate) const DEFAULT_CAP: NonZeroU8 = NonZeroU8::new(5).expect("5 is not 0");

/// Why an admitted pull request has no preview yet.
pub(crate) const WAITING: &str = "Waiting for a preview slot";

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
    if terms.source.repository_id != repository_id.get() {
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

/// The preview `event` opens under `plan`, on the Server with the fewest open previews of
/// the ones `trusted` says can hold one. Refused, so the pull request waits, while its
/// Parent is where it merges, while the plan's cap of previews is open, or while no Server
/// can hold it.
pub(super) fn slot(
    tx: &mut dyn Tx,
    who: &Actor,
    (project, plan, preview): (&crate::id::ProjectId, &Stored, &StoredPreview),
    event: &PullRequest,
    trusted: &Trusted,
) -> Result<PreviewNamespace, RpcError> {
    let terms = fit(tx, who, (project, plan, event.repository_id), &preview.rule)?;
    if terms.ended {
        return Err(error::conflict(
            "Previews are off for this repository",
            json!({}),
        ));
    }
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
    let live = live(tx, who)?;
    let open = live
        .iter()
        .filter(|open| open.project == *project && open.repository_id == event.repository_id)
        .count();
    if open >= usize::from(preview.cap.get()) {
        return Err(error::conflict(WAITING, json!({})));
    }
    let Some(homes) = trusted.preview_homes.as_deref() else {
        return Err(error::conflict(
            "Ployz Cloud couldn't tell which Servers can hold a preview",
            json!({}),
        ));
    };
    let home = homes
        .iter()
        .min_by_key(|home| {
            let held = live
                .iter()
                .filter(|open| open.preview.homed_on(home))
                .count();
            (held, *home)
        })
        .ok_or_else(|| {
            error::conflict(
                "No Server can hold a preview: upgrade one, or let one take Services",
                json!({}),
            )
        })?;
    let nonce = uuid::Uuid::new_v4().into_bytes();
    let nonce = [nonce[0], nonce[1], nonce[2], nonce[3]];
    Ok(PreviewNamespace::mint(&terms.rule, home, nonce))
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

/// Bring every preview of `who`'s Organization, or of `repository` alone, to what the
/// recorded facts say. Each one that no longer fits closes once: its pull request
/// closed with nothing a merge holds open, it now merges into the Parent its Branch
/// was made from, or `trusted` no longer lists its home. Then each plan's waiting pull
/// requests open, oldest first, while the cap leaves room and a home can take one.
/// Why `asked`, the pull request the caller is about, got no preview goes in
/// `automated`. Every change to a pull request's facts or to the Servers ends here.
pub(super) fn reconcile(
    tx: &mut dyn Tx,
    who: &Actor,
    (repository, asked): (Option<RepositoryId>, Option<PullRequestNumber>),
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let ours = |id: RepositoryId| repository.is_none_or(|repository| repository == id);
    for open in live(tx, who)? {
        if !ours(open.repository_id) {
            continue;
        }
        if let Some(because) = unfit(tx, who, &open, trusted)? {
            crate::removal::close(tx, who, &open.environment, because, automated)?;
        }
    }
    let rows = tx.query(
        "SELECT project_id, repository_id, plan FROM config_pr_plan \
         WHERE organization_id = ?1 ORDER BY project_id, repository_id",
        &[who.organization.as_str().into()],
    )?;
    for row in rows {
        let project = row.parse::<crate::id::ProjectId>(0, "Project ID")?;
        let repository_id: RepositoryId = row.number(1, "repository")?;
        let plan: Stored = row.json(2, "PR plan")?;
        if ours(repository_id) {
            fill(
                tx,
                who,
                (&project, repository_id, &plan),
                asked,
                trusted,
                automated,
            )?;
        }
    }
    Ok(())
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

/// Open previews under `plan` for its open pull requests that have none, oldest first,
/// as far as [`slot`] lets each. One that needs approval, or a bot's the plan leaves
/// out, waits for news of it.
fn fill(
    tx: &mut dyn Tx,
    who: &Actor,
    (project, repository_id, plan): (&crate::id::ProjectId, RepositoryId, &Stored),
    asked: Option<PullRequestNumber>,
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let Some(preview) = plan.preview.as_ref().filter(|_| plan.enabled) else {
        return Ok(());
    };
    let Some(start) = plan
        .start_from
        .as_ref()
        .map(|id| start_from(tx, id))
        .transpose()?
        .flatten()
    else {
        return Ok(());
    };
    let rows = tx.query(
        &format!(
            "SELECT number, {RECORD} FROM config_pull_request \
             WHERE organization_id = ?1 AND repository_id = ?2 AND updated <> '' \
             AND merged IS NULL ORDER BY number"
        ),
        &[who.organization.as_str().into(), repository_id.into()],
    )?;
    for row in rows {
        let number: PullRequestNumber = row.number(0, "pull request")?;
        let record = Record::read(&row, 1)?;
        let event = record.facts();
        if !matches!(record.phase, Phase::Open { .. })
            || (event.bot && !plan.include_bots)
            || current(tx, who, repository_id, number, false)?
                .iter()
                .any(|(_, has)| has == project)
        {
            continue;
        }
        let deployed =
            if !record
                .phase
                .admits(&event.head, &record.facts, trusted.members.as_deref())
            {
                Ok(Some(automation::Deploy::NeedsApproval))
            } else if trusted.runnable().is_err() {
                Ok(Some(automation::Deploy::NoServers))
            } else {
                crate::storage::attempt(tx, |tx| {
                    let namespace = slot(tx, who, (project, plan, preview), &event, trusted)?;
                    create(tx, who, &start, plan, (&event, trusted, Some(&namespace)))
                })
            };
        let opened = matches!(deployed, Ok(Some(automation::Deploy::Admitted(_))));
        if opened || asked == Some(number) {
            automation::record(&start.summary.id, deployed, automated)?;
        } else if let Err(error) = deployed
            && !error::skippable(&error)
        {
            return Err(error);
        }
    }
    Ok(())
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
