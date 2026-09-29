//! Review: the Environment Change Set `diff` shows, the Saved revisions Publish
//! writes, and the version that binds a review to the bases it was computed on.
//!
//! A version names the Working revision, the latest Saved revision and the Head
//! (what the Change Set compares against). Publish and Discard recompute the review
//! under the Environment's lock and refuse a version that no longer matches.

use ployz_core::RpcError;
use ployz_core::config::{
    ChangeKind, ChangeSetInput, EnvironmentNodeType, ReviewComparisonRole, ReviewLifecycleKind,
    ReviewNodeIdentity, ReviewNodeProjection, ReviewStateProjection, SavedEnvironmentIntent,
    ServiceSettingChange, canonicalize_environment_intent, compile_environment_intent,
    parse_environment_intent, project_environment_changes,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, Revision};
use crate::scope::{Environment, EnvironmentSummary, revision_param};
use crate::settings::ServiceSetting;
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
}

/// What happens to one node, and its changed Settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct NodeChange {
    /// The node's type and ID.
    #[serde(flatten)]
    pub node: ReviewNodeIdentity,
    /// Its name.
    pub name: String,
    /// Whether it is created, changed or removed.
    pub lifecycle: ReviewLifecycleKind,
    /// What `settings` compare against; `None` when nothing exists to compare.
    pub comparison: Option<ReviewComparisonRole>,
    /// Its changed Settings, by `SERVICE.SETTING` path.
    pub settings: Vec<ServiceSettingChange>,
    /// What the change does to Volume data: `deleted` for a deployed Volume it
    /// removes, `kept` for a Service that stops mounting a Volume that stays.
    pub data: Option<DataEffect>,
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
}

pub(crate) fn review(tx: &mut dyn Tx, environment: &Environment) -> Result<Review, RpcError> {
    let id = &environment.summary.id;
    let saved = latest_saved(tx, id)?;
    let head = crate::deployment::head(tx, environment)?;
    let introductions = introductions(tx, environment)?;
    let project = |token: &str, intent: &SavedEnvironmentIntent| projection(id, token, intent);
    let changes = project_environment_changes(ChangeSetInput {
        working: project("working", &environment.working),
        applied: project("applied", &head.applied),
        saved: saved.as_ref().map(|saved| project("saved", &saved.intent)),
        submitted: (head.intent != head.applied).then(|| project(&head.token, &head.intent)),
        node_introductions: project("introductions", &introductions),
    })
    .map_err(|_| error::corrupt("Environment document"))?;
    let intents = [&environment.working, &head.intent];
    let name = |node: &ReviewNodeIdentity| {
        let services = intents
            .into_iter()
            .flat_map(|intent| &intent.services)
            .find(|service| service.id == node.id)
            .map(|service| service.slug.clone());
        services
            .or_else(|| volume_name(&intents, &node.id))
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
        changes: changes
            .groups
            .into_iter()
            .map(|group| {
                let name = name(&group.node);
                let settings: Vec<ServiceSettingChange> = group
                    .settings
                    .into_iter()
                    .map(|mut row| {
                        if row.path.starts_with("env.") {
                            row.before = shown_env(row.before);
                            row.after = shown_env(row.after);
                        }
                        if let Some(volume) = row.path.strip_prefix("mounts.") {
                            row.path = format!(
                                "{name}.mounts.{}",
                                volume_name(&intents, volume).unwrap_or_default()
                            );
                            row.before = row.before.get("mountPath").cloned().unwrap_or_default();
                            row.after = row.after.get("mountPath").cloned().unwrap_or_default();
                            return row;
                        }
                        if group.node.node_type == EnvironmentNodeType::Volume {
                            row.path = format!("volumes.{name}.{}", row.path);
                            return row;
                        }
                        row.path = match ServiceSetting::ALL
                            .into_iter()
                            .find(|setting| setting.field() == row.path)
                        {
                            Some(setting) => {
                                row.before = setting.shown(row.before);
                                row.after = setting.shown(row.after);
                                format!("{name}.{}", setting.name())
                            }
                            None => format!("{name}.{}", row.path),
                        };
                        row
                    })
                    .collect();
                let data = data_effect(&group.node, group.lifecycle, &settings, &head.applied);
                NodeChange {
                    node: group.node,
                    name,
                    lifecycle: group.lifecycle,
                    comparison: group.comparison,
                    settings,
                    data,
                }
            })
            .collect(),
    };
    renames(&mut view, &environment.working, &head.intent);
    Ok(Review { view, saved, head })
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
            can_restore: true,
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
                lifecycle: ReviewLifecycleKind::Update,
                comparison: Some(ReviewComparisonRole::Head),
                settings: vec![row],
                data: None,
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
    }
}

/// A variable change as `get` shows values: text, or `{"secret": true}`.
fn shown_env(value: serde_json::Value) -> serde_json::Value {
    match value.get("kind").and_then(serde_json::Value::as_str) {
        Some("secret") => json!({ "secret": true }),
        Some(_) => value.get("value").cloned().unwrap_or_default(),
        None => value,
    }
}

/// Refuse unless `version` still names this review; the refusal carries the fresh one.
pub(crate) fn check(review: &Review, version: Option<&str>) -> Result<(), RpcError> {
    match version {
        Some(version) if version != review.view.version => Err(error::conflict(
            "The Environment changed after this review. Review the latest changes and try again",
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
) -> Result<(Revision, bool), RpcError> {
    let intent = canonicalize_environment_intent(intent);
    if let Some(latest) = latest
        && latest.intent == intent
    {
        return Ok((latest.revision, false));
    }
    let revision = Revision(latest.map_or(0, |latest| latest.revision.0) + 1);
    tx.execute(
        "INSERT INTO config_saved (environment_id, revision, organization_id, intent) \
         VALUES (?1, ?2, ?3, ?4)",
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
            who.organization.as_str().into(),
            serde_json::to_string(&intent)
                .expect("Saved State is JSON")
                .as_str()
                .into(),
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
    let mut intent = empty(&environment.working);
    for row in rows {
        let corrupt = |_| error::corrupt("Node Introduction");
        if row.text(1)? == "volume" {
            intent
                .volumes
                .push(serde_json::from_str(row.text(0)?).map_err(corrupt)?);
        } else {
            intent
                .services
                .push(serde_json::from_str(row.text(0)?).map_err(corrupt)?);
        }
    }
    Ok(intent)
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
    let intent = serde_json::from_str(row.text(1)?)
        .ok()
        .and_then(|value| parse_environment_intent(value).ok())
        .ok_or_else(|| error::corrupt("Saved State"))?;
    Ok(Some(Saved {
        revision: Revision(u64::try_from(row.int(0)?).map_err(|_| error::corrupt("revision"))?),
        intent: canonicalize_environment_intent(intent),
    }))
}

pub(crate) fn empty(like: &SavedEnvironmentIntent) -> SavedEnvironmentIntent {
    SavedEnvironmentIntent {
        version: like.version,
        environment_slug: like.environment_slug.clone(),
        services: Vec::new(),
        volumes: Vec::new(),
    }
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
