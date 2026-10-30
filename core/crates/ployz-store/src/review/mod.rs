//! Review: the Environment Change Set `diff` shows, the Saved revisions Publish
//! writes, and the version that binds a review to the bases it was computed on.
//!
//! A version names the Working revision, the latest Saved revision and the Head
//! (what the Change Set compares against). Publish and Discard recompute the review
//! under the Environment's lock and refuse a version that no longer matches.

pub(crate) mod approval;
mod authored;
pub(crate) mod diff;
pub(crate) mod history;
pub(crate) mod publish;
pub(crate) mod sealed;

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{
    At, ChangeKind, ChangeSetInput, EnvironmentNodeType, ReviewComparisonRole, ReviewLifecycleKind,
    ReviewNodeIdentity, ReviewNodeProjection, ReviewStateProjection, RowId, SavedConfigIntent,
    SavedEnvironmentIntent, SavedServiceIntent, SavedVariableValue, SavedVolumeIntent,
    ServiceGitBranch, ServiceSettingChange, ServiceSource, Setting,
    canonicalize_environment_intent, compile_environment_intent, project_environment_changes,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

pub use approval::{DestructiveEffect, DestructiveKind, LostAfter};
pub(crate) use approval::{
    Gate, Moving, Pruning, TakingOff, approve, approve_move, approve_retry, move_asks,
};

use crate::Actor;
use crate::error;
use crate::git::GitSetting;
use crate::id::{EnvironmentId, Hostname, Revision};
use crate::scope::{Environment, EnvironmentSummary, revision_param};
use crate::settings::{ServiceSetting, SettingPath, shown};
use crate::storage::Tx;

/// An Environment's staged changes, grouped by node, and the version to act on them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct DiffView {
    pub environment: EnvironmentSummary,
    /// Pass back to `deploy` or `discard` to act on exactly this review.
    pub version: String,
    /// The latest Saved revision, if anything was ever published.
    pub saved: Option<Revision>,
    /// Whether Saved State already holds this Working State.
    #[serde(rename = "holds_staged")]
    pub published: bool,
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
    /// Whether every Deploy of the latest Saved revision ran nothing, so none of what it
    /// saved reached a Server. Its draft reversals are changes that never ran.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub saved_ran_nothing: bool,
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
    /// In a Branch, the Setup Commands the next Deploy runs: those of its Services
    /// that haven't deployed in it yet.
    #[serde(default)]
    #[ts(as = "Option<Vec<crate::SetupCommand>>", optional)]
    pub setup: Vec<crate::SetupCommand>,
    /// What deploying Working State destroys of Applied State and of what the Deployment
    /// in flight puts in place, sorted; empty when nothing.
    #[serde(default)]
    #[ts(as = "Option<Vec<DestructiveEffect>>", optional)]
    pub effects: BTreeSet<DestructiveEffect>,
    /// Each reference in Working State to a Service or variable that isn't there.
    #[serde(default)]
    #[ts(as = "Option<Vec<crate::BrokenReference>>", optional)]
    pub broken: Vec<crate::BrokenReference>,
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
        by_name(&mut changes);
        changes
    }

    /// How many changes [`Self::review_changes`] lists, counting each node and each Setting.
    #[must_use]
    pub fn review_count(&self) -> usize {
        self.review_changes()
            .iter()
            .map(|change| {
                change.settings.len() + usize::from(change.lifecycle != ReviewLifecycleKind::Update)
            })
            .sum()
    }

    /// The draft reversals on nodes with no deploy change: saved changes the draft no
    /// longer holds, which a discard naming the node brings back.
    #[must_use]
    pub fn reversals(&self) -> Vec<NodeChange> {
        self.draft_changes
            .iter()
            .filter(|draft| !self.changes.iter().any(|change| change.node == draft.node))
            .cloned()
            .collect()
    }
}

