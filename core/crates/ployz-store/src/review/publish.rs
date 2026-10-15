//! Publish (Working State → Saved State) and Discard (undo staged changes at any
//! level). Both recompute the review under the Environment's lock and refuse a
//! stale version; neither rebases.

use ployz_core::RpcError;
use ployz_core::config::canonicalize_environment_intent;

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
    /// abandons a draft reversal; Review discards the server-derived visible rows.
    #[serde(default)]
    #[ts(as = "Option<DiscardTarget>", optional)]
    pub target: DiscardTarget,
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
    /// False when nothing it names was staged.
    pub changed: bool,
}

pub(crate) fn publish(
    tx: &mut dyn Tx,
    who: &Actor,
    publish: &Publish,
    trusted: &Trusted,
) -> Result<Published, RpcError> {
    let mut environment = scope::lock(tx, who, &publish.environment)?;
    crate::removal::marked(tx, &environment)?;
    let review = review::review(tx, &environment)?;
    review::check(&review, publish.version.as_deref())?;
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
    review::approve(
        who,
        &environment.summary.id,
        &review,
        &target,
        &trusted.approval,
    )?;
    let lost = review::lost(losses);
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
    Ok(Discarded {
        environment,
        saved: reviewed.saved.as_ref().map(|saved| saved.revision),
        changed,
    })
}
