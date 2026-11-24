//! Where each Volume's data began. The approval that first mounts a Volume mints its
//! birth, and the client posts it with the signed versions or the rule whose specs carry
//! it. The Store records it once and lowers every later spec with it. It never makes one:
//! a birth it got wrong only makes a spec no signature covers.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::authority::{Births, CopyStep, PlannedVolume, PlannedVolumes};
use ployz_core::config::SavedEnvironmentIntent;
use ployz_core::{DockerVolumeName, Namespace, RpcError, VolumeBirth};
use serde_json::json;

use crate::error;
use crate::id::EnvironmentId;
use crate::storage::Tx;

/// Each recorded birth of `environment`'s Volumes, by Volume ID.
pub(crate) fn born(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeMap<String, VolumeBirth>, RpcError> {
    tx.query(
        "SELECT volume_id, birth FROM config_volume_birth WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?
    .iter()
    .map(|row| Ok((row.text(0)?.to_owned(), row.json(1, "Volume birth")?)))
    .collect()
}

/// Each Volume of `intent`, by the name its specs declare it under. One is unborn when no
/// approval recorded its birth and no Server can hold its data: no Deployment fixed its
/// storage and Applied State doesn't hold it.
pub(crate) fn planned(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    intent: &SavedEnvironmentIntent,
) -> Result<PlannedVolumes, RpcError> {
    let held = Held::read(tx, environment, intent)?;
    intent
        .volumes
        .iter()
        .map(|volume| {
            Ok((
                declared(&volume.resource_id)?,
                PlannedVolume {
                    name: volume.name.clone(),
                    unborn: held.unborn(&volume.resource_id),
                },
            ))
        })
        .collect()
}

/// Record `births`, which an approval minted for Volumes of `intent` in `namespace`. A
/// birth already recorded stays, so a retry posting it again changes nothing.
///
/// # Errors
/// Returns `invalid_argument` for a birth made in another Namespace, for a Volume
/// `intent` doesn't hold, or for one whose data a Server may already hold; `conflict`
/// when another approval recorded a different birth first.
pub(crate) fn record(
    tx: &mut dyn Tx,
    (environment, namespace): (&EnvironmentId, &Namespace),
    intent: &SavedEnvironmentIntent,
    births: &Births,
) -> Result<(), RpcError> {
    if births.is_empty() {
        return Ok(());
    }
    let held = Held::read(tx, environment, intent)?;
    for (name, birth) in births {
        let volume = intent
            .volumes
            .iter()
            .find(|volume| declared(&volume.resource_id).ok().as_ref() == Some(name))
            .ok_or_else(|| {
                error::invalid(
                    format!("{name} isn't a volume this environment saved. Approve again."),
                    json!({ "volume": name }),
                )
            })?;
        if birth.namespace != *namespace {
            return Err(error::invalid(
                format!(
                    "{} began in another environment. Approve again.",
                    volume.name
                ),
                json!({ "volume": volume.name }),
            ));
        }
        match held.born.get(&volume.resource_id) {
            Some(recorded) if recorded == birth => continue,
            Some(_) => {
                return Err(error::conflict(
                    format!(
                        "Another approval already made {} a new volume. Approve again.",
                        volume.name
                    ),
                    json!({ "volume": volume.name }),
                ));
            }
            None if !held.unborn(&volume.resource_id) => {
                return Err(error::invalid(
                    format!(
                        "{} already holds data, so this approval can't make it a new volume. \
                         Approve again.",
                        volume.name
                    ),
                    json!({ "volume": volume.name }),
                ));
            }
            None => {}
        }
        insert(tx, environment, &volume.resource_id, birth)?;
    }
    Ok(())
}

/// Record the birth each of `copies` mints for the new Volume it restores into, in
/// `namespace`. Staging a restore fixes the new Volume's storage, so here that lock is no
/// sign of data; instead the Copy must make the restore staging reviewed: the new Volume
/// restores from the Copy's source, at the Copy's point time. A birth already recorded
/// stays, so a retry posting it again changes nothing, and a Version that mounts the new
/// Volume later records the same birth.
///
/// # Errors
/// Returns `invalid_argument` for a Copy made in another Namespace, into a Volume
/// `intent` doesn't hold, one that doesn't restore from the Copy's source at its point,
/// or one whose data a Server may already hold; `conflict` when another approval recorded
/// a different birth first.
pub(crate) fn record_copies(
    tx: &mut dyn Tx,
    (environment, namespace): (&EnvironmentId, &Namespace),
    intent: &SavedEnvironmentIntent,
    copies: &[CopyStep],
) -> Result<(), RpcError> {
    if copies.is_empty() {
        return Ok(());
    }
    let mut held = Held::read(tx, environment, intent)?;
    let physical = |resource: &str| declared(resource).map(|name| namespace.volume_name(&name));
    for copy in copies {
        let volume = intent
            .volumes
            .iter()
            .find(|volume| physical(&volume.resource_id).ok().as_ref() == Some(&copy.into))
            .ok_or_else(|| {
                error::invalid(
                    format!(
                        "{} isn't a volume this environment saved. Approve again.",
                        copy.into
                    ),
                    json!({ "volume": copy.into }),
                )
            })?;
        if copy.namespace != *namespace || copy.birth.namespace != *namespace {
            return Err(error::invalid(
                format!(
                    "The copy into {} began in another environment. Approve again.",
                    volume.name
                ),
                json!({ "volume": volume.name }),
            ));
        }
        let restores = volume.restored_from.as_ref().is_some_and(|from| {
            from.at.checked_mul(1000) == Some(copy.taken_at.as_millis())
                && intent.volumes.iter().any(|source| {
                    source.resource_id == from.volume
                        && physical(&source.resource_id).ok().as_ref() == Some(&copy.source)
                })
        });
        if !restores {
            return Err(error::invalid(
                format!(
                    "The copy into {} isn't the restore this environment staged. Approve again.",
                    volume.name
                ),
                json!({ "volume": volume.name }),
            ));
        }
        match held.born.get(&volume.resource_id) {
            Some(recorded) if *recorded == copy.birth => continue,
            Some(_) => {
                return Err(error::conflict(
                    format!(
                        "Another approval already made {} a new volume. Approve again.",
                        volume.name
                    ),
                    json!({ "volume": volume.name }),
                ));
            }
            None if held.applied.contains(&volume.resource_id) => {
                return Err(error::invalid(
                    format!(
                        "{} already holds data, so this copy can't make it a new volume. \
                         Approve again.",
                        volume.name
                    ),
                    json!({ "volume": volume.name }),
                ));
            }
            None => {}
        }
        insert(tx, environment, &volume.resource_id, &copy.birth)?;
        // A second Copy into the same Volume in this Deploy replays this birth or conflicts.
        held.born
            .insert(volume.resource_id.clone(), copy.birth.clone());
    }
    Ok(())
}

fn insert(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    volume: &str,
    birth: &VolumeBirth,
) -> Result<(), RpcError> {
    let text = serde_json::to_string(birth).map_err(|_| error::corrupt("Volume birth"))?;
    tx.execute(
        "INSERT INTO config_volume_birth (environment_id, volume_id, birth) \
         VALUES (?1, ?2, ?3)",
        &[
            environment.as_str().into(),
            volume.into(),
            text.as_str().into(),
        ],
    )?;
    Ok(())
}

/// The name a Volume's specs declare it under.
fn declared(resource_id: &str) -> Result<DockerVolumeName, RpcError> {
    DockerVolumeName::for_resource(resource_id).map_err(|_| error::corrupt("Volume"))
}

/// What says a Volume's data may be on a Server, or where it began.
struct Held {
    born: BTreeMap<String, VolumeBirth>,
    locked: BTreeSet<String>,
    applied: BTreeSet<String>,
}

impl Held {
    fn read(
        tx: &mut dyn Tx,
        environment: &EnvironmentId,
        intent: &SavedEnvironmentIntent,
    ) -> Result<Self, RpcError> {
        Ok(Self {
            born: born(tx, environment)?,
            locked: crate::volume::locked_storage(tx, environment)?
                .into_keys()
                .collect(),
            applied: crate::deployment::deployed_volumes(tx, environment, intent)?,
        })
    }

    fn unborn(&self, volume: &str) -> bool {
        !self.born.contains_key(volume)
            && !self.locked.contains(volume)
            && !self.applied.contains(volume)
    }
}
