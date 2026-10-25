//! Publish (Working State → Saved State) and Discard (undo staged changes at any
//! level). Both recompute the review under the Environment's lock and refuse a
//! stale version; neither rebases.

use ployz_core::RpcError;
use ployz_core::config::{
    EnvironmentNodeType, ReviewLifecycleKind, ServiceSettingChange, canonicalize_environment_intent,
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::{Revision, VolumeName};
use crate::review;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::SettingPath;
use crate::storage::Tx;
use crate::{Actor, Trusted, deployment};

/// Put Working State in Saved State without deploying it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Refuse with `conflict` unless this is still the latest `diff` version, or the
    /// version a refusal to delete data handed back.
    #[serde(default)]
    pub version: Option<String>,
    /// What this saved revision changes, in the saver's words.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub message: Option<String>,
    /// Deployed Volumes whose removal it may publish, by name: the next full Deploy
    /// deletes their data. Publishing one refuses with `confirmation_required` unless
    /// it names each one and passes the `version` that refusal handed back.
    #[serde(default)]
    #[ts(as = "Option<Vec<VolumeName>>", optional)]
    pub accept_volume_loss: Vec<VolumeName>,
    /// Caller-minted: this save sent again with the same ID returns the first one's
    /// answer instead of saving what is staged by then.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub id: Option<String>,
}

/// The Saved revision Working State is now in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Published {
    pub environment: EnvironmentSummary,
    /// The Saved revision that now holds Working State; none when nothing was ever
    /// published and nothing is staged.
    pub saved: Option<Revision>,
    /// False when Saved State already held it, or nothing is staged.
    pub created: bool,
}

/// Undo staged changes: all of them, one node's, or one Setting, variable or mount's.
/// It names a [`SettingPath`] rather than a `RowId`, as the diff it undoes does:
/// it is a whole-setting action, not one of a Sync view's rows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Discard {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// The comparison to return to. Head preserves the deployment baseline; Saved
    /// abandons a draft reversal; Review returns everything to Saved, or to Head
    /// before the first Save.
    /// `None` returns everything to Head, and a path to its row's baseline: Saved
    /// when only a draft reversal lists it, else Head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub target: Option<DiscardTarget>,
    /// `SERVICE`, `volumes.VOLUME`, `configs.CONFIG`, `SERVICE.SETTING`,
    /// `SERVICE.env.KEY`, `SERVICE.mounts.VOLUME` or `SERVICE.configs.CONFIG`; none
    /// discards everything.
    #[serde(default)]
    pub path: Option<SettingPath>,
    /// Refuse with `conflict` unless this is still the latest `diff` version.
    #[serde(default)]
    pub version: Option<String>,
}

/// The reviewed baseline a Discard restores into the draft.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DiscardTarget {
    #[default]
    Head,
    Saved,
    Review,
}

/// The Environment after a discard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Discarded {
    pub environment: EnvironmentSummary,
    /// The latest Saved revision, unchanged by Discard.
    pub saved: Option<Revision>,
    /// Whether every Deploy of that revision ran nothing, as [`DiffView::saved_ran_nothing`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub saved_ran_nothing: bool,
    /// False when nothing it names was staged.
    pub changed: bool,
    /// The draft reversals left after it, each with the discard that brings it back.
    #[serde(default)]
    #[ts(as = "Option<Vec<Reversal>>", optional)]
    pub reversed: Vec<Reversal>,
    /// Each staged change it dropped, by path: a Service, Volume or Config it no
    /// longer makes or removes, or a changed Setting.
    #[serde(default)]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub dropped: Vec<String>,
}

/// A Saved change the draft no longer holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Reversal {
    #[serde(flatten)]
    pub change: super::NodeChange,
    pub way_back: WayBack,
}

/// The discard that brings a [`Reversal`] back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct WayBack {
    /// The path to discard. `None` when another node in the draft has its type and
    /// name, so its path names that node too: only a discard to Saved brings it back.
    pub path: Option<SettingPath>,
    /// True when it also drops the draft's other unsaved changes.
    pub drops_unsaved: bool,
}

