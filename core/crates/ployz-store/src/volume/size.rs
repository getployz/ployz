//! A deployed Managed Volume's size. It lives in three places: Working State, the
//! `config_volume_storage` lock row and Applied State. The lock row is the one the
//! Machines answer to, so Head and Applied State read the lock, and only Cloud, which
//! drives the Machines through a resize, moves it ([`VolumeResized`]). No `Command`
//! reaches it, so `/api/config/write`, the CLI and the agent can't claim a size the
//! Machines don't hold.

use ployz_core::config::{SavedVolumeIntent, VolumeKind};
use ployz_core::{ProvisionedVolumeMaximumBytes, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::Actor;
use crate::deployment::{self, DeploymentSummary};
use crate::error;
use crate::id::{EnvironmentId, VolumeId};
use crate::scope;
use crate::storage::Tx;

/// A deployed Managed Volume's size moved, in either direction, as Cloud's resize
/// run reports it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumeResized {
    /// By ID: a rename since the run started can't redirect it.
    pub environment: EnvironmentId,
    pub volume: VolumeId,
    /// The size the Store recorded when the run read it; the record is a
    /// compare-and-set on it.
    pub from: ProvisionedVolumeMaximumBytes,
    pub to: ProvisionedVolumeMaximumBytes,
    /// Set by a run that found no Server holding a copy, so it changes only the size
    /// the next Deploy creates the Volume at. Absent from every other record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unheld: Option<Unheld>,
}

/// What a run that found no copy saw before it asked the Servers: the Environment's
/// latest numbered Deployment, none before the first. A Deploy admitted since may
/// have created the copy at the old size, so the record refuses once a newer one
/// exists or one is in flight.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Unheld {
    pub latest_deployment: Option<u64>,
}

/// Record `resized` in one transaction under the Environment's lock. This is the only
/// way a deployed Volume's size changes: a run that grows records after the last
/// Machine grew, a run that shrinks records before the first Machine shrinks, and one
/// refused midway records the sizes swapped to put the old one back. The lock row
/// moves from `from` to `to`, in either direction, and Applied State mirrors it: a
/// different Applied size is what an older revision asked for, not what the Servers
/// hold. Working State follows only where it still says `from`; anything else there
/// is a user's intent, such as a staged History restore, and stays. Saved revisions
/// are history and are never touched.
///
/// A lock row already at `to` is a replayed record: it answers Ok and writes nothing,
/// even when Working State was restored to `from` since. Once the lock moved on from
/// `to`, the same record is no replay and fails the compare-and-set.
///
/// A record that lowers the size waits for the Environment's Deployment in flight to
/// end: a Deploy claims its Volumes at the lock's size, and a copy lowered beneath that
/// claim fails its planner. A Deploy's admission and its claim take the same
/// Environment lock, so either the record sees the Deployment or the Deployment's
/// claim reads the lowered size. A record that
/// raises, a grow's or a refused shrink's undo, never waits, unless it is
/// [`Unheld`]: no copy holds the size yet, so a Deployment in flight, or one
/// admitted since the run looked, may create it at the lock's old size. One whose
/// runner's lease lapsed counts too, and so does one whose outcome is unknown,
/// as an abandoned run leaves it, until a later one applies: its Machines may
/// still be preparing the copy.
///
/// Every record that writes changes Applied State, which Cloud's change log reads, so
/// the lock row needs no change-log source of its own.
///
/// # Errors
/// `not_found` when the Volume has no fixed storage, or is gone from both Working and
/// Applied State; `conflict` with `reason: "size_changed"` when the lock row holds
/// anything but `Provisioned { from }`, a Docker Volume included; `conflict` with
/// `reason: "deploying"`, naming the Deployment, when it lowers the size while one is
/// in flight, or is [`Unheld`] and one is in flight or newer than it saw.
pub(crate) fn record(
    tx: &mut dyn Tx,
    who: &Actor,
    resized: &VolumeResized,
) -> Result<(), RpcError> {
    let mut environment = scope::lock_id(tx, who, &resized.environment)?;
    let id = environment.summary.id.clone();
    let volume = resized.volume.as_str();
    let applied = applied_volume(tx, &id, volume)?;
    let staged = environment
        .working
        .volumes
        .iter_mut()
        .find(|node| node.resource_id == volume);
    let recorded = super::locked_storage(tx, &id)?.remove(volume);
    let (Some(recorded), Some(name)) = (
        recorded,
        staged
            .as_deref()
            .or(applied.as_ref())
            .map(|node| node.name.clone()),
    ) else {
        return Err(error::not_found(
            "No deployed Volume to record a size for.",
            json!({ "volume": volume }),
        ));
    };
    let to = VolumeKind::Provisioned {
        maximum_bytes: resized.to,
    };
    if recorded == to {
        return Ok(());
    }
    let from = VolumeKind::Provisioned {
        maximum_bytes: resized.from,
    };
    if recorded != from {
        let message = match recorded {
            VolumeKind::Docker {} => {
                format!("{name} is a Docker Volume; it has no size to record.")
            }
            VolumeKind::Provisioned { maximum_bytes } => format!(
                "{name} is recorded at {maximum_bytes}, not {}; read its size again.",
                resized.from
            ),
        };
        return Err(error::conflict(
            message,
            json!({ "reason": "size_changed", "volume": volume, "recorded": recorded }),
        ));
    }
    if let Some(unheld) = &resized.unheld
        && let Some(newer) = deployment::claimed_or_queued(tx, &id)?
            .or(deployment::latest_numbered(tx, &id)?
                .filter(|latest| latest.number > unheld.latest_deployment))
            .or(deployment::unknown_since_applied(tx, &id)?)
    {
        return Err(deployed_since(&name, volume, &newer));
    }
    if resized.to < resized.from
        && let Some(running) = deployment::in_flight(tx, &id)?
    {
        return Err(deploying(&name, volume, &running));
    }
    let storage = serde_json::to_string(&to).expect("storage is JSON");
    tx.execute(
        "UPDATE config_volume_storage SET storage = ?3 \
         WHERE environment_id = ?1 AND volume_id = ?2",
        &[id.as_str().into(), volume.into(), storage.as_str().into()],
    )?;
    if let Some(mut node) = applied {
        node.storage = to;
        tx.execute(
            "UPDATE config_applied SET node = ?3 WHERE environment_id = ?1 AND node_id = ?2",
            &[
                id.as_str().into(),
                volume.into(),
                scope::Node::Volume(&node).document().as_str().into(),
            ],
        )?;
    }
    let follows = staged.is_some_and(|node| {
        let follows = node.storage == from;
        if follows {
            node.storage = to;
        }
        follows
    });
    if follows {
        scope::save_working(tx, &mut environment)?;
    }
    Ok(())
}

