//! Destructive review of a Deploy: which deployed Volumes it removes, which Servers
//! hold their data, and whether the caller accepted each loss by name.
//!
//! What the Servers hold is evidence Cloud (or the hidden CLI runner) observes
//! outside the transaction and passes in as [`VolumeObservation`]. Missing or
//! incomplete evidence refuses: a Deploy never deletes data it could not see. A
//! Volume no Deploy ever applied needs no evidence, so undeployed Environments stay
//! operable without Servers. Admission freezes the exact Docker Volumes accepted,
//! each with its Machine, and the runner deletes only those.

use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::{DockerVolumeId, DockerVolumeName, Namespace, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::id::{VolumeId, VolumeName};
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

/// Review `removed` against what the Servers hold. Refuses with `unavailable` when
/// the observation is missing, didn't look for one of them, or a Server didn't
/// answer; with `confirmation_required` unless `accepted` names every Volume whose
/// data some Server holds; and with `invalid_argument` for an accepted name this
/// Deploy doesn't remove. `retry` is the version the refusal hands back.
pub(crate) fn review(
    removed: Vec<RemovedVolume>,
    observed: Option<&VolumeObservation>,
    accepted: &[VolumeName],
    retry: &str,
) -> Result<Vec<VolumeLoss>, RpcError> {
    let names = || {
        removed
            .iter()
            .map(|volume| &volume.name)
            .collect::<Vec<_>>()
    };
    if let Some(unknown) = accepted.iter().find(|name| !names().contains(name)) {
        return Err(error::invalid(
            format!("This Deploy deletes no Volume named {unknown}"),
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
            "This Deploy removes deployed Volumes, and the Servers weren't checked for their \
             data, so it refuses. Deploy again once the Servers can be reached",
            json!({ "volumes": names() }),
        ));
    };
    if !observed.unanswered.is_empty() {
        return Err(error::unobserved(
            "This Deploy removes deployed Volumes, and some Servers didn't answer whether they \
             hold their data, so it refuses. Deploy again once they answer",
            json!({ "volumes": names(), "unanswered": observed.unanswered }),
        ));
    }
    let losses: Vec<VolumeLoss> = removed
        .into_iter()
        .map(|volume| VolumeLoss {
            deletes: observed
                .held
                .iter()
                .filter(|held| held.name == volume.docker_volume)
                .cloned()
                .collect(),
            volume,
        })
        .collect();
    let unaccepted: Vec<&VolumeName> = losses
        .iter()
        .filter(|loss| !loss.deletes.is_empty() && !accepted.contains(&loss.volume.name))
        .map(|loss| &loss.volume.name)
        .collect();
    if !unaccepted.is_empty() {
        let lost: Vec<&VolumeLoss> = losses
            .iter()
            .filter(|loss| !loss.deletes.is_empty())
            .collect();
        return Err(error::confirmation_required(
            format!(
                "This Deploy permanently deletes the data of {}. Accept each by name",
                lost.iter()
                    .map(|loss| loss.volume.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            json!({
                "volumes": lost,
                "accept": lost.iter().map(|loss| &loss.volume.name).collect::<Vec<_>>(),
                "version": retry,
            }),
        ));
    }
    Ok(losses)
}