impl WayBack {
    fn of(change: &super::NodeChange, draft: &[super::NodeChange]) -> Self {
        // A same-named node in the draft is never in Saved, so it is a draft change.
        let named_twice = draft.iter().any(|other| {
            other.node != change.node
                && other.node.node_type == change.node.node_type
                && other.name == change.name
        });
        let path = (!named_twice)
            .then(|| match change.node.node_type {
                EnvironmentNodeType::Service => change.name.clone(),
                EnvironmentNodeType::Volume => format!("volumes.{}", change.name),
                EnvironmentNodeType::Config => format!("configs.@{}", change.node.id),
            })
            .and_then(|path| SettingPath::parse(&path).ok());
        Self {
            drops_unsaved: path.is_none(),
            path,
        }
    }
}

pub(crate) fn publish(
    tx: &mut dyn Tx,
    who: &Actor,
    publish: &Publish,
    trusted: &Trusted,
) -> Result<Published, RpcError> {
    let mut environment = scope::lock(tx, who, &publish.environment)?;
    trusted.writes(&environment.summary.name)?;
    crate::removal::marked(tx, &environment)?;
    let review = review::review(tx, &environment)?;
    if let Err(stale) = review::check(&review, publish.version.as_deref()) {
        let target = canonicalize_environment_intent(environment.working.clone());
        let probe =
            deployment::namespace(tx, who, &environment.summary, false).and_then(|namespace| {
                review::destructive(
                    who,
                    &review,
                    (&target, &namespace, review::Shipping::Publish),
                    (None, &[], review::consent(review.saved.as_ref())),
                    trusted.volumes.as_ref(),
                )
            });
        return Err(review::with_loss(stale, probe));
    }
    // A manual Save ends the draft's proposals: what they brought is its own now.
    let consumed = crate::branch::end_included(tx, &environment.summary.id, None)?;
    if review.saved.is_none()
        && canonicalize_environment_intent(environment.working.clone())
            == canonicalize_environment_intent(review.head.intent.clone())
    {
        // Nothing staged: Head already is Working State. Proposals ending still
        // moves the revision, so a review of them is stale.
        if consumed {
            scope::persist_working(tx, &mut environment)?;
        }
        return Ok(Published {
            environment: environment.summary,
            saved: None,
            created: false,
        });
    }
    // Saved State then holds the removal, which the next full Deploy ships: the
    // same destructive review, before anything is saved.
    let target = canonicalize_environment_intent(environment.working.clone());
    let namespace = deployment::namespace(tx, who, &environment.summary, false)?;
    let losses = review::destructive(
        who,
        &review,
        (&target, &namespace, review::Shipping::Publish),
        (
            publish.version.as_deref(),
            &publish.accept_volume_loss,
            review::consent(review.saved.as_ref()),
        ),
        trusted.volumes.as_ref(),
    )
    .map_err(|mut error| {
        // Volume loss is the one confirmation `destructive` asks; saving deletes
        // nothing itself, so the loss is the next deploy's.
        if error.code == ployz_core::RpcErrorCode::ConfirmationRequired
            && let Some(rest) = error.message.strip_prefix("This permanently deletes")
        {
            error.message = format!("Next deploy deletes{rest}");
        }
        error
    })?;
    review::guard_saving(tx, &environment.summary.id, review.saved.as_ref(), &target)?;
    let gate = review::Gate::new(tx, &environment.summary.id, trusted)?;
    review::approve(
        who,
        &review,
        (&target, review::Shipping::Publish, &review::Pruning::Off),
        &gate,
    )?;
    // Saved as the next Deploy's consent, a loss that deletes nothing too: the
    // Servers held no data, so a push removes it while they still hold none.
    let lost = losses;
    let (saved, created) = review::publish(
        tx,
        who,
        &environment.summary.id,
        environment.working,
        review.saved.as_ref(),
        (publish.message.as_deref(), &lost),
    )?;
    if !created
        && let Some(latest) = review.saved.as_ref()
        && latest.accepted != lost
    {
        review::reconsent(tx, &environment.summary.id, latest.revision, &lost)?;
    }
    // Saved State already held it: proposals ending still moves the revision.
    let mut summary = environment.summary;
    if consumed && !created {
        let mut whole = scope::load_by_id(tx, &summary.id)?;
        scope::persist_working(tx, &mut whole)?;
        summary = whole.summary;
    }
    Ok(Published {
        environment: summary,
        saved: Some(saved),
        created,
    })
}