/// The refusal of a record that lowers Volume `name` while Deployment `running` is in
/// flight: it claimed, or will claim, the Volume at the size the lock holds, and a copy
/// lowered beneath that claim fails its planner.
fn deploying(name: &str, volume: &str, running: &DeploymentSummary) -> RpcError {
    let cancel = running
        .number
        .map_or_else(|| running.id.to_string(), |number| number.to_string());
    error::conflict(
        format!(
            "Deployment {} is in flight; shrink {name} when it ends, or end it now with \
             ployz deployment cancel {cancel}.",
            running.label()
        ),
        json!({
            "reason": "deploying",
            "volume": volume,
            "deployment": { "id": running.id, "number": running.number },
        }),
    )
}

/// The refusal of an [`Unheld`] record for Volume `name` once Deployment `newer` is in
/// flight or was admitted since the run looked for copies: it may create the Volume
/// at the lock's size, so the run looks again.
fn deployed_since(name: &str, volume: &str, newer: &DeploymentSummary) -> RpcError {
    error::conflict(
        format!(
            "Deployment {} may create {name} at its recorded size; Ployz looks for its \
             copies again before it changes the size.",
            newer.label()
        ),
        json!({
            "reason": "deploying",
            "volume": volume,
            "deployment": { "id": newer.id, "number": newer.number },
            "unheld": true,
        }),
    )
}

/// Applied State's node for Volume `volume`, if a Deploy put it there.
fn applied_volume(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    volume: &str,
) -> Result<Option<SavedVolumeIntent>, RpcError> {
    tx.query(
        "SELECT node FROM config_applied WHERE environment_id = ?1 AND node_id = ?2",
        &[environment.as_str().into(), volume.into()],
    )?
    .first()
    .map(|row| row.json(0, "Applied State"))
    .transpose()
}

/// `volumes` at the size their Servers hold: a Provisioned Volume's lock row, once a
/// Deploy fixed one, whatever its Saved revision asked for. A Saved revision keeps
/// what it asked for as history; read as the latest Saved State, or put into Head
/// or Applied State, it says the lock's size. A Docker Volume and an undeployed one
/// stay as they are.
pub(crate) fn held(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    volumes: &mut [SavedVolumeIntent],
) -> Result<(), RpcError> {
    if volumes
        .iter()
        .all(|volume| matches!(volume.storage, VolumeKind::Docker {}))
    {
        return Ok(());
    }
    let locked = super::locked_storage(tx, environment)?;
    for volume in volumes {
        if let (VolumeKind::Provisioned { .. }, Some(fixed @ VolumeKind::Provisioned { .. })) =
            (&volume.storage, locked.get(&volume.resource_id))
        {
            volume.storage = *fixed;
        }
    }
    Ok(())
}