/// What happens to one node, and its changed Settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct NodeChange {
    /// The node's type and ID.
    #[serde(flatten)]
    pub node: ReviewNodeIdentity,
    pub name: String,
    /// Its own Sync row, which joins it to what moved it.
    pub row: RowId,
    pub lifecycle: ReviewLifecycleKind,
    /// What `settings` compare against; `None` when nothing exists to compare.
    pub comparison: Option<ReviewComparisonRole>,
    /// Its changed Settings, by `SERVICE.SETTING` path.
    pub settings: Vec<ServiceSettingChange>,
    /// What the change does to Volume data: `deleted` for a Volume it removes that is
    /// deployed or mounted by a Deployment in flight, `kept` for a Service that stops
    /// mounting a Volume that stays.
    pub data: Option<DataEffect>,
    /// For a Config changed in place, the deployed Services mounting it: they restart
    /// to take its new files.
    #[serde(default)]
    pub restarts: Vec<ployz_core::ServiceName>,
}

impl NodeChange {
    /// Each Setting row of `changes`, by path, as people read them shown together.
    #[must_use]
    pub fn labels(changes: &[Self]) -> BTreeMap<String, String> {
        let paths: Vec<String> = changes
            .iter()
            .flat_map(|change| change.settings.iter().map(|row| row.path.clone()))
            .collect();
        let shown = Self::shown_paths(changes, &paths);
        paths.into_iter().zip(shown).collect()
    }

    /// `paths`, shown together, as people read them: each Config, and each mount
    /// whichever Service holds it, by the name `changes` know its id by, as
    /// [`ployz_core::config::shown_paths`] reads them.
    #[must_use]
    pub fn shown_paths(changes: &[Self], paths: &[String]) -> Vec<String> {
        Self::shown_paths_by(paths, |id| Self::config_named(changes, id))
    }

    /// `paths`, shown together, naming each Config by `name(id)`.
    #[must_use]
    pub fn shown_paths_by(
        paths: &[String],
        name: impl Fn(&str) -> Option<ployz_core::ConfigName>,
    ) -> Vec<String> {
        let named: Vec<_> = paths
            .iter()
            .map(|path| {
                let id = path
                    .split_once("configs.@")
                    .map(|(_, rest)| rest.split_once('.').map_or(rest, |(id, _)| id));
                (path.as_str(), id.and_then(&name))
            })
            .collect();
        let named: Vec<_> = named
            .iter()
            .map(|(path, name)| (*path, name.as_ref()))
            .collect();
        ployz_core::config::shown_paths(&named)
    }

    /// The name `changes` know the Config `id` by, from its own change or a mount of it.
    #[must_use]
    pub fn config_named(changes: &[Self], id: &str) -> Option<ployz_core::ConfigName> {
        changes.iter().find_map(|change| {
            if change.node.node_type == EnvironmentNodeType::Config && change.node.id == id {
                return ployz_core::ConfigName::parse(&change.name).ok();
            }
            change.settings.iter().find_map(|row| {
                let (_, mounted) = row.path.rsplit_once(".configs.@")?;
                (mounted == id).then(|| row.config_name.clone()).flatten()
            })
        })
    }
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
    /// The Volume losses accepted when this revision was saved, each with the
    /// Docker Volumes seen holding its data then. They cover the next Deploy of
    /// this revision until the evidence changes.
    pub(crate) accepted: Vec<crate::VolumeLoss>,
}

/// What the Change Set compares Working State against: the submitted Deployment's
/// Saved revision, else Applied State.
pub(crate) struct Head {
    pub(crate) token: String,
    pub(crate) intent: SavedEnvironmentIntent,
    pub(crate) applied: SavedEnvironmentIntent,
    /// Every Deployment in flight, newest first.
    pub(crate) in_flight: Vec<InFlight>,
    /// The Services a Deployment of unknown outcome since the last applied Deployment
    /// of every Service may have started: its runner lost track of it, so they may run though Applied State
    /// lacks them. A Service comes once for each such Deployment.
    pub(crate) may_run: Vec<SavedServiceIntent>,
}

