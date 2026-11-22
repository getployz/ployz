//! Publish (Working State → Saved State) and Discard (undo staged changes at any
//! level). Both recompute the review under the Environment's lock and refuse a
//! stale version; neither rebases.

use ployz_core::RpcError;
use ployz_core::config::{
    EnvironmentNodeType, ReviewLifecycleKind, SavedEnvironmentIntent, ServiceSettingChange,
    canonicalize_environment_intent,
};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::{EnvironmentId, Revision, VolumeName};
use crate::review;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::SettingPath;
use crate::storage::Tx;
use crate::trusted::Evidence;
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
            .then(|| change.path())
            .and_then(|path| SettingPath::parse(&path).ok());
        Self {
            drops_unsaved: path.is_none(),
            path,
        }
    }
}

/// The Environment's summary once its proposals ended and nothing else moved
/// its revision: ending them still moves it, so a review taken before is stale.
/// `None` when `ended` is false.
fn after_ending(
    tx: &mut dyn Tx,
    trusted: &Trusted,
    id: &EnvironmentId,
    ended: bool,
) -> Result<Option<EnvironmentSummary>, RpcError> {
    if !ended {
        return Ok(None);
    }
    let mut whole = scope::load_by_id(tx, id)?;
    scope::write_working(
        tx,
        Evidence::Caller(trusted),
        &mut whole,
        scope::WorkingWrite::Trusted(scope::Reason::Publish),
    )?;
    Ok(Some(whole.summary))
}

/// Review `environment` for a Save that ships `shipping`, then end the draft's
/// proposals: what they brought is the Save's own now. A stale `version` is refused
/// with what the fresh review deletes. Returns the review, and whether it ended any
/// proposal.
pub(crate) fn review_for_save(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &scope::Environment,
    (shipping, version): (review::Shipping<'_>, Option<&str>),
    trusted: &Trusted,
) -> Result<(review::Review, bool), RpcError> {
    let review = review::review(tx, environment)?;
    if let Err(stale) = review::check(&review, version) {
        let target = canonicalize_environment_intent(environment.working.clone());
        let probe = deployment::namespace(tx, who, &environment.summary).and_then(|namespace| {
            review::destructive(
                tx,
                who,
                &review,
                (&target, &namespace, shipping),
                (None, &[], review::consent(review.saved.as_ref())),
                trusted.volumes.as_ref(),
            )
        });
        return Err(review::with_loss(stale, probe));
    }
    let consumed = crate::branch::end_included(tx, &environment.summary.id, None)?;
    Ok((review, consumed))
}

/// What a Save that keeps the latest Saved revision, because it holds the same
/// document, does with the losses saved with that revision as the next Deploy's
/// consent.
#[derive(Clone, Copy)]
pub(crate) enum UnchangedSave {
    /// Replace them with the losses this Save reviewed: a Publish reviews every
    /// pending removal, so it records the consent the next Deploy reads.
    RenewsConsent,
    /// Leave them: a Deploy ships its reviewed losses itself, and a narrowed one
    /// reviews no pending removal.
    KeepsConsent,
}

/// A Save of `environment`'s Working State, reviewed by [`review_for_save`].
pub(crate) struct Save<'a> {
    pub(crate) environment: &'a scope::Environment,
    pub(crate) review: &'a review::Review,
    /// Working State, canonical.
    pub(crate) target: &'a SavedEnvironmentIntent,
    pub(crate) namespace: &'a ployz_core::Namespace,
    pub(crate) shipping: review::Shipping<'a>,
    pub(crate) pruning: &'a review::Pruning,
    pub(crate) version: Option<&'a str>,
    pub(crate) accept_volume_loss: &'a [VolumeName],
    pub(crate) message: Option<&'a str>,
    pub(crate) unchanged: UnchangedSave,
}

