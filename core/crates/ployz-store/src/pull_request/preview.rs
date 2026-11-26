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

/// Refuse `rule` unless a key a Server trusts, as `trusted` says Cloud read them, signed
/// it: every Server would refuse each preview it opened.
pub(super) fn signed(rule: &Signed<AutoDeployRule>, trusted: &Trusted) -> Result<(), RpcError> {
    let Some(signers) = &trusted.signers else {
        return Err(error::unavailable(
            "Ployz Cloud couldn't read which keys your Servers trust: try again",
        ));
    };
    if rule.approvers(&signers.keys()).is_err() {
        return Err(error::invalid(
            "No key your Servers trust signed the preview rule: sign it again",
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

/// Open the oldest pull requests waiting for a preview of `project`'s plan for
/// `repository_id`, as many as its cap leaves room for. One that still needs a member's
/// approval isn't waiting for a slot.
pub(super) fn fill(
    tx: &mut dyn Tx,
    who: &Actor,
    (project, repository_id): (&crate::id::ProjectId, RepositoryId),
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let Some(plan) = load(tx, project, repository_id)? else {
        return Ok(());
    };
    let Some(preview) = &plan.preview else {
        return Ok(());
    };
    let held = live(tx, who)?
        .iter()
        .filter(|open| open.project == *project && open.repository_id == repository_id)
        .count();
    let mut free = usize::from(preview.cap.get()).saturating_sub(held);
    let rows = tx.query(
        &format!(
            "SELECT number, {RECORD} FROM config_pull_request \
             WHERE organization_id = ?1 AND repository_id = ?2 AND updated <> '' \
             AND merged IS NULL ORDER BY number"
        ),
        &[who.organization.as_str().into(), repository_id.into()],
    )?;
    for row in rows {
        if free == 0 {
            break;
        }
        let number: PullRequestNumber = row.number(0, "pull request")?;
        let record = Record::read(&row, 1)?;
        let event = record.facts();
        let waiting = matches!(record.phase, Phase::Open { .. })
            && !(event.bot && !plan.include_bots)
            && record
                .phase
                .admits(&event.head, &record.facts, trusted.members.as_deref());
        let has = |tx: &mut dyn Tx| -> Result<bool, RpcError> {
            Ok(current(tx, who, repository_id, number, false)?
                .iter()
                .any(|(_, has)| has == project))
        };
        if !waiting || has(tx)? {
            continue;
        }
        let at = Opening {
            event: &event,
            admits: true,
            trusted,
        };
        open(tx, who, &at, automated)?;
        // One that gets no preview, such as one into its Parent, leaves the slot free.
        if has(tx)? {
            free -= 1;
        }
    }
    Ok(())
}

/// Close each open preview whose Server `trusted` no longer lists as able to hold one, so
/// its pull request opens a new preview on another, with empty Volumes. Nothing closes
/// while Cloud couldn't tell, nor without the members a new preview's admission reads.
pub(crate) fn rehome(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    automated: &mut Automated,
) -> Result<(), RpcError> {
    let (Some(homes), Some(_)) = (trusted.preview_homes.as_deref(), &trusted.members) else {
        return Ok(());
    };
    for open in live(tx, who)? {
        if homes.iter().any(|home| open.preview.homed_on(home)) {
            continue;
        }
        let because = crate::removal::mark::Closed::Moved;
        crate::removal::close(tx, who, &open.environment, because, automated)?;
        fill(
            tx,
            who,
            (&open.project, open.repository_id),
            trusted,
            automated,
        )?;
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
