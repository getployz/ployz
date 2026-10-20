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
/// Every record that writes changes Applied State, which Cloud's change log reads, so
/// the lock row needs no change-log source of its own.
///
/// # Errors
/// `not_found` when the Volume has no fixed storage, or is gone from both Working and
/// Applied State; `conflict` with `reason: "size_changed"` when the lock row holds
/// anything but `Provisioned { from }`, a Docker Volume included.
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
    /// restore or undo, a Branch landing. A Managed Volume fits at or below its fixed
    /// size, since a Deploy accepts a live bound at or above the size it asks for, so
    /// rolling back past a grow still deploys. A Docker Volume, or a change of kind,
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
            ) if edited => error::conflict(
                if asked > fixed {
                    format!(
                        "{name} is deployed at {fixed}; grow it with ployz volume set {name} \
                         --size {}",
                        gb(*asked)
                    )
                } else {
                    format!("{name} is deployed at {fixed}; a deployed Volume can't shrink yet")
                },
                json!({ "volume": name, "storage_locked": true, "recorded": fixed }),
            ),
            (
                VolumeKind::Provisioned {
                    maximum_bytes: fixed,
                },
                VolumeKind::Provisioned {
                    maximum_bytes: asked,
                },
            ) => error::conflict(
                format!(
                    "This revision asks {asked} for {name}, which is deployed at {fixed}. Grow it \
                     first with ployz volume set {name} --size {}",
                    gb(*asked)
                ),
                json!({
                    "volume": name, "storage_locked": true, "recorded": fixed, "asked": asked
                }),
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
        ) => {
            if edited {
                asked == fixed
            } else {
                asked <= fixed
            }
        }
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
    fn a_managed_volume_fits_exactly_when_edited_and_at_or_below_when_replayed() {
        let docker = VolumeKind::Docker {};
        for (fixed, asked, edited, expected) in [
            (managed(10), managed(10), true, true),
            (managed(10), managed(10), false, true),
            (managed(10), managed(5), true, false),
            (managed(10), managed(5), false, true),
            (managed(10), managed(20), true, false),
            (managed(10), managed(20), false, false),
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
