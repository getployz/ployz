//! Review: the Environment Change Set `diff` shows, the Saved revisions Publish
//! writes, and the version that binds a review to the bases it was computed on.
//!
//! A version names the Working revision, the latest Saved revision and the Head
//! (what the Change Set compares against). Publish and Discard recompute the review
//! under the Environment's lock and refuse a version that no longer matches.

mod authored;
pub(crate) mod diff;
pub(crate) mod history;
pub(crate) mod publish;

use ployz_core::RpcError;
use ployz_core::config::{
    At, ChangeKind, ChangeSetInput, EnvironmentNodeType, ReviewComparisonRole, ReviewLifecycleKind,
    ReviewNodeIdentity, ReviewNodeProjection, ReviewStateProjection, RowId, SavedEnvironmentIntent,
    ServiceSettingChange, canonicalize_environment_intent, compile_environment_intent,
    project_environment_changes,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, Revision};
use crate::scope::{Environment, EnvironmentSummary, revision_param};
use crate::settings::{ServiceSetting, SettingPath, shown};
use crate::storage::Tx;

/// An Environment's staged changes, grouped by node, and the version to act on them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct DiffView {
    /// The Environment reviewed.
    pub environment: EnvironmentSummary,
    /// Pass back to `publish` or `discard` to act on exactly this review.
    pub version: String,
    /// The latest Saved revision, if anything was ever published.
    pub saved: Option<Revision>,
    /// Whether Saved State already holds this Working State.
    pub published: bool,
    /// Every changed node.
    pub changes: Vec<NodeChange>,
    /// How many changes there are, counting each node and each Setting.
    pub total_count: usize,
    /// Working State compared with the latest Saved revision, without Introduction fallback.
    #[serde(default)]
    #[ts(as = "Option<Vec<NodeChange>>", optional)]
    pub draft_changes: Vec<NodeChange>,
    /// Changes that an explicit Save records, independent of the deployment comparison.
    #[serde(default)]
    #[ts(as = "Option<usize>", optional)]
    pub draft_count: usize,
    /// Staged changes that arrived from the Parent's deploy by Follow, still at the
    /// value they arrived with, until they deploy.
    #[serde(default)]
    pub incoming: Vec<crate::IncomingChange>,
    /// The Parent's deployed values that followed into this Branch beside its own
    /// changes, or that it discarded: take one to stage it.
    #[serde(default)]
    pub follow_hints: Vec<crate::FollowHint>,
    /// The sources included in this draft: Remove one to put back what it still
    /// brings. Save or Deploy ends them.
    #[serde(default)]
    pub included: Vec<crate::Included>,
}

impl DiffView {
    /// One review list with runtime rows and otherwise hidden draft reversals.
    #[must_use]
    pub fn review_changes(&self) -> Vec<NodeChange> {
        let mut changes = self.changes.clone();
        for draft in &self.draft_changes {
            let Some(current) = changes
                .iter_mut()
                .find(|current| current.node == draft.node)
            else {
                changes.push(draft.clone());
                continue;
            };
            if current.lifecycle != ReviewLifecycleKind::Update {
                continue;
            }
            for row in &draft.settings {
                if !current
                    .settings
                    .iter()
                    .any(|runtime| runtime.path == row.path)
                {
                    current.settings.push(row.clone());
                }
            }
            if draft.lifecycle != ReviewLifecycleKind::Update {
                current.lifecycle = draft.lifecycle;
                current.comparison = Some(ReviewComparisonRole::Saved);
            }
        }
        changes
    }
}

/// What happens to one node, and its changed Settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct NodeChange {
    /// The node's type and ID.
    #[serde(flatten)]
    pub node: ReviewNodeIdentity,
    /// Its name.
    pub name: String,
    /// Its own Sync row, which joins it to what moved it.
    pub row: RowId,
    /// Whether it is created, changed or removed.
    pub lifecycle: ReviewLifecycleKind,
    /// What `settings` compare against; `None` when nothing exists to compare.
    pub comparison: Option<ReviewComparisonRole>,
    /// Its changed Settings, by `SERVICE.SETTING` path.
    pub settings: Vec<ServiceSettingChange>,
    /// What the change does to Volume data: `deleted` for a deployed Volume it
    /// removes, `kept` for a Service that stops mounting a Volume that stays.
    pub data: Option<DataEffect>,
    /// For a Config changed in place, the deployed Services mounting it: they restart
    /// to take its new files.
    #[serde(default)]
    pub restarts: Vec<ployz_core::ServiceName>,
}

