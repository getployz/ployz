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

use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::{DockerVolumeId, DockerVolumeName, MachineId, Namespace, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, VolumeId, VolumeName};
use crate::storage::Tx;
use crate::trusted::VolumeObservation;

/// A deployed Volume a Deploy removes, with the Docker Volume that holds its data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RemovedVolume {
    pub id: VolumeId,
    pub name: VolumeName,
    /// The Docker Volume each Server holds its data in.
    pub docker_volume: DockerVolumeName,
}

/// A removed Volume and the Docker Volumes found holding its data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VolumeLoss {
    #[serde(flatten)]
    pub volume: RemovedVolume,
    /// Each Docker Volume a Deploy deletes, on the Server holding it.
    pub deletes: Vec<DockerVolumeId>,
}

/// List the Docker Volumes a Git push may delete, across the Organization.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ConsentedQuery {}

/// What a push needs observed: the Docker Volumes of each Volume whose loss was
/// accepted when it was saved and that is still deployed. Pass what the Servers
/// hold with the push, or it deletes none of them.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ConsentedView {
    pub volumes: Vec<DockerVolumeName>,
}

pub(crate) fn consented(tx: &mut dyn Tx, who: &Actor) -> Result<ConsentedView, RpcError> {
    let rows = tx.query(
        "SELECT s.environment_id, s.accepted_losses FROM config_saved s \
         JOIN config_environment e ON e.id = s.environment_id \
         WHERE s.organization_id = ?1 AND s.accepted_losses IS NOT NULL \
         AND s.revision = (SELECT MAX(l.revision) FROM config_saved l WHERE l.environment_id = s.environment_id) \
         ORDER BY s.environment_id",
        &[who.organization.as_str().into()],
    )?;
    let mut volumes = Vec::new();
    for row in rows {
        let id: EnvironmentId = row.parse(0, "Environment ID")?;
        let accepted: Vec<VolumeLoss> = serde_json::from_str(row.text(1)?)
            .map_err(|_| error::corrupt("accepted Volume losses"))?;
        let environment = crate::scope::load_by_id(tx, &id)?;
        let applied = crate::deployment::head(tx, &environment)?.applied;
        volumes.extend(
            accepted
                .into_iter()
                .filter(|loss| {
                    applied
                        .volumes
                        .iter()
                        .any(|volume| volume.resource_id == loss.volume.id.as_str())
                })
                .map(|loss| loss.volume.docker_volume),
        );
    }
    Ok(ConsentedView { volumes })
}

/// The Volumes Applied State holds that `target` drops: what a full Deploy of
/// `target` removes from the Servers.
pub(crate) fn removed(
    applied: &SavedEnvironmentIntent,
    target: &SavedEnvironmentIntent,
    namespace: &Namespace,
) -> Result<Vec<RemovedVolume>, RpcError> {
    applied
        .volumes
        .iter()
        .filter(|old| drops(target, &old.resource_id))
        .map(|volume| {
            Ok(RemovedVolume {
                id: VolumeId::parse(volume.resource_id.as_str())
                    .map_err(|_| error::corrupt("Volume ID"))?,
                name: VolumeName::parse(volume.name.as_str())
                    .map_err(|_| error::corrupt("Volume name"))?,
                docker_volume: docker_volume(namespace, &volume.resource_id)?,
            })
        })
        .collect()
}

fn drops(target: &SavedEnvironmentIntent, volume: &str) -> bool {
    target.volumes.iter().all(|kept| kept.resource_id != volume)
}

/// The losses of `consent` whose Volumes `target` still removes, so consent never
/// outlives a revision that restores its Volume.
pub(crate) fn still_removed(
    consent: &[VolumeLoss],
    target: &SavedEnvironmentIntent,
) -> Vec<VolumeLoss> {
    consent
        .iter()
        .filter(|loss| drops(target, loss.volume.id.as_str()))
        .cloned()
        .collect()
}

/// The Docker Volume a Volume's data lives in: lowering names it `vol-{id}`, and
/// the Servers scope that to the Namespace.
///
/// # Errors
/// Returns `internal` when `volume` can't name a Docker Volume.
pub fn docker_volume(namespace: &Namespace, volume: &str) -> Result<DockerVolumeName, RpcError> {
    let logical = DockerVolumeName::parse(format!("vol-{volume}"))
        .map_err(|_| error::corrupt("Volume ID"))?;
    Ok(namespace.volume_name(&logical))
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
            json!({ "valid_children": names }),
        ));
    }
    let (losses, unanswered) = held(removed, observed)?;
    if !unanswered.is_empty() {
        return Err(error::unobserved(
            "This removes deployed Volumes, and some Servers didn't answer whether they hold \
             their data, so it refuses. Try again once they answer",
            json!({ "volumes": names, "unanswered": unanswered }),
        ));
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
    Err(error::confirmation_required(
        format!(
            "This permanently deletes the data of {}. Accept each by name",
            lost.iter()
                .map(|loss| loss.volume.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        json!({
            "volumes": lost,
            "accept": lost.iter().map(|loss| &loss.volume.name).collect::<Vec<_>>(),
            "version": bound,
        }),
    ))
}

/// Each of `removed` with the Docker Volumes the Servers hold of it, and the
/// Servers that didn't answer. Refuses `unavailable` when nobody looked for one.
pub(crate) fn held(
    removed: Vec<RemovedVolume>,
    observed: Option<&VolumeObservation>,
) -> Result<(Vec<VolumeLoss>, Vec<MachineId>), RpcError> {
    if removed.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let Some(observed) = observed.filter(|observed| {
        removed
            .iter()
            .all(|volume| observed.sought.contains(&volume.docker_volume))
    }) else {
        return Err(error::unobserved(
            "This removes deployed Volumes, and the Servers weren't checked for their data, \
             so it refuses. Try again once the Servers can be reached",
            json!({ "volumes": removed.iter().map(|volume| &volume.name).collect::<Vec<_>>() }),
        ));
    };
    let losses = removed
        .into_iter()
        .map(|volume| {
            let mut deletes: Vec<DockerVolumeId> = observed
                .held
                .iter()
                .filter(|held| held.name == volume.docker_volume)
                .cloned()
                .collect();
            deletes.sort_by_key(ToString::to_string);
            VolumeLoss { deletes, volume }
        })
        .collect();
    Ok((losses, observed.unanswered.clone()))
}

/// Whether `consent` accepted deleting `loss`: the same Volume, and no Docker
/// Volume beyond those seen when it was accepted. A Server that has since started
/// holding its data is new evidence, so it asks again.
pub(crate) fn covered(loss: &VolumeLoss, consent: &[VolumeLoss]) -> bool {
    consent.iter().any(|accepted| {
        accepted.volume.id == loss.volume.id
            && loss
                .deletes
                .iter()
                .all(|delete| accepted.deletes.contains(delete))
    })
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

/// The first 8 bytes of `text`'s SHA-256, in hex: enough to tell reviews apart.
pub(crate) fn short_digest(text: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
    hex::encode(digest.as_ref().get(..8).unwrap_or_default())
}

#[cfg(test)]
#[path = "review_tests.rs"]
mod tests;