/// What a Deployment in flight puts in place: its target nodes as its Saved revision
/// holds them.
pub(crate) struct InFlight {
    pub(crate) cluster_domain: Option<Hostname>,
    pub(crate) services: Vec<SavedServiceIntent>,
    pub(crate) volumes: Vec<SavedVolumeIntent>,
    pub(crate) configs: Vec<SavedConfigIntent>,
    /// The ID of each node it removes.
    pub(crate) removes: BTreeSet<String>,
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
    let mut introductions = introductions(tx, environment)?;
    sealed::hide_introduced(tx, id, &mut introductions, &environment.working)?;
    let deployed = approval::deployed(&head);
    let mut view = compare(
        environment,
        saved.as_ref(),
        &head,
        &deployed,
        &introductions,
    )?;
    let cluster_domains = if crate::domain::has_generated(&deployed) {
        crate::deployment::cluster_domains(tx, id, &head)?
    } else {
        std::collections::BTreeMap::new()
    };
    view.effects = approval::destructive_effects(&deployed, &environment.working, &cluster_domains);
    let saved_intent = saved.as_ref().map(|saved| &saved.intent);
    let names = crate::config_item::names_in(
        &[&environment.working, &head.intent, &head.applied]
            .into_iter()
            .chain(saved_intent)
            .collect::<Vec<_>>(),
    );
    view.broken = crate::rules::broken_in(tx, environment, names)?;
    let baseline = saved.as_ref().map_or_else(
        || crate::scope::empty(environment.summary.name.as_str()),
        |saved| saved.intent.clone(),
    );
    let draft = saved_comparison(environment, &baseline)?;
    view.draft_changes = draft.changes;
    view.draft_count = draft.total_count;
    // Only staged changes or draft reversals are worded by it.
    if let Some(saved) = &saved
        && (view.total_count > 0 || view.draft_count > 0)
    {
        view.saved_ran_nothing = crate::deployment::only_ran_nothing(tx, id, saved.revision)?;
    }
    by_name(&mut view.changes);
    by_name(&mut view.draft_changes);
    Ok(Review { view, saved, head })
}

/// Services, then Volumes, then Configs, each by name. A node removed and one recreated
/// under its name list removal first, then update, then create; the id only settles what
/// nothing else tells apart.
fn by_name(changes: &mut [NodeChange]) {
    let step = |lifecycle: ReviewLifecycleKind| match lifecycle {
        ReviewLifecycleKind::Delete => 0,
        ReviewLifecycleKind::Update => 1,
        ReviewLifecycleKind::Create => 2,
    };
    changes.sort_by(|a, b| {
        (a.node.node_type, &a.name, step(a.lifecycle), &a.node.id).cmp(&(
            b.node.node_type,
            &b.name,
            step(b.lifecycle),
            &b.node.id,
        ))
    });
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
    deployed: &SavedEnvironmentIntent,
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
        saved_ran_nothing: false,
        incoming: Vec::new(),
        follow_hints: Vec::new(),
        included: Vec::new(),
        setup: Vec::new(),
        effects: BTreeSet::new(),
        broken: Vec::new(),
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
                let data =
                    data_effect(&group.node, group.lifecycle, deployed, &environment.working);
                let restarts =
                    restarts(&group.node, group.lifecycle, &environment.working, deployed)?;
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
    sources(&mut view, &environment.working);
    renames(&mut view, &environment.working, &head.intent);
    Ok(view)
}

/// Each Service's Setting changes among `changes`, with the Service's node ID.
pub(crate) fn service_rows(
    changes: &mut [NodeChange],
) -> impl Iterator<Item = (&str, &mut ServiceSettingChange)> {
    changes
        .iter_mut()
        .filter(|change| change.node.node_type == EnvironmentNodeType::Service)
        .flat_map(|NodeChange { node, settings, .. }| {
            let id = node.id.as_str();
            settings.iter_mut().map(move |row| (id, row))
        })
}

/// What the destructive review gates.
#[derive(Clone, Copy)]
pub(crate) enum Shipping<'a> {
    /// Deploying these Services of Working State, none deploying all of it, with
    /// the new upload the Deploy brings, if any.
    Deploy {
        services: &'a [ployz_core::ServiceName],
        upload: Option<&'a ployz_core::UploadDigest>,
    },
    /// Publishing Working State without deploying it.
    Publish,
}

