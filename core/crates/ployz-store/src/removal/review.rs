//! Destructive review of a Deploy: which deployed Volumes it removes, which Servers
//! hold their data, and whether the caller accepted each loss by name.
//!
//! What the Servers hold is evidence Cloud (or the hidden CLI runner) observes
//! outside the transaction and passes in as [`VolumeObservation`]. Missing or
//! incomplete evidence refuses: a Deploy never deletes data it could not see. A
//! Volume no Deploy ever applied needs no evidence, so undeployed Environments stay
//! operable without Servers. Publish and every publishing Deploy run the same review,
//! as Saved State then holds the removal. A confirmation binds the exact Docker
//! Volumes found; admission freezes them, each with its Machine, and the runner
//! deletes only those.

use std::collections::BTreeMap;

use ployz_core::{DockerVolumeName, MachineId, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::deployment::loss::{RemovedVolume, VolumeLoss, covered, held};
use crate::digest::short_digest;
use crate::error::{self, Accept, Confirmation};
use crate::id::{EnvironmentId, VolumeName};
use crate::storage::Tx;
use crate::trusted::VolumeObservation;

/// List the Docker Volumes a Git push may delete, across the Organization.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ConsentedQuery {}

/// What a push needs observed: the Docker Volumes of each Volume whose loss was
/// accepted when it was saved and that still runs or is on its way. Pass what the
/// Servers hold with the push, or it deletes none of them.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConsentedView {
    pub volumes: Vec<DockerVolumeName>,
}

pub(crate) fn consented(tx: &mut dyn Tx, who: &Actor) -> Result<ConsentedView, RpcError> {
    let rows = tx.query(
        "SELECT s.environment_id FROM config_saved s \
         JOIN config_environment e ON e.id = s.environment_id \
         WHERE s.organization_id = ?1 AND s.accepted_losses IS NOT NULL \
         AND s.revision = (SELECT MAX(l.revision) FROM config_saved l WHERE l.environment_id = s.environment_id) \
         ORDER BY s.environment_id",
        &[who.organization.as_str().into()],
    )?;
    let mut volumes = Vec::new();
    for row in rows {
        let id: EnvironmentId = row.parse(0, "Environment ID")?;
        let Some(saved) = crate::review::latest_saved(tx, &id)? else {
            continue;
        };
        let environment = crate::scope::load_by_id(tx, &id)?;
        let head = crate::deployment::head(tx, &environment)?;
        let dropped = crate::deployment::Landing::of(&head).dropped(&saved.intent);
        volumes.extend(
            saved
                .accepted
                .into_iter()
                .filter(|loss| {
                    dropped
                        .volumes
                        .iter()
                        .any(|volume| volume.resource_id == loss.volume.id.as_str())
                })
                .map(|loss| loss.volume.docker_volume),
        );
    }
    Ok(ConsentedView { volumes })
}

/// Review `removed` against what the Servers hold, for the Deploy or Publish of
/// review `base` (its version). Refuses with `unavailable` when the observation is
/// missing, didn't look for one of them, or a Server didn't answer; with
/// `invalid_argument` for an accepted name this doesn't remove; and with
/// `confirmation_required` unless `accepted` names every Volume whose data some
/// Server holds and `version` is the one that refusal hands back. That version binds
/// the Organization, the Environment and every Docker Volume found, each on its
/// Server, so a Server that starts holding one asks again. A loss `consent` covers
/// needs no acceptance: it was accepted when its removal was saved.
pub(crate) fn review(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    (base, version): (&str, Option<&str>),
    removed: Vec<RemovedVolume>,
    observed: Option<&VolumeObservation>,
    (accepted, consent): (&[VolumeName], &[VolumeLoss]),
) -> Result<Vec<VolumeLoss>, RpcError> {
    let names: Vec<VolumeName> = removed.iter().map(|volume| volume.name.clone()).collect();
    if let Some(unknown) = accepted.iter().find(|name| !names.contains(name)) {
        return Err(error::invalid(
            format!("This deletes no Volume named {unknown}"),
            error::valid(&names),
        ));
    }
    let (losses, unanswered) = held(removed, observed)?;
    if !unanswered.is_empty() {
        let asked = unanswered.iter().copied().collect();
        let servers = crate::deployment::server_names(tx, &who.organization, Some(&asked))?;
        return Err(silent(&names, &unanswered, &servers));
    }
    let lost: Vec<&VolumeLoss> = losses
        .iter()
        .filter(|loss| !loss.deletes.is_empty() && !covered(loss, consent))
        .collect();
    if lost.is_empty() {
        return Ok(losses);
    }
    let bound = format!("{base}:{}", digest(who, environment, &lost));
    let confirmed = version == Some(bound.as_str())
        && lost.iter().all(|loss| accepted.contains(&loss.volume.name));
    if confirmed {
        return Ok(losses);
    }
    Err(Confirmation {
        volumes: json!(lost),
        accept: Some(Accept::Names(
            lost.iter().map(|loss| loss.volume.name.clone()).collect(),
        )),
        version: Some(bound),
        ..Confirmation::default()
    }
    .refuse(error::data_loss_message(
        "This permanently deletes",
        lost.iter().map(|loss| &loss.volume.name),
    )))
}

/// The refusal for removing `volumes` while the Servers `unanswered` didn't answer
/// whether they hold their data, each named by `servers`, else by its ID.
fn silent(
    volumes: &[VolumeName],
    unanswered: &[MachineId],
    servers: &BTreeMap<MachineId, String>,
) -> RpcError {
    let named: Vec<String> = unanswered
        .iter()
        .map(|machine| {
            servers
                .get(machine)
                .cloned()
                .unwrap_or_else(|| machine.to_string())
        })
        .collect();
    let (who, holds, answers) = match named.as_slice() {
        [server] => (format!("Server {server}"), "it holds", "it answers"),
        servers => (
            format!("Servers {}", servers.join(", ")),
            "they hold",
            "they answer",
        ),
    };
    error::unobserved(
        format!(
            "This removes deployed Volumes, and {who} didn't answer whether {holds} their \
             data, so it refuses. Try again once {answers}"
        ),
        json!({ "volumes": volumes, "unanswered": unanswered }),
    )
}

/// What a confirmation to delete `lost` binds: who, where, and each Docker Volume
/// on each Server.
pub(crate) fn digest(who: &Actor, environment: &EnvironmentId, lost: &[&VolumeLoss]) -> String {
    let bound = json!({
        "organization": who.organization,
        "environment": environment,
        "volumes": lost,
    });
    short_digest(&bound.to_string())
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