pub(crate) fn discard(
    tx: &mut dyn Tx,
    who: &Actor,
    discard: &Discard,
) -> Result<Discarded, RpcError> {
    let environment = scope::lock(tx, who, &discard.environment)?;
    let reviewed = review::review(tx, &environment)?;
    review::check(&reviewed, discard.version.as_deref())?;
    let prepared = super::authored::prepare_discard(
        tx,
        &environment,
        &reviewed,
        discard.target,
        discard.path.as_ref(),
    )?;
    let (mut environment, changed) = prepared.persist(tx)?;
    // Discarding the whole draft ends its proposals; a path keeps them.
    if discard.path.is_none() && crate::branch::end_included(tx, &environment.id, None)? && !changed
    {
        let mut whole = scope::load_by_id(tx, &environment.id)?;
        scope::persist_working(tx, &mut whole)?;
        environment = whole.summary;
    }
    let before = reviewed.view.review_changes();
    let view = if changed {
        let after = scope::load_by_id(tx, &environment.id)?;
        review::review(tx, &after)?.view
    } else {
        reviewed.view
    };
    let reversed = view
        .reversals()
        .into_iter()
        .map(|change| Reversal {
            way_back: WayBack::of(&change, &view.draft_changes),
            change,
        })
        .collect();
    Ok(Discarded {
        environment,
        saved: reviewed.saved.as_ref().map(|saved| saved.revision),
        saved_ran_nothing: view.saved_ran_nothing,
        changed,
        reversed,
        dropped: dropped(&before, &view),
    })
}

/// Core names a source that changed since its introduction `web.source`; `set`, and
/// the review of a Service never deployed (see `sources`), name it by its kind. So a
/// dropped one on such a Service is named by the kind the discard threw away,
/// `web.image` or `web.repository`; a deployed Service's stays `web.source`.
fn source_named(row: &ServiceSettingChange, never_deployed: bool) -> String {
    let set = match row.after.get("type").and_then(|kind| kind.as_str()) {
        Some("image") => "image",
        Some("git") => "repository",
        _ => return row.path.clone(),
    };
    match row.path.strip_suffix(".source") {
        Some(service) if never_deployed => format!("{service}.{set}"),
        _ => row.path.clone(),
    }
}

/// The paths of `before`'s changes that `view` no longer holds: a changed
/// Setting, or a node it no longer makes or removes.
fn dropped(before: &[super::NodeChange], view: &super::DiffView) -> Vec<String> {
    let after = view.review_changes();
    before
        .iter()
        .flat_map(|change| {
            let now = after
                .iter()
                .find(|now| now.node == change.node && now.lifecycle == change.lifecycle);
            // A create among the deploy changes has no Applied State, as `sources` reads it.
            let never_deployed = view.changes.iter().any(|deploy| {
                deploy.node == change.node && deploy.lifecycle == ReviewLifecycleKind::Create
            });
            match (change.lifecycle, now) {
                (ReviewLifecycleKind::Update, None) => {
                    change.settings.iter().map(|row| row.path.clone()).collect()
                }
                (_, Some(now)) => change
                    .settings
                    .iter()
                    .filter(|row| {
                        !now.settings.iter().any(|kept| {
                            kept.path == row.path
                                && kept.before == row.before
                                && kept.after == row.after
                        })
                    })
                    .map(|row| source_named(row, never_deployed))
                    .collect(),
                (_, None) => vec![match change.node.node_type {
                    EnvironmentNodeType::Service => change.name.clone(),
                    EnvironmentNodeType::Volume => format!("volumes.{}", change.name),
                    EnvironmentNodeType::Config => format!("configs.{}", change.name),
                }],
            }
        })
        .collect()
}