/// Save Working State as the next Saved revision, or keep the latest when it holds
/// the same document. In order, it refuses a Volume loss not accepted, a source
/// change the caller may not protect, and a prune the caller may not destroy, then
/// asks for approval. Returns the revision Saved State is now at, whether this
/// created it, and the losses it reviewed, saved as the next Deploy's consent: a
/// loss that deletes nothing too, since the Servers held no data, so a push removes
/// it while they still hold none.
pub(crate) fn save(
    tx: &mut dyn Tx,
    who: &Actor,
    save: &Save<'_>,
    trusted: &Trusted,
) -> Result<(Revision, bool, Vec<crate::VolumeLoss>), RpcError> {
    let Save {
        environment,
        review,
        target,
        namespace,
        shipping,
        pruning,
        version,
        accept_volume_loss,
        message,
        unchanged,
    } = *save;
    let id = &environment.summary.id;
    let losses = review::destructive(
        tx,
        who,
        review,
        (target, namespace, shipping),
        (
            version,
            accept_volume_loss,
            review::consent(review.saved.as_ref()),
        ),
        trusted.volumes.as_ref(),
    )?;
    review::guard_saving(
        tx,
        Evidence::Caller(trusted),
        id,
        review.saved.as_ref(),
        target,
    )?;
    pruning.admit(trusted, None)?;
    let gate = review::Gate::new(tx, id, trusted)?;
    review::approve(who, review, (target, shipping, pruning), &gate)?;
    let (saved, created) = review::publish(
        tx,
        Evidence::Caller(trusted),
        who,
        id,
        environment.working.clone(),
        review.saved.as_ref(),
        (message, &losses),
    )?;
    if let UnchangedSave::RenewsConsent = unchanged
        && !created
        && let Some(latest) = review.saved.as_ref()
        && latest.accepted != losses
    {
        review::reconsent(tx, id, latest.revision, &losses)?;
    }
    Ok((saved, created, losses))
}

pub(crate) fn publish(
    tx: &mut dyn Tx,
    who: &Actor,
    publish: &Publish,
    trusted: &Trusted,
) -> Result<Published, RpcError> {
    let environment = scope::lock(tx, who, &publish.environment)?;
    trusted.writes(&environment.summary.name)?;
    crate::removal::marked(tx, &environment)?;
    let (review, consumed) = review_for_save(
        tx,
        who,
        &environment,
        (review::Shipping::Publish, publish.version.as_deref()),
        trusted,
    )?;
    if review.saved.is_none()
        && canonicalize_environment_intent(environment.working.clone())
            == canonicalize_environment_intent(review.head.intent.clone())
    {
        // Nothing staged: Head already is Working State.
        let summary = after_ending(tx, trusted, &environment.summary.id, consumed)?
            .unwrap_or(environment.summary);
        return Ok(Published {
            environment: summary,
            saved: None,
            created: false,
        });
    }
    // Saved State then holds the removal, which the next full Deploy ships: the
    // same destructive review, before anything is saved.
    let target = canonicalize_environment_intent(environment.working.clone());
    let namespace = deployment::namespace(tx, who, &environment.summary)?;
    let (saved, created, _) = save(
        tx,
        who,
        &Save {
            environment: &environment,
            review: &review,
            target: &target,
            namespace: &namespace,
            shipping: review::Shipping::Publish,
            pruning: &review::Pruning::Off,
            version: publish.version.as_deref(),
            accept_volume_loss: &publish.accept_volume_loss,
            message: publish.message.as_deref(),
            unchanged: UnchangedSave::RenewsConsent,
        },
        trusted,
    )?;
    // Saved State already held it.
    let summary = after_ending(tx, trusted, &environment.summary.id, consumed && !created)?
        .unwrap_or(environment.summary);
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
    trusted: &Trusted,
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
        trusted,
    )?;
    let (mut environment, changed) = prepared.persist(tx, Evidence::Caller(trusted))?;
    // Discarding the whole draft ends its proposals; a path keeps them.
    let ended = discard.path.is_none() && crate::branch::end_included(tx, &environment.id, None)?;
    if let Some(summary) = after_ending(tx, trusted, &environment.id, ended && !changed)? {
        environment = summary;
    }
    let before = reviewed.view.review_changes();
    let region = reviewed.view.region.clone();
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
    let mut dropped = dropped(&before, &view);
    if region.is_some() && view.region != region {
        dropped.insert(0, SettingPath::REGION.to_string());
    }
    Ok(Discarded {
        environment,
        saved: reviewed.saved.as_ref().map(|saved| saved.revision),
        saved_ran_nothing: view.saved_ran_nothing,
        changed,
        reversed,
        dropped,
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
