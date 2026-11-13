//! `config_saved`: every revision of an Environment's Saved State. The statements over
//! the table live here; none of them takes a lock.

use ployz_core::RpcError;
use ployz_core::config::{SavedEnvironmentIntent, canonicalize_environment_intent};

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, Revision};
use crate::scope::revision_param;
use crate::storage::Tx;

/// One Saved revision.
pub(crate) struct Saved {
    pub(crate) revision: Revision,
    pub(crate) intent: SavedEnvironmentIntent,
    /// The Volume losses accepted when this revision was saved, each with the
    /// Docker Volumes seen holding its data then. They cover the next Deploy of
    /// this revision until the evidence changes.
    pub(crate) accepted: Vec<crate::VolumeLoss>,
}

/// The revision `intent` is Saved as, with the evidence it was saved with.
pub(crate) struct Insert<'a> {
    pub(crate) revision: Revision,
    pub(crate) intent: &'a SavedEnvironmentIntent,
    pub(crate) message: Option<&'a str>,
    pub(crate) accepted: &'a [crate::VolumeLoss],
    pub(crate) changed: &'a [(String, String)],
}

fn accepted_json(accepted: &[crate::VolumeLoss]) -> Option<String> {
    (!accepted.is_empty()).then(|| serde_json::to_string(accepted).expect("Volume losses are JSON"))
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

/// The document Saved as `revision`, as stored; none when there is no such revision.
pub(crate) fn intent_at(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
) -> Result<Option<SavedEnvironmentIntent>, RpcError> {
    tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1 AND revision = ?2",
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
        ],
    )?
    .first()
    .map(|row| row.intent(0, "Saved State"))
    .transpose()
}

/// Every document `environment` has Saved, as stored.
pub(crate) fn intents(
    tx: &mut dyn Tx,
    environment: &str,
) -> Result<Vec<SavedEnvironmentIntent>, RpcError> {
    tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1",
        &[environment.into()],
    )?
    .iter()
    .map(|row| row.intent(0, "Saved State"))
    .collect()
}

/// Add a revision of `environment`'s Saved State.
pub(crate) fn insert(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    revision: &Insert<'_>,
) -> Result<(), RpcError> {
    let changed = (!revision.changed.is_empty())
        .then(|| serde_json::to_string(revision.changed).expect("value changes are JSON"));
    tx.execute(
        "INSERT INTO config_saved (environment_id, revision, organization_id, intent, message, saved_by, saved_at, accepted_losses, value_changes) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        &[
            environment.as_str().into(),
            revision_param(revision.revision)?.into(),
            who.organization.as_str().into(),
            serde_json::to_string(revision.intent)
                .expect("Saved State is JSON")
                .as_str()
                .into(),
            revision
                .message
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .into(),
            who.principal.as_ref().map(crate::Principal::as_str).into(),
            crate::deployment::now().into(),
            accepted_json(revision.accepted).as_deref().into(),
            changed.as_deref().into(),
        ],
    )?;
    Ok(())
}

/// Replace the consent saved with `revision`.
pub(crate) fn set_accepted_losses(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
    accepted: &[crate::VolumeLoss],
) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_saved SET accepted_losses = ?3 WHERE environment_id = ?1 AND revision = ?2",
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
            accepted_json(accepted).as_deref().into(),
        ],
    )?;
    Ok(())
}

/// Replace the document Saved as `revision`.
pub(crate) fn rewrite_intent(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
    intent: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_saved SET intent = ?3 WHERE environment_id = ?1 AND revision = ?2",
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
            serde_json::to_string(intent)
                .expect("Saved State is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(())
}