/// What a staged change does to data on the Servers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DataEffect {
    /// Deploying it deletes a Volume's data, once each loss is accepted by name.
    Deleted,
    /// A Volume is detached, and it keeps its data.
    Kept,
}

/// One Saved revision.
pub(crate) struct Saved {
    pub(crate) revision: Revision,
    pub(crate) intent: SavedEnvironmentIntent,
}

/// What the Change Set compares Working State against: the submitted Deployment's
/// Saved revision, else Applied State.
pub(crate) struct Head {
    pub(crate) token: String,
    pub(crate) intent: SavedEnvironmentIntent,
    pub(crate) applied: SavedEnvironmentIntent,
}

/// A review and the bases it was computed on.
pub(crate) struct Review {
    pub(crate) view: DiffView,
    pub(crate) saved: Option<Saved>,
    pub(crate) head: Head,
    /// The pull requests the draft includes, and whether each merged here.
    pub(crate) gated: Vec<crate::branch::Gated>,
}

pub(crate) fn review(tx: &mut dyn Tx, environment: &Environment) -> Result<Review, RpcError> {
    let id = &environment.summary.id;
    let saved = latest_saved(tx, id)?;
    let head = crate::deployment::head(tx, environment)?;
    let introductions = introductions(tx, environment)?;
    let mut view = compare(environment, saved.as_ref(), &head, &introductions)?;
    let baseline = saved.as_ref().map_or_else(
        || crate::scope::empty(environment.summary.name.as_str()),
        |saved| saved.intent.clone(),
    );
    let draft = saved_comparison(environment, &baseline)?;
    view.draft_changes = draft.changes;
    view.draft_count = draft.total_count;
    let gated = crate::branch::gated(tx, id)?;
    if let Some(gate) = crate::branch::version_gate(&gated) {
        view.version = format!("{}:{gate}", view.version);
    }
    Ok(Review {
        view,
        saved,
        head,
        gated,
    })
}

fn saved_comparison(
    environment: &Environment,
    baseline: &SavedEnvironmentIntent,
) -> Result<authored::Summary, RpcError> {
    authored::compare(baseline, &environment.working)
}