/// Whether a storage an intent asks for fits the storage a Volume was fixed at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StorageRule<'a> {
    /// A hand-typed change of one Volume, by resource ID: that Volume keeps its fixed
    /// storage exactly, and a different size points at `ployz volume set --size`,
    /// which resizes every copy and records the size. Without this a user could
    /// stage 5 GB on a Volume its Servers hold at 20 GB, deploy it, and read 5 GB in
    /// `volume ls`. Every other Volume of the intent is checked as `Replay`, so a
    /// History restore staged earlier doesn't block an unrelated edit.
    Edit { volume: &'a str },
    /// An intent made earlier coming back: publish, Deploy admission, a History
    /// restore or undo, a Branch landing. A Managed Volume fits at any size, since a
    /// Deploy ships it at the size its Servers hold ([`held`]), so a revision saved
    /// before a grow or a shrink still deploys. A Docker Volume, or a change of kind,
    /// must match.
    Replay,
}

/// Refuse `volumes` when one is given storage its fixed storage doesn't fit under
/// `rule`.
pub(crate) fn check_storage<'a>(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    volumes: impl IntoIterator<Item = &'a SavedVolumeIntent>,
    rule: StorageRule<'_>,
) -> Result<(), RpcError> {
    let volumes: Vec<_> = volumes.into_iter().collect();
    if volumes.is_empty() {
        return Ok(());
    }
    let locked = super::locked_storage(tx, environment)?;
    for volume in volumes {
        let Some(fixed) = locked.get(&volume.resource_id) else {
            continue;
        };
        let edited = rule
            == StorageRule::Edit {
                volume: &volume.resource_id,
            };
        if fits(fixed, &volume.storage, edited) {
            continue;
        }
        let name = &volume.name;
        return Err(match (fixed, &volume.storage) {
            (
                VolumeKind::Provisioned {
                    maximum_bytes: fixed,
                },
                VolumeKind::Provisioned {
                    maximum_bytes: asked,
                },
            ) => error::conflict(
                format!(
                    "{name} is deployed at {fixed}; {} it with ployz volume set {name} --size {}",
                    if asked > fixed { "grow" } else { "shrink" },
                    gb(*asked)
                ),
                json!({ "volume": name, "storage_locked": true, "recorded": fixed }),
            ),
            _ => error::conflict(
                format!(
                    "Volume {name} keeps its storage once a Deploy has been requested; add a new \
                     Volume for other storage"
                ),
                json!({ "volume": name, "storage_locked": true }),
            ),
        });
    }
    Ok(())
}

fn fits(fixed: &VolumeKind, asked: &VolumeKind, edited: bool) -> bool {
    match (fixed, asked) {
        (
            VolumeKind::Provisioned {
                maximum_bytes: fixed,
            },
            VolumeKind::Provisioned {
                maximum_bytes: asked,
            },
        ) => !edited || asked == fixed,
        (fixed, asked) => fixed == asked,
    }
}

/// `bytes` as `volume set --size` takes it: the GB number without its unit.
fn gb(bytes: ProvisionedVolumeMaximumBytes) -> String {
    bytes.to_string().trim_end_matches(" GB").to_owned()
}

#[cfg(test)]
mod tests {
    use ployz_core::config::VolumeKind;

    use super::{fits, gb};

    fn managed(gb: u64) -> VolumeKind {
        VolumeKind::Provisioned {
            maximum_bytes: (gb * 1_000_000_000).try_into().unwrap(),
        }
    }

    #[test]
    fn a_managed_volume_fits_exactly_when_edited_and_at_any_size_when_replayed() {
        let docker = VolumeKind::Docker {};
        for (fixed, asked, edited, expected) in [
            (managed(10), managed(10), true, true),
            (managed(10), managed(10), false, true),
            (managed(10), managed(5), true, false),
            (managed(10), managed(5), false, true),
            (managed(10), managed(20), true, false),
            (managed(10), managed(20), false, true),
            (docker, docker, true, true),
            (docker, docker, false, true),
            (docker, managed(10), true, false),
            (docker, managed(10), false, false),
            (managed(10), docker, true, false),
            (managed(10), docker, false, false),
        ] {
            assert_eq!(
                fits(&fixed, &asked, edited),
                expected,
                "fixed {fixed:?}, asked {asked:?}, edited {edited}"
            );
        }
    }

    #[test]
    fn the_size_flag_takes_the_gb_number_alone() {
        assert_eq!(gb(20_000_000_000.try_into().unwrap()), "20");
        assert_eq!(gb(1_500_000_000.try_into().unwrap()), "1.5");
    }
}
