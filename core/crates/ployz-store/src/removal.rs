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
use ployz_core::{DockerVolumeId, DockerVolumeName, Namespace, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::id::{EnvironmentId, VolumeId, VolumeName};
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
        .filter(|old| {
            target
                .volumes
                .iter()
                .all(|new| new.resource_id != old.resource_id)
        })
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

/// The Docker Volume a Volume's data lives in: lowering names it `vol-{id}`, and
/// the Servers scope that to the Namespace.
pub(crate) fn docker_volume(
    namespace: &Namespace,
    volume: &str,
) -> Result<DockerVolumeName, RpcError> {
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
/// Server, so a Server that starts holding one asks again.
pub(crate) fn review(
    who: &Actor,
    environment: &EnvironmentId,
    (base, version): (&str, Option<&str>),
    removed: Vec<RemovedVolume>,
    observed: Option<&VolumeObservation>,
    accepted: &[VolumeName],
) -> Result<Vec<VolumeLoss>, RpcError> {
    let names = || {
        removed
            .iter()
            .map(|volume| &volume.name)
            .collect::<Vec<_>>()
    };
    if let Some(unknown) = accepted.iter().find(|name| !names().contains(name)) {
        return Err(error::invalid(
            format!("This deletes no Volume named {unknown}"),
            json!({ "valid_children": names() }),
        ));
    }
    if removed.is_empty() {
        return Ok(Vec::new());
    }
    let Some(observed) = observed.filter(|observed| {
        removed
            .iter()
            .all(|volume| observed.sought.contains(&volume.docker_volume))
    }) else {
        return Err(error::unobserved(
            "This removes deployed Volumes, and the Servers weren't checked for their data, \
             so it refuses. Try again once the Servers can be reached",
            json!({ "volumes": names() }),
        ));
    };
    if !observed.unanswered.is_empty() {
        return Err(error::unobserved(
            "This removes deployed Volumes, and some Servers didn't answer whether they hold \
             their data, so it refuses. Try again once they answer",
            json!({ "volumes": names(), "unanswered": observed.unanswered }),
        ));
    }
    let losses: Vec<VolumeLoss> = removed
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
    let lost: Vec<&VolumeLoss> = losses
        .iter()
        .filter(|loss| !loss.deletes.is_empty())
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

/// What a confirmation to delete `lost` binds: who, where, and each Docker Volume
/// on each Server.
fn digest(who: &Actor, environment: &EnvironmentId, lost: &[&VolumeLoss]) -> String {
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