/// The Volume data shipping `target` would delete, each accepted by name in
/// `accepted` under the reviewed `version`, or covered by `consent` saved with it.
/// A full Deploy removes what Applied State holds, or a Deployment in flight puts in
/// place, and `target` dropped; publishing puts that removal in Saved State for the
/// next Deploy, so it is reviewed then too, and the reviewed losses are saved as
/// that Deploy's consent.
///
/// # Errors
/// `confirmation_required` until every loss is accepted; `unavailable` when the
/// Servers weren't asked what they hold.
pub(crate) fn destructive(
    tx: &mut dyn Tx,
    who: &Actor,
    review: &Review,
    (target, namespace, shipping): (
        &SavedEnvironmentIntent,
        &ployz_core::Namespace,
        Shipping<'_>,
    ),
    (version, accepted, consent): (Option<&str>, &[crate::id::VolumeName], &[crate::VolumeLoss]),
    observed: Option<&crate::VolumeObservation>,
) -> Result<Vec<crate::VolumeLoss>, RpcError> {
    // A Save reviews what Saved State holds pending even when it saves nothing
    // new, so it can record or renew the consent the next Deploy reads.
    let reviews = match shipping {
        Shipping::Publish => true,
        Shipping::Deploy { services, .. } => {
            services.is_empty()
                || review
                    .saved
                    .as_ref()
                    .is_none_or(|saved| saved.intent != *target)
        }
    };
    let removed = match reviews {
        true => crate::removal::removed(&approval::deployed(&review.head), target, namespace)?,
        false => Vec::new(),
    };
    crate::removal::review(
        tx,
        who,
        &review.view.environment.id,
        (&review.view.version, version),
        removed,
        observed,
        (accepted, consent),
    )
}

/// Add to `stale`, the conflict for a stale version, what the fresh review deletes:
/// `probe` is [`destructive`] run at no version and with nothing accepted. When it
/// asks to confirm a loss, its version, Volumes and names to accept go in
/// `details.volume_loss`, so one retry with both goes through. Anything else it
/// says leaves the conflict as it was.
pub(crate) fn with_loss(
    mut stale: RpcError,
    probe: Result<Vec<crate::VolumeLoss>, RpcError>,
) -> RpcError {
    if let Err(asked) = probe
        && asked.code == ployz_core::RpcErrorCode::ConfirmationRequired
        && let Some(details) = stale.details.as_object_mut()
    {
        details.insert(
            "volume_loss".to_owned(),
            json!({
                "version": asked.details.get("version"),
                "volumes": asked.details.get("volumes"),
                "accept": asked.details.get("accept"),
            }),
        );
    }
    stale
}

/// The losses saved with `saved` as consent for its next Deploy.
pub(crate) fn consent(saved: Option<&Saved>) -> &[crate::VolumeLoss] {
    saved.map_or(&[], |saved| saved.accepted.as_slice())
}

/// Core compares a new Service with its introduction, which holds the source it was
/// added with, so its create gets [`introduced_source`]. One changed since already has
/// its row.
fn sources(view: &mut DiffView, working: &SavedEnvironmentIntent) {
    for change in &mut view.changes {
        if change.lifecycle != ReviewLifecycleKind::Create
            || change.node.node_type != EnvironmentNodeType::Service
        {
            continue;
        }
        let Some(service) = working
            .services
            .iter()
            .find(|service| service.id == change.node.id)
        else {
            continue;
        };
        let rows: Vec<_> =
            introduced_source(&service.slug, &service.lineage_id, &service.config.source)
                .into_iter()
                .filter(|row| !change.settings.iter().any(|shown| shown.row == row.row))
                .collect();
        view.total_count += rows.len();
        change.settings.splice(0..0, rows);
    }
}

