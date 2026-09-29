//! Publish (Working State → Saved State) and Discard (undo staged changes at any
//! level). Both recompute the review under the Environment's lock and refuse a
//! stale version; neither rebases.

use ployz_core::RpcError;
use ployz_core::config::{
    EnvironmentNodeType, SavedEnvironmentIntent, ServiceConfig, canonicalize_environment_intent,
    compare_service_settings, restore_environment_node,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::Revision;
use crate::review::{self, Review};
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{self, ServiceSetting, SettingPath};
use crate::storage::Tx;

/// Put Working State in Saved State without deploying it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    /// The Environment to publish.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Refuse with `conflict` unless this is still the latest `diff` version.
    #[serde(default)]
    pub version: Option<String>,
}

/// The Saved revision Working State is now in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Published {
    /// The Environment.
    pub environment: EnvironmentSummary,
    /// The Saved revision that now holds Working State.
    pub saved: Revision,
    /// False when Saved State already held it.
    pub created: bool,
}

/// Undo staged changes: all of them, one Service's, or one Setting's.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Discard {
    /// The Environment to discard in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// `SERVICE` or `SERVICE.SETTING`; none discards everything.
    #[serde(default)]
    pub path: Option<SettingPath>,
    /// Refuse with `conflict` unless this is still the latest `diff` version.
    #[serde(default)]
    pub version: Option<String>,
}

/// The Environment after a discard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Discarded {
    /// The Environment.
    pub environment: EnvironmentSummary,
    /// The latest Saved revision, which follows the discard so the next Deploy ships it.
    pub saved: Option<Revision>,
}

pub(crate) fn publish(
    tx: &mut dyn Tx,
    who: &Actor,
    publish: &Publish,
) -> Result<Published, RpcError> {
    let environment = scope::lock(tx, who, &publish.environment)?;
    let review = review::review(tx, &environment)?;
    review::check(&review, publish.version.as_deref())?;
    let (saved, created) = review::publish(
        tx,
        who,
        &environment.summary.id,
        environment.working,
        review.saved.as_ref(),
    )?;
    Ok(Published {
        environment: environment.summary,
        saved,
        created,
    })
}

pub(crate) fn discard(
    tx: &mut dyn Tx,
    who: &Actor,
    discard: &Discard,
) -> Result<Discarded, RpcError> {
    let mut environment = scope::lock(tx, who, &discard.environment)?;
    let review = review::review(tx, &environment)?;
    review::check(&review, discard.version.as_deref())?;
    let Review { saved, head, .. } = review;
    let (working, restored) = match &discard.path {
        // Everything returns to Head, in Working and Saved State alike.
        None => (head.intent.clone(), saved.as_ref().map(|_| head.intent)),
        Some(path) => restore(
            tx,
            &environment,
            path,
            saved.as_ref().map(|saved| &saved.intent),
            head.intent,
        )?,
    };
    let mut revision = saved.as_ref().map(|saved| saved.revision);
    if let Some(restored) = restored {
        revision =
            Some(review::publish(tx, who, &environment.summary.id, restored, saved.as_ref())?.0);
    }
    if canonicalize_environment_intent(working.clone())
        != canonicalize_environment_intent(environment.working.clone())
    {
        environment.working = working;
        scope::save_working(tx, &mut environment)?;
    }
    Ok(Discarded {
        environment: environment.summary,
        saved: revision,
    })
}

/// Restore one Service, or one of its Settings, in Working State and, when Saved
/// State follows, in Saved State too. Returns (Working, Saved to publish).
fn restore(
    tx: &mut dyn Tx,
    environment: &scope::Environment,
    path: &SettingPath,
    saved: Option<&SavedEnvironmentIntent>,
    head: SavedEnvironmentIntent,
) -> Result<(SavedEnvironmentIntent, Option<SavedEnvironmentIntent>), RpcError> {
    let working = &environment.working;
    let id = [working, &head]
        .into_iter()
        .flat_map(|intent| &intent.services)
        .find(|service| service.slug == path.service().as_str())
        .map(|service| service.id.clone())
        .ok_or_else(|| settings::no_service(path.service(), &environment.summary.name, working))?;
    let config = |intent: Option<&SavedEnvironmentIntent>| {
        intent
            .and_then(|intent| intent.services.iter().find(|service| service.id == id))
            .map(|service| ServiceConfig::from(service.config.clone()))
    };
    let head_node = config(Some(&head));
    let saved_node = config(saved);
    let field = path.setting().map(ServiceSetting::field);
    // A new node's Setting resets to its Introduction, and stays unpublished.
    let introduction = field.is_some() && head_node.is_none();
    if introduction && saved_node.is_some() {
        return Err(error::conflict(
            "This Setting has no discard baseline: its Service is published but not deployed",
            json!({ "path": path }),
        ));
    }
    let baseline = if introduction {
        review::introductions(tx, environment)?
    } else {
        head
    };
    let restore = |current: &SavedEnvironmentIntent| {
        restore_environment_node(
            current.clone(),
            Some(&baseline),
            EnvironmentNodeType::Service,
            &id,
            field,
        )
        .map_err(|error| {
            error::conflict(
                format!(
                    "Discard would leave an invalid Environment: {}",
                    error.message
                ),
                json!({ "service": path.service() }),
            )
        })
    };
    // Saved State follows, except for a Setting that Saved State holds at Head already.
    let saved_follows = match (field, &saved_node, &head_node) {
        (None, _, _) => true,
        (Some(field), Some(saved), Some(head)) => compare_service_settings(saved, Some(head))
            .iter()
            .any(|row| row.path == field && row.can_restore),
        (Some(_), _, _) => false,
    };
    let saved = match saved {
        Some(saved) if saved_follows && !introduction => Some(restore(saved)?),
        Some(_) | None => None,
    };
    Ok((restore(working)?, saved))
}