fn compare(
    environment: &Environment,
    saved: Option<&Saved>,
    head: &Head,
    introductions: &SavedEnvironmentIntent,
) -> Result<DiffView, RpcError> {
    let id = &environment.summary.id;
    let project = |token: &str, intent: &SavedEnvironmentIntent| projection(id, token, intent);
    let changes = project_environment_changes(ChangeSetInput {
        working: project("working", &environment.working),
        applied: project("applied", &head.applied),
        saved: saved.as_ref().map(|saved| project("saved", &saved.intent)),
        submitted: (head.intent != head.applied).then(|| project(&head.token, &head.intent)),
        node_introductions: project("introductions", introductions),
    })
    .map_err(|_| error::corrupt("Environment document"))?;
    let intents = [&environment.working, &head.intent];
    let every = [
        &environment.working,
        &head.intent,
        &head.applied,
        saved
            .as_ref()
            .map_or(&environment.working, |saved| &saved.intent),
    ];
    let mut names = crate::config_item::names_in(&every);
    names.extend(environment.names());
    let lineage = |node: &ReviewNodeIdentity| {
        every.iter().find_map(|intent| match node.node_type {
            EnvironmentNodeType::Service => intent
                .services
                .iter()
                .find(|service| service.id == node.id)
                .map(|service| service.lineage_id.clone()),
            EnvironmentNodeType::Volume => intent
                .volumes
                .iter()
                .find(|volume| volume.resource_id == node.id)
                .map(|volume| volume.resource_lineage_id.clone()),
            EnvironmentNodeType::Config => intent
                .configs
                .iter()
                .find(|config| config.resource_id == node.id)
                .map(|config| config.resource_lineage_id.clone()),
        })
    };
    let name = |node: &ReviewNodeIdentity| {
        let services = intents
            .into_iter()
            .flat_map(|intent| &intent.services)
            .find(|service| service.id == node.id)
            .map(|service| service.slug.clone());
        services
            .or_else(|| volume_name(&intents, &node.id))
            .or_else(|| config_name(&intents, &node.id))
            .unwrap_or_default()
    };
    let mut view = DiffView {
        environment: environment.summary.clone(),
        version: format!(
            "{}:{}:{}",
            environment.summary.revision,
            saved.as_ref().map_or(0, |saved| saved.revision.0),
            head.token
        ),
        saved: saved.as_ref().map(|saved| saved.revision),
        published: saved.as_ref().is_some_and(|saved| {
            saved.intent == canonicalize_environment_intent(environment.working.clone())
        }),
        total_count: changes.total_count,
        draft_changes: Vec::new(),
        draft_count: 0,
        incoming: Vec::new(),
        follow_hints: Vec::new(),
        included: Vec::new(),
        changes: changes
            .groups
            .into_iter()
            .map(|group| {
                let name = name(&group.node);
                let lineage =
                    lineage(&group.node).ok_or_else(|| error::corrupt("Environment document"))?;
                let settings: Vec<ServiceSettingChange> = group
                    .settings
                    .into_iter()
                    .map(|(mut row, at)| {
                        let at = match row.path.split_once('.') {
                            Some(("mounts", id)) => every
                                .iter()
                                .flat_map(|intent| &intent.volumes)
                                .find(|volume| volume.resource_id == id)
                                .map(|volume| At::Mount(volume.resource_lineage_id.clone())),
                            Some(("configs", id)) => every
                                .iter()
                                .flat_map(|intent| &intent.configs)
                                .find(|config| config.resource_id == id)
                                .map(|config| At::ConfigMount(config.resource_lineage_id.clone())),
                            _ => at,
                        };
                        // A row's Setting is what Discard takes, whichever part of it changed.
                        let setting = match &at {
                            Some(At::Setting(setting)) => ServiceSetting::of(*setting),
                            _ => None,
                        };
                        row.row = at.map(|at| RowId::of(&lineage, at));
                        if row.path.starts_with("mounts.") {
                            row.before = row.before.get("mountPath").cloned().unwrap_or_default();
                            row.after = row.after.get("mountPath").cloned().unwrap_or_default();
                        }
                        if row.path.starts_with("configs.") {
                            row.before = row.before.get("mountDir").cloned().unwrap_or_default();
                            row.after = row.after.get("mountDir").cloned().unwrap_or_default();
                        }
                        if group.node.node_type == EnvironmentNodeType::Volume {
                            row.path = format!("volumes.{name}.{}", row.path);
                        } else if group.node.node_type == EnvironmentNodeType::Config {
                            row.before = crate::config_item::shown_file(row.before, &names);
                            row.after = crate::config_item::shown_file(row.after, &names);
                            row.path = format!("configs.@{}.{}", group.node.id, row.path);
                        } else {
                            row.config_name = row
                                .path
                                .strip_prefix("configs.")
                                .and_then(|id| config_name(&intents, id))
                                .and_then(|name| ployz_core::ConfigName::parse(&name).ok());
                            row.before = shown(&row.path, row.before);
                            row.after = shown(&row.path, row.after);
                            row.path = match setting {
                                Some(setting) => format!("{name}.{}", setting.name()),
                                None => SettingPath::from_core(&name, &row.path, |family, id| {
                                    if family == "configs" {
                                        config_name(&intents, id)
                                    } else {
                                        volume_name(&intents, id)
                                    }
                                    .unwrap_or_default()
                                }),
                            };
                        }
                        // `discard` takes exactly the paths that parse.
                        row.can_restore = SettingPath::parse(&row.path).is_ok();
                        row
                    })
                    .collect();
                let data = data_effect(&group.node, group.lifecycle, &settings, &head.applied);
                let restarts = restarts(
                    &group.node,
                    group.lifecycle,
                    &environment.working,
                    &head.applied,
                )?;
                Ok(NodeChange {
                    node: group.node,
                    name,
                    row: RowId::node(&lineage),
                    lifecycle: group.lifecycle,
                    comparison: group.comparison,
                    settings,
                    data,
                    restarts,
                })
            })
            .collect::<Result<_, RpcError>>()?,
    };
    renames(&mut view, &environment.working, &head.intent);
    Ok(view)
}

/// What the destructive review gates.
pub(crate) enum Shipping<'a> {
    /// Deploying these Services of Working State; none deploys all of it.
    Deploy(&'a [ployz_core::ServiceName]),
    /// Publishing Working State without deploying it.
    Publish,
}