/// What a Service named `slug` shows of `source` as it is created: what it runs, as
/// `set` names it: `web.image`, or `web.repository` and `web.branch`.
#[must_use]
pub fn introduced_source(
    slug: &str,
    lineage: &str,
    source: &ServiceSource,
) -> Vec<ServiceSettingChange> {
    let parts = match source {
        ServiceSource::Image { image, .. } => {
            vec![(ServiceSetting::Image, Setting::Source, json!(image))]
        }
        ServiceSource::Git {
            repository, branch, ..
        } => {
            let mut parts = vec![(
                ServiceSetting::Git(GitSetting::Repository),
                Setting::Source,
                json!(repository),
            )];
            if let ServiceGitBranch::Connected { name } = branch {
                parts.push((
                    ServiceSetting::Git(GitSetting::Branch),
                    Setting::Branch,
                    json!(name),
                ));
            }
            parts
        }
        ServiceSource::Empty { .. } => Vec::new(),
    };
    parts
        .into_iter()
        .map(|(setting, at, after)| ServiceSettingChange {
            path: format!("{slug}.{}", setting.name()),
            kind: ChangeKind::Add,
            before: serde_json::Value::Null,
            after,
            // Discarding the Service takes it; alone it has nothing to go back to.
            can_restore: false,
            row: Some(RowId::of(lineage, At::Setting(at))),
            config_name: None,
            hostnames: None,
        })
        .collect()
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
            hostnames: None,
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

/// A Volume `deployed` holds, removed, deletes data; a Service removed, or one of
/// its mounts dropped, keeps a Volume `working` still holds.
fn data_effect(
    node: &ReviewNodeIdentity,
    lifecycle: ReviewLifecycleKind,
    deployed: &SavedEnvironmentIntent,
    working: &SavedEnvironmentIntent,
) -> Option<DataEffect> {
    match node.node_type {
        EnvironmentNodeType::Volume => (lifecycle == ReviewLifecycleKind::Delete
            && deployed
                .volumes
                .iter()
                .any(|volume| volume.resource_id == node.id))
        .then_some(DataEffect::Deleted),
        EnvironmentNodeType::Service => approval::detached(deployed, working, &node.id)
            .next()
            .map(|_| DataEffect::Kept),
        EnvironmentNodeType::Config => None,
    }
}

/// The Services of `working` that mount Config `node`, updated, and already run in
/// `deployed`.
fn restarts(
    node: &ReviewNodeIdentity,
    lifecycle: ReviewLifecycleKind,
    working: &SavedEnvironmentIntent,
    deployed: &SavedEnvironmentIntent,
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
                && deployed.services.iter().any(|old| old.id == service.id)
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
                .is_some_and(|rest| rest.starts_with(':'))
    };
    match version {
        Some(version) if !names_it(version) => Err(error::conflict(
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
    (message, accepted): (Option<&str>, &[crate::VolumeLoss]),
) -> Result<(Revision, bool), RpcError> {
    let intent = canonicalize_environment_intent(intent);
    crate::variables::refuse_awaiting(&intent)?;
    crate::volume::check_storage(
        tx,
        environment,
        &intent.volumes,
        crate::volume::StorageRule::Replay,
    )?;
    if let Some(latest) = latest
        && latest.intent == intent
    {
        return Ok((latest.revision, false));
    }
    guard_saving(tx, environment, latest, &intent)?;
    let revision = Revision(latest.map_or(0, |latest| latest.revision.0) + 1);
    let accepted = (!accepted.is_empty())
        .then(|| serde_json::to_string(accepted).expect("Volume losses are JSON"));
    let changed = latest.map_or_else(Vec::new, |latest| value_changes(&latest.intent, &intent));
    let changed = (!changed.is_empty())
        .then(|| serde_json::to_string(&changed).expect("value changes are JSON"));
    tx.execute(
        "INSERT INTO config_saved (environment_id, revision, organization_id, intent, message, saved_by, saved_at, accepted_losses, value_changes) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
            accepted.as_deref().into(),
            changed.as_deref().into(),
        ],
    )?;
    scrub_sealed(tx, environment)?;
    Ok((revision, true))
}

/// The `[service_id, variable_id]` of each variable whose value `saved` changed from
/// `previous`, found as [`Scrub`] finds it: by ID, or else by key.
fn value_changes(
    previous: &SavedEnvironmentIntent,
    saved: &SavedEnvironmentIntent,
) -> Vec<(String, String)> {
    let mut changed = Vec::new();
    for service in &saved.services {
        let Some(was) = previous.services.iter().find(|was| was.id == service.id) else {
            continue;
        };
        for variable in &service.variables {
            let before = was
                .variables
                .iter()
                .find(|was| was.id == variable.id)
                .or_else(|| was.variables.iter().find(|was| was.key == variable.key));
            if before.is_some_and(|before| {
                before.value != variable.value
                    || before.value_fingerprint != variable.value_fingerprint
            }) {
                changed.push((service.id.clone(), variable.id.clone()));
            }
        }
    }
    changed
}

/// [`guard_source`] for saving `intent` over Saved State `latest`. Shipping asks it
/// before an approval, so no approval is asked of a write the caller may not make.
pub(crate) fn guard_saving(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    latest: Option<&Saved>,
    intent: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let before = latest.map_or(&[][..], |latest| latest.intent.services.as_slice());
    guard_source(tx, environment, before, &intent.services)
}

/// Refuse a caller who may not protect when `after` changes what a GitHub merge ships
/// into Protected Environment `environment`: the repository or deploy branch of a
/// Service in `before`, or a Git Service it adds. Removing a Service is removal's to
/// approve. Staging and publishing both ask, so no command brings a source in.
pub(crate) fn guard_source(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    before: &[SavedServiceIntent],
    after: &[SavedServiceIntent],
) -> Result<(), RpcError> {
    crate::storage::admit_protect(tx, |tx| touches_source(tx, environment, before, after))
}

/// The Services whose source `after` changes in `environment`, when it is Protected.
pub(crate) fn touches_source(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    before: &[SavedServiceIntent],
    after: &[SavedServiceIntent],
) -> Result<Option<serde_json::Value>, RpcError> {
    let changed: Vec<&str> = after
        .iter()
        .filter(
            |service| match before.iter().find(|old| old.id == service.id) {
                Some(old) => git_source(old) != git_source(service),
                None => git_source(service).is_some(),
            },
        )
        .map(|service| service.slug.as_str())
        .collect();
    if changed.is_empty() || !crate::scope::protected(tx, environment)? {
        return Ok(None);
    }
    Ok(Some(json!({ "services": changed })))
}

/// The repository and deploy branch a GitHub merge ships `service` from, if any.
fn git_source(service: &SavedServiceIntent) -> Option<(u64, &ServiceGitBranch)> {
    match &service.config.source {
        ServiceSource::Git {
            repository_id,
            branch,
            ..
        } => Some((*repository_id, branch)),
        ServiceSource::Empty { .. } | ServiceSource::Image { .. } => None,
    }
}

/// Replace the consent saved with `revision`, which an unchanged Save renews.
pub(crate) fn reconsent(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
    accepted: &[crate::VolumeLoss],
) -> Result<(), RpcError> {
    let accepted = (!accepted.is_empty())
        .then(|| serde_json::to_string(accepted).expect("Volume losses are JSON"));
    tx.execute(
        "UPDATE config_saved SET accepted_losses = ?3 WHERE environment_id = ?1 AND revision = ?2",
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
            accepted.as_deref().into(),
        ],
    )?;
    Ok(())
}

/// Sealing a value removes every earlier copy of it from History: plain text
/// and older ciphertexts both become the sealed value, matched by stable ID so
/// a rename keeps it, and plain text of a value sealed then deleted becomes
/// sealed without a value. A revision a deployment still runs keeps its copy
/// until that deployment leaves flight, because it records what it applied
/// from that revision when it finishes.
pub(crate) struct Scrub {
    latest: Option<Saved>,
    ever: sealed::EverSealed,
    running: BTreeSet<Revision>,
}

impl Scrub {
    pub(crate) fn load(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Self, RpcError> {
        let ever = sealed::EverSealed::load(tx, environment.as_str())?;
        if ever.is_empty() {
            return Ok(Self {
                latest: None,
                ever,
                running: BTreeSet::new(),
            });
        }
        let running = tx
            .query(
                &format!(
                    "SELECT saved_revision FROM config_deployment WHERE environment_id = ?1 AND {}",
                    crate::deployment::in_flight_sql()
                ),
                &[environment.as_str().into()],
            )?
            .iter()
            .map(|row| row.number(0, "Deployment revision").map(Revision))
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self {
            latest: latest_saved(tx, environment)?,
            ever,
            running,
        })
    }

    pub(crate) fn ever(&self) -> &sealed::EverSealed {
        &self.ever
    }

    /// Remove from `saved`, the version saved at `revision`, what sealing took
    /// out of History; whether anything changed.
    pub(crate) fn apply(&self, revision: Revision, saved: &mut SavedEnvironmentIntent) -> bool {
        let Some(latest) = &self.latest else {
            return false;
        };
        if revision >= latest.revision || self.running.contains(&revision) {
            return false;
        }
        let mut scrubbed = false;
        for service in &mut saved.services {
            let now = latest
                .intent
                .services
                .iter()
                .find(|now| now.id == service.id)
                .map(|now| &now.variables);
            for was in &mut service.variables {
                if was.value == SavedVariableValue::SecretWithoutValue {
                    continue;
                }
                let now = now.and_then(|now| {
                    now.iter()
                        .find(|now| now.id == was.id)
                        .or_else(|| now.iter().find(|now| now.key == was.key))
                });
                let (value, fingerprint) = match now {
                    Some(now) if sealed::sealed(&now.value) => {
                        (&now.value, now.value_fingerprint.as_str())
                    }
                    // Added again plain since: only this very variable was sealed.
                    Some(now)
                        if now.id != was.id
                            && !sealed::sealed(&was.value)
                            && self.ever.holds_id(&service.id, was) =>
                    {
                        (&SavedVariableValue::SecretWithoutValue, "")
                    }
                    None if !sealed::sealed(&was.value) && self.ever.holds(&service.id, was) => {
                        (&SavedVariableValue::SecretWithoutValue, "")
                    }
                    Some(_) | None => continue,
                };
                if was.value != *value {
                    was.value = value.clone();
                    fingerprint.clone_into(&mut was.value_fingerprint);
                    // A sealed value is never generated.
                    was.generated = None;
                    scrubbed = true;
                }
            }
        }
        scrubbed
    }
}

/// Write [`Scrub`] into every earlier saved version, and keep no sealed plain value
/// in a Sync base, so storage holds no copy History hides.
pub(crate) fn scrub_sealed(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    crate::branch::scrub_bases(tx, environment.as_str())?;
    let scrub = Scrub::load(tx, environment)?;
    let Some(latest) = &scrub.latest else {
        return Ok(());
    };
    let rows = tx.query(
        "SELECT revision, intent FROM config_saved WHERE environment_id = ?1 AND revision < ?2",
        &[
            environment.as_str().into(),
            revision_param(latest.revision)?.into(),
        ],
    )?;
    for row in rows {
        let earlier = Revision(row.number(0, "revision")?);
        let mut saved = row.intent(1, "Saved State")?;
        if scrub.apply(earlier, &mut saved) {
            tx.execute(
                "UPDATE config_saved SET intent = ?3 WHERE environment_id = ?1 AND revision = ?2",
                &[
                    environment.as_str().into(),
                    revision_param(earlier)?.into(),
                    serde_json::to_string(&saved)
                        .expect("Saved State is JSON")
                        .as_str()
                        .into(),
                ],
            )?;
        }
    }
    Ok(())
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
        "SELECT revision, intent, accepted_losses FROM config_saved WHERE environment_id = ?1 \
         ORDER BY revision DESC LIMIT 1",
        &[environment.as_str().into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let mut intent = canonicalize_environment_intent(row.intent(1, "Saved State")?);
    // The revision keeps the size it asked for as history; read as the latest
    // Saved State, it says the size the Servers hold, as Head does.
    crate::volume::size::held(tx, environment, &mut intent.volumes)?;
    let accepted = match row.optional_text(2)? {
        Some(json) => {
            serde_json::from_str(json).map_err(|_| error::corrupt("accepted Volume losses"))?
        }
        None => Vec::new(),
    };
    Ok(Some(Saved {
        revision: Revision(row.number(0, "revision")?),
        intent,
        accepted,
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
