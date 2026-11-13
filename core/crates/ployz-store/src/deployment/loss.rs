//! The Volumes a Deploy removes, and the Docker Volumes the Servers hold of them:
//! what a destructive review, a Deployment and Branch automation all name a loss by.
//!
//! What the Servers hold is evidence Cloud (or the hidden CLI runner) observes
//! outside the transaction and passes in as [`VolumeObservation`]. Missing or
//! incomplete evidence refuses: a Deploy never deletes data it could not see.

use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::{DockerVolumeId, DockerVolumeName, MachineId, Namespace, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::error;
use crate::id::{VolumeId, VolumeName};
use crate::trusted::{ObservationStage, VolumeObservation};

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

/// The Volumes `held` holds that `target` drops: what a full Deploy of `target`
/// removes from the Servers.
pub(crate) fn removed(
    held: &SavedEnvironmentIntent,
    target: &SavedEnvironmentIntent,
    namespace: &Namespace,
) -> Result<Vec<RemovedVolume>, RpcError> {
    held.volumes
        .iter()
        .filter(|old| drops(target, &old.resource_id))
        .map(|volume| {
            Ok(RemovedVolume {
                id: VolumeId::parse(volume.resource_id.as_str())
                    .map_err(|_| error::corrupt("Volume ID"))?,
                name: VolumeName::parse(volume.name.as_str())
                    .map_err(|_| error::corrupt("Volume name"))?,
                docker_volume: docker_volume(
                    namespace,
                    named_for(namespace, volume).unwrap_or(&volume.resource_id),
                )?,
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

/// The ID a Volume's Docker Volume is named for in `namespace` when not its own: in a
/// preview's derived Namespace, its lineage's, so an Own Copy takes its parent's name
/// and every preview of a rule names its Volumes alike.
pub(crate) fn named_for<'a>(
    namespace: &Namespace,
    volume: &'a ployz_core::config::SavedVolumeIntent,
) -> Option<&'a str> {
    ployz_core::authority::preview::is_preview_namespace(namespace)
        .then_some(volume.resource_lineage_id.as_str())
}

/// The Docker Volume a Volume's data lives in: lowering names it `vol-{id}`, and
/// the Servers scope that to the Namespace.
///
/// # Errors
/// Returns `internal` when `volume` can't name a Docker Volume.
pub fn docker_volume(namespace: &Namespace, volume: &str) -> Result<DockerVolumeName, RpcError> {
    let logical =
        DockerVolumeName::for_resource(volume).map_err(|_| error::corrupt("Volume ID"))?;
    Ok(namespace.volume_name(&logical))
}

/// Each of `removed` with the Docker Volumes the Servers hold of it, and the
/// Servers that didn't answer. Refuses `unavailable` when nobody looked for one, or
/// the look couldn't ask the Servers.
pub(crate) fn held(
    removed: Vec<RemovedVolume>,
    observed: Option<&VolumeObservation>,
) -> Result<(Vec<VolumeLoss>, Vec<MachineId>), RpcError> {
    if removed.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let Some(observed) = observed.filter(|observed| {
        observed.failure.is_none()
            && removed
                .iter()
                .all(|volume| observed.sought.contains(&volume.docker_volume))
    }) else {
        return Err(unchecked(&removed, observed));
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

/// The refusal for `removed` when the Servers weren't checked for their data. When
/// the check ran it names the Servers it couldn't check, and carries why as the cause.
fn unchecked(removed: &[RemovedVolume], observed: Option<&VolumeObservation>) -> RpcError {
    let volumes: Vec<&VolumeName> = removed.iter().map(|volume| &volume.name).collect();
    let Some(failure) = observed.and_then(|observed| observed.failure.as_ref()) else {
        return error::unobserved(
            "This removes deployed Volumes, and the Servers weren't checked for their data, \
             so it refuses. Try again once the Servers can be reached",
            json!({ "volumes": volumes }),
        );
    };
    let servers = &failure.servers;
    let endpoint = failure.endpoint.as_ref();
    let mut refused = match failure.stage {
        ObservationStage::Unreachable => {
            let (who, them) = match (servers.as_slice(), endpoint) {
                ([], Some(endpoint)) => (endpoint.clone(), "it"),
                ([], None) => ("the Servers".to_owned(), "they"),
                ([server], _) => (format!("Server {server}"), "it"),
                (servers, _) => (format!("Servers {}", servers.join(", ")), "they"),
            };
            error::unobserved(
                format!(
                    "This removes deployed Volumes, and Ployz couldn't reach {who} to check \
                     for their data, so it refuses. Try again once {them} can be reached"
                ),
                json!({ "volumes": volumes, "unanswered": servers }),
            )
        }
        ObservationStage::Answered { .. } => {
            let server = servers.first();
            let who = match (server, endpoint) {
                (Some(server), _) => format!("Server {server}"),
                (None, Some(endpoint)) => endpoint.clone(),
                (None, None) => "the Server Ployz reached".to_owned(),
            };
            let details = match server {
                Some(server) => json!({ "volumes": volumes, "server": server }),
                None => json!({ "volumes": volumes }),
            };
            error::unobserved(
                format!(
                    "This removes deployed Volumes, and {who} returned an error when Ployz \
                     checked for their data, so it refuses. Fix the error {who} returned, then \
                     try again"
                ),
                details,
            )
        }
    };
    if let (Some(endpoint), Some(details)) = (endpoint, refused.details.as_object_mut()) {
        details.insert("endpoint".to_owned(), json!(endpoint));
    }
    refused.cause = std::iter::once(failure.reason.clone())
        .chain(failure.cause.iter().cloned())
        .collect();
    refused
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

#[cfg(test)]
#[path = "loss_tests.rs"]
mod tests;