/// The Volume data shipping `target` would delete, each accepted by name in
/// `accepted` under the reviewed `version`.
/// A full Deploy removes what Applied State holds and `target` dropped; publishing
/// puts that removal in Saved State for the next full Deploy, so it is reviewed
/// then too. Only a full Deploy's losses come back: it alone deletes them.
///
/// # Errors
/// `confirmation_required` until every loss is accepted; `unavailable` when the
/// Servers weren't asked what they hold.
pub(crate) fn destructive(
    who: &Actor,
    review: &Review,
    (target, namespace, shipping): (
        &SavedEnvironmentIntent,
        &ployz_core::Namespace,
        Shipping<'_>,
    ),
    (version, accepted): (Option<&str>, &[crate::id::VolumeName]),
    observed: Option<&crate::VolumeObservation>,
) -> Result<Vec<crate::VolumeLoss>, RpcError> {
    let full = matches!(shipping, Shipping::Deploy(services) if services.is_empty());
    let publishes = review
        .saved
        .as_ref()
        .is_none_or(|saved| saved.intent != *target);
    let removed = match full || publishes {
        true => crate::removal::removed(&review.head.applied, target, namespace)?,
        false => Vec::new(),
    };
    let losses = crate::removal::review(
        who,
        &review.view.environment.id,
        (&review.view.version, version),
        removed,
        observed,
        accepted,
    )?;
    Ok(if full { losses } else { Vec::new() })
}

/// Core compares Settings only, so a Service renamed since Head gets its name row
/// here: a rename changes nothing else, yet only a Deploy ships it.
fn renames(view: &mut DiffView, working: &SavedEnvironmentIntent, head: &SavedEnvironmentIntent) {
    for service in &working.services {
        let Some(before) = head
            .services
            .iter()
            .find(|deployed| deployed.id == service.id && deployed.slug != service.slug)
        else {
            continue;
        };
        let row = ServiceSettingChange {
            path: format!("{}.name", service.slug),
            kind: ChangeKind::Update,
            before: json!(before.slug),
            after: json!(service.slug),
            // Renaming it back undoes it: `discard` takes no name path.
            can_restore: false,
            row: None,
            config_name: None,
        };
        view.total_count += 1;
        match view
            .changes
            .iter_mut()
            .find(|change| change.node.id == service.id)
        {
            Some(change) => change.settings.insert(0, row),
            None => view.changes.push(NodeChange {
                node: ReviewNodeIdentity {
                    node_type: EnvironmentNodeType::Service,
                    id: service.id.clone(),
                },
                name: service.slug.clone(),
                row: RowId::node(&service.lineage_id),
                lifecycle: ReviewLifecycleKind::Update,
                comparison: Some(ReviewComparisonRole::Head),
                settings: vec![row],
                data: None,
                restarts: Vec::new(),
            }),
        }
    }
}

/// The name of Volume `id` in Working State or Head.
fn volume_name(intents: &[&SavedEnvironmentIntent; 2], id: &str) -> Option<String> {
    intents
        .iter()
        .flat_map(|intent| &intent.volumes)
        .find(|volume| volume.resource_id == id)
        .map(|volume| volume.name.clone())
}

fn config_name(intents: &[&SavedEnvironmentIntent; 2], id: &str) -> Option<String> {
    intents
        .iter()
        .flat_map(|intent| &intent.configs)
        .find(|config| config.resource_id == id)
        .map(|config| config.name.to_string())
}

/// A deployed Volume removed deletes data; a Service removed, or one of its mounts
/// dropped, keeps the Volume.
fn data_effect(
    node: &ReviewNodeIdentity,
    lifecycle: ReviewLifecycleKind,
    settings: &[ServiceSettingChange],
    applied: &SavedEnvironmentIntent,
) -> Option<DataEffect> {
    match node.node_type {
        EnvironmentNodeType::Volume => (lifecycle == ReviewLifecycleKind::Delete
            && applied
                .volumes
                .iter()
                .any(|volume| volume.resource_id == node.id))
        .then_some(DataEffect::Deleted),
        EnvironmentNodeType::Service => {
            let detached = settings
                .iter()
                .any(|row| row.path.contains(".mounts.") && row.after.is_null());
            let removed_with_mounts = lifecycle == ReviewLifecycleKind::Delete
                && applied
                    .services
                    .iter()
                    .any(|service| service.id == node.id && !service.volume_attachments.is_empty());
            (detached || removed_with_mounts).then_some(DataEffect::Kept)
        }
        EnvironmentNodeType::Config => None,
    }
}

