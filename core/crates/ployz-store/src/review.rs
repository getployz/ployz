//! Review: the Environment Change Set `diff` shows, the Saved revisions Publish
//! writes, and the version that binds a review to the bases it was computed on.
//!
//! A version names the Working revision, the latest Saved revision and the Head
//! (what the Change Set compares against). Publish and Discard recompute the review
//! under the Environment's lock and refuse a version that no longer matches.

use ployz_core::RpcError;
use ployz_core::config::{
    ChangeSetInput, ReviewComparisonRole, ReviewLifecycleKind, ReviewNodeIdentity,
    ReviewNodeProjection, ReviewStateProjection, SavedEnvironmentIntent, ServiceSettingChange,
    canonicalize_environment_intent, compile_environment_intent, parse_environment_intent,
    project_environment_changes,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, Revision};
use crate::scope::{Environment, EnvironmentSummary, revision_param};
use crate::settings::ServiceSetting;
use crate::storage::Tx;

/// An Environment's staged changes, grouped by node, and the version to act on them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    let head = head(environment);
    let introductions = introductions(tx, environment)?;
    let project = |token: &str, intent: &SavedEnvironmentIntent| projection(id, token, intent);
    let changes = project_environment_changes(ChangeSetInput {
        working: project("working", &environment.working),
        applied: project(&head.token, &head.intent),
        saved: saved.as_ref().map(|saved| project("saved", &saved.intent)),
        submitted: None,
        node_introductions: project("introductions", &introductions),
    })
    .map_err(|_| error::corrupt("Environment document"))?;
    let name = |node: &ReviewNodeIdentity| {
        [&environment.working, &head.intent]
            .into_iter()
            .flat_map(|intent| &intent.services)
            .find(|service| service.id == node.id)
            .map(|service| service.slug.clone())
            .unwrap_or_default()
    };
    let view = DiffView {
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
                let settings = group
                    .settings
                    .into_iter()
                    .map(|mut row| {
                        row.path = match ServiceSetting::ALL
                            .into_iter()
                            .find(|setting| setting.field() == row.path)
                        {
                            Some(setting) => format!("{name}.{}", setting.name()),
                            None => format!("{name}.{}", row.path),
                        };
                        row
                    })
                    .collect();
                NodeChange {
                    node: group.node,
                    name,
                    lifecycle: group.lifecycle,
                    comparison: group.comparison,
                    settings,
                }
            })
            .collect(),
    };
    Ok(Review { view, saved, head })
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
        "SELECT node FROM config_node_introduction WHERE environment_id = ?1 AND node_type = 'service'",
        &[environment.summary.id.as_str().into()],
    )?;
    let mut intent = empty(&environment.working);
    for row in rows {
        intent.services.push(
            serde_json::from_str(row.text(0)?).map_err(|_| error::corrupt("Node Introduction"))?,
        );
    }
    Ok(intent)
}

fn head(environment: &Environment) -> Head {
    // ponytail: nothing deploys from the Store yet, so Head is empty. Deployments
    // supply the submitted revision, else Applied State, and their token.
    Head {
        token: "none".to_owned(),
        intent: empty(&environment.working),
    }
}

fn latest_saved(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Option<Saved>, RpcError> {
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

fn empty(like: &SavedEnvironmentIntent) -> SavedEnvironmentIntent {
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