fn restarts(
    node: &ReviewNodeIdentity,
    lifecycle: ReviewLifecycleKind,
    working: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
) -> Result<Vec<ployz_core::ServiceName>, RpcError> {
    if node.node_type != EnvironmentNodeType::Config || lifecycle != ReviewLifecycleKind::Update {
        return Ok(Vec::new());
    }
    working
        .services
        .iter()
        .filter(|service| {
            service
                .config_attachments
                .iter()
                .any(|mount| mount.config_resource_id == node.id)
                && applied.services.iter().any(|old| old.id == service.id)
        })
        .map(|service| {
            ployz_core::ServiceName::parse(service.slug.as_str())
                .map_err(|_| error::corrupt("Service name"))
        })
        .collect()
}

/// Refuse unless `version` still names this review; the refusal carries the fresh
/// one. A version a destructive review handed back names this review, then what it
/// deletes (see `crate::removal::review`). No version is refused only while the
/// draft includes a pull request: whether it merged must have been reviewed.
pub(crate) fn check(review: &Review, version: Option<&str>) -> Result<(), RpcError> {
    let current = review.view.version.as_str();
    let names_it = |version: &str| {
        version == current
            || version
                .strip_prefix(current)
                .is_some_and(|rest| rest.starts_with(':') && !rest.starts_with(":g"))
    };
    match version {
        Some(version) if !names_it(version) => Err(error::conflict(
            "The Environment changed after this review. Review the latest changes and try again",
            json!({ "diff": review.view }),
        )),
        None if !review.gated.is_empty() => Err(error::conflict(
            "This draft includes a pull request: review it and pass its version",
            json!({ "diff": review.view }),
        )),
        Some(_) | None => Ok(()),
    }
}

/// Publish `intent` as the next Saved revision, or keep the latest when it already
/// holds the same document. Returns the revision Saved State is now at, and whether
/// this created it.
pub(crate) fn publish(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    intent: SavedEnvironmentIntent,
    latest: Option<&Saved>,
    message: Option<&str>,
) -> Result<(Revision, bool), RpcError> {
    let intent = canonicalize_environment_intent(intent);
    crate::volume::check_storage(tx, environment, &intent)?;
    if let Some(latest) = latest
        && latest.intent == intent
    {
        return Ok((latest.revision, false));
    }
    let revision = Revision(latest.map_or(0, |latest| latest.revision.0) + 1);
    tx.execute(
        "INSERT INTO config_saved (environment_id, revision, organization_id, intent, message, saved_by, saved_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
            who.organization.as_str().into(),
            serde_json::to_string(&intent)
                .expect("Saved State is JSON")
                .as_str()
                .into(),
            message.map(str::trim).filter(|message| !message.is_empty()).into(),
            who.principal.as_ref().map(crate::Principal::as_str).into(),
            crate::deployment::now().into(),
        ],
    )?;
    Ok((revision, true))
}

/// Every node as first created in this Environment, as one document.
pub(crate) fn introductions(
    tx: &mut dyn Tx,
    environment: &Environment,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let rows = tx.query(
        "SELECT node, node_type FROM config_node_introduction WHERE environment_id = ?1",
        &[environment.summary.id.as_str().into()],
    )?;
    crate::scope::nodes(&rows, &environment.working, "Node Introduction")
}

pub(crate) fn latest_saved(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Saved>, RpcError> {
    let rows = tx.query(
        "SELECT revision, intent FROM config_saved WHERE environment_id = ?1 \
         ORDER BY revision DESC LIMIT 1",
        &[environment.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let intent = row.intent(1, "Saved State")?;
    Ok(Some(Saved {
        revision: Revision(row.number(0, "revision")?),
        intent: canonicalize_environment_intent(intent),
    }))
}

fn projection(
    environment: &EnvironmentId,
    token: &str,
    intent: &SavedEnvironmentIntent,
) -> ReviewStateProjection {
    ReviewStateProjection {
        token: token.to_owned(),
        nodes: compile_environment_intent(environment.as_str(), intent.clone())
            .node_snapshots
            .into_iter()
            .map(|node| ReviewNodeProjection {
                node: ReviewNodeIdentity {
                    node_type: node.snapshot.0.node_type(),
                    id: node.node_id,
                },
                config: Some(serde_json::to_value(&node.snapshot.0).expect("configs are JSON")),
            })
            .collect(),
    }
}
